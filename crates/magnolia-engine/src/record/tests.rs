//! The recorder's unit tests: the order in which it reads its two rings, a power cut replayed on a
//! take's own writes and syncs, and a panic of the capture tap.

use std::error::Error;
use std::fs;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::Duration;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use rtrb::{Producer, RingBuffer};

use super::files::FileOp;
use super::marks::{Drained, EndReason, Mark, MarkKind, Next, count, drain, next, step};
use super::tap::{RingFills, TapRings};
use super::{RecordConfig, RecordTrack, Recorder, TakeEnd};
use crate::engine::{self, Destination, EngineConfig, Source};
use crate::nodes::Sine;
use crate::offline::OfflineBackend;
use crate::time::{SamplePosition, SampleRate};
use crate::wav::{RepairableFile, decode, read_info, repair};

type TestResult = Result<(), Box<dyn Error>>;

// ---- The order of the rings (`marks.rs`) ----

/// A tap's two rings as a test pushes into them, the recorder's ends of them, and what the
/// recorder's drain made of them: each take it opened, with the frames it received.
struct Rings {
    frames: Producer<f32>,
    marks: Producer<Mark>,
    rings: TapRings,
    /// Frames the tap pushed.
    pushed: u64,
    takes: Vec<Vec<f32>>,
}

impl Rings {
    fn new() -> Self {
        let (frames, frames_out) = RingBuffer::new(64);
        let (marks, marks_out) = RingBuffer::new(8);
        Self {
            frames,
            marks,
            rings: TapRings {
                frames: frames_out,
                marks: marks_out,
                fills: Arc::new(RingFills {
                    frames: AtomicU64::new(0),
                    marks: AtomicU64::new(0),
                }),
                consumed: 0,
            },
            pushed: 0,
            takes: Vec::new(),
        }
    }

    fn push_mark(&mut self, kind: MarkKind) {
        let position = self.pushed;
        assert!(self.marks.push(Mark { position, kind }).is_ok());
    }

    fn push_frames(&mut self, value: f32, count: u64) {
        for _ in 0..count {
            assert!(self.frames.push(value).is_ok());
        }
        self.pushed = self.pushed.saturating_add(count);
    }

    /// A take's start, then its frames, all of `value`.
    fn push_take(&mut self, value: f32, count: u64) {
        self.push_mark(MarkKind::Start {
            play: 1,
            first_sample: 0,
            round_trip: 0,
        });
        self.push_frames(value, count);
    }

    fn push_end(&mut self, length: u64) {
        self.push_mark(MarkKind::End {
            length,
            reason: EndReason::Stopped,
        });
    }

    /// The recorder's own count of the mono frames waiting, read first.
    fn count(&self) -> u64 {
        count(&self.rings, 1)
    }

    /// The recorder's own step, from frames counted before: what it receives goes to the takes.
    fn step(&mut self, available: u64) -> Next {
        let takes = &mut self.takes;
        step(&mut self.rings, 1, available, &mut |drained| {
            receive(takes, drained)
        })
    }

    /// The recorder's own drain.
    fn drain(&mut self) -> bool {
        let takes = &mut self.takes;
        drain(&mut self.rings, 1, &mut |drained| receive(takes, drained))
    }
}

/// Files what the drain hands over: a start opens a take, frames go to the last take opened.
fn receive(takes: &mut Vec<Vec<f32>>, drained: Drained<'_>) {
    match drained {
        Drained::Frames(samples) => {
            if let Some(take) = takes.last_mut() {
                take.extend_from_slice(samples);
            }
        }
        Drained::Mark(Mark {
            kind: MarkKind::Start { .. },
            ..
        }) => takes.push(Vec::new()),
        Drained::Mark(_) => {}
    }
}

/// The recorder counts the frames, then the tap ends take A, starts take B and pushes B's frames,
/// then the recorder looks at the marks, through its own step and drain: A's frames go to A, B's to
/// B.
#[test]
fn frames_counted_before_the_marks_go_to_their_own_take() {
    let mut rings = Rings::new();
    rings.push_take(1.0, 10);
    let available = rings.count();
    rings.push_end(10);
    rings.push_take(2.0, 5);
    rings.push_end(5);
    assert_eq!(rings.step(available), Next::ApplyMark, "the start of A");
    assert_eq!(
        rings.step(available),
        Next::Frames(10),
        "A's frames, counted"
    );
    assert!(rings.drain());
    assert_eq!(rings.takes, [vec![1.0; 10], vec![2.0; 5]]);
    assert!(!rings.drain(), "nothing more");
}

