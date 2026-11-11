//! Reading a Standard MIDI File's bytes.

use super::{
    CONTROLLER, END_OF_TRACK, EventKind, Format, KEY_PRESSURE, MAX_EVENTS, MAX_TICKS_PER_QUARTER,
    MAX_VARIABLE_LENGTH_BYTES, META, MidiFile, MidiFileError, NOTE_OFF, NOTE_ON, PITCH_BEND,
    PITCH_BEND_CENTRE, SYSTEM_EXCLUSIVE, SYSTEM_EXCLUSIVE_ESCAPE, TEMPO, TEMPO_BYTES,
    TIME_SIGNATURE, TIME_SIGNATURE_BYTES, Track, TrackEvent,
};

/// Reads a Standard MIDI File of format 0 or 1 from its bytes, as the
/// [module's documentation](super) says.
///
/// # Errors
///
/// A plain [`MidiFileError`] for a file that is not a MIDI file Magnolia reads, is cut short, holds
/// more events than the reader takes, or is malformed.
pub fn read(bytes: &[u8]) -> Result<MidiFile, MidiFileError> {
    read_with_event_limit(bytes, MAX_EVENTS)
}

/// Reads a file of at most `max_events` events, ends of track aside. Only tests lower the bound
/// from [`MAX_EVENTS`], to reach it without reading millions of events.
pub(super) fn read_with_event_limit(
    bytes: &[u8],
    max_events: u32,
) -> Result<MidiFile, MidiFileError> {
    let mut file = ByteReader { rest: bytes };
    let header = read_header(&mut file)?;
    let mut tracks = Vec::new();
    let mut events_left = max_events;
    let mut found: u16 = 0;
    while found < header.track_count {
        let Some(chunk) = next_chunk(&mut file)? else {
            return Err(MidiFileError::MissingTracks {
                expected: header.track_count,
                found,
            });
        };
        if &chunk.id == b"MTrk" {
            // Below the header's count, which is a u16, so it never saturates.
            found = found.saturating_add(1);
            tracks.push(read_track(chunk.body, found, &mut events_left)?);
        }
    }
    Ok(MidiFile {
        format: header.format,
        ticks_per_quarter: header.ticks_per_quarter,
        tracks,
    })
}

/// Bytes read from the front. Each read takes the bytes it returns, or nothing when too few are
/// left.
struct ByteReader<'a> {
    rest: &'a [u8],
}

impl<'a> ByteReader<'a> {
    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    fn byte(&mut self) -> Option<u8> {
        let (&byte, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(byte)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (&array, rest) = self.rest.split_first_chunk()?;
        self.rest = rest;
        Some(array)
    }

    fn take(&mut self, len: u32) -> Option<&'a [u8]> {
        let (taken, rest) = self.rest.split_at_checked(usize::try_from(len).ok()?)?;
        self.rest = rest;
        Some(taken)
    }
}

/// What the `MThd` chunk says.
struct Header {
    format: Format,
    track_count: u16,
    ticks_per_quarter: u16,
}

/// Reads the `MThd` chunk, which must come first.
fn read_header(file: &mut ByteReader<'_>) -> Result<Header, MidiFileError> {
    if file.array() != Some(*b"MThd") {
        return Err(MidiFileError::NotMidiFile);
    }
    let length = u32::from_be_bytes(file.array().ok_or(MidiFileError::Truncated)?);
    let body = file.take(length).ok_or(MidiFileError::Truncated)?;
    // The bytes past the sixth, which a later version of the standard may define, are skipped.
    let &[f0, f1, t0, t1, d0, d1] = body
        .first_chunk()
        .ok_or(MidiFileError::HeaderTooShort(length))?;
    let format = match u16::from_be_bytes([f0, f1]) {
        0 => Format::SingleTrack,
        1 => Format::Parallel,
        other => return Err(MidiFileError::UnsupportedFormat(other)),
    };
    let division = u16::from_be_bytes([d0, d1]);
    if division > MAX_TICKS_PER_QUARTER {
        return Err(MidiFileError::TimecodeDivision(division));
    }
    if division == 0 {
        return Err(MidiFileError::ZeroTicksPerQuarter);
    }
    Ok(Header {
        format,
        track_count: u16::from_be_bytes([t0, t1]),
        ticks_per_quarter: division,
    })
}

