//! The disk reader's work: which blocks each stream needs, reading them from the clips' files,
//! mixing them, and writing them into their slots. It runs on the reader's thread, or on its
//! owner's for a reader without one; never on the real-time thread.
//!
//! A pass fills, round after round, the streams that hold the fewest frames ahead of their player
//! first, one run of consecutive blocks each, until every stream holds its read-ahead. A run reads
//! each clip it covers in one read of at least 256 KB, when the clip covers it whole. A stream is
//! left until it lacks a whole run, unless it holds less than half its read-ahead or its window
//! reaches its last clip's end, so that reads stay large. Once every stream holds what a play from
//! the last locate reads first, the pass publishes that generation primed, and a play of it may
//! start.

// A bad file must never crash the reader: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::reader::{DiskStats, Shared, Target};
use super::stream::{Cursor, Stream, block_of, first_sample, generation_tag, slot_holds};
use super::{AudioClip, AudioFile, BLOCK_FRAMES};
use crate::engine::FadeTable;
use crate::stats::EngineStats;
use crate::wav::decode;

/// The least a read brings from a file, when the clip covers the blocks read.
const MIN_READ_BYTES: usize = 256 * 1024;

/// How many frames the reader keeps ahead of each player, below which a player asks for more, and
/// from a cue's sample before a play from it is primed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Amounts {
    pub(crate) read_ahead: i64,
    pub(crate) threshold: i64,
    pub(crate) primed: i64,
}

/// The reader's work and what it keeps between passes.
pub(crate) struct Worker {
    shared: Arc<Shared>,
    /// The engine's statistics, where it publishes the generations it primes.
    stats: Arc<EngineStats>,
    tracks: Box<[TrackFill]>,
    fade: Arc<FadeTable>,
    read_ahead: i64,
    threshold: i64,
    primed: i64,
    scratch: Scratch,
    /// When the ask that started the pass under way was made, until its first run is timed.
    asked_at: Option<u64>,
    /// The last generation it published primed, so that it looks no more once it has.
    published_primed: u64,
    /// A round's streams to fill, by how many frames they hold ahead of their player.
    order: Vec<(i64, usize)>,
    /// The streams a pass found it cannot fill now, as when an older play still holds their slots.
    blocked: Vec<bool>,
}

/// The buffers one run works in, sized for the longest run.
struct Scratch {
    /// One read's bytes.
    bytes: Vec<u8>,
    /// One read's samples, decoded, interleaved.
    decoded: Vec<f32>,
    /// A run's frames, mixed, interleaved.
    mixed: Vec<f32>,
}

/// What the reader knows of one stream.
struct TrackFill {
    stream: Arc<Stream>,
    /// Sorted by start, never overlapping.
    clips: Box<[AudioClip]>,
    /// Blocks per run: enough for a read of `MIN_READ_BYTES` of its smallest frames, at most half
    /// its window.
    blocks_per_run: i64,
    /// The generation `filled` holds.
    generation: u64,
    /// The blocks filled in a row for `generation`, which the slots still hold.
    filled: Range<i64>,
}

/// The samples a stream should hold now: from `start` to `end_block`'s start.
struct Window {
    start: i64,
    end_block: i64,
}

/// The blocks a rolling play of an older generation may still read, whose slots the reader leaves.
struct Kept {
    generation_tag: u64,
    blocks: Range<i64>,
}

impl Kept {
    /// What `cursor`'s play may still read, when it rolls an older generation than the target's:
    /// until its stop is asked, everything ahead of its position, since it may roll on; once asked,
    /// up to the stop, which holds every path's compensation and the fade, whatever the graph.
    /// The stop only ever shortens what is kept, so a cursor the reader read a moment ago never
    /// lets it overwrite what the play still reads.
    fn by(cursor: Cursor, target_generation: u64) -> Option<Self> {
        if !cursor.rolling || cursor.generation == 0 || cursor.generation == target_generation {
            return None;
        }
        let first = block_of(cursor.position);
        let end = match cursor.stop {
            Some(stop) if stop <= cursor.position => return None,
            Some(stop) => block_of(stop.saturating_sub(1)).saturating_add(1),
            None => i64::MAX,
        };
        Some(Self {
            generation_tag: generation_tag(cursor.generation),
            blocks: first..end,
        })
    }

