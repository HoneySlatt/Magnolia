//! Samples: the grid the audio device plays.

// The period converts time with this module's code: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use super::{Grid, Moment, TICKS_PER_QUARTER, TIME_UNITS_PER_SECOND, div_ceil, to_i64};

/// A position in samples, counted from the absolute timeline's zero, at some [`SampleRate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SamplePosition(pub i64);

/// A sample rate in hertz, from 8 kHz to 768 kHz, whose sample is a whole number of time units:
/// every rate of [`SampleRate::STANDARD_HERTZ`] is one. Pulled-down rates such as 44,056 Hz are
/// not: a session runs at its nominal rate, and a pull is the audio device's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleRate(u32);

impl SampleRate {
    /// The lowest rate accepted, in hertz.
    pub const MIN_HERTZ: u32 = 8_000;
    /// The highest rate accepted, in hertz.
    pub const MAX_HERTZ: u32 = 768_000;
    /// The standard sample rates, in hertz.
    pub const STANDARD_HERTZ: [u32; 15] = [
        8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
        352_800, 384_000, 705_600, 768_000,
    ];

    /// A sample rate, or `None` if `hertz` is outside 8 kHz to 768 kHz or its sample is not a
    /// whole number of time units.
    pub fn new(hertz: u32) -> Option<SampleRate> {
        let in_range = (Self::MIN_HERTZ..=Self::MAX_HERTZ).contains(&hertz);
        let whole = TIME_UNITS_PER_SECOND.checked_rem(i64::from(hertz)) == Some(0);
        (in_range && whole).then_some(SampleRate(hertz))
    }

    /// The rate in hertz.
    pub fn hertz(self) -> u32 {
        self.0
    }

    /// The length of one sample, in time units.
    pub fn time_units_per_sample(self) -> i64 {
        // A rate is at least 8 kHz, so the division never fails and the fallback, a sample of a
        // whole second, is never taken.
        TIME_UNITS_PER_SECOND
            .checked_div(i64::from(self.0))
            .unwrap_or(TIME_UNITS_PER_SECOND)
    }

    /// The length of one sample, in the unit of moments.
    fn moment_units_per_sample(self) -> i128 {
        // A sample lasts at most a second, 2^39.7 time units: the product never saturates.
        i128::from(self.time_units_per_sample()).saturating_mul(i128::from(TICKS_PER_QUARTER))
    }
}

impl Grid for SampleRate {
    type Position = SamplePosition;

    fn moment_of(&self, position: SamplePosition) -> Option<Moment> {
        // At most 2^63 · 2^26.7 · 2^22.2 = 2^112: never overflows.
        i128::from(position.0)
            .checked_mul(self.moment_units_per_sample())
            .map(Moment)
    }

    fn first_at_or_after(&self, moment: Moment) -> Option<SamplePosition> {
        to_i64(div_ceil(moment.0, self.moment_units_per_sample())?).map(SamplePosition)
    }

    fn last_at_or_before(&self, moment: Moment) -> Option<SamplePosition> {
        to_i64(moment.0.div_euclid(self.moment_units_per_sample())).map(SamplePosition)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::time::{TimePosition, TimeUnits, place, read};

    #[test]
    fn every_standard_rate_is_accepted() {
        for hertz in SampleRate::STANDARD_HERTZ {
            assert!(SampleRate::new(hertz).is_some(), "{hertz} Hz");
        }
    }

    #[test]
    fn rates_out_of_range_or_off_the_time_unit_are_refused() {
        for hertz in [0, 1, 7_999, 44_056, 47_952, 48_048, 768_001, u32::MAX] {
            assert_eq!(SampleRate::new(hertz), None, "{hertz} Hz");
        }
    }

    #[test]
    fn a_sample_is_a_whole_number_of_time_units_both_ways() -> Result<(), Box<dyn Error>> {
        let rate = SampleRate::new(44_100).ok_or("44.1 kHz is a rate")?;
        assert_eq!(rate.time_units_per_sample(), 20_480_000);
        let sample = SamplePosition(-123_456_789);
        let time = place(sample, &rate, &TimeUnits).ok_or("the sample has a time")?;
        assert_eq!(time, TimePosition(-123_456_789 * 20_480_000));
        assert_eq!(place(time, &TimeUnits, &rate), Some(sample));
        assert_eq!(read(time, &TimeUnits, &rate), Some(sample));
        Ok(())
    }

    #[test]
    fn a_time_between_samples_is_placed_after_and_read_before() -> Result<(), Box<dyn Error>> {
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let time = TimePosition(rate.time_units_per_sample() * 10 + 1);
        assert_eq!(place(time, &TimeUnits, &rate), Some(SamplePosition(11)));
        assert_eq!(read(time, &TimeUnits, &rate), Some(SamplePosition(10)));
        Ok(())
    }
}
