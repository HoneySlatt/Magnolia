//! The test session's file: what it holds, in JSON, read and written with serde
//! ([ADR-0028](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0028-the-test-session-in-json.md)),
//! and [`parse`], which refuses a file that fails any check that needs no file on disk.
//!
//! Version 2 of the format:
//!
//! ```json
//! {
//!   "format": "magnolia-test-session", "version": 2,
//!   "sample_rate": 48000,
//!   "tempo": { "origin": 0, "changes": [ { "tick": 0, "bpm": [140, 1] } ],
//!              "meters": [ { "bar": 0, "beats": 4, "beat_unit": 4 } ] },
//!   "master": { "channels": 2, "inserts": [] },
//!   "buses": [ { "name": "drums", "channels": 2,
//!                "inserts": [ { "test_latency": { "samples": 64 } } ],
//!                "output": "master", "gain": 1.0 } ],
//!   "tracks": [
//!     { "name": "lead vocal", "kind": "audio", "channels": 1,
//!       "clips": [ { "file": "audio/lead.wav", "start": 0, "file_offset": 0, "length": 48000,
//!                    "gain": 1.0 } ],
//!       "inserts": [], "output": "master", "gain": 1.0,
//!       "sends": [ { "to": "drums", "gain": 0.25 } ] },
//!     { "name": "bass", "kind": "midi", "instrument": { "test_synth": {} },
//!       "clips": [ { "file": "midi/bass.mid", "start": 0, "length": 3870720 } ],
//!       "inserts": [], "output": "master", "gain": 1.0, "sends": [] },
//!     { "name": "voice", "kind": "input", "channels": 1, "inputs": [1], "armed": true,
//!       "inserts": [ { "test_latency": { "samples": 32 } } ], "output": "master", "gain": 1.0,
//!       "sends": [] }
//!   ]
//! }
//! ```
//!
//! Version 1, which this reader still reads, is the same without input tracks.
//!
//! Units ([ADR-0018](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0018-the-time-model.md)):
//! `tempo.origin`, where tick 0 lies, and an audio clip's `start` are in time units, the clip's
//! start placed on the first sample at or after it; a tempo change's `tick`, and a MIDI clip's
//! `start` and `length`, in ticks, [`TICKS_PER_QUARTER`] to a quarter note; a tempo is a fraction
//! of quarter notes a minute, `[numerator, denominator]`; an audio clip's `file_offset` and
//! `length` are in frames of its file. Paths are relative to the session file's folder and stay
//! inside it: an absolute path or a `..` is refused, a `.` ignored, so that one file has one
//! spelling; a symbolic link inside the folder is followed wherever it leads.
//!
//! The signal flows from each track's source (an audio track's clips, a MIDI track's instrument,
//! an input track's capture channels) through its inserts in series, then its gain, to its output,
//! and from after its gain through each send's gain to the send's destination. A bus sums what
//! feeds it, then runs its inserts and its gain into its output; the master sums what feeds it,
//! then runs its inserts into the playback channels. An output or a send names `master` or a bus;
//! a bus may feed another bus, not in a cycle; buses have no sends. What feeds a bus or the master
//! has its channels, except a mono source, which feeds both channels of a stereo one.
//!
//! An input track has no clips: it plays the device's capture channels its `inputs` name,
//! numbered from 1, one per channel of the track, live, whether the transport plays or not. A
//! capture channel feeds one channel of one input track at most. An armed input track is recorded
//! when the session is played with a recorder ([`SessionRecorder`](crate::SessionRecorder)).
//!
//! The schema is strict: a field unknown or missing (an optional field given as `null` counts as
//! absent, so `"inputs": null` passes in version 1 as no `inputs` does), a value of the wrong type or out of range, a
//! version newer than this reader's, each refuses the file with a [`FileError`] that names the
//! field by its path, such as `tracks[1].clips[0].start`. Other limits: every position lies at or
//! after the timeline's zero, where a render starts, and every clip ends within 24 hours of it, at
//! the session's rate, so that a render lasts at most a day; a track's audio clips do not overlap
//! ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md));
//! a session reads at most [`MAX_AUDIO_FILES`] audio files, each kept open while it plays. The
//! memory a session asks for is checked when it is opened, against
//! [`MEMORY_BUDGET`](crate::load::MEMORY_BUDGET).

// The loader runs this module on files from outside; a bad file must never crash Magnolia, so
// nothing here may panic.
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod check;
mod position;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::fmt;

use magnolia_engine::time::TempoMapError;
use serde::{Deserialize, Serialize};

pub(crate) use check::{
    Routing, Strip, Target, TrackSource, place_audio_clip, place_midi_clip, relative_path,
};

#[cfg(doc)]
use magnolia_engine::time::TICKS_PER_QUARTER;

