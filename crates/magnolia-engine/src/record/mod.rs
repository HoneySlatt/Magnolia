//! Recording: the capture tap, which copies the capture channels of armed tracks on the real-time
//! thread, and the recorder, which writes them into WAV takes that survive a crash
//! ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md),
//! [ADR-0031](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0031-the-round-trip-reported-and-takes-placed-by-it.md),
//! [ADR-0032](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0032-recording-by-a-capture-tap-of-the-processor.md)).
//!
//! - [`Recorder::start`] takes the recording folder's lock, recovers what a crash left there
//!   ([`recover`]), and starts the recorder's thread and its sync threads.
//! - [`Recorder::tap`] makes an armed track's [`Tap`]: its rings, and its first take's file, made
//!   ahead. [`Controller::arm`](crate::engine::Controller::arm) arms it at the next commit, and
//!   [`Controller::disarm`](crate::engine::Controller::disarm) disarms it: arming is an edit, like
//!   the graph's.
//! - At each period's start, after the transport's commands and before the graph, in a
//!   `catch_unwind` of its own, the processor's capture tap copies the device's capture frames of
//!   each take's window into the track's rings, and wakes the recorder through [`wake_recorder`]
//!   once enough frames wait or a take starts or ends.
//! - The recorder's thread writes each take's frames straight to its file, a gap as zeros, and has
//!   it synced on its sync threads: after each second of audio, and at the take's end.
//!
//! A take's first frame is the frame captured a round trip after the output played its first
//! timeline sample; the round trip is the device's capture and playback latencies, read at the
//! play's start, plus the recorder's extra latency ([`RecordConfig::extra_latency`]). A change of
//! the device's latency during a take ends it. `docs/ARCHITECTURE.md` (*Recording*) describes the
//! flow.

// The tap runs on the real-time thread, and the recorder and recovery must never crash on a file
// they meet: nothing here may panic (ADR-0022). The lints cover every submodule.
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod files;
mod marks;
mod recorder;
mod recover;
mod syncer;
pub(crate) mod tap;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

pub use files::TakeName;
pub use recorder::{Recorder, RecorderStats, RingFill, TakeEnd, TakeSummary};
pub use recover::{RecoveredTake, Recovery, recover};
pub use tap::{MAX_WINDOWS, Tap, wake_recorder};

use crate::time::SampleRate;
use crate::wav::SampleFormat;

/// The originator in every take's `bext` chunk, by which recovery knows Magnolia's takes.
const ORIGINATOR: &str = "Magnolia";

/// How a take's samples are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TakeFormat {
    /// 32-bit floats: the device's samples, bit for bit.
    #[default]
    Float32,
    /// 24-bit integers: a quarter smaller, each sample rounded to 24 bits.
    Int24,
}

impl TakeFormat {
    fn sample_format(self) -> SampleFormat {
        match self {
            Self::Float32 => SampleFormat::Float32,
            Self::Int24 => SampleFormat::Int24,
        }
    }
}

/// What a recorder is made with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordConfig {
    /// The recording folder, which must exist: the takes' files, and the lock file
    /// `.magnolia-recording.lock`.
    pub folder: PathBuf,
    /// The engine's rate, which the takes are written at.
    pub sample_rate: SampleRate,
    /// How the takes' samples are written: 32-bit floats by default.
    pub format: TakeFormat,
    /// How many frames each track's ring holds, between the tap and the recorder's thread: 4
    /// seconds by default, so that a stall of the disk that long drops nothing. From
    /// [`RecordConfig::MIN_RING_FRAMES`] to [`RecordConfig::MAX_RING_FRAMES`], and longer than the
    /// device's longest period, or every period drops its frames.
    pub ring_frames: u64,
    /// The recorder's own latency beyond the device's, in frames: the extra loopback latency
    /// measured with `jack_iodelay`, 0 by default
    /// ([ADR-0031](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)).
    pub extra_latency: u32,
    /// How many threads sync the takes, so that several files sync in parallel: 4 by default, from
    /// 1 to [`RecordConfig::MAX_SYNC_THREADS`]. Measured with 32 armed tracks on the high tier's
    /// NVMe, 8 threads slowed one another and stalled the recorder's writes up to tens of
    /// milliseconds, 4 or fewer about one (`benches/recorder.rs`).
    pub sync_threads: usize,
}

