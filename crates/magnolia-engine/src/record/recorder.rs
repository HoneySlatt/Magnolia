//! The recorder: its handle, its thread, `magnolia-recorder`, which writes each take as its frames
//! come, and what it says of itself.
//!
//! The thread sleeps until a tap asks it to drain, through the flag every tap of the recorder
//! shares and [`wake_recorder`](super::wake_recorder). Each pass drains every track's rings in the
//! order `marks.rs` gives, writes the frames straight to the take's file, a gap as zeros, and
//! queues for the sync threads (`syncer.rs`) a sync of each take after each second of its audio,
//! and each take's end. It then makes the next take's file of each track that lacks one, so that a
//! take never waits for its file, and has the folder synced once for the batch.

use std::fs::{self, File};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::files::{
    AheadFile, FolderLock, SyncFile, TakeFile, make_take_file, next_take_number, open_folder,
    sanitized,
};
use super::marks::{self, EndReason, Mark, MarkKind, Next};
use super::recover::{Recovery, recover_locked};
use super::syncer::{self, Ending, Job, SyncContext, SyncQueue};
use super::tap::{RecorderLink, TapRings, TrackTap, WakeFlag};
use super::{ORIGINATOR, RecordConfig, RecordError, RecordTrack, Tap};
use crate::stats::EngineStats;
use crate::time::SamplePosition;
use crate::wav::{Bext, SampleFormat, WavError, WavWriter};

/// The most channels a take may have, as a WAV file may.
const MAX_TAKE_CHANNELS: u16 = 1_024;

/// The most bytes a track's ring of frames may take: 1 GiB.
const MAX_RING_BYTES: usize = 1 << 30;

/// The frames of zeros written at once for a gap.
const ZERO_FRAMES: usize = 1_024;

/// What the recorder says of itself: atomics only, readable from any thread.
#[derive(Debug, Default)]
pub struct RecorderStats {
    takes_started: AtomicU64,
    takes_finished: AtomicU64,
    frames_written: AtomicU64,
    gaps: AtomicU64,
    frames_padded: AtomicU64,
    write_errors: AtomicU64,
    latency_ends: AtomicU64,
    passes: AtomicU64,
    pass_nanoseconds: AtomicU64,
    longest_pass_nanoseconds: AtomicU64,
    syncs: AtomicU64,
    sync_nanoseconds: AtomicU64,
    longest_sync_nanoseconds: AtomicU64,
}

impl RecorderStats {
    /// How many takes started: their start mark reached the recorder.
    pub fn takes_started(&self) -> u64 {
        self.takes_started.load(Ordering::Relaxed)
    }

    /// How many takes were finished: their frames synced, their sizes written and synced. A take
    /// without frames, or whose writes failed, is not counted.
    pub fn takes_finished(&self) -> u64 {
        self.takes_finished.load(Ordering::Relaxed)
    }

    /// How many frames of the device the recorder has written into takes' files, padding apart:
    /// each counted once its write has returned, so a kill keeps it.
    pub fn frames_written(&self) -> u64 {
        self.frames_written.load(Ordering::Relaxed)
    }

    /// How many gaps the taps marked, where they dropped frames.
    pub fn gaps(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }

    /// How many frames of zeros the recorder wrote in place of frames the taps dropped, in gaps and
    /// at takes' ends. While every dropped frame lies in a take that ends, it equals
    /// [`EngineStats::capture_overruns`].
    pub fn frames_padded(&self) -> u64 {
        self.frames_padded.load(Ordering::Relaxed)
    }

    /// How many writes, syncs or files failed: a take's write, the making of a take's file, a sync
    /// of a take or of the folder, a take's sizes.
    pub fn write_errors(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }

    /// How many takes a change of the device's latency ended (ADR-0031).
    pub fn latency_ends(&self) -> u64 {
        self.latency_ends.load(Ordering::Relaxed)
    }

    /// How many passes of the recorder's thread did something: drained a ring, made or removed a
    /// file, queued a sync.
    pub fn passes(&self) -> u64 {
        self.passes.load(Ordering::Relaxed)
    }

    /// The time those passes took, all together.
    pub fn pass_time(&self) -> Duration {
        Duration::from_nanos(self.pass_nanoseconds.load(Ordering::Relaxed))
    }

    /// The longest pass: how long the rings may wait for the recorder's thread, beyond its wake.
    pub fn longest_pass(&self) -> Duration {
        Duration::from_nanos(self.longest_pass_nanoseconds.load(Ordering::Relaxed))
    }

    /// How many syncs the sync threads made: of takes' data and sizes, and of the folder.
    pub fn syncs(&self) -> u64 {
        self.syncs.load(Ordering::Relaxed)
    }

    /// The time those syncs took, all together.
    pub fn sync_time(&self) -> Duration {
        Duration::from_nanos(self.sync_nanoseconds.load(Ordering::Relaxed))
    }

