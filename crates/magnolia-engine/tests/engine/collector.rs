//! What the processor lets go is dropped by the collector thread, never by the processor's own,
//! and only once its swap is steady; a slot is reused only then, under a new generation. While the
//! collector's queue is full, a swap waits for it, and events go on.

use std::thread;

use magnolia_engine::engine::{Commit, Destination, EngineConfig, NodeId, Source};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::Gain;

use crate::support::{
    Constant, DropWitness, DroppedBy, Gate, LARGEST_BLOCK, Rig, assert_matches, rate, read,
    wait_for,
};

#[test]
fn a_removed_node_is_dropped_by_the_collector_once_its_swap_is_steady() {
    let mut rig = Rig::new(0, 1);
    let (witness, dropped_by) = DropWitness::new();
    let witness = rig.add(witness);
    rig.controller
        .connect(Source::Node(witness, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);

    rig.controller.remove_node(witness).unwrap();
    rig.controller.commit().unwrap();
    // A fade lasts 240 samples: after 128 the node still fades out.
    rig.play(&[64, 64]);
    assert!(
        read(&dropped_by).is_none(),
        "dropped before its swap was steady"
    );
    let during_the_fade = rig.add(Constant::new(0.0));
    assert_ne!(
        during_the_fade.slot, witness.slot,
        "slot reused during the fade"
    );

    rig.play_until_steady(64);
    wait_for("the collector to drop the node", || {
        read(&dropped_by).is_some()
    });
    let (thread_id, thread_name) = read(&dropped_by).clone().unwrap();
    assert_ne!(
        thread_id,
        thread::current().id(),
        "dropped on the processor's thread"
    );
    assert_eq!(thread_name.as_deref(), Some("magnolia-collector"));

    let reused = rig.add(Constant::new(0.0));
    assert_eq!(
        reused,
        NodeId {
            slot: witness.slot,
            generation: witness.generation + 1,
        }
    );
}

/// A silent node whose drop panics.
struct PanicsWhenDropped;

impl Node for PanicsWhenDropped {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        context.output_mut(0).fill(0.0);
        ProcessStatus::Continue
    }
}

impl Drop for PanicsWhenDropped {
    fn drop(&mut self) {
        panic!("a test node's drop panics, on purpose");
    }
}

#[test]
fn the_collector_outlives_a_node_whose_drop_panics() {
    let mut rig = Rig::new(0, 1);
    let panics = rig.add(PanicsWhenDropped);
    let (witness, dropped_by) = DropWitness::new();
    let witness = rig.add(witness);
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    rig.controller.remove_node(panics).unwrap();
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    rig.controller.remove_node(witness).unwrap();
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    wait_for("the collector to drop the second node", || {
        read(&dropped_by).is_some()
    });
}

/// An engine whose collector is stuck in a node's drop, its queue full and one item waiting in an
/// overflow slot, so that the next swap must wait until the gate opens.
struct Busy {
    rig: Rig,
    gate: Gate,
    /// A constant 0.25 on the playback channel, which no swap touches.
    untouched: NodeId,
    /// Which thread dropped the node in the overflow slot.
    overflowed_dropped_by: DroppedBy,
}

