//! Fuzzes the tempo map with arbitrary changes, song starts, sample rates and positions, valid or
//! not. A map that builds must follow the rounding rule at every position, read its bars back, and
//! give every tick to exactly one block; nothing may panic or overflow.

#![no_main]

#[path = "../../tests/common/mod.rs"]
mod common;

use common::{check_round_trip, check_rule_at};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_engine::time::{
    Grid, Meter, MusicalPosition, SamplePosition, SampleRate, Tempo, TempoMap, TimePosition,
    TimeUnits, place, read,
};

fuzz_target!(|data: &[u8]| {
    // An input that runs out ends its case early; that is not a failure.
    let _ = check(&mut Unstructured::new(data));
});

fn check(input: &mut Unstructured<'_>) -> Result<()> {
    let Ok(map) = TempoMap::new(
        TimePosition(input.arbitrary()?),
        &tempo_changes(input)?,
        &meter_changes(input)?,
    ) else {
        return Ok(());
    };
    let rate = sample_rate(input)?;
    for _ in 0..8 {
        let time = TimePosition(input.arbitrary()?);
        let moment = TimeUnits
            .moment_of(time)
            .expect("a time position has a moment");
        // Any time position reads and places within the ticks' and the samples' ranges: a time
        // unit is shorter than a tick and than a sample.
        assert!(
            check_rule_at(&map, moment),
            "{time:?} is out of the ticks' range"
        );
        assert!(
            check_rule_at(&rate, moment),
            "{time:?} is out of the samples' range"
        );

        let tick = MusicalPosition(input.arbitrary()?);
        if let Some(moment) = map.moment_of(tick) {
            check_rule_at(&rate, moment);
            check_rule_at(&TimeUnits, moment);
            // A time unit is shorter than a tick at any tempo.
            check_round_trip(tick, &map, &TimeUnits);
        }
        if let Some(bbt) = map.bbt_at(tick) {
            assert_eq!(map.position_of(bbt), Some(tick), "{bbt:?}");
        }

        let sample = SamplePosition(input.arbitrary()?);
        check_round_trip(sample, &rate, &TimeUnits);
        check_block(&map, rate, sample, input.int_in_range(0..=8_192)?);
    }
    Ok(())
}

/// Every tick of a block plays inside it, and the ticks just outside play outside it.
fn check_block(map: &TempoMap, rate: SampleRate, start: SamplePosition, frames: u32) {
    let in_range = ticks_in_range(map, rate, start, frames);
    let Some(ticks) = map.ticks_in_block(rate, start, frames) else {
        assert!(!in_range, "a block within the ranges has no ticks");
        return;
    };
    assert!(in_range, "a block beyond the ranges has ticks");
    // The block ends after its last sample, which may be the last of the range.
    let end = i128::from(start.0) + i128::from(frames);
    let sample =
        |tick: i64| place(MusicalPosition(tick), map, &rate).map(|sample| i128::from(sample.0));
    if ticks.is_empty() {
        assert_eq!(
            ticks.start, ticks.end,
            "a block's range never runs backwards"
        );
    } else {
        for tick in [ticks.start.0, ticks.end.0 - 1] {
            if let Some(sample) = sample(tick) {
                assert!(
                    (i128::from(start.0)..end).contains(&sample),
                    "tick {tick} plays outside its block"
                );
            }
        }
    }
    if let Some(before) = ticks.start.0.checked_sub(1).and_then(sample) {
        assert!(
            before < i128::from(start.0),
            "the tick before the block plays inside it"
        );
    }
    if let Some(after) = sample(ticks.end.0) {
        assert!(after >= end, "the tick after the block plays inside it");
    }
}

/// Whether a block lies within the ranges: its samples from `start - 1` to `start + frames - 1`,
/// and the ticks read at both ends, below the largest tick.
fn ticks_in_range(map: &TempoMap, rate: SampleRate, start: SamplePosition, frames: u32) -> bool {
    let ends = start
        .0
        .checked_sub(1)
        .and_then(|before| Some((before, before.checked_add(i64::from(frames))?)));
    let Some((before, last)) = ends else {
        return false;
    };
    let tick_in_range = |sample: i64| {
        read(SamplePosition(sample), &rate, map).is_some_and(|tick| tick.0 < i64::MAX)
    };
    tick_in_range(before) && tick_in_range(last)
}

/// Up to twelve tempo changes, mostly valid: in order from tick 0, in the tempos accepted.
fn tempo_changes(input: &mut Unstructured<'_>) -> Result<Vec<(MusicalPosition, Tempo)>> {
    let fastest = Tempo::from_bpm(Tempo::MAX_BPM, 1)
        .unwrap()
        .quarter_duration();
    let slowest = Tempo::from_bpm(Tempo::MIN_BPM, 1)
        .unwrap()
        .quarter_duration();
    let mut changes = Vec::new();
    let mut tick = 0_i64;
    for index in 0..input.int_in_range(0..=12)? {
        if input.ratio(1, 16)? {
            tick = input.arbitrary()?;
        } else if index > 0 {
            tick = tick.saturating_add(input.int_in_range(1..=1 << 40)?);
        }
        let quarter = if input.ratio(1, 16)? {
            input.arbitrary()?
        } else {
            input.int_in_range(fastest..=slowest)?
        };
        if let Some(tempo) = Tempo::from_quarter_duration(quarter) {
            changes.push((MusicalPosition(tick), tempo));
        }
    }
    Ok(changes)
}

/// Up to six meter changes, mostly valid: in order from bar 0.
fn meter_changes(input: &mut Unstructured<'_>) -> Result<Vec<(i64, Meter)>> {
    let mut changes = Vec::new();
    let mut bar = 0_i64;
    for index in 0..input.int_in_range(0..=6)? {
        if input.ratio(1, 16)? {
            bar = input.arbitrary()?;
        } else if index > 0 {
            bar = bar.saturating_add(input.int_in_range(1..=1 << 20)?);
        }
        let beat_unit = 1_u16 << input.int_in_range(0..=7)?;
        if let Some(meter) = Meter::new(input.arbitrary()?, beat_unit) {
            changes.push((bar, meter));
        }
    }
    Ok(changes)
}

/// A standard sample rate, or rarely an arbitrary one, refused unless it is valid.
fn sample_rate(input: &mut Unstructured<'_>) -> Result<SampleRate> {
    let hertz = if input.ratio(1, 8)? {
        input.arbitrary()?
    } else {
        *input.choose(&SampleRate::STANDARD_HERTZ)?
    };
    Ok(SampleRate::new(hertz).unwrap_or_else(|| SampleRate::new(48_000).unwrap()))
}
