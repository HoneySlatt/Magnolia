//! The disk reader's handle, its thread, and what it shares with the players.
//!
//! The reader's thread, `magnolia-disk`, sleeps until a locate or a player asks for a refill, then
//! fills every stream (`fill.rs`) and sleeps again. A player asks through one flag shared by all,
//! set once, and one wake, [`wake_disk_reader`], the call the real-time allowlist holds for it
//! ([ADR-0023](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0023-the-real-time-allowlist.md),
//! [ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
//! A reader made without a thread fills when its owner calls [`DiskReader::fill`], as an offline
//! render does before each block.

// The players read what this module shares on the real-time thread: nothing here may panic
// (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::error::Error;
use std::fmt;
use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use super::fill::{Amounts, Worker};
use super::player::AudioPlayer;
use super::stream::Stream;
use super::{AudioTrack, BLOCK_FRAMES, DiskConfig, DiskError, checked_clips};
use crate::callback_times::{CallbackTimes, Snapshot};
use crate::engine::{Cue, FadeTable, fade_frames};
use crate::seqlock::SeqLock;
use crate::stats::EngineStats;
use crate::time::SamplePosition;

/// How much each stream must hold from a cue's sample before a play from it is primed: half a
/// second, or half the read-ahead if shorter, which the reader always fills when it can.
const PRIMED_SECONDS_DIVISOR: u64 = 2;

/// The longest a waiting caller sleeps between two looks at the reader.
const WAIT_STEP: Duration = Duration::from_millis(1);

/// Wakes the disk reader's thread: a futex wake when it sleeps, nothing when it is awake already
/// (`Thread::unpark`). It is the one call on the real-time allowlist that the players need
/// ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)):
/// `rtsan-suppressions.txt` names this function, so RealtimeSanitizer lets its system call pass
/// and still reports any other. It is public so that the sanitizer's test can call it, and never
/// inlined, so that it shows on the call stack.
#[inline(never)]
pub fn wake_disk_reader(reader: &Thread) {
    reader.unpark();
}

/// What the reader's handle, its thread and the players share.
pub(crate) struct Shared {
    pub(crate) streams: Box<[Arc<Stream>]>,
    /// The last locate: its generation, then its first sample as an `i64`'s bits; generation 0
    /// before any. The handle alone writes it.
    pub(crate) target: SeqLock<2>,
    /// Whether a player or a locate asked the reader to fill since its last pass began.
    pub(crate) asked: AtomicBool,
    /// A test's pause: the reader fills nothing while it is set.
    pub(crate) paused: AtomicBool,
    /// Set when the handle is dropped: the thread ends.
    pub(crate) closed: AtomicBool,
    /// Set when the thread ended after a panic: nothing fills the streams any more.
    pub(crate) failed: AtomicBool,
    /// Passes begun and ended, so that a waiting caller can tell whether the reader is idle.
    pub(crate) passes_begun: AtomicU64,
    pub(crate) passes_ended: AtomicU64,
    pub(crate) stats: DiskStats,
    /// What the times in `asked_at` count from.
    epoch: Instant,
    /// When the player whose ask woke the reader asked, in nanoseconds from `epoch`; 0 once a pass
    /// has taken it. For `DiskStats::refill_latency`.
    pub(crate) asked_at: AtomicU64,
    /// Makes the reader's next pass panic, so that a test sees what follows.
    #[cfg(test)]
    pub(crate) panic_in_pass: AtomicBool,
    /// How many blocks the reader had filled when it last published a generation primed.
    #[cfg(test)]
    pub(crate) blocks_when_primed: AtomicU64,
}

/// The last locate, as the reader's thread reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) generation: u64,
    pub(crate) from: i64,
}

impl Shared {
    /// What the reader of `streams` shares, before any locate. The flag the players set is
    /// written through `black_box`, so that they never fault on its page (`engine/memory.rs`).
    pub(crate) fn new(streams: Box<[Arc<Stream>]>) -> Self {
        Self {
            streams,
            target: SeqLock::new([0, 0]),
            asked: AtomicBool::new(black_box(false)),
            paused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            passes_begun: AtomicU64::new(0),
            passes_ended: AtomicU64::new(0),
            stats: DiskStats::default(),
            epoch: Instant::now(),
            asked_at: AtomicU64::new(black_box(0)),
            #[cfg(test)]
            panic_in_pass: AtomicBool::new(false),
            #[cfg(test)]
            blocks_when_primed: AtomicU64::new(0),
        }
    }

