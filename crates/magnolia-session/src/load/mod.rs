//! Opening a test session: [`Session::open`] reads its file and checks it, opens its audio files,
//! reads its MIDI files into sequences, and resolves its routing, so that
//! [`Session::build`](crate::build) only makes nodes.

// The loader reads files from outside; a bad file must never crash Magnolia, so nothing here may
// panic.
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod midi;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use magnolia_engine::disk::{AudioClip, AudioFile, AudioTrack};
use magnolia_engine::engine::{MidiSequence, SequenceError};
use magnolia_engine::time::{SamplePosition, SampleRate, TempoMap};
use magnolia_engine::wav::WavError;

use crate::file::{
    self, FileError, Insert, Instrument, Routing, SessionFile, Track, TrackSource,
    place_audio_clip, place_midi_clip, relative_path,
};
use crate::midi_file::{self, MidiFile, MidiFileError};

/// The most bytes a session's JSON file, or one of its MIDI files, may hold: 64 MiB.
pub const MAX_FILE_BYTES: u64 = 64 << 20;

/// A test session, opened: its files checked, its audio files open, its MIDI files read into
/// sequences, its routing resolved. [`build`](Self::build) makes an engine that plays it, as many
/// times as asked; the files stay open as long as the session or an engine built from it.
#[derive(Debug)]
pub struct Session {
    pub(crate) rate: SampleRate,
    pub(crate) tempo_map: Arc<TempoMap>,
    pub(crate) master_channels: u16,
    pub(crate) master_inserts: Vec<Insert>,
    pub(crate) routing: Routing,
    /// Each track's name and what it plays, in the file's order, as `routing.tracks`.
    pub(crate) tracks: Vec<LoadedTrack>,
    pub(crate) last_sample: SamplePosition,
}

/// A track's name and what it plays, loaded.
#[derive(Debug)]
pub(crate) struct LoadedTrack {
    pub(crate) name: String,
    pub(crate) source: LoadedSource,
}

/// What a track plays, loaded.
#[derive(Debug)]
pub(crate) enum LoadedSource {
    /// Its clips, their files open, for the disk reader.
    Audio(AudioTrack),
    /// Its instrument, and its clips as one sequence.
    Midi {
        instrument: Instrument,
        sequence: Arc<MidiSequence>,
    },
}

impl Session {
    /// Opens the test session at `path`: reads it, at most [`MAX_FILE_BYTES`], and checks it
    /// ([`file::parse`]); opens each audio file once, and checks that it is at the session's rate,
    /// has its track's channels and holds every frame of its clips; reads each MIDI file once,
    /// at most `MAX_FILE_BYTES`, and makes each MIDI track's clips one sequence. Paths are relative
    /// to the session file's folder.
    ///
    /// A MIDI clip plays its whole file, every track merged, its ticks scaled from the file's
    /// division by the first tick at or after; each note off ends the earliest note of its channel
    /// and key, and a note without its note off ends at the clip's end. The file's tempo and time
    /// signatures are ignored: the session's tempo map rules.
    ///
    /// # Errors
    ///
    /// A [`LoadError`] that names the file and the field of the session at fault.
    pub fn open(path: impl AsRef<Path>) -> Result<Session, LoadError> {
        let path = path.as_ref();
        let text = read_to_string_limited(path).map_err(|error| match error {
            Limited::Read(error) => LoadError::Read {
                path: path.to_path_buf(),
                error,
            },
            Limited::TooLarge => LoadError::TooLarge {
                path: path.to_path_buf(),
            },
        })?;
        let in_file = |error| LoadError::File {
            path: path.to_path_buf(),
            error,
        };
        let file = file::parse(&text).map_err(in_file)?;
        let folder = path.parent().unwrap_or(Path::new(""));
        Loader::new(&file, folder)
            .load()
            .map_err(|error| match error {
                Failed::File(error) => in_file(error),
                Failed::Load(error) => error,
            })
    }

    /// The session's sample rate.
    pub fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    /// The sample after the last clip's end, audio or MIDI; 0 for a session without clips.
    pub fn last_sample(&self) -> SamplePosition {
        self.last_sample
    }

