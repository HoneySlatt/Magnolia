//! The node contract: what every node of the engine's graph implements, today the engine's own
//! nodes and its delay lines, from M1.6 the proxy of every plugin host
//! ([ADR-0021](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0021-the-node-contract.md)).
//!
//! The processor calls [`Node::process`] once per sub-block on the real-time thread, with a
//! [`ProcessContext`] that lends the node its input and output channels, its events and the
//! [`Transport`] as the node sees it. A node is created and dropped off the real-time thread: the
//! controller sends it in, the collector drops it.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use crate::time::SamplePosition;

/// The largest latency a node may declare, in samples: 65,536, 1.37 s at 48 kHz. The engine takes
/// a larger one as this, and counts it
/// ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)).
pub const MAX_LATENCY: u32 = 65_536;

/// A node of the engine's graph.
///
/// Its channel counts and its latency are fixed for its life: a different layout is a new node,
/// swapped in. Its output must not depend on how the engine cuts blocks, since the engine splits a
/// period into sub-blocks at a swap's fade or past its largest block, and a render must equal
/// playback to the bit. It keeps denormals out of its own state: the threads that run nodes keep
/// the default floating-point mode.
pub trait Node: Send {
    /// How many input channels the node reads.
    fn input_channels(&self) -> usize;

    /// How many output channels the node writes.
    fn output_channels(&self) -> usize;

    /// How many samples late the node's output comes, such as a lookahead limiter's lookahead: the
    /// engine compensates it by running the paths that meet it earlier. Read once, when the node is
    /// added; at most [`MAX_LATENCY`]. No latency by default.
    fn latency(&self) -> u32 {
        0
    }

    /// Processes one sub-block: reads the inputs and the events, writes every output channel.
    /// Runs on the real-time thread: it must not allocate, lock, block or panic.
    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus;
}

/// What a node says of the sub-block it has just processed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// The outputs are valid.
    Continue,
    /// The outputs are valid for this sub-block, and the node needs no call while its inputs stay
    /// silent and no event comes for it. The engine then skips it and writes silence in its
    /// place.
    Sleep,
    /// The outputs are discarded. The engine writes silence in their place, counts the error and
    /// calls the node again in the next sub-block.
    Error,
}

/// An event for one node, at a sample of the sub-block being processed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    /// The sample of the sub-block at which the event takes effect, from 0.
    pub offset: u32,
    /// What happens.
    pub kind: EventKind,
}

/// What an event does
/// ([ADR-0027](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0027-timeline-events-from-sources-on-nodes.md)).
/// A node ignores the kinds it has no use for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    /// Sets one of the node's parameters, which each node numbers and documents.
    Parameter {
        /// The parameter's number.
        index: u32,
        /// Its new value.
        value: f64,
    },
    /// A note starts.
    NoteOn(Note),
    /// A note ends. At one sample, every note off comes before any other event, so that a note
    /// ended and started again on the same sample sounds.
    NoteOff(Note),
    /// The pitch wheel of a MIDI channel moves.
    PitchBend {
        /// The MIDI channel, from 0 to 15.
        channel: u8,
        /// The wheel's position, from -1 (down) to 1 (up); 0 at rest.
        value: f32,
    },
    /// A MIDI controller of a channel moves, such as the sustain pedal (64).
    Controller {
        /// The MIDI channel, from 0 to 15.
        channel: u8,
        /// The controller's number, from 0 to 127.
        number: u8,
        /// Its position, from 0 to 1.
        value: f32,
    },
}

/// A note, as a note on or a note off carries it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// The note's own id, which its note off and its expressions repeat (M1.6); `None` when its
    /// source gives none, as a MIDI file does.
    pub id: Option<u32>,
    /// The MIDI channel, from 0 to 15.
    pub channel: u8,
    /// The key, from 0 to 127; 60 is middle C.
    pub key: u8,
    /// How hard the note is struck or released, from 0 to 1.
    pub velocity: f32,
}

/// The transport as one node sees it in one sub-block
/// ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)).
///
/// The transport's clock is the timeline sample that the output plays. A node runs ahead of the
/// output by its compensation, so it sees the clock plus that distance: a source on a latent path
/// reads the timeline early, and its sound reaches the output in line with the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transport {
    /// Whether the transport rolls through the whole sub-block. The processor cuts a sub-block where
    /// the transport stops, so it never stops inside one. After a stop, it rolls until its clock
    /// has played the stop's sample, where the nodes nearest the output end their notes.
    pub rolling: bool,
    /// The timeline sample of the sub-block's first frame at the node's input: the transport's
    /// clock plus how far ahead of the output the node's input runs. Stopped, the clock holds the
    /// next sample it would play.
    pub position: SamplePosition,
    /// The timeline samples that play since the last play: none before its start, none from its
    /// stop.
    pub audible: AudibleRegion,
}

impl Transport {
    /// The transport before any play: stopped at sample 0, its audible region empty.
    pub const STOPPED: Transport = Transport {
        rolling: false,
        position: SamplePosition(0),
        audible: AudibleRegion {
            start: SamplePosition(0),
            stop: Some(SamplePosition(0)),
        },
    };
}

