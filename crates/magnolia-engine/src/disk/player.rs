//! The audio player: the node that plays a track's stream on the real-time thread.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{Ordering, fence};
use std::thread::Thread;

use super::reader::Shared;
use super::stream::{BLOCK, Cursor, Stream, block_of, first_sample, generation_tag, slot_word};
use crate::engine::FadeTable;
use crate::node::{Node, ProcessContext, ProcessStatus, Transport};
use crate::stats::EngineStats;

/// The node that plays one audio track's stream, made by the
/// [`DiskReader`](super::DiskReader) with its track: no input, the track's channels as outputs
/// ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
///
/// While the transport rolls, at its own position, its compensation included, it copies the
/// frames of the play's audible region that its stream holds for the play's generation, fades
/// them out over the stop's last 5 ms, and writes zeros elsewhere. A frame its stream does not hold
/// ready is silence, counted in [`EngineStats::disk_underruns`]; it never waits. Over silence, a
/// stopped transport, a silent block, or what lies outside its clips, it flags its outputs silent.
///
/// After each sub-block it tells the reader where it reads next, and records the fewest frames it
/// found ready ahead since its play started, the low-water mark. When it holds less than half the
/// read-ahead ready, lacked a frame, or its play's stop has just become known or been passed, it
/// asks the reader for more: one flag shared by every player, and one wake,
/// [`wake_disk_reader`](super::wake_disk_reader), when the flag was clear. It never sleeps: it has
/// no input that could wake it.
pub struct AudioPlayer {
    stream: Arc<Stream>,
    shared: Arc<Shared>,
    /// The reader's thread; none for a reader without one.
    reader: Option<Thread>,
    stats: Arc<EngineStats>,
    fade: Arc<FadeTable>,
    /// Below this many frames ready ahead, it asks for a refill: half the read-ahead.
    threshold: i64,
    /// The play it plays: its generation and its audible region's start; none while stopped.
    play: Option<(u64, i64)>,
    /// The play's stop, once the player has asked for a refill at it; and whether it has asked on
    /// passing it.
    stop_asked: Option<i64>,
    passed_stop: bool,
}

/// What a sub-block found in the slots.
#[derive(Clone, Copy, Default)]
struct Found {
    /// Whether it copied a block that is not silent.
    sound: bool,
    /// How many frames it lacked.
    missing: u64,
}

impl AudioPlayer {
    pub(crate) fn new(
        stream: Arc<Stream>,
        shared: Arc<Shared>,
        reader: Option<Thread>,
        stats: Arc<EngineStats>,
        fade: Arc<FadeTable>,
        threshold: i64,
    ) -> Self {
        Self {
            stream,
            shared,
            reader,
            stats,
            fade,
            threshold,
            play: None,
            stop_asked: None,
            passed_stop: false,
        }
    }

    /// Notes the play under way; a new one starts a new low-water mark.
    fn note_play(&mut self, transport: &Transport) {
        let play = (transport.generation, transport.audible.start.0);
        if self.play != Some(play) {
            self.play = Some(play);
            self.stop_asked = None;
            self.passed_stop = false;
            self.stream.reset_low_water();
        }
    }

    /// Whether the play's stop changes what the reader keeps for it: when the stop becomes known,
    /// the reader may write past it; when the player passes it, everything it kept is free. The
    /// player then asks for a refill, after publishing the cursor that says so, whatever it
    /// believes its stream holds: the reader may have started a new generation meanwhile, and the
    /// play that follows waits for it.
    fn stop_changes_what_is_kept(&mut self, transport: &Transport, end: i64) -> bool {
        let Some(stop) = transport.audible.stop.map(|stop| stop.0) else {
            return false;
        };
        let known_now = self.stop_asked != Some(stop);
        self.stop_asked = Some(stop);
        let passed_now = end >= stop && !self.passed_stop;
        self.passed_stop |= end >= stop;
        known_now || passed_now
    }

    /// The timeline samples of a sub-block from `position`, `frames` long, that the play needs from
    /// the stream: those of its audible region within the clips.
    fn needed(&self, transport: &Transport, position: i64, frames: i64) -> Range<i64> {
        let extent = &self.stream.extent;
        let start = position.max(transport.audible.start.0).max(extent.start);
        let end = position
            .saturating_add(frames)
            .min(transport.audible.stop.map_or(i64::MAX, |stop| stop.0))
            .min(extent.end);
        start..end.max(start)
    }

