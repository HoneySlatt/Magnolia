//! Audio clips streamed from disk: the disk reader, which reads WAV files ahead of the playhead on
//! a thread of its own, and the audio players, the nodes that play what it read
//! ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
//!
//! Each audio track has one **stream**: a ring of slots, each holding one block of
//! [`BLOCK_FRAMES`] frames of the track's clips mixed together, and one atomic word that says which
//! block of which generation it holds, and whether it is silent. Block `b`, the frames from
//! `b × BLOCK_FRAMES`, lives in slot `b mod N`.
//!
//! - The [`DiskReader`] owns the clips. [`DiskReader::locate`] starts a new **generation** from a
//!   timeline sample and gives its [`Cue`](crate::engine::Cue); the reader fills every stream from
//!   there, then keeps each one [`DiskConfig::read_ahead`] frames ahead of its player, woken when a
//!   player runs low.
//!   It renders a block from zeros, then each clip's frames times its gain; a clip that starts
//!   before the cue fades in over the 5 ms after it.
//! - The [`AudioPlayer`] of a track, a node of the engine's graph, copies on the real-time thread
//!   the frames of the play's audible region from the slots its play's generation holds, fades out
//!   over the stop's last 5 ms, and writes silence elsewhere. A frame it lacks is silence, counted
//!   in [`EngineStats::disk_underruns`](crate::stats::EngineStats::disk_underruns); it never waits.
//!
//! A play reads a generation through its cue: `Controller::play(cue)`, sent at once after the
//! locate; it starts once the reader has published the cue primed. While an older play still
//! rolls, the reader leaves alone the slots it may still read, so that a play while rolling fades
//! out over its own audio. Generations are the engine's, so that a reader made again never reuses
//! one. `docs/ARCHITECTURE.md` describes the flow.

// The players run on the real-time thread, and the reader must never crash on a bad file: nothing
// here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod fill;
mod player;
mod reader;
mod stream;

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use player::AudioPlayer;
pub use reader::{DiskReader, DiskStats, PrimeError, wake_disk_reader};

use crate::time::{SamplePosition, SampleRate};
use crate::wav::{WavError, WavInfo, read_info};

/// The frames of one block of a stream: what one slot holds.
pub const BLOCK_FRAMES: usize = 4_096;

/// A WAV file opened once for the disk reader, which reads its frames where it needs them.
#[derive(Debug)]
pub struct AudioFile {
    path: PathBuf,
    file: File,
    info: WavInfo,
}

impl AudioFile {
    /// Opens a WAV file and reads its header ([`read_info`]).
    ///
    /// # Errors
    ///
    /// [`WavError::Io`] when the file cannot be opened or read; the other [`WavError`]s for a file
    /// that is not a WAV file Magnolia reads.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, WavError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(WavError::Io)?;
        let info = read_info(&mut BufReader::new(&file))?;
        Ok(Arc::new(Self { path, file, info }))
    }

    /// The path it was opened at.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What its header says of its samples.
    pub fn info(&self) -> &WavInfo {
        &self.info
    }
}

/// An audio clip: frames of a file placed on the timeline.
#[derive(Clone, Debug)]
pub struct AudioClip {
    /// The file its frames come from.
    pub file: Arc<AudioFile>,
    /// The timeline sample its first frame plays at.
    pub start: SamplePosition,
    /// The file's frame its first frame is.
    pub file_offset: u64,
    /// How many frames it plays. Frames past the file's end play as silence, counted in
    /// [`DiskStats::unread_frames`].
    pub length: u64,
    /// What its samples are multiplied by.
    pub gain: f32,
}

impl AudioClip {
    /// The timeline sample after its last frame; `None` past `i64`.
    fn end(&self) -> Option<i64> {
        i64::try_from(self.length)
            .ok()
            .and_then(|length| self.start.0.checked_add(length))
    }
}

/// An audio track: its channels, and clips that do not overlap.
#[derive(Clone, Debug)]
pub struct AudioTrack {
    /// The channels of the track, of each of its files, and of its player's outputs: 1 to 1,024.
    pub channels: u16,
    /// Its clips, in any order; the reader sorts them by start.
    pub clips: Vec<AudioClip>,
}

/// What a disk reader is made with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskConfig {
    /// The engine's rate, at which every file must be.
    pub sample_rate: SampleRate,
    /// How many frames ahead of its player the reader keeps each stream: 10 seconds by default.
    /// A player that holds less than half of it ready wakes the reader. From
    /// [`DiskConfig::MIN_READ_AHEAD`] to [`DiskConfig::MAX_READ_AHEAD`], and at least twice the
    /// longest period, or the players underrun. Each stream holds it in memory: 10 seconds of
    /// stereo at 48 kHz take 3.9 MB.
    pub read_ahead: u64,
}

impl DiskConfig {
    /// The shortest read-ahead: two blocks.
    pub const MIN_READ_AHEAD: u64 = 2 * BLOCK_FRAMES as u64;
    /// The longest read-ahead: 2^28 frames, 93 minutes at 48 kHz.
    pub const MAX_READ_AHEAD: u64 = 1 << 28;

    /// A configuration that reads 10 seconds ahead.
    pub fn new(sample_rate: SampleRate) -> Self {
        Self {
            sample_rate,
            read_ahead: u64::from(sample_rate.hertz()).saturating_mul(10),
        }
    }
}

/// The furthest from the timeline's zero a clip may lie, either way: 2^41 frames, 530 days at
/// 48 kHz. A slot's word holds its block's number in 30 bits.
const TIMELINE_LIMIT: i64 = 1 << 41;