    /// Whether a slot's word holds one of the kept blocks.
    fn holds(&self, word: u64) -> bool {
        slot_holds(word).is_some_and(|(generation, block)| {
            generation == self.generation_tag && self.blocks.contains(&block)
        })
    }
}

impl Worker {
    /// A worker for `tracks`, each stream with its clips, keeping `amounts.read_ahead` frames ahead
    /// of each player, filling at once a stream that holds less than `amounts.threshold`, and
    /// publishing in `stats` each generation it primes.
    pub(crate) fn new(
        shared: Arc<Shared>,
        stats: Arc<EngineStats>,
        tracks: Vec<(Arc<Stream>, Box<[AudioClip]>)>,
        fade: Arc<FadeTable>,
        amounts: Amounts,
    ) -> Self {
        let Amounts {
            read_ahead,
            threshold,
            primed,
        } = amounts;
        let window_blocks = (read_ahead.max(1) as u64).div_ceil(BLOCK_FRAMES as u64);
        // The longest run's bytes and samples, over every stream.
        let mut largest = (0, 0);
        let tracks: Box<[TrackFill]> = tracks
            .into_iter()
            .map(|(stream, clips)| {
                let channels = stream.channels.max(1);
                let smallest_frame = clips.iter().map(frame_bytes).min().unwrap_or(channels);
                let biggest_frame = clips.iter().map(frame_bytes).max().unwrap_or(0);
                let blocks_per_run = MIN_READ_BYTES
                    .div_ceil(BLOCK_FRAMES.saturating_mul(smallest_frame.max(1)))
                    .clamp(1, (window_blocks / 2).max(1) as usize);
                let frames = blocks_per_run.saturating_mul(BLOCK_FRAMES);
                largest.0 = largest.0.max(frames.saturating_mul(biggest_frame));
                largest.1 = largest.1.max(frames.saturating_mul(channels));
                TrackFill {
                    stream,
                    clips,
                    blocks_per_run: blocks_per_run as i64,
                    generation: 0,
                    filled: 0..0,
                }
            })
            .collect();
        let (bytes, samples) = largest;
        let blocked = vec![false; tracks.len()];
        Self {
            shared,
            stats,
            tracks,
            fade,
            read_ahead,
            threshold,
            primed,
            asked_at: None,
            published_primed: 0,
            scratch: Scratch {
                bytes: vec![0; bytes],
                decoded: vec![0.0; samples],
                mixed: vec![0.0; samples],
            },
            order: Vec::new(),
            blocked,
        }
    }

    /// What it shares with the handle and the players.
    pub(crate) fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// Fills every stream that needs it for the last locate, the stream that holds the fewest
    /// frames ahead first, one run at a time, until none needs more or none can be filled now.
    /// Stops early when the reader is closed or paused, and plans again after a new locate. A pass
    /// that filled anything is a refill, timed in the reader's statistics. It publishes the last
    /// locate's generation primed as soon as it is, after the run that primes it, or at its end
    /// for streams that needed no run.
    pub(crate) fn pass(&mut self) {
        #[cfg(test)]
        self.panic_if_asked();
        let start = Instant::now();
        let asked_at = self.shared.asked_at.swap(0, Ordering::AcqRel);
        self.asked_at = (asked_at != 0).then_some(asked_at);
        let blocks_before = self.shared.stats.blocks_filled();
        self.fill_what_is_needed();
        let blocks = self
            .shared
            .stats
            .blocks_filled()
            .saturating_sub(blocks_before);
        self.shared.stats.record_pass(start.elapsed(), blocks);
        self.publish_if_primed(self.shared.target());
    }

    /// Publishes `target`'s generation primed, once, when every stream holds what a play from it
    /// reads first: a play of it waits for that, not for the whole read-ahead.
    fn publish_if_primed(&mut self, target: Target) {
        if target.generation > self.published_primed && self.holds_primed(target) {
            self.stats.publish_primed(target.generation);
            self.published_primed = target.generation;
            #[cfg(test)]
            self.shared
                .blocks_when_primed
                .store(self.shared.stats.blocks_filled(), Ordering::Release);
        }
    }