    /// Before reading: records how many frames the stream holds ready ahead of `needed_from`, and
    /// says whether to ask for a refill: below the threshold. A stream that holds another
    /// generation says nothing of this play's supply.
    fn watch_supply(&self, transport: &Transport, needed_from: i64) -> bool {
        let extent_end = self.stream.extent.end;
        let stop = transport.audible.stop.map_or(i64::MAX, |stop| stop.0);
        // A play without a cue reads no stream: there is no supply to watch.
        if transport.generation == 0 || needed_from >= extent_end.min(stop) {
            return false;
        }
        let (generation, until) = self.stream.ready_until();
        if generation != generation_tag(transport.generation) {
            return false;
        }
        if until >= extent_end {
            return false;
        }
        let ahead = until.saturating_sub(needed_from).max(0);
        self.stream.record_supply(ahead as u64);
        ahead < self.threshold
    }

    /// Copies what the slots hold for `generation` over `needed` into the outputs, whose first
    /// frame is at `position`.
    fn read(
        &self,
        context: &mut ProcessContext<'_>,
        generation: u64,
        position: i64,
        needed: Range<i64>,
    ) -> Found {
        let mut found = Found::default();
        let mut start = needed.start;
        while start < needed.end {
            let block = block_of(start);
            let end = needed.end.min(first_sample(block).saturating_add(BLOCK));
            let stretch = start..end;
            match self.copy_block(context, generation, block, position, stretch.clone()) {
                Copied::Sound => found.sound = true,
                Copied::Silent => {}
                Copied::Missing => {
                    found.missing = found
                        .missing
                        .saturating_add(end.saturating_sub(start) as u64);
                }
            }
            start = end;
        }
        found
    }

    /// Copies `stretch`, within one block, from its slot into the outputs, if the slot holds that
    /// block of `generation`: read its state, copy, read its state again, and take the block as
    /// missing if the reader rewrote the slot meanwhile.
    fn copy_block(
        &self,
        context: &mut ProcessContext<'_>,
        generation: u64,
        block: i64,
        position: i64,
        stretch: Range<i64>,
    ) -> Copied {
        // Generation 0 is a play without a cue: no stream holds it.
        let Some(slot) = self.stream.slot(block).filter(|_| generation != 0) else {
            return Copied::Missing;
        };
        let state = slot.state.load(Ordering::Acquire);
        if state == slot_word(generation, block, true) {
            return Copied::Silent;
        }
        if state != slot_word(generation, block, false) {
            return Copied::Missing;
        }
        let in_block = offset(stretch.start, first_sample(block));
        let in_output = offset(stretch.start, position);
        let length = offset(stretch.end, stretch.start);
        for (channel, output) in context.outputs_mut().enumerate() {
            let source = slot
                .channel(channel)
                .get(in_block..)
                .unwrap_or_default()
                .get(..length)
                .unwrap_or_default();
            let target = output
                .get_mut(in_output..)
                .unwrap_or_default()
                .get_mut(..length)
                .unwrap_or_default();
            for (target, source) in target.iter_mut().zip(source) {
                *target = f32::from_bits(source.load(Ordering::Relaxed));
            }
        }
        // Orders the copy's loads before the state's second load: a sample written after the
        // state was emptied shows as a changed state.
        fence(Ordering::Acquire);
        if slot.state.load(Ordering::Relaxed) != state {
            for output in context.outputs_mut() {
                output
                    .get_mut(in_output..)
                    .unwrap_or_default()
                    .get_mut(..length)
                    .unwrap_or_default()
                    .fill(0.0);
            }
            return Copied::Missing;
        }
        Copied::Sound
    }

    /// Fades out the copied frames over the fade's length before the play's stop, as a swap fades
    /// an edge out.
    fn fade_out(
        &self,
        context: &mut ProcessContext<'_>,
        position: i64,
        stop: i64,
        needed: Range<i64>,
    ) {
        let gains = self.fade.fade_in();
        let fade_start = stop.saturating_sub(gains.len() as i64);
        let start = needed.start.max(fade_start);
        let end = needed.end.min(stop);
        if start >= end {
            return;
        }
        let gains = gains
            .get(offset(start, fade_start)..)
            .unwrap_or_default()
            .get(..offset(end, start))
            .unwrap_or_default();
        let in_output = offset(start, position);
        for output in context.outputs_mut() {
            let samples = output.get_mut(in_output..).unwrap_or_default();
            for (sample, gain) in samples.iter_mut().zip(gains) {
                *sample *= 1.0 - gain;
            }
        }
    }
}

