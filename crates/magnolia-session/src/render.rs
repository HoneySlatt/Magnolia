//! The offline render: a session played from its start, block after block, on the caller's
//! thread, into a WAV file.

use std::error::Error;
use std::fmt;
use std::io::{Seek, Write};
use std::time::Duration;

use magnolia_engine::disk::{DiskConfig, PrimeError};
use magnolia_engine::engine::TransportError;
use magnolia_engine::offline::OfflineBackend;
#[cfg(doc)]
use magnolia_engine::stats::PlayStart;
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{SampleFormat, WavError, WavWriter};

use crate::build::{BuildError, BuiltSession, ReaderMode};
use crate::load::Session;

/// What an offline render did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderReport {
    /// The frames written: from the engine sample where the output reaches the play's first
    /// sample, as many as the session's last sample and one second of tail after it.
    pub frames: u64,
    /// The frames the audio players lacked, which played as silence: 0, as the reader fills before
    /// each block.
    pub underruns: u64,
    /// The session's largest compensation, in samples: how far the play pre-rolled before its
    /// first sample reached the output.
    pub largest_compensation: u64,
}

/// Renders `session` offline into `writer`, as a WAV file of 32-bit floats with the master's
/// channels at the session's rate.
///
/// It builds the session with a disk reader without a thread, locates it at sample 0 and plays
/// from there, in blocks of `block_frames` frames, the reader filled before each. It writes from
/// the engine sample where the output reaches timeline sample 0, once the largest compensation has
/// pre-rolled ([`PlayStart::audible_at`]), up to the session's last sample plus one second of
/// tail, which holds the releases of notes and what the delays still hold. So a render of a
/// session is the same to the bit whatever its block length, and the same as a play from sample 0,
/// from where its output reaches that sample on.
///
/// # Errors
///
/// [`RenderError::BlockOutOfRange`] for a block of 0 frames or longer than half the disk reader's
/// read-ahead, 5 seconds, past which the reader could not keep up; otherwise what building,
/// playing or writing refused.
pub fn render<W: Write + Seek>(
    session: &Session,
    block_frames: usize,
    writer: W,
) -> Result<RenderReport, RenderError> {
    let rate = session.sample_rate();
    let longest_block = DiskConfig::new(rate).read_ahead / 2;
    if block_frames == 0 || block_frames as u64 > longest_block {
        return Err(RenderError::BlockOutOfRange {
            frames: block_frames,
            longest: longest_block,
        });
    }
    let BuiltSession {
        mut controller,
        processor,
        mut reader,
        last_sample,
    } = session.build(ReaderMode::Offline)?;
    let cue = reader.locate(SamplePosition(0));
    // A reader without a thread fills, then answers without waiting.
    reader.wait_primed(cue, Duration::ZERO)?;
    controller.play(cue)?;

    let channels = session.master_channels();
    let mut backend = OfflineBackend::new(processor);
    let mut wav = WavWriter::new(writer, SampleFormat::Float32, channels, rate.hertz())?;
    let tail = u64::from(rate.hertz());
    let wanted = u64::try_from(last_sample.0)
        .unwrap_or_default()
        .saturating_add(tail);
    let mut written = 0;
    let mut block_start = 0;
    let mut audible_at = None;
    let mut interleaved = Vec::with_capacity(block_frames.saturating_mul(usize::from(channels)));
    while written < wanted {
        reader.fill();
        let played = backend.play_period(block_frames, &[]);
        let audible = match audible_at {
            Some(audible) => audible,
            None => {
                // The play is applied at the first period, which finds its cue primed.
                let start = controller
                    .stats()
                    .play_start()
                    .ok_or(RenderError::PlayDidNotStart)?;
                let audible = u64::try_from(start.audible_at.0).unwrap_or_default();
                audible_at = Some(audible);
                audible
            }
        };
        let block_end = block_start + block_frames as u64;
        if block_end > audible {
            let first = audible.saturating_sub(block_start) as usize;
            let frames = (block_frames - first).min((wanted - written) as usize);
            interleaved.clear();
            for frame in first..first + frames {
                interleaved.extend(played.iter().map(|channel| channel[frame]));
            }
            wav.write_frames(&interleaved)?;
            written += frames as u64;
        }
        block_start = block_end;
    }
    wav.finish()?;
    Ok(RenderReport {
        frames: written,
        underruns: controller.stats().disk_underruns(),
        largest_compensation: audible_at.unwrap_or_default(),
    })
}

/// Why a render failed.
#[derive(Debug)]
pub enum RenderError {
    /// The block is 0 frames, or longer than the longest the disk reader keeps up with.
    BlockOutOfRange {
        /// The block asked for.
        frames: usize,
        /// The longest block: half the reader's read-ahead.
        longest: u64,
    },
    /// The session could not be built into an engine.
    Build(BuildError),
    /// The disk reader could not prime the play's cue.
    Prime(PrimeError),
    /// The transport refused the play.
    Transport(TransportError),
    /// The play did not start at the first block, as it must once its cue is primed.
    PlayDidNotStart,
    /// The WAV file could not be written.
    Wav(WavError),
}

impl From<BuildError> for RenderError {
    fn from(error: BuildError) -> Self {
        Self::Build(error)
    }
}

impl From<PrimeError> for RenderError {
    fn from(error: PrimeError) -> Self {
        Self::Prime(error)
    }
}

impl From<TransportError> for RenderError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<WavError> for RenderError {
    fn from(error: WavError) -> Self {
        Self::Wav(error)
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockOutOfRange { frames, longest } => write!(
                formatter,
                "a render's block holds 1 to {longest} frames, half the disk reader's read-ahead, \
                 not {frames}"
            ),
            Self::Build(error) => write!(formatter, "{error}"),
            Self::Prime(error) => write!(formatter, "the disk reader did not prime: {error}"),
            Self::Transport(error) => write!(formatter, "the play was refused: {error}"),
            Self::PlayDidNotStart => {
                formatter.write_str("the play did not start at the render's first block")
            }
            Self::Wav(error) => write!(formatter, "the render could not be written: {error}"),
        }
    }
}

impl Error for RenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Prime(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Wav(error) => Some(error),
            Self::BlockOutOfRange { .. } | Self::PlayDidNotStart => None,
        }
    }
}
