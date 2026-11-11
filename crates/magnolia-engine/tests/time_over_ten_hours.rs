//! Checks the time model's rounding rule on every grid over more than ten hours, as M1.2 asks:
//! positions on random tempo maps, at every standard sample rate and every frame rate, either side
//! of zero and of each change. A tick's sample is also checked against an oracle that sums the
//! tempo segments from scratch, without the map's lookups.

mod common;

use common::{Step, check_round_trip, check_rule_at};
use magnolia_engine::time::{
    FramePosition, FrameRate, Grid, Meter, MusicalPosition, SamplePosition, SampleRate,
    TICKS_PER_QUARTER, TIME_UNITS_PER_SECOND, Tempo, TempoMap, TimePosition, TimeUnits, Timecode,
    TimecodeClock, place, read,
};

const HOUR: i64 = 3_600 * TIME_UNITS_PER_SECOND;

/// SplitMix64: a small, fixed sequence of numbers, so that a failure reproduces.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `range`, nearly uniform.
    fn between(&mut self, range: std::ops::Range<i64>) -> i64 {
        let width = (range.end - range.start) as u64;
        range.start + (self.next() % width) as i64
    }
}

/// A random tempo map: one to twelve changes over ten hours, tempos from 10 to 1,000 BPM, some
/// with a fraction, a random song start.
fn random_tempo_map(
    random: &mut Random,
) -> (TimePosition, Vec<(MusicalPosition, Tempo)>, TempoMap) {
    let origin = TimePosition(random.between(-HOUR..HOUR));
    let mut changes = vec![(MusicalPosition(0), random_tempo(random))];
    let mut tick = 0;
    for _ in 0..random.between(0..12) {
        tick += random.between(1..2_000 * TICKS_PER_QUARTER);
        changes.push((MusicalPosition(tick), random_tempo(random)));
    }
    let meters = [(0, Meter::new(4, 4).unwrap())];
    let map = TempoMap::new(origin, &changes, &meters).unwrap();
    (origin, changes, map)
}

fn random_tempo(random: &mut Random) -> Tempo {
    let denominator = [1, 2, 4, 10, 100, 1_000][random.between(0..6) as usize];
    let numerator = random.between(10 * denominator..1_000 * denominator + 1);
    Tempo::from_bpm(numerator as u32, denominator as u32).unwrap()
}

/// The sample a tick plays on, computed from scratch: the exact instant of the tick, as a fraction
/// of a second over TIME_UNITS_PER_SECOND · TICKS_PER_QUARTER, summed segment by segment.
fn oracle_sample(
    origin: TimePosition,
    changes: &[(MusicalPosition, Tempo)],
    tick: i64,
    rate: SampleRate,
) -> i64 {
    let mut instant = i128::from(origin.0) * i128::from(TICKS_PER_QUARTER);
    if tick < 0 {
        // The first tempo also holds before tick 0.
        instant += i128::from(tick) * i128::from(changes[0].1.quarter_duration());
    }
    for (index, &(start, tempo)) in changes.iter().enumerate() {
        if tick <= start.0 {
            break;
        }
        let end = changes.get(index + 1).map_or(i64::MAX, |next| next.0.0);
        let ticks_in_segment = tick.min(end) - start.0;
        instant += i128::from(ticks_in_segment) * i128::from(tempo.quarter_duration());
    }
    let per_sample =
        i128::from(TIME_UNITS_PER_SECOND / i64::from(rate.hertz())) * i128::from(TICKS_PER_QUARTER);
    // The first sample at or after the instant.
    let floor = instant.div_euclid(per_sample);
    let ceiling = if instant.rem_euclid(per_sample) == 0 {
        floor
    } else {
        floor + 1
    };
    i64::try_from(ceiling).unwrap()
}

#[test]
fn samples_and_time_units_follow_the_rule_and_round_trip_over_ten_hours() {
    let mut random = Random(1);
    for hertz in SampleRate::STANDARD_HERTZ {
        let rate = SampleRate::new(hertz).unwrap();
        for _ in 0..2_000 {
            let time = TimePosition(random.between(-HOUR..11 * HOUR));
            assert!(check_rule_at(&rate, TimeUnits.moment_of(time).unwrap()));
            let sample = read(time, &TimeUnits, &rate).unwrap();
            assert!(check_round_trip(sample, &rate, &TimeUnits));
        }
    }
}

