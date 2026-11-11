//! The node contract: what every node of the engine's graph implements, today a sine and a gain,
//! from M1.6 the proxy of every plugin host
//! ([ADR-0021](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0021-the-node-contract.md)).
//!
//! The processor calls [`Node::process`] once per sub-block on the real-time thread, with a
//! [`ProcessContext`] that lends the node its input and output channels and its events. A node is
//! created and dropped off the real-time thread: the controller sends it in, the collector drops it.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

/// A node of the engine's graph.
///
/// Its channel counts are fixed for its life: a different layout is a new node, swapped in. Its
/// output must not depend on how the engine cuts blocks, since the engine splits a period into
/// sub-blocks at a swap's fade or past its largest block, and a render must equal playback to the
/// bit. It keeps denormals out of its own state: the threads that run nodes keep the default
/// floating-point mode.
pub trait Node: Send {
    /// How many input channels the node reads.
    fn input_channels(&self) -> usize;

    /// How many output channels the node writes.
    fn output_channels(&self) -> usize;

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

/// What an event does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    /// Sets one of the node's parameters, which each node numbers and documents.
    Parameter {
        /// The parameter's number.
        index: u32,
        /// Its new value.
        value: f64,
    },
}

/// What a node receives for one sub-block: its channels and its events.
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
}

impl<'a> ProcessContext<'a> {
    /// A context over a node's channels, laid out every `stride` samples in `inputs` and
    /// `outputs`, one silence flag per channel.
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
        }
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

    /// This node's events in this sub-block, sorted by offset, then by arrival.
    pub fn events(&self) -> &'a [Event] {
        self.events
    }
}

/// Where a channel starts in a region whose channels lie every `stride` samples.
fn channel_start(channel: usize, stride: usize) -> Option<usize> {
    channel.checked_mul(stride)
}
