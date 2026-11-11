//! Timed events, from the controller's queue to each node's sub-block.
//!
//! At each period's start, once it has applied any swap, the processor pops everything the
//! controller queued into a pending buffer. The controller sends an event only once the processor
//! holds its node, and never more events than the pending buffer can hold (it counts those the
//! processor has taken out of it, in [`EngineStats`](crate::stats::EngineStats)), so the processor
//! can always pop the whole queue. For each sub-block it takes the pending events due before the
//! sub-block's end, sorts them by node, sample, note offs first, and arrival, and lends each node
//! its own, with offsets from the sub-block's start. Every buffer is reserved and written
//! beforehand; sorting happens in place. Later events wait in the pending buffer.
//!
//! An event keeps its own sample until a node receives it: a late one then gets offset 0, after
//! the late events of earlier samples. So a note played and released before the period, both late,
//! still comes on, then off.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use rtrb::Consumer;

use super::compiled::Phase;
use super::graph::NodeId;
use super::memory::written_vec;
use crate::node::{Event, EventKind};
use crate::time::SamplePosition;

/// An event for one node at a sample of the engine's timeline, as the controller sends it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedEvent {
    /// The node it is for.
    pub node: NodeId,
    /// The sample at which it takes effect. An event for a sample already played takes effect at
    /// the first sample of the next period.
    pub at: SamplePosition,
    /// What happens.
    pub kind: EventKind,
}

/// An event in the processor's hands, numbered in the order it arrived.
#[derive(Clone, Copy, Debug)]
struct PendingEvent {
    node: NodeId,
    /// The sample it is due at, even past: a late event keeps it, to keep its order.
    at: i64,
    arrival: u64,
    kind: EventKind,
}

/// The processor's end of the event queue, and its buffers.
pub(crate) struct EventInbox {
    queue: Consumer<TimedEvent>,
    /// Popped events not yet due, sorted by sample, then arrival.
    pending: Vec<PendingEvent>,
    /// The events due in the current sub-block, sorted by node, sample, note offs first, and
    /// arrival.
    due: Vec<PendingEvent>,
    /// The current node's events, as it receives them.
    node_events: Vec<Event>,
    /// How many events have arrived, to number the next.
    arrivals: u64,
}

impl EventInbox {
    /// An inbox reading `queue`, its buffers reserved for `capacity` events. Built off the
    /// real-time thread.
    pub(crate) fn new(queue: Consumer<TimedEvent>, capacity: usize) -> Self {
        Self {
            queue,
            pending: written_vec(capacity),
            due: written_vec(capacity),
            node_events: written_vec(capacity),
            arrivals: 0,
        }
    }

    /// Pops the queued events, as many as the pending buffer has room for, which is all of them
    /// since the controller sends no more than that.
    pub(crate) fn pop(&mut self) {
        let room = self.pending.spare_capacity_mut().len();
        let mut popped = false;
        for _ in 0..room {
            let Ok(event) = self.queue.pop() else { break };
            self.pending.push(PendingEvent {
                node: event.node,
                at: event.at.0,
                arrival: self.arrivals,
                kind: event.kind,
            });
            self.arrivals = self.arrivals.wrapping_add(1);
            popped = true;
        }
        if popped {
            self.pending
                .sort_unstable_by_key(|event| (event.at, event.arrival));
        }
    }

    /// Takes out of the pending buffer the events due before `end`, except those for nodes that
    /// wait for a later phase, and sorts them for [`events_of`](Self::events_of). Returns how many
    /// it took.
    pub(crate) fn take_due(&mut self, end: i64, phase: &Phase) -> usize {
        let Self { pending, due, .. } = self;
        due.clear();
        pending.retain(|event| {
            let is_due = event.at < end && !phase.node_waits(event.node);
            // `due` has the pending buffer's room, and starts empty: the push never allocates.
            if is_due && !due.spare_capacity_mut().is_empty() {
                due.push(*event);
                false
            } else {
                true
            }
        });
        due.sort_unstable_by_key(|event| {
            (
                event.node,
                event.at,
                rank_at_one_sample(event.kind),
                event.arrival,
            )
        });
        due.len()
    }

    /// The due events of one node, with offsets from `start`, the sub-block's first sample; a late
    /// event's offset is 0.
    pub(crate) fn events_of(&mut self, node: NodeId, start: i64) -> &[Event] {
        let Self {
            due, node_events, ..
        } = self;
        node_events.clear();
        let first = due.partition_point(|event| event.node < node);
        let ours = due.get(first..).unwrap_or(&[]);
        let count = ours.partition_point(|event| event.node == node);
        for event in ours.iter().take(count) {
            let offset = event.at.saturating_sub(start).max(0);
            // `node_events` has the pending buffer's room: the push never allocates.
            if !node_events.spare_capacity_mut().is_empty() {
                node_events.push(Event {
                    offset: u32::try_from(offset).unwrap_or(u32::MAX),
                    kind: event.kind,
                });
            }
        }
        node_events
    }
}

/// Where an event goes among a node's events at one sample, before their order of arrival: a note
/// off first, so that a note ended and started again at one sample sounds (ADR-0027).
pub(crate) fn rank_at_one_sample(kind: EventKind) -> u8 {
    match kind {
        EventKind::NoteOff(_) => 0,
        EventKind::Parameter { .. }
        | EventKind::NoteOn(_)
        | EventKind::PitchBend { .. }
        | EventKind::Controller { .. } => 1,
    }
}