    /// The longest sync.
    pub fn longest_sync(&self) -> Duration {
        Duration::from_nanos(self.longest_sync_nanoseconds.load(Ordering::Relaxed))
    }

    pub(crate) fn count_write_error(&self) {
        self.write_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_take_finished(&self) {
        self.takes_finished.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_sync(&self, time: Duration) {
        let nanoseconds = u64::try_from(time.as_nanos()).unwrap_or(u64::MAX);
        self.syncs.fetch_add(1, Ordering::Relaxed);
        self.sync_nanoseconds
            .fetch_add(nanoseconds, Ordering::Relaxed);
        self.longest_sync_nanoseconds
            .fetch_max(nanoseconds, Ordering::Relaxed);
    }

    fn record_pass(&self, time: Duration) {
        let nanoseconds = u64::try_from(time.as_nanos()).unwrap_or(u64::MAX);
        self.passes.fetch_add(1, Ordering::Relaxed);
        self.pass_nanoseconds
            .fetch_add(nanoseconds, Ordering::Relaxed);
        self.longest_pass_nanoseconds
            .fetch_max(nanoseconds, Ordering::Relaxed);
    }
}

/// How a take ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeEnd {
    /// It reached the end of its play's audible region: the transport stopped.
    Stopped,
    /// Its track was disarmed.
    Disarmed,
    /// The device's latency changed during it (ADR-0031).
    LatencyChanged,
    /// The engine's processor dropped its tap without ending it, as when the backend closed.
    TapDropped,
    /// A panic stopped the capture tap ([`EngineStats::tap_stopped`]).
    TapStopped,
    /// The recorder ended while it recorded.
    RecorderClosed,
}

/// One take that ended, as [`Recorder::takes`] lists it.
#[derive(Clone, Debug)]
pub struct TakeSummary {
    /// The track's number.
    pub track: u32,
    /// The number of the play it recorded, as `Controller::play` returned it.
    pub play: u64,
    /// The take's number on its track; `None` when it had no file.
    pub take: Option<u32>,
    /// Its file; `None` for a take without frames, whose file was removed, or one whose file could
    /// not be made.
    pub path: Option<PathBuf>,
    /// The timeline sample of its first frame, its `bext` time reference.
    pub first_sample: SamplePosition,
    /// The round trip that placed it, in frames.
    pub round_trip: u64,
    /// Its frames in the file, zeros in place of dropped frames included.
    pub frames: u64,
    /// Of those, the zeros.
    pub padded: u64,
    /// How it ended.
    pub ended: TakeEnd,
    /// The write, sync or file that failed, if one did: the take ended there, its file left as a
    /// crash leaves it, which recovery repairs.
    pub error: Option<Arc<WavError>>,
}

/// The ended takes, in the order they were reported.
#[expect(
    clippy::disallowed_types,
    reason = "the recorder's and the sync threads' list of ended takes, read by the handle; never on the real-time path"
)]
#[derive(Default)]
pub(crate) struct TakeList(std::sync::Mutex<Vec<TakeSummary>>);

impl TakeList {
    pub(crate) fn push(&self, summary: TakeSummary) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(summary);
    }

    fn all(&self) -> Vec<TakeSummary> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// How full a track's rings have been at their fullest, as [`Recorder::fullest_fill`] gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingFill {
    /// Of its ring of frames, in frames.
    pub frames: u64,
    /// Of its ring of marks.
    pub marks: u64,
}

/// What the recorder's handle and its thread share.
struct Shared {
    flag: Arc<WakeFlag>,
    /// The tracks the handle has made taps for, which the thread has not yet taken.
    #[expect(
        clippy::disallowed_types,
        reason = "the handle hands new tracks to the recorder's thread; never on the real-time path"
    )]
    inbox: std::sync::Mutex<Vec<TrackWriter>>,
    closed: AtomicBool,
    paused: AtomicBool,
    /// Set when the thread ended after a panic.
    failed: AtomicBool,
    passes_begun: AtomicU64,
    passes_ended: AtomicU64,
    /// A test's hook: every write to a take's file fails while it is set.
    fail_writes: Arc<AtomicBool>,
    engine: Arc<EngineStats>,
    stats: Arc<RecorderStats>,
    takes: Arc<TakeList>,
    queue: Arc<SyncQueue>,
    #[cfg(test)]
    logs: TestLogs,
}

/// A unit test's logs of the takes' file operations.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestLogs {
    pub(crate) enabled: AtomicBool,
    #[expect(
        clippy::disallowed_types,
        reason = "a unit test's logs, written by the recorder's thread"
    )]
    pub(crate) logs: std::sync::Mutex<Vec<(PathBuf, super::files::OpLog)>>,
}

impl Shared {
    /// Asks the thread for a pass: a read-modify-write, as every set of the flag.
    fn ask(&self, thread: &Thread) {
        self.flag.asked.swap(true, Ordering::AcqRel);
        thread.unpark();
    }
}

