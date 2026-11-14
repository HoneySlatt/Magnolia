//! The capture tap: on the real-time thread, it copies the capture frames of each armed track's
//! takes into the track's rings, and wakes the recorder
//! ([ADR-0032](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0032-recording-by-a-capture-tap-of-the-processor.md)).
//!
//! A take's **window** is counted in engine samples. A play that starts publishes `audible_at`, the
//! engine sample at which the output plays its first timeline sample `from`; with `R` the round
//! trip read at the play's start, the take's first frame is the frame captured at engine sample
//! `audible_at + R`, and it lasts `stop - from` frames once the transport's stop is known. A take
//! starts at the timeline's zero at the earliest: a play from before it skips the frames before
//! it. Windows never overlap, since a play starts only after the last one's stop has played; a
//! track's tap holds up to [`MAX_WINDOWS`] at once, the old take's tail and the plays that follow.
//!
//! Each period, for each window that the period's frames reach, the tap pushes the window's frames
//! into the track's frames ring, interleaved by the take's channels, and its marks into the marks
//! ring (`marks.rs`): its start before its first frame, a gap where frames were dropped, its end
//! after its last. Frames the ring has no room for are dropped whole, counted in
//! [`EngineStats::capture_overruns`], and become a gap; marks keep room for what must follow them,
//! so that a take's end always fits once its start is in:
//!
//! - a start goes in only with room for three marks: itself, a gap and the end;
//! - a gap, with room for two: itself and the end;
//! - a window's start and its waiting gap go in only right before frames that fit, and frames go in
//!   only once both are in, so marks and frames stay in order, and a gap grows over every frame
//!   dropped until frames fit again: one gap per stall of the recorder;
//! - an end folds a gap that still waits into its length: the recorder pads the take to it.
//!
//! A take whose start never finds room before its end is lost; its frames were counted as overruns.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt;
use std::hint::black_box;
use std::iter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread::Thread;

use rtrb::{Consumer, Producer, RingBuffer};

use super::marks::{EndReason, Mark, MarkKind};
use crate::device::DeviceBuffers;
use crate::engine::PlayInfo;
use crate::stats::{DeviceLatency, EngineStats};

/// The most takes a track's tap holds at once: the one whose frames come in, and the plays after
/// it whose frames have not started, as when plays and stops come faster than the round trip.
/// Past it, a play makes no take, counted in [`EngineStats::missed_takes`].
pub const MAX_WINDOWS: usize = 16;

/// A tap asks the recorder to drain once its ring holds this many frames it has not announced, or
/// once it has pushed a mark: about 43 ms at 48 kHz.
pub(crate) const WAKE_FRAMES: u64 = 2_048;

/// The marks a track's ring holds: a take needs two, and one per gap.
pub(crate) const MARKS_CAPACITY: usize = 256;

/// Wakes the recorder's thread: a futex wake when it sleeps, nothing when it is awake already
/// (`Thread::unpark`). It is the one call on the real-time allowlist that the capture tap needs
/// ([ADR-0032](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0032-recording-by-a-capture-tap-of-the-processor.md)):
/// `rtsan-suppressions.txt` names this function, so RealtimeSanitizer lets its system call pass
/// and still reports any other. It is public so that the sanitizer's test can name it, and never
/// inlined, so that it shows on the call stack.
#[inline(never)]
pub fn wake_recorder(recorder: &Thread) {
    recorder.unpark();
}

/// The flag by which taps ask a recorder to drain, shared by every tap of that recorder and its
/// thread.
#[derive(Debug)]
pub(crate) struct WakeFlag {
    /// Whether a tap asked since the recorder's last pass began. Every set and its clearing is a
    /// read-modify-write: whoever sets it from clear wakes the thread, and a pass that clears it
    /// sees every push made before the set.
    pub(crate) asked: AtomicBool,
    /// The engine sample of the last period that asked, so that a period asks once at most. Only
    /// the processor writes it.
    asked_at: AtomicI64,
}

impl WakeFlag {
    /// A clear flag, written through `black_box` so that the processor never faults on its page.
    pub(crate) fn new() -> Self {
        Self {
            asked: AtomicBool::new(black_box(false)),
            asked_at: AtomicI64::new(black_box(i64::MIN)),
        }
    }
}

