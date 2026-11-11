//! A compiled graph, as the processor plays it, and the message that carries it across.
//!
//! The controller builds both off the real-time thread (`compile.rs`); the processor only reads
//! them. A compiled graph holds indices into the processor's buffer pool and arena, never
//! pointers, so it can be built before the memory it names is installed.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use super::arena::ArenaChunk;
use super::graph::NodeId;
use super::pool::BufferPool;
use crate::node::Node;

/// A swap's phases, in order, the last one steady, and the nodes it lets go.
pub(crate) struct CompiledGraph {
    /// The swap's sequence number, which the processor publishes when it applies the graph and
    /// when the graph reaches its steady phase.
    pub(crate) sequence: u64,
    /// At least one phase. Every phase but the last lasts a fade; the last lasts until the next
    /// swap.
    pub(crate) phases: Box<[Phase]>,
    /// The nodes the previous graph ran and this one does not: the processor moves them out of
    /// its arena when this graph reaches its steady phase.
    pub(crate) dropped: Box<[NodeId]>,
    /// Where the processor moves the dropped nodes, written and reserved for all of them, so that
    /// moving them neither allocates nor faults. It goes to the collector, full.
    pub(crate) dropped_nodes: Vec<Box<dyn Node>>,
}

impl CompiledGraph {
    pub(crate) fn phase(&self, index: usize) -> Option<&Phase> {
        self.phases.get(index)
    }

    /// Whether the phase at `index` is the steady one, the last.
    pub(crate) fn is_steady(&self, index: usize) -> bool {
        index.checked_add(1) == Some(self.phases.len())
    }
}

/// One stage of a swap: which nodes run, in which order, and what each channel sums.
#[derive(Debug)]
pub(crate) struct Phase {
    /// How many samples the phase lasts; `None` for the steady phase.
    pub(crate) length: Option<usize>,
    /// The nodes in topological order: each runs after every node that feeds it.
    pub(crate) steps: Box<[Step]>,
    /// The sums into each step's input channels, step by step, then into each playback channel.
    pub(crate) sums: Box<[Sum]>,
    /// The edges that each sum adds, sum by sum.
    pub(crate) terms: Box<[Term]>,
    /// The sums into the playback channels.
    pub(crate) playback_sums: Span,
    /// The nodes a later phase runs that this one does not, sorted: their events wait for that
    /// phase instead of being dropped.
    pub(crate) waiting_nodes: Box<[NodeId]>,
}

impl Phase {
    /// The sums a span names.
    pub(crate) fn sums(&self, span: Span) -> &[Sum] {
        span.of(&self.sums)
    }

    /// The edges a sum adds.
    pub(crate) fn terms(&self, sum: &Sum) -> &[Term] {
        sum.terms.of(&self.terms)
    }

    /// Whether a later phase runs `node` while this one does not.
    pub(crate) fn node_waits(&self, node: NodeId) -> bool {
        self.waiting_nodes.binary_search(&node).is_ok()
    }
}

/// One node's turn in a phase.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    pub(crate) node: NodeId,
    /// The node's input channels, in the pool's input region.
    pub(crate) inputs: Channels,
    /// The node's output channels, in the pool's output region.
    pub(crate) outputs: Channels,
    /// The sums into the node's input channels, one per channel, in order.
    pub(crate) sums: Span,
}

/// Consecutive channels of one region of the buffer pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Channels {
    pub(crate) first: usize,
    pub(crate) count: usize,
}

/// What one channel of the input region sums.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sum {
    /// The channel summed into, in the input region: a playback channel or a node's input.
    pub(crate) target: usize,
    /// The edges summed, in the phase's terms.
    pub(crate) terms: Span,
}

/// One edge of a sum.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Term {
    /// The channel the edge reads, in the output region: a capture channel or a node's output.
    pub(crate) source: usize,
    pub(crate) gain: GainShape,
}

/// How an edge's gain moves over its phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GainShape {
    /// 1 throughout.
    Constant,
    /// From 0 to 1 over the phase: an edge the swap adds.
    FadeIn,
    /// From 1 to 0 over the phase: an edge the swap removes.
    FadeOut,
}

/// A run of items in one of a phase's lists, from `start` up to `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Span {
    /// The items of `list` this span names; none if it names items out of range.
    pub(crate) fn of<T>(self, list: &[T]) -> &[T] {
        list.get(self.start..self.end).unwrap_or(&[])
    }
}

/// Everything one swap brings to the processor, applied whole at the start of a period.
///
/// What the processor takes out of it, it replaces with `None`; the message then goes to the
/// collector, holding whatever it displaced.
pub(crate) struct SwapMessage {
    pub(crate) graph: Option<Box<CompiledGraph>>,
    /// The new nodes, each with its id.
    pub(crate) nodes: Vec<(NodeId, Option<Box<dyn Node>>)>,
    /// New chunks of the arena, each with its place in the arena's table.
    pub(crate) chunks: Vec<(usize, Option<ArenaChunk>)>,
    /// A larger buffer pool, when the graph needs more channels than the processor's holds.
    pub(crate) pool: Option<BufferPool>,
}