/// What every take of the recorder is written with.
struct TakeContext {
    folder: PathBuf,
    folder_file: Arc<File>,
    format: SampleFormat,
    rate: u32,
    /// The recorder's start, in nanoseconds since the Unix epoch: its takes' references hold it.
    started: u64,
}

/// The recorder: it writes the takes of the tracks it makes taps for into WAV files that survive a
/// crash, in its folder
/// ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md),
/// [ADR-0032](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0032-recording-by-a-capture-tap-of-the-processor.md)).
///
/// [`start`](Self::start) takes the folder's exclusive lock, which it holds for its life, recovers
/// what a crash left there ([`recover`](super::recover)), and starts its thread,
/// `magnolia-recorder`, and its sync threads, `magnolia-sync`. [`tap`](Self::tap) makes an armed
/// track's tap, which [`Controller::arm`](crate::engine::Controller::arm) arms.
///
/// Each take is a file named `<track number>-<track name>-take<n>.wav`, `n` continuing past the
/// files of that track and name already there, made empty ahead of its take: the next take's file
/// of each armed track always exists. Its frames go to the file as they come, 32-bit floats by
/// default ([`TakeFormat`](super::TakeFormat)), behind a header whose sizes say unknown and a
/// `bext` chunk: originator `Magnolia`, a reference unique to the take, the time reference of its
/// first timeline sample. The take's data is synced after each second of its audio, and at its end,
/// once its sizes are written, on the sync threads. A gap is written as zeros. A take that ends
/// without frames removes its file.
///
/// A write that fails ends its take: the file is left as a crash leaves it, the error counted in
/// [`RecorderStats::write_errors`] and given in the take's [`TakeSummary`]; the take's later frames
/// are drained and dropped.
///
/// Dropping it drains what the rings hold, ends the takes under way, removes the files made ahead,
/// waits for the sync threads, then releases the folder's lock. Drop it once its tracks are
/// disarmed: a tap still armed fills its ring, then drops frames, counted as overruns.
pub struct Recorder {
    shared: Arc<Shared>,
    thread: Thread,
    writer: Option<JoinHandle<()>>,
    link: Arc<RecorderLink>,
    config: RecordConfig,
    folder_file: Arc<File>,
    recovery: Recovery,
    /// The rings' fills of each tap made, by track.
    fills: Vec<(u32, Arc<super::tap::RingFills>)>,
    /// The folder's lock, released once everything above is done.
    _lock: FolderLock,
}

impl Recorder {
    /// Starts a recorder in `config.folder`, for the engine whose statistics are `stats`: takes the
    /// folder's lock, recovers it, starts the threads.
    ///
    /// # Errors
    ///
    /// A [`RecordError`] for a configuration out of range, a folder that cannot be opened, locked
    /// or recovered, one that another recorder holds, or a thread that did not start.
    pub fn start(config: RecordConfig, stats: &Arc<EngineStats>) -> Result<Self, RecordError> {
        config.check()?;
        let folder_file = Arc::new(open_folder(&config.folder)?);
        let lock = FolderLock::take(&config.folder)?;
        let recovery = recover_locked(&config.folder)?;
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        let recorder_stats = Arc::new(RecorderStats::default());
        let takes = Arc::new(TakeList::default());
        let queue = Arc::new(SyncQueue::new());
        let sync_context = Arc::new(SyncContext {
            queue: Arc::clone(&queue),
            stats: Arc::clone(&recorder_stats),
            takes: Arc::clone(&takes),
        });
        let sync_threads = syncer::start(&sync_context, config.sync_threads)
            .map_err(RecordError::ThreadDidNotStart)?;
        let shared = Arc::new(Shared {
            flag: Arc::new(WakeFlag::new()),
            inbox: Default::default(),
            closed: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            passes_begun: AtomicU64::new(0),
            passes_ended: AtomicU64::new(0),
            fail_writes: Arc::new(AtomicBool::new(false)),
            engine: Arc::clone(stats),
            stats: recorder_stats,
            takes,
            queue: Arc::clone(&queue),
            #[cfg(test)]
            logs: TestLogs::default(),
        });
        let writer = Writer {
            shared: Arc::clone(&shared),
            context: TakeContext {
                folder: config.folder.clone(),
                folder_file: Arc::clone(&folder_file),
                format: config.format.sample_format(),
                rate: config.sample_rate.hertz(),
                started,
            },
            tracks: Vec::new(),
            sync_threads,
        };
        let spawned = thread::Builder::new()
            .name("magnolia-recorder".into())
            .spawn(move || serve(writer));
        let handle = match spawned {
            Ok(handle) => handle,
            Err(error) => {
                // The writer and its sync threads' handles went down with the closure: the threads
                // end once their queue is closed.
                queue.close();
                return Err(RecordError::ThreadDidNotStart(error));
            }
        };
        let thread = handle.thread().clone();
        let link = Arc::new(RecorderLink {
            flag: Arc::clone(&shared.flag),
            thread: thread.clone(),
            extra_latency: u64::from(config.extra_latency),
            #[cfg(test)]
            panic_in_tap: AtomicBool::new(false),
        });
        Ok(Self {
            shared,
            thread,
            writer: Some(handle),
            link,
            config,
            folder_file,
            recovery,
            fills: Vec::new(),
            _lock: lock,
        })
    }