    pub(crate) fn target(&self) -> Target {
        let [generation, from] = self.target.read();
        Target {
            generation,
            from: from as i64,
        }
    }

    /// Asks the reader to fill: sets the shared flag, and wakes the thread once, when the flag was
    /// not set already, noting when for `DiskStats::refill_latency`. A player asks after publishing
    /// its cursor. Every set of the flag, a player's, a locate's or the end of a pause, is a
    /// read-modify-write, even when the flag is set already, and the reader clears it by one as
    /// each pass begins: a chain of read-modify-writes carries every cursor published before an ask
    /// to the pass that clears it, where a plain store in the chain would cut it. Real-time safe:
    /// the wake is `wake_disk_reader`'s, and `Instant::now` reads the clock without a system call.
    pub(crate) fn ask(&self, thread: Option<&Thread>) {
        if self.asked.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.asked_at.compare_exchange(
            0,
            self.since_epoch(),
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        if let Some(thread) = thread {
            wake_disk_reader(thread);
        }
    }

    /// The time since `epoch`, in nanoseconds, at least 1.
    pub(crate) fn since_epoch(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos())
            .unwrap_or(u64::MAX)
            .max(1)
    }
}

/// What the disk reader says of itself: atomics only, readable from any thread.
#[derive(Debug, Default)]
pub struct DiskStats {
    blocks_filled: AtomicU64,
    reads: AtomicU64,
    bytes_read: AtomicU64,
    unread_frames: AtomicU64,
    read_errors: AtomicU64,
    refills: AtomicU64,
    refill_nanoseconds: AtomicU64,
    longest_refill_nanoseconds: AtomicU64,
    refill_latency: CallbackTimes,
}

impl DiskStats {
    /// From a player's ask that wakes the reader to the end of the reader's first run after it,
    /// which publishes the refill of the stream that held least: one time per such ask, in the
    /// histogram the callback times use.
    pub fn refill_latency(&self) -> Snapshot {
        self.refill_latency.snapshot()
    }

    pub(crate) fn record_refill_latency(&self, time: Duration) {
        self.refill_latency.record(time);
    }

    /// How many passes filled at least one block: the refills, the first fill after a locate
    /// included.
    pub fn refills(&self) -> u64 {
        self.refills.load(Ordering::Relaxed)
    }

    /// The time the refills took, all together, from the start of each pass to its end.
    pub fn refill_time(&self) -> Duration {
        Duration::from_nanos(self.refill_nanoseconds.load(Ordering::Relaxed))
    }

    /// The longest refill.
    pub fn longest_refill(&self) -> Duration {
        Duration::from_nanos(self.longest_refill_nanoseconds.load(Ordering::Relaxed))
    }

    /// Counts a pass that took `time`, if it filled any block. One thread fills at a time.
    pub(crate) fn record_pass(&self, time: Duration, blocks: u64) {
        if blocks == 0 {
            return;
        }
        let nanoseconds = u64::try_from(time.as_nanos()).unwrap_or(u64::MAX);
        self.refills.fetch_add(1, Ordering::Relaxed);
        self.refill_nanoseconds
            .fetch_add(nanoseconds, Ordering::Relaxed);
        self.longest_refill_nanoseconds
            .fetch_max(nanoseconds, Ordering::Relaxed);
    }

    /// How many blocks the reader has written into slots.
    pub fn blocks_filled(&self) -> u64 {
        self.blocks_filled.load(Ordering::Relaxed)
    }

    /// How many reads of a file it has made.
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// How many bytes those reads brought.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// How many frames of clips their file did not give, past its end or after a failed read,
    /// which the reader rendered as silence.
    pub fn unread_frames(&self) -> u64 {
        self.unread_frames.load(Ordering::Relaxed)
    }

    /// How many reads failed, for another reason than the file's end.
    pub fn read_errors(&self) -> u64 {
        self.read_errors.load(Ordering::Relaxed)
    }

    pub(crate) fn count_read(&self, bytes: usize) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes_read.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn count_blocks(&self, blocks: u64) {
        self.blocks_filled.fetch_add(blocks, Ordering::Relaxed);
    }

