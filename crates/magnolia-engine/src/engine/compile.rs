//! Compiling a swap: from the graph the processor plays to the one committed, the phases that
//! carry the sound across without a click. Off the real-time thread.
//!
//! Both graphs are compensated (`compensation.rs`): their delay lines are nodes like any other, so
//! a change of delays crossfades or dips like any other change.
//!
//! With `R` the edges removed and `A` the edges added:
//!
//! - a commit that adds and removes no edge is one steady phase. The first commit is no exception:
//!   its sources fade in from the silence before it, as any new source does;
//! - otherwise, if the union of the two graphs is acyclic and holds at most 1.25 times the nodes
//!   of the larger, a **crossfade**: one phase that runs the union, one fade long, then the steady
//!   new graph. Each path that changes carries one fade, not one per edge, so that inserting or
//!   removing a node keeps the level (see [`crossfade_gain`]);
//! - otherwise a **dip**: the old graph with `R` fading out, one fade long, then the new graph with
//!   `A` fading in, one fade long, then the steady new graph. The processor cuts its sub-blocks at
//!   each phase's last sample, so nothing separates the two fades at any period.
//!
//! The union's limit keeps a large edit, such as a new template, from running both graphs at once
//! for a fade, which would double the work of those blocks.

use std::collections::{BTreeMap, BTreeSet};

use super::compensation::Compensated;
use super::compiled::{Channels, CompiledGraph, GainShape, Phase, Span, Step, Sum, Term};
use super::description::{Description, NodeShape, topological_order};
use super::graph::{Destination, Edge, NodeId, Source};
use super::memory::written_vec;

/// The engine's channels and fade, which every compiled graph depends on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompileContext {
    pub(crate) capture_channels: usize,
    pub(crate) playback_channels: usize,
    pub(crate) fade_frames: usize,
}

/// A compiled swap and what the controller must send with it.
pub(crate) struct Compiled {
    pub(crate) graph: CompiledGraph,
    /// The channels the graph needs in each region of the buffer pool.
    pub(crate) input_channels: usize,
    pub(crate) output_channels: usize,
    /// The nodes the new graph runs and the old one did not.
    pub(crate) added: Vec<NodeId>,
}

/// Compiles the swap from `old`, the graph the processor plays, to `new`, both valid.
pub(crate) fn compile(
    old_graph: &Compensated,
    new_graph: &Compensated,
    sequence: u64,
    context: CompileContext,
) -> Compiled {
    let (old, new) = (&old_graph.description, &new_graph.description);
    // A phase records each node's compensation in the graph it plays, the other for the nodes
    // only that one holds.
    let old_then_new = [&old_graph.ahead_in, &new_graph.ahead_in];
    let new_then_old = [&new_graph.ahead_in, &old_graph.ahead_in];
    let old_edges: BTreeSet<Edge> = old.edges.iter().copied().collect();
    let new_edges: BTreeSet<Edge> = new.edges.iter().copied().collect();
    let edges_change = old_edges != new_edges;

    let mut union_nodes = old.nodes.clone();
    union_nodes.extend(new.nodes.iter().map(|(&node, &shape)| (node, shape)));
    let added: Vec<NodeId> = new
        .nodes
        .keys()
        .filter(|node| !old.nodes.contains_key(node))
        .copied()
        .collect();
    let dropped: Box<[NodeId]> = old
        .nodes
        .keys()
        .filter(|node| !new.nodes.contains_key(node))
        .copied()
        .collect();

    let steady = PhasePlan {
        nodes: &new.nodes,
        edges: shaped(&new_edges, |_| GainShape::Constant),
        length: None,
        waiting_nodes: Vec::new(),
        ahead_in: new_then_old,
    };
    let (laid_out, phases) = if !edges_change {
        (&new.nodes, vec![steady])
    } else {
        let fade = Some(context.fade_frames);
        let union_edges: BTreeSet<Edge> = old_edges.union(&new_edges).copied().collect();
        let union_edge_list: Vec<Edge> = union_edges.iter().copied().collect();
        let union_is_acyclic =
            topological_order(union_nodes.keys().copied(), &union_edge_list).is_ok();
        let larger = old.nodes.len().max(new.nodes.len());
        let union_is_small = 4 * union_nodes.len() <= 5 * larger;
        if union_is_acyclic && union_is_small {
            let fed_in_new = nodes_fed_by(&new_edges);
            let crossfade = shaped(&union_edges, |edge| {
                crossfade_gain(edge, old, new, &old_edges, &new_edges, &fed_in_new)
            });
            let crossfade = PhasePlan {
                nodes: &union_nodes,
                edges: crossfade,
                length: fade,
                waiting_nodes: Vec::new(),
                ahead_in: new_then_old,
            };
            (&union_nodes, vec![crossfade, steady])
        } else {
            let fading_out = shaped(&old_edges, |edge| {
                if new_edges.contains(edge) {
                    GainShape::Constant
                } else {
                    GainShape::FadeOut
                }
            });
            let fading_in = shaped(&new_edges, |edge| {
                if old_edges.contains(edge) {
                    GainShape::Constant
                } else {
                    GainShape::FadeIn
                }
            });
            // The new nodes run from the second phase on: their events wait through the first.
            let fading_out = PhasePlan {
                nodes: &old.nodes,
                edges: fading_out,
                length: fade,
                waiting_nodes: added.clone(),
                ahead_in: old_then_new,
            };
            let fading_in = PhasePlan {
                nodes: &new.nodes,
                edges: fading_in,
                length: fade,
                waiting_nodes: Vec::new(),
                ahead_in: new_then_old,
            };
            (&union_nodes, vec![fading_out, fading_in, steady])
        }
    };

    let layout = Layout::new(laid_out, context);
    let phases = phases
        .into_iter()
        .map(|plan| build_phase(plan, &layout, context))
        .collect();
    Compiled {
        graph: CompiledGraph {
            sequence,
            phases,
            dropped_nodes: written_vec(dropped.len()),
            dropped,
            largest_ahead_in: new_graph.largest_ahead_in,
        },
        input_channels: layout.input_channels,
        output_channels: layout.output_channels,
        added,
    }
}