    /// Makes the tap of `track`: its rings, written through, and its first take's file, made empty
    /// ahead, numbered past the track's takes already in the folder. Arm it with
    /// [`Controller::arm`](crate::engine::Controller::arm); a tap dropped unarmed, or disarmed,
    /// leaves the recorder, which removes its track's file made ahead.
    ///
    /// # Errors
    ///
    /// [`RecordError::ChannelsOutOfRange`] for no channel or more than 1,024;
    /// [`RecordError::RingTooLarge`] for a ring past 1 GiB; [`RecordError::Folder`] when the folder
    /// cannot be read; [`RecordError::TakeFile`] when the take's file cannot be made.
    pub fn tap(&mut self, track: RecordTrack) -> Result<Tap, RecordError> {
        let channels = u16::try_from(track.capture_channels.len())
            .ok()
            .filter(|channels| (1..=MAX_TAKE_CHANNELS).contains(channels))
            .ok_or(RecordError::ChannelsOutOfRange {
                track: track.number,
                channels: track.capture_channels.len(),
            })?;
        let ring_frames = usize::try_from(self.config.ring_frames).unwrap_or(usize::MAX);
        let bytes = ring_frames
            .saturating_mul(usize::from(channels))
            .saturating_mul(size_of::<f32>());
        if bytes > MAX_RING_BYTES {
            return Err(RecordError::RingTooLarge {
                track: track.number,
                bytes,
            });
        }
        let name = sanitized(&track.name);
        let folder = &self.config.folder;
        let mut next_take =
            next_take_number(folder, track.number, &name).map_err(|error| RecordError::Folder {
                folder: folder.clone(),
                error,
            })?;
        let ahead = make_take_file(folder, track.number, &name, &mut next_take)
            .map_err(RecordError::TakeFile)?;
        self.shared.queue.sync_folder(&self.folder_file);
        let (tap, rings) = TrackTap::new(
            track.number,
            track.capture_channels.into_boxed_slice(),
            ring_frames,
            Arc::clone(&self.link),
        );
        self.fills.push((track.number, Arc::clone(&rings.fills)));
        let writer = TrackWriter {
            track: track.number,
            name,
            channels,
            rings,
            consumed: 0,
            ahead: Some(ahead),
            next_take,
            take: None,
            zeros: vec![0.0; ZERO_FRAMES.saturating_mul(usize::from(channels))].into_boxed_slice(),
            gone: false,
        };
        self.shared
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(writer);
        self.shared.ask(&self.thread);
        Ok(Tap {
            track: Box::new(tap),
        })
    }

    /// What [`recover`](super::recover) found in the folder when the recorder started.
    pub fn recovery(&self) -> &Recovery {
        &self.recovery
    }

    /// The recording folder.
    pub fn folder(&self) -> &Path {
        &self.config.folder
    }

    /// What the recorder says of itself.
    pub fn stats(&self) -> &RecorderStats {
        &self.shared.stats
    }

    /// Every take that has ended, in the order its end was done: synced, its sizes written, or
    /// failed. The sync threads end takes in parallel, so takes that end together may come in any
    /// order.
    pub fn takes(&self) -> Vec<TakeSummary> {
        self.shared.takes.all()
    }

    /// How full the rings of `track`'s last tap have been at their fullest: their high-water marks.
    /// `None` for a track the recorder made no tap for.
    pub fn fullest_fill(&self, track: u32) -> Option<RingFill> {
        let (_, fills) = self
            .fills
            .iter()
            .rev()
            .find(|(number, _)| *number == track)?;
        Some(RingFill {
            frames: fills.frames.load(Ordering::Relaxed),
            marks: fills.marks.load(Ordering::Relaxed),
        })
    }

    /// Whether the recorder's thread ended after a panic: nothing drains the rings any more, and the
    /// takes under way stay as a crash leaves them.
    pub fn has_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    /// Pauses the recorder's thread, or lets it go on: while paused it drains nothing, as a disk
    /// that stalls. A pass under way finishes first: wait until idle before pausing for a stall
    /// that starts at a known frame. For tests.
    #[doc(hidden)]
    pub fn pause(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
        if !paused {
            self.shared.ask(&self.thread);
        }
    }

