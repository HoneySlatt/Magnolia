//! Delay compensation: how far ahead of the output each node runs, and the delay lines that align
//! paths of different latencies where they meet
//! ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)).
//! Off the real-time thread.
//!
//! Every commit compensates the committed description, from the playback channels back:
//!
//! - a playback channel requires its signal 0 samples ahead;
//! - a node is **audible** when it reaches a playback channel, directly or through other nodes;
//!   a node that is not demands nothing: the nodes that feed it ignore it, and it counts in no
//!   compensation but its own;
//! - a node is **live** when a capture channel feeds it, directly or through other nodes;
//! - a node's output runs as far ahead as the most demanding audible node it feeds requires (a
//!   playback channel requiring 0), or 0 when it feeds none; a live node's, as the least
//!   demanding, so that nothing live is ever delayed;
//! - its input runs as far ahead as its output, plus its own latency.
//!
//! An edge from a node that is not live into an audible node, or a playback channel, that requires
//! less than that node gives needs a delay of the difference. Edges out of a live node or a capture
//! channel, and edges into a node that is not audible, need none. One delay
//! line serves each destination and delay: every edge that needs that delay there goes into the
//! delay line's channel of the same number, whose output feeds the destination's. The delay lines
//! are nodes of the arena, which the controller keeps across commits while their destination and
//! delay stay; the user's description never holds them.

use std::collections::{BTreeMap, BTreeSet};

use super::description::{Description, NodeShape, topological_order};
use super::graph::{Destination, Edge, NodeId, Source};

/// Where a delay line leads: a node's inputs, or the playback channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DelayDestination {
    Node(NodeId),
    Playback,
}

/// What makes a delay line: where it leads, and how many samples it delays. While a commit keeps
/// both, it keeps the delay line's node, and so its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DelayKey {
    pub(crate) destination: DelayDestination,
    pub(crate) delay: u64,
}

/// A graph as the processor plays it: the user's description with a delay line on each edge that
/// needs one, and how far ahead of the output each node's input runs.
#[derive(Clone, Debug, Default)]
pub(crate) struct Compensated {
    /// The user's nodes and the delay lines; the user's edges, those that need a delay going
    /// through their delay line.
    pub(crate) description: Description,
    /// In samples, for every node of `description`.
    pub(crate) ahead_in: BTreeMap<NodeId, u64>,
    /// The largest `ahead_in` of the audible nodes, live ones counted only when they have timeline
    /// sources: the session's largest compensation, by which the transport pre-rolls; 0 for a
    /// graph without them. A delay line runs no further ahead
    /// than the node that feeds it.
    pub(crate) largest_ahead_in: u64,
    /// The delay lines' nodes.
    pub(crate) delay_lines: BTreeMap<DelayKey, NodeId>,
}

impl Compensated {
    /// Whether `node` is one of the delay lines.
    pub(crate) fn is_delay_line(&self, node: NodeId) -> bool {
        self.delay_lines.values().any(|&line| line == node)
    }
}

/// How a description's latencies are compensated, before its delay lines have nodes.
pub(crate) struct Plan {
    /// How far ahead of the output each node's input runs, in samples.
    ahead_in: BTreeMap<NodeId, u64>,
    /// The edges that need a delay, each with the delay line it goes through.
    delayed: BTreeMap<Edge, DelayKey>,
    /// The delay lines the plan needs, each with its channels: as many as its destination's
    /// inputs.
    delay_lines: BTreeMap<DelayKey, usize>,
    /// The largest `ahead_in` of the audible nodes, live ones only with timeline sources.
    largest_ahead_in: u64,
}

impl Plan {
    /// The delay lines the plan needs, each with its channels.
    pub(crate) fn delay_lines(&self) -> &BTreeMap<DelayKey, usize> {
        &self.delay_lines
    }

    /// The graph the processor plays: `description`, the one planned, with each delayed edge going
    /// through its delay line, whose node `nodes` gives.
    pub(crate) fn compensate(
        self,
        description: &Description,
        nodes: &BTreeMap<DelayKey, NodeId>,
    ) -> Compensated {
        let Plan {
            mut ahead_in,
            delayed,
            delay_lines,
            largest_ahead_in,
        } = self;
        let mut graph_nodes = description.nodes.clone();
        let mut lines = BTreeMap::new();
        for (&key, &channels) in &delay_lines {
            let line = *nodes
                .get(&key)
                .expect("the controller gives every delay line of the plan a node");
            graph_nodes.insert(
                line,
                NodeShape {
                    inputs: channels,
                    outputs: channels,
                    latency: 0,
                },
            );
            // A delay line runs as far ahead as the edges into it: its destination, plus its delay.
            let required = match key.destination {
                DelayDestination::Node(node) => ahead_in.get(&node).copied().unwrap_or(0),
                DelayDestination::Playback => 0,
            };
            ahead_in.insert(line, required.saturating_add(key.delay));
            lines.insert(key, line);
        }
        let mut edges = BTreeSet::new();
        for edge in &description.edges {
            let Some(&line) = delayed.get(edge).and_then(|key| lines.get(key)) else {
                edges.insert(*edge);
                continue;
            };
            let channel = match edge.destination {
                Destination::Node(_, channel) | Destination::Playback(channel) => channel,
            };
            edges.insert(Edge {
                source: edge.source,
                destination: Destination::Node(line, channel),
            });
            edges.insert(Edge {
                source: Source::Node(line, channel),
                destination: edge.destination,
            });
        }
        Compensated {
            description: Description {
                nodes: graph_nodes,
                edges: edges.into_iter().collect(),
            },
            ahead_in,
            largest_ahead_in,
            delay_lines: lines,
        }
    }
}