/// Why a disk reader could not be made.
#[derive(Debug)]
pub enum DiskError {
    /// The read-ahead is outside [`DiskConfig::MIN_READ_AHEAD`] to [`DiskConfig::MAX_READ_AHEAD`].
    ReadAheadOutOfRange(u64),
    /// A track's channels are 0 or above 1,024.
    ChannelsOutOfRange {
        /// The track, in the order given.
        track: usize,
        /// Its channels.
        channels: u16,
    },
    /// A clip's file is not at the engine's rate.
    RateMismatch {
        /// The track, in the order given.
        track: usize,
        /// The clip, in the track's order given.
        clip: usize,
        /// The file.
        path: PathBuf,
        /// The file's rate, in hertz.
        file_rate: u32,
        /// The engine's rate, in hertz.
        engine_rate: u32,
    },
    /// A clip's file has other channels than its track.
    ChannelsMismatch {
        /// The track, in the order given.
        track: usize,
        /// The clip, in the track's order given.
        clip: usize,
        /// The file.
        path: PathBuf,
        /// The file's channels.
        file_channels: u16,
        /// The track's channels.
        track_channels: u16,
    },
    /// A clip's gain is not finite.
    GainNotFinite {
        /// The track, in the order given.
        track: usize,
        /// The clip, in the track's order given.
        clip: usize,
    },
    /// A clip lies further than 2^41 frames from the timeline's zero, either way.
    ClipOutOfRange {
        /// The track, in the order given.
        track: usize,
        /// The clip, in the track's order given.
        clip: usize,
    },
    /// Two clips of a track overlap; until Stage 2 decides how layers play, a track's clips may
    /// not.
    ClipsOverlap {
        /// The track, in the order given.
        track: usize,
        /// The earlier clip, in the track's order given.
        first: usize,
        /// The clip that starts before it ends.
        second: usize,
    },
    /// The reader's thread did not start.
    ThreadDidNotStart(io::Error),
}

impl fmt::Display for DiskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadAheadOutOfRange(frames) => write!(
                formatter,
                "the read-ahead must be {} to {} frames, not {frames}",
                DiskConfig::MIN_READ_AHEAD,
                DiskConfig::MAX_READ_AHEAD
            ),
            Self::ChannelsOutOfRange { track, channels } => write!(
                formatter,
                "audio track {track} has {channels} channels; a track has 1 to 1,024"
            ),
            Self::RateMismatch {
                track,
                clip,
                path,
                file_rate,
                engine_rate,
            } => write!(
                formatter,
                "clip {clip} of audio track {track} plays {}, at {file_rate} Hz, but the engine \
                 runs at {engine_rate} Hz",
                path.display()
            ),
            Self::ChannelsMismatch {
                track,
                clip,
                path,
                file_channels,
                track_channels,
            } => write!(
                formatter,
                "clip {clip} of audio track {track} plays {}, of {file_channels} channels, on a \
                 track of {track_channels}",
                path.display()
            ),
            Self::GainNotFinite { track, clip } => write!(
                formatter,
                "clip {clip} of audio track {track} has a gain that is not a finite number"
            ),
            Self::ClipOutOfRange { track, clip } => write!(
                formatter,
                "clip {clip} of audio track {track} lies further than 2^41 frames from the \
                 timeline's zero"
            ),
            Self::ClipsOverlap {
                track,
                first,
                second,
            } => write!(
                formatter,
                "clips {first} and {second} of audio track {track} overlap; a track's clips may \
                 not"
            ),
            Self::ThreadDidNotStart(error) => {
                write!(formatter, "the disk reader's thread did not start: {error}")
            }
        }
    }
}

impl Error for DiskError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ThreadDidNotStart(error) => Some(error),
            _ => None,
        }
    }
}

/// Checks a track against the engine, and returns its clips sorted by start, those of no frame
/// left out.
fn checked_clips(
    index: usize,
    track: &AudioTrack,
    rate: SampleRate,
) -> Result<Box<[AudioClip]>, DiskError> {
    if !(1..=1_024).contains(&track.channels) {
        return Err(DiskError::ChannelsOutOfRange {
            track: index,
            channels: track.channels,
        });
    }
    for (clip_index, clip) in track.clips.iter().enumerate() {
        let info = clip.file.info();
        if info.sample_rate != rate.hertz() {
            return Err(DiskError::RateMismatch {
                track: index,
                clip: clip_index,
                path: clip.file.path().to_path_buf(),
                file_rate: info.sample_rate,
                engine_rate: rate.hertz(),
            });
        }
        if info.channels != track.channels {
            return Err(DiskError::ChannelsMismatch {
                track: index,
                clip: clip_index,
                path: clip.file.path().to_path_buf(),
                file_channels: info.channels,
                track_channels: track.channels,
            });
        }
        if !clip.gain.is_finite() {
            return Err(DiskError::GainNotFinite {
                track: index,
                clip: clip_index,
            });
        }
        let within = |sample: i64| (-TIMELINE_LIMIT..=TIMELINE_LIMIT).contains(&sample);
        if !clip.end().is_some_and(within) || !within(clip.start.0) {
            return Err(DiskError::ClipOutOfRange {
                track: index,
                clip: clip_index,
            });
        }
    }
    let mut order: Vec<usize> = (0..track.clips.len())
        .filter(|&clip| track.clips.get(clip).is_some_and(|clip| clip.length > 0))
        .collect();
    order.sort_by_key(|&clip| track.clips.get(clip).map(|clip| clip.start));
    for pair in order.windows(2) {
        let [first, second] = *pair else { continue };
        let (Some(earlier), Some(later)) = (track.clips.get(first), track.clips.get(second)) else {
            continue;
        };
        if earlier.end().is_none_or(|end| end > later.start.0) {
            return Err(DiskError::ClipsOverlap {
                track: index,
                first,
                second,
            });
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|clip| track.clips.get(clip).cloned())
        .collect())
}