    /// Makes every later write to a take's file fail, or no longer. For tests.
    #[doc(hidden)]
    pub fn fail_writes(&self, fail: bool) {
        self.shared.fail_writes.store(fail, Ordering::SeqCst);
    }

    /// Waits until the recorder has done all it was asked: no drain asked, no pass under way, no
    /// track waiting to be taken, no sync waiting or under way. Returns whether it got there before
    /// `timeout`. For tests, which play periods without waiting on time.
    #[doc(hidden)]
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's thread waits for the recorder's threads; never on the real-time path"
    )]
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            let asked = self.shared.flag.asked.load(Ordering::SeqCst);
            let begun = self.shared.passes_begun.load(Ordering::SeqCst);
            let ended = self.shared.passes_ended.load(Ordering::SeqCst);
            let inbox_empty = self
                .shared
                .inbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty();
            if !asked && begun == ended && inbox_empty && self.shared.queue.is_idle() {
                return true;
            }
            if start.elapsed() >= timeout || self.has_failed() {
                return false;
            }
            thread::sleep(Duration::from_micros(50));
        }
    }

    /// The link its taps hold. For unit tests.
    #[cfg(test)]
    pub(crate) fn link(&self) -> &RecorderLink {
        &self.link
    }

    /// The logs of the takes' file operations, logging every take from now on. For unit tests.
    #[cfg(test)]
    pub(crate) fn test_logs(&self) -> &TestLogs {
        &self.shared.logs
    }
}

impl Drop for Recorder {
    /// Ends the recorder's thread, which drains the rings, ends the takes under way, removes the
    /// files made ahead and waits for the sync threads. Off the real-time thread.
    #[expect(
        clippy::disallowed_methods,
        reason = "the owner's thread waits for the recorder's thread to end; never on the real-time path"
    )]
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.thread.unpark();
        if let Some(writer) = self.writer.take() {
            // A thread that panicked has said so through `failed`.
            let _ = writer.join();
        }
    }
}

/// The recorder's thread.
struct Writer {
    shared: Arc<Shared>,
    context: TakeContext,
    tracks: Vec<TrackWriter>,
    sync_threads: Vec<JoinHandle<()>>,
}

/// The recorder's thread: drains whenever asked, sleeps otherwise, and ends once its handle is
/// dropped. A panic ends it too, after setting `failed`; the sync threads still finish their queue.
fn serve(mut writer: Writer) {
    let served = catch_unwind(AssertUnwindSafe(|| writer.serve_until_closed()));
    if served.is_err() {
        writer.shared.failed.store(true, Ordering::Release);
    }
    writer.shared.queue.close();
    syncer::join(std::mem::take(&mut writer.sync_threads));
}

impl Writer {
    #[expect(
        clippy::disallowed_methods,
        reason = "the recorder's thread sleeps until a tap wakes it; it never runs on the real-time path"
    )]
    fn serve_until_closed(&mut self) {
        loop {
            if self.shared.closed.load(Ordering::Acquire) {
                self.close();
                return;
            }
            if !self.shared.paused.load(Ordering::SeqCst) {
                self.shared.passes_begun.fetch_add(1, Ordering::SeqCst);
                // Cleared before the pass reads the rings, by a read-modify-write that sees every
                // push made before the asks it clears. A tap that asks again during the pass finds
                // it clear, sets it and wakes the thread, and the loop runs once more.
                self.shared.flag.asked.swap(false, Ordering::AcqRel);
                self.pass();
                self.shared.passes_ended.fetch_add(1, Ordering::SeqCst);
            }
            if self.shared.closed.load(Ordering::Acquire) {
                continue;
            }
            if self.shared.paused.load(Ordering::SeqCst)
                || !self.shared.flag.asked.load(Ordering::SeqCst)
            {
                thread::park();
            }
        }
    }

    /// One pass: takes the new tracks, drains every track, ends the takes of taps gone or stopped,
    /// makes the files ahead, queues the syncs due.
    fn pass(&mut self) {
        let start = Instant::now();
        let mut did = self.take_new_tracks();
        // Read before the rings: a tap stopped has pushed all it ever will.
        let tap_stopped = self.shared.engine.tap_stopped();
        let mut folder_changed = false;
        for track in &mut self.tracks {
            // Read before draining: a tap dropped has pushed all it ever will, and rtrb's acquiring
            // read of it makes those pushes visible.
            track.gone = track.rings.frames.is_abandoned();
            did |= track.drain(&self.context, &self.shared);
            let ended = if track.gone {
                Some(TakeEnd::TapDropped)
            } else {
                tap_stopped.then_some(TakeEnd::TapStopped)
            };
            if let Some(ended) = ended {
                did |= track.end_take(
                    None,
                    ended,
                    &self.context,
                    &self.shared,
                    &mut folder_changed,
                );
            }
        }
        for track in &mut self.tracks {
            if track.gone {
                folder_changed |= track.remove_ahead(&self.shared.stats);
            }
        }
        self.tracks.retain(|track| !track.gone);
        for track in &mut self.tracks {
            folder_changed |= track.make_ahead(&self.context, &self.shared.stats);
        }
        if folder_changed {
            self.shared.queue.sync_folder(&self.context.folder_file);
            did = true;
        }
        for track in &mut self.tracks {
            did |= track.ask_sync_if_due(self.context.rate, &self.shared.queue);
        }
        if did {
            self.shared.stats.record_pass(start.elapsed());
        }
    }

    /// Takes the tracks the handle made taps for. Returns whether there were any.
    fn take_new_tracks(&mut self) -> bool {
        let mut inbox = self
            .shared
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let taken = !inbox.is_empty();
        self.tracks.append(&mut inbox);
        taken
    }

    /// The recorder ends: drains what the rings hold, ends the takes under way, removes the files
    /// made ahead and has the folder synced.
    fn close(&mut self) {
        self.pass();
        let mut folder_changed = false;
        for track in &mut self.tracks {
            track.end_take(
                None,
                TakeEnd::RecorderClosed,
                &self.context,
                &self.shared,
                &mut folder_changed,
            );
            folder_changed |= track.remove_ahead(&self.shared.stats);
        }
        if folder_changed {
            self.shared.queue.sync_folder(&self.context.folder_file);
        }
    }
}