/// An edge's gain in a crossfade, so that each path that changes carries one fade:
///
/// - an edge both graphs hold is constant;
/// - a removed edge fades out, unless it goes into a node the swap drops: that node's own
///   outgoing edges fade out, and fading its input too would fade the path twice;
/// - an added edge fades in, unless it comes from a node the swap adds that has an input edge:
///   that node's input edges fade in already. A node the swap adds without an input edge, a new
///   source, fades its own edges in, so that its sound never steps in.
///
/// Inserting a node X between A and the output then gives `A (1 - f) + X(A f)`, the level of A
/// throughout for a unity X; removing it gives the mirror. A path through a node both graphs hold
/// can still carry two fades: an existing node newly put in series fades in on both of its new
/// edges, one taken out of series fades out on both of its old ones, and a removed edge followed
/// by an added edge through a node both graphs hold makes a path neither graph has, faded out and
/// in at once.
///
/// `fed_in_new` holds the nodes that some edge of the new graph goes into.
pub(super) fn crossfade_gain(
    edge: &Edge,
    old: &Description,
    new: &Description,
    old_edges: &BTreeSet<Edge>,
    new_edges: &BTreeSet<Edge>,
    fed_in_new: &BTreeSet<NodeId>,
) -> GainShape {
    match (old_edges.contains(edge), new_edges.contains(edge)) {
        (true, true) => GainShape::Constant,
        (true, false) => match edge.destination {
            Destination::Node(node, _) if !new.nodes.contains_key(&node) => GainShape::Constant,
            _ => GainShape::FadeOut,
        },
        _ => match edge.source {
            Source::Node(node, _)
                if !old.nodes.contains_key(&node) && fed_in_new.contains(&node) =>
            {
                GainShape::Constant
            }
            _ => GainShape::FadeIn,
        },
    }
}

/// The nodes that some edge of `edges` goes into.
pub(super) fn nodes_fed_by(edges: &BTreeSet<Edge>) -> BTreeSet<NodeId> {
    edges
        .iter()
        .filter_map(|edge| match edge.destination {
            Destination::Node(node, _) => Some(node),
            Destination::Playback(_) => None,
        })
        .collect()
}