#[test]
fn ticks_follow_the_rule_and_the_oracle_over_ten_hours() {
    let mut random = Random(2);
    for _ in 0..1_500 {
        let (origin, changes, map) = random_tempo_map(&mut random);
        let rate =
            SampleRate::new(SampleRate::STANDARD_HERTZ[random.between(0..15) as usize]).unwrap();
        let mut ticks: Vec<i64> = changes
            .iter()
            .flat_map(|&(start, _)| (-3..=3).map(move |delta| start.0 + delta))
            .collect();
        for _ in 0..100 {
            let time = TimePosition(random.between(-2 * HOUR..11 * HOUR));
            ticks.push(read(time, &TimeUnits, &map).unwrap().0);
            assert!(check_rule_at(&map, TimeUnits.moment_of(time).unwrap()));
            let sample = read(time, &TimeUnits, &rate).unwrap();
            assert!(check_rule_at(&map, rate.moment_of(sample).unwrap()));
            assert!(check_rule_at(
                &rate,
                map.moment_of(read(sample, &rate, &map).unwrap()).unwrap(),
            ));
        }
        for tick in ticks {
            let tick = MusicalPosition(tick);
            let sample = place(tick, &map, &rate).unwrap();
            assert_eq!(
                sample.0,
                oracle_sample(origin, &changes, tick.0, rate),
                "{tick:?}"
            );
            assert!(check_rule_at(&rate, map.moment_of(tick).unwrap()));
            // Where a tick is no longer than a sample, on both sides of the tick, a sample read as
            // a tick comes back whole.
            let slowest = map
                .tempo_at(tick)
                .quarter_duration()
                .max(map.tempo_at(tick.step(-1).unwrap()).quarter_duration());
            if slowest <= rate.time_units_per_sample() * TICKS_PER_QUARTER {
                let read_sample = read(tick, &map, &rate).unwrap();
                assert!(check_round_trip(read_sample, &rate, &map));
            }
        }
    }
}

#[test]
fn frames_follow_the_rule_and_round_trip_over_ten_hours_at_every_rate() {
    let mut random = Random(3);
    for frame_rate in FrameRate::ALL {
        for _ in 0..200 {
            let day = frame_rate.frames_per_day();
            let start_label = Timecode::from_frame_number(random.between(0..day), frame_rate);
            let start = TimePosition(random.between(-HOUR..HOUR));
            let clock = TimecodeClock::new(frame_rate, start_label, start).unwrap();
            let rate = SampleRate::new(SampleRate::STANDARD_HERTZ[random.between(0..15) as usize])
                .unwrap();
            let (_, _, map) = random_tempo_map(&mut random);
            for _ in 0..40 {
                let time = TimePosition(random.between(-HOUR..11 * HOUR));
                assert!(check_rule_at(&clock, TimeUnits.moment_of(time).unwrap()));
                let frame = read(time, &TimeUnits, &clock).unwrap();
                assert_eq!(clock.frame_of(clock.label(frame)), Some(frame));
                // Samples, time units and ticks are each finer than a frame.
                assert!(check_round_trip(frame, &clock, &rate));
                assert!(check_round_trip(frame, &clock, &TimeUnits));
                assert!(check_round_trip(frame, &clock, &map));
            }
        }
    }
}

#[test]
fn a_ten_hour_film_at_29_97_drop_frame_lands_on_its_exact_sample() {
    let rate = FrameRate::Fps29_97Drop;
    let start = Timecode {
        hours: 1,
        minutes: 0,
        seconds: 0,
        frames: 0,
    };
    let clock = TimecodeClock::new(rate, start, TimePosition(0)).unwrap();
    let end = Timecode { hours: 11, ..start };
    let frame = clock.frame_of(end).unwrap();
    assert_eq!(frame, FramePosition(1_078_920));
    let samples = SampleRate::new(48_000).unwrap();
    // 1,078,920 frames of 1,601.6 samples: 1,727,998,272 exactly.
    assert_eq!(
        place(frame, &clock, &samples),
        Some(SamplePosition(1_727_998_272))
    );
    assert_eq!(
        read(SamplePosition(1_727_998_272), &samples, &clock),
        Some(frame)
    );
    assert_eq!(
        read(SamplePosition(1_727_998_271), &samples, &clock),
        Some(frame.step(-1).unwrap())
    );
}

/// Why conversions go through exact moments, never through a third grid: at 120 BPM, tick 322,780
/// lies 0.0333561 s in, before frame 1 of 29.97 (0.0333667 s), but on sample 1,471.0026 at
/// 44.1 kHz. Through samples it would play on sample 1,472, which lies in frame 1.
#[test]
fn a_ticks_frame_does_not_depend_on_the_sample_rate() {
    let tempo = Tempo::from_bpm(120, 1).unwrap();
    let map = TempoMap::constant(TimePosition(0), tempo, Meter::new(4, 4).unwrap()).unwrap();
    let start = Timecode {
        hours: 0,
        minutes: 0,
        seconds: 0,
        frames: 0,
    };
    let clock = TimecodeClock::new(FrameRate::Fps29_97, start, TimePosition(0)).unwrap();
    let tick = MusicalPosition(322_780);
    assert_eq!(read(tick, &map, &clock), Some(FramePosition(0)));
    for hertz in SampleRate::STANDARD_HERTZ {
        let rate = SampleRate::new(hertz).unwrap();
        let sample = place(tick, &map, &rate).unwrap();
        // The sample a tick plays on may lie in the next frame; the tick itself does not move.
        assert!(read(sample, &rate, &clock).unwrap() >= FramePosition(0));
    }
    let rate = SampleRate::new(44_100).unwrap();
    let sample = place(tick, &map, &rate).unwrap();
    assert_eq!(sample, SamplePosition(1_472));
    assert_eq!(read(sample, &rate, &clock), Some(FramePosition(1)));
}
