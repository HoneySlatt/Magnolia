//! Standard MIDI Files, formats 0 and 1, read and written by Magnolia's own code
//! ([ADR-0029](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0029-magnolias-own-wav-and-midi-file-readers.md)).
//!
//! [`read`] turns a file's bytes into a [`MidiFile`]: its tracks, each the list of the events
//! Magnolia plays, at their ticks. [`write`](write()) turns a `MidiFile` back into bytes. For
//! every file that `read` returns, `read(&write(&file))` gives it back.
//!
//! What is read:
//!
//! - the `MThd` chunk, first: a length of at least 6 bytes, the bytes past the sixth skipped;
//!   format 0 or 1; a division in ticks per quarter note, 1 to 32,767. Format 2, and a division in
//!   SMPTE frames (its top bit set), are refused. A format 0 file whose header counts other than
//!   one track is read as its header says.
//! - as many `MTrk` chunks as the header counts, in file order. Fewer is an error; those past the
//!   count are not read. Any other chunk is skipped.
//! - in each track, the events Magnolia plays, each at the sum of the delta times before it: notes
//!   on and off, pitch bends, controllers (channel mode messages included), tempo (meta 0x51) and
//!   time signature (meta 0x58). A note on of velocity 0 is a note off. Program changes, channel
//!   and key pressure, system-exclusive events (0xF0 and 0xF7) and every other meta event are read
//!   and dropped.
//! - variable-length quantities of at most 4 bytes, so a delta time of at most 2^28 − 1 ticks.
//!
//! Running status lets a channel message leave out its status byte when it repeats the status of
//! the message before. The standard says that a system-exclusive or a meta event cancels it.
//! Readers differ on meta events: midly cancels running status there, as the standard says; mido
//! keeps it across them. Magnolia's reader cancels it after a system-exclusive event, where a data
//! byte is then an error, and keeps it across meta events. A file that follows the standard
//! never puts a data byte right after a meta event, so it reads the same either way; a file whose
//! writer kept running status across a tempo change reads instead of failing. The writer follows
//! the standard: it gives the status byte again after every meta event.
//!
//! Leniency: a track whose chunk ends without an end of track (meta 0x2F), which the standard
//! requires, ends with its chunk; bytes after an end of track, inside its chunk, are not read. A
//! tempo or time signature longer than the standard gives it has its extra bytes skipped.
//!
//! Bounds, so that a small file never makes the reader allocate without bound: at most 65,535
//! tracks, which the header's 16-bit count bounds, and at most 16,777,216 events in the whole
//! file, dropped events included and ends of track aside; a file of more is refused. Nothing is
//! allocated from a length a file declares. With at most 16,777,216 events, each at most 2^28 − 1
//! ticks after the one before, a tick stays under 2^52 and never overflows.
//!
//! What is written: [`write`](write()) gives the layout.
//!
//! Limits: a file wrapped in RIFF (RMID) is not read. What a track holds past its last event, the
//! tick of its end of track, is not kept; the writer ends each track at its last event. A time
//! signature's clicks and thirty-second notes per quarter are not kept; the writer gives 24 MIDI
//! clocks a click and 8 thirty-second notes a quarter. Text, names, markers, key signatures and
//! system-exclusive data are not kept.

// The session loader runs this module on files from outside; a bad file must never crash
// Magnolia, so nothing here may panic. The lints cover the reader and the writer, which are its
// submodules.
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod read;
mod write;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::fmt;

pub use read::read;
pub use write::write;

/// A Standard MIDI File, as [`read`] returns it and [`write`](write()) takes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MidiFile {
    /// How the tracks relate: one track, or several played together.
    pub format: Format,
    /// The ticks in a quarter note: 1 to 32,767 as read.
    pub ticks_per_quarter: u16,
    /// The tracks, in file order: at most 65,535.
    pub tracks: Vec<Track>,
}

/// A MIDI file's format, from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// Format 0: one track, every channel in it.
    SingleTrack,
    /// Format 1: several tracks, played together; the first usually holds the tempo map.
    Parallel,
}

impl Format {
    /// The number the header gives this format.
    fn number(self) -> u16 {
        match self {
            Self::SingleTrack => 0,
            Self::Parallel => 1,
        }
    }
}

