//! The controller: the engine's non-real-time half, which edits the graph, compiles each swap and
//! sends it, and sends events. It may allocate and block; the processor never waits for it.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rtrb::{Producer, PushError};

use super::arena::{ArenaChunk, CHUNK_SLOTS, MAX_CHUNKS, new_chunk};
use super::compensation::{self, Compensated, DelayKey, Plan};
use super::compile::{CompileContext, attach_sources, compile};
use super::compiled::SwapMessage;
use super::delay_line::DelayLine;
use super::description::{Description, NodeShape};
use super::events::TimedEvent;
use super::graph::{Destination, Edge, GraphError, NodeId, Source};
use super::pool::BufferPool;
use super::sequence::MidiSequence;
use super::sources::TimelineSources;
use super::transport::{Play, TransportCommand};
use crate::node::{MAX_LATENCY, Node};
use crate::stats::EngineStats;
use crate::time::{SamplePosition, TempoMap};

/// The engine's non-real-time half, made with its [`Processor`](super::Processor) by
/// [`engine::new`](super::new).
///
/// It holds a description of the graph, which [`add_node`](Self::add_node),
/// [`remove_node`](Self::remove_node), [`connect`](Self::connect) and
/// [`disconnect`](Self::disconnect) edit, and [`commit`](Self::commit) checks, compensates and
/// sends to the processor as a swap. At most one swap is in flight: a commit made while one is
/// becomes deferred, and [`poll`](Self::poll) sends it once the previous swap has reached its
/// steady phase, with every edit committed meanwhile. The controller owns the nodes' slots: it
/// reuses one only once the processor has let its node go. It holds an event for a node that a
/// swap brings until the processor has applied that swap ([`send_event`](Self::send_event)).
///
/// Each commit compensates the nodes' latencies
/// ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)):
/// the graph sent holds a delay line where paths of different latencies meet, a node of the arena
/// that the controller adds, keeps while its destination and delay stay, and removes. The
/// description being edited never holds them.
///
/// A node may have timeline sources, MIDI sequences ([`set_sequences`](Self::set_sequences)), laid
/// on the timeline by a tempo map ([`set_tempo_map`](Self::set_tempo_map)); both take effect at the
/// next commit, which sends them with its graph
/// ([ADR-0027](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0027-timeline-events-from-sources-on-nodes.md)).
/// [`play`](Self::play) and [`stop`](Self::stop) drive the transport, which pre-rolls by the
/// graph's largest compensation.
pub struct Controller {
    stats: Arc<EngineStats>,
    context: CompileContext,
    stride: usize,
    messages: Producer<Box<SwapMessage>>,
    events: Producer<TimedEvent>,
    transport: Producer<TransportCommand>,
    /// How many plays have been sent, to number the next.
    plays_sent: u64,
    /// How many events the processor holds at once, queued or pending; the controller holds as
    /// many at most, waiting for their swap.
    event_capacity: usize,
    /// How many events the controller has pushed into the queue. Less the events the processor
    /// has taken (`EngineStats::events_taken`), it is how many the processor holds.
    events_pushed: u64,
    /// The description being edited.
    draft: Description,
    /// The timeline sources being edited, which the next commit sends with its graph.
    sources: TimelineSources,
    /// The last commit not yet sent, if a commit is deferred.
    committed: Option<Committed>,
    /// The graph of the last swap sent, compensated: the graph the processor plays, or will.
    sent: Compensated,
    /// The last swap sent, until the processor reports it steady.
    in_flight: Option<InFlight>,
    next_sequence: u64,
    /// Nodes added and not yet sent to the processor.
    unsent: BTreeMap<NodeId, Box<dyn Node>>,
    /// The delay lines that the deferred commit adds, not yet sent. A later commit compensates
    /// again and drops those it does not keep.
    unsent_delay_lines: BTreeMap<NodeId, Box<dyn Node>>,
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

/// A commit not yet sent: its graph, compensated, and its timeline sources.
struct Committed {
    graph: Compensated,
    sources: TimelineSources,
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
    /// The processor holds as many events as its pending buffer can, until it takes some out at
    /// their sample; or the controller holds as many waiting for their swap. The event is given
    /// back: send it again after a period.
    QueueFull(TimedEvent),
}

/// Why [`Controller::play`] or [`Controller::stop`] was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The queue of transport commands is full: the processor takes them all at its next period.
    /// Send the command again after a period.
    QueueFull,
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
    pub(crate) transport: Producer<TransportCommand>,
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
            transport: parts.transport,
            plays_sent: 0,
            event_capacity: parts.event_capacity,
            events_pushed: 0,
            draft: Description::default(),
            sources: TimelineSources::default(),
            committed: None,
            sent: Compensated::default(),
            in_flight: None,
            next_sequence: 1,
            unsent: BTreeMap::new(),
            unsent_delay_lines: BTreeMap::new(),
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

