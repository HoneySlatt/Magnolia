//! Magnolia's time model: where every position lies, exactly, over any length of session
//! ([ADR-0018](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0018-the-time-model.md)).
//!
//! Four grids measure time, each a [`Grid`]:
//!
//! - **samples**, at a [`SampleRate`]: what the audio device plays, [`SamplePosition`];
//! - **time units**, 1/[`TIME_UNITS_PER_SECOND`] of a second, the [`TimeUnits`] grid: absolute
//!   positions, which do not depend on the sample rate, [`TimePosition`];
//! - **ticks**, [`TICKS_PER_QUARTER`] per quarter note: musical positions, [`MusicalPosition`],
//!   laid on time by a [`TempoMap`];
//! - **timecode frames**, at a [`FrameRate`]: [`FramePosition`], laid on time by a
//!   [`TimecodeClock`].
//!
//! Every position has an exact [`Moment`], computed with integers alone. A conversion goes from a
//! position to its moment, then from the moment to the other grid, which is the only step that
//! rounds. The one rounding rule:
//!
//! - **placing** a position on another grid gives the first point of that grid at or after its
//!   moment ([`place`]): a note plays on the first sample at or after its instant, never before;
//! - **reading** where a position falls on another grid gives the last point of that grid at or
//!   before its moment ([`read`]): the frame shown for a sample is the frame it lies in.
//!
//! For every grid, position `p` and moment `m`, the rule is this law, which the tests check:
//! `first_at_or_after(m) <= p` exactly when `m <= moment_of(p)`, and `p <= last_at_or_before(m)`
//! exactly when `moment_of(p) <= m`. Placing a position on a finer grid and reading it back gives
//! the position again; no conversion drifts with the distance from zero.
//!
//! ```
//! use magnolia_engine::time::{
//!     FramePosition, FrameRate, Meter, MusicalPosition, SamplePosition, SampleRate,
//!     TICKS_PER_QUARTER, Tempo, TempoMap, TimePosition, Timecode, TimecodeClock, place, read,
//! };
//!
//! // Bar 0 starts at the timeline's zero, at 120 BPM in 4/4.
//! let tempo = Tempo::from_bpm(120, 1).unwrap();
//! let map = TempoMap::constant(TimePosition(0), tempo, Meter::new(4, 4).unwrap()).unwrap();
//! let rate = SampleRate::new(48_000).unwrap();
//!
//! // The second beat plays half a second in.
//! let second_beat = MusicalPosition(TICKS_PER_QUARTER);
//! assert_eq!(place(second_beat, &map, &rate), Some(SamplePosition(24_000)));
//!
//! // At 25 frames a second from 01:00:00:00, that sample lies in frame 12.5, read as frame 12.
//! let start = Timecode { hours: 1, minutes: 0, seconds: 0, frames: 0 };
//! let clock = TimecodeClock::new(FrameRate::Fps25, start, TimePosition(0)).unwrap();
//! let frame = read(SamplePosition(24_000), &rate, &clock).unwrap();
//! assert_eq!(frame, FramePosition(12));
//! assert_eq!(clock.label(frame), Timecode { frames: 12, ..start });
//! ```

mod samples;
mod tempo_map;
mod timecode;

pub use samples::{SamplePosition, SampleRate};
pub use tempo_map::{
    Bbt, Meter, MusicalPosition, TICKS_PER_QUARTER, Tempo, TempoMap, TempoMapError,
};
pub use timecode::{FramePosition, FrameRate, Timecode, TimecodeClock};

/// Time units in one second. A sample at every standard rate from 8 kHz to 768 kHz, a frame at
/// every [`FrameRate`], a microsecond and an eighth of a frame are each a whole number of time
/// units: 903,168,000,000 = 2^17 · 3^2 · 5^6 · 7^2.
pub const TIME_UNITS_PER_SECOND: i64 = 903_168_000_000;

/// An exact instant, in units of 1/([`TIME_UNITS_PER_SECOND`] · [`TICKS_PER_QUARTER`]) of a
/// second, the unit in which a tick at any tempo is a whole number. Moments exist only to
/// convert between grids; nothing stores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Moment(i128);

/// A position on the absolute timeline, in time units: 1/[`TIME_UNITS_PER_SECOND`] of a second.
/// It spans about 118 days either side of zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimePosition(pub i64);