/// A take being written.
struct OpenTake {
    play: u64,
    first_sample: i64,
    round_trip: u64,
    /// Its number and file, unless no file could be made for it.
    number: Option<u32>,
    path: Option<PathBuf>,
    /// The file made ahead, until its first frames write the header.
    file: Option<File>,
    writer: Option<WavWriter<TakeFile>>,
    sync: Option<Arc<SyncFile>>,
    /// Frames in the file, zeros included.
    written: u64,
    padded: u64,
    /// Frames written since its last sync was asked.
    since_sync: u64,
    error: Option<Arc<WavError>>,
}

/// One track, as the recorder's thread drains and writes it.
struct TrackWriter {
    track: u32,
    /// Sanitized.
    name: String,
    channels: u16,
    rings: TapRings,
    /// Frames taken from the frames ring since the tap was made: where the next one lies in the
    /// tap's stream, which the marks' positions count in.
    consumed: u64,
    /// The next take's file, made ahead.
    ahead: Option<AheadFile>,
    /// The number of the next file made.
    next_take: u32,
    take: Option<OpenTake>,
    /// Zeros, written for gaps: whole frames.
    zeros: Box<[f32]>,
    /// Set when its tap was dropped: the track leaves after this pass.
    gone: bool,
}

impl TrackWriter {
    /// Drains the rings: frames up to the next mark, the mark, and so on. Returns whether it took
    /// anything.
    fn drain(&mut self, context: &TakeContext, shared: &Shared) -> bool {
        let channels = usize::from(self.channels);
        let mut took = false;
        let mut counted_again = false;
        loop {
            // The frames' count first, then the marks: `marks.rs` says why.
            let available = self.rings.frames.slots().checked_div(channels).unwrap_or(0) as u64;
            let mark = self.rings.marks.peek().ok().copied();
            match marks::next(self.consumed, available, mark.as_ref()) {
                Next::Frames(count) => {
                    if !self.take_frames(count, context, shared) {
                        return took;
                    }
                }
                Next::ApplyMark => {
                    if let Some(mark) = mark {
                        self.apply(mark, context, shared);
                    }
                    let _ = self.rings.marks.pop();
                }
                // Counting again always reaches the mark's frames; a mark that lies beyond every
                // frame pushed would be a bug of the tap's, which must not spin the thread.
                Next::CountAgain if !counted_again => {
                    counted_again = true;
                    continue;
                }
                Next::CountAgain | Next::Idle => return took,
            }
            took = true;
            counted_again = false;
        }
    }

    /// Takes `count` frames from the ring into the take under way; dropped when it has failed.
    /// Returns whether the ring gave them, as it always does for frames counted.
    fn take_frames(&mut self, count: u64, context: &TakeContext, shared: &Shared) -> bool {
        let channels = usize::from(self.channels);
        let samples = usize::try_from(count)
            .unwrap_or(usize::MAX)
            .saturating_mul(channels);
        let Ok(chunk) = self.rings.frames.read_chunk(samples) else {
            return false;
        };
        let (first, second) = chunk.as_slices();
        if let Some(take) = &mut self.take {
            for samples in [first, second] {
                take.write(self.track, self.channels, samples, context, shared);
            }
        }
        chunk.commit_all();
        self.consumed = self.consumed.saturating_add(count);
        true
    }