/// The first field of every test session: what the file is.
pub const FORMAT: &str = "magnolia-test-session";

/// The version the generator writes, and the newest this reader reads.
pub const VERSION: u32 = 2;

/// The oldest version this reader reads: version 1, which has no input tracks.
pub const OLDEST_VERSION: u32 = 1;

/// The version that brought input tracks.
pub const INPUT_TRACKS_SINCE: u32 = 2;

/// The name an output or a send gives the master. No bus may take it.
pub const MASTER: &str = "master";

/// The most distinct audio files a session reads: each stays open while the session plays.
pub const MAX_AUDIO_FILES: usize = 512;

/// The most channels a track, a bus or the master may have: the most a WAV file holds. Also the
/// highest capture channel an input track may name.
pub const MAX_CHANNELS: u16 = 1_024;

/// A test session, as its JSON file holds it. [`parse`] reads one and checks it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFile {
    /// What the file is: [`FORMAT`].
    pub format: String,
    /// The version of the format it follows: [`VERSION`].
    pub version: u32,
    /// The rate every audio file and the engine run at, in hertz.
    pub sample_rate: u32,
    /// Where musical time lies on the timeline.
    pub tempo: TempoSection,
    /// The output every path ends in.
    pub master: Master,
    /// The buses, which sum tracks and other buses.
    pub buses: Vec<Bus>,
    /// The tracks.
    pub tracks: Vec<Track>,
}

impl SessionFile {
    /// The session as JSON text, indented two spaces a level, its fields in the order of the
    /// format, an optional field left out when absent: what the generator writes.
    ///
    /// # Errors
    ///
    /// None in practice: serde_json fails only for types this file does not hold.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// The tempo map: where tick 0 lies, the tempo changes and the meter changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempoSection {
    /// The time position of tick 0, in time units, at or after the timeline's zero.
    pub origin: i64,
    /// The tempo changes, in increasing order of ticks, the first at tick 0.
    pub changes: Vec<TempoChange>,
    /// The meter changes, in increasing order of bars, the first at bar 0.
    pub meters: Vec<MeterChange>,
}

/// A tempo from a tick on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempoChange {
    /// Where it starts, in ticks.
    pub tick: i64,
    /// Quarter notes a minute, as a fraction `[numerator, denominator]`: 174 is `[174, 1]`, 93.5
    /// is `[187, 2]`; from 1 to 1,000.
    pub bpm: [u32; 2],
}

/// A meter from a bar on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterChange {
    /// The bar it starts at, counted from 0.
    pub bar: i64,
    /// The beats in a bar, at least 1.
    pub beats: u16,
    /// The note value of a beat: a power of two from 1 to 64.
    pub beat_unit: u16,
}

/// The master: the playback channels, and the inserts before them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Master {
    /// The playback channels: 2 to [`MAX_CHANNELS`].
    pub channels: u16,
    /// The inserts, in series, before the playback channels.
    pub inserts: Vec<Insert>,
}

/// A bus: it sums what feeds it, then runs its inserts and its gain into its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bus {
    /// Its name, by which outputs and sends name it: unique, and not [`MASTER`].
    pub name: String,
    /// Its channels: 1 to [`MAX_CHANNELS`].
    pub channels: u16,
    /// Its inserts, in series.
    pub inserts: Vec<Insert>,
    /// Where it goes: [`MASTER`] or another bus, never back to itself.
    pub output: String,
    /// What its signal is multiplied by after its inserts: a finite number.
    pub gain: f32,
}

/// A track: an audio track plays clips of WAV files, a MIDI track plays clips of MIDI files into
/// its instrument, an input track plays the device's capture channels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// Its name, as reports and its takes' files show it.
    pub name: String,
    /// What it plays.
    pub kind: TrackKind,
    /// An audio track's channels, those of its files, or an input track's: 1 to [`MAX_CHANNELS`].
    /// A MIDI track has none: its instrument gives two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    /// A MIDI track's instrument. Other tracks have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<Instrument>,
    /// An input track's capture channels, numbered from 1 to [`MAX_CHANNELS`], one per channel of
    /// the track, in order; each feeds one channel of one input track at most. Other tracks have
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<u16>>,
    /// Whether an input track is armed: recorded when the session is played with a recorder.
    /// Other tracks have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed: Option<bool>,
    /// An audio or a MIDI track's clips, in any order. An input track has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clips: Option<Vec<Clip>>,
    /// Its inserts, in series, after its source.
    pub inserts: Vec<Insert>,
    /// Where it goes: [`MASTER`] or a bus.
    pub output: String,
    /// What its signal is multiplied by after its inserts: a finite number.
    pub gain: f32,
    /// Its sends, taken after its gain.
    pub sends: Vec<TrackSend>,
}