/// What a recorder shares with the taps it makes: how they wake it, and its extra latency.
pub(crate) struct RecorderLink {
    pub(crate) flag: Arc<WakeFlag>,
    pub(crate) thread: Thread,
    /// The recorder's own latency beyond the device's, in frames (ADR-0031).
    pub(crate) extra_latency: u64,
    /// Makes the tap's next run panic, so that a test sees what follows.
    #[cfg(test)]
    pub(crate) panic_in_tap: AtomicBool,
}

impl RecorderLink {
    /// Asks the recorder to drain, once per period at most, from the period that starts at engine
    /// sample `period`: sets the flag, and wakes the thread when the flag was clear.
    fn ask(&self, period: i64) {
        if self.flag.asked_at.load(Ordering::Relaxed) == period {
            return;
        }
        self.flag.asked_at.store(period, Ordering::Relaxed);
        if !self.flag.asked.swap(true, Ordering::AcqRel) {
            wake_recorder(&self.thread);
        }
    }
}

/// How full a track's rings have been at their fullest: the tap records it after each push.
#[derive(Debug)]
pub(crate) struct RingFills {
    /// In frames.
    pub(crate) frames: AtomicU64,
    pub(crate) marks: AtomicU64,
}

/// A tap's identity: the controller and the processor tell taps apart by it, never by their
/// track's number, which a re-armed track keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TapId(u64);

impl TapId {
    /// A new id, never handed out before in this process.
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// An armed track's capture tap, made by [`Recorder::tap`](super::Recorder::tap) and armed by
/// [`Controller::arm`](crate::engine::Controller::arm): the processor copies the track's capture
/// channels into its rings from the next play after the commit that sends it.
pub struct Tap {
    pub(crate) track: Box<TrackTap>,
}

impl Tap {
    /// The number of the track it records.
    pub fn track(&self) -> u32 {
        self.track.track
    }

    /// The engine's capture channels it records, one per channel of its takes.
    pub fn capture_channels(&self) -> &[usize] {
        &self.track.channels
    }
}

impl fmt::Debug for Tap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tap")
            .field("track", &self.track.track)
            .field("capture_channels", &self.track.channels)
            .finish_non_exhaustive()
    }
}

/// What the recorder keeps of a tap it made: the other ends of its rings.
pub(crate) struct TapRings {
    pub(crate) frames: Consumer<f32>,
    pub(crate) marks: Consumer<Mark>,
    pub(crate) fills: Arc<RingFills>,
}

/// One take, from its play's start until its end is pushed.
#[derive(Clone, Copy, Debug)]
struct Window {
    /// The play it records.
    play: u64,
    /// The timeline sample of the take's first frame, at or after 0.
    first_sample: i64,
    /// The engine sample at which its first frame is captured.
    start: i64,
    /// Its frames, once the transport's stop is known.
    length: Option<u64>,
    /// The device's latency read at the play's start.
    latency: DeviceLatency,
    /// The round trip that placed it.
    round_trip: u64,
    /// The take's frames the tap has handled: pushed, or dropped.
    reached: u64,
    /// Whether its start mark is in the ring.
    started: bool,
    /// The take's frames dropped and not yet marked: the last `gap` frames of those reached.
    gap: u64,
}

impl Window {
    /// The engine samples whose frames the take still needs: from the first not reached, up to its
    /// end, or on while its stop is unknown.
    fn remaining(&self) -> (i64, i64) {
        let from = self.start.saturating_add(as_samples(self.reached));
        let to = self.length.map_or(i64::MAX, |length| {
            self.start.saturating_add(as_samples(length))
        });
        (from, to)
    }

    fn is_complete(&self) -> bool {
        self.length.is_some_and(|length| self.reached >= length)
    }

    fn start_mark(&self) -> MarkKind {
        MarkKind::Start {
            play: self.play,
            first_sample: self.first_sample,
            round_trip: self.round_trip,
        }
    }
}

/// What the tap needs of a period: the device's capture, where the period lies, the last play and
/// the device's latency.
pub(crate) struct PeriodView<'a> {
    pub(crate) device: &'a dyn DeviceBuffers,
    /// The engine sample of the period's first frame.
    pub(crate) start: i64,
    pub(crate) frames: usize,
    pub(crate) play: Option<PlayInfo>,
    pub(crate) latency: DeviceLatency,
}

