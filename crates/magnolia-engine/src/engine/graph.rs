//! The words of a graph's description: nodes by id, edges between channels, and what is wrong
//! with a description that cannot be compiled.

// The processor compares and sorts node ids in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::error::Error;
use std::fmt;

/// A node's identity: the slot it occupies in the processor's arena, and the generation of that
/// slot. A slot is reused for a new node only once the processor has let the old one go, with the
/// next generation, so an old id never reaches a new node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId {
    /// The node's slot in the arena.
    pub slot: u32,
    /// How many nodes occupied the slot before this one.
    pub generation: u32,
}

/// Where an edge takes its signal from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// A capture channel of the audio device, by number.
    Capture(usize),
    /// A node's output channel: the node, and the channel's number.
    Node(NodeId, usize),
}

/// Where an edge brings its signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Destination {
    /// A node's input channel: the node, and the channel's number.
    Node(NodeId, usize),
    /// A playback channel of the audio device, by number.
    Playback(usize),
}

/// A connection from one channel to another. Every edge into a channel is summed there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Edge {
    /// Where the signal comes from.
    pub source: Source,
    /// Where it goes.
    pub destination: Destination,
}

/// Which end of an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeEnd {
    /// The end the signal comes from.
    Source,
    /// The end the signal goes to.
    Destination,
}

/// Why the controller refused an edit or a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// No node of the description has this id: it was never added, or was removed.
    UnknownNode(NodeId),
    /// An edge names a channel its end does not have.
    ChannelOutOfRange {
        /// The edge.
        edge: Edge,
        /// Which end names the channel.
        end: EdgeEnd,
        /// How many channels that end has.
        channels: usize,
    },
    /// The description holds this edge twice.
    DuplicateEdge(Edge),
    /// The edges form a cycle, which passes through this node.
    Cycle(NodeId),
    /// The description holds no such edge to disconnect.
    MissingEdge(Edge),
    /// The arena has no slot left for another node.
    TooManyNodes,
}

impl fmt::Display for GraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(node) => write!(formatter, "no node {node:?} in the description"),
            Self::ChannelOutOfRange {
                edge,
                end,
                channels,
            } => write!(
                formatter,
                "the {end:?} of {edge:?} names a channel out of range: it has {channels}"
            ),
            Self::DuplicateEdge(edge) => write!(formatter, "the description holds {edge:?} twice"),
            Self::Cycle(node) => write!(formatter, "the edges form a cycle through {node:?}"),
            Self::MissingEdge(edge) => write!(formatter, "the description holds no {edge:?}"),
            Self::TooManyNodes => write!(formatter, "the arena has no slot left for a node"),
        }
    }
}

impl Error for GraphError {}
