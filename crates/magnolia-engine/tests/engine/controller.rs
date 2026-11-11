//! The controller refuses a description it cannot compile, each fault with its own error, merges
//! the commits made while a swap is in flight into the one it sends next, and finds slots for a
//! commit's delay lines among those the commit itself frees.

use std::time::Duration;

use magnolia_engine::engine::{
    Commit, Destination, Edge, EdgeEnd, GraphError, NodeId, Source, SwapStatus, WaitError,
};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::{Gain, Sine, TestLatency};
use magnolia_engine::offline::OfflineBackend;

use crate::support::{Constant, Rig, assert_matches, rate};

#[test]
fn a_cycle_is_refused_and_names_a_node_on_it() {
    let mut rig = Rig::new(0, 1);
    let first = rig.add(Gain::new(1, 1.0));
    let second = rig.add(Gain::new(1, 1.0));
    let third = rig.add(Gain::new(1, 1.0));
    rig.controller
        .connect(Source::Node(third, 0), Destination::Node(first, 0));
    rig.controller
        .connect(Source::Node(first, 0), Destination::Node(second, 0));
    rig.controller
        .connect(Source::Node(second, 0), Destination::Node(first, 0));
    let error = rig.controller.commit().unwrap_err();
    assert!(
        error == GraphError::Cycle(first) || error == GraphError::Cycle(second),
        "{error:?}"
    );
    // The description is kept as it was, to be fixed.
    rig.controller
        .disconnect(Source::Node(second, 0), Destination::Node(first, 0))
        .unwrap();
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 1 }
    );
}

#[test]
fn an_unknown_node_is_refused() {
    let mut rig = Rig::new(0, 1);
    let never_added = NodeId {
        slot: 7,
        generation: 0,
    };
    rig.controller
        .connect(Source::Node(never_added, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit(),
        Err(GraphError::UnknownNode(never_added))
    );
    assert_eq!(
        rig.controller.remove_node(never_added),
        Err(GraphError::UnknownNode(never_added))
    );
}

#[test]
fn a_channel_out_of_range_is_refused_at_either_end() {
    let mut rig = Rig::new(1, 1);
    let gain = rig.add(Gain::new(2, 1.0));
    let cases = [
        (
            Source::Node(gain, 2),
            Destination::Playback(0),
            EdgeEnd::Source,
            2,
        ),
        (
            Source::Capture(1),
            Destination::Playback(0),
            EdgeEnd::Source,
            1,
        ),
        (
            Source::Capture(0),
            Destination::Node(gain, 5),
            EdgeEnd::Destination,
            2,
        ),
        (
            Source::Capture(0),
            Destination::Playback(1),
            EdgeEnd::Destination,
            1,
        ),
    ];
    for (source, destination, end, channels) in cases {
        rig.controller.connect(source, destination);
        let edge = Edge {
            source,
            destination,
        };
        assert_eq!(
            rig.controller.commit(),
            Err(GraphError::ChannelOutOfRange {
                edge,
                end,
                channels
            })
        );
        rig.controller.disconnect(source, destination).unwrap();
    }
}

#[test]
fn a_duplicate_edge_is_refused_and_a_missing_one_cannot_be_disconnected() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(1.0));
    let edge = Edge {
        source: Source::Node(source, 0),
        destination: Destination::Playback(0),
    };
    rig.controller.connect(edge.source, edge.destination);
    rig.controller.connect(edge.source, edge.destination);
    assert_eq!(
        rig.controller.commit(),
        Err(GraphError::DuplicateEdge(edge))
    );
    rig.controller
        .disconnect(edge.source, edge.destination)
        .unwrap();
    rig.controller
        .disconnect(edge.source, edge.destination)
        .unwrap();
    assert_eq!(
        rig.controller.disconnect(edge.source, edge.destination),
        Err(GraphError::MissingEdge(edge))
    );
}