    fn apply(&mut self, mark: Mark, context: &TakeContext, shared: &Shared) {
        let mut folder_changed = false;
        match mark.kind {
            MarkKind::Start {
                play,
                first_sample,
                round_trip,
            } => {
                // A take still open would be a bug of the tap's; it ends at what it holds.
                self.end_take(
                    None,
                    TakeEnd::TapDropped,
                    context,
                    shared,
                    &mut folder_changed,
                );
                self.start_take(play, first_sample, round_trip, context, shared);
            }
            MarkKind::Gap { at, length } => {
                shared.stats.gaps.fetch_add(1, Ordering::Relaxed);
                if let Some(take) = &mut self.take {
                    let length = at.saturating_add(length);
                    take.pad_to(
                        length,
                        self.track,
                        self.channels,
                        &self.zeros,
                        context,
                        shared,
                    );
                }
            }
            MarkKind::End { length, reason } => {
                let ended = match reason {
                    EndReason::Stopped => TakeEnd::Stopped,
                    EndReason::Disarmed => TakeEnd::Disarmed,
                    EndReason::LatencyChanged => TakeEnd::LatencyChanged,
                };
                self.end_take(Some(length), ended, context, shared, &mut folder_changed);
            }
        }
        if folder_changed {
            shared.queue.sync_folder(&context.folder_file);
        }
    }

    /// Opens a take in the file made ahead, or in one made now.
    fn start_take(
        &mut self,
        play: u64,
        first_sample: i64,
        round_trip: u64,
        context: &TakeContext,
        shared: &Shared,
    ) {
        shared.stats.takes_started.fetch_add(1, Ordering::Relaxed);
        let ahead = match self.ahead.take() {
            Some(ahead) => Ok(ahead),
            None => make_take_file(&context.folder, self.track, &self.name, &mut self.next_take),
        };
        let (number, path, file, error) = match ahead {
            Ok(ahead) => (Some(ahead.number), Some(ahead.path), Some(ahead.file), None),
            Err(error) => {
                shared.stats.count_write_error();
                (None, None, None, Some(Arc::new(WavError::Io(error))))
            }
        };
        self.take = Some(OpenTake {
            play,
            first_sample,
            round_trip,
            number,
            path,
            file,
            writer: None,
            sync: None,
            written: 0,
            padded: 0,
            since_sync: 0,
            error,
        });
    }

    /// Ends the take under way, if any: padded to `length` when given, then handed to the sync
    /// threads; a take without frames removes its file. Returns whether a take ended.
    fn end_take(
        &mut self,
        length: Option<u64>,
        ended: TakeEnd,
        context: &TakeContext,
        shared: &Shared,
        folder_changed: &mut bool,
    ) -> bool {
        let Some(mut take) = self.take.take() else {
            return false;
        };
        if let Some(length) = length {
            take.pad_to(
                length,
                self.track,
                self.channels,
                &self.zeros,
                context,
                shared,
            );
        }
        if ended == TakeEnd::LatencyChanged {
            shared.stats.latency_ends.fetch_add(1, Ordering::Relaxed);
        }
        let mut summary = TakeSummary {
            track: self.track,
            play: take.play,
            take: take.number,
            path: take.path.clone(),
            first_sample: SamplePosition(take.first_sample),
            round_trip: take.round_trip,
            frames: take.written,
            padded: take.padded,
            ended,
            error: take.error.clone(),
        };
        if take.writer.is_none() && take.error.is_none() {
            // No frame reached the file, still empty: removed.
            drop(take.file.take());
            if let Some(path) = &take.path {
                match fs::remove_file(path) {
                    Ok(()) => *folder_changed = true,
                    Err(_) => shared.stats.count_write_error(),
                }
            }
            summary.take = None;
            summary.path = None;
            shared.takes.push(summary);
            return true;
        }
        shared.queue.push(Job::End(Box::new(Ending {
            writer: take.writer,
            sync: take.sync,
            summary,
        })));
        true
    }

    /// Asks the sync threads for a sync of the take under way once a second of its audio has been
    /// written since the last, unless one is in flight. Returns whether it asked.
    fn ask_sync_if_due(&mut self, rate: u32, queue: &SyncQueue) -> bool {
        let Some(take) = &mut self.take else {
            return false;
        };
        let Some(sync) = take
            .sync
            .as_ref()
            .filter(|_| take.since_sync >= u64::from(rate))
        else {
            return false;
        };
        if sync.in_flight.swap(true, Ordering::AcqRel) {
            return false;
        }
        queue.push(Job::Sync(Arc::clone(sync)));
        take.since_sync = 0;
        true
    }

    /// Makes the next take's file if the track has none ahead. Returns whether it made one.
    fn make_ahead(&mut self, context: &TakeContext, stats: &RecorderStats) -> bool {
        if self.ahead.is_some() {
            return false;
        }
        match make_take_file(&context.folder, self.track, &self.name, &mut self.next_take) {
            Ok(ahead) => {
                self.ahead = Some(ahead);
                true
            }
            Err(_) => {
                // The next take tries again, and reports the error if it fails then.
                stats.count_write_error();
                false
            }
        }
    }