    pub(crate) fn count_unread(&self, frames: u64) {
        if frames > 0 {
            self.unread_frames.fetch_add(frames, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_read_error(&self) {
        self.read_errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Why [`DiskReader::wait_primed`] returned without the streams primed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimeError {
    /// The timeout passed.
    TimedOut,
    /// A later locate replaced the cue's generation: no stream fills it any more.
    Superseded,
    /// A reader without a thread filled what it could, and still lacks frames: an older play
    /// still rolls over the slots they go to. Play the cue, render, and fill again.
    NotPrimed,
    /// The reader's thread stopped after a panic: nothing fills the streams any more.
    ReaderStopped,
}

impl fmt::Display for PrimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::TimedOut => "the disk streams were not primed before the timeout",
            Self::Superseded => "a later locate replaced the cue",
            Self::NotPrimed => "an older play still holds the slots the cue's frames go to",
            Self::ReaderStopped => "the disk reader's thread stopped after a panic",
        };
        formatter.write_str(text)
    }
}

impl Error for PrimeError {}

/// Who fills the streams.
enum Driver {
    /// The reader's own thread.
    Thread {
        thread: Thread,
        handle: Option<JoinHandle<()>>,
    },
    /// The owner, on its own thread, through [`DiskReader::fill`].
    Caller(Box<Worker>),
}

/// The disk reader: it owns the audio tracks' clips and fills their streams, which their
/// [`AudioPlayer`]s play
/// ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
///
/// [`start`](Self::start) makes it with a thread of its own, which sleeps until a locate or a
/// player wakes it; [`offline`](Self::offline) without one, for a render that fills before each
/// block. To play from a sample: [`locate`](Self::locate), which gives a [`Cue`], then
/// `Controller::play(cue)`, at once, stopped or rolling: the play starts once the reader has primed
/// its cue, every stream holding half a second from there. [`wait_primed`](Self::wait_primed)
/// waits for the same, for a caller that wants a timeout and an error. While an older play rolls,
/// the reader leaves the slots it may still read: everything ahead of it until its stop is asked,
/// then up to that stop.
///
/// Its generations come from the engine's statistics, so that a reader made again on the same
/// engine, or a second one, never reuses one. Dropping it stops its thread, after the read under
/// way. The players it made then play silence, counting it as underruns.
pub struct DiskReader {
    shared: Arc<Shared>,
    driver: Driver,
    /// The engine's statistics: its generations, and where the players count what they lack.
    stats: Arc<EngineStats>,
    /// The generation of this reader's last locate; 0 before any.
    generation: u64,
}

/// What a reader is built from, before its driver.
struct Parts {
    worker: Worker,
    players: PlayerParts,
}

/// What the players of a reader share.
struct PlayerParts {
    shared: Arc<Shared>,
    /// The fade they fade out with.
    fade: Arc<FadeTable>,
    /// Below this many frames ready, a player asks for a refill.
    threshold: i64,
}

impl PlayerParts {
    /// One player per stream, in the tracks' order, which wakes `reader`, the reader's thread if
    /// it has one, and counts what it lacks in `stats`.
    fn players(&self, reader: Option<&Thread>, stats: &Arc<EngineStats>) -> Vec<AudioPlayer> {
        self.shared
            .streams
            .iter()
            .map(|stream| {
                AudioPlayer::new(
                    Arc::clone(stream),
                    Arc::clone(&self.shared),
                    reader.cloned(),
                    Arc::clone(stats),
                    Arc::clone(&self.fade),
                    self.threshold,
                )
            })
            .collect()
    }
}

impl DiskReader {
    /// A reader for `tracks`, with a thread of its own, `magnolia-disk`, and one player per track,
    /// in their order. Every file must be at the engine's rate and have its track's channels; a
    /// track's clips may not overlap. The players count what they lack in `stats`, the engine's.
    ///
    /// # Errors
    ///
    /// A [`DiskError`] for a configuration or a track out of range, or a thread that did not
    /// start.
    pub fn start(
        config: DiskConfig,
        tracks: &[AudioTrack],
        stats: &Arc<EngineStats>,
    ) -> Result<(Self, Vec<AudioPlayer>), DiskError> {
        let Parts { worker, players } = Self::parts(config, tracks, stats)?;
        let handle = thread::Builder::new()
            .name("magnolia-disk".into())
            .spawn(move || serve(worker))
            .map_err(DiskError::ThreadDidNotStart)?;
        let thread = handle.thread().clone();
        let reader = Self {
            shared: Arc::clone(&players.shared),
            driver: Driver::Thread {
                thread: thread.clone(),
                handle: Some(handle),
            },
            stats: Arc::clone(stats),
            generation: 0,
        };
        Ok((reader, players.players(Some(&thread), stats)))
    }

    /// A reader for `tracks` without a thread: its streams fill only when its owner calls
    /// [`fill`](Self::fill), as an offline render does before each block. Otherwise as
    /// [`start`](Self::start).
    ///
    /// # Errors
    ///
    /// A [`DiskError`] for a configuration or a track out of range.
    pub fn offline(
        config: DiskConfig,
        tracks: &[AudioTrack],
        stats: &Arc<EngineStats>,
    ) -> Result<(Self, Vec<AudioPlayer>), DiskError> {
        let Parts { worker, players } = Self::parts(config, tracks, stats)?;
        let reader = Self {
            shared: Arc::clone(&players.shared),
            driver: Driver::Caller(Box::new(worker)),
            stats: Arc::clone(stats),
            generation: 0,
        };
        Ok((reader, players.players(None, stats)))
    }

    /// Checks the configuration and the tracks, and builds the streams and the worker that fills
    /// them, which publishes the generations it primes in `stats`.
    fn parts(
        config: DiskConfig,
        tracks: &[AudioTrack],
        stats: &Arc<EngineStats>,
    ) -> Result<Parts, DiskError> {
        let read_ahead = config.read_ahead;
        if !(DiskConfig::MIN_READ_AHEAD..=DiskConfig::MAX_READ_AHEAD).contains(&read_ahead) {
            return Err(DiskError::ReadAheadOutOfRange(read_ahead));
        }
        // The blocks the window spans, at most one more than it holds whole, and one spare, so that
        // two blocks the reader wants at once never share a slot.
        let slot_count = read_ahead.div_ceil(BLOCK_FRAMES as u64).saturating_add(2) as usize;
        // At most 2^28: no overflow below.
        let read_ahead = read_ahead as i64;
        let mut clips = Vec::with_capacity(tracks.len());
        let mut streams = Vec::with_capacity(tracks.len());
        for (index, track) in tracks.iter().enumerate() {
            let checked = checked_clips(index, track, config.sample_rate)?;
            let extent = match (checked.first(), checked.last()) {
                (Some(first), Some(last)) => first.start.0..last.end().unwrap_or(first.start.0),
                _ => 0..0,
            };
            let stream = Arc::new(Stream::new(usize::from(track.channels), extent, slot_count));
            clips.push(checked);
            streams.push(stream);
        }
        let shared = Arc::new(Shared::new(streams.iter().map(Arc::clone).collect()));
        let fade = Arc::new(FadeTable::new(fade_frames(config.sample_rate)));
        let threshold = read_ahead / 2;
        let primed_frames = (u64::from(config.sample_rate.hertz()) / PRIMED_SECONDS_DIVISOR)
            .min(threshold as u64) as i64;
        let worker = Worker::new(
            Arc::clone(&shared),
            Arc::clone(stats),
            streams.into_iter().zip(clips).collect(),
            Arc::clone(&fade),
            Amounts {
                read_ahead,
                threshold,
                primed: primed_frames,
            },
        );
        Ok(Parts {
            worker,
            players: PlayerParts {
                shared,
                fade,
                threshold,
            },
        })
    }

    /// Starts a new generation from `from`, the next of the engine's: the reader fills every
    /// stream from there, and leaves the older generations, but for the frames a rolling play may
    /// still read. Returns the cue a play from there takes; it plays once the reader has primed it.
    /// Never waits; a reader without a thread fills at its next [`fill`](Self::fill).
    pub fn locate(&mut self, from: SamplePosition) -> Cue {
        self.generation = self.stats.next_generation();
        self.shared.target.write([self.generation, from.0 as u64]);
        if let Driver::Thread { thread, .. } = &self.driver {
            // A read-modify-write, as every set of the flag (`Shared::ask`).
            self.shared.asked.swap(true, Ordering::AcqRel);
            thread.unpark();
        }
        Cue::new(from, self.generation)
    }

    /// Waits until the reader has primed `cue`: every stream holds what a play from it reads first,
    /// half a second from its sample, or half the read-ahead if shorter, or up to its last clip's
    /// end. A play of the cue waits for the same, without a timeout; this is for a caller that
    /// wants one, and an error. A cue of generation 0 reads no stream, and is primed at once. A
    /// reader without a thread fills first, then answers without waiting.
    ///
    /// # Errors
    ///
    /// [`PrimeError::Superseded`] for a cue of another locate than this reader's last;
    /// [`PrimeError::ReaderStopped`] when its thread has stopped; [`PrimeError::TimedOut`];
    /// [`PrimeError::NotPrimed`] for a reader without a thread whose fill could not prime.
    #[expect(
        clippy::disallowed_methods,
        reason = "the caller's thread waits for the disk reader; it never runs on the real-time path"
    )]
    pub fn wait_primed(&mut self, cue: Cue, timeout: Duration) -> Result<(), PrimeError> {
        if cue.generation() == 0 {
            return Ok(());
        }
        if cue.generation() != self.generation {
            return Err(PrimeError::Superseded);
        }
        let primed = |stats: &EngineStats| stats.primed_generation() >= cue.generation();
        if let Driver::Caller(worker) = &mut self.driver {
            worker.pass();
            return if primed(&self.stats) {
                Ok(())
            } else {
                Err(PrimeError::NotPrimed)
            };
        }
        let start = Instant::now();
        loop {
            if self.shared.failed.load(Ordering::Acquire) {
                return Err(PrimeError::ReaderStopped);
            }
            if primed(&self.stats) {
                return Ok(());
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(PrimeError::TimedOut);
            }
            thread::sleep(WAIT_STEP.min(timeout.saturating_sub(elapsed)));
        }
    }

