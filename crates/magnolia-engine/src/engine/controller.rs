//! The controller: the engine's non-real-time half, which edits the graph, compiles each swap and
//! sends it, and sends events. It may allocate and block; the processor never waits for it.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rtrb::{Producer, PushError};

use super::arena::{ArenaChunk, CHUNK_SLOTS, MAX_CHUNKS, new_chunk};
use super::compile::{CompileContext, compile};
use super::compiled::SwapMessage;
use super::description::{Description, NodeShape};
use super::events::TimedEvent;
use super::graph::{Destination, Edge, GraphError, NodeId, Source};
use super::pool::BufferPool;
use crate::node::Node;
use crate::stats::EngineStats;

/// The engine's non-real-time half, made with its [`Processor`](super::Processor) by
/// [`engine::new`](super::new).
///
/// It holds a description of the graph, which [`add_node`](Self::add_node),
/// [`remove_node`](Self::remove_node), [`connect`](Self::connect) and
/// [`disconnect`](Self::disconnect) edit, and [`commit`](Self::commit) checks and sends to the
/// processor as a swap. At most one swap is in flight: a commit made while one is becomes deferred,
/// and [`poll`](Self::poll) sends it once the previous swap has reached its steady phase,
/// with every edit committed meanwhile. The controller owns the nodes' slots: it reuses one only
/// once the processor has let its node go. It holds an event for a node that a swap brings until
/// the processor has applied that swap ([`send_event`](Self::send_event)).
pub struct Controller {
    stats: Arc<EngineStats>,
    context: CompileContext,
    stride: usize,
    messages: Producer<Box<SwapMessage>>,
    events: Producer<TimedEvent>,
    /// How many events the queue holds; the controller holds as many at most.
    event_capacity: usize,
    /// The description being edited.
    draft: Description,
    /// The last description committed and not yet sent, if a commit is deferred.
    committed: Option<Description>,
    /// The description of the last swap sent: the graph the processor plays, or will.
    sent: Description,
    /// The last swap sent, until the processor reports it steady.
    in_flight: Option<InFlight>,
    next_sequence: u64,
    /// Nodes added and not yet sent to the processor.
    unsent: BTreeMap<NodeId, Box<dyn Node>>,
    /// Events for nodes the processor does not hold yet, in the order they came: sent once the
    /// processor has applied the swap that brings their node.
    held_events: Vec<TimedEvent>,
    slots: Slots,
    /// The arena's chunks the processor holds.
    chunks_sent: usize,
    /// The channels of the processor's buffer pool, in its input and output regions.
    pool_inputs: usize,
    pool_outputs: usize,
}

/// A swap sent and not yet steady.
struct InFlight {
    sequence: u64,
    /// The nodes it brings, sorted: their events wait until the processor has applied it.
    added: Vec<NodeId>,
    /// The slots of the nodes it drops, free once it is steady.
    freed_slots: Vec<u32>,
}

/// What [`Controller::commit`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    /// The swap went to the processor, which applies it at its next period.
    Sent {
        /// The swap's sequence number, which [`EngineStats`] reports when it is applied and
        /// when it is steady.
        sequence: u64,
    },
    /// A swap is in flight: this one waits, and [`Controller::poll`] sends it once that swap is
    /// steady.
    Deferred,
}

/// Whether every commit has reached the processor's steady phase, and every event sent for a node
/// it brings has gone to the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStatus {
    /// Every commit is steady: the processor plays the last committed graph.
    Steady,
    /// A swap is in flight, a commit is deferred, or events wait for the processor: for the swap
    /// that brings their node, or for room in the queue.
    Pending,
}

/// Why [`Controller::wait_until_steady`] returned without the swaps being steady.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    /// The timeout passed: the backend may not be calling the processor.
    TimedOut,
    /// The processor is poisoned, and applies no swap any more.
    Poisoned,
}

/// Why [`Controller::send_event`] did not take an event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SendEventError {
    /// The event queue is full, or the controller already holds as many events as the queue
    /// holds; the event is given back.
    QueueFull(TimedEvent),
}