    /// Whether every stream holds, for `target`, what a play from its sample reads first: `primed`
    /// frames from there, or up to its last clip's end.
    fn holds_primed(&self, target: Target) -> bool {
        let tag = generation_tag(target.generation);
        self.tracks.iter().all(|track| {
            let extent = &track.stream.extent;
            let needed_from = target.from.max(extent.start);
            let needed_to = target.from.saturating_add(self.primed).min(extent.end);
            if needed_from >= needed_to {
                return true;
            }
            // Until the play starts, the stream is filled in a row from the block `needed_from`
            // lies in; after, it is primed already.
            let (generation, until) = track.stream.ready_until();
            generation == tag && until >= needed_to
        })
    }

    /// Times the ask that started this pass, once its first run has published.
    fn time_the_ask(&mut self) {
        if let Some(asked_at) = self.asked_at.take() {
            let nanoseconds = self.shared.since_epoch().saturating_sub(asked_at);
            self.shared
                .stats
                .record_refill_latency(Duration::from_nanos(nanoseconds));
        }
    }

    /// A test's panic in the reader's thread, when the test asked for one.
    #[cfg(test)]
    #[expect(
        clippy::panic,
        reason = "a test makes the reader's thread panic on purpose, to see what follows"
    )]
    fn panic_if_asked(&self) {
        if self.shared.panic_in_pass.load(Ordering::Acquire) {
            panic!("a test's panic in the disk reader, on purpose");
        }
    }

    fn fill_what_is_needed(&mut self) {
        self.blocked.fill(false);
        loop {
            if self.must_stop() {
                return;
            }
            let target = self.shared.target();
            if target.generation == 0 {
                return;
            }
            self.order.clear();
            for (index, track) in self.tracks.iter_mut().enumerate() {
                if self.blocked.get(index).copied().unwrap_or(true) {
                    continue;
                }
                if let Some(ahead) = track.wants(target, self.read_ahead, self.threshold) {
                    self.order.push((ahead, index));
                }
            }
            if self.order.is_empty() {
                return;
            }
            // One round: a run for each stream that wants one, the one that holds least first.
            let mut order = std::mem::take(&mut self.order);
            order.sort_unstable();
            for &(_, index) in &order {
                if self.must_stop() || self.shared.target() != target {
                    break;
                }
                if self.fill_run(index, target) {
                    self.time_the_ask();
                    self.publish_if_primed(target);
                } else if let Some(blocked) = self.blocked.get_mut(index) {
                    *blocked = true;
                }
            }
            self.order = order;
        }
    }

    fn must_stop(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire) || self.shared.paused.load(Ordering::SeqCst)
    }

    /// Fills one run of a stream: the blocks after those it filled in a row, up to a run's length,
    /// its window's end, or the first slot an older rolling play still holds. Returns whether it
    /// filled any.
    fn fill_run(&mut self, index: usize, target: Target) -> bool {
        let Self {
            shared,
            tracks,
            fade,
            read_ahead,
            scratch,
            ..
        } = self;
        let Some(track) = tracks.get_mut(index) else {
            return false;
        };
        let Some(window) = track.window(target, *read_ahead) else {
            return false;
        };
        let first = track.filled.end;
        let planned = first
            .saturating_add(track.blocks_per_run)
            .min(window.end_block);
        let end = track.unkept_end(first, planned, target.generation);
        if end <= first {
            return false;
        }
        let stream = &track.stream;
        let channels = stream.channels.max(1);
        let Some(mixed) = render(
            &track.clips,
            channels,
            target.from,
            first..end,
            fade.fade_in(),
            scratch,
            &shared.stats,
        ) else {
            return false;
        };
        let block_samples = BLOCK_FRAMES.saturating_mul(channels);
        for (block, frames) in (first..end).zip(mixed.chunks(block_samples)) {
            stream.write_block(target.generation, block, frames);
        }
        track.filled.end = end;
        track.filled.start = track
            .filled
            .start
            .max(end.saturating_sub(stream.slot_count()));
        stream.publish_ready(target.generation, end);
        shared.stats.count_blocks(end.saturating_sub(first) as u64);
        true
    }
}