    /// The master's channels: the engine's playback channels.
    pub fn master_channels(&self) -> u16 {
        self.master_channels
    }
}

/// Why a loader stopped: the session's file, at a field, or one of the files it names.
enum Failed {
    File(FileError),
    Load(LoadError),
}

impl From<FileError> for Failed {
    fn from(error: FileError) -> Self {
        Self::File(error)
    }
}

impl From<LoadError> for Failed {
    fn from(error: LoadError) -> Self {
        Self::Load(error)
    }
}

/// What opens a session's files, each once.
struct Loader<'a> {
    file: &'a SessionFile,
    folder: &'a Path,
    audio_files: HashMap<PathBuf, Arc<AudioFile>>,
    midi_files: HashMap<PathBuf, MidiFile>,
}

impl<'a> Loader<'a> {
    fn new(file: &'a SessionFile, folder: &'a Path) -> Self {
        Self {
            file,
            folder,
            audio_files: HashMap::new(),
            midi_files: HashMap::new(),
        }
    }

    fn load(mut self) -> Result<Session, Failed> {
        let rate = self.file.rate()?;
        let tempo_map = self.file.tempo.map()?;
        let routing = self.file.routing()?;
        let mut tracks = Vec::with_capacity(self.file.tracks.len());
        let mut last_sample = 0;
        for (index, track) in self.file.tracks.iter().enumerate() {
            let field = format!("tracks[{index}]");
            let (source, end) = match track.source(&field)? {
                TrackSource::Audio { channels } => {
                    self.audio_track(&field, track, channels, rate)?
                }
                TrackSource::Midi(instrument) => {
                    self.midi_track(&field, track, instrument, rate, &tempo_map)?
                }
            };
            last_sample = last_sample.max(end);
            tracks.push(LoadedTrack {
                name: track.name.clone(),
                source,
            });
        }
        Ok(Session {
            rate,
            tempo_map: Arc::new(tempo_map),
            master_channels: self.file.master.channels,
            master_inserts: self.file.master.inserts.clone(),
            routing,
            tracks,
            last_sample: SamplePosition(last_sample),
        })
    }

    /// An audio track of `channels` channels, at `field`, and the sample after its last clip.
    fn audio_track(
        &mut self,
        field: &str,
        track: &Track,
        channels: u16,
        rate: SampleRate,
    ) -> Result<(LoadedSource, i64), Failed> {
        let mut clips = Vec::with_capacity(track.clips.len());
        let mut end = 0;
        for (index, clip) in track.clips.iter().enumerate() {
            let field = format!("{field}.clips[{index}]");
            let placed = place_audio_clip(&field, clip, rate)?;
            let file_field = format!("{field}.file");
            let audio = self.audio_file(&file_field, &clip.file)?;
            let info = audio.info();
            let failure = |problem| LoadError::Audio {
                field: file_field.clone(),
                path: audio.path().to_path_buf(),
                problem,
            };
            if info.sample_rate != rate.hertz() {
                return Err(failure(AudioProblem::RateMismatch {
                    file_rate: info.sample_rate,
                    session_rate: rate.hertz(),
                })
                .into());
            }
            if info.channels != channels {
                return Err(failure(AudioProblem::ChannelMismatch {
                    file_channels: info.channels,
                    track_channels: channels,
                })
                .into());
            }
            // The check of the file has made sure that this sum fits.
            let clip_end = placed.file_offset.saturating_add(clip.length);
            if clip_end > info.frames {
                return Err(failure(AudioProblem::PastFileEnd {
                    clip_end,
                    file_frames: info.frames,
                })
                .into());
            }
            end = end.max(placed.end);
            clips.push(AudioClip {
                file: Arc::clone(&audio),
                start: placed.start,
                file_offset: placed.file_offset,
                length: clip.length,
                gain: placed.gain,
            });
        }
        Ok((LoadedSource::Audio(AudioTrack { channels, clips }), end))
    }