/// Capacity of the queue that carries swaps. One swap is in flight at most; a second place lets
/// the controller push without checking that the processor has popped the last message.
pub(crate) const MESSAGE_CAPACITY: usize = 2;

/// The longest the controller sleeps between two looks at the processor while it waits.
const WAIT_STEP: Duration = Duration::from_millis(1);

/// The parts the engine builds a controller from.
pub(crate) struct ControllerParts {
    pub(crate) stats: Arc<EngineStats>,
    pub(crate) context: CompileContext,
    pub(crate) stride: usize,
    pub(crate) messages: Producer<Box<SwapMessage>>,
    pub(crate) events: Producer<TimedEvent>,
    pub(crate) event_capacity: usize,
    pub(crate) pool_inputs: usize,
    pub(crate) pool_outputs: usize,
}

impl Controller {
    pub(crate) fn new(parts: ControllerParts) -> Self {
        Self {
            stats: parts.stats,
            context: parts.context,
            stride: parts.stride,
            messages: parts.messages,
            events: parts.events,
            event_capacity: parts.event_capacity,
            draft: Description::default(),
            committed: None,
            sent: Description::default(),
            in_flight: None,
            next_sequence: 1,
            unsent: BTreeMap::new(),
            held_events: Vec::new(),
            slots: Slots::default(),
            chunks_sent: 0,
            pool_inputs: parts.pool_inputs,
            pool_outputs: parts.pool_outputs,
        }
    }

    /// The engine's statistics.
    pub fn stats(&self) -> &Arc<EngineStats> {
        &self.stats
    }

    /// Adds a node to the description, in a free slot; it plays once a commit sends it.
    pub fn add_node(&mut self, node: Box<dyn Node>) -> Result<NodeId, GraphError> {
        self.reclaim_slots();
        let id = self.slots.allocate()?;
        let shape = NodeShape {
            inputs: node.input_channels(),
            outputs: node.output_channels(),
        };
        self.draft.nodes.insert(id, shape);
        self.unsent.insert(id, node);
        Ok(id)
    }

    /// Removes a node from the description, with every edge to or from it.
    pub fn remove_node(&mut self, node: NodeId) -> Result<(), GraphError> {
        if self.draft.nodes.remove(&node).is_none() {
            return Err(GraphError::UnknownNode(node));
        }
        self.draft.edges.retain(|edge| !touches(edge, node));
        Ok(())
    }

    /// Adds an edge to the description. It is checked at the next commit.
    pub fn connect(&mut self, source: Source, destination: Destination) {
        self.draft.edges.push(Edge {
            source,
            destination,
        });
    }

    /// Removes an edge from the description.
    pub fn disconnect(
        &mut self,
        source: Source,
        destination: Destination,
    ) -> Result<(), GraphError> {
        let edge = Edge {
            source,
            destination,
        };
        let index = self
            .draft
            .edges
            .iter()
            .position(|held| *held == edge)
            .ok_or(GraphError::MissingEdge(edge))?;
        self.draft.edges.remove(index);
        Ok(())
    }

    /// Checks the description and sends it to the processor as a swap, or defers it while another
    /// swap is in flight. A description that fails its check stays as it is, to be fixed.
    pub fn commit(&mut self) -> Result<Commit, GraphError> {
        self.draft.validate(
            self.context.capture_channels,
            self.context.playback_channels,
        )?;
        self.reclaim_slots();
        // A node added and removed before any swap carried it never reached the processor: it is
        // dropped here and its slot is free at once.
        let abandoned: Vec<NodeId> = self
            .unsent
            .keys()
            .filter(|node| !self.draft.nodes.contains_key(node))
            .copied()
            .collect();
        for node in abandoned {
            self.unsent.remove(&node);
            self.slots.release(node.slot);
        }
        self.committed = Some(self.draft.clone());
        if self.in_flight.is_some() {
            return Ok(Commit::Deferred);
        }
        let sequence = self.send_committed();
        self.send_held_events();
        Ok(sequence.map_or(Commit::Deferred, |sequence| Commit::Sent { sequence }))
    }