impl Track {
    /// Its clips; none for an input track, or a track that gives none.
    pub fn clips(&self) -> &[Clip] {
        self.clips.as_deref().unwrap_or_default()
    }
}

/// What a track plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Clips of WAV files, streamed from disk.
    Audio,
    /// Clips of MIDI files, played into an instrument.
    Midi,
    /// The device's capture channels, live. Since version 2.
    Input,
}

/// A MIDI track's instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Instrument {
    /// The engine's test synth, `{ "test_synth": {} }`: two outputs.
    TestSynth {},
}

/// A clip of a file, placed on the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// Its file, relative to the session file's folder.
    pub file: String,
    /// Where it starts, at or after the timeline's zero: in time units for an audio clip, placed
    /// on the first sample at or after; in ticks for a MIDI clip.
    pub start: i64,
    /// An audio clip's first frame in its file. A MIDI clip has none: it plays its file from its
    /// start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_offset: Option<u64>,
    /// How long it lasts, at least 1: in frames of its file for an audio clip, which must hold
    /// them all; in ticks for a MIDI clip, whose events at or past its end do not play.
    pub length: u64,
    /// What an audio clip's samples are multiplied by: a finite number. A MIDI clip has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<f32>,
}

/// A node in a chain of inserts, standing for a plugin until plugins come (M1.6).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Insert {
    /// A pure delay that declares its latency, which the engine compensates.
    TestLatency {
        /// The delay, in samples: 0 to 65,536, the engine's largest latency.
        samples: u32,
    },
    /// A gain.
    Gain {
        /// What the signal is multiplied by: a finite number.
        value: f32,
    },
}

/// A send: a track's signal, after its gain, through a gain of its own to a bus or the master.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSend {
    /// Where it goes: [`MASTER`] or a bus.
    pub to: String,
    /// What the signal is multiplied by: a finite number.
    pub gain: f32,
}

/// The two fields read before the rest, so that a newer version is refused as such rather than
/// for the fields it adds.
#[derive(Deserialize)]
struct Header {
    format: String,
    version: u32,
}

/// Reads a test session from its JSON text and checks everything that needs no file on disk: the
/// format and its version, each value's range, the tempo map, the names that outputs and sends
/// give, the channels of what feeds what, cycles of buses, overlapping audio clips, the count of
/// audio files, and the capture channels of input tracks.
///
/// # Errors
///
/// A [`FileError`] naming the first field found wrong, by its path.
pub fn parse(text: &str) -> Result<SessionFile, FileError> {
    let header: Header =
        serde_json::from_str(text).map_err(|error| FileError::from_json(text, error))?;
    if header.format != FORMAT {
        return Err(FileError::new(
            "format",
            Problem::NotATestSession(header.format),
        ));
    }
    if !(OLDEST_VERSION..=VERSION).contains(&header.version) {
        return Err(FileError::new(
            "version",
            Problem::UnsupportedVersion(header.version),
        ));
    }
    let file: SessionFile =
        serde_json::from_str(text).map_err(|error| FileError::from_json(text, error))?;
    file.check()?;
    Ok(file)
}

/// Why a test session's text was refused: the field, by its path such as
/// `tracks[1].clips[0].start`, and what is wrong with it.
#[derive(Debug)]
pub struct FileError {
    field: String,
    problem: Problem,
}

impl FileError {
    pub(crate) fn new(field: impl Into<String>, problem: Problem) -> Self {
        Self {
            field: field.into(),
            problem,
        }
    }

    /// An error of serde_json's, with the path of the field at its line and column.
    fn from_json(text: &str, error: serde_json::Error) -> Self {
        Self {
            field: position::field_at(text, error.line(), error.column()),
            problem: Problem::Json(error),
        }
    }

    /// The field's path, such as `tracks[1].clips[0].start`; empty for the file as a whole.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// What is wrong with it.
    pub fn problem(&self) -> &Problem {
        &self.problem
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field.is_empty() {
            write!(formatter, "{}", self.problem)
        } else {
            write!(formatter, "{}: {}", self.field, self.problem)
        }
    }
}

impl Error for FileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.problem {
            Problem::Json(error) => Some(error),
            Problem::TempoMap(error) => Some(error),
            _ => None,
        }
    }
}

