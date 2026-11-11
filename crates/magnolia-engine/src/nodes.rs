//! The engine's own nodes: a sine to test with, a gain, a pure delay that declares a latency, to
//! test delay compensation with, and a test instrument that plays notes.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::f64::consts::TAU;
use std::fmt;

use crate::engine::{DelayLine, fade_frames};
use crate::node::{EventKind, Node, Note, ProcessContext, ProcessStatus};
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

    /// Sets the phase step of `frequency` hertz; a frequency that is not finite is ignored.
    fn set_frequency(&mut self, frequency: f64) {
        if frequency.is_finite() {
            self.step = phase_step(frequency, self.sample_rate);
        }
    }

    /// Applies a parameter's change; a sine ignores every other kind of event.
    fn apply(&mut self, kind: EventKind) {
        let EventKind::Parameter { index, value } = kind else {
            return;
        };
        match index {
            Self::FREQUENCY => self.set_frequency(value),
            Self::GAIN if value.is_finite() => self.gain = value as f32,
            _ => {}
        }
    }
}

/// 2^64, the number of phase units in a cycle.
const TWO_TO_THE_64: f64 = 18_446_744_073_709_551_616.0;

/// The phase step of `frequency` hertz, finite: the cycles per sample, modulo one, in 2^-64 cycles.
/// The product is a whole number, so the cast only saturates when `rem_euclid` rounds a tiny
/// negative ratio up to exactly one cycle, a step one unit short.
fn phase_step(frequency: f64, sample_rate: SampleRate) -> u64 {
    let cycles_per_sample = (frequency / f64::from(sample_rate.hertz())).rem_euclid(1.0);
    (cycles_per_sample * TWO_TO_THE_64) as u64
}

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

    /// The gain after `kind`, from `gain`. A gain ignores every event but its parameter's change.
    fn after(gain: f32, kind: EventKind) -> f32 {
        match kind {
            EventKind::Parameter { index, value } if index == Self::GAIN && value.is_finite() => {
                value as f32
            }
            _ => gain,
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

/// A pure delay of `latency` samples on any number of channels, each input to the output of the
/// same number, which declares that latency: it stands for a latent plugin, such as a lookahead
/// limiter, until plugins come (M1.6). The engine compensates it
/// ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)).
///
/// Its output does not depend on how blocks are cut. It sleeps once its inputs have been silent for
/// its latency and the whole sub-block.
pub struct TestLatency {
    line: DelayLine,
    channels: usize,
    latency: u32,
}

impl TestLatency {
    /// A delay of `latency` samples on `channels` channels, its history silent. It allocates
    /// `channels` times `latency` samples: make it off the real-time thread.
    pub fn new(channels: usize, latency: u32) -> Self {
        Self {
            line: DelayLine::new(channels, latency as usize),
            channels,
            latency,
        }
    }
}

impl fmt::Debug for TestLatency {
    /// Its channels and latency, without its history.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestLatency")
            .field("channels", &self.channels)
            .field("latency", &self.latency)
            .finish_non_exhaustive()
    }
}

impl Node for TestLatency {
    fn input_channels(&self) -> usize {
        self.channels
    }

    fn output_channels(&self) -> usize {
        self.channels
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        self.line.process(context)
    }
}

/// The voices of a [`TestSynth`].
const VOICES: usize = 16;

/// The MIDI channels.
const CHANNELS: usize = 16;

/// A test instrument that plays notes, to test MIDI with until plugins come (M1.6): no input, two
/// outputs carrying the same signal.
///
/// It has 16 voices; a note that finds none free takes the oldest. A voice plays a sine and a
/// sawtooth at half its level, at its key's pitch (key 69 at 440 Hz, equal temperament), bent by
/// its channel's pitch bend up to two semitones either way, scaled by its velocity, under a linear
/// attack and a linear release of 5 ms each. A note softer than [`TestSynth::SOFTEST`] does not
/// sound and takes no voice. A note off releases every held voice of its channel and key, whatever
/// its id; controllers and parameters are ignored.
///
/// Every change takes effect at its event's sample, its phases are whole numbers of 2^-64 cycles
/// advanced by a whole step each sample, and its envelopes move by one fixed step each sample, so
/// its output does not depend on how blocks are cut. Its state holds no decaying value: the
/// envelopes reach exactly 0. A sounding voice's gain, its envelope times its velocity, is at least
/// `SOFTEST / 240` at 48 kHz, far from subnormal numbers. When no voice sounds through a sub-block,
/// it writes silence, flags both outputs silent and sleeps.
#[derive(Debug)]
pub struct TestSynth {
    sample_rate: SampleRate,
    /// The attack's and the release's length, in samples: 5 ms, at least 1.
    envelope_frames: u32,
    voices: [Voice; VOICES],
    /// Each channel's pitch bend, from -1 to 1.
    bends: [f32; CHANNELS],
    /// How many notes have started, to tell the oldest voice.
    notes_started: u64,
}

/// One voice of a [`TestSynth`].
#[derive(Clone, Copy, Debug)]
struct Voice {
    stage: Stage,
    channel: u8,
    key: u8,
    velocity: f32,
    /// The envelope, in steps of 1/`envelope_frames`: 0 is silent, `envelope_frames` full.
    level: u32,
    /// In 2^-64 cycles.
    phase: u64,
    step: u64,
    /// When its note started, counted in notes.
    started: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Silent, free for a note.
    Idle,
    /// Rising to full, then holding.
    Held,
    /// Falling to silence.
    Released,
}

impl Voice {
    const IDLE: Voice = Voice {
        stage: Stage::Idle,
        channel: 0,
        key: 0,
        velocity: 0.0,
        level: 0,
        phase: 0,
        step: 0,
        started: 0,
    };

    /// The voice's next sample, its envelope moved one step first; `None` once it is idle.
    fn next_sample(&mut self, envelope_frames: u32) -> Option<f64> {
        match self.stage {
            Stage::Idle => return None,
            Stage::Held => self.level = self.level.saturating_add(1).min(envelope_frames),
            Stage::Released => self.level = self.level.saturating_sub(1),
        }
        let gain = f64::from(self.level) / f64::from(envelope_frames) * f64::from(self.velocity);
        let cycles = self.phase as f64 / TWO_TO_THE_64;
        let saw = 2.0 * cycles - 1.0;
        let sample = gain * ((TAU * cycles).sin() + 0.5 * saw);
        self.phase = self.phase.wrapping_add(self.step);
        if self.stage == Stage::Released && self.level == 0 {
            self.stage = Stage::Idle;
        }
        Some(sample)
    }
}

impl TestSynth {
    /// The softest velocity that sounds. A softer one would scale a voice towards subnormal
    /// numbers, which many processors handle far slower than normal ones.
    pub const SOFTEST: f32 = 1.0e-6;

    /// A synth at `sample_rate`, every voice silent, every bend at rest.
    pub fn new(sample_rate: SampleRate) -> Self {
        Self {
            sample_rate,
            envelope_frames: u32::try_from(fade_frames(sample_rate))
                .unwrap_or(u32::MAX)
                .max(1),
            voices: [Voice::IDLE; VOICES],
            bends: [0.0; CHANNELS],
            notes_started: 0,
        }
    }

    /// The phase step of `key` on `channel`, bent by the channel's bend.
    fn step_of(&self, channel: u8, key: u8) -> u64 {
        let bend = self.bends.get(usize::from(channel)).copied().unwrap_or(0.0);
        let semitones = f64::from(key) - 69.0 + 2.0 * f64::from(bend);
        phase_step(440.0 * (semitones / 12.0).exp2(), self.sample_rate)
    }

    fn apply(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn(note) => self.start(note),
            EventKind::NoteOff(note) => {
                for voice in &mut self.voices {
                    let ours = voice.channel == note.channel && voice.key == note.key;
                    if ours && voice.stage == Stage::Held {
                        voice.stage = Stage::Released;
                    }
                }
            }
            EventKind::PitchBend { channel, value } => self.bend(channel, value),
            EventKind::Controller { .. } | EventKind::Parameter { .. } => {}
        }
    }

    /// Starts a note on a free voice, or the oldest. A stolen voice rises from where it was. A note
    /// softer than [`SOFTEST`](Self::SOFTEST) does not sound.
    fn start(&mut self, note: Note) {
        let audible = note.velocity.is_finite() && note.velocity >= Self::SOFTEST;
        if usize::from(note.channel) >= CHANNELS || !audible {
            return;
        }
        let step = self.step_of(note.channel, note.key);
        let free = self
            .voices
            .iter()
            .position(|voice| voice.stage == Stage::Idle);
        let oldest = || {
            (0..VOICES).min_by_key(|&index| self.voices.get(index).map_or(0, |voice| voice.started))
        };
        let Some(voice) = free
            .or_else(oldest)
            .and_then(|index| self.voices.get_mut(index))
        else {
            return;
        };
        *voice = Voice {
            stage: Stage::Held,
            channel: note.channel,
            key: note.key,
            velocity: note.velocity.clamp(0.0, 1.0),
            level: voice.level,
            phase: 0,
            step,
            started: self.notes_started,
        };
        self.notes_started = self.notes_started.wrapping_add(1);
    }

    /// Moves a channel's pitch wheel, and the pitch of its voices with it.
    fn bend(&mut self, channel: u8, value: f32) {
        if !value.is_finite() {
            return;
        }
        let Some(bend) = self.bends.get_mut(usize::from(channel)) else {
            return;
        };
        *bend = value.clamp(-1.0, 1.0);
        for index in 0..VOICES {
            let Some(&voice) = self.voices.get(index) else {
                continue;
            };
            if voice.stage != Stage::Idle && voice.channel == channel {
                let step = self.step_of(channel, voice.key);
                if let Some(voice) = self.voices.get_mut(index) {
                    voice.step = step;
                }
            }
        }
    }

    /// The next sample of every voice together, and whether any voice sounded.
    fn next_sample(&mut self) -> (f32, bool) {
        let mut sum = 0.0;
        let mut sounded = false;
        for voice in &mut self.voices {
            if let Some(sample) = voice.next_sample(self.envelope_frames) {
                sum += sample;
                sounded = true;
            }
        }
        (sum as f32, sounded)
    }
}

impl Node for TestSynth {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        2
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let mut events = context.events().iter().peekable();
        let mut sounded = false;
        let mut outputs = context.outputs_mut();
        let first = outputs.next().unwrap_or_default();
        for (sample_index, sample) in first.iter_mut().enumerate() {
            while let Some(event) = events.next_if(|event| event.offset as usize <= sample_index) {
                self.apply(event.kind);
            }
            let (value, voice_sounded) = self.next_sample();
            *sample = value;
            sounded |= voice_sounded;
        }
        if let Some(second) = outputs.next() {
            for (copy, sample) in second.iter_mut().zip(first.iter()) {
                *copy = *sample;
            }
        }
        drop(outputs);
        // An offset past the sub-block cannot come from the engine; it still takes effect.
        for event in events {
            self.apply(event.kind);
        }
        if sounded {
            return ProcessStatus::Continue;
        }
        // No voice sounded: every sample written is zero.
        for channel in 0..2 {
            context.set_output_silent(channel, true);
        }
        ProcessStatus::Sleep
    }
}