/// A way of measuring time: positions on it, each at an exact [`Moment`].
///
/// Every method returns `None` only when a result does not fit its type.
pub trait Grid {
    /// A position on this grid.
    type Position: Copy + Ord;

    /// The exact moment of a position.
    fn moment_of(&self, position: Self::Position) -> Option<Moment>;

    /// The first position of this grid at or after a moment: where a moment is placed.
    fn first_at_or_after(&self, moment: Moment) -> Option<Self::Position>;

    /// The last position of this grid at or before a moment: where a moment is read.
    fn last_at_or_before(&self, moment: Moment) -> Option<Self::Position>;
}

/// Places a position on another grid: the first point of `onto` at or after its moment. Use it to
/// put something somewhere, such as a note on the sample it plays at, or a note snapped to a frame.
pub fn place<From: Grid, Onto: Grid>(
    position: From::Position,
    from: &From,
    onto: &Onto,
) -> Option<Onto::Position> {
    onto.first_at_or_after(from.moment_of(position)?)
}

/// Reads where a position falls on another grid: the last point of `onto` at or before its moment.
/// Use it to say where something is, such as the bar or the frame a sample lies in.
pub fn read<From: Grid, Onto: Grid>(
    position: From::Position,
    from: &From,
    onto: &Onto,
) -> Option<Onto::Position> {
    onto.last_at_or_before(from.moment_of(position)?)
}

/// The grid of time units, on which [`TimePosition`]s lie.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeUnits;

impl Grid for TimeUnits {
    type Position = TimePosition;

    fn moment_of(&self, position: TimePosition) -> Option<Moment> {
        Some(Moment(
            i128::from(position.0) * i128::from(TICKS_PER_QUARTER),
        ))
    }

    fn first_at_or_after(&self, moment: Moment) -> Option<TimePosition> {
        to_i64(div_ceil(moment.0, i128::from(TICKS_PER_QUARTER))).map(TimePosition)
    }

    fn last_at_or_before(&self, moment: Moment) -> Option<TimePosition> {
        to_i64(moment.0.div_euclid(i128::from(TICKS_PER_QUARTER))).map(TimePosition)
    }
}

/// The smallest integer at or above `numerator / denominator`, for a positive denominator.
fn div_ceil(numerator: i128, denominator: i128) -> i128 {
    let quotient = numerator.div_euclid(denominator);
    if numerator.rem_euclid(denominator) == 0 {
        quotient
    } else {
        quotient + 1
    }
}

fn to_i64(value: i128) -> Option<i64> {
    i64::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_unit_holds_a_whole_number_of_microseconds_and_frames() {
        assert_eq!(TIME_UNITS_PER_SECOND % 1_000_000, 0);
        for rate in FrameRate::ALL {
            let (frames, seconds) = rate.frames_per_second();
            let eighth_of_a_frame = i128::from(TIME_UNITS_PER_SECOND) * i128::from(seconds);
            assert_eq!(eighth_of_a_frame % (8 * i128::from(frames)), 0, "{rate:?}");
        }
    }

    #[test]
    fn rounding_follows_the_floor_and_the_ceiling_on_both_sides_of_zero() {
        assert_eq!(div_ceil(7, 2), 4);
        assert_eq!(div_ceil(-7, 2), -3);
        assert_eq!(div_ceil(-8, 2), -4);
        assert_eq!((-7_i128).div_euclid(2), -4);
        let between = Moment(i128::from(TICKS_PER_QUARTER) * 5 - 1);
        assert_eq!(TimeUnits.first_at_or_after(between), Some(TimePosition(5)));
        assert_eq!(TimeUnits.last_at_or_before(between), Some(TimePosition(4)));
        let negative = Moment(-1);
        assert_eq!(TimeUnits.first_at_or_after(negative), Some(TimePosition(0)));
        assert_eq!(
            TimeUnits.last_at_or_before(negative),
            Some(TimePosition(-1))
        );
    }

    #[test]
    fn a_moment_beyond_a_grid_reads_as_none() {
        let far = Moment(i128::MAX);
        assert_eq!(TimeUnits.first_at_or_after(far), None);
        assert_eq!(TimeUnits.last_at_or_before(far), None);
    }
}
