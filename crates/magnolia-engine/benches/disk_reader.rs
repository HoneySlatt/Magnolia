//! Measures the disk reader on 16 audio tracks of 24-bit stereo at 48 kHz, each one clip of a
//! 30-second file: the time of a refill and the reader's throughput, the files cold and warm; and
//! the players' low-water mark over 30 seconds of playing in real time.
//! `cargo bench --bench disk_reader`; the figures go to docs/measurements.md.
//!
//! - **Sweep**: a reader without a thread, filled before each period of 4,096 frames, the periods
//!   played as fast as the processor goes. Its throughput is the bytes read over the time the fills
//!   took.
//! - **Real time**: a reader on its own thread, reading 10 s ahead, woken by its players, through
//!   30 s of periods of 256 frames paced by the clock: the refills' count and time, the time from a
//!   player's ask to the reader's publication of the first refill after it (`refill_latency`), the
//!   fewest frames any player found ready, the underruns, and the processor's callback times, on
//!   the benchmark's thread of normal priority.
//!
//! Cold, each file is first dropped from the page cache with GNU `dd if=<file> iflag=nocache
//! count=0`, which asks the kernel to drop it (`posix_fadvise(POSIX_FADV_DONTNEED)`) and needs no
//! privilege; warm, the same run again. The bytes that really came from the disk are read from the
//! process's `/proc/self/io`. The files go to the folder `MAGNOLIA_BENCH_DIR` names, or the
//! system's temporary folder, and are removed at the end: 16 files of 8.6 MB.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use magnolia_engine::disk::{AudioClip, AudioFile, AudioTrack, DiskConfig, DiskReader};
use magnolia_engine::engine::{self, Controller, Destination, EngineConfig, Source};
use magnolia_engine::node::Node;
use magnolia_engine::offline::OfflineBackend;
use magnolia_engine::time::{SamplePosition, SampleRate};
use magnolia_engine::wav::{SampleFormat, WavWriter};

const TRACKS: usize = 16;
const RATE: u32 = 48_000;
const SECONDS: usize = 30;
const FRAMES: usize = SECONDS * RATE as usize;
/// The bytes of a frame of 24-bit stereo.
const FRAME_BYTES: u64 = 6;