/// What one block's copy found.
enum Copied {
    Sound,
    Silent,
    Missing,
}

/// The distance from `from` to `to` on the timeline as an index, 0 if negative; distances within a
/// sub-block or a block always fit.
fn offset(to: i64, from: i64) -> usize {
    usize::try_from(to.saturating_sub(from)).unwrap_or(0)
}

impl Node for AudioPlayer {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        self.stream.channels
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let transport = context.transport();
        let position = transport.position.0;
        let frames = i64::try_from(context.frames()).unwrap_or(i64::MAX);
        for output in context.outputs_mut() {
            output.fill(0.0);
        }
        let mut found = Found::default();
        let mut refill = false;
        if transport.rolling {
            self.note_play(&transport);
            let needed = self.needed(&transport, position, frames);
            refill = self.watch_supply(&transport, needed.start);
            found = self.read(context, transport.generation, position, needed.clone());
            if let Some(stop) = transport.audible.stop {
                self.fade_out(context, position, stop.0, needed);
            }
            if found.missing > 0 {
                self.stats.count_disk_underruns(found.missing);
                refill = true;
            }
            refill |= self.stop_changes_what_is_kept(&transport, position.saturating_add(frames));
        } else {
            self.play = None;
        }
        for channel in 0..self.stream.channels {
            context.set_output_silent(channel, !found.sound);
        }
        self.stream.publish_cursor(Cursor {
            generation: transport.generation,
            position: if transport.rolling {
                position.saturating_add(frames)
            } else {
                position
            },
            rolling: transport.rolling,
            stop: transport.audible.stop.map(|stop| stop.0),
        });
        // After the cursor: the pass the request starts then sees where the player reads next,
        // and frees what a stopping play has passed.
        if refill {
            self.shared.ask(self.reader.as_ref());
        }
        ProcessStatus::Continue
    }
}

