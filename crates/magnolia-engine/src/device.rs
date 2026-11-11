//! What a backend lends the processor for one period: [`DeviceBuffers`].

// The processor calls this module's trait in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

/// The audio device's buffers for one period, as a backend lends them to
/// [`Processor::process`](crate::engine::Processor::process).
///
/// The device-less backend ([`OfflineBackend`](crate::offline::OfflineBackend)) implements it for
/// tests, the JACK backend for the audio server. The processor reads it on the real-time thread,
/// so an implementation must not allocate, lock or block. A channel out of range, or a buffer
/// shorter than [`frames`](Self::frames), is never a panic: the processor takes such a capture
/// channel as silence and leaves such a playback channel as it is
/// ([ADR-0022](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0022-a-panic-on-the-real-time-path-gives-silence.md)).
pub trait DeviceBuffers {
    /// The number of frames in this period.
    fn frames(&self) -> usize;

    /// The number of capture channels the device offers.
    fn capture_channels(&self) -> usize;

    /// The number of playback channels the device offers.
    fn playback_channels(&self) -> usize;

    /// A capture channel's samples for this period; empty for a channel out of range.
    fn capture(&self, channel: usize) -> &[f32];

    /// A playback channel's samples for this period, to write; empty for a channel out of range.
    fn playback(&mut self, channel: usize) -> &mut [f32];
}