    /// A MIDI track at `field`, its clips made one sequence, and the sample its last clip ends on.
    fn midi_track(
        &mut self,
        field: &str,
        track: &Track,
        instrument: Instrument,
        rate: SampleRate,
        tempo_map: &TempoMap,
    ) -> Result<(LoadedSource, i64), Failed> {
        let mut clips = Vec::with_capacity(track.clips.len());
        let mut end = 0;
        for (index, clip) in track.clips.iter().enumerate() {
            let field = format!("{field}.clips[{index}]");
            let placed = place_midi_clip(&field, clip, rate, tempo_map)?;
            let file_field = format!("{field}.file");
            let (path, file) = self.midi_file(&file_field, &clip.file)?;
            let made = midi::clip_from_file(file, placed.start, placed.length).map_err(|_| {
                LoadError::Midi {
                    field: file_field.clone(),
                    path,
                    problem: MidiProblem::TicksOutOfRange,
                }
            })?;
            end = end.max(placed.end.0);
            clips.push(made);
        }
        let sequence = MidiSequence::new(&clips).map_err(|error| LoadError::Sequence {
            field: field.to_owned(),
            error,
        })?;
        Ok((
            LoadedSource::Midi {
                instrument,
                sequence: Arc::new(sequence),
            },
            end,
        ))
    }

    /// The audio file `path` names, at `field`, opened the first time it is asked for.
    fn audio_file(&mut self, field: &str, path: &str) -> Result<Arc<AudioFile>, Failed> {
        let key = relative_path(field, path)?;
        match self.audio_files.entry(key) {
            Entry::Occupied(entry) => Ok(Arc::clone(entry.get())),
            Entry::Vacant(entry) => {
                let on_disk = self.folder.join(entry.key());
                let audio = AudioFile::open(&on_disk).map_err(|error| LoadError::Audio {
                    field: field.to_owned(),
                    path: on_disk,
                    problem: AudioProblem::Unreadable(error),
                })?;
                Ok(Arc::clone(entry.insert(audio)))
            }
        }
    }

    /// The MIDI file `path` names, at `field`, read the first time it is asked for, and its path on
    /// disk.
    fn midi_file(&mut self, field: &str, path: &str) -> Result<(PathBuf, &MidiFile), Failed> {
        let key = relative_path(field, path)?;
        let on_disk = self.folder.join(&key);
        let file = match self.midi_files.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let failure = |problem| LoadError::Midi {
                    field: field.to_owned(),
                    path: on_disk.clone(),
                    problem,
                };
                let bytes = read_limited(&on_disk).map_err(|error| {
                    failure(match error {
                        Limited::Read(error) => MidiProblem::Unreadable(error),
                        Limited::TooLarge => MidiProblem::TooLarge,
                    })
                })?;
                let read = midi_file::read(&bytes)
                    .map_err(|error| failure(MidiProblem::Malformed(error)))?;
                entry.insert(read)
            }
        };
        Ok((on_disk, file))
    }
}

/// Why a file could not be read whole.
enum Limited {
    Read(io::Error),
    /// It holds more than [`MAX_FILE_BYTES`].
    TooLarge,
}

/// The bytes of the file at `path`, at most [`MAX_FILE_BYTES`].
fn read_limited(path: &Path) -> Result<Vec<u8>, Limited> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take(MAX_FILE_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .map_err(Limited::Read)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Limited::TooLarge);
    }
    Ok(bytes)
}

/// The text of the file at `path`, at most [`MAX_FILE_BYTES`], in UTF-8.
fn read_to_string_limited(path: &Path) -> Result<String, Limited> {
    let bytes = read_limited(path)?;
    String::from_utf8(bytes)
        .map_err(|error| Limited::Read(io::Error::new(io::ErrorKind::InvalidData, error)))
}