    /// Adds a node to the description, in a free slot; it plays once a commit sends it. Its
    /// latency is read here, once: one above [`MAX_LATENCY`] is taken as `MAX_LATENCY` and
    /// counted in [`EngineStats::latencies_capped`].
    pub fn add_node(&mut self, node: Box<dyn Node>) -> Result<NodeId, GraphError> {
        self.reclaim_slots();
        let id = self.slots.allocate()?;
        let declared = node.latency();
        if declared > MAX_LATENCY {
            self.stats.count_latency_capped();
        }
        let shape = NodeShape {
            inputs: node.input_channels(),
            outputs: node.output_channels(),
            latency: declared.min(MAX_LATENCY),
        };
        self.draft.nodes.insert(id, shape);
        self.unsent.insert(id, node);
        Ok(id)
    }

    /// Removes a node from the description, with every edge to or from it and its timeline
    /// sources.
    pub fn remove_node(&mut self, node: NodeId) -> Result<(), GraphError> {
        if self.draft.nodes.remove(&node).is_none() {
            return Err(GraphError::UnknownNode(node));
        }
        self.draft.edges.retain(|edge| !touches(edge, node));
        self.sources.sequences.remove(&node);
        Ok(())
    }

    /// Gives a node its timeline sources: MIDI sequences, whose events it receives while the
    /// transport rolls, each in the samples its own position covers; none removes them. They take
    /// effect at the next commit. The processor never frees a sequence: a replaced one reaches the
    /// collector inside the old graph.
    pub fn set_sequences(
        &mut self,
        node: NodeId,
        sequences: Vec<Arc<MidiSequence>>,
    ) -> Result<(), GraphError> {
        if !self.draft.nodes.contains_key(&node) {
            return Err(GraphError::UnknownNode(node));
        }
        if sequences.is_empty() {
            self.sources.sequences.remove(&node);
        } else {
            self.sources.sequences.insert(node, sequences);
        }
        Ok(())
    }

    /// Sets the tempo map that lays the sequences' ticks on the timeline; until one is set, they
    /// play nothing. It takes effect at the next commit.
    pub fn set_tempo_map(&mut self, tempo_map: Arc<TempoMap>) {
        self.sources.tempo_map = Some(tempo_map);
    }

    /// Plays from a timeline sample, `from`: the processor applies it at its next period's start.
    /// Returns the play's sequence number, from 1, by which [`EngineStats::play_start`] says when
    /// the output reaches `from`: the transport pre-rolls by the graph's largest compensation, so
    /// that every path reaches the output on `from` together. While the transport rolls, it stops
    /// first, as [`stop`](Self::stop) does, and the play starts at the first period after the stop
    /// ends.
    pub fn play(&mut self, from: SamplePosition) -> Result<u64, TransportError> {
        let sequence = self.plays_sent + 1;
        self.transport
            .push(TransportCommand::Play(Play { from, sequence }))
            .map_err(|_| TransportError::QueueFull)?;
        self.plays_sent = sequence;
        Ok(sequence)
    }