impl TrackFill {
    /// The samples the stream should hold now for `target`: from where its player reads next in
    /// that generation, or from the target's sample for a player of another, to `read_ahead`
    /// further or its last clip's end. Starts its bookkeeping again, and publishes that nothing is
    /// ready, when the generation changed or the player left what is filled. `None` when nothing
    /// is left to hold.
    fn window(&mut self, target: Target, read_ahead: i64) -> Option<Window> {
        let extent = self.stream.extent.clone();
        let cursor = self.stream.cursor();
        let start = if cursor.generation == target.generation {
            cursor.position.max(target.from)
        } else {
            target.from
        };
        let start = start.max(extent.start);
        let first_block = block_of(start);
        let in_filled = (self.filled.start..=self.filled.end).contains(&first_block);
        if self.generation != target.generation || !in_filled {
            self.generation = target.generation;
            self.filled = first_block..first_block;
            self.stream.publish_ready(target.generation, first_block);
        }
        let end = start.saturating_add(read_ahead).min(extent.end);
        (end > start).then(|| Window {
            start,
            end_block: block_of(end.saturating_sub(1)).saturating_add(1),
        })
    }

    /// How many frames the stream holds ahead of its window's start, if it should be filled now:
    /// when it lacks a whole run, holds less than `threshold`, or its window reaches its last
    /// clip's end.
    fn wants(&mut self, target: Target, read_ahead: i64, threshold: i64) -> Option<i64> {
        let window = self.window(target, read_ahead)?;
        if self.filled.end >= window.end_block {
            return None;
        }
        let lacking = window.end_block.saturating_sub(self.filled.end);
        let ahead = first_sample(self.filled.end).saturating_sub(window.start);
        let last_block = block_of(self.stream.extent.end.saturating_sub(1)).saturating_add(1);
        let urgent = ahead < threshold || window.end_block == last_block;
        (lacking >= self.blocks_per_run || urgent).then_some(ahead)
    }

    /// The end of the blocks from `first` to `end` that can be written now: up to the first whose
    /// slot holds a block that an older rolling play may still read.
    fn unkept_end(&self, first: i64, end: i64, target_generation: u64) -> i64 {
        let Some(kept) = Kept::by(self.stream.cursor(), target_generation) else {
            return end;
        };
        (first..end)
            .find(|&block| {
                self.stream
                    .slot(block)
                    .is_some_and(|slot| kept.holds(slot.state.load(Ordering::Acquire)))
            })
            .unwrap_or(end)
    }
}

/// The bytes of one frame of a clip's file.
fn frame_bytes(clip: &AudioClip) -> usize {
    let info = clip.file.info();
    usize::from(info.channels).saturating_mul(info.sample_format.bytes_per_sample())
}

/// Mixes the clips over `blocks` into the scratch's mixed frames, interleaved, and returns them:
/// zeros, then each clip's frames times its gain, from `from` on; a clip that starts before `from`
/// also times the fade-in over the fade's length from `from`. `None` if the run does not fit the
/// scratch, which its sizing prevents.
fn render<'a>(
    clips: &[AudioClip],
    channels: usize,
    from: i64,
    blocks: Range<i64>,
    fade_in: &[f32],
    scratch: &'a mut Scratch,
    stats: &DiskStats,
) -> Option<&'a [f32]> {
    let run = first_sample(blocks.start)..first_sample(blocks.end);
    let frames = usize::try_from(run.end.saturating_sub(run.start)).ok()?;
    let samples = frames.checked_mul(channels)?;
    scratch.mixed.get_mut(..samples)?.fill(0.0);
    let render_from = run.start.max(from);
    let first_clip = clips.partition_point(|clip| clip.end().is_some_and(|end| end <= render_from));
    for clip in clips.get(first_clip..).unwrap_or_default() {
        if clip.start.0 >= run.end {
            break;
        }
        let Some(clip_end) = clip.end() else {
            continue;
        };
        let stretch = clip.start.0.max(render_from)..clip_end.min(run.end);
        if stretch.is_empty() {
            continue;
        }
        let fade = (clip.start.0 < from).then_some(Fade {
            from,
            gains: fade_in,
        });
        mix_clip(clip, stretch, run.start, channels, fade, scratch, stats);
    }
    scratch.mixed.get(..samples)
}

/// The fade-in of a clip that starts before the play's sample: its gains from `from` on.
#[derive(Clone, Copy)]
struct Fade<'a> {
    from: i64,
    gains: &'a [f32],
}