/// A chunk after the header: its id, and its body of the length it declares.
struct Chunk<'a> {
    id: [u8; 4],
    body: &'a [u8],
}

/// The next chunk, or `None` at the end of the file. Chunks follow one another without padding.
fn next_chunk<'a>(file: &mut ByteReader<'a>) -> Result<Option<Chunk<'a>>, MidiFileError> {
    if file.is_empty() {
        return Ok(None);
    }
    let id = file.array().ok_or(MidiFileError::Truncated)?;
    let length = u32::from_be_bytes(file.array().ok_or(MidiFileError::Truncated)?);
    let body = file.take(length).ok_or(MidiFileError::Truncated)?;
    Ok(Some(Chunk { id, body }))
}

/// Reads the events of the track numbered `track`, from its chunk's body, and counts them against
/// the file's `events_left`.
fn read_track(body: &[u8], track: u16, events_left: &mut u32) -> Result<Track, MidiFileError> {
    let mut reader = TrackReader {
        bytes: ByteReader { rest: body },
        track,
        running_status: None,
    };
    let mut events = Vec::new();
    let mut tick: u64 = 0;
    // A track whose chunk ends without an end of track ends with its chunk.
    while !reader.bytes.is_empty() {
        let delta = reader.variable_length()?;
        // Never saturates: a file holds at most 2^24 events besides its ends of track, each at
        // most 2^28 - 1 ticks after the one before, so a tick stays under 2^52.
        tick = tick.saturating_add(u64::from(delta));
        let event = reader.event()?;
        if event == Event::EndOfTrack {
            break;
        }
        *events_left = events_left
            .checked_sub(1)
            .ok_or(MidiFileError::TooManyEvents)?;
        if let Event::Kept(kind) = event {
            events.push(TrackEvent { tick, kind });
        }
    }
    Ok(Track { events })
}

/// What an event read means for the track.
#[derive(PartialEq, Eq)]
enum Event {
    /// An event Magnolia plays.
    Kept(EventKind),
    /// An event read and dropped.
    Dropped,
    /// The end of the track.
    EndOfTrack,
}

/// Reads the events of one track.
struct TrackReader<'a> {
    bytes: ByteReader<'a>,
    /// The track's number, for errors.
    track: u16,
    /// The status of the last channel message, which a message without a status byte repeats;
    /// `None` at the track's start and after a system-exclusive event.
    running_status: Option<u8>,
}

impl<'a> TrackReader<'a> {
    /// Reads one event, after its delta time.
    fn event(&mut self) -> Result<Event, MidiFileError> {
        let first = self.byte()?;
        match first {
            // A data byte: the message repeats the status of the one before.
            0x00..=0x7F => {
                let status = self
                    .running_status
                    .ok_or(MidiFileError::MissingStatus { track: self.track })?;
                self.channel_message(status, first)
            }
            0x80..=0xEF => {
                self.running_status = Some(first);
                let data = self.data_byte()?;
                self.channel_message(first, data)
            }
            SYSTEM_EXCLUSIVE | SYSTEM_EXCLUSIVE_ESCAPE => {
                self.running_status = None;
                let length = self.variable_length()?;
                self.take(length)?;
                Ok(Event::Dropped)
            }
            // Running status is kept across a meta event: the module's documentation says why.
            META => self.meta_event(),
            status => Err(MidiFileError::UnsupportedStatus {
                track: self.track,
                status,
            }),
        }
    }

