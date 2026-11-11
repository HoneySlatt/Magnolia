//! Musical time: ticks, tempos, meters, and the map that lays them on the absolute timeline.

// The period converts time with this module's code: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::error::Error;
use std::fmt;
use std::ops::Range;

use super::{
    Grid, Moment, SamplePosition, SampleRate, TIME_UNITS_PER_SECOND, TimePosition, TimeUnits,
    div_ceil, read, to_i64,
};

/// Ticks in one quarter note: 4,838,400 = 2^10 · 3^3 · 5^2 · 7. Binary divisions down to 1/1024 of
/// a quarter, triplets three deep, quintuplets two deep and septuplets are whole numbers of ticks,
/// and so are the common resolutions of MIDI files: 24, 48, 96, 120, 192, 240, 384, 480, 960,
/// 1,920, 3,840, 7,680, 9,600 and 15,360 ticks a quarter. Others, such as 1,000 or 2,048, round
/// on import. A tick is no longer than a sample at 768 kHz from 9.53 quarters per minute up.
pub const TICKS_PER_QUARTER: i64 = 4_838_400;

/// A musical position in ticks, [`TICKS_PER_QUARTER`] to a quarter note, counted from the
/// [`TempoMap`]'s start: tick 0 is the first beat of bar 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MusicalPosition(pub i64);

/// A tempo: the length of a quarter note, a whole number of time units. Between 1 and 1,000
/// quarter notes a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tempo {
    quarter: i64,
}

impl Tempo {
    /// The slowest tempo, in quarter notes a minute.
    pub const MIN_BPM: u32 = 1;
    /// The fastest tempo, in quarter notes a minute.
    pub const MAX_BPM: u32 = 1_000;

    const TIME_UNITS_PER_MINUTE: i64 = 60 * TIME_UNITS_PER_SECOND;
    /// The quarter note of the fastest tempo, in time units.
    const SHORTEST_QUARTER: i64 = Self::TIME_UNITS_PER_MINUTE / Self::MAX_BPM as i64;
    /// The quarter note of the slowest tempo, in time units.
    const LONGEST_QUARTER: i64 = Self::TIME_UNITS_PER_MINUTE / Self::MIN_BPM as i64;

    /// The tempo whose quarter note lasts `time_units`, or `None` outside the tempos accepted.
    pub fn from_quarter_duration(time_units: i64) -> Option<Tempo> {
        (Self::SHORTEST_QUARTER..=Self::LONGEST_QUARTER)
            .contains(&time_units)
            .then_some(Tempo {
                quarter: time_units,
            })
    }

    /// The tempo of `numerator / denominator` quarter notes a minute, as typed: 174 BPM is
    /// `(174, 1)`, 93.5 is `(187, 2)`. Its quarter note is the nearest whole number of time units,
    /// a half rounded up: within 5.6e-13 s of the exact length. `None` outside the tempos accepted.
    pub fn from_bpm(numerator: u32, denominator: u32) -> Option<Tempo> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        // At most 2^45.6 · 2^32 · 2 + 2^32: nothing overflows, so `None` means a tempo out of range.
        let length =
            i128::from(Self::TIME_UNITS_PER_MINUTE).checked_mul(i128::from(denominator))?;
        let numerator = i128::from(numerator);
        let nearest = length
            .checked_mul(2)?
            .checked_add(numerator)?
            .checked_div_euclid(numerator.checked_mul(2)?)?;
        Self::from_quarter_duration(to_i64(nearest)?)
    }

    /// The length of a quarter note, in time units.
    pub fn quarter_duration(self) -> i64 {
        self.quarter
    }
}

/// A meter: so many beats to a bar, each a note of some value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Meter {
    beats: u16,
    beat_unit: u16,
}

impl Meter {
    /// The shortest beat accepted: a sixty-fourth note.
    pub const MAX_BEAT_UNIT: u16 = 64;
    /// The ticks in a whole note.
    const TICKS_PER_WHOLE_NOTE: i64 = 4 * TICKS_PER_QUARTER;