    /// For a reader made without a thread, [`offline`](Self::offline): fills every stream now, on
    /// the caller's thread, up to its read-ahead from where its player reads next, or from the last
    /// locate's sample for a player that has not started its play. A render calls it before each
    /// block, no longer than half the read-ahead, and never underruns. Returns whether it filled;
    /// a reader with a thread leaves that to its thread, and returns `false`.
    pub fn fill(&mut self) -> bool {
        match &mut self.driver {
            Driver::Caller(worker) => {
                worker.pass();
                true
            }
            Driver::Thread { .. } => false,
        }
    }

    /// What the reader says of itself.
    pub fn stats(&self) -> &DiskStats {
        &self.shared.stats
    }

    /// The fewest frames a track's player found ready ahead of it since its play started: its
    /// low-water mark. `None` for a track out of range, or before the player found any supply.
    pub fn low_water(&self, track: usize) -> Option<u64> {
        self.shared.streams.get(track)?.low_water()
    }

    /// Pauses the reader's thread, or lets it go on: while paused it fills nothing, as a disk that
    /// stalls. For tests.
    #[doc(hidden)]
    pub fn pause(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
        if let Driver::Thread { thread, .. } = &self.driver
            && !paused
        {
            // A read-modify-write, as every set of the flag (`Shared::ask`).
            self.shared.asked.swap(true, Ordering::AcqRel);
            thread.unpark();
        }
    }

