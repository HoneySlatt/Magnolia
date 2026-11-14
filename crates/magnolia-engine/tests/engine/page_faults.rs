//! A swap takes no page fault on the processing thread: every page the processor will touch was
//! written by the controller first; nor do audio players, whose streams' slots are written when
//! they are made, nor capture taps, whose rings are written when they are made. RealtimeSanitizer
//! cannot see a page fault, which is not a call; the kernel counts them per thread, in field 10 of
//! `/proc/thread-self/stat`.

use std::fs::File;
use std::io::Read;
use std::time::Duration;

use magnolia_engine::disk::BLOCK_FRAMES;
use magnolia_engine::engine::{Commit, Destination, NodeId, Source};
use magnolia_engine::nodes::{Gain, Sine, TestLatency};
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::SampleFormat;

use crate::disk_support::{DiskRig, Reader, TempDir, TrackSpec, clip, write_wav};
use crate::record_support::RecordRig;
use crate::support::{Rig, rate};

const PERIOD: usize = 64;

/// The minor page faults of the calling thread so far. Reads into a buffer on the stack, so that
/// reading takes no fault of its own once warm.
fn minor_faults() -> u64 {
    let mut buffer = [0u8; 1_024];
    let mut file = File::open("/proc/thread-self/stat").unwrap();
    let length = file.read(&mut buffer).unwrap();
    let stat = std::str::from_utf8(&buffer[..length]).unwrap();
    // The command's name, in parentheses, may hold spaces: count the fields after it. Field 3,
    // the state, comes first; field 10 is the seventh after it.
    let after_name = &stat[stat.rfind(')').unwrap() + 1..];
    after_name
        .split_whitespace()
        .nth(7)
        .unwrap()
        .parse()
        .unwrap()
}

/// Adds a sine through a gain to the playback channel, as a track would be.
fn add_track(rig: &mut Rig) -> [NodeId; 2] {
    let sine = rig.add(Sine::new(rate(), 220.0, 0.01));
    let gain = rig.add(Gain::new(1, 0.5));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Playback(0));
    [sine, gain]
}

/// Commits, then plays periods until the swap is steady and the dropped nodes are sent, counting
/// the faults of those periods alone: the controller works before the first count.
fn faults_across_the_swap(rig: &mut Rig) -> u64 {
    faults_across_the_swap_and_after(rig, 0)
}

/// As `faults_across_the_swap`, playing on until `frames` samples at least have played since the
/// commit, so that the nodes reach every page of their memory.
fn faults_across_the_swap_and_after(rig: &mut Rig, frames: usize) -> u64 {
    let Commit::Sent { sequence } = rig.controller.commit().unwrap() else {
        panic!("the last swap was steady: this one should have been sent");
    };
    let stats = rig.controller.stats().clone();
    let before = minor_faults();
    let mut played = 0;
    while stats.last_swap_steady() < sequence || played < frames {
        rig.backend.play_period(PERIOD, &[]);
        played += PERIOD;
    }
    rig.backend.play_period(PERIOD, &[]);
    let after = minor_faults();
    rig.controller.poll();
    after - before
}

#[test]
fn a_swap_takes_no_page_fault_on_the_processing_thread_after_warm_up() {
    let mut rig = Rig::new(0, 1);
    // Sixteen tracks give the pool room; removing eight leaves room for the swaps that follow.
    let mut tracks: Vec<[NodeId; 2]> = (0..16).map(|_| add_track(&mut rig)).collect();
    faults_across_the_swap(&mut rig);
    minor_faults();
    for track in tracks.drain(8..) {
        for node in track {
            rig.controller.remove_node(node).unwrap();
        }
    }
    faults_across_the_swap(&mut rig);

    // A swap that grows the graph within the pool: a track added, then removed, crossfading
    // each time. The first round warms the code and the stack.
    let mut within_the_pool = Vec::new();
    for _ in 0..4 {
        let track = add_track(&mut rig);
        within_the_pool.push(faults_across_the_swap(&mut rig));
        for node in track {
            rig.controller.remove_node(node).unwrap();
        }
        within_the_pool.push(faults_across_the_swap(&mut rig));
    }

    // A swap that sends a larger pool: each round more than doubles the tracks, which outgrows
    // the pool, grown by half again at the last round.
    let mut larger_pool = Vec::new();
    for _ in 0..4 {
        let added = tracks.len() + 1;
        for _ in 0..added {
            tracks.push(add_track(&mut rig));
        }
        larger_pool.push(faults_across_the_swap(&mut rig));
    }

    println!("minor faults per swap within the pool: {within_the_pool:?}");
    println!("minor faults per swap with a larger pool: {larger_pool:?}");
    // Every swap after the first round of each kind.
    assert!(
        within_the_pool[2..].iter().all(|&faults| faults == 0),
        "faults per swap within the pool: {within_the_pool:?}"
    );
    assert!(
        larger_pool[1..].iter().all(|&faults| faults == 0),
        "faults per swap with a larger pool: {larger_pool:?}"
    );
}