/// One `MTrk` chunk's events.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Track {
    /// The events, sorted by tick; at one tick, in file order.
    pub events: Vec<TrackEvent>,
}

/// An event of a track, at its tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackEvent {
    /// The ticks from the track's start: the sum of the delta times up to this event.
    pub tick: u64,
    /// What happens at that tick.
    pub kind: EventKind,
}

/// What an event does. Channels count from 0 to 15; keys, velocities, controller numbers and
/// controller values from 0 to 127.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// A note starts. Its velocity is 1 to 127: a note on of velocity 0 reads as a note off.
    NoteOn {
        /// The channel.
        channel: u8,
        /// The key.
        key: u8,
        /// How hard the key was struck.
        velocity: u8,
    },
    /// A note stops.
    NoteOff {
        /// The channel.
        channel: u8,
        /// The key.
        key: u8,
        /// How fast the key was released; 0 for a note on of velocity 0.
        velocity: u8,
    },
    /// The channel's pitch bends.
    PitchBend {
        /// The channel.
        channel: u8,
        /// The bend, from -8,192 to 8,191; 0 is no bend.
        value: i16,
    },
    /// A controller of the channel changes, or a channel mode message (numbers 120 to 127).
    Controller {
        /// The channel.
        channel: u8,
        /// The controller's number.
        number: u8,
        /// Its new value.
        value: u8,
    },
    /// The tempo changes (meta event 0x51).
    Tempo {
        /// The length of a quarter note, as the file gives it: at most 16,777,215.
        microseconds_per_quarter: u32,
    },
    /// The time signature changes (meta event 0x58), as the file gives it.
    TimeSignature {
        /// The beats in a bar.
        numerator: u8,
        /// The beat's note as a power of two: 2 for a quarter, 3 for an eighth.
        denominator_power: u8,
    },
}

/// The bytes of the `MThd` chunk's body that Magnolia reads and writes: the format, the track
/// count and the division.
const HEADER_BYTES: u32 = 6;

/// The most ticks a quarter note may have: a division with its top bit set counts SMPTE frames.
const MAX_TICKS_PER_QUARTER: u16 = 0x7FFF;

/// The most events a file may hold, dropped events included and ends of track aside.
const MAX_EVENTS: u32 = 1 << 24;

/// The bytes a variable-length quantity may take, and the largest value they hold.
const MAX_VARIABLE_LENGTH_BYTES: usize = 4;
const MAX_VARIABLE_LENGTH: u32 = 0x0FFF_FFFF;

/// The status bytes of channel messages, without their channel.
const NOTE_OFF: u8 = 0x80;
const NOTE_ON: u8 = 0x90;
const KEY_PRESSURE: u8 = 0xA0;
const CONTROLLER: u8 = 0xB0;
const PITCH_BEND: u8 = 0xE0;

/// The status bytes of system-exclusive events and of meta events.
const SYSTEM_EXCLUSIVE: u8 = 0xF0;
const SYSTEM_EXCLUSIVE_ESCAPE: u8 = 0xF7;
const META: u8 = 0xFF;

/// The meta events' types that Magnolia reads or writes.
const TEXT: u8 = 0x01;
const END_OF_TRACK: u8 = 0x2F;
const TEMPO: u8 = 0x51;
const TIME_SIGNATURE: u8 = 0x58;

/// The bytes the standard gives a tempo and a time signature meta event.
const TEMPO_BYTES: usize = 3;
const TIME_SIGNATURE_BYTES: usize = 4;

/// The largest value of a byte of data in a channel message, of a channel, of a tempo's three
/// bytes, and the range of a pitch bend.
const MAX_DATA_BYTE: u8 = 0x7F;
const MAX_CHANNEL: u8 = 0x0F;
const MAX_TEMPO: u32 = 0x00FF_FFFF;
const PITCH_BEND_CENTRE: i16 = 0x2000;
const MIN_PITCH_BEND: i16 = -0x2000;
const MAX_PITCH_BEND: i16 = 0x1FFF;