    /// Waits until the reader's thread has done all it was asked: no request pending, and no pass
    /// under way. Returns whether it got there before `timeout`. For tests, which play periods
    /// without waiting on time.
    #[doc(hidden)]
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's thread waits for the disk reader; it never runs on the real-time path"
    )]
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            let asked = self.shared.asked.load(Ordering::SeqCst);
            let begun = self.shared.passes_begun.load(Ordering::SeqCst);
            let ended = self.shared.passes_ended.load(Ordering::SeqCst);
            if !asked && begun == ended {
                return true;
            }
            if start.elapsed() >= timeout || self.shared.failed.load(Ordering::Acquire) {
                return false;
            }
            thread::sleep(Duration::from_micros(50));
        }
    }
}

impl Drop for DiskReader {
    /// Stops the reader's thread and waits for it, so that its files are closed once the reader is
    /// gone. Runs off the real-time thread.
    #[expect(
        clippy::disallowed_methods,
        reason = "the owner's thread waits for the disk reader's thread to end; never on the real-time path"
    )]
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Driver::Thread { thread, handle } = &mut self.driver {
            thread.unpark();
            if let Some(handle) = handle.take() {
                // A thread that panicked has said so through `failed`; nothing more to report here.
                let _ = handle.join();
            }
        }
    }
}

