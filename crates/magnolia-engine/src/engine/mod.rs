//! The engine: a graph of nodes, compiled off the real-time thread and swapped in at a period's
//! start without a click, played by the processor, its leftovers freed by the collector
//! ([ADR-0007](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0007-the-audio-engine.md)).
//!
//! [`new`] makes the two halves, which never share memory except through queues and
//! atomics:
//!
//! - the [`Controller`], on any thread but the audio one, edits a description of the graph,
//!   compiles each change into a swap and sends it, and sends timed events;
//! - the [`Processor`], owned by a backend, runs once a period on the real-time thread: it applies
//!   a swap whole at a period's start, plays its phases, and hands what it replaces to the
//!   collector thread, which drops it.
//!
//! A swap that changes edges fades them over 5 ms: a crossfade through the union of both graphs,
//! or, when the union would cycle or cost too much, a dip that fades the old graph out, then the
//! new one in. Each commit compensates the nodes' latencies: the compiled graph holds delay lines
//! where paths of different latencies meet, and the transport pre-rolls by the largest
//! compensation. A node may have timeline sources, [`MidiSequence`]s, whose events the processor
//! reads at the node's own transport position. `docs/ARCHITECTURE.md` describes the threads and
//! queues.
//!
//! ```
//! use magnolia_engine::engine::{self, Destination, EngineConfig, Source};
//! use magnolia_engine::nodes::Sine;
//! use magnolia_engine::offline::OfflineBackend;
//! use magnolia_engine::time::SampleRate;
//!
//! let rate = SampleRate::new(48_000).unwrap();
//! let (mut controller, processor) = engine::new(EngineConfig::new(rate, 64, 0, 1)).unwrap();
//! let sine = controller.add_node(Box::new(Sine::new(rate, 440.0, 0.5))).unwrap();
//! controller.connect(Source::Node(sine, 0), Destination::Playback(0));
//! controller.commit().unwrap();
//!
//! let mut backend = OfflineBackend::new(processor);
//! let played = backend.play(&[64, 64]);
//! assert_eq!(played[0].len(), 128);
//! assert_eq!(controller.stats().periods(), 2);
//! ```

mod arena;
mod collector;
mod compensation;
mod compile;
mod compiled;
mod controller;
mod delay_line;
mod description;
mod events;
mod fade;
mod graph;
mod memory;
mod pool;
mod processor;
mod sequence;
mod sources;
mod transport;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::fmt;
use std::io;
use std::sync::Arc;

use rtrb::RingBuffer;

pub use collector::wake_collector;
pub use controller::{Commit, Controller, SendEventError, SwapStatus, TransportError, WaitError};
pub use events::TimedEvent;
pub use graph::{Destination, Edge, EdgeEnd, GraphError, NodeId, Source};
pub use processor::Processor;
pub use sequence::{
    ControllerPoint, MidiClip, MidiNote, MidiSequence, PitchBendPoint, SequenceError,
};

pub(crate) use delay_line::DelayLine;
pub(crate) use fade::fade_frames;

use arena::Arena;
use collector::{RELEASED_PER_SWAP, start_collector};
use compensation::Compensated;
use compile::{CompileContext, compile};
use controller::{ControllerParts, MESSAGE_CAPACITY};
use events::EventInbox;
use fade::FadeTable;
use pool::BufferPool;
use processor::ProcessorParts;
use sources::SourceReader;
use transport::{COMMAND_CAPACITY, TransportClock};

use crate::stats::EngineStats;
use crate::time::SampleRate;

/// What an engine is made with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    /// The rate the device plays at.
    pub sample_rate: SampleRate,
    /// The largest number of frames the processor works on at once, from 1 to
    /// [`EngineConfig::MAX_BLOCK`]. A longer period is cut into sub-blocks of this length. It sets
    /// the stride of the buffer pool: this length, rounded up to 16 floats.
    pub largest_block: usize,
    /// The capture channels the graph can read.
    pub capture_channels: usize,
    /// The playback channels the graph can write.
    pub playback_channels: usize,
    /// How many events the processor holds at once, in its queue and its buffer of pending events
    /// together, until it takes them out at their sample; the controller refuses more. The
    /// controller also holds as many at most, waiting for the swap that brings their node. It also
    /// bounds the events a node's timeline sources give it in one sub-block: past that, they are
    /// dropped and counted. Which ones a sub-block holds depends on how the period was cut, so once
    /// some are dropped, a render no longer nulls with playback.
    pub event_capacity: usize,
    /// How many items the processor can hand the collector before it runs; at least
    /// [`EngineConfig::MIN_GARBAGE_CAPACITY`].
    pub garbage_capacity: usize,
}