    /// Without blocking: notes whether the swap in flight is steady, sends a deferred commit once
    /// it is, and sends the held events whose node the processor now holds. Call it regularly
    /// while it returns `Pending`: nothing else sends a deferred commit, nor those events.
    pub fn poll(&mut self) -> SwapStatus {
        self.reclaim_slots();
        if self.in_flight.is_none() && self.committed.is_some() {
            self.send_committed();
        }
        self.send_held_events();
        // Events held for a node no commit has sent yet wait for that commit, not for a poll.
        let events_wait = self
            .held_events
            .iter()
            .any(|event| !self.unsent.contains_key(&event.node));
        if self.in_flight.is_none() && self.committed.is_none() && !events_wait {
            SwapStatus::Steady
        } else {
            SwapStatus::Pending
        }
    }

    /// Blocks until every commit is steady, polling in steps of at most a millisecond, or until
    /// `timeout` passes or the processor is poisoned.
    #[expect(
        clippy::disallowed_methods,
        reason = "the controller's thread waits for the processor between polls; it never runs on the real-time path"
    )]
    pub fn wait_until_steady(&mut self, timeout: Duration) -> Result<(), WaitError> {
        let start = Instant::now();
        loop {
            if self.poll() == SwapStatus::Steady {
                return Ok(());
            }
            if self.stats.poisoned() {
                return Err(WaitError::Poisoned);
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(WaitError::TimedOut);
            }
            thread::sleep(WAIT_STEP.min(timeout - elapsed));
        }
    }

    /// Sends an event to a node.
    ///
    /// An event for a node that a swap brings waits in the controller until the processor has
    /// applied that swap, so that it never reaches the processor before its node; any later event
    /// for that node waits behind it, so that a node's events keep their order. `poll`,
    /// `wait_until_steady` and the next `send_event` send what no longer waits. An event for a node
    /// the processor does not hold is dropped there, and counted.
    pub fn send_event(&mut self, event: TimedEvent) -> Result<(), SendEventError> {
        self.send_held_events();
        let waits = self.waits_for_its_swap(event.node)
            || self.held_events.iter().any(|held| held.node == event.node);
        if waits {
            if self.held_events.len() >= self.event_capacity {
                return Err(SendEventError::QueueFull(event));
            }
            self.held_events.push(event);
            return Ok(());
        }
        self.events.push(event).map_err(|error| match error {
            PushError::Full(event) => SendEventError::QueueFull(event),
        })
    }

    /// Whether `node` comes with a swap the processor has not applied yet: one not sent, or the
    /// swap in flight before the processor reports it applied.
    fn waits_for_its_swap(&self, node: NodeId) -> bool {
        self.unsent.contains_key(&node)
            || self.in_flight.as_ref().is_some_and(|in_flight| {
                self.stats.last_swap_applied() < in_flight.sequence
                    && in_flight.added.binary_search(&node).is_ok()
            })
    }

    /// Frees the slots of the swap in flight once the processor reports it steady.
    fn reclaim_slots(&mut self) {
        let Some(in_flight) = &self.in_flight else {
            return;
        };
        if self.stats.last_swap_steady() < in_flight.sequence {
            return;
        }
        if let Some(in_flight) = self.in_flight.take() {
            for slot in in_flight.freed_slots {
                self.slots.release(slot);
            }
        }
    }

    /// Compiles the committed description against the last one sent, and sends it with what the
    /// processor needs: the new nodes, new chunks of the arena, a larger pool. Returns the swap's
    /// sequence number, or `None` if the queue was full and the commit stays deferred.
    fn send_committed(&mut self) -> Option<u64> {
        let target = self.committed.take()?;
        let first = self.next_sequence == 1;
        let sequence = self.next_sequence;
        let compiled = compile(&self.sent, &target, first, sequence, self.context);

        let added = compiled.added;
        let nodes: Vec<(NodeId, Option<Box<dyn Node>>)> = added
            .iter()
            .map(|&node| (node, self.unsent.remove(&node)))
            .collect();
        let chunks_needed = self.slots.chunks_needed();
        let chunks: Vec<(usize, Option<ArenaChunk>)> = (self.chunks_sent..chunks_needed)
            .map(|index| (index, Some(new_chunk())))
            .collect();
        let pool = (compiled.input_channels > self.pool_inputs
            || compiled.output_channels > self.pool_outputs)
            .then(|| {
                let inputs = grown(self.pool_inputs, compiled.input_channels);
                let outputs = grown(self.pool_outputs, compiled.output_channels);
                BufferPool::new(self.stride, inputs, outputs)
            });
        let pool_size = pool
            .as_ref()
            .map(|pool| (pool.input_channels(), pool.output_channels()));
        let freed_slots = compiled
            .graph
            .dropped
            .iter()
            .map(|node| node.slot)
            .collect();
        let message = Box::new(SwapMessage {
            graph: Some(Box::new(compiled.graph)),
            nodes,
            chunks,
            pool,
        });
        if let Err(PushError::Full(mut message)) = self.messages.push(message) {
            // Unreachable while one swap is in flight at most; the commit stays deferred.
            for (node, held) in message.nodes.drain(..) {
                if let Some(held) = held {
                    self.unsent.insert(node, held);
                }
            }
            self.committed = Some(target);
            return None;
        }
        self.chunks_sent = chunks_needed;
        if let Some((inputs, outputs)) = pool_size {
            self.pool_inputs = inputs;
            self.pool_outputs = outputs;
        }
        self.sent = target;
        self.in_flight = Some(InFlight {
            sequence,
            added,
            freed_slots,
        });
        self.next_sequence += 1;
        Some(sequence)
    }

    /// Sends the held events whose node no longer waits for its swap, in the order they came.
    /// Those the queue has no room for stay held.
    fn send_held_events(&mut self) {
        let mut index = 0;
        while let Some(&event) = self.held_events.get(index) {
            if self.waits_for_its_swap(event.node) {
                index += 1;
                continue;
            }
            if self.events.push(event).is_err() {
                return;
            }
            self.held_events.remove(index);
        }
    }
}