    /// Reads the rest of a channel message of `status`, whose first data byte is `first`.
    fn channel_message(&mut self, status: u8, first: u8) -> Result<Event, MidiFileError> {
        let channel = status & 0x0F;
        let kind = match status & 0xF0 {
            NOTE_OFF => EventKind::NoteOff {
                channel,
                key: first,
                velocity: self.data_byte()?,
            },
            NOTE_ON => match self.data_byte()? {
                0 => EventKind::NoteOff {
                    channel,
                    key: first,
                    velocity: 0,
                },
                velocity => EventKind::NoteOn {
                    channel,
                    key: first,
                    velocity,
                },
            },
            CONTROLLER => EventKind::Controller {
                channel,
                number: first,
                value: self.data_byte()?,
            },
            PITCH_BEND => EventKind::PitchBend {
                channel,
                value: pitch_bend(first, self.data_byte()?),
            },
            KEY_PRESSURE => {
                self.data_byte()?;
                return Ok(Event::Dropped);
            }
            // Program change (0xC0) and channel pressure (0xD0), whose one data byte is read.
            _ => return Ok(Event::Dropped),
        };
        Ok(Event::Kept(kind))
    }

    /// Reads a meta event, after its status byte.
    fn meta_event(&mut self) -> Result<Event, MidiFileError> {
        let meta_type = self.byte()?;
        let length = self.variable_length()?;
        let data = self.take(length)?;
        let too_short = MidiFileError::MetaTooShort {
            track: self.track,
            meta_type,
            length,
        };
        let kind = match meta_type {
            END_OF_TRACK => return Ok(Event::EndOfTrack),
            TEMPO => {
                let &[high, middle, low] = data.first_chunk::<TEMPO_BYTES>().ok_or(too_short)?;
                EventKind::Tempo {
                    microseconds_per_quarter: u32::from_be_bytes([0, high, middle, low]),
                }
            }
            TIME_SIGNATURE => {
                // The clicks and thirty-second notes per quarter that follow are not kept.
                let &[numerator, denominator_power, _, _] = data
                    .first_chunk::<TIME_SIGNATURE_BYTES>()
                    .ok_or(too_short)?;
                EventKind::TimeSignature {
                    numerator,
                    denominator_power,
                }
            }
            _ => return Ok(Event::Dropped),
        };
        Ok(Event::Kept(kind))
    }

    /// Reads a variable-length quantity: seven bits a byte, the highest first, every byte but the
    /// last with its top bit set.
    fn variable_length(&mut self) -> Result<u32, MidiFileError> {
        let mut value: u32 = 0;
        for _ in 0..MAX_VARIABLE_LENGTH_BYTES {
            let byte = self.byte()?;
            // Four bytes of seven bits fit in a u32, so nothing is shifted out.
            value = value << 7 | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(MidiFileError::VariableLengthTooLong { track: self.track })
    }

    /// Reads a data byte of a channel message: below 0x80.
    fn data_byte(&mut self) -> Result<u8, MidiFileError> {
        match self.byte()? {
            status @ 0x80.. => Err(MidiFileError::StatusInsideMessage {
                track: self.track,
                status,
            }),
            data => Ok(data),
        }
    }

    fn byte(&mut self) -> Result<u8, MidiFileError> {
        self.bytes.byte().ok_or_else(|| self.cut_short())
    }

    fn take(&mut self, len: u32) -> Result<&'a [u8], MidiFileError> {
        self.bytes.take(len).ok_or_else(|| self.cut_short())
    }

    fn cut_short(&self) -> MidiFileError {
        MidiFileError::TrackCutShort { track: self.track }
    }
}

/// A pitch bend from its two data bytes, the low seven bits first: 0x2000 is no bend.
fn pitch_bend(low: u8, high: u8) -> i16 {
    // Both bytes are below 0x80, so the bend's 14 bits lie in 0..=0x3FFF, and the result in
    // -0x2000..=0x1FFF: the subtraction never wraps.
    (i16::from(high) << 7 | i16::from(low)).wrapping_sub(PITCH_BEND_CENTRE)
}