/// A folder of the benchmark's own, removed with its files when dropped.
struct Folder(PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes the 16 files, each a sine of its own pitch at half scale on both channels, and syncs
/// them to the disk.
fn write_files(folder: &Path) -> Vec<PathBuf> {
    (0..TRACKS)
        .map(|track| {
            let path = folder.join(format!("track-{track:02}.wav"));
            let file = BufWriter::new(File::create(&path).unwrap());
            let mut writer = WavWriter::new(file, SampleFormat::Int24, 2, RATE).unwrap();
            let step = std::f64::consts::TAU * (110.0 + 20.0 * track as f64) / f64::from(RATE);
            let mut second = Vec::with_capacity(2 * RATE as usize);
            for start in (0..FRAMES).step_by(RATE as usize) {
                second.clear();
                for frame in start..start + RATE as usize {
                    let sample = (0.5 * (step * frame as f64).sin()) as f32;
                    second.extend([sample, sample]);
                }
                writer.write_frames(&second).unwrap();
            }
            // Written to the disk now: the kernel drops only pages already written back.
            let file = writer.finish().unwrap().into_inner().unwrap();
            file.sync_all().unwrap();
            path
        })
        .collect()
}

/// Drops each file from the page cache, so that the next reads come from the disk.
fn drop_from_cache(paths: &[PathBuf]) {
    for path in paths {
        let status = Command::new("dd")
            .arg(format!("if={}", path.display()))
            .args(["iflag=nocache", "count=0", "status=none"])
            .status()
            .expect("GNU dd runs");
        assert!(status.success(), "dd could not drop {}", path.display());
    }
}

/// The bytes this process has caused to be read from storage so far.
fn disk_read_bytes() -> u64 {
    let io = fs::read_to_string("/proc/self/io").unwrap();
    io.lines()
        .find_map(|line| line.strip_prefix("read_bytes: "))
        .and_then(|bytes| bytes.trim().parse().ok())
        .unwrap()
}

/// One track per file, one clip of the whole file from the timeline's zero.
fn tracks(paths: &[PathBuf]) -> Vec<AudioTrack> {
    paths
        .iter()
        .map(|path| AudioTrack {
            channels: 2,
            clips: vec![AudioClip {
                file: AudioFile::open(path).unwrap(),
                start: SamplePosition(0),
                file_offset: 0,
                length: FRAMES as u64,
                gain: 1.0,
            }],
        })
        .collect()
}

/// An engine whose players all sum into two playback channels, and its reader, with a thread or
/// without.
fn rig(
    paths: &[PathBuf],
    with_thread: bool,
    largest_block: usize,
) -> (Controller, OfflineBackend, DiskReader) {
    let rate = SampleRate::new(RATE).unwrap();
    let (mut controller, processor) =
        engine::new(EngineConfig::new(rate, largest_block, 0, 2)).unwrap();
    let config = DiskConfig::new(rate);
    let tracks = tracks(paths);
    let (reader, players) = if with_thread {
        DiskReader::start(config, &tracks, controller.stats()).unwrap()
    } else {
        DiskReader::offline(config, &tracks, controller.stats()).unwrap()
    };
    for player in players {
        let channels = player.output_channels();
        let player = controller.add_node(Box::new(player)).unwrap();
        for channel in 0..channels {
            controller.connect(
                Source::Node(player, channel),
                Destination::Playback(channel),
            );
        }
    }
    controller.commit().unwrap();
    (controller, OfflineBackend::new(processor), reader)
}

fn megabytes_per_second(bytes: u64, time: Duration) -> f64 {
    bytes as f64 / 1e6 / time.as_secs_f64()
}

fn milliseconds(time: Duration) -> f64 {
    time.as_secs_f64() * 1e3
}

/// Fills before each period of 4,096 frames, as an offline render does, and plays the periods as
/// fast as the processor goes.
fn sweep(paths: &[PathBuf], cold: bool) {
    if cold {
        drop_from_cache(paths);
    }
    let from_disk = disk_read_bytes();
    let (mut controller, mut backend, mut reader) = rig(paths, false, 4_096);
    let cue = reader.locate(SamplePosition(0));
    let start = Instant::now();
    reader.fill();
    let first = start.elapsed();
    let first_bytes = reader.stats().bytes_read();
    controller.play(cue).unwrap();
    let mut refills = Vec::new();
    for _ in 0..FRAMES.div_ceil(4_096) {
        let bytes = reader.stats().bytes_read();
        let start = Instant::now();
        reader.fill();
        let time = start.elapsed();
        if reader.stats().bytes_read() > bytes {
            refills.push(time);
        }
        backend.play_period(4_096, &[]);
    }
    let bytes = reader.stats().bytes_read() - first_bytes;
    let time: Duration = refills.iter().sum();
    let longest = refills.iter().max().copied().unwrap_or_default();
    println!(
        "sweep, {}: first fill {:.1} ms for {:.1} MB ({:.0} MB/s); {} refills of {:.2} MB, \
         {:.2} ms each on average, {:.2} ms at most, {:.0} MB/s, {:.0} times real time; \
         {:.1} MB from the disk; {} underruns",
        if cold { "cold" } else { "warm" },
        milliseconds(first),
        first_bytes as f64 / 1e6,
        megabytes_per_second(first_bytes, first),
        refills.len(),
        bytes as f64 / 1e6 / refills.len() as f64,
        milliseconds(time) / refills.len() as f64,
        milliseconds(longest),
        megabytes_per_second(bytes, time),
        (bytes / FRAME_BYTES) as f64 / f64::from(RATE) / time.as_secs_f64(),
        (disk_read_bytes() - from_disk) as f64 / 1e6,
        controller.stats().disk_underruns(),
    );
}

/// Plays 30 s in periods of 256 frames paced by the clock, the reader on its own thread.
#[expect(
    clippy::disallowed_methods,
    reason = "the benchmark's thread paces its periods; it stands for a backend, not for the real-time code"
)]
fn real_time(paths: &[PathBuf], cold: bool) {
    if cold {
        drop_from_cache(paths);
    }
    let from_disk = disk_read_bytes();
    let (mut controller, mut backend, mut reader) = rig(paths, true, 256);
    let start = Instant::now();
    let cue = reader.locate(SamplePosition(0));
    reader.wait_primed(cue, Duration::from_secs(30)).unwrap();
    let primed = start.elapsed();
    assert!(reader.wait_idle(Duration::from_secs(30)));
    let filled = start.elapsed();
    let first_bytes = reader.stats().bytes_read();
    let (first_refills, first_time) = (reader.stats().refills(), reader.stats().refill_time());
    controller.play(cue).unwrap();
    let period = Duration::from_secs_f64(256.0 / f64::from(RATE));
    let start = Instant::now();
    for index in 0..FRAMES / 256 {
        let deadline = start + period * index as u32;
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        backend.play_period(256, &[]);
        controller.poll();
    }
    let played = start.elapsed();
    let callbacks = controller.stats().callback_times.snapshot();
    let latency = reader.stats().refill_latency();
    let stats = reader.stats();
    let refills = stats.refills() - first_refills;
    let time = stats.refill_time() - first_time;
    let bytes = stats.bytes_read() - first_bytes;
    let low_waters: Vec<u64> = (0..TRACKS)
        .filter_map(|track| reader.low_water(track))
        .collect();
    let lowest = low_waters.iter().min().copied().unwrap_or_default();
    let highest = low_waters.iter().max().copied().unwrap_or_default();
    println!(
        "real time, {}: primed in {:.1} ms, read-ahead filled in {:.1} ms ({:.1} MB); {:.1} s \
         played; {} refills of {:.1} MB, {:.1} ms each on average, {:.1} ms at most (any refill), \
         {:.0} MB/s; {} asks, each published {:.1} µs after at the median, {:.1} µs at most; \
         low-water {:.2} s to {:.2} s ahead ({} to {} frames) over {} players; \
         {:.1} MB from the disk; {} underruns; callbacks of 256 frames: median {:.1} µs, \
         99.9th percentile {:.1} µs, longest {:.1} µs",
        if cold { "cold" } else { "warm" },
        milliseconds(primed),
        milliseconds(filled),
        first_bytes as f64 / 1e6,
        played.as_secs_f64(),
        refills,
        bytes as f64 / 1e6 / refills.max(1) as f64,
        milliseconds(time) / refills.max(1) as f64,
        milliseconds(stats.longest_refill()),
        megabytes_per_second(bytes, time),
        latency.count(),
        microseconds(latency.quantile(1, 2)),
        microseconds(Some(latency.maximum())),
        lowest as f64 / f64::from(RATE),
        highest as f64 / f64::from(RATE),
        lowest,
        highest,
        low_waters.len(),
        (disk_read_bytes() - from_disk) as f64 / 1e6,
        controller.stats().disk_underruns(),
        microseconds(callbacks.quantile(1, 2)),
        microseconds(callbacks.quantile(999, 1_000)),
        microseconds(Some(callbacks.maximum())),
    );
}

fn microseconds(time: Option<Duration>) -> f64 {
    time.unwrap_or_default().as_secs_f64() * 1e6
}

fn main() {
    let base = std::env::var_os("MAGNOLIA_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let folder = Folder(base.join(format!("magnolia-disk-bench-{}", std::process::id())));
    fs::create_dir_all(&folder.0).unwrap();
    println!(
        "{TRACKS} tracks of 24-bit stereo at {} kHz, {SECONDS} s each, in {}",
        RATE / 1_000,
        folder.0.display()
    );
    let paths = write_files(&folder.0);
    sweep(&paths, true);
    sweep(&paths, false);
    real_time(&paths, true);
    real_time(&paths, false);
}