/// Plans the compensation of `description`, valid, for an engine of `playback_channels`: how far
/// ahead each node runs, which edges need a delay, and the delay lines they go through.
/// `reads_timeline` says which nodes have timeline sources: a live node counts in the pre-roll only
/// if it has some, since a live path has nothing else to read ahead (ADR-0032).
pub(crate) fn plan(
    description: &Description,
    playback_channels: usize,
    reads_timeline: impl Fn(NodeId) -> bool,
) -> Plan {
    let order = topological_order(description.nodes.keys().copied(), &description.edges)
        .expect("a committed description is acyclic");
    let mut fed_by: BTreeMap<NodeId, Vec<Destination>> = BTreeMap::new();
    let mut live = BTreeSet::new();
    for edge in &description.edges {
        match (edge.source, edge.destination) {
            (Source::Node(node, _), destination) => {
                fed_by.entry(node).or_default().push(destination)
            }
            (Source::Capture(_), Destination::Node(node, _)) => {
                live.insert(node);
            }
            (Source::Capture(_), Destination::Playback(_)) => {}
        }
    }
    let feeds = |node: &NodeId| fed_by.get(node).map(Vec::as_slice).unwrap_or_default();
    // Every node comes after the nodes that feed it, so liveness flows forwards in one pass.
    for node in &order {
        if live.contains(node) {
            for destination in feeds(node) {
                if let Destination::Node(fed, _) = destination {
                    live.insert(*fed);
                }
            }
        }
    }
    // Audibility and requirements flow backwards: a node's destinations come later in the order.
    let mut audible = BTreeSet::new();
    let mut ahead_in: BTreeMap<NodeId, u64> = BTreeMap::new();
    let mut ahead_out: BTreeMap<NodeId, u64> = BTreeMap::new();
    // What an audible destination requires; `None` for a node that is not audible.
    let required = |audible: &BTreeSet<NodeId>,
                    ahead_in: &BTreeMap<NodeId, u64>,
                    destination: &Destination| match destination {
        Destination::Node(node, _) if audible.contains(node) => {
            Some(ahead_in.get(node).copied().unwrap_or(0))
        }
        Destination::Node(..) => None,
        Destination::Playback(_) => Some(0),
    };
    for node in order.iter().rev() {
        let requirements: Vec<u64> = feeds(node)
            .iter()
            .filter_map(|destination| required(&audible, &ahead_in, destination))
            .collect();
        if !requirements.is_empty() {
            audible.insert(*node);
        }
        let out = if live.contains(node) {
            requirements.iter().min()
        } else {
            requirements.iter().max()
        }
        .copied()
        .unwrap_or(0);
        let latency = description
            .nodes
            .get(node)
            .map_or(0, |shape| u64::from(shape.latency));
        ahead_out.insert(*node, out);
        ahead_in.insert(*node, out.saturating_add(latency));
    }
    let mut delayed = BTreeMap::new();
    let mut delay_lines = BTreeMap::new();
    for edge in &description.edges {
        let Source::Node(node, _) = edge.source else {
            continue;
        };
        if live.contains(&node) {
            continue;
        }
        let Some(required) = required(&audible, &ahead_in, &edge.destination) else {
            continue;
        };
        let out = ahead_out.get(&node).copied().unwrap_or(0);
        let delay = out.saturating_sub(required);
        if delay == 0 {
            continue;
        }
        let (destination, channels) = match edge.destination {
            Destination::Node(fed, _) => (
                DelayDestination::Node(fed),
                description.nodes.get(&fed).map_or(0, |shape| shape.inputs),
            ),
            Destination::Playback(_) => (DelayDestination::Playback, playback_channels),
        };
        let key = DelayKey { destination, delay };
        delayed.insert(*edge, key);
        delay_lines.insert(key, channels);
    }
    let largest_ahead_in = audible
        .iter()
        .filter(|node| !live.contains(node) || reads_timeline(**node))
        .filter_map(|node| ahead_in.get(node))
        .copied()
        .max()
        .unwrap_or(0);
    Plan {
        ahead_in,
        delayed,
        delay_lines,
        largest_ahead_in,
    }
}