/// One armed track's tap, as the processor runs it.
pub(crate) struct TrackTap {
    pub(crate) id: TapId,
    pub(crate) track: u32,
    /// The engine's capture channel of each channel of the take.
    pub(crate) channels: Box<[usize]>,
    frames: Producer<f32>,
    marks: Producer<Mark>,
    link: Arc<RecorderLink>,
    fills: Arc<RingFills>,
    /// The takes under way, in the order of their plays.
    windows: [Option<Window>; MAX_WINDOWS],
    /// The last play the tap has seen: a play after it opens a window.
    last_play: u64,
    /// Frames pushed into the frames ring since the tap was made.
    pushed: u64,
    /// Frames pushed since the tap last asked the recorder to drain.
    unannounced: u64,
    /// Whether a mark was pushed since then.
    marked: bool,
}

impl TrackTap {
    /// A tap for `track` that records `channels` into rings of `ring_frames` frames, and the ends
    /// the recorder keeps. Off the real-time thread: every page of both rings is written here, by
    /// pushing through every slot and popping it again.
    pub(crate) fn new(
        track: u32,
        channels: Box<[usize]>,
        ring_frames: usize,
        link: Arc<RecorderLink>,
    ) -> (Self, TapRings) {
        let samples = ring_frames.saturating_mul(channels.len());
        let (mut frames, mut frames_out) = RingBuffer::new(samples);
        if let Ok(chunk) = frames.write_chunk_uninit(samples) {
            chunk.fill_from_iter(iter::repeat_n(black_box(0.0), samples));
        }
        if let Ok(chunk) = frames_out.read_chunk(samples) {
            chunk.commit_all();
        }
        let (mut marks, mut marks_out) = RingBuffer::new(MARKS_CAPACITY);
        let blank = Mark {
            position: black_box(0),
            kind: MarkKind::Gap { at: 0, length: 0 },
        };
        while marks.push(blank).is_ok() {}
        while marks_out.pop().is_ok() {}
        let fills = Arc::new(RingFills {
            frames: AtomicU64::new(black_box(0)),
            marks: AtomicU64::new(black_box(0)),
        });
        let tap = Self {
            id: TapId::next(),
            track,
            channels,
            frames,
            marks,
            link,
            fills: Arc::clone(&fills),
            windows: [None; MAX_WINDOWS],
            last_play: 0,
            pushed: 0,
            unannounced: 0,
            marked: false,
        };
        let rings = TapRings {
            frames: frames_out,
            marks: marks_out,
            fills,
        };
        (tap, rings)
    }

    /// Runs one period: follows the transport, ends the takes of another latency, and copies each
    /// window's frames.
    fn run(&mut self, view: &PeriodView<'_>, stats: &EngineStats) {
        #[cfg(test)]
        assert!(
            !self.link.panic_in_tap.load(Ordering::Relaxed),
            "a test's panic in the tap, on purpose"
        );
        self.follow(view, stats);
        self.end_windows_of_another_latency(view.latency);
        self.copy(view, stats);
    }

    /// Whether the recorder should drain: a mark was pushed, or enough frames.
    fn wants_drain(&self) -> bool {
        self.marked || self.unannounced >= WAKE_FRAMES
    }

    fn announced(&mut self) {
        self.marked = false;
        self.unannounced = 0;
    }

    /// Ends the take under way, its track disarmed: at what the tap has reached. The plays whose
    /// frames have not started make no take.
    fn retire(&mut self) {
        for slot in &mut self.windows {
            let Some(window) = slot.take() else { continue };
            if window.reached > 0 {
                push_end(
                    &mut self.marks,
                    &self.fills,
                    self.pushed,
                    &window,
                    EndReason::Disarmed,
                );
                self.marked = true;
            }
        }
    }

    /// Opens a window for a play the tap has not seen, and gives the play's window its length once
    /// the stop is known.
    fn follow(&mut self, view: &PeriodView<'_>, stats: &EngineStats) {
        let Some(play) = view.play else { return };
        if play.sequence > self.last_play {
            self.last_play = play.sequence;
            self.open(play, view.latency, stats);
        }
        let Some(stop) = play.stop else { return };
        for slot in &mut self.windows {
            let Some(window) = slot
                .as_mut()
                .filter(|window| window.play == play.sequence && window.length.is_none())
            else {
                continue;
            };
            let length = u64::try_from(stop.saturating_sub(window.first_sample)).unwrap_or(0);
            if length == 0 && window.reached == 0 {
                // The audible region lies wholly before the timeline's zero: no take.
                *slot = None;
            } else {
                // Never shorter than what was reached: a stop always lies beyond it.
                window.length = Some(length.max(window.reached));
            }
        }
    }

