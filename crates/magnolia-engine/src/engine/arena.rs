//! The arena: where the processor keeps its nodes, apart from any compiled graph, so that they
//! survive a swap.
//!
//! It is a table of fixed-size chunks of slots. The table is allocated whole at the start, so
//! adding a chunk only fills one of its entries and never reallocates on the real-time thread, and
//! a node never moves. The controller decides which slot holds which node; the processor installs
//! nodes and takes them out at the swaps the controller sends.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::hint::black_box;

use super::graph::NodeId;
use super::sources::SourceState;
use crate::node::Node;

/// Slots in one chunk of the arena.
pub(crate) const CHUNK_SLOTS: usize = 64;
/// Chunks the arena's table holds: 262,144 nodes at most.
pub(crate) const MAX_CHUNKS: usize = 4_096;

/// A place for one node.
pub(crate) struct Slot {
    /// The generation of the node the slot holds, or held last.
    generation: u32,
    node: Option<Box<dyn Node>>,
    /// Whether the node returned `Sleep` at its last call.
    sleeping: bool,
    /// What the node's timeline sources hold: the notes they started and not ended, the last play
    /// they stopped. It lives here, beside the node, so that it survives a swap.
    sources: SourceState,
}

/// A node in its slot, as the processor runs it.
pub(crate) struct Occupied<'a> {
    pub(crate) node: &'a mut (dyn Node + 'static),
    /// Whether the node returned `Sleep` at its last call.
    pub(crate) sleeping: &'a mut bool,
    /// What its timeline sources hold.
    pub(crate) sources: &'a mut SourceState,
}

/// A chunk of slots, sent by the controller when the arena needs more.
pub(crate) type ArenaChunk = Box<[Slot]>;

/// A chunk of empty slots, every page of it written (see `memory.rs`). Built off the real-time
/// thread.
pub(crate) fn new_chunk() -> ArenaChunk {
    let chunk: ArenaChunk = (0..CHUNK_SLOTS)
        .map(|_| Slot {
            generation: black_box(0),
            node: None,
            sleeping: false,
            sources: SourceState::default(),
        })
        .collect();
    black_box(&chunk);
    chunk
}

/// The processor's nodes, by slot.
pub(crate) struct Arena {
    chunks: Box<[Option<ArenaChunk>]>,
}

impl Arena {
    /// An arena without chunks, its whole table written. Built off the real-time thread.
    pub(crate) fn new() -> Self {
        let chunks: Box<[Option<ArenaChunk>]> = (0..MAX_CHUNKS).map(|_| black_box(None)).collect();
        black_box(&chunks);
        Self { chunks }
    }

    /// Puts a chunk at its place in the table. What was there, normally nothing, goes back into
    /// `chunk`, for the collector.
    pub(crate) fn install_chunk(&mut self, index: usize, chunk: &mut Option<ArenaChunk>) {
        if let Some(entry) = self.chunks.get_mut(index) {
            std::mem::swap(entry, chunk);
        }
    }

    /// Puts a node in its slot, under its generation, awake, holding no note. What the slot held,
    /// normally nothing, goes back into `node`, for the collector. Without the slot's chunk, the
    /// node stays in `node`.
    pub(crate) fn install_node(&mut self, id: NodeId, node: &mut Option<Box<dyn Node>>) {
        if let Some(slot) = self.slot_mut(id.slot) {
            slot.generation = id.generation;
            slot.sleeping = false;
            slot.sources = SourceState::default();
            std::mem::swap(&mut slot.node, node);
        }
    }

    /// A node, its sleep flag and its sources' state, if its slot holds it: the same generation, and
    /// a node.
    pub(crate) fn occupied_mut(&mut self, id: NodeId) -> Option<Occupied<'_>> {
        let Slot {
            generation,
            node,
            sleeping,
            sources,
        } = self.slot_mut(id.slot)?;
        if *generation != id.generation {
            return None;
        }
        Some(Occupied {
            node: node.as_deref_mut()?,
            sleeping,
            sources,
        })
    }

    /// Takes a node out of its slot, if the slot holds that node.
    pub(crate) fn take_node(&mut self, id: NodeId) -> Option<Box<dyn Node>> {
        self.slot_mut(id.slot)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.node.take())
    }

    fn slot_mut(&mut self, slot: u32) -> Option<&mut Slot> {
        let slot = slot as usize;
        let chunk = slot.checked_div(CHUNK_SLOTS)?;
        let index = slot.checked_rem(CHUNK_SLOTS)?;
        self.chunks.get_mut(chunk)?.as_mut()?.get_mut(index)
    }
}