/// What one phase runs, before its channels are laid out.
struct PhasePlan<'a> {
    nodes: &'a BTreeMap<NodeId, NodeShape>,
    /// Sorted, each with the shape of its gain.
    edges: Vec<(Edge, GainShape)>,
    /// How many samples the phase lasts; `None` for the steady phase.
    length: Option<usize>,
    /// The nodes a later phase runs that this one does not, sorted.
    waiting_nodes: Vec<NodeId>,
    /// Where to find each node's compensation: the first map that holds the node.
    ahead_in: [&'a BTreeMap<NodeId, u64>; 2],
}

/// Each edge of a set with its gain's shape.
fn shaped(edges: &BTreeSet<Edge>, shape: impl Fn(&Edge) -> GainShape) -> Vec<(Edge, GainShape)> {
    edges.iter().map(|edge| (*edge, shape(edge))).collect()
}

/// Where each node's channels lie in the buffer pool, for every phase of one compiled graph. The
/// playback channels open the input region and the capture channels the output region; the nodes
/// follow, in the order of their ids.
struct Layout {
    nodes: BTreeMap<NodeId, (Channels, Channels)>,
    input_channels: usize,
    output_channels: usize,
}

impl Layout {
    fn new(nodes: &BTreeMap<NodeId, NodeShape>, context: CompileContext) -> Self {
        let mut input_channels = context.playback_channels;
        let mut output_channels = context.capture_channels;
        let nodes = nodes
            .iter()
            .map(|(&node, shape)| {
                let inputs = Channels {
                    first: input_channels,
                    count: shape.inputs,
                };
                let outputs = Channels {
                    first: output_channels,
                    count: shape.outputs,
                };
                input_channels += shape.inputs;
                output_channels += shape.outputs;
                (node, (inputs, outputs))
            })
            .collect();
        Self {
            nodes,
            input_channels,
            output_channels,
        }
    }

    fn channels_of(&self, node: NodeId) -> (Channels, Channels) {
        *self
            .nodes
            .get(&node)
            .expect("every node of a phase is laid out")
    }

    /// The output-region channel an edge reads.
    fn source_channel(&self, source: Source) -> usize {
        match source {
            Source::Capture(channel) => channel,
            Source::Node(node, channel) => self.channels_of(node).1.first + channel,
        }
    }
}

/// One phase: its nodes in topological order, each channel summing its edges.
fn build_phase(plan: PhasePlan<'_>, layout: &Layout, context: CompileContext) -> Phase {
    let plain_edges: Vec<Edge> = plan.edges.iter().map(|&(edge, _)| edge).collect();
    let order = topological_order(plan.nodes.keys().copied(), &plain_edges)
        .expect("a phase's graph was checked to be acyclic");
    // The edges arrive sorted, so each channel sums its terms in the edges' order, the same in
    // every phase and at every commit.
    let mut terms_into: BTreeMap<Destination, Vec<Term>> = BTreeMap::new();
    for &(edge, gain) in &plan.edges {
        terms_into.entry(edge.destination).or_default().push(Term {
            source: layout.source_channel(edge.source),
            gain,
        });
    }

    let mut sums = Vec::new();
    let mut terms = Vec::new();
    let mut add_sum = |sums: &mut Vec<Sum>, target: usize, destination: Destination| {
        let start = terms.len();
        terms.extend(terms_into.get(&destination).into_iter().flatten().copied());
        sums.push(Sum {
            target,
            terms: Span {
                start,
                end: terms.len(),
            },
        });
    };
    let mut steps = Vec::with_capacity(order.len());
    for node in order {
        let (inputs, outputs) = layout.channels_of(node);
        let start = sums.len();
        for channel in 0..inputs.count {
            add_sum(
                &mut sums,
                inputs.first + channel,
                Destination::Node(node, channel),
            );
        }
        steps.push(Step {
            node,
            inputs,
            outputs,
            sums: Span {
                start,
                end: sums.len(),
            },
            ahead_in: plan
                .ahead_in
                .iter()
                .find_map(|ahead_in| ahead_in.get(&node))
                .copied()
                .unwrap_or(0),
        });
    }
    let start = sums.len();
    for channel in 0..context.playback_channels {
        add_sum(&mut sums, channel, Destination::Playback(channel));
    }
    let playback_sums = Span {
        start,
        end: sums.len(),
    };
    Phase {
        length: plan.length,
        steps: steps.into(),
        sums: sums.into(),
        terms: terms.into(),
        playback_sums,
        waiting_nodes: plan.waiting_nodes.into(),
    }
}