/// Adds a sine that plays directly and through a latency of 65,536 samples, so that its direct
/// path needs a delay line of 256 KiB: past the C library's threshold, a fresh mapping whose pages
/// the processor would fault on, unless written first.
fn add_latent_track(rig: &mut Rig) -> [NodeId; 2] {
    let sine = rig.add(Sine::new(rate(), 330.0, 0.01));
    let latent = rig.add(TestLatency::new(1, 65_536));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(latent, 0));
    rig.controller
        .connect(Source::Node(latent, 0), Destination::Playback(0));
    [sine, latent]
}

#[test]
fn a_swap_that_adds_or_removes_a_delay_line_takes_no_page_fault_after_warm_up() {
    let mut rig = Rig::new(0, 1);
    // Eight tracks give the pool room for the swaps that follow.
    let tracks: Vec<[NodeId; 2]> = (0..8).map(|_| add_track(&mut rig)).collect();
    faults_across_the_swap(&mut rig);
    for node in tracks.into_iter().flatten() {
        rig.controller.remove_node(node).unwrap();
    }
    faults_across_the_swap(&mut rig);

    // Each round adds a latent track, which brings a delay line, then removes it, which drops
    // the delay line. The first round warms the code and the stack.
    let mut faults = Vec::new();
    for _ in 0..4 {
        let track = add_latent_track(&mut rig);
        // Long enough for the delay lines to turn once: every page of their history.
        faults.push(faults_across_the_swap_and_after(&mut rig, 65_536));
        assert_eq!(rig.controller.delay_lines().len(), 1);
        for node in track {
            rig.controller.remove_node(node).unwrap();
        }
        faults.push(faults_across_the_swap(&mut rig));
        assert!(rig.controller.delay_lines().is_empty());
    }
    println!("minor faults per swap with a delay line: {faults:?}");
    assert!(
        faults[2..].iter().all(|&faults| faults == 0),
        "faults per swap with a delay line: {faults:?}"
    );
}

#[test]
fn audio_players_take_no_page_fault_on_the_processing_thread_after_warm_up() {
    let dir = TempDir::new("page-faults");
    let tracks: Vec<TrackSpec> = (0..2)
        .map(|track| {
            let name = format!("track-{track}.wav");
            let file = write_wav(&dir, &name, SampleFormat::Int24, 2, 120_000, 20 + track);
            TrackSpec {
                channels: 2,
                clips: vec![clip(&file, 0, 0, 120_000, 1.0)],
            }
        })
        .collect();
    // Eight blocks ahead: a ring of ten slots, 40,960 frames.
    let mut rig = DiskRig::new(&tracks, Reader::Thread, PERIOD, 8 * BLOCK_FRAMES as u64);
    rig.play_from(0);
    // Each round plays 400 periods of 64 frames, 25,600 frames, five eighths of the ring; the three
    // rounds measured go nearly twice round it, so the reader rewrites every slot while the players
    // read. The first round warms the code and the stack.
    let mut rounds = Vec::new();
    for _ in 0..4 {
        let mut faults = 0;
        for _ in 0..400 {
            assert!(rig.reader.wait_idle(Duration::from_secs(10)));
            let before = minor_faults();
            rig.rig.backend.play_period(PERIOD, &[]);
            faults += minor_faults() - before;
            rig.rig.controller.poll();
        }
        rounds.push(faults);
    }
    println!("minor faults per round of audio playing: {rounds:?}");
    assert_eq!(rig.rig.controller.stats().disk_underruns(), 0);
    assert!(
        rounds[1..].iter().all(|&faults| faults == 0),
        "faults per round of audio playing: {rounds:?}"
    );
}

#[test]
fn capture_taps_take_no_page_fault_on_the_processing_thread_while_the_recorder_drains() {
    // Rings of 8,192 frames: each round goes three times round them.
    let mut rig = RecordRig::new("page-faults", 3, |config| config.ring_frames = 8_192);
    rig.arm(1, "voice", &[0, 1]);
    rig.arm(2, "guitar", &[2]);
    rig.rig.controller.commit().unwrap();
    let capture: Vec<Vec<f32>> = (0..3)
        .map(|channel| {
            (0..PERIOD)
                .map(|sample| (sample + channel) as f32 * 1e-3)
                .collect()
        })
        .collect();
    let capture: Vec<&[f32]> = capture.iter().map(Vec::as_slice).collect();
    rig.rig.backend.play_period(PERIOD, &capture);
    rig.rig.controller.poll();
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    // Each round plays 400 periods of 64 frames while the recorder drains the rings and writes the
    // takes. The first round warms the code and the stack.
    let mut rounds = Vec::new();
    for _ in 0..4 {
        let mut faults = 0;
        for _ in 0..400 {
            rig.wait_idle();
            let before = minor_faults();
            rig.rig.backend.play_period(PERIOD, &capture);
            faults += minor_faults() - before;
            rig.rig.controller.poll();
        }
        rounds.push(faults);
    }
    println!("minor faults per round of recording: {rounds:?}");
    let stats = rig.rig.controller.stats().clone();
    assert_eq!(stats.capture_overruns(), 0);
    assert!(rig.recorder().stats().frames_written() > 3 * 25_600);
    assert!(
        rounds[1..].iter().all(|&faults| faults == 0),
        "faults per round of recording: {rounds:?}"
    );
}