impl RecordConfig {
    /// The shortest ring: twice the frames past which a tap wakes the recorder.
    pub const MIN_RING_FRAMES: u64 = 4_096;
    /// The longest ring: 2^24 frames, nearly 6 minutes at 48 kHz.
    pub const MAX_RING_FRAMES: u64 = 1 << 24;
    /// The most sync threads.
    pub const MAX_SYNC_THREADS: usize = 64;

    /// A configuration that records 32-bit floats into `folder` at `sample_rate`, through rings of
    /// 4 seconds, without extra latency, synced by 4 threads.
    pub fn new(folder: impl Into<PathBuf>, sample_rate: SampleRate) -> Self {
        Self {
            folder: folder.into(),
            sample_rate,
            format: TakeFormat::Float32,
            ring_frames: u64::from(sample_rate.hertz()).saturating_mul(4),
            extra_latency: 0,
            sync_threads: 4,
        }
    }

    fn check(&self) -> Result<(), RecordError> {
        if !(Self::MIN_RING_FRAMES..=Self::MAX_RING_FRAMES).contains(&self.ring_frames) {
            return Err(RecordError::RingOutOfRange(self.ring_frames));
        }
        if !(1..=Self::MAX_SYNC_THREADS).contains(&self.sync_threads) {
            return Err(RecordError::SyncThreadsOutOfRange(self.sync_threads));
        }
        Ok(())
    }
}

/// A track to arm, as [`Recorder::tap`] takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordTrack {
    /// The track's number, which its takes' files start with.
    pub number: u32,
    /// The track's name, which its takes' files hold, sanitized: letters and digits of any script,
    /// `-` and `_` kept, every other character turned into `_`, at most 64 characters.
    pub name: String,
    /// The engine's capture channels it records, one per channel of its takes: 1 to 1,024.
    pub capture_channels: Vec<usize>,
}

/// Why a recorder, a tap or a recovery failed.
#[derive(Debug)]
pub enum RecordError {
    /// The ring is outside [`RecordConfig::MIN_RING_FRAMES`] to
    /// [`RecordConfig::MAX_RING_FRAMES`]: this many frames.
    RingOutOfRange(u64),
    /// The sync threads are 0 or more than [`RecordConfig::MAX_SYNC_THREADS`].
    SyncThreadsOutOfRange(usize),
    /// A track to arm has no capture channel, or more than 1,024.
    ChannelsOutOfRange {
        /// The track's number.
        track: u32,
        /// Its channels.
        channels: usize,
    },
    /// A track's ring would take more than 1 GiB: its frames times its channels.
    RingTooLarge {
        /// The track's number.
        track: u32,
        /// The bytes it would take.
        bytes: usize,
    },
    /// Another recorder, or a recovery, holds the folder's lock.
    FolderLocked(PathBuf),
    /// The folder could not be opened, read, locked or synced.
    Folder {
        /// The folder.
        folder: PathBuf,
        /// What failed.
        error: io::Error,
    },
    /// A take's file could not be made.
    TakeFile(io::Error),
    /// A thread of the recorder did not start.
    ThreadDidNotStart(io::Error),
}

impl fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RingOutOfRange(frames) => write!(
                formatter,
                "a track's ring must hold {} to {} frames, not {frames}",
                RecordConfig::MIN_RING_FRAMES,
                RecordConfig::MAX_RING_FRAMES
            ),
            Self::SyncThreadsOutOfRange(threads) => write!(
                formatter,
                "the recorder needs 1 to {} sync threads, not {threads}",
                RecordConfig::MAX_SYNC_THREADS
            ),
            Self::ChannelsOutOfRange { track, channels } => write!(
                formatter,
                "track {track} would record {channels} channels; a take has 1 to 1,024"
            ),
            Self::RingTooLarge { track, bytes } => write!(
                formatter,
                "track {track}'s ring would take {bytes} bytes, more than 1 GiB: shorten the ring"
            ),
            Self::FolderLocked(folder) => write!(
                formatter,
                "another recorder or recovery is using the recording folder {}",
                folder.display()
            ),
            Self::Folder { folder, error } => write!(
                formatter,
                "the recording folder {} could not be used: {error}",
                folder.display()
            ),
            Self::TakeFile(error) => write!(formatter, "a take's file could not be made: {error}"),
            Self::ThreadDidNotStart(error) => {
                write!(formatter, "a thread of the recorder did not start: {error}")
            }
        }
    }
}

impl Error for RecordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Folder { error, .. } | Self::TakeFile(error) | Self::ThreadDidNotStart(error) => {
                Some(error)
            }
            _ => None,
        }
    }
}