    /// Removes the file made ahead, which no take used. Returns whether it removed one.
    fn remove_ahead(&mut self, stats: &RecorderStats) -> bool {
        let Some(ahead) = self.ahead.take() else {
            return false;
        };
        drop(ahead.file);
        match fs::remove_file(&ahead.path) {
            Ok(()) => true,
            Err(_) => {
                stats.count_write_error();
                false
            }
        }
    }
}

impl OpenTake {
    /// The take's writer, made at its first frames: the header, its sizes unknown, and its `bext`
    /// chunk, written into the file made ahead. `None` once the take has failed.
    fn writer(
        &mut self,
        track: u32,
        channels: u16,
        context: &TakeContext,
        shared: &Shared,
    ) -> Option<&mut WavWriter<TakeFile>> {
        if self.error.is_some() {
            return None;
        }
        if self.writer.is_none() {
            match self.open_writer(track, channels, context, shared) {
                Ok(()) => {}
                Err(error) => {
                    self.fail(error, &shared.stats);
                    return None;
                }
            }
        }
        self.writer.as_mut()
    }

    fn open_writer(
        &mut self,
        track: u32,
        channels: u16,
        context: &TakeContext,
        shared: &Shared,
    ) -> Result<(), WavError> {
        let file = self
            .file
            .take()
            .ok_or_else(|| WavError::Io(std::io::Error::other("the take's file was not made")))?;
        let sync = SyncFile::of(&file).map_err(WavError::Io)?;
        let file = TakeFile::new(file, Arc::clone(&shared.fail_writes));
        #[cfg(test)]
        let (file, sync) = {
            let log = self.test_log(shared);
            (file.with_log(log.clone()), sync.with_log(log))
        };
        let reference = format!(
            "{:016x}{track:08x}{:08x}",
            context.started,
            self.number.unwrap_or(0)
        );
        let bext = Bext {
            originator: ORIGINATOR,
            originator_reference: &reference,
            time_reference: u64::try_from(self.first_sample).unwrap_or(0),
            ..Bext::default()
        };
        self.sync = Some(Arc::new(sync));
        self.writer = Some(WavWriter::with_bext(
            file,
            context.format,
            channels,
            context.rate,
            &bext,
        )?);
        Ok(())
    }

    #[cfg(test)]
    fn test_log(&self, shared: &Shared) -> Option<super::files::OpLog> {
        if !shared.logs.enabled.load(Ordering::Acquire) {
            return None;
        }
        let log = super::files::OpLog::default();
        let path = self.path.clone().unwrap_or_default();
        shared
            .logs
            .logs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((path, log.clone()));
        Some(log)
    }

    /// Writes interleaved frames of the device; dropped once the take has failed.
    fn write(
        &mut self,
        track: u32,
        channels: u16,
        samples: &[f32],
        context: &TakeContext,
        shared: &Shared,
    ) {
        let frames = samples
            .len()
            .checked_div(usize::from(channels))
            .unwrap_or(0) as u64;
        if frames == 0 {
            return;
        }
        let Some(writer) = self.writer(track, channels, context, shared) else {
            return;
        };
        match writer.write_frames(samples) {
            Ok(()) => {
                self.written = self.written.saturating_add(frames);
                self.since_sync = self.since_sync.saturating_add(frames);
                shared
                    .stats
                    .frames_written
                    .fetch_add(frames, Ordering::Relaxed);
            }
            Err(error) => self.fail(error, &shared.stats),
        }
    }

    /// Writes zeros, taken from `zeros`, until the take holds `length` frames; nothing once it has
    /// failed.
    fn pad_to(
        &mut self,
        length: u64,
        track: u32,
        channels: u16,
        zeros: &[f32],
        context: &TakeContext,
        shared: &Shared,
    ) {
        let samples_per_frame = usize::from(channels);
        while self.written < length {
            let frames = length.saturating_sub(self.written).min(ZERO_FRAMES as u64);
            let samples = usize::try_from(frames)
                .unwrap_or(ZERO_FRAMES)
                .saturating_mul(samples_per_frame);
            let Some(zeros) = zeros.get(..samples) else {
                return;
            };
            let Some(writer) = self.writer(track, channels, context, shared) else {
                return;
            };
            if let Err(error) = writer.write_frames(zeros) {
                self.fail(error, &shared.stats);
                return;
            }
            self.written = self.written.saturating_add(frames);
            self.padded = self.padded.saturating_add(frames);
            self.since_sync = self.since_sync.saturating_add(frames);
            shared
                .stats
                .frames_padded
                .fetch_add(frames, Ordering::Relaxed);
        }
    }

    /// The take has failed: it writes nothing more, its file left as a crash leaves it.
    fn fail(&mut self, error: WavError, stats: &RecorderStats) {
        stats.count_write_error();
        self.error = Some(Arc::new(error));
    }
}
