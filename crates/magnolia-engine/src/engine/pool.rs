//! The buffer pool: every channel buffer the processor works in, one pool for the whole engine.
//!
//! It has two regions. The input region holds the playback channels, then each node's input
//! channels; the output region holds the capture channels, then each node's output channels. A
//! node reads its inputs from one region and writes its outputs to the other, two disjoint slices,
//! so no `unsafe` is needed. A node's channels are consecutive in each region. Each channel takes
//! `stride` samples: the largest block, rounded up to 16 floats. Every channel has a silence flag.
//!
//! Compiled graphs name channels by index. The controller sends a larger pool only when a graph
//! needs more channels than the pool holds; the processor sends the old one to the collector.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use super::compiled::Channels;
use super::memory::written_slice;

/// The processor's channel buffers and their silence flags.
#[derive(Debug)]
pub(crate) struct BufferPool {
    pub(crate) stride: usize,
    pub(crate) inputs: Box<[f32]>,
    pub(crate) input_silence: Box<[bool]>,
    pub(crate) outputs: Box<[f32]>,
    pub(crate) output_silence: Box<[bool]>,
}

impl BufferPool {
    /// A pool of `input_channels` and `output_channels` channels of `stride` samples, every page
    /// of it written, every channel silent. Built off the real-time thread.
    pub(crate) fn new(stride: usize, input_channels: usize, output_channels: usize) -> Self {
        // A size past the address space cannot be asked for: the controller sizes pools from the
        // nodes it holds. Saturated, it would fail to allocate, off the real-time thread.
        let samples = |channels: usize| channels.saturating_mul(stride);
        Self {
            stride,
            inputs: written_slice(samples(input_channels), 0.0),
            input_silence: written_slice(input_channels, true),
            outputs: written_slice(samples(output_channels), 0.0),
            output_silence: written_slice(output_channels, true),
        }
    }

    pub(crate) fn input_channels(&self) -> usize {
        self.input_silence.len()
    }

    pub(crate) fn output_channels(&self) -> usize {
        self.output_silence.len()
    }
}

/// The first `frames` samples of a channel of a region.
pub(crate) fn channel(region: &[f32], stride: usize, channel: usize, frames: usize) -> &[f32] {
    channel
        .checked_mul(stride)
        .and_then(|start| region.get(start..))
        .and_then(|samples| samples.get(..frames))
        .unwrap_or(&[])
}

/// The first `frames` samples of a channel of a region, to write.
pub(crate) fn channel_mut(
    region: &mut [f32],
    stride: usize,
    channel: usize,
    frames: usize,
) -> &mut [f32] {
    channel
        .checked_mul(stride)
        .and_then(|start| region.get_mut(start..))
        .and_then(|samples| samples.get_mut(..frames))
        .unwrap_or_default()
}

/// Every sample of consecutive channels of a region, each channel `stride` long.
pub(crate) fn channels(region: &[f32], stride: usize, channels: Channels) -> &[f32] {
    sample_range(stride, channels)
        .and_then(|(start, end)| region.get(start..end))
        .unwrap_or(&[])
}

/// Every sample of consecutive channels of a region, to write.
pub(crate) fn channels_mut(region: &mut [f32], stride: usize, channels: Channels) -> &mut [f32] {
    sample_range(stride, channels)
        .and_then(|(start, end)| region.get_mut(start..end))
        .unwrap_or_default()
}

/// The silence flags of consecutive channels.
pub(crate) fn flags(silence: &[bool], channels: Channels) -> &[bool] {
    channels
        .first
        .checked_add(channels.count)
        .and_then(|end| silence.get(channels.first..end))
        .unwrap_or(&[])
}

/// The silence flags of consecutive channels, to write.
pub(crate) fn flags_mut(silence: &mut [bool], channels: Channels) -> &mut [bool] {
    channels
        .first
        .checked_add(channels.count)
        .and_then(|end| silence.get_mut(channels.first..end))
        .unwrap_or_default()
}

fn sample_range(stride: usize, channels: Channels) -> Option<(usize, usize)> {
    let start = channels.first.checked_mul(stride)?;
    let length = channels.count.checked_mul(stride)?;
    Some((start, start.checked_add(length)?))
}