#[test]
fn commits_made_during_a_swap_merge_into_the_next_one() {
    let mut rig = Rig::new(0, 1);
    let sources: Vec<_> = [1.0, 2.0, 4.0]
        .into_iter()
        .map(|value| rig.add(Constant::new(value)))
        .collect();
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    let connect = |rig: &mut Rig, index: usize| {
        rig.controller
            .connect(Source::Node(sources[index], 0), Destination::Playback(0));
    };
    connect(&mut rig, 0);
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    connect(&mut rig, 1);
    assert_eq!(rig.controller.commit().unwrap(), Commit::Deferred);
    connect(&mut rig, 2);
    assert_eq!(rig.controller.commit().unwrap(), Commit::Deferred);
    // Edited after the last commit: not sent until committed.
    rig.controller
        .disconnect(Source::Node(sources[2], 0), Destination::Playback(0))
        .unwrap();
    rig.play_until_steady(64);
    assert_eq!(rig.controller.stats().last_swap_steady(), 3);
    assert_matches(&rig.play(&[64]), &[7.0; 64], 0.0, "merged");
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    assert_matches(&rig.play(&[64]), &[3.0; 64], 0.0, "committed later");
}

#[test]
fn wait_until_steady_blocks_until_a_backend_on_its_own_thread_has_played_the_swap() {
    let rig = Rig::new(0, 1);
    let Rig {
        mut controller,
        backend,
    } = rig;
    let source = controller.add_node(Box::new(Constant::new(1.0))).unwrap();
    controller.connect(Source::Node(source, 0), Destination::Playback(0));
    controller.commit().unwrap();
    // No backend plays yet: the wait times out.
    assert_eq!(
        controller.wait_until_steady(Duration::from_millis(20)),
        Err(WaitError::TimedOut)
    );
    let stats = controller.stats().clone();
    let audio = std::thread::spawn(move || {
        let mut backend: OfflineBackend = backend;
        while stats.periods() < 1_000 {
            backend.play_period(64, &[]);
            std::thread::yield_now();
        }
    });
    assert_eq!(controller.wait_until_steady(Duration::from_secs(5)), Ok(()));
    assert_eq!(controller.poll(), SwapStatus::Steady);
    join(audio);
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test's thread waits for the test's backend thread; neither is real-time"
)]
fn join(thread: std::thread::JoinHandle<()>) {
    thread.join().unwrap();
}

/// A node without a channel, to fill the arena with at little cost.
struct Filler;

impl Node for Filler {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        0
    }

    fn process(&mut self, _context: &mut ProcessContext<'_>) -> ProcessStatus {
        ProcessStatus::Sleep
    }
}

/// Fills every slot the arena has left with fillers, and returns them.
fn fill_the_arena(rig: &mut Rig) -> Vec<NodeId> {
    let mut fillers = Vec::new();
    loop {
        match rig.controller.add_node(Box::new(Filler)) {
            Ok(filler) => fillers.push(filler),
            Err(GraphError::TooManyNodes) => return fillers,
            Err(error) => panic!("adding a filler: {error}"),
        }
    }
}

#[test]
fn a_commit_takes_for_its_delay_line_the_slot_of_a_node_it_abandons() {
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.5));
    let latent = rig.add(TestLatency::new(1, 100));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(latent, 0));
    rig.controller
        .connect(Source::Node(latent, 0), Destination::Playback(0));
    let fillers = fill_the_arena(&mut rig);
    // A filler removed before any commit sent it: this commit frees its slot, which the sine's
    // direct path needs for its delay line.
    rig.controller.remove_node(fillers[0]).unwrap();
    assert_eq!(rig.controller.commit(), Ok(Commit::Sent { sequence: 1 }));
    assert_eq!(rig.controller.delay_lines().len(), 1);
}

#[test]
fn a_commit_takes_for_its_delay_line_the_slot_of_a_deferred_delay_line_it_replaces() {
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.5));
    let short = rig.add(TestLatency::new(1, 100));
    let long = rig.add(TestLatency::new(1, 200));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    assert_eq!(rig.controller.commit(), Ok(Commit::Sent { sequence: 1 }));
    // The arena full but for one slot, which the deferred commit's delay line takes.
    let fillers = fill_the_arena(&mut rig);
    rig.controller.remove_node(fillers[0]).unwrap();
    rig.controller.commit().unwrap();
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(short, 0));
    rig.controller
        .connect(Source::Node(short, 0), Destination::Playback(0));
    assert_eq!(rig.controller.commit(), Ok(Commit::Deferred));
    // Moving the bus to the longer latency replaces that delay line, never sent, by another.
    rig.controller
        .disconnect(Source::Node(sine, 0), Destination::Node(short, 0))
        .unwrap();
    rig.controller
        .disconnect(Source::Node(short, 0), Destination::Playback(0))
        .unwrap();
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(long, 0));
    rig.controller
        .connect(Source::Node(long, 0), Destination::Playback(0));
    assert_eq!(rig.controller.commit(), Ok(Commit::Deferred));
}