    /// Stops the transport, applied at the processor's next period's start. Every path ends on
    /// the same timeline sample: what the most latent node has read, plus a fade's 5 ms. There each
    /// node's sources end the notes it holds, release the sustain pedal and reset the pitch bend.
    /// A stop asked before anything audible was read stops at once, silent. A play waiting for a
    /// stop is cancelled.
    pub fn stop(&mut self) -> Result<(), TransportError> {
        self.transport
            .push(TransportCommand::Stop)
            .map_err(|_| TransportError::QueueFull)
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

    /// Checks the description, compensates its latencies and sends it to the processor as a swap,
    /// with the timeline sources, or defers it while another swap is in flight. A description that
    /// fails its check stays as it is, to be fixed; so does one whose delay lines find no free slot
    /// in the arena.
    pub fn commit(&mut self) -> Result<Commit, GraphError> {
        self.draft.validate(
            self.context.capture_channels,
            self.context.playback_channels,
        )?;
        self.reclaim_slots();
        let plan = compensation::plan(&self.draft, self.context.playback_channels);
        let delay_lines = self.delay_line_nodes(&plan)?;
        self.committed = Some(Committed {
            graph: plan.compensate(&self.draft, &delay_lines),
            sources: self.sources.clone(),
        });
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
    /// The processor holds at most [`EngineConfig::event_capacity`](super::EngineConfig) events,
    /// queued or pending: past that, the event is refused until the processor has taken some at
    /// their sample, so that an event due now never waits behind events far ahead.
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
        self.push_event(event).map_err(SendEventError::QueueFull)
    }

    /// Pushes an event into the queue, unless the processor already holds as many events as its
    /// pending buffer can; gives it back then.
    fn push_event(&mut self, event: TimedEvent) -> Result<(), TimedEvent> {
        // The processor publishes its count after the events have left its buffer, so a count read
        // late only makes the controller refuse early, never overfill the buffer.
        let held = self.events_pushed.saturating_sub(self.stats.events_taken());
        if held >= self.event_capacity as u64 {
            return Err(event);
        }
        self.events
            .push(event)
            .map_err(|PushError::Full(event)| event)?;
        self.events_pushed += 1;
        Ok(())
    }

    /// The delay lines of the graph last sent to the processor, which a commit adds where paths of
    /// different latencies meet. For tests.
    #[doc(hidden)]
    pub fn delay_lines(&self) -> Vec<NodeId> {
        self.sent.delay_lines.values().copied().collect()
    }

    /// Gives a node to each delay line that `plan`, the draft's, needs: the one the processor
    /// plays, or the deferred commit's, for the same destination and delay, or a new one.
    ///
    /// New ones take the slots this commit frees first: those of the nodes added and removed
    /// before any swap carried them, and those of the deferred commit's delay lines it does not
    /// keep. None of these ever reached the processor, so their slots are free at once. When even
    /// those are too few, it fails with `TooManyNodes` before changing anything.
    fn delay_line_nodes(&mut self, plan: &Plan) -> Result<BTreeMap<DelayKey, NodeId>, GraphError> {
        let deferred = self
            .committed
            .as_ref()
            .map(|committed| &committed.graph.delay_lines);
        let mut nodes = BTreeMap::new();
        let mut missing = Vec::new();
        for (&key, &channels) in plan.delay_lines() {
            let kept = self
                .sent
                .delay_lines
                .get(&key)
                .or_else(|| deferred.and_then(|deferred| deferred.get(&key)));
            match kept {
                Some(&line) => {
                    nodes.insert(key, line);
                }
                None => missing.push((key, channels)),
            }
        }
        let abandoned: Vec<NodeId> = self
            .unsent
            .keys()
            .filter(|node| !self.draft.nodes.contains_key(node))
            .copied()
            .collect();
        let kept: BTreeSet<NodeId> = nodes.values().copied().collect();
        let replaced: Vec<NodeId> = self
            .unsent_delay_lines
            .keys()
            .filter(|line| !kept.contains(line))
            .copied()
            .collect();
        let freed = abandoned.len().saturating_add(replaced.len());
        if missing.len() > self.slots.available().saturating_add(freed) {
            return Err(GraphError::TooManyNodes);
        }
        for node in abandoned {
            self.unsent.remove(&node);
            self.slots.release(node.slot);
        }
        for line in replaced {
            self.unsent_delay_lines.remove(&line);
            self.slots.release(line.slot);
        }
        for (key, channels) in missing {
            // Counted above: the arena has a slot for each.
            let line = self.slots.allocate()?;
            let delay = usize::try_from(key.delay).unwrap_or(usize::MAX);
            self.unsent_delay_lines
                .insert(line, Box::new(DelayLine::new(channels, delay)));
            nodes.insert(key, line);
        }
        Ok(nodes)
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
        let Committed {
            graph: target,
            sources,
        } = self.committed.take()?;
        let sequence = self.next_sequence;
        let mut compiled = compile(&self.sent, &target, sequence, self.context);
        attach_sources(&mut compiled.graph, &sources);

        let added = compiled.added;
        let nodes: Vec<(NodeId, Option<Box<dyn Node>>)> = added
            .iter()
            .map(|&node| {
                let held = self
                    .unsent
                    .remove(&node)
                    .or_else(|| self.unsent_delay_lines.remove(&node));
                (node, held)
            })
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
                    if target.is_delay_line(node) {
                        self.unsent_delay_lines.insert(node, held);
                    } else {
                        self.unsent.insert(node, held);
                    }
                }
            }
            self.committed = Some(Committed {
                graph: target,
                sources,
            });
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
            if self.push_event(event).is_err() {
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

    /// How many slots `allocate` can still hand out: those free, and those never handed out.
    fn available(&self) -> usize {
        let never_handed_out = (CHUNK_SLOTS * MAX_CHUNKS).saturating_sub(self.generations.len());
        self.free.len().saturating_add(never_handed_out)
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

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => write!(formatter, "the queue of transport commands is full"),
        }
    }
}

impl Error for TransportError {}
