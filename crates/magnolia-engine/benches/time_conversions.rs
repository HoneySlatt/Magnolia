//! Measures the time model's conversions as the engine calls them for each block: the ticks whose
//! events play in a block, then the sample of each event. `cargo bench --bench time_conversions`;
//! the figures go to docs/measurements.md.
//!
//! The positions are spread over ten hours, and so are the tempo changes of each map. Each figure
//! is the median of nine runs of a loop timed with `std::time::Instant`, inputs and results passed
//! through `black_box` so that the compiler cannot fold them away.

use std::hint::black_box;
use std::time::Instant;

use magnolia_engine::time::{
    Meter, MusicalPosition, SamplePosition, SampleRate, TICKS_PER_QUARTER, TIME_UNITS_PER_SECOND,
    Tempo, TempoMap, TimePosition, place, read,
};

const RUNS: usize = 9;
const BLOCK_FRAMES: u32 = 64;
const EVENTS_PER_BLOCK: i64 = 16;
const POSITIONS: i64 = 4_096;
const TEN_HOURS: i64 = 36_000 * TIME_UNITS_PER_SECOND;

/// A map of `changes` tempos from 100 to 469.3 BPM, each lasting a `changes`-th of ten hours.
fn tempo_map(changes: i64) -> TempoMap {
    let segment = TEN_HOURS / changes;
    let mut tick = 0;
    let tempos: Vec<(MusicalPosition, Tempo)> = (0..changes)
        .map(|index| {
            let tempo = Tempo::from_bpm(1_000 + 37 * (index % 100) as u32, 10).unwrap();
            let change = (MusicalPosition(tick), tempo);
            tick += segment / tempo.quarter_duration() * TICKS_PER_QUARTER;
            change
        })
        .collect();
    TempoMap::new(TimePosition(0), &tempos, &[(0, Meter::new(4, 4).unwrap())]).unwrap()
}

/// The median time of one call of `work`, in nanoseconds, over `RUNS` runs of `calls` calls.
fn median_nanoseconds(calls: usize, mut work: impl FnMut(usize)) -> f64 {
    let mut runs: Vec<f64> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            for call in 0..calls {
                work(black_box(call));
            }
            start.elapsed().as_nanos() as f64 / calls as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[RUNS / 2]
}

/// What the engine does for one block: find its ticks, then place each event on its sample.
fn convert_block(map: &TempoMap, rate: SampleRate, start: SamplePosition) -> i64 {
    let ticks = map.ticks_in_block(rate, start, BLOCK_FRAMES).unwrap();
    let step = ((ticks.end.0 - ticks.start.0) / EVENTS_PER_BLOCK).max(1);
    let mut sum = 0;
    let mut tick = ticks.start.0;
    while tick < ticks.end.0 {
        sum += place(MusicalPosition(tick), map, &rate).unwrap().0;
        tick += step;
    }
    sum
}

fn main() {
    let rate = SampleRate::new(48_000).unwrap();
    let ten_hours_of_samples = TEN_HOURS / rate.time_units_per_sample();
    // Block starts spread over ten hours, in a scattered order so that lookups jump around.
    let blocks: Vec<SamplePosition> = (0..POSITIONS)
        .map(|index| {
            let spread = (index * 1_543) % POSITIONS;
            let sample = spread * ten_hours_of_samples / POSITIONS;
            SamplePosition(sample - sample % i64::from(BLOCK_FRAMES))
        })
        .collect();
    println!("rate 48 kHz, blocks of {BLOCK_FRAMES} frames, {EVENTS_PER_BLOCK} events a block");
    for changes in [1, 1_000] {
        let map = tempo_map(changes);
        let ticks: Vec<MusicalPosition> = blocks
            .iter()
            .map(|&sample| read(sample, &rate, &map).unwrap())
            .collect();
        let count = blocks.len();
        let tick_ns = median_nanoseconds(2_000_000, |call| {
            black_box(place(black_box(ticks[call % count]), &map, &rate));
        });
        let sample_ns = median_nanoseconds(2_000_000, |call| {
            black_box(read(black_box(blocks[call % count]), &rate, &map));
        });
        let block_ns = median_nanoseconds(200_000, |call| {
            black_box(convert_block(&map, rate, black_box(blocks[call % count])));
        });
        println!(
            "{changes} tempo change(s): tick to sample {tick_ns:.1} ns, sample to tick \
             {sample_ns:.1} ns, a block {block_ns:.0} ns"
        );
    }
}
