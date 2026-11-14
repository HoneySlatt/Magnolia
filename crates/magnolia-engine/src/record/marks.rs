//! The marks a tap sends beside a track's frames, and the order in which the recorder reads the two
//! rings.
//!
//! A track's tap pushes the take's frames into one ring and its marks into another: a take's start,
//! a gap where frames were dropped, a take's end. Each mark carries the **position** at which it
//! applies in the track's stream of frames: how many frames the tap had pushed into the frames ring
//! before it. The tap pushes a mark before every frame that follows it, and after every frame that
//! precedes it.
//!
//! **The order of reading.** The recorder reads how many frames the frames ring holds *first*, then
//! looks at the next mark, and writes frames only up to that mark's position. It is right because
//! every frame it counted was pushed before that count was read, and every mark that precedes such
//! a frame was pushed before the frame: the count's acquiring read makes the mark's push visible,
//! so the mark is seen before any frame that follows it is written. Read the other way, marks
//! first, the recorder could find no mark, then count frames that the tap pushed after an end and a
//! new start it did not see, and write them into the old take (`record/tests.rs` shows both).
//!
//! A mark whose position lies beyond the frames counted means that the count was read before the
//! frames it follows were pushed: the recorder writes what it counted, then counts again, and the
//! new count reaches the mark.
//!
//! [`drain`] is that order: the recorder's thread drains every track through it, and the unit tests
//! drive its [`count`] and [`step`] with pushes in between.

// The tap pushes marks on the real-time thread: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use super::tap::TapRings;

/// What the tap tells the recorder beside a track's frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    /// How many frames the tap had pushed into the track's frames ring before this mark, since the
    /// tap was made: where the mark applies in the stream.
    pub(crate) position: u64,
    pub(crate) kind: MarkKind,
}

/// The kinds of mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkKind {
    /// A take starts: its frames follow.
    Start {
        /// The number of the play it records, as `Controller::play` returned it.
        play: u64,
        /// The timeline sample of its first frame, at or after 0.
        first_sample: i64,
        /// The round trip it is placed by, in frames: the device's capture and playback latencies
        /// read at the play's start, plus the recorder's extra latency (ADR-0031).
        round_trip: u64,
    },
    /// Frames of the take that the tap dropped: the recorder writes zeros in their place.
    Gap {
        /// The take's frame where the gap starts.
        at: u64,
        /// How many frames it lasts.
        length: u64,
    },
    /// The take ends: it holds `length` frames, the last of them zeros if they were dropped.
    End {
        /// The take's frames, counted from its first.
        length: u64,
        /// Why it ended.
        reason: EndReason,
    },
}

/// Why a tap ended a take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    /// The take reached its length, the audible region's: the transport stopped.
    Stopped,
    /// Its track was disarmed.
    Disarmed,
    /// The device's latency changed, which would place the rest of the take wrong (ADR-0031).
    LatencyChanged,
}

/// What the recorder does next with a track's rings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Write this many frames, which come before any mark not yet applied.
    Frames(u64),
    /// Apply the mark it looked at, then pop it.
    ApplyMark,
    /// The mark lies beyond the frames counted: count them again.
    CountAgain,
    /// Nothing waits.
    Idle,
}

/// What the recorder does next, having consumed `consumed` frames of the stream, counted `available`
/// frames waiting in the frames ring, and *then* looked at `mark`, the next mark not yet applied.
pub(crate) fn next(consumed: u64, available: u64, mark: Option<&Mark>) -> Next {
    match mark {
        None if available > 0 => Next::Frames(available),
        None => Next::Idle,
        Some(mark) => {
            let before = mark.position.saturating_sub(consumed);
            if before == 0 {
                Next::ApplyMark
            } else if available > 0 {
                Next::Frames(available.min(before))
            } else {
                Next::CountAgain
            }
        }
    }
}

/// What the recorder takes out of a track's rings, in the order they hold it.
pub(crate) enum Drained<'a> {
    /// Interleaved samples of whole frames.
    Frames(&'a [f32]),
    Mark(Mark),
}

/// How many whole frames of `channels` channels the frames ring holds: read first.
pub(crate) fn count(rings: &TapRings, channels: usize) -> u64 {
    rings.frames.slots().checked_div(channels).unwrap_or(0) as u64
}

/// One step of the drain, `available` the frames [`count`] read before: looks at the next mark, then
/// hands `receive` either the frames that come before it, or the mark, which it then pops.
pub(crate) fn step(
    rings: &mut TapRings,
    channels: usize,
    available: u64,
    receive: &mut dyn FnMut(Drained<'_>),
) -> Next {
    let mark = rings.marks.peek().ok().copied();
    let next = next(rings.consumed, available, mark.as_ref());
    match next {
        Next::Frames(frames) => {
            let samples = usize::try_from(frames)
                .unwrap_or(usize::MAX)
                .saturating_mul(channels);
            // Frames counted are always there to read.
            let Ok(chunk) = rings.frames.read_chunk(samples) else {
                return Next::Idle;
            };
            let (first, second) = chunk.as_slices();
            receive(Drained::Frames(first));
            receive(Drained::Frames(second));
            chunk.commit_all();
            rings.consumed = rings.consumed.saturating_add(frames);
        }
        Next::ApplyMark => {
            if let Some(mark) = mark {
                receive(Drained::Mark(mark));
            }
            let _ = rings.marks.pop();
        }
        Next::CountAgain | Next::Idle => {}
    }
    next
}

/// Drains a track's rings in order, handing `receive` frames up to each mark, then the mark.
/// Returns whether it took anything.
pub(crate) fn drain(
    rings: &mut TapRings,
    channels: usize,
    receive: &mut dyn FnMut(Drained<'_>),
) -> bool {
    let mut took = false;
    let mut counted_again = false;
    loop {
        let available = count(rings, channels);
        match step(rings, channels, available, receive) {
            Next::Frames(_) | Next::ApplyMark => {
                took = true;
                counted_again = false;
            }
            // Counting again always reaches the mark's frames; a mark that lies beyond every frame
            // pushed would be a bug of the tap's, which must not spin the thread.
            Next::CountAgain if !counted_again => counted_again = true,
            Next::CountAgain | Next::Idle => return took,
        }
    }
}
