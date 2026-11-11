//! Writing a Standard MIDI File.

use super::{
    CONTROLLER, END_OF_TRACK, EventKind, HEADER_BYTES, MAX_CHANNEL, MAX_DATA_BYTE, MAX_PITCH_BEND,
    MAX_TEMPO, MAX_TICKS_PER_QUARTER, MAX_VARIABLE_LENGTH, META, MIN_PITCH_BEND, MidiFile,
    NOTE_OFF, NOTE_ON, PITCH_BEND, PITCH_BEND_CENTRE, TEMPO, TEXT, TIME_SIGNATURE, Track,
};

/// The MIDI clocks per metronome click, and the thirty-second notes per quarter note, that a time
/// signature is written with: a click every quarter note, a quarter note of 8 thirty-second notes.
const CLOCKS_PER_CLICK: u8 = 24;
const THIRTY_SECONDS_PER_QUARTER: u8 = 8;

/// Writes `file` as a Standard MIDI File.
///
/// The layout: an `MThd` chunk of 6 bytes, which gives `file.format`, the track count and the
/// ticks per quarter note; then one `MTrk` chunk per track, in order. Each event is written as
/// the delta time from the event before, then the event. Running status leaves out the status
/// byte of a channel message that repeats the status of the message before; every meta event
/// cancels it, as the standard says. A note off is written as a note off (0x80). A time signature
/// is written with 24 MIDI clocks a click and 8 thirty-second notes a quarter. An end of track
/// ends each track, at the tick of its last event.
///
/// A delta time holds at most 2^28 − 1 ticks. A longer gap between two events is bridged by empty
/// text events (meta 0x01), each 2^28 − 1 ticks after the one before, which readers skip.
///
/// What [`read`](super::read) never returns is written as follows. A field out of its range is
/// clamped into it: a channel to 15; a key, a velocity, a controller's number or value to 127; a
/// pitch bend to -8,192..=8,191; a tempo to 16,777,215 microseconds; the ticks per quarter note to
/// 1..=32,767. A note on of velocity 0 is written as it is, and reads back as a note off. An event
/// earlier than the one before it is written at that one's tick. Tracks past the 65,535th are not
/// written, as the header cannot count them.
///
/// Limits: the reader takes at most 16,777,216 events. A file that holds more, bridging text
/// events included, is written but does not read back; so is a tick past 2^52 − 2^24, which takes
/// more than 16,777,216 delta times of 2^28 − 1 ticks. Each bridging event takes 7 bytes, so a tick near 2^64 would take
/// hundreds of gigabytes: the caller keeps its ticks within what a file holds. A track's chunk past
/// 4 GiB, which needs hundreds of millions of events, gets a wrong length.
pub fn write(file: &MidiFile) -> Vec<u8> {
    let track_count = u16::try_from(file.tracks.len()).unwrap_or(u16::MAX);
    let ticks_per_quarter = file.ticks_per_quarter.clamp(1, MAX_TICKS_PER_QUARTER);
    let mut out = Vec::new();
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&HEADER_BYTES.to_be_bytes());
    out.extend_from_slice(&file.format.number().to_be_bytes());
    out.extend_from_slice(&track_count.to_be_bytes());
    out.extend_from_slice(&ticks_per_quarter.to_be_bytes());

    let mut body = Vec::new();
    for track in file.tracks.iter().take(usize::from(track_count)) {
        body.clear();
        write_track(track, &mut body);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(&body);
    }
    out
}

/// Writes the body of a track's `MTrk` chunk into `out`.
fn write_track(track: &Track, out: &mut Vec<u8>) {
    let mut writer = TrackWriter {
        out,
        tick: 0,
        running_status: None,
    };
    for event in &track.events {
        writer.delta_time_to(event.tick);
        writer.event(event.kind);
    }
    // The end of track, at the last event's tick.
    writer.delta_time_to(writer.tick);
    writer.meta_event(END_OF_TRACK, &[]);
}

/// Writes the events of one track.
struct TrackWriter<'a> {
    out: &'a mut Vec<u8>,
    /// The tick of the last event written.
    tick: u64,
    /// The status of the last channel message written, which the next one may leave out; `None`
    /// at the track's start and after a meta event.
    running_status: Option<u8>,
}