/// What is wrong with a field of a test session.
#[derive(Debug)]
pub enum Problem {
    /// The text is not JSON, or does not follow the schema: a field unknown, missing or given
    /// twice, or a value of the wrong type or beyond its type's range. serde_json's message names
    /// the field and gives the line and column.
    Json(serde_json::Error),
    /// The format is not [`FORMAT`], but this.
    NotATestSession(String),
    /// The version is not one this reader reads, [`OLDEST_VERSION`] to [`VERSION`], but this one:
    /// newer than this reader's, or 0.
    UnsupportedVersion(u32),
    /// The field came with a later version of the format than the file's.
    NotInVersion {
        /// The file's version.
        version: u32,
        /// The version that brought the field.
        since: u32,
    },
    /// A value is out of its range.
    OutOfRange {
        /// The value, as the file gives it.
        value: String,
        /// What the field accepts.
        accepted: &'static str,
    },
    /// A field that this kind of track needs is missing.
    MissingForKind {
        /// The kind of track, or of clip.
        kind: &'static str,
    },
    /// A field that this kind of track does not have is given.
    NotForKind {
        /// The kind of track, or of clip.
        kind: &'static str,
    },
    /// An output or a send names neither the master nor a bus, but this.
    UnknownDestination(String),
    /// A bus takes the name of an earlier bus, or the master's: this one.
    NameTaken(String),
    /// What feeds a bus or the master has other channels than it, and is not mono into stereo.
    ChannelMismatch {
        /// The channels of what feeds.
        from: u16,
        /// The channels of what it feeds.
        to: u16,
    },
    /// The bus's output leads back to itself, through these buses.
    BusCycle(Vec<String>),
    /// The audio clip starts before an earlier clip of its track ends; a track's audio clips may
    /// not overlap.
    ClipsOverlap {
        /// The earlier clip, by its index in the track.
        earlier: usize,
    },
    /// A file's path is absolute; paths are relative to the session file's folder.
    AbsolutePath,
    /// A file's path is empty.
    EmptyPath,
    /// A file's path climbs out of a folder with `..`; paths stay inside the session file's
    /// folder.
    PathLeavesFolder,
    /// The clip's audio file is past the [`MAX_AUDIO_FILES`]th the session reads.
    TooManyAudioFiles,
    /// An input track names more or fewer capture channels than it has channels.
    InputCount {
        /// The capture channels it names.
        inputs: usize,
        /// Its channels.
        channels: u16,
    },
    /// The capture channel is used already, by the input at this field, such as
    /// `tracks[2].inputs[0]`: each feeds one channel of one input track at most.
    CaptureChannelTaken {
        /// The capture channel, from 1.
        channel: u16,
        /// The field that uses it first.
        by: String,
    },
    /// The tempo map cannot be built.
    TempoMap(TempoMapError),
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "{error}"),
            Self::NotATestSession(format) => write!(
                formatter,
                "{format:?} is not {FORMAT:?}: the file is not a Magnolia test session"
            ),
            Self::UnsupportedVersion(version) if *version > VERSION => write!(
                formatter,
                "version {version} is newer than this reader's, {VERSION}: update Magnolia"
            ),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "version {version} does not exist; this reader reads versions {OLDEST_VERSION} \
                 to {VERSION}"
            ),
            Self::NotInVersion { version, since } => write!(
                formatter,
                "version {version} of the format does not have this; it came with version {since}"
            ),
            Self::OutOfRange { value, accepted } => {
                write!(formatter, "{value} is out of range: {accepted}")
            }
            Self::MissingForKind { kind } => write!(formatter, "{kind} needs this field"),
            Self::NotForKind { kind } => write!(formatter, "{kind} does not have this field"),
            Self::UnknownDestination(name) => write!(
                formatter,
                "{name:?} names neither {MASTER:?} nor a bus of the session"
            ),
            Self::NameTaken(name) => write!(
                formatter,
                "{name:?} is taken, by an earlier bus or the master; each bus has a name of its \
                 own"
            ),
            Self::ChannelMismatch { from, to } => write!(
                formatter,
                "{from} channels feed {to}; what feeds a bus or the master has its channels, or \
                 is mono into stereo"
            ),
            Self::BusCycle(buses) => write!(
                formatter,
                "the bus feeds itself back, through {}; buses may not form a cycle",
                buses.join(", then ")
            ),
            Self::ClipsOverlap { earlier } => write!(
                formatter,
                "starts before clip {earlier} of its track ends; a track's audio clips may not \
                 overlap"
            ),
            Self::AbsolutePath => formatter
                .write_str("the path is absolute; paths are relative to the session file's folder"),
            Self::EmptyPath => formatter.write_str("the path is empty"),
            Self::PathLeavesFolder => formatter.write_str(
                "the path climbs out of a folder with `..`; paths stay inside the session file's \
                 folder",
            ),
            Self::TooManyAudioFiles => write!(
                formatter,
                "the session reads at most {MAX_AUDIO_FILES} audio files, and this is one more"
            ),
            Self::InputCount { inputs, channels } => write!(
                formatter,
                "{inputs} capture channels for a track of {channels}; an input track names one \
                 per channel"
            ),
            Self::CaptureChannelTaken { channel, by } => write!(
                formatter,
                "capture channel {channel} is used already, by {by}; each feeds one channel of \
                 one input track at most"
            ),
            Self::TempoMap(error) => write!(formatter, "{error}"),
        }
    }
}