    /// `beats` beats of `beat_unit` notes, such as 7/8 as `(7, 8)`; `None` unless there is at
    /// least one beat and the unit is a power of two from 1 to 64.
    pub fn new(beats: u16, beat_unit: u16) -> Option<Meter> {
        let unit_accepted = beat_unit.is_power_of_two() && beat_unit <= Self::MAX_BEAT_UNIT;
        (beats >= 1 && unit_accepted).then_some(Meter { beats, beat_unit })
    }

    /// The beats in a bar.
    pub fn beats(self) -> u16 {
        self.beats
    }

    /// The note value of a beat: 4 for a quarter note, 8 for an eighth.
    pub fn beat_unit(self) -> u16 {
        self.beat_unit
    }

    /// The ticks in one beat.
    pub fn ticks_per_beat(self) -> i64 {
        // The beat unit is a power of two from 1 to 64, so the division never fails and the
        // fallback, a whole note, is never taken.
        Self::TICKS_PER_WHOLE_NOTE
            .checked_div(i64::from(self.beat_unit))
            .unwrap_or(Self::TICKS_PER_WHOLE_NOTE)
    }

    /// The ticks in one bar.
    pub fn ticks_per_bar(self) -> i64 {
        // At most 2^16 · 2^24.3: never saturates.
        i64::from(self.beats).saturating_mul(self.ticks_per_beat())
    }
}

/// A musical position in bars, beats and ticks, each counted from zero: tick 0 of the map is bar 0,
/// beat 0, tick 0. Shown to a user, bars and beats usually count from one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bbt {
    /// The bar.
    pub bar: i64,
    /// The beat in the bar, below the meter's beats.
    pub beat: u16,
    /// The tick in the beat, below the meter's ticks per beat.
    pub tick: i64,
}

/// Why a [`TempoMap`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempoMapError {
    /// The first tempo change is not at tick 0, or there is none.
    TempoNotFromStart,
    /// The first meter change is not at bar 0, or there is none.
    MeterNotFromStart,
    /// Two tempo changes are not in increasing order of ticks.
    TempoChangesOutOfOrder,
    /// Two meter changes are not in increasing order of bars.
    MeterChangesOutOfOrder,
    /// A change lies beyond the absolute timeline or the ticks' range.
    OutOfRange,
}

impl fmt::Display for TempoMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            TempoMapError::TempoNotFromStart => "the first tempo change must be at tick 0",
            TempoMapError::MeterNotFromStart => "the first meter change must be at bar 0",
            TempoMapError::TempoChangesOutOfOrder => "tempo changes must be in increasing order",
            TempoMapError::MeterChangesOutOfOrder => "meter changes must be in increasing order",
            TempoMapError::OutOfRange => "a change lies beyond the timeline's range",
        })
    }
}

impl Error for TempoMapError {}

/// Where musical time lies on the absolute timeline: tempo changes at ticks, meter changes at bars,
/// and the time position of tick 0, the song's start.
///
/// The tempo between two changes is constant. The first tempo and the first meter also hold before
/// tick 0. Every moment is computed exactly from the change before it, so nothing accumulates
/// error, however many changes there are. The map is built off the real-time thread; its lookups
/// neither allocate nor lock, and cost a binary search over the changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TempoMap {
    origin: TimePosition,
    tempo_segments: Segments<TempoSegment>,
    meter_segments: Segments<MeterSegment>,
}

/// A map's segments of one kind: never empty, so that every position lies in one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Segments<S> {
    /// The segment from the start, which also holds before it.
    first: S,
    /// The others, in increasing order of their starts.
    later: Vec<S>,
}

impl<S> Segments<S> {
    /// The last segment that `has_started`, or the first when no later one has. Since the later
    /// segments are in order, `has_started` holds for the first few of them and no other.
    fn last_started(&self, has_started: impl FnMut(&S) -> bool) -> &S {
        let started = self.later.partition_point(has_started);
        started
            .checked_sub(1)
            .and_then(|index| self.later.get(index))
            .unwrap_or(&self.first)
    }

