//! The delay line: a node that delays each of its channels by the same whole number of samples.
//! The compiler puts one where paths of different latencies meet (`compensation.rs`); the test
//! node [`TestLatency`](crate::nodes::TestLatency) is one that declares its delay as a latency.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use super::memory::written_slice;
use crate::node::{Node, ProcessContext, ProcessStatus};

/// Delays each input channel by `delay` samples into the output channel of the same number.
///
/// Its output does not depend on how blocks are cut. It sleeps once its inputs have been silent
/// for its delay and the whole sub-block, its output then all zeros and flagged silent; its
/// history is then all zeros too, so waking needs nothing. It declares no latency: the compiler
/// places it from the latencies of the nodes around it, and never reads its own.
pub(crate) struct DelayLine {
    channels: usize,
    delay: usize,
    /// The last `delay` input samples of each channel, channel after channel: a ring per channel,
    /// all turning together.
    history: Box<[f32]>,
    /// Where each ring holds its oldest sample, the next to come out.
    position: usize,
    /// How many samples every input has been silent for, up to the end of the last sub-block. A
    /// sub-block with any input sounding counts as sounding to its end.
    silent_for: u64,
}

impl DelayLine {
    /// A delay line of `channels` channels and `delay` samples, its history silent, every page of
    /// it written (`memory.rs`). Built off the real-time thread.
    pub(crate) fn new(channels: usize, delay: usize) -> Self {
        Self {
            channels,
            delay,
            history: written_slice(channels.saturating_mul(delay), 0.0),
            position: 0,
            // A silent history is as if the inputs had always been silent.
            silent_for: u64::MAX,
        }
    }
}

impl Node for DelayLine {
    fn input_channels(&self) -> usize {
        self.channels
    }

    fn output_channels(&self) -> usize {
        self.channels
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let frames = context.frames();
        let mut every_input_silent = true;
        let mut end = self.position;
        for channel in 0..self.channels {
            every_input_silent &= context.input_is_silent(channel);
            let input = context.input(channel);
            let ring = ring_mut(&mut self.history, channel, self.delay);
            end = delay_through(ring, self.position, input, context.output_mut(channel));
        }
        self.position = end;
        self.silent_for = if every_input_silent {
            self.silent_for.saturating_add(frames as u64)
        } else {
            0
        };
        // The sub-block's output is the input from `delay` samples before each of its samples:
        // silent if the inputs have been silent since `delay` samples before its start.
        let output_silent = self.silent_for >= (self.delay as u64).saturating_add(frames as u64);
        for channel in 0..self.channels {
            context.set_output_silent(channel, output_silent);
        }
        if output_silent {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}

/// One channel's ring in `history`.
fn ring_mut(history: &mut [f32], channel: usize, delay: usize) -> &mut [f32] {
    channel
        .checked_mul(delay)
        .and_then(|start| Some(start..start.checked_add(delay)?))
        .and_then(|range| history.get_mut(range))
        .unwrap_or_default()
}

/// Writes `input` delayed by the ring's length into `output`: each output sample is the ring's
/// oldest, which the input sample then replaces. The ring's oldest sample is at `position`; returns
/// where it is after the block. An empty ring delays nothing.
fn delay_through(ring: &mut [f32], position: usize, input: &[f32], output: &mut [f32]) -> usize {
    if ring.is_empty() {
        for (output, input) in output.iter_mut().zip(input) {
            *output = *input;
        }
        return position;
    }
    let mut position = position;
    let mut done = 0;
    while done < input.len() {
        // The samples up to the ring's end, or to the block's.
        let stretch = ring
            .len()
            .saturating_sub(position)
            .min(input.len().saturating_sub(done));
        let (Some(output), Some(oldest), Some(input)) = (
            output.get_mut(done..done.saturating_add(stretch)),
            ring.get_mut(position..position.saturating_add(stretch)),
            input.get(done..done.saturating_add(stretch)),
        ) else {
            break;
        };
        // Unreachable, like the `break` above: the position stays within the ring, and `done`
        // within the block. Either would otherwise loop forever or panic.
        if stretch == 0 {
            break;
        }
        for ((output, oldest), input) in output.iter_mut().zip(oldest).zip(input) {
            *output = *oldest;
            *oldest = *input;
        }
        done = done.saturating_add(stretch);
        position = position.saturating_add(stretch);
        if position >= ring.len() {
            position = 0;
        }
    }
    position
}