    /// Opens the window of `play`, placed by the round trip it reads now; none for an audible
    /// region known empty, or wholly before the timeline's zero.
    fn open(&mut self, play: PlayInfo, latency: DeviceLatency, stats: &EngineStats) {
        let first_sample = play.from.max(0);
        if play.stop.is_some_and(|stop| stop <= first_sample) {
            return;
        }
        let skipped = first_sample.saturating_sub(play.from);
        let round_trip = latency.round_trip().saturating_add(self.link.extra_latency);
        let window = Window {
            play: play.sequence,
            first_sample,
            start: play
                .audible_at
                .saturating_add(as_samples(round_trip))
                .saturating_add(skipped),
            length: None,
            latency,
            round_trip,
            reached: 0,
            started: false,
            gap: 0,
        };
        match self.windows.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => *slot = Some(window),
            None => stats.count_missed_take(),
        }
    }

    /// Ends every take placed by a latency other than the device's now, at what it has reached
    /// (ADR-0031). A take whose frames have not started ends empty, so that the change is reported.
    fn end_windows_of_another_latency(&mut self, latency: DeviceLatency) {
        let mut ended = false;
        for slot in &mut self.windows {
            let Some(window) = slot.take_if(|window| window.latency != latency) else {
                continue;
            };
            push_end(
                &mut self.marks,
                &self.fills,
                self.pushed,
                &window,
                EndReason::LatencyChanged,
            );
            ended = true;
        }
        if ended {
            self.marked = true;
            self.compact();
        }
    }

    /// Copies, window after window, the period's frames that each needs, and ends those complete.
    fn copy(&mut self, view: &PeriodView<'_>, stats: &EngineStats) {
        let period_end = view.start.saturating_add(as_samples(view.frames as u64));
        let mut ended = false;
        let mut dropped: u64 = 0;
        for index in 0..MAX_WINDOWS {
            let Some(mut window) = self.windows.get(index).copied().flatten() else {
                continue;
            };
            let (from, to) = window.remaining();
            // Frames of the window before this period were never offered: the tap runs every period
            // from the window's opening, so none are. Were any, they would be a gap, so that what
            // follows stays in place.
            let missed = to.min(view.start).saturating_sub(from);
            if missed > 0 {
                let missed = as_frames(missed);
                window.gap = window.gap.saturating_add(missed);
                window.reached = window.reached.saturating_add(missed);
                dropped = dropped.saturating_add(missed);
            }
            let first = from.max(view.start);
            let last = to.min(period_end);
            if first < last {
                let count = as_frames(last.saturating_sub(first));
                let offset = usize::try_from(first.saturating_sub(view.start)).unwrap_or(0);
                if !self.take_frames(&mut window, view, offset, count) {
                    window.gap = window.gap.saturating_add(count);
                    dropped = dropped.saturating_add(count);
                }
                window.reached = window.reached.saturating_add(count);
            }
            let complete = window.is_complete();
            if complete {
                push_end(
                    &mut self.marks,
                    &self.fills,
                    self.pushed,
                    &window,
                    EndReason::Stopped,
                );
                self.marked = true;
                ended = true;
            }
            if let Some(slot) = self.windows.get_mut(index) {
                *slot = (!complete).then_some(window);
            }
        }
        stats.count_capture_overruns(dropped);
        if ended {
            self.compact();
        }
    }

    /// Pushes `count` frames of the period, from `offset`, for `window`, if the frames ring has room
    /// for them: after its start and its waiting gap, if the marks ring has room for those. Returns
    /// whether they went in; otherwise they are dropped.
    fn take_frames(
        &mut self,
        window: &mut Window,
        view: &PeriodView<'_>,
        offset: usize,
        count: u64,
    ) -> bool {
        let Ok(count) = usize::try_from(count) else {
            return false;
        };
        let samples = count.saturating_mul(self.channels.len());
        if samples == 0 || self.frames.slots() < samples {
            return false;
        }
        let pushed = self.pushed;
        if !window.started {
            window.started = push_start(&mut self.marks, &self.fills, pushed, window);
            if !window.started {
                return false;
            }
            self.marked = true;
        }
        if window.gap > 0 {
            if !push_gap(&mut self.marks, &self.fills, pushed, window) {
                return false;
            }
            self.marked = true;
        }
        if !self.push_frames(view, offset, count) {
            return false;
        }
        self.pushed = self.pushed.saturating_add(count as u64);
        self.unannounced = self.unannounced.saturating_add(count as u64);
        true
    }

    /// Pushes `count` frames of the device's capture, from `offset` in the period, interleaved by
    /// the take's channels, if the ring has room for them all. A capture buffer shorter than the
    /// period is silence, as the graph takes it.
    fn push_frames(&mut self, view: &PeriodView<'_>, offset: usize, count: usize) -> bool {
        let channels = self.channels.len();
        let samples = count.saturating_mul(channels);
        // Zeroed first: a channel the device lacks stays silent.
        let Ok(mut chunk) = self.frames.write_chunk(samples) else {
            return false;
        };
        let (first, second) = chunk.as_mut_slices();
        // The ring holds whole frames, so it wraps between two.
        let first_frames = first.len().checked_div(channels).unwrap_or(0);
        for (index, &channel) in self.channels.iter().enumerate() {
            let buffer = view.device.capture(channel);
            if buffer.len() < view.frames {
                continue;
            }
            let Some(source) = buffer.get(offset..offset.saturating_add(count)) else {
                continue;
            };
            let (head, tail) = source.split_at(first_frames.min(source.len()));
            let into_first = first.iter_mut().skip(index).step_by(channels);
            for (target, sample) in into_first.zip(head) {
                *target = *sample;
            }
            let into_second = second.iter_mut().skip(index).step_by(channels);
            for (target, sample) in into_second.zip(tail) {
                *target = *sample;
            }
        }
        chunk.commit_all();
        let capacity = self.frames.buffer().capacity();
        let fill = capacity
            .saturating_sub(self.frames.slots())
            .checked_div(channels)
            .unwrap_or(0);
        self.fills.frames.fetch_max(fill as u64, Ordering::Relaxed);
        true
    }

    /// Moves the windows still under way to the front, in order.
    fn compact(&mut self) {
        let mut kept = [None; MAX_WINDOWS];
        for (slot, window) in kept.iter_mut().zip(self.windows.iter().flatten()) {
            *slot = Some(*window);
        }
        self.windows = kept;
    }
}