/// The reader's thread: fills the streams whenever asked, sleeps otherwise, and ends once its
/// handle is dropped. A panic ends it too, after setting `failed`.
fn serve(mut worker: Worker) {
    let shared = worker.shared();
    let served = catch_unwind(AssertUnwindSafe(|| {
        serve_until_closed(&mut worker, &shared)
    }));
    if served.is_err() {
        shared.failed.store(true, Ordering::Release);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the disk reader's thread sleeps until a locate or a player wakes it; it never runs on the real-time path"
)]
fn serve_until_closed(worker: &mut Worker, shared: &Shared) {
    loop {
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        if !shared.paused.load(Ordering::SeqCst) {
            shared.passes_begun.fetch_add(1, Ordering::SeqCst);
            // Cleared before the pass reads the cursors, by a read-modify-write that sees every
            // cursor published before the asks it clears (`Shared::ask`). A player that asks again
            // during the pass finds it clear, sets it, and the loop runs once more.
            shared.asked.swap(false, Ordering::AcqRel);
            worker.pass();
            shared.passes_ended.fetch_add(1, Ordering::SeqCst);
        }
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        if shared.paused.load(Ordering::SeqCst) || !shared.asked.load(Ordering::SeqCst) {
            thread::park();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::SampleRate;

    type TestResult = Result<(), Box<dyn Error>>;

    /// A reader whose thread panicked fills nothing more: `wait_primed` says so at once, rather
    /// than waiting for its timeout, and the reader is never idle again.
    #[test]
    fn a_reader_whose_thread_stopped_after_a_panic_says_so() -> TestResult {
        let stats = EngineStats::new_written();
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let (mut reader, _) = DiskReader::start(DiskConfig::new(rate), &[], &stats)?;
        reader.shared.panic_in_pass.store(true, Ordering::Release);
        let cue = reader.locate(SamplePosition(0));
        let start = Instant::now();
        assert_eq!(
            reader.wait_primed(cue, Duration::from_secs(30)),
            Err(PrimeError::ReaderStopped)
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "it did not wait out its timeout"
        );
        assert!(!reader.wait_idle(Duration::from_millis(100)));
        assert_eq!(stats.primed_generation(), 0, "nothing was primed");
        Ok(())
    }

    /// A play waits for its cue's priming, not for the whole read-ahead: the reader publishes the
    /// generation primed after the run that primes it, though the pass goes on filling. One mono
    /// float track, 64 blocks ahead, filled 16 blocks per run, primed at 6.
    #[test]
    fn a_generation_is_published_primed_as_soon_as_it_is() -> TestResult {
        let path =
            std::env::temp_dir().join(format!("magnolia-unit-primed-{}.wav", std::process::id()));
        let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        let mut writer =
            crate::wav::WavWriter::new(file, crate::wav::SampleFormat::Float32, 1, 48_000)?;
        writer.write_frames(&[0.5; 300_000])?;
        writer.finish()?;
        let track = AudioTrack {
            channels: 1,
            clips: vec![super::super::AudioClip {
                file: super::super::AudioFile::open(&path)?,
                start: SamplePosition(0),
                file_offset: 0,
                length: 300_000,
                gain: 1.0,
            }],
        };
        std::fs::remove_file(&path)?;
        let stats = EngineStats::new_written();
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let config = DiskConfig {
            sample_rate: rate,
            read_ahead: 64 * BLOCK_FRAMES as u64,
        };
        let (mut reader, _) = DiskReader::offline(config, &[track], &stats)?;
        let cue = reader.locate(SamplePosition(0));
        assert!(reader.fill());
        assert_eq!(stats.primed_generation(), cue.generation());
        assert_eq!(
            reader.stats().blocks_filled(),
            64,
            "the pass filled the whole window"
        );
        assert_eq!(
            reader.shared.blocks_when_primed.load(Ordering::Acquire),
            16,
            "published primed after the first run"
        );
        Ok(())
    }
}