/// Whether an edge goes from or to a node.
fn touches(edge: &Edge, node: NodeId) -> bool {
    matches!(edge.source, Source::Node(from, _) if from == node)
        || matches!(edge.destination, Destination::Node(to, _) if to == node)
}

/// A pool region's new channel count: what the graph needs, and at least half again the old
/// count, so that a growing graph does not send a pool at every commit.
fn grown(current: usize, needed: usize) -> usize {
    needed.max(current + current / 2)
}

/// The arena's slots, as the controller hands them out: each slot's generation, and the free
/// slots.
#[derive(Debug, Default)]
struct Slots {
    generations: Vec<u32>,
    free: Vec<u32>,
}

impl Slots {
    /// A free slot under its current generation, or a new slot.
    fn allocate(&mut self) -> Result<NodeId, GraphError> {
        if let Some(slot) = self.free.pop() {
            let generation = self.generations[slot as usize];
            return Ok(NodeId { slot, generation });
        }
        if self.generations.len() >= CHUNK_SLOTS * MAX_CHUNKS {
            return Err(GraphError::TooManyNodes);
        }
        let slot = u32::try_from(self.generations.len()).map_err(|_| GraphError::TooManyNodes)?;
        self.generations.push(0);
        Ok(NodeId {
            slot,
            generation: 0,
        })
    }

    /// Frees a slot whose node the processor no longer holds; its next node gets the next
    /// generation.
    fn release(&mut self, slot: u32) {
        let generation = &mut self.generations[slot as usize];
        *generation = generation.wrapping_add(1);
        self.free.push(slot);
    }

    /// The chunks the arena needs for every slot handed out so far.
    fn chunks_needed(&self) -> usize {
        self.generations.len().div_ceil(CHUNK_SLOTS)
    }
}

impl fmt::Display for WaitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => write!(formatter, "the swaps were not steady before the timeout"),
            Self::Poisoned => write!(formatter, "the processor is poisoned"),
        }
    }
}

impl Error for WaitError {}

impl fmt::Display for SendEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull(event) => write!(formatter, "the event queue is full: {event:?}"),
        }
    }
}

impl Error for SendEventError {}