/// Pushes `window`'s start, at `position`, if the ring has room for it, a gap and the end. Returns
/// whether it went in.
fn push_start(
    marks: &mut Producer<Mark>,
    fills: &RingFills,
    position: u64,
    window: &Window,
) -> bool {
    marks.slots() >= 3 && push_mark(marks, fills, position, window.start_mark())
}

/// Pushes `window`'s waiting gap, at `position`, if the ring has room for it and the end. Returns
/// whether it went in, and clears the gap then.
fn push_gap(
    marks: &mut Producer<Mark>,
    fills: &RingFills,
    position: u64,
    window: &mut Window,
) -> bool {
    let gap = MarkKind::Gap {
        at: window.reached.saturating_sub(window.gap),
        length: window.gap,
    };
    let pushed = marks.slots() >= 2 && push_mark(marks, fills, position, gap);
    if pushed {
        window.gap = 0;
    }
    pushed
}

/// Pushes `window`'s end at `position`, after its start if that still waits: the end's length holds
/// any gap that waits. The start keeps room for the end; a start that never found room loses the
/// take now, unless the ring has room for both.
fn push_end(
    marks: &mut Producer<Mark>,
    fills: &RingFills,
    position: u64,
    window: &Window,
    reason: EndReason,
) {
    if !window.started {
        if marks.slots() < 2 {
            return;
        }
        push_mark(marks, fills, position, window.start_mark());
    }
    let end = MarkKind::End {
        length: window.reached,
        reason,
    };
    push_mark(marks, fills, position, end);
}

/// Pushes a mark, and records the ring's fill. Returns whether it went in.
fn push_mark(marks: &mut Producer<Mark>, fills: &RingFills, position: u64, kind: MarkKind) -> bool {
    let pushed = marks.push(Mark { position, kind }).is_ok();
    let fill = marks.buffer().capacity().saturating_sub(marks.slots());
    fills.marks.fetch_max(fill as u64, Ordering::Relaxed);
    pushed
}