impl Fade<'_> {
    /// The fade-in's gain at a timeline sample, if the sample lies in the fade.
    fn gain_at(&self, sample: i64) -> Option<f32> {
        let index = usize::try_from(sample.checked_sub(self.from)?).ok()?;
        self.gains.get(index).copied()
    }
}

/// Reads a clip's frames over `stretch`, timeline samples it covers, and adds them, times its gain
/// and its fade, into the mixed frames of a run that starts at `run_start`. Frames its file does
/// not hold stay zero, and are counted.
fn mix_clip(
    clip: &AudioClip,
    stretch: Range<i64>,
    run_start: i64,
    channels: usize,
    fade: Option<Fade<'_>>,
    scratch: &mut Scratch,
    stats: &DiskStats,
) {
    let count = stretch.end.saturating_sub(stretch.start) as u64;
    let into_clip = stretch.start.saturating_sub(clip.start.0) as u64;
    let first_frame = clip.file_offset.saturating_add(into_clip);
    let Scratch {
        bytes,
        decoded,
        mixed,
    } = scratch;
    let got = read_frames(&clip.file, first_frame, count, bytes, decoded, stats);
    stats.count_unread(count.saturating_sub(got));
    let offset = usize::try_from(stretch.start.saturating_sub(run_start))
        .unwrap_or(usize::MAX)
        .saturating_mul(channels);
    let got_samples = usize::try_from(got)
        .unwrap_or(usize::MAX)
        .saturating_mul(channels);
    let target = mixed.get_mut(offset..).unwrap_or_default();
    let source = decoded.get(..got_samples).unwrap_or_default();
    let frames = target
        .chunks_exact_mut(channels)
        .zip(source.chunks_exact(channels));
    for (sample, (mixed_frame, decoded_frame)) in stretch.zip(frames) {
        let fade_gain = fade.and_then(|fade| fade.gain_at(sample));
        for (mixed, &decoded) in mixed_frame.iter_mut().zip(decoded_frame) {
            let value = decoded * clip.gain;
            *mixed += match fade_gain {
                Some(gain) => value * gain,
                None => value,
            };
        }
    }
}

/// Reads up to `count` frames of `file` from its frame `first_frame` in one read, and decodes them
/// into `decoded`. Returns how many it got: fewer past the file's end or after a failed read.
fn read_frames(
    file: &AudioFile,
    first_frame: u64,
    count: u64,
    bytes: &mut [u8],
    decoded: &mut [f32],
    stats: &DiskStats,
) -> u64 {
    let info = file.info();
    let frame_bytes =
        usize::from(info.channels).saturating_mul(info.sample_format.bytes_per_sample());
    let available = info.frames.saturating_sub(first_frame).min(count);
    let wanted = usize::try_from(available)
        .ok()
        .and_then(|frames| frames.checked_mul(frame_bytes));
    let offset = first_frame
        .checked_mul(frame_bytes as u64)
        .and_then(|bytes| bytes.checked_add(info.data_offset));
    let (Some(wanted), Some(offset)) = (wanted, offset) else {
        return 0;
    };
    let Some(buffer) = bytes.get_mut(..wanted) else {
        return 0;
    };
    let read = read_at(&file.file, buffer, offset, stats);
    let whole = read.checked_div(frame_bytes).unwrap_or(0);
    let Some(read) = buffer.get(..whole.saturating_mul(frame_bytes)) else {
        return 0;
    };
    decode(info.sample_format, info.channels, read, decoded) as u64
}

/// Reads `buffer` from `offset` of `file`, without moving any file position, until it is full or
/// the file ends; returns how many bytes it got. A failed read is counted, and ends it.
fn read_at(file: &File, buffer: &mut [u8], offset: u64, stats: &DiskStats) -> usize {
    let mut done = 0;
    while let Some(rest) = buffer.get_mut(done..).filter(|rest| !rest.is_empty()) {
        match file.read_at(rest, offset.saturating_add(done as u64)) {
            // The file ends here: a clip past its end, or a file cut short since it was opened.
            Ok(0) => break,
            Ok(read) => {
                stats.count_read(read);
                done = done.saturating_add(read);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                stats.count_read_error();
                break;
            }
        }
    }
    done
}
