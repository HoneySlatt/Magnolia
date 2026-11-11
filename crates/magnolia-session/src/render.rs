//! The offline render: a session played from its start, block after block, on the caller's
//! thread, into a WAV file.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use magnolia_engine::disk::{DiskConfig, PrimeError};
use magnolia_engine::engine::TransportError;
use magnolia_engine::offline::OfflineBackend;
#[cfg(doc)]
use magnolia_engine::stats::PlayStart;
use magnolia_engine::time::SamplePosition;
use magnolia_engine::time::SampleRate;
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
    let tail = u64::try_from(tail_after_the_last_clip(rate).0).unwrap_or_default();
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

/// Renders `session` as [`render`] does into a new WAV file at `path`, which must not exist, so
/// that the file appears whole or not at all: it renders into `.<name>.partial` beside it, syncs
/// it to disk, then renames it to `path`. A render that fails removes its partial file.
///
/// It refuses a `path` that exists. A file made at `path` by another program during the render
/// would be replaced by the rename; the check before it is all that guards against that.
///
/// # Errors
///
/// [`RenderError::OutputExists`] for a file in the way; [`RenderError::Output`] when the partial
/// file cannot be made, synced or renamed, or exists already from a render that was killed;
/// otherwise what [`render`] refuses.
pub fn render_to_file(
    session: &Session,
    block_frames: usize,
    path: &Path,
) -> Result<RenderReport, RenderError> {
    if path.symlink_metadata().is_ok() {
        return Err(RenderError::OutputExists(path.to_path_buf()));
    }
    let partial = partial_path(path)?;
    let file = File::create_new(&partial).map_err(output_error(&partial))?;
    let rendered = render_and_sync(session, block_frames, file, &partial).and_then(|report| {
        fs::rename(&partial, path).map_err(output_error(path))?;
        Ok(report)
    });
    if rendered.is_err() {
        // The error reported is the render's; a partial file that cannot be removed stays, and
        // the next render into `path` says so.
        let _ = fs::remove_file(&partial);
    }
    rendered
}

/// `.<name>.partial` beside `path`.
fn partial_path(path: &Path) -> Result<PathBuf, RenderError> {
    let name = path.file_name().ok_or_else(|| RenderError::Output {
        path: path.to_path_buf(),
        error: io::Error::new(io::ErrorKind::InvalidInput, "the path names no file"),
    })?;
    let mut partial = OsString::from(".");
    partial.push(name);
    partial.push(".partial");
    Ok(path.with_file_name(partial))
}

/// Renders into `file`, at `path`, and syncs it to disk.
fn render_and_sync(
    session: &Session,
    block_frames: usize,
    file: File,
    path: &Path,
) -> Result<RenderReport, RenderError> {
    let mut writer = BufWriter::new(file);
    let report = render(session, block_frames, &mut writer)?;
    let file = writer
        .into_inner()
        .map_err(|error| output_error(path)(error.into_error()))?;
    file.sync_all().map_err(output_error(path))?;
    Ok(report)
}

/// An error of the file at `path`.
fn output_error(path: &Path) -> impl FnOnce(io::Error) -> RenderError + use<> {
    let path = path.to_path_buf();
    move |error| RenderError::Output { path, error }
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
    /// The file to render into exists already: a render never writes over a file.
    OutputExists(PathBuf),
    /// The file to render into could not be made, completed or put in place.
    Output {
        /// The file.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
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
            Self::OutputExists(path) => write!(
                formatter,
                "{} exists already; a render never writes over a file",
                path.display()
            ),
            Self::Output { path, error } => write!(formatter, "{}: {error}", path.display()),
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
            Self::Output { error, .. } => Some(error),
            Self::BlockOutOfRange { .. } | Self::PlayDidNotStart | Self::OutputExists(_) => None,
        }
    }
}

/// The tail a render plays, and a play by default, after the session's last clip: one second, so
/// that releases and reverbs ring out.
pub fn tail_after_the_last_clip(rate: SampleRate) -> SamplePosition {
    SamplePosition(i64::from(rate.hertz()))
}