/// The armed tracks' taps, as the processor runs them at each period's start, after the transport's
/// commands and before the graph, in a `catch_unwind` of their own.
pub(crate) struct CaptureTap {
    tracks: Vec<TrackTap>,
    /// A swap's change of the armed tracks, applied at the next run, with the number of the last
    /// play started when the swap applied: a tap it arms records from the play after.
    staged: Option<(Box<TapChange>, u64)>,
    /// The change being applied, which ends up holding the taps it retires, for the collector.
    /// Kept here, never in a local, so that a panic never drops it on the real-time thread.
    retiring: Option<Box<TapChange>>,
    /// Set after a panic: the taps run no more.
    stopped: bool,
}

/// What a swap changes of the armed tracks, built by the controller.
pub(crate) struct TapChange {
    /// The taps the swap arms, with room for every tap of the new set, written beforehand, so that
    /// the processor moves the taps it keeps in without allocating or faulting. Once applied, it
    /// holds the taps retired, for the collector.
    pub(crate) taps: Vec<TrackTap>,
    /// The taps the processor holds that the swap keeps, sorted.
    pub(crate) kept: Box<[TapId]>,
}

impl CaptureTap {
    /// No tap armed.
    pub(crate) fn new() -> Self {
        Self {
            tracks: Vec::new(),
            staged: None,
            retiring: None,
            stopped: false,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Stops the taps for good, after a panic in the period that starts at engine sample
    /// `period`, and wakes their recorders, which end the takes under way at what they hold. Only
    /// the links are touched: a tap the panic left half-changed is never read again.
    pub(crate) fn stop(&mut self, period: i64) {
        self.stopped = true;
        for track in &self.tracks {
            track.link.ask(period);
        }
    }

    /// Takes a swap's change, which the next run applies; `play` is the last play started when the
    /// swap applied. Gives back a change staged before and never applied, for the collector.
    pub(crate) fn stage(&mut self, change: Box<TapChange>, play: u64) -> Option<Box<TapChange>> {
        self.staged
            .replace((change, play))
            .map(|(displaced, _)| displaced)
    }

    /// Runs one period: applies a staged change, ends the takes of the taps it retires, runs every
    /// tap, then asks each recorder to drain, once, after every push. Returns the taps retired, for
    /// the collector.
    pub(crate) fn run(
        &mut self,
        view: &PeriodView<'_>,
        stats: &EngineStats,
    ) -> Option<Box<TapChange>> {
        if let Some((change, play)) = self.staged.take() {
            self.apply(change, play);
        }
        for track in &mut self.tracks {
            track.run(view, stats);
        }
        let retired = self
            .retiring
            .as_deref_mut()
            .map_or(&mut [] as &mut [TrackTap], |change| {
                change.taps.as_mut_slice()
            });
        for track in self.tracks.iter_mut().chain(retired.iter_mut()) {
            if track.wants_drain() {
                track.link.ask(view.start);
                track.announced();
            }
        }
        self.retiring.take()
    }

    /// Moves the taps `change` keeps into its list, which becomes the processor's, ends the takes of
    /// the others, and keeps them in `retiring`. New taps record from the play after `play`.
    fn apply(&mut self, change: Box<TapChange>, play: u64) {
        let Self {
            tracks, retiring, ..
        } = self;
        let change = retiring.insert(change);
        for track in &mut change.taps {
            track.last_play = play;
        }
        let mut index = tracks.len();
        while index > 0 {
            index = index.saturating_sub(1);
            let kept = tracks
                .get(index)
                .is_some_and(|track| change.kept.binary_search(&track.id).is_ok());
            // Room was reserved for every tap kept: the push never allocates.
            if kept && !change.taps.spare_capacity_mut().is_empty() {
                let track = tracks.swap_remove(index);
                change.taps.push(track);
            }
        }
        std::mem::swap(tracks, &mut change.taps);
        for track in &mut change.taps {
            track.retire();
        }
    }
}

/// A count of frames as a distance between engine samples.
fn as_samples(frames: u64) -> i64 {
    i64::try_from(frames).unwrap_or(i64::MAX)
}

/// A distance between engine samples, never negative, as a count of frames.
fn as_frames(samples: i64) -> u64 {
    u64::try_from(samples).unwrap_or(0)
}