fn busy_collector() -> Busy {
    let mut config = EngineConfig::new(rate(), LARGEST_BLOCK, 0, 1);
    config.garbage_capacity = EngineConfig::MIN_GARBAGE_CAPACITY;
    let mut rig = Rig::with_config(config);
    let gate = Gate::new();
    let untouched = rig.add(Constant::new(0.25));
    let blocking = rig.add(gate.node());
    let (overflowed, overflowed_dropped_by) = DropWitness::new();
    let overflowed = rig.add(overflowed);
    rig.controller
        .connect(Source::Node(untouched, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    play_until_applied(&mut rig, 1);
    // Swap 2 drops the blocking node, the last item it hands the collector: the collector takes
    // all three, then stays in the node's drop, its queue empty. Swap 2 itself may wait for the
    // collector to drop what swap 1 replaced.
    rig.controller.remove_node(blocking).unwrap();
    rig.controller.commit().unwrap();
    play_until_applied(&mut rig, 2);
    gate.wait_until_entered();
    // Swap 3 grows the pool and drops a node, without changing an edge, so it is steady at once:
    // the old pool, the used message and the old graph fill the queue, and the dropped node
    // waits in an overflow slot.
    for _ in 0..40 {
        rig.add(Gain::new(1, 1.0));
    }
    rig.controller.remove_node(overflowed).unwrap();
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    assert_eq!(rig.controller.stats().last_swap_steady(), 3);
    Busy {
        rig,
        gate,
        untouched,
        overflowed_dropped_by,
    }
}

/// Plays periods, a millisecond apart, until the processor has applied swap `sequence`: it waits
/// while the collector has not dropped what the last swap replaced.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's thread lets the collector run between periods; it never runs on the real-time path"
)]
fn play_until_applied(rig: &mut Rig, sequence: u64) {
    for _ in 0..5_000 {
        rig.play(&[64]);
        if rig.controller.stats().last_swap_applied() >= sequence {
            return;
        }
        thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("swap {sequence} was not applied in 5,000 periods");
}

/// Adds a constant at `value` on the playback channel, and commits it: swap 4.
fn add_a_source(rig: &mut Rig, value: f32) -> NodeId {
    let source = rig.add(Constant::new(value));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 4 }
    );
    source
}

#[test]
fn a_swap_waits_while_the_collectors_queue_is_full_and_is_applied_once_it_drains() {
    let Busy {
        mut rig,
        gate,
        overflowed_dropped_by,
        ..
    } = busy_collector();
    add_a_source(&mut rig, 0.5);
    rig.play(&[64; 20]);
    let stats = rig.controller.stats().clone();
    assert_eq!(stats.last_swap_applied(), 3, "the swap waited");
    assert!(read(&overflowed_dropped_by).is_none());

    gate.open();
    play_until_applied(&mut rig, 4);
    rig.play_until_steady(64);
    assert_eq!(stats.last_swap_applied(), 4);
    assert_eq!(stats.last_swap_steady(), 4);
    assert_matches(&rig.play(&[64]), &[0.75; 64], 0.0, "after the swap");
    wait_for(
        "the collector to drop the node from the overflow slot",
        || read(&overflowed_dropped_by).is_some(),
    );
}

#[test]
fn an_event_for_an_untouched_node_plays_on_time_while_a_swap_waits() {
    let Busy {
        mut rig,
        gate,
        untouched,
        ..
    } = busy_collector();
    add_a_source(&mut rig, 0.5);
    let next = rig.controller.stats().block_start().0 + 64;
    rig.send(untouched, next + 10, 0, 0.75);
    let played = rig.play(&[64, 64]);
    let expected: Vec<f64> = (0..128).map(|n| if n < 10 { 0.25 } else { 0.75 }).collect();
    assert_matches(&played, &expected, 0.0, "the untouched node's event");
    assert_eq!(
        rig.controller.stats().last_swap_applied(),
        3,
        "the swap waited"
    );
    gate.open();
}

#[test]
fn an_event_for_a_node_a_waiting_swap_brings_reaches_it_once_the_swap_is_applied() {
    let Busy { mut rig, gate, .. } = busy_collector();
    let added = add_a_source(&mut rig, 0.0);
    let next = rig.controller.stats().block_start().0 + 64;
    rig.send(added, next, 0, 0.5);
    rig.play(&[64; 10]);
    let stats = rig.controller.stats().clone();
    assert_eq!(stats.last_swap_applied(), 3, "the swap waited");

    gate.open();
    play_until_applied(&mut rig, 4);
    rig.play_until_steady(64);
    assert_matches(&rig.play(&[64]), &[0.75; 64], 0.0, "the new node's event");
    assert_eq!(stats.events_dropped(), 0);
}