/// A mark beyond the frames counted, the count read before the frames it follows were pushed: the
/// step asks to count again, and the drain does, reaching the mark.
#[test]
fn a_mark_beyond_the_frames_counted_makes_the_recorder_count_again() {
    let mut rings = Rings::new();
    rings.push_take(1.0, 4);
    assert!(rings.drain());
    let available = rings.count();
    rings.push_frames(1.0, 2);
    rings.push_end(6);
    assert_eq!(available, 0);
    assert_eq!(rings.step(available), Next::CountAgain);
    assert!(rings.drain());
    assert_eq!(rings.takes, [vec![1.0; 6]]);
    let end = Mark {
        position: 12,
        kind: MarkKind::End {
            length: 12,
            reason: EndReason::Stopped,
        },
    };
    assert_eq!(next(10, 5, Some(&end)), Next::Frames(2));
    assert_eq!(next(12, 5, Some(&end)), Next::ApplyMark);
    assert_eq!(next(12, 5, None), Next::Frames(5));
    assert_eq!(next(12, 0, None), Next::Idle);
}

/// The other order, marks first: the recorder finds no mark beyond A's start, then counts frames
/// that the tap pushed after A's end and B's start, and writes B's frames into A. This is the bug
/// the order of `marks.rs` prevents; `next` is the decision the recorder's step makes.
#[test]
fn marks_read_before_the_frames_would_put_the_next_takes_frames_in_the_last() {
    let mut rings = Rings::new();
    rings.push_take(1.0, 10);
    assert_eq!(rings.step(0), Next::ApplyMark, "the start of A");
    // Marks first: none waits.
    let mark = rings.rings.marks.peek().ok().copied();
    assert_eq!(mark, None);
    rings.push_end(10);
    rings.push_take(2.0, 5);
    // Then the frames, counted after the tap went on: all fifteen would go to A.
    let available = rings.count();
    assert_eq!(
        next(rings.rings.consumed, available, mark.as_ref()),
        Next::Frames(15)
    );
}

// ---- A power cut replayed ----

/// A folder of its own, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "magnolia-unit-record-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A file in memory, which `repair` works on.
struct MemoryFile(Cursor<Vec<u8>>);

impl Read for MemoryFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}

impl Write for MemoryFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for MemoryFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.0.seek(to)
    }
}

