//! The fade of a graph swap: a raised cosine over 5 ms, which the processor reads from one table.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::f64::consts::PI;

use super::compiled::GainShape;
use super::memory::written_slice;
use crate::time::SampleRate;

/// How long a fade lasts, in milliseconds.
const FADE_MILLISECONDS: u32 = 5;

/// The fade's length in samples at a rate: 5 ms, to the nearest sample, a half up: 240 at 48 kHz,
/// 221 at 44.1 kHz.
pub(crate) fn fade_frames(rate: SampleRate) -> usize {
    // At most 768,000 · 5 + 500: no overflow.
    let thousandths = u64::from(rate.hertz()).saturating_mul(u64::from(FADE_MILLISECONDS));
    (thousandths.saturating_add(500) / 1000) as usize
}

/// The fade-in gain at each sample of a fade, from just above 0 to just below 1:
/// `0.5 - 0.5 cos(π (i + 0.5) / N)` for sample `i` of `N`. The fade-out gain at a sample is
/// `1 -` the fade-in gain there, so the two sum to exactly 1 in `f32`, and the curve is symmetric:
/// the fade-out is the fade-in read backwards.
#[derive(Debug)]
pub(crate) struct FadeTable {
    fade_in: Box<[f32]>,
}

/// The gain of one edge over a stretch of samples.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Gain<'a> {
    /// Passes the signal unchanged.
    One,
    /// Passes nothing.
    Zero,
    /// Multiplies each sample by the fade-in gain at its place.
    FadeIn(&'a [f32]),
    /// Multiplies each sample by one minus the fade-in gain at its place.
    FadeOut(&'a [f32]),
}

impl FadeTable {
    /// The table for a fade of `frames` samples.
    pub(crate) fn new(frames: usize) -> Self {
        let mut fade_in = written_slice(frames, 0.0);
        let length = frames as f64;
        for (index, gain) in fade_in.iter_mut().enumerate() {
            let middle = index as f64 + 0.5;
            *gain = (0.5 - 0.5 * (PI * middle / length).cos()) as f32;
        }
        Self { fade_in }
    }

    /// The fade-in gain at each sample of the fade, in order: what an audio clip is multiplied by
    /// where a play starts inside it. The fade-out gain at a sample is one minus it.
    pub(crate) fn fade_in(&self) -> &[f32] {
        &self.fade_in
    }

    /// An edge's gain over `frames` samples, from sample `elapsed` of its phase. Past the table, a
    /// fade-in has ended at 1 and a fade-out at 0; the processor cuts sub-blocks at a phase's end,
    /// so a fading phase never reads past it.
    pub(crate) fn gain(&self, shape: GainShape, elapsed: usize, frames: usize) -> Gain<'_> {
        let stretch = elapsed
            .checked_add(frames)
            .and_then(|end| self.fade_in.get(elapsed..end));
        match (shape, stretch) {
            (GainShape::Constant, _) => Gain::One,
            (GainShape::FadeIn, Some(gains)) => Gain::FadeIn(gains),
            (GainShape::FadeIn, None) => Gain::One,
            (GainShape::FadeOut, Some(gains)) => Gain::FadeOut(gains),
            (GainShape::FadeOut, None) => Gain::Zero,
        }
    }
}