impl EngineConfig {
    /// The largest block an engine accepts.
    pub const MAX_BLOCK: usize = 65_536;
    /// The smallest queue to the collector: room for everything one swap replaces at once.
    pub const MIN_GARBAGE_CAPACITY: usize = RELEASED_PER_SWAP;

    /// A configuration with room for 1,024 queued events and 64 items for the collector.
    pub fn new(
        sample_rate: SampleRate,
        largest_block: usize,
        capture_channels: usize,
        playback_channels: usize,
    ) -> Self {
        Self {
            sample_rate,
            largest_block,
            capture_channels,
            playback_channels,
            event_capacity: 1_024,
            garbage_capacity: 64,
        }
    }

    fn check(&self) -> Result<(), EngineError> {
        if !(1..=Self::MAX_BLOCK).contains(&self.largest_block) {
            return Err(EngineError::LargestBlockOutOfRange(self.largest_block));
        }
        if self.event_capacity == 0 {
            return Err(EngineError::NoEventCapacity);
        }
        if self.garbage_capacity < Self::MIN_GARBAGE_CAPACITY {
            return Err(EngineError::GarbageCapacityTooSmall(self.garbage_capacity));
        }
        Ok(())
    }
}

/// Why an engine could not be made.
#[derive(Debug)]
pub enum EngineError {
    /// The largest block is 0 or above [`EngineConfig::MAX_BLOCK`].
    LargestBlockOutOfRange(usize),
    /// The event queue has no room.
    NoEventCapacity,
    /// The collector's queue is smaller than [`EngineConfig::MIN_GARBAGE_CAPACITY`].
    GarbageCapacityTooSmall(usize),
    /// The collector's thread did not start.
    CollectorDidNotStart(io::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LargestBlockOutOfRange(frames) => write!(
                formatter,
                "the largest block must hold 1 to {} frames, not {frames}",
                EngineConfig::MAX_BLOCK
            ),
            Self::NoEventCapacity => write!(formatter, "the event queue must have room"),
            Self::GarbageCapacityTooSmall(capacity) => write!(
                formatter,
                "the collector's queue must hold at least {} items, not {capacity}",
                EngineConfig::MIN_GARBAGE_CAPACITY
            ),
            Self::CollectorDidNotStart(error) => {
                write!(formatter, "the collector's thread did not start: {error}")
            }
        }
    }
}

impl Error for EngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CollectorDidNotStart(error) => Some(error),
            _ => None,
        }
    }
}

/// Makes an engine's two halves, and starts its collector. The processor plays silence until the
/// controller's first commit.
pub fn new(config: EngineConfig) -> Result<(Controller, Processor), EngineError> {
    config.check()?;
    let stats = EngineStats::new_written();
    let stride = config.largest_block.next_multiple_of(16);
    let context = CompileContext {
        capture_channels: config.capture_channels,
        playback_channels: config.playback_channels,
        fade_frames: fade_frames(config.sample_rate),
    };
    let (message_producer, message_consumer) = RingBuffer::new(MESSAGE_CAPACITY);
    let (event_producer, event_consumer) = RingBuffer::new(config.event_capacity);
    let (transport_producer, transport_consumer) = RingBuffer::new(COMMAND_CAPACITY);
    let garbage =
        start_collector(config.garbage_capacity).map_err(EngineError::CollectorDidNotStart)?;
    // Silence: a graph without nodes, its playback channels summing nothing.
    let empty = Compensated::default();
    let silence = compile(&empty, &empty, 0, context);
    let pool = BufferPool::new(stride, silence.input_channels, silence.output_channels);
    let controller = Controller::new(ControllerParts {
        stats: Arc::clone(&stats),
        context,
        stride,
        messages: message_producer,
        events: event_producer,
        transport: transport_producer,
        event_capacity: config.event_capacity,
        pool_inputs: pool.input_channels(),
        pool_outputs: pool.output_channels(),
    });
    let processor = Processor::new(ProcessorParts {
        stats,
        sample_rate: config.sample_rate,
        largest_block: config.largest_block,
        capture_channels: config.capture_channels,
        playback_channels: config.playback_channels,
        graph: Box::new(silence.graph),
        pool,
        arena: Arena::new(),
        fade: FadeTable::new(context.fade_frames),
        events: EventInbox::new(event_consumer, config.event_capacity),
        sources: SourceReader::new(config.event_capacity),
        transport: TransportClock::new(transport_consumer, context.fade_frames),
        messages: message_consumer,
        garbage,
    });
    Ok((controller, processor))
}
