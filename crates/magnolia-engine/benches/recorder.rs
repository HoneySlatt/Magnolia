//! Measures the recorder with 1, 8 and 32 armed mono tracks, each recording 10 s of noise at
//! 48 kHz in real time, in periods of 256 frames: the passes of the recorder's thread and the
//! longest, which is how long a ring may wait beyond its wake; the syncs and the longest; the
//! fullest ring; the frames dropped; and the processor's callback times, the capture tap included,
//! on the benchmark's thread of normal priority. 1 and 8 tracks run with the default 4 sync
//! threads; 32 tracks with 8, 4, 2 and 1, so that what syncing in parallel gains and costs shows. `cargo bench --bench recorder`; the figures go to docs/measurements.md.
//!
//! The takes go to the folder `MAGNOLIA_BENCH_DIR` names, or the system's temporary folder, and are
//! removed at the end: at most 32 files of 1.9 MB a run.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use magnolia_engine::engine::{self, Destination, EngineConfig, Source};
use magnolia_engine::nodes::Sine;
use magnolia_engine::offline::OfflineBackend;
use magnolia_engine::record::{RecordConfig, RecordTrack, Recorder};
use magnolia_engine::time::{SamplePosition, SampleRate};

const RATE: u32 = 48_000;
const PERIOD: usize = 256;
const SECONDS: usize = 10;

/// A folder of the benchmark's own, removed with its files when dropped.
struct Folder(PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() {
    println!("the recorder, {SECONDS} s recorded in real time, periods of {PERIOD} frames");
    for (tracks, sync_threads) in [(1, 4), (8, 4), (32, 8), (32, 4), (32, 2), (32, 1)] {
        measure(tracks, sync_threads);
    }
}

/// Records `tracks` mono tracks for [`SECONDS`] in real time with `sync_threads` sync threads, and
/// prints what the recorder says of it.
#[expect(
    clippy::disallowed_methods,
    reason = "the benchmark's thread paces its periods; it stands for a backend, not for the real-time code"
)]
fn measure(tracks: usize, sync_threads: usize) {
    let root =
        std::env::var_os("MAGNOLIA_BENCH_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let folder = Folder(root.join(format!("magnolia-bench-recorder-{}", std::process::id())));
    fs::create_dir_all(&folder.0).unwrap();
    let rate = SampleRate::new(RATE).unwrap();
    let (mut controller, processor) =
        engine::new(EngineConfig::new(rate, PERIOD, tracks, 1)).unwrap();
    let sine = controller
        .add_node(Box::new(Sine::new(rate, 440.0, 0.1)))
        .unwrap();
    controller.connect(Source::Node(sine, 0), Destination::Playback(0));
    let mut config = RecordConfig::new(&folder.0, rate);
    config.sync_threads = sync_threads;
    let mut recorder = Recorder::start(config, controller.stats()).unwrap();
    for track in 0..tracks {
        let tap = recorder
            .tap(RecordTrack {
                number: track as u32 + 1,
                name: "bench".into(),
                capture_channels: vec![track],
            })
            .unwrap();
        controller.arm(tap).unwrap();
    }
    controller.commit().unwrap();
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let capture: Vec<Vec<f32>> = (0..tracks)
        .map(|_| {
            (0..PERIOD)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state >> 40) as f32 / (1 << 24) as f32 - 0.5
                })
                .collect()
        })
        .collect();
    let capture: Vec<&[f32]> = capture.iter().map(Vec::as_slice).collect();
    let mut backend = OfflineBackend::new(processor);
    backend.play_period(PERIOD, &capture);
    controller.poll();
    controller.play(SamplePosition(0)).unwrap();
    let periods = SECONDS * RATE as usize / PERIOD;
    let period_time = Duration::from_secs_f64(PERIOD as f64 / f64::from(RATE));
    let start = Instant::now();
    for period in 0..periods {
        let due = start + period_time * period as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        backend.play_period(PERIOD, &capture);
        controller.poll();
    }
    controller.stop().unwrap();
    for _ in 0..8 {
        backend.play_period(PERIOD, &capture);
        controller.poll();
    }
    assert!(recorder.wait_idle(Duration::from_secs(60)));
    let stats = recorder.stats();
    let fullest = (1..=tracks as u32)
        .filter_map(|track| recorder.fullest_fill(track))
        .map(|fill| fill.frames)
        .max()
        .unwrap_or(0);
    let callbacks = controller.stats().callback_times.snapshot();
    let mean = |total: Duration, count: u64| total.checked_div(count as u32).unwrap_or_default();
    println!(
        "{tracks:>2} tracks, {sync_threads} sync threads: {} takes, {} frames written, {} dropped",
        stats.takes_finished(),
        stats.frames_written(),
        controller.stats().capture_overruns()
    );
    println!(
        "   passes: {}, mean {:?}, longest {:?}; syncs: {}, mean {:?}, longest {:?}",
        stats.passes(),
        mean(stats.pass_time(), stats.passes()),
        stats.longest_pass(),
        stats.syncs(),
        mean(stats.sync_time(), stats.syncs()),
        stats.longest_sync()
    );
    println!(
        "   fullest ring: {fullest} frames; callbacks: median {:?}, 99.9th {:?}, longest {:?}; \
         write errors: {}",
        callbacks.quantile(1, 2).unwrap_or_default(),
        callbacks.quantile(999, 1_000).unwrap_or_default(),
        callbacks.maximum(),
        stats.write_errors()
    );
    drop(recorder);
}
