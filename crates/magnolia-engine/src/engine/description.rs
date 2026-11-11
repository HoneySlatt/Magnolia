//! A graph's description, as the controller holds and edits it: nodes with their channel counts,
//! and edges. It is checked whole at each commit, then compiled (`compile.rs`). Off the real-time
//! thread.

use std::collections::{BTreeMap, BTreeSet};

use super::graph::{Destination, Edge, EdgeEnd, GraphError, NodeId, Source};

/// A node's channel counts, fixed for its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeShape {
    pub(crate) inputs: usize,
    pub(crate) outputs: usize,
}

/// Nodes and edges, in any state between two commits: an edge may name a missing node or
/// channel, or be held twice, until [`validate`](Description::validate) says so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Description {
    pub(crate) nodes: BTreeMap<NodeId, NodeShape>,
    pub(crate) edges: Vec<Edge>,
}

impl Description {
    /// Checks every edge's ends against the nodes and the device's channels, then looks for an
    /// edge held twice, then for a cycle.
    pub(crate) fn validate(
        &self,
        capture_channels: usize,
        playback_channels: usize,
    ) -> Result<(), GraphError> {
        for &edge in &self.edges {
            let source_channels = match edge.source {
                Source::Capture(_) => capture_channels,
                Source::Node(node, _) => self.shape(node)?.outputs,
            };
            check_channel(edge, EdgeEnd::Source, source_channels)?;
            let destination_channels = match edge.destination {
                Destination::Node(node, _) => self.shape(node)?.inputs,
                Destination::Playback(_) => playback_channels,
            };
            check_channel(edge, EdgeEnd::Destination, destination_channels)?;
        }
        let mut sorted = self.edges.clone();
        sorted.sort_unstable();
        if let Some(pair) = sorted.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(GraphError::DuplicateEdge(pair[0]));
        }
        topological_order(self.nodes.keys().copied(), &self.edges)?;
        Ok(())
    }

    fn shape(&self, node: NodeId) -> Result<NodeShape, GraphError> {
        self.nodes
            .get(&node)
            .copied()
            .ok_or(GraphError::UnknownNode(node))
    }
}

/// Checks that the channel `end` of `edge` names is one of the `channels` it has.
fn check_channel(edge: Edge, end: EdgeEnd, channels: usize) -> Result<(), GraphError> {
    let channel = match end {
        EdgeEnd::Source => match edge.source {
            Source::Capture(channel) | Source::Node(_, channel) => channel,
        },
        EdgeEnd::Destination => match edge.destination {
            Destination::Node(_, channel) | Destination::Playback(channel) => channel,
        },
    };
    if channel < channels {
        Ok(())
    } else {
        Err(GraphError::ChannelOutOfRange {
            edge,
            end,
            channels,
        })
    }
}

/// The nodes in an order where each comes after every node that feeds it (Kahn's algorithm);
/// among nodes free to go next, the smallest id first, so that the order never depends on how the
/// description was edited. Edges from capture or to playback do not constrain it. A cycle is an
/// error that names a node on it.
pub(crate) fn topological_order(
    nodes: impl IntoIterator<Item = NodeId>,
    edges: &[Edge],
) -> Result<Vec<NodeId>, GraphError> {
    let mut feeders: BTreeMap<NodeId, usize> = nodes.into_iter().map(|node| (node, 0)).collect();
    let mut fed: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in edges {
        if let (Source::Node(from, _), Destination::Node(to, _)) = (edge.source, edge.destination) {
            fed.entry(from).or_default().push(to);
            *feeders.entry(to).or_default() += 1;
        }
    }
    let mut ready: BTreeSet<NodeId> = feeders
        .iter()
        .filter(|&(_, &count)| count == 0)
        .map(|(&node, _)| node)
        .collect();
    let mut order = Vec::with_capacity(feeders.len());
    while let Some(node) = ready.pop_first() {
        order.push(node);
        for next in fed.get(&node).map(Vec::as_slice).unwrap_or_default() {
            let count = feeders.get_mut(next).expect("every fed node was counted");
            *count -= 1;
            if *count == 0 {
                ready.insert(*next);
            }
        }
    }
    if order.len() == feeders.len() {
        return Ok(order);
    }
    Err(GraphError::Cycle(node_on_a_cycle(&feeders, edges)))
}

/// A node on a cycle, among the nodes Kahn's algorithm could not order: each of those still has a
/// feeder among them, so walking from feeder to feeder must come back to a node already seen.
fn node_on_a_cycle(feeders: &BTreeMap<NodeId, usize>, edges: &[Edge]) -> NodeId {
    let left = |node: &NodeId| feeders.get(node).is_some_and(|&count| count > 0);
    let feeder_of = |node: NodeId| {
        edges
            .iter()
            .find_map(|edge| match (edge.source, edge.destination) {
                (Source::Node(from, _), Destination::Node(to, _)) if to == node && left(&from) => {
                    Some(from)
                }
                _ => None,
            })
    };
    let mut node = *feeders
        .keys()
        .find(|node| left(node))
        .expect("a node is left when the order is short");
    let mut seen = BTreeSet::new();
    while seen.insert(node) {
        node = feeder_of(node).expect("a node left has a feeder left");
    }
    node
}
