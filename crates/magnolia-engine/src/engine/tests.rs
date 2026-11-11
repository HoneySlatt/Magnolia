//! Unit tests of the engine's parts that the public interface cannot reach: the fade's table, the
//! choice between a crossfade and a dip, and the collector's overflow.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

use rtrb::RingBuffer;

use super::arena::{Arena, new_chunk};
use super::collector::{Garbage, GarbageOut, RELEASED_PER_SWAP};
use super::compile::{CompileContext, compile, crossfade_gain, nodes_fed_by};
use super::compiled::GainShape;
use super::description::{Description, NodeShape};
use super::fade::{FadeTable, Gain, fade_frames};
use super::graph::{Destination, Edge, NodeId, Source};
use crate::time::SampleRate;

#[test]
fn a_fade_lasts_five_milliseconds_to_the_nearest_sample() {
    let frames = |hertz| fade_frames(SampleRate::new(hertz).unwrap());
    assert_eq!(frames(48_000), 240);
    assert_eq!(frames(44_100), 221);
    assert_eq!(frames(8_000), 40);
    assert_eq!(frames(192_000), 960);
}

#[test]
fn a_fade_in_and_its_fade_out_sum_to_one_at_every_sample() {
    let table = FadeTable::new(240);
    let (Gain::FadeIn(fade_in), Gain::FadeOut(fade_out)) = (
        table.gain(GainShape::FadeIn, 0, 240),
        table.gain(GainShape::FadeOut, 0, 240),
    ) else {
        panic!("a fading phase reads the table");
    };
    for (index, (&rising, &falling)) in fade_in.iter().zip(fade_out).enumerate() {
        assert_eq!(rising + (1.0 - falling), rising + (1.0 - rising));
        assert_eq!(rising + (1.0 - rising), 1.0, "sample {index}");
        // The raised cosine is symmetric: the fade-out is the fade-in read backwards.
        let mirror = fade_in[239 - index];
        assert!(
            (1.0 - rising - mirror).abs() <= f32::EPSILON,
            "sample {index}"
        );
    }
    assert!(fade_in[0] > 0.0 && fade_in[239] < 1.0);
    assert!(fade_in.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn past_its_table_a_fade_has_ended() {
    let table = FadeTable::new(240);
    assert!(matches!(table.gain(GainShape::FadeIn, 200, 64), Gain::One));
    assert!(matches!(table.gain(GainShape::FadeOut, 240, 1), Gain::Zero));
    assert!(matches!(table.gain(GainShape::Constant, 0, 64), Gain::One));
}

const CONTEXT: CompileContext = CompileContext {
    capture_channels: 0,
    playback_channels: 1,
    fade_frames: 240,
};

fn node(slot: u32) -> NodeId {
    NodeId {
        slot,
        generation: 0,
    }
}

/// A description of `slots`, each a node of one input and one output, and the chains given.
fn description(slots: &[u32], chains: &[&[u32]]) -> Description {
    let mut description = Description::default();
    for &slot in slots {
        let shape = NodeShape {
            inputs: 1,
            outputs: 1,
        };
        description.nodes.insert(node(slot), shape);
    }
    for chain in chains {
        for pair in chain.windows(2) {
            description.edges.push(Edge {
                source: Source::Node(node(pair[0]), 0),
                destination: Destination::Node(node(pair[1]), 0),
            });
        }
        description.edges.push(Edge {
            source: Source::Node(node(*chain.last().unwrap()), 0),
            destination: Destination::Playback(0),
        });
    }
    description
}

fn phase_lengths(old: &Description, new: &Description, first: bool) -> Vec<Option<usize>> {
    let compiled = compile(old, new, first, 1, CONTEXT);
    compiled
        .graph
        .phases
        .iter()
        .map(|phase| phase.length)
        .collect()
}

#[test]
fn the_first_commit_and_a_commit_without_new_edges_are_one_steady_phase() {
    let empty = Description::default();
    let chain = description(&[0, 1], &[&[0, 1]]);
    assert_eq!(phase_lengths(&empty, &chain, true), [None]);
    let more_nodes = description(&[0, 1, 2], &[&[0, 1]]);
    assert_eq!(phase_lengths(&chain, &more_nodes, false), [None]);
}

#[test]
fn a_small_acyclic_union_crossfades_and_a_large_one_dips() {
    // Four nodes; one more node, one edge moved: the union holds 5, at most 1.25 times 4.
    let old = description(&[0, 1, 2, 3], &[&[0], &[1], &[2], &[3]]);
    let one_more = description(&[0, 1, 2, 3, 4], &[&[0], &[1], &[2], &[4]]);
    assert_eq!(phase_lengths(&old, &one_more, false), [Some(240), None]);
    // Two replaced: the union holds 6 of 4, too many.
    let two_replaced = description(&[0, 1, 4, 5], &[&[0], &[1], &[4], &[5]]);
    assert_eq!(
        phase_lengths(&old, &two_replaced, false),
        [Some(240), Some(240), None]
    );
}

#[test]
fn a_reordered_insert_dips_since_its_union_would_cycle() {
    let old = description(&[0, 1, 2], &[&[0, 1, 2]]);
    let reordered = description(&[0, 1, 2], &[&[0, 2, 1]]);
    let compiled = compile(&old, &reordered, false, 1, CONTEXT);
    let phases = &compiled.graph.phases;
    assert_eq!(phases.len(), 3);
    // The old order, every removed edge fading out; then the new order, every added edge fading in.
    let order = |index: usize| -> Vec<u32> {
        phases[index]
            .steps
            .iter()
            .map(|step| step.node.slot)
            .collect()
    };
    assert_eq!(order(0), [0, 1, 2]);
    assert_eq!(order(1), [0, 2, 1]);
    assert!(
        phases[0]
            .terms
            .iter()
            .all(|term| term.gain == GainShape::FadeOut)
    );
    assert!(
        phases[1]
            .terms
            .iter()
            .all(|term| term.gain == GainShape::FadeIn)
    );
    assert!(
        phases[2]
            .terms
            .iter()
            .all(|term| term.gain == GainShape::Constant)
    );
}

#[test]
fn a_dip_holds_the_events_of_its_new_nodes_through_its_first_phase() {
    let old = description(&[0, 1, 2, 3], &[&[0]]);
    let new = description(&[4, 5, 6, 7], &[&[4]]);
    let compiled = compile(&old, &new, false, 1, CONTEXT);
    let phases = &compiled.graph.phases;
    assert_eq!(phases.len(), 3);
    assert_eq!(
        &*phases[0].waiting_nodes,
        &[node(4), node(5), node(6), node(7)]
    );
    assert!(phases[1].waiting_nodes.is_empty());
    assert_eq!(
        &*compiled.graph.dropped,
        &[node(0), node(1), node(2), node(3)]
    );
    assert_eq!(compiled.graph.dropped_nodes.capacity(), 4);
    assert_eq!(compiled.added, [node(4), node(5), node(6), node(7)]);
}

#[test]
fn every_node_of_a_swap_has_channels_of_its_own() {
    let old = description(&[0, 1], &[&[0, 1]]);
    let new = description(&[1, 2], &[&[2, 1]]);
    let compiled = compile(&old, &new, false, 1, CONTEXT);
    // The playback channel, then one input for each of the three nodes; the same for outputs,
    // without capture.
    assert_eq!(compiled.input_channels, 4);
    assert_eq!(compiled.output_channels, 3);
}

/// A collector's end whose queue the test drains by hand.
fn garbage_out(capacity: usize) -> (GarbageOut, rtrb::Consumer<Garbage>) {
    let (producer, consumer) = RingBuffer::new(capacity);
    let out = GarbageOut::new(
        producer,
        thread::current(),
        Arc::new(AtomicBool::new(false)),
    );
    (out, consumer)
}

#[test]
fn a_full_collector_queue_keeps_the_items_and_holds_back_the_next_swap() {
    let (mut out, mut queue) = garbage_out(RELEASED_PER_SWAP);
    assert!(out.can_take_swap());
    for _ in 0..RELEASED_PER_SWAP + 2 {
        out.release(Garbage::Nodes(Vec::new()));
    }
    assert!(!out.can_take_swap(), "two items wait in the overflow");
    // The collector takes two items: the overflow moves into the queue, which is full again.
    queue.pop().ok().unwrap();
    queue.pop().ok().unwrap();
    out.flush_overflow();
    assert_eq!(queue.slots(), RELEASED_PER_SWAP);
    assert!(!out.can_take_swap(), "the queue is full");
    while queue.pop().is_ok() {}
    assert!(out.can_take_swap());
}

#[test]
fn the_arena_gives_a_node_only_to_its_own_generation() {
    let mut arena = Arena::new();
    let id = NodeId {
        slot: 70,
        generation: 3,
    };
    let mut node: Option<Box<dyn crate::node::Node>> =
        Some(Box::new(crate::nodes::Gain::new(1, 1.0)));
    // Without the slot's chunk, the node stays where it was.
    arena.install_node(id, &mut node);
    assert!(node.is_some());
    let mut chunk = Some(new_chunk());
    arena.install_chunk(1, &mut chunk);
    assert!(chunk.is_none());
    arena.install_node(id, &mut node);
    assert!(node.is_none());
    let older = NodeId {
        generation: 2,
        ..id
    };
    assert!(arena.occupied_mut(older).is_none());
    assert!(arena.take_node(older).is_none());
    assert!(arena.occupied_mut(id).is_some());
    assert!(arena.take_node(id).is_some());
    assert!(arena.occupied_mut(id).is_none());
}

/// The crossfade's gain of each edge of the union of `old` and `new`, by `(from, to)`, where a
/// node is its slot and `None` is the playback channel.
fn crossfade_gains(old: &Description, new: &Description) -> Vec<((u32, Option<u32>), GainShape)> {
    let old_edges: BTreeSet<Edge> = old.edges.iter().copied().collect();
    let new_edges: BTreeSet<Edge> = new.edges.iter().copied().collect();
    let fed_in_new = nodes_fed_by(&new_edges);
    old_edges
        .union(&new_edges)
        .map(|edge| {
            let Source::Node(from, _) = edge.source else {
                panic!("these graphs have no capture");
            };
            let to = match edge.destination {
                Destination::Node(to, _) => Some(to.slot),
                Destination::Playback(_) => None,
            };
            let gain = crossfade_gain(edge, old, new, &old_edges, &new_edges, &fed_in_new);
            ((from.slot, to), gain)
        })
        .collect()
}

#[test]
fn an_inserted_or_removed_node_fades_its_path_once() {
    use GainShape::{Constant, FadeIn, FadeOut};
    let direct = description(&[0], &[&[0]]);
    let through = description(&[0, 1], &[&[0, 1]]);
    // Insert 1 between 0 and the output: the edge into it fades in, its output is constant.
    assert_eq!(
        crossfade_gains(&direct, &through),
        [
            ((0, Some(1)), FadeIn),
            ((0, None), FadeOut),
            ((1, None), Constant)
        ]
    );
    // Remove it: the edge into it is constant, its output fades out.
    assert_eq!(
        crossfade_gains(&through, &direct),
        [
            ((0, Some(1)), Constant),
            ((0, None), FadeIn),
            ((1, None), FadeOut)
        ]
    );
}

#[test]
fn a_new_source_fades_in_and_an_existing_node_put_in_series_fades_twice() {
    use GainShape::{FadeIn, FadeOut};
    let one = description(&[0], &[&[0]]);
    // A new node without an input edge fades its own edges in.
    let with_a_new_source = description(&[0, 1], &[&[0], &[1]]);
    assert_eq!(
        crossfade_gains(&one, &with_a_new_source),
        [((0, None), GainShape::Constant), ((1, None), FadeIn)]
    );
    // An existing node, 1, put between 0 and the output fades in on both of its new edges: that
    // path carries two fades, a limit of the rule.
    let beside = description(&[0, 1], &[&[0]]);
    let in_series = description(&[0, 1], &[&[0, 1]]);
    assert_eq!(
        crossfade_gains(&beside, &in_series),
        [
            ((0, Some(1)), FadeIn),
            ((0, None), FadeOut),
            ((1, None), FadeIn)
        ]
    );
}