impl RepairableFile for MemoryFile {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.0.get_mut().resize(len as usize, 0);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The value capture channel `channel` holds at engine sample `sample`: exact, never zero.
fn signal(channel: usize, sample: i64) -> f32 {
    let value = sample
        .rem_euclid(1 << 20)
        .saturating_add((channel as i64).saturating_mul(1 << 20))
        .saturating_add(1);
    value as f32 * (1.0 / (1 << 24) as f32)
}

/// A pseudo-random generator from a fixed seed (xorshift64*).
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Records a take of 3.5 s of three channels, logging every write and sync of its file, and
/// returns the log, the take's first engine sample, and its frames.
fn record_a_logged_take(folder: &Folder) -> Result<(Vec<FileOp>, i64, u64), Box<dyn Error>> {
    let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
    let (mut controller, processor) = engine::new(EngineConfig::new(rate, 1_024, 3, 1))?;
    let mut backend = OfflineBackend::new(processor);
    let mut recorder = Recorder::start(RecordConfig::new(&folder.0, rate), controller.stats())?;
    recorder.test_logs().enabled.store(true, Ordering::Release);
    let tap = recorder.tap(RecordTrack {
        number: 1,
        name: "voice".into(),
        capture_channels: vec![0, 1, 2],
    })?;
    controller.arm(tap)?;
    controller.commit()?;
    let mut position = 0_i64;
    let mut period = |backend: &mut OfflineBackend| {
        let buffers: Vec<Vec<f32>> = (0..3)
            .map(|channel| {
                (0..1_024)
                    .map(|offset| signal(channel, position.saturating_add(offset)))
                    .collect()
            })
            .collect();
        let buffers: Vec<&[f32]> = buffers.iter().map(Vec::as_slice).collect();
        backend.play_period(1_024, &buffers);
        position = position.saturating_add(1_024);
    };
    period(&mut backend);
    controller.play(SamplePosition(0))?;
    for index in 0..164 {
        period(&mut backend);
        if index % 4 == 3 {
            assert!(recorder.wait_idle(Duration::from_secs(20)));
        }
    }
    controller.stop()?;
    period(&mut backend);
    assert!(recorder.wait_idle(Duration::from_secs(20)));
    let audible_at = controller
        .stats()
        .play_start()
        .ok_or("the play started")?
        .audible_at
        .0;
    let takes = recorder.takes();
    let logs = recorder
        .test_logs()
        .logs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    drop(recorder);
    assert_eq!(takes.len(), 1);
    assert_eq!(logs.len(), 1);
    let (_, log) = logs.first().ok_or("the take was logged")?;
    let ops = log.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let take = takes.first().ok_or("the take ended")?;
    Ok((ops, audible_at, take.frames))
}

/// A take's writes and syncs, replayed on a file in memory that keeps every write before the last
/// sync and a random subset of the later ones, as a power cut leaves a file: `repair` keeps at
/// least every frame synced, exactly, and only whole frames.
#[test]
fn a_power_cut_keeps_every_frame_synced_exact_and_only_whole_frames() -> TestResult {
    let folder = Folder::new("power-cut")?;
    let (ops, audible_at, frames) = record_a_logged_take(&folder)?;
    let syncs = ops.iter().filter(|op| matches!(op, FileOp::Sync)).count();
    assert!(syncs >= 5, "a sync a second and two at the end: {syncs}");
    // Between two syncs, a second of audio at most, and what one pass wrote past it: the test
    // waits for the recorder every 4,096 frames.
    let mut written_since_sync: usize = 0;
    for op in &ops {
        match op {
            FileOp::Write { bytes, .. } => {
                written_since_sync = written_since_sync.saturating_add(bytes.len());
            }
            FileOp::Sync => {
                assert!(
                    written_since_sync <= (48_000 + 4_096) * 12,
                    "{written_since_sync}"
                );
                written_since_sync = 0;
            }
        }
    }
    // The file whole, as the recorder left it.
    let whole = replay(&ops, ops.len(), &mut |_| true);
    let info = read_info(&mut Cursor::new(&whole))?;
    assert!(!info.length_repaired);
    assert_eq!(info.frames, frames);
    let data_offset = info.data_offset;
    let frame_bytes: u64 = 3 * 4;
    // The whole take holds the signal; a repaired file's synced frames are then checked against
    // its bytes.
    let data_end = data_offset.saturating_add(frames.saturating_mul(frame_bytes));
    let data = whole
        .get(data_offset as usize..data_end as usize)
        .ok_or("the data lies in the file")?;
    let mut samples = vec![0.0; (frames as usize).saturating_mul(3)];
    decode(info.sample_format, 3, data, &mut samples);
    for (frame, values) in samples.chunks(3).enumerate() {
        for (channel, value) in values.iter().enumerate() {
            let expected = signal(channel, audible_at.saturating_add(frame as i64));
            assert_eq!(value.to_bits(), expected.to_bits(), "frame {frame}");
        }
    }
    let mut repaired_seen: u32 = 0;
    for seed in 1..=300_u64 {
        let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let cut = random
            .next()
            .checked_rem(ops.len() as u64)
            .unwrap_or(0)
            .saturating_add(1) as usize;
        let before_cut = ops.get(..cut).unwrap_or_default();
        let last_sync = before_cut
            .iter()
            .rposition(|op| matches!(op, FileOp::Sync))
            .unwrap_or(0);
        let synced_end = before_cut
            .iter()
            .take(last_sync)
            .filter_map(|op| match op {
                FileOp::Write { at, bytes } if *at >= data_offset => {
                    Some(at.saturating_add(bytes.len() as u64))
                }
                _ => None,
            })
            .max()
            .unwrap_or(data_offset);
        let synced_frames = synced_end
            .saturating_sub(data_offset)
            .checked_div(frame_bytes)
            .unwrap_or(0);
        let mut keep = |index: usize| index < last_sync || random.next() & 1 == 0;
        let mut file = MemoryFile(Cursor::new(replay(&ops, cut, &mut keep)));
        match repair(&mut file) {
            Ok(_) => {}
            Err(error) => {
                // Only a file whose header never reached the disk is beyond repair.
                assert_eq!(synced_frames, 0, "seed {seed}: {error}");
                continue;
            }
        }
        let bytes = file.0.into_inner();
        let info = read_info(&mut Cursor::new(&bytes))?;
        assert!(!info.length_repaired, "seed {seed}");
        assert!(
            info.frames >= synced_frames,
            "seed {seed}: {} < {synced_frames}",
            info.frames
        );
        let data_end = data_offset.saturating_add(info.frames.saturating_mul(frame_bytes));
        let length = bytes.len() as u64;
        assert!(
            length == data_end || length == data_end.saturating_add(data_end & 1),
            "seed {seed}: whole frames only"
        );
        assert_eq!(
            info.bext.map(|bext| bext.time_reference),
            Some(0),
            "seed {seed}"
        );
        assert_eq!(info.data_offset, data_offset, "seed {seed}");
        let synced = data_offset as usize
            ..data_offset.saturating_add(synced_frames.saturating_mul(frame_bytes)) as usize;
        assert!(
            bytes.get(synced.clone()) == whole.get(synced),
            "seed {seed}: the synced frames, exact"
        );
        repaired_seen = repaired_seen.saturating_add(1);
    }
    assert!(
        repaired_seen > 200,
        "most cuts fall after the header: {repaired_seen}"
    );
    Ok(())
}

/// The bytes a file holds once the first `cut` operations of `ops` happened and the writes `keep`
/// keeps reached the disk; a hole reads as zeros.
fn replay(ops: &[FileOp], cut: usize, keep: &mut dyn FnMut(usize) -> bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (index, op) in ops.iter().take(cut).enumerate() {
        let FileOp::Write { at, bytes: written } = op else {
            continue;
        };
        if !keep(index) {
            continue;
        }
        let start = *at as usize;
        let end = start.saturating_add(written.len());
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        if let Some(target) = bytes.get_mut(start..end) {
            target.copy_from_slice(written);
        }
    }
    bytes
}

// ---- A panic of the tap ----

/// A panic of the capture tap stops the tap alone, flagged: the graph plays on, unpoisoned, and the
/// recorder ends the take under way at what it holds.
#[test]
fn a_panic_of_the_tap_stops_it_alone_and_its_take_ends_at_what_it_holds() -> TestResult {
    let folder = Folder::new("tap-panic")?;
    let rate = SampleRate::new(48_000).ok_or("48 kHz is a rate")?;
    let (mut controller, processor) = engine::new(EngineConfig::new(rate, 1_024, 1, 1))?;
    let mut backend = OfflineBackend::new(processor);
    let mut recorder = Recorder::start(RecordConfig::new(&folder.0, rate), controller.stats())?;
    let sine = controller.add_node(Box::new(Sine::new(rate, 440.0, 0.5)))?;
    controller.connect(Source::Node(sine, 0), Destination::Playback(0));
    let tap = recorder.tap(RecordTrack {
        number: 1,
        name: "voice".into(),
        capture_channels: vec![0],
    })?;
    controller.arm(tap)?;
    controller.commit()?;
    let capture = vec![0.25_f32; 256];
    backend.play_period(256, &[&capture]);
    controller.play(SamplePosition(0))?;
    for _ in 0..10 {
        backend.play_period(256, &[&capture]);
    }
    recorder.link().panic_in_tap.store(true, Ordering::Relaxed);
    backend.play_period(256, &[&capture]);
    recorder.link().panic_in_tap.store(false, Ordering::Relaxed);
    let stats = controller.stats().clone();
    assert!(stats.tap_stopped());
    assert!(!stats.poisoned(), "the graph is not touched");
    let played = backend.play_period(256, &[&capture]);
    let played = played.first().ok_or("one playback channel")?;
    assert!(
        played.iter().any(|&sample| sample != 0.0),
        "the graph plays on"
    );
    assert!(recorder.wait_idle(Duration::from_secs(20)));
    let takes = recorder.takes();
    assert_eq!(takes.len(), 1);
    let take = takes.first().ok_or("the take ended")?;
    assert_eq!(take.ended, TakeEnd::TapStopped);
    assert_eq!(take.frames, 10 * 256, "the frames copied before the panic");
    assert!(take.error.is_none());
    let written = recorder.stats().frames_written();
    for _ in 0..10 {
        backend.play_period(256, &[&capture]);
    }
    assert!(recorder.wait_idle(Duration::from_secs(20)));
    assert_eq!(
        recorder.stats().frames_written(),
        written,
        "the tap records no more"
    );
    drop(recorder);
    let path = take.path.clone().ok_or("the take has a file")?;
    let info = read_info(&mut fs::File::open(&path)?)?;
    assert_eq!(info.frames, 10 * 256);
    assert!(!info.length_repaired, "it was finished");
    Ok(())
}