impl fmt::Debug for AudioPlayer {
    /// Its channels and the clips' extent, without its stream's samples.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioPlayer")
            .field("channels", &self.stream.channels)
            .field("extent", &self.stream.extent)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::disk::BLOCK_FRAMES;
    use crate::engine::fade_frames;
    use crate::time::SampleRate;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A writer rewrites one slot with ever newer generations, each block all of its generation's
    /// number, while copies run: every copy the player takes holds one generation whole, and it
    /// catches copies that straddled a write.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's thread waits for its writer thread; it never runs on the real-time path"
    )]
    fn a_copy_that_straddles_a_write_is_never_taken() -> TestResult {
        let stream = Arc::new(Stream::new(2, 0..i64::MAX, 3));
        let shared = Arc::new(Shared::new(Box::new([Arc::clone(&stream)])));
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let fade = Arc::new(FadeTable::new(fade_frames(rate)));
        let player = AudioPlayer::new(
            Arc::clone(&stream),
            shared,
            None,
            EngineStats::new_written(),
            fade,
            0,
        );
        let done = Arc::new(AtomicBool::new(false));
        // The writer writes once per copy the test begins, after a pseudo-random wait, so that its
        // writes fall during copies and after them.
        let attempts = Arc::new(AtomicU64::new(0));
        let writer = {
            let (stream, done, attempts) = (
                Arc::clone(&stream),
                Arc::clone(&done),
                Arc::clone(&attempts),
            );
            thread::spawn(move || {
                let mut generation: u32 = 0;
                let mut seen = 0;
                let mut random: u64 = 0x2545_F491_4F6C_DD1D;
                while !done.load(Ordering::Relaxed) {
                    while attempts.load(Ordering::Relaxed) == seen && !done.load(Ordering::Relaxed)
                    {
                        std::hint::spin_loop();
                    }
                    seen = attempts.load(Ordering::Relaxed);
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    for _ in 0..random % 20_000 {
                        std::hint::spin_loop();
                    }
                    generation += 1;
                    let frames = vec![generation as f32; 2 * BLOCK_FRAMES];
                    stream.write_block(u64::from(generation), 0, &frames);
                }
            })
        };
        stream.write_block(0, 0, &vec![1.0; 2 * BLOCK_FRAMES]);
        let mut outputs = vec![0.0; 2 * BLOCK_FRAMES];
        let mut silence = [false; 2];
        let (mut taken, mut caught) = (0, 0);
        let start = Instant::now();
        while (taken < 1_000 || caught < 100) && start.elapsed() < Duration::from_secs(20) {
            attempts.fetch_add(1, Ordering::Relaxed);
            let word = stream
                .slot(0)
                .ok_or("the stream has slots")?
                .state
                .load(Ordering::Acquire);
            let Some((generation, 0)) = super::super::stream::slot_holds(word) else {
                continue;
            };
            let mut context = ProcessContext::new(
                BLOCK_FRAMES,
                BLOCK_FRAMES,
                &[],
                &[],
                &mut outputs,
                &mut silence,
                &[],
            );
            let stretch = 0..BLOCK;
            match player.copy_block(&mut context, generation, 0, 0, stretch) {
                Copied::Sound => {
                    taken += 1;
                    let expected = generation as f32;
                    assert!(
                        outputs.iter().all(|&sample| sample == expected),
                        "a copy taken holds generation {generation} whole"
                    );
                }
                Copied::Missing => caught += 1,
                Copied::Silent => unreachable!("no block is silent"),
            }
        }
        done.store(true, Ordering::Relaxed);
        writer.join().map_err(|_| "the writer panicked")?;
        println!("copies taken: {taken}, caught straddling a write: {caught}");
        assert!(
            taken >= 1_000 && caught >= 100,
            "taken {taken}, caught {caught}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod stop_tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::disk::BLOCK_FRAMES;
    use crate::engine::fade_frames;
    use crate::node::AudibleRegion;
    use crate::time::{SamplePosition, SampleRate};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Runs `player` over one sub-block of 64 frames from `position`, its play of generation 1 from
    /// 0 asked to stop at `stop`; returns whether it asked for a refill.
    fn asks(player: &mut AudioPlayer, shared: &Shared, position: i64, stop: i64) -> bool {
        shared.asked.store(false, Ordering::SeqCst);
        let mut outputs = [0.0; 64];
        let mut silence = [false];
        let transport = Transport {
            rolling: true,
            position: SamplePosition(position),
            audible: AudibleRegion {
                start: SamplePosition(0),
                stop: Some(SamplePosition(stop)),
            },
            generation: 1,
        };
        let mut context = ProcessContext::new(64, 64, &[], &[], &mut outputs, &mut silence, &[])
            .with_transport(transport);
        player.process(&mut context);
        shared.asked.load(Ordering::SeqCst)
    }

    /// A player asks for a refill when its play's stop becomes known, and when it passes the stop,
    /// whatever it believes its stream holds: the reader keeps slots for a rolling play until then,
    /// and the play that follows waits for those slots. Between the two, it does not ask.
    #[test]
    fn a_player_asks_when_its_stop_is_known_and_when_it_passes_it() -> TestResult {
        let stream = Arc::new(Stream::new(1, 0..1_000_000, 4));
        let shared = Arc::new(Shared::new(Box::new([Arc::clone(&stream)])));
        // The stream holds the play's block, and says it holds plenty of its generation: no frame
        // lacking and no low supply to ask for.
        stream.write_block(1, 0, &[0.5; BLOCK_FRAMES]);
        stream.publish_ready(1, 200);
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let fade = Arc::new(FadeTable::new(fade_frames(rate)));
        let mut player = AudioPlayer::new(
            Arc::clone(&stream),
            Arc::clone(&shared),
            None,
            EngineStats::new_written(),
            fade,
            0,
        );
        assert!(
            asks(&mut player, &shared, 0, 1_000),
            "the stop became known"
        );
        assert!(!asks(&mut player, &shared, 64, 1_000), "nothing changed");
        assert!(
            !asks(&mut player, &shared, 896, 1_000),
            "not past the stop yet"
        );
        assert!(asks(&mut player, &shared, 960, 1_000), "it passes the stop");
        assert!(!asks(&mut player, &shared, 1_024, 1_000), "past it already");
        Ok(())
    }
}