/// Why a MIDI file could not be read. A track's number counts the `MTrk` chunks in file order,
/// from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MidiFileError {
    /// The file does not start with an `MThd` chunk.
    NotMidiFile,
    /// The file ends inside a chunk: its header, a track, or a chunk skipped before a track still
    /// to be read.
    Truncated,
    /// The `MThd` chunk holds fewer than 6 bytes: this many.
    HeaderTooShort(u32),
    /// The format is neither 0 nor 1, but this one; format 2 holds independent sequences.
    UnsupportedFormat(u16),
    /// The division, this one, counts SMPTE frames rather than ticks per quarter note.
    TimecodeDivision(u16),
    /// The division gives 0 ticks per quarter note.
    ZeroTicksPerQuarter,
    /// The file holds fewer `MTrk` chunks than its header counts.
    MissingTracks {
        /// The tracks the header counts.
        expected: u16,
        /// The tracks the file holds.
        found: u16,
    },
    /// The file holds more than 16,777,216 events, ends of track aside.
    TooManyEvents,
    /// A track's chunk ends inside an event.
    TrackCutShort {
        /// The track's number.
        track: u16,
    },
    /// A variable-length quantity runs past 4 bytes.
    VariableLengthTooLong {
        /// The track's number.
        track: u16,
    },
    /// An event starts with a data byte where no running status holds: at the start of a track,
    /// or after a system-exclusive event.
    MissingStatus {
        /// The track's number.
        track: u16,
    },
    /// A channel message holds a status byte, this one, where a data byte is needed.
    StatusInsideMessage {
        /// The track's number.
        track: u16,
        /// The status byte found.
        status: u8,
    },
    /// An event starts with a status byte that a MIDI file may not hold, this one: a system
    /// common or real-time message, 0xF1 to 0xF6 or 0xF8 to 0xFE.
    UnsupportedStatus {
        /// The track's number.
        track: u16,
        /// The status byte found.
        status: u8,
    },
    /// A tempo or time-signature meta event holds fewer bytes than the standard gives it.
    MetaTooShort {
        /// The track's number.
        track: u16,
        /// The meta event's type: 0x51 or 0x58.
        meta_type: u8,
        /// The bytes it holds.
        length: u32,
    },
}

impl fmt::Display for MidiFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMidiFile => formatter.write_str("the file is not a MIDI file"),
            Self::Truncated => formatter.write_str("the MIDI file ends inside a chunk"),
            Self::HeaderTooShort(bytes) => write!(
                formatter,
                "the MIDI file's header holds {bytes} bytes, fewer than its 6"
            ),
            Self::UnsupportedFormat(format) => write!(
                formatter,
                "the MIDI file is of format {format}; Magnolia reads formats 0 and 1"
            ),
            Self::TimecodeDivision(division) => write!(
                formatter,
                "the MIDI file counts its time in SMPTE frames (division {division:#06x}); \
                 Magnolia reads ticks per quarter note"
            ),
            Self::ZeroTicksPerQuarter => {
                formatter.write_str("the MIDI file gives 0 ticks per quarter note")
            }
            Self::MissingTracks { expected, found } => write!(
                formatter,
                "the MIDI file holds {found} tracks where its header counts {expected}"
            ),
            Self::TooManyEvents => write!(
                formatter,
                "the MIDI file holds more than {MAX_EVENTS} events"
            ),
            Self::TrackCutShort { track } => write!(
                formatter,
                "track {track} of the MIDI file ends inside an event"
            ),
            Self::VariableLengthTooLong { track } => write!(
                formatter,
                "track {track} of the MIDI file holds a variable-length number longer than \
                 {MAX_VARIABLE_LENGTH_BYTES} bytes"
            ),
            Self::MissingStatus { track } => write!(
                formatter,
                "track {track} of the MIDI file starts an event with a data byte where no running \
                 status holds"
            ),
            Self::StatusInsideMessage { track, status } => write!(
                formatter,
                "track {track} of the MIDI file holds the status byte {status:#04x} inside a \
                 channel message"
            ),
            Self::UnsupportedStatus { track, status } => write!(
                formatter,
                "track {track} of the MIDI file holds the status byte {status:#04x}, which a \
                 MIDI file may not hold"
            ),
            Self::MetaTooShort {
                track,
                meta_type,
                length,
            } => write!(
                formatter,
                "track {track} of the MIDI file holds a meta event of type {meta_type:#04x} of \
                 {length} bytes, too few for its type"
            ),
        }
    }
}

impl Error for MidiFileError {}