impl TrackWriter<'_> {
    /// Writes the delta time from the last event to `tick`, or to the last event's tick when
    /// `tick` lies before it. A gap longer than a delta time holds is bridged by empty text
    /// events.
    fn delta_time_to(&mut self, tick: u64) {
        let mut delta = tick.saturating_sub(self.tick);
        self.tick = self.tick.max(tick);
        while delta > u64::from(MAX_VARIABLE_LENGTH) {
            write_variable_length(self.out, MAX_VARIABLE_LENGTH);
            self.meta_event(TEXT, &[]);
            delta = delta.saturating_sub(u64::from(MAX_VARIABLE_LENGTH));
        }
        // At most MAX_VARIABLE_LENGTH after the loop, so it fits.
        write_variable_length(
            self.out,
            u32::try_from(delta).unwrap_or(MAX_VARIABLE_LENGTH),
        );
    }

    /// Writes an event, after its delta time.
    fn event(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn {
                channel,
                key,
                velocity,
            } => self.channel_message(NOTE_ON, channel, [key, velocity]),
            EventKind::NoteOff {
                channel,
                key,
                velocity,
            } => self.channel_message(NOTE_OFF, channel, [key, velocity]),
            EventKind::Controller {
                channel,
                number,
                value,
            } => self.channel_message(CONTROLLER, channel, [number, value]),
            EventKind::PitchBend { channel, value } => {
                self.channel_message(PITCH_BEND, channel, pitch_bend_bytes(value));
            }
            EventKind::Tempo {
                microseconds_per_quarter,
            } => {
                let [_, high, middle, low] = microseconds_per_quarter.min(MAX_TEMPO).to_be_bytes();
                self.meta_event(TEMPO, &[high, middle, low]);
            }
            EventKind::TimeSignature {
                numerator,
                denominator_power,
            } => self.meta_event(
                TIME_SIGNATURE,
                &[
                    numerator,
                    denominator_power,
                    CLOCKS_PER_CLICK,
                    THIRTY_SECONDS_PER_QUARTER,
                ],
            ),
        }
    }

    /// Writes a channel message of two data bytes: its status byte, unless running status gives
    /// it, then its data.
    fn channel_message(&mut self, message: u8, channel: u8, data: [u8; 2]) {
        let status = message | channel.min(MAX_CHANNEL);
        if self.running_status != Some(status) {
            self.out.push(status);
            self.running_status = Some(status);
        }
        self.out.extend(data.map(|byte| byte.min(MAX_DATA_BYTE)));
    }

    /// Writes a meta event of a few bytes, which cancels running status.
    fn meta_event(&mut self, meta_type: u8, data: &[u8]) {
        self.out.extend_from_slice(&[META, meta_type]);
        // A meta event written here holds at most 4 bytes.
        write_variable_length(self.out, u32::try_from(data.len()).unwrap_or(0));
        self.out.extend_from_slice(data);
        self.running_status = None;
    }
}

/// Writes a variable-length quantity of at most 2^28 − 1: seven bits a byte, the highest first,
/// every byte but the last with its top bit set.
fn write_variable_length(out: &mut Vec<u8>, value: u32) {
    let value = value.min(MAX_VARIABLE_LENGTH);
    let mut started = false;
    for shift in [21, 14, 7] {
        let group = (value >> shift & 0x7F) as u8;
        if group != 0 || started {
            out.push(0x80 | group);
            started = true;
        }
    }
    out.push((value & 0x7F) as u8);
}

/// A pitch bend's two data bytes, the low seven bits first, the bend clamped to its range.
fn pitch_bend_bytes(value: i16) -> [u8; 2] {
    // The clamped bend plus the centre lies in 0..=0x3FFF: the addition never wraps, and both
    // bytes are below 0x80.
    let bits = value
        .clamp(MIN_PITCH_BEND, MAX_PITCH_BEND)
        .wrapping_add(PITCH_BEND_CENTRE);
    [(bits & 0x7F) as u8, (bits >> 7) as u8]
}