/// Why a test session could not be opened.
#[derive(Debug)]
pub enum LoadError {
    /// The session file could not be read.
    Read {
        /// The session file.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
    /// The session file holds more than [`MAX_FILE_BYTES`].
    TooLarge {
        /// The session file.
        path: PathBuf,
    },
    /// The session file was refused: a field of it is wrong.
    File {
        /// The session file.
        path: PathBuf,
        /// The field, and what is wrong with it.
        error: FileError,
    },
    /// An audio clip's file was refused.
    Audio {
        /// The clip's `file` field, by its path in the session, such as `tracks[0].clips[2].file`.
        field: String,
        /// The audio file.
        path: PathBuf,
        /// What is wrong with it.
        problem: AudioProblem,
    },
    /// A MIDI clip's file was refused.
    Midi {
        /// The clip's `file` field, by its path in the session.
        field: String,
        /// The MIDI file.
        path: PathBuf,
        /// What is wrong with it.
        problem: MidiProblem,
    },
    /// A MIDI track's clips do not make a sequence.
    Sequence {
        /// The track, by its path in the session, such as `tracks[3]`.
        field: String,
        /// Why.
        error: SequenceError,
    },
}

/// What is wrong with an audio clip's file.
#[derive(Debug)]
pub enum AudioProblem {
    /// It cannot be opened, or is not a WAV file Magnolia reads.
    Unreadable(WavError),
    /// It is at another rate than the session's.
    RateMismatch {
        /// The file's rate, in hertz.
        file_rate: u32,
        /// The session's rate, in hertz.
        session_rate: u32,
    },
    /// It has other channels than its track.
    ChannelMismatch {
        /// The file's channels.
        file_channels: u16,
        /// The track's channels.
        track_channels: u16,
    },
    /// The clip runs past the file's last frame.
    PastFileEnd {
        /// The file's frame after the clip's last: its offset plus its length.
        clip_end: u64,
        /// The frames the file holds.
        file_frames: u64,
    },
}

/// What is wrong with a MIDI clip's file.
#[derive(Debug)]
pub enum MidiProblem {
    /// It cannot be read.
    Unreadable(io::Error),
    /// It holds more than [`MAX_FILE_BYTES`].
    TooLarge,
    /// It is not a MIDI file Magnolia reads.
    Malformed(MidiFileError),
    /// An event's tick, scaled to the time model's, lies past the ticks' range.
    TicksOutOfRange,
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => {
                write!(formatter, "{} cannot be read: {error}", path.display())
            }
            Self::TooLarge { path } => write!(
                formatter,
                "{} holds more than {MAX_FILE_BYTES} bytes, more than a test session may",
                path.display()
            ),
            Self::File { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::Audio {
                field,
                path,
                problem,
            } => write!(formatter, "{field}: {}: {problem}", path.display()),
            Self::Midi {
                field,
                path,
                problem,
            } => write!(formatter, "{field}: {}: {problem}", path.display()),
            Self::Sequence { field, error } => write!(formatter, "{field}: {error}"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { error, .. } => Some(error),
            Self::File { error, .. } => Some(error),
            Self::Audio {
                problem: AudioProblem::Unreadable(error),
                ..
            } => Some(error),
            Self::Midi {
                problem: MidiProblem::Unreadable(error),
                ..
            } => Some(error),
            Self::Midi {
                problem: MidiProblem::Malformed(error),
                ..
            } => Some(error),
            Self::Sequence { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for AudioProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(error) => write!(formatter, "not a WAV file Magnolia reads: {error}"),
            Self::RateMismatch {
                file_rate,
                session_rate,
            } => write!(
                formatter,
                "the file is at {file_rate} Hz, the session at {session_rate} Hz"
            ),
            Self::ChannelMismatch {
                file_channels,
                track_channels,
            } => write!(
                formatter,
                "the file has {file_channels} channels, its track {track_channels}"
            ),
            Self::PastFileEnd {
                clip_end,
                file_frames,
            } => write!(
                formatter,
                "the clip runs to frame {clip_end} of the file, which holds {file_frames}"
            ),
        }
    }
}

impl fmt::Display for MidiProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(error) => write!(formatter, "cannot be read: {error}"),
            Self::TooLarge => write!(
                formatter,
                "holds more than {MAX_FILE_BYTES} bytes, more than a test session reads"
            ),
            Self::Malformed(error) => write!(formatter, "{error}"),
            Self::TicksOutOfRange => formatter
                .write_str("an event lies too far from the file's start for the ticks' range"),
        }
    }
}
