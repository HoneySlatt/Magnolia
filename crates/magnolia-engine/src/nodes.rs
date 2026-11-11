//! The engine's own nodes: a sine to test with, and a gain.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::f64::consts::TAU;

use crate::node::{EventKind, Node, ProcessContext, ProcessStatus};
use crate::time::SampleRate;

/// A sine wave on one output, without input. It never sleeps.
///
/// Parameters: 0 is the frequency in hertz, 1 the gain. A value that is not finite is ignored.
///
/// Its phase is a whole number of 2^-64 cycles, advanced by the same whole step each sample, so
/// its output does not depend on how blocks are cut and never drifts from its own step.
#[derive(Debug)]
pub struct Sine {
    sample_rate: SampleRate,
    phase: u64,
    step: u64,
    gain: f32,
}

impl Sine {
    /// The parameter that sets the frequency, in hertz.
    pub const FREQUENCY: u32 = 0;
    /// The parameter that sets the gain.
    pub const GAIN: u32 = 1;

    /// A sine of `frequency` hertz and amplitude `gain`, starting at phase 0 on its first sample.
    pub fn new(sample_rate: SampleRate, frequency: f64, gain: f32) -> Self {
        let mut sine = Self {
            sample_rate,
            phase: 0,
            step: 0,
            gain: 0.0,
        };
        sine.set_frequency(frequency);
        if gain.is_finite() {
            sine.gain = gain;
        }
        sine
    }

    /// Sets the phase step of `frequency` hertz: the cycles per sample, modulo one, in 2^-64
    /// cycles. The product is a whole number, so the cast only saturates when `rem_euclid` rounds
    /// a tiny negative ratio up to exactly one cycle, a step one unit short.
    fn set_frequency(&mut self, frequency: f64) {
        if !frequency.is_finite() {
            return;
        }
        let cycles_per_sample = (frequency / f64::from(self.sample_rate.hertz())).rem_euclid(1.0);
        self.step = (cycles_per_sample * TWO_TO_THE_64) as u64;
    }

    fn apply(&mut self, kind: EventKind) {
        let EventKind::Parameter { index, value } = kind;
        match index {
            Self::FREQUENCY => self.set_frequency(value),
            Self::GAIN if value.is_finite() => self.gain = value as f32,
            _ => {}
        }
    }
}

/// 2^64, the number of phase units in a cycle.
const TWO_TO_THE_64: f64 = 18_446_744_073_709_551_616.0;

impl Node for Sine {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let mut events = context.events().iter().peekable();
        for (sample_index, sample) in context.output_mut(0).iter_mut().enumerate() {
            while let Some(event) = events.next_if(|event| event.offset as usize <= sample_index) {
                self.apply(event.kind);
            }
            let cycles = self.phase as f64 / TWO_TO_THE_64;
            *sample = (f64::from(self.gain) * (TAU * cycles).sin()) as f32;
            self.phase = self.phase.wrapping_add(self.step);
        }
        // An offset past the sub-block cannot come from the engine; it still takes effect.
        for event in events {
            self.apply(event.kind);
        }
        ProcessStatus::Continue
    }
}

/// A gain on any number of channels, each input to the output of the same number.
///
/// Parameter 0 is the gain; a value that is not finite is ignored. When every input is silent, it
/// writes silence, flags every output silent and sleeps.
#[derive(Debug)]
pub struct Gain {
    channels: usize,
    gain: f32,
}

impl Gain {
    /// The parameter that sets the gain.
    pub const GAIN: u32 = 0;

    /// A gain of `gain` on `channels` channels.
    pub fn new(channels: usize, gain: f32) -> Self {
        Self {
            channels,
            gain: if gain.is_finite() { gain } else { 0.0 },
        }
    }

    /// The gain after `kind`, from `gain`.
    fn after(gain: f32, kind: EventKind) -> f32 {
        let EventKind::Parameter { index, value } = kind;
        if index == Self::GAIN && value.is_finite() {
            value as f32
        } else {
            gain
        }
    }
}

impl Node for Gain {
    fn input_channels(&self) -> usize {
        self.channels
    }

    fn output_channels(&self) -> usize {
        self.channels
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let events = context.events();
        let frames = context.frames();
        let mut every_input_silent = true;
        for channel in 0..self.channels {
            let silent = context.input_is_silent(channel);
            let input = context.input(channel);
            let output = context.output_mut(channel);
            if silent {
                output.fill(0.0);
            } else {
                every_input_silent = false;
                // The gain changes at each event's sample: scale each stretch between two events.
                let mut gain = self.gain;
                let mut start = 0;
                for event in events {
                    // `max` then `min` rather than `clamp`, which panics on bounds out of order.
                    let end = (event.offset as usize).max(start).min(frames);
                    scale(input, output, start, end, gain);
                    gain = Self::after(gain, event.kind);
                    start = end;
                }
                scale(input, output, start, frames, gain);
            }
            context.set_output_silent(channel, silent);
        }
        self.gain = events
            .iter()
            .fold(self.gain, |gain, event| Self::after(gain, event.kind));
        if every_input_silent {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}

/// Writes `input` times `gain` into `output`, from sample `start` to `end`.
fn scale(input: &[f32], output: &mut [f32], start: usize, end: usize, gain: f32) {
    let (Some(input), Some(output)) = (input.get(start..end), output.get_mut(start..end)) else {
        return;
    };
    for (output, input) in output.iter_mut().zip(input) {
        *output = input * gain;
    }
}