    /// The last segment.
    fn last(&self) -> &S {
        self.later.last().unwrap_or(&self.first)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TempoSegment {
    start: MusicalPosition,
    start_moment: Moment,
    tempo: Tempo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MeterSegment {
    first_bar: i64,
    start: MusicalPosition,
    meter: Meter,
}

impl TempoMap {
    /// A map whose tick 0 lies at `origin`, with tempo changes at ticks and meter changes at bars,
    /// each list in increasing order and starting at tick 0 and bar 0.
    pub fn new(
        origin: TimePosition,
        tempo_changes: &[(MusicalPosition, Tempo)],
        meter_changes: &[(i64, Meter)],
    ) -> Result<TempoMap, TempoMapError> {
        Ok(TempoMap {
            origin,
            tempo_segments: tempo_segments(origin, tempo_changes)?,
            meter_segments: meter_segments(meter_changes)?,
        })
    }

    /// A map of one tempo and one meter from tick 0, at `origin`.
    pub fn constant(
        origin: TimePosition,
        tempo: Tempo,
        meter: Meter,
    ) -> Result<TempoMap, TempoMapError> {
        TempoMap::new(origin, &[(MusicalPosition(0), tempo)], &[(0, meter)])
    }

    /// The time position of tick 0.
    pub fn origin(&self) -> TimePosition {
        self.origin
    }

    /// The tempo at a musical position.
    pub fn tempo_at(&self, position: MusicalPosition) -> Tempo {
        self.tempo_segment_at(position).tempo
    }

    /// The meter at a musical position.
    pub fn meter_at(&self, position: MusicalPosition) -> Meter {
        self.meter_segment_at(position).meter
    }

    /// The bar, beat and tick a musical position lies in.
    pub fn bbt_at(&self, position: MusicalPosition) -> Option<Bbt> {
        let segment = self.meter_segment_at(position);
        // The difference of two `i64` and a meter's positive lengths: no step fails.
        let offset = i128::from(position.0).checked_sub(i128::from(segment.start.0))?;
        let ticks_per_bar = i128::from(segment.meter.ticks_per_bar());
        let ticks_per_beat = i128::from(segment.meter.ticks_per_beat());
        let bars = offset.checked_div_euclid(ticks_per_bar)?;
        let in_bar = offset.checked_rem_euclid(ticks_per_bar)?;
        Some(Bbt {
            bar: to_i64(i128::from(segment.first_bar).checked_add(bars)?)?,
            beat: u16::try_from(in_bar.checked_div(ticks_per_beat)?).ok()?,
            tick: to_i64(in_bar.checked_rem(ticks_per_beat)?)?,
        })
    }

    /// The musical position of a bar, beat and tick, or `None` if its beat or tick lies beyond its
    /// bar's meter, or the position beyond the ticks' range.
    pub fn position_of(&self, bbt: Bbt) -> Option<MusicalPosition> {
        let segment = self
            .meter_segments
            .last_started(|segment| segment.first_bar <= bbt.bar);
        let meter = segment.meter;
        if bbt.beat >= meter.beats() || !(0..meter.ticks_per_beat()).contains(&bbt.tick) {
            return None;
        }
        // At most 2^64 · 2^40.3 plus less than a bar: no step overflows an `i128`.
        let bars = i128::from(bbt.bar).checked_sub(i128::from(segment.first_bar))?;
        let in_bar = i128::from(bbt.beat)
            .checked_mul(i128::from(meter.ticks_per_beat()))?
            .checked_add(i128::from(bbt.tick))?;
        let ticks = bars
            .checked_mul(i128::from(meter.ticks_per_bar()))?
            .checked_add(i128::from(segment.start.0))?
            .checked_add(in_bar)?;
        to_i64(ticks).map(MusicalPosition)
    }

    /// The ticks whose events play in a block of `frames` samples from `start`: the events
    /// [`place`](super::place)d on a sample from `start` to `start + frames - 1`. Each tick plays
    /// in exactly one block. `None` only at the ends of the ranges of samples or ticks.
    pub fn ticks_in_block(
        &self,
        rate: SampleRate,
        start: SamplePosition,
        frames: u32,
    ) -> Option<Range<MusicalPosition>> {
        // place(tick) <= s exactly when tick <= read(s), so the block's ticks are those after
        // read(start - 1) up to read(start + frames - 1).
        let before = SamplePosition(start.0.checked_sub(1)?);
        let last = SamplePosition(before.0.checked_add(i64::from(frames))?);
        let first = read(before, &rate, self)?.0.checked_add(1)?;
        let end = read(last, &rate, self)?.0.checked_add(1)?;
        Some(MusicalPosition(first)..MusicalPosition(end))
    }

    fn tempo_segment_at(&self, position: MusicalPosition) -> &TempoSegment {
        self.tempo_segments
            .last_started(|segment| segment.start <= position)
    }

    fn tempo_segment_at_moment(&self, moment: Moment) -> &TempoSegment {
        self.tempo_segments
            .last_started(|segment| segment.start_moment <= moment)
    }

    fn meter_segment_at(&self, position: MusicalPosition) -> &MeterSegment {
        self.meter_segments
            .last_started(|segment| segment.start <= position)
    }
}

/// In the unit of moments, a tick lasts as many units as its tempo's quarter note lasts time units.
impl Grid for TempoMap {
    type Position = MusicalPosition;

    fn moment_of(&self, position: MusicalPosition) -> Option<Moment> {
        let segment = self.tempo_segment_at(position);
        // The difference of two `i64` never overflows an `i128`.
        let ticks = i128::from(position.0).checked_sub(i128::from(segment.start.0))?;
        let offset = ticks.checked_mul(i128::from(segment.tempo.quarter))?;
        segment.start_moment.0.checked_add(offset).map(Moment)
    }

    // A quarter note lasts at least 2^35.6 time units, so the ticks since a segment's start are
    // fewer than 2^92, and adding an `i64` to them never overflows an `i128`.

    fn first_at_or_after(&self, moment: Moment) -> Option<MusicalPosition> {
        let segment = self.tempo_segment_at_moment(moment);
        let since = moment.0.checked_sub(segment.start_moment.0)?;
        let ticks = div_ceil(since, i128::from(segment.tempo.quarter))?;
        to_i64(i128::from(segment.start.0).checked_add(ticks)?).map(MusicalPosition)
    }

    fn last_at_or_before(&self, moment: Moment) -> Option<MusicalPosition> {
        let segment = self.tempo_segment_at_moment(moment);
        let since = moment.0.checked_sub(segment.start_moment.0)?;
        let ticks = since.div_euclid(i128::from(segment.tempo.quarter));
        to_i64(i128::from(segment.start.0).checked_add(ticks)?).map(MusicalPosition)
    }
}

fn tempo_segments(
    origin: TimePosition,
    changes: &[(MusicalPosition, Tempo)],
) -> Result<Segments<TempoSegment>, TempoMapError> {
    let Some((&(MusicalPosition(0), first_tempo), later_changes)) = changes.split_first() else {
        return Err(TempoMapError::TempoNotFromStart);
    };
    let origin_moment = TimeUnits
        .moment_of(origin)
        .ok_or(TempoMapError::OutOfRange)?;
    let mut segments = Segments {
        first: TempoSegment {
            start: MusicalPosition(0),
            start_moment: origin_moment,
            tempo: first_tempo,
        },
        later: Vec::with_capacity(later_changes.len()),
    };
    for &(start, tempo) in later_changes {
        let previous = *segments.last();
        if start <= previous.start {
            return Err(TempoMapError::TempoChangesOutOfOrder);
        }
        // The difference of two `i64` never overflows an `i128`: only the moment can.
        let start_moment = i128::from(start.0)
            .checked_sub(i128::from(previous.start.0))
            .and_then(|ticks| ticks.checked_mul(i128::from(previous.tempo.quarter)))
            .and_then(|length| previous.start_moment.0.checked_add(length))
            .map(Moment)
            .ok_or(TempoMapError::OutOfRange)?;
        // Every change lies on the absolute timeline, so any moment between two changes converts.
        if TimeUnits.last_at_or_before(start_moment).is_none() {
            return Err(TempoMapError::OutOfRange);
        }
        segments.later.push(TempoSegment {
            start,
            start_moment,
            tempo,
        });
    }
    Ok(segments)
}

fn meter_segments(changes: &[(i64, Meter)]) -> Result<Segments<MeterSegment>, TempoMapError> {
    let Some((&(0, first_meter), later_changes)) = changes.split_first() else {
        return Err(TempoMapError::MeterNotFromStart);
    };
    let mut segments = Segments {
        first: MeterSegment {
            first_bar: 0,
            start: MusicalPosition(0),
            meter: first_meter,
        },
        later: Vec::with_capacity(later_changes.len()),
    };
    for &(first_bar, meter) in later_changes {
        let previous = *segments.last();
        if first_bar <= previous.first_bar {
            return Err(TempoMapError::MeterChangesOutOfOrder);
        }
        // Bars from 0 on, in increasing order: their difference never overflows.
        let start = first_bar
            .checked_sub(previous.first_bar)
            .and_then(|bars| bars.checked_mul(previous.meter.ticks_per_bar()))
            .and_then(|length| previous.start.0.checked_add(length))
            .ok_or(TempoMapError::OutOfRange)?;
        segments.later.push(MeterSegment {
            first_bar,
            start: MusicalPosition(start),
            meter,
        });
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::time::place;

    fn four_four() -> Result<Meter, &'static str> {
        Meter::new(4, 4).ok_or("4/4 is a meter")
    }

    fn bpm(beats_per_minute: u32) -> Result<Tempo, &'static str> {
        Tempo::from_bpm(beats_per_minute, 1).ok_or("the tempo is in range")
    }

    #[test]
    fn ticks_per_quarter_divide_the_common_resolutions_and_tuplets() {
        let midi = [
            24, 48, 96, 120, 192, 240, 384, 480, 960, 1_920, 3_840, 7_680, 9_600, 15_360,
        ];
        let tuplets = [1_024, 27, 25, 7, 9, 5, 3];
        for division in midi.into_iter().chain(tuplets) {
            assert_eq!(TICKS_PER_QUARTER % division, 0, "{division}");
        }
        for rounded in [1_000, 2_048, 11, 13] {
            assert_ne!(TICKS_PER_QUARTER % rounded, 0, "{rounded}");
        }
    }

    #[test]
    fn tempos_out_of_range_or_empty_are_refused() {
        assert_eq!(Tempo::from_bpm(0, 1), None);
        assert_eq!(Tempo::from_bpm(1, 0), None);
        assert_eq!(Tempo::from_bpm(1_001, 1), None);
        assert_eq!(Tempo::from_bpm(1, 2), None);
        assert!(Tempo::from_bpm(1, 1).is_some());
        assert!(Tempo::from_bpm(1_000, 1).is_some());
        assert!(Tempo::from_bpm(u32::MAX, u32::MAX).is_some());
        assert_eq!(Tempo::from_quarter_duration(0), None);
        assert_eq!(Tempo::from_quarter_duration(-1), None);
    }

    #[test]
    fn a_typed_tempo_reads_back_as_typed() -> Result<(), Box<dyn Error>> {
        for (numerator, denominator) in [(174, 1), (187, 2), (120, 1), (7_001, 1_000)] {
            let quarter = Tempo::from_bpm(numerator, denominator)
                .ok_or("the tempo is in range")?
                .quarter_duration();
            let thousandths = (60_000 * i128::from(TIME_UNITS_PER_SECOND)
                + i128::from(quarter) / 2)
                / i128::from(quarter);
            let typed = 1_000 * i128::from(numerator) / i128::from(denominator);
            assert_eq!(thousandths, typed, "{numerator}/{denominator}");
        }
        Ok(())
    }

    #[test]
    fn a_tempo_rounds_to_the_nearest_time_unit() -> Result<(), Box<dyn Error>> {
        // 174 BPM: 60/174 s = 311,437,241,379.31 time units.
        assert_eq!(bpm(174)?.quarter_duration(), 311_437_241_379);
        // 120 BPM: exactly half a second.
        assert_eq!(bpm(120)?.quarter_duration(), TIME_UNITS_PER_SECOND / 2);
        // 1,048,576/1,125 BPM: a quarter of 58,139,648,437.5 time units, a half rounded up.
        let on_a_half = Tempo::from_bpm(1_048_576, 1_125).ok_or("the tempo is in range")?;
        assert_eq!(on_a_half.quarter_duration(), 58_139_648_438);
        Ok(())
    }

    #[test]
    fn meters_need_a_beat_and_a_power_of_two() -> Result<(), Box<dyn Error>> {
        assert_eq!(Meter::new(0, 4), None);
        assert_eq!(Meter::new(4, 0), None);
        assert_eq!(Meter::new(4, 3), None);
        assert_eq!(Meter::new(4, 128), None);
        assert_eq!(
            Meter::new(7, 8).ok_or("7/8 is a meter")?.ticks_per_bar(),
            7 * TICKS_PER_QUARTER / 2
        );
        assert_eq!(
            Meter::new(1, 64).ok_or("1/64 is a meter")?.ticks_per_beat(),
            302_400
        );
        Ok(())
    }

    #[test]
    fn a_map_needs_ordered_changes_from_the_start() -> Result<(), Box<dyn Error>> {
        let origin = TimePosition(0);
        let tempo = [(MusicalPosition(0), bpm(120)?)];
        let meter = [(0, four_four()?)];
        assert_eq!(
            TempoMap::new(origin, &[], &meter),
            Err(TempoMapError::TempoNotFromStart)
        );
        assert_eq!(
            TempoMap::new(origin, &[(MusicalPosition(1), bpm(120)?)], &meter),
            Err(TempoMapError::TempoNotFromStart)
        );
        assert_eq!(
            TempoMap::new(origin, &tempo, &[]),
            Err(TempoMapError::MeterNotFromStart)
        );
        assert_eq!(
            TempoMap::new(origin, &tempo, &[(1, four_four()?)]),
            Err(TempoMapError::MeterNotFromStart)
        );
        let repeated = [
            (MusicalPosition(0), bpm(120)?),
            (MusicalPosition(0), bpm(90)?),
        ];
        assert_eq!(
            TempoMap::new(origin, &repeated, &meter),
            Err(TempoMapError::TempoChangesOutOfOrder)
        );
        let backwards = [(0, four_four()?), (4, four_four()?), (2, four_four()?)];
        assert_eq!(
            TempoMap::new(origin, &tempo, &backwards),
            Err(TempoMapError::MeterChangesOutOfOrder)
        );
        Ok(())
    }

    #[test]
    fn a_change_beyond_the_timeline_is_refused() -> Result<(), Box<dyn Error>> {
        let slowest = Tempo::from_bpm(1, 1).ok_or("1 BPM is a tempo")?;
        let far = [
            (MusicalPosition(0), slowest),
            (MusicalPosition(i64::MAX), bpm(120)?),
        ];
        assert_eq!(
            TempoMap::new(TimePosition(0), &far, &[(0, four_four()?)]),
            Err(TempoMapError::OutOfRange)
        );
        let meters = [
            (0, Meter::new(u16::MAX, 1).ok_or("65535/1 is a meter")?),
            (i64::MAX, four_four()?),
        ];
        let tempo = [(MusicalPosition(0), bpm(120)?)];
        assert_eq!(
            TempoMap::new(TimePosition(0), &tempo, &meters),
            Err(TempoMapError::OutOfRange)
        );
        Ok(())
    }

    #[test]
    fn bar_one_thousand_at_120_bpm_falls_on_its_exact_sample() -> Result<(), Box<dyn Error>> {
        let map = TempoMap::constant(TimePosition(0), bpm(120)?, four_four()?)?;
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let bar = map
            .position_of(Bbt {
                bar: 1_000,
                beat: 0,
                tick: 0,
            })
            .ok_or("bar 1,000 has a position")?;
        assert_eq!(bar, MusicalPosition(1_000 * 4 * TICKS_PER_QUARTER));
        assert_eq!(place(bar, &map, &rate), Some(SamplePosition(96_000_000)));
        Ok(())
    }

    #[test]
    fn the_origin_moves_the_song_start() -> Result<(), Box<dyn Error>> {
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let one_second = TimePosition(TIME_UNITS_PER_SECOND);
        let map = TempoMap::constant(one_second, bpm(120)?, four_four()?)?;
        assert_eq!(
            place(MusicalPosition(0), &map, &rate),
            Some(SamplePosition(48_000))
        );
        assert_eq!(
            read(SamplePosition(0), &rate, &map),
            Some(MusicalPosition(-2 * TICKS_PER_QUARTER))
        );
        Ok(())
    }

    #[test]
    fn a_tempo_change_starts_where_the_previous_tempo_ends() -> Result<(), Box<dyn Error>> {
        let changes = [
            (MusicalPosition(0), bpm(120)?),
            (MusicalPosition(4 * TICKS_PER_QUARTER), bpm(60)?),
        ];
        let map = TempoMap::new(TimePosition(0), &changes, &[(0, four_four()?)])?;
        let rate = SampleRate::new(44_100).ok_or("44.1 kHz is a rate")?;
        // Four quarters at 120 BPM last two seconds; the fifth, at 60 BPM, one more.
        let fifth = MusicalPosition(5 * TICKS_PER_QUARTER);
        assert_eq!(
            place(MusicalPosition(4 * TICKS_PER_QUARTER), &map, &rate),
            Some(SamplePosition(88_200))
        );
        assert_eq!(place(fifth, &map, &rate), Some(SamplePosition(132_300)));
        assert_eq!(map.tempo_at(fifth), bpm(60)?);
        assert_eq!(map.tempo_at(MusicalPosition(-1)), bpm(120)?);
        Ok(())
    }

    #[test]
    fn bars_beats_and_ticks_follow_meter_changes() -> Result<(), Box<dyn Error>> {
        let meters = [
            (0, four_four()?),
            (2, Meter::new(7, 8).ok_or("7/8 is a meter")?),
        ];
        let map = TempoMap::new(TimePosition(0), &[(MusicalPosition(0), bpm(120)?)], &meters)?;
        let eighth = TICKS_PER_QUARTER / 2;
        let bar_two = 8 * TICKS_PER_QUARTER;
        let cases = [
            (
                0,
                Bbt {
                    bar: 0,
                    beat: 0,
                    tick: 0,
                },
            ),
            (
                bar_two - 1,
                Bbt {
                    bar: 1,
                    beat: 3,
                    tick: TICKS_PER_QUARTER - 1,
                },
            ),
            (
                bar_two,
                Bbt {
                    bar: 2,
                    beat: 0,
                    tick: 0,
                },
            ),
            (
                bar_two + 7 * eighth + 3 * eighth + 5,
                Bbt {
                    bar: 3,
                    beat: 3,
                    tick: 5,
                },
            ),
            (
                -1,
                Bbt {
                    bar: -1,
                    beat: 3,
                    tick: TICKS_PER_QUARTER - 1,
                },
            ),
        ];
        for (ticks, bbt) in cases {
            assert_eq!(map.bbt_at(MusicalPosition(ticks)), Some(bbt), "{ticks}");
            assert_eq!(
                map.position_of(bbt),
                Some(MusicalPosition(ticks)),
                "{bbt:?}"
            );
        }
        assert_eq!(
            map.meter_at(MusicalPosition(bar_two)),
            Meter::new(7, 8).ok_or("7/8 is a meter")?
        );
        // Beat 6 exists in bar 2's 7/8, not in the 4/4 of bar 1.
        let late_beat = Bbt {
            bar: 2,
            beat: 6,
            tick: 0,
        };
        let late_tick = MusicalPosition(bar_two + 6 * eighth);
        assert_eq!(map.position_of(late_beat), Some(late_tick));
        assert_eq!(
            map.position_of(Bbt {
                bar: 1,
                ..late_beat
            }),
            None
        );
        Ok(())
    }

    #[test]
    fn a_beat_or_tick_beyond_its_bar_has_no_position() -> Result<(), Box<dyn Error>> {
        let map = TempoMap::constant(
            TimePosition(0),
            bpm(120)?,
            Meter::new(3, 4).ok_or("3/4 is a meter")?,
        )?;
        assert_eq!(
            map.position_of(Bbt {
                bar: 0,
                beat: 3,
                tick: 0
            }),
            None
        );
        assert_eq!(
            map.position_of(Bbt {
                bar: 0,
                beat: 0,
                tick: TICKS_PER_QUARTER
            }),
            None
        );
        assert_eq!(
            map.position_of(Bbt {
                bar: 0,
                beat: 0,
                tick: -1
            }),
            None
        );
        assert_eq!(
            map.position_of(Bbt {
                bar: i64::MAX,
                beat: 0,
                tick: 0
            }),
            None
        );
        Ok(())
    }

    #[test]
    fn every_tick_plays_in_exactly_one_block_on_its_placed_sample() -> Result<(), Box<dyn Error>> {
        let changes = [
            (MusicalPosition(0), bpm(174)?),
            (MusicalPosition(1_000_003), bpm(61)?),
        ];
        let map = TempoMap::new(TimePosition(-12_345), &changes, &[(0, four_four()?)])?;
        let rate = SampleRate::new(44_100).ok_or("44.1 kHz is a rate")?;
        let frames = 64;
        let mut expected_first = None;
        for block in -50..200 {
            let start = SamplePosition(block * i64::from(frames));
            let ticks = map
                .ticks_in_block(rate, start, frames)
                .ok_or("the block has ticks")?;
            if let Some(first) = expected_first {
                assert_eq!(ticks.start, first, "blocks must tile the ticks");
            }
            expected_first = Some(ticks.end);
            for tick in [ticks.start.0, ticks.end.0 - 1] {
                let sample =
                    place(MusicalPosition(tick), &map, &rate).ok_or("the tick has a sample")?;
                assert!((start.0..start.0 + i64::from(frames)).contains(&sample.0));
            }
        }
        let empty = map
            .ticks_in_block(rate, SamplePosition(0), 0)
            .ok_or("an empty block has ticks")?;
        assert!(empty.is_empty());
        Ok(())
    }

    #[test]
    fn a_block_has_ticks_unless_it_reaches_an_end_of_the_ranges() -> Result<(), Box<dyn Error>> {
        let map = TempoMap::constant(TimePosition(0), bpm(120)?, four_four()?)?;
        let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
        let longest = map
            .ticks_in_block(rate, SamplePosition(0), u32::MAX)
            .ok_or("the longest block has ticks")?;
        let last = place(MusicalPosition(longest.end.0 - 1), &map, &rate)
            .ok_or("the last tick has a sample")?;
        assert_eq!(last, SamplePosition(i64::from(u32::MAX) - 1));
        assert_eq!(map.ticks_in_block(rate, SamplePosition(i64::MIN), 64), None);
        // At 120 BPM and 48 kHz, a sample holds 201.6 ticks: the last ticks lie near this sample.
        let last_ticks = SamplePosition(i64::MAX / 202 - 2 * i64::from(u32::MAX));
        assert!(map.ticks_in_block(rate, last_ticks, u32::MAX).is_some());
        let beyond_the_ticks = SamplePosition(i64::MAX / 201);
        assert_eq!(map.ticks_in_block(rate, beyond_the_ticks, 64), None);
        Ok(())
    }
}