/// The timeline samples that play: from the position a play asked for, up to the stop, which
/// stays unknown until the transport is asked to stop. A source plays only the events whose sample
/// lies in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudibleRegion {
    /// The first sample that plays: the position the play asked for.
    pub start: SamplePosition,
    /// The first sample that no longer plays; `None` while the transport has not been asked to
    /// stop. A stop asked before anything audible was read leaves the region empty: its stop is its
    /// start.
    pub stop: Option<SamplePosition>,
}

/// What a node receives for one sub-block: its channels, its events and the transport.
///
/// Each channel holds exactly [`frames`](Self::frames) samples. A channel number out of range
/// gives an empty slice and reads as silent, so that a node's mistake never panics on the
/// real-time thread.
#[derive(Debug)]
pub struct ProcessContext<'a> {
    frames: usize,
    stride: usize,
    inputs: &'a [f32],
    input_silence: &'a [bool],
    outputs: &'a mut [f32],
    output_silence: &'a mut [bool],
    events: &'a [Event],
    transport: Transport,
}

impl<'a> ProcessContext<'a> {
    /// A context over a node's channels, laid out every `stride` samples in `inputs` and
    /// `outputs`, one silence flag per channel, the transport [stopped](Transport::STOPPED) until
    /// [`with_transport`](Self::with_transport) says otherwise.
    pub(crate) fn new(
        frames: usize,
        stride: usize,
        inputs: &'a [f32],
        input_silence: &'a [bool],
        outputs: &'a mut [f32],
        output_silence: &'a mut [bool],
        events: &'a [Event],
    ) -> Self {
        Self {
            frames,
            stride,
            inputs,
            input_silence,
            outputs,
            output_silence,
            events,
            transport: Transport::STOPPED,
        }
    }

    /// The same context, with the transport as the node sees it.
    pub(crate) fn with_transport(self, transport: Transport) -> Self {
        Self { transport, ..self }
    }

    /// The number of samples in each channel of this sub-block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The number of input channels.
    pub fn input_channels(&self) -> usize {
        self.input_silence.len()
    }

    /// The number of output channels.
    pub fn output_channels(&self) -> usize {
        self.output_silence.len()
    }

    /// An input channel's samples. The slice does not borrow the context, so that a node can read
    /// an input while it writes an output.
    pub fn input(&self, channel: usize) -> &'a [f32] {
        let inputs: &'a [f32] = self.inputs;
        channel_start(channel, self.stride)
            .and_then(|start| inputs.get(start..))
            .and_then(|channel| channel.get(..self.frames))
            .unwrap_or(&[])
    }

    /// Whether an input channel is silent: nothing it sums can sound, and it holds zeros.
    pub fn input_is_silent(&self, channel: usize) -> bool {
        self.input_silence.get(channel).copied().unwrap_or(true)
    }

    /// An output channel's samples, to write. They hold no meaningful value before the node writes
    /// them.
    pub fn output_mut(&mut self, channel: usize) -> &mut [f32] {
        let frames = self.frames;
        channel_start(channel, self.stride)
            .and_then(|start| self.outputs.get_mut(start..))
            .and_then(|channel| channel.get_mut(..frames))
            .unwrap_or_default()
    }

    /// Every output channel's samples, in order, to write.
    pub fn outputs_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let frames = self.frames;
        let channels = self.output_silence.len();
        // `chunks_mut` panics on a zero size; the engine's stride is never zero.
        self.outputs
            .chunks_mut(self.stride.max(1))
            .take(channels)
            .map(move |channel| channel.get_mut(..frames).unwrap_or_default())
    }

    /// Whether an output channel is flagged silent. Every output starts the sub-block unflagged.
    pub fn output_is_silent(&self, channel: usize) -> bool {
        self.output_silence.get(channel).copied().unwrap_or(true)
    }

    /// Flags an output channel silent, or not. A node may flag a channel silent only if it wrote
    /// zeros there: the nodes it feeds then skip it.
    pub fn set_output_silent(&mut self, channel: usize, silent: bool) {
        if let Some(flag) = self.output_silence.get_mut(channel) {
            *flag = silent;
        }
    }

    /// This node's events in this sub-block, sorted by the sample each is due at: at one sample the
    /// note offs first, then the rest; within each group, the events of the node's timeline
    /// sources before the controller's, each in the order it came
    /// ([ADR-0027](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0027-timeline-events-from-sources-on-nodes.md)).
    /// A controller's event due before the sub-block, late, has offset 0, and keeps its place among
    /// the controller's others by its sample, so a note played and released late still comes on,
    /// then off.
    pub fn events(&self) -> &'a [Event] {
        self.events
    }

    /// The transport at this node's input, for this sub-block.
    pub fn transport(&self) -> Transport {
        self.transport
    }
}

/// Where a channel starts in a region whose channels lie every `stride` samples.
fn channel_start(channel: usize, stride: usize) -> Option<usize> {
    channel.checked_mul(stride)
}
