//! A swap takes no page fault on the processing thread: every page the processor will touch was
//! written by the controller first. RealtimeSanitizer cannot see a page fault, which is not a
//! call; the kernel counts them per thread, in field 10 of `/proc/thread-self/stat`.

use std::fs::File;
use std::io::Read;

use magnolia_engine::engine::{Commit, Destination, NodeId, Source};
use magnolia_engine::nodes::{Gain, Sine, TestLatency};

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
