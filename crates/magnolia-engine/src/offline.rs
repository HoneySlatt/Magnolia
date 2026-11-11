//! The device-less backend: drives a [`Processor`] on the caller's thread, period by period, with
//! capture samples it is given, and returns what it played. The tests use it; the JACK backend is
//! the other implementation of [`DeviceBuffers`].

// The processor calls this module's `DeviceBuffers` in every period: nothing here may panic
// (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use crate::device::DeviceBuffers;
use crate::engine::Processor;

/// A backend without a device. Its periods may have any length, each different, longer than the
/// engine's largest block or not.
pub struct OfflineBackend {
    processor: Processor,
    capture: Vec<Vec<f32>>,
    playback: Vec<Vec<f32>>,
}

impl OfflineBackend {
    /// A backend for `processor`, with as many capture and playback channels as its engine.
    pub fn new(processor: Processor) -> Self {
        let (capture_channels, playback_channels) = processor.channels();
        Self {
            processor,
            capture: vec![Vec::new(); capture_channels],
            playback: vec![Vec::new(); playback_channels],
        }
    }

    /// The processor it drives.
    pub fn processor(&self) -> &Processor {
        &self.processor
    }

    /// Plays one period of `frames` frames and returns what each playback channel played.
    ///
    /// `capture` holds each capture channel's samples for the period: a channel missing, or
    /// shorter than the period, reaches the processor as it is, which takes it as silence. Each
    /// playback channel starts the period at zero.
    pub fn play_period(&mut self, frames: usize, capture: &[&[f32]]) -> &[Vec<f32>] {
        for (channel, buffer) in self.capture.iter_mut().enumerate() {
            buffer.clear();
            buffer.extend_from_slice(capture.get(channel).copied().unwrap_or_default());
        }
        for buffer in &mut self.playback {
            buffer.clear();
            buffer.resize(frames, 0.0);
        }
        let mut period = Period {
            frames,
            capture: &self.capture,
            playback: &mut self.playback,
        };
        self.processor.process(&mut period);
        &self.playback
    }

    /// Plays periods of the lengths given, one after the other, without capture, and returns what
    /// each playback channel played over all of them.
    pub fn play(&mut self, periods: &[usize]) -> Vec<Vec<f32>> {
        let mut played = vec![Vec::new(); self.playback.len()];
        for &frames in periods {
            for (all, period) in played.iter_mut().zip(self.play_period(frames, &[])) {
                all.extend_from_slice(period);
            }
        }
        played
    }
}

/// One period's buffers, lent to the processor.
struct Period<'a> {
    frames: usize,
    capture: &'a [Vec<f32>],
    playback: &'a mut [Vec<f32>],
}

impl DeviceBuffers for Period<'_> {
    fn frames(&self) -> usize {
        self.frames
    }

    fn capture_channels(&self) -> usize {
        self.capture.len()
    }

    fn playback_channels(&self) -> usize {
        self.playback.len()
    }

    fn capture(&self, channel: usize) -> &[f32] {
        self.capture
            .get(channel)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn playback(&mut self, channel: usize) -> &mut [f32] {
        self.playback
            .get_mut(channel)
            .map(Vec::as_mut_slice)
            .unwrap_or_default()
    }
}
