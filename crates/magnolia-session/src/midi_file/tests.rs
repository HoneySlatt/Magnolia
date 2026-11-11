//! The bound on a file's events, reached by lowering it rather than by reading 16,777,216 events,
//! which takes seconds in a debug build. The rest of the module is tested through its public
//! interface, in `tests/midi_file.rs`.

use std::error::Error;

use super::read::read_with_event_limit;
use super::{
    EventKind, Format, MAX_EVENTS, MAX_VARIABLE_LENGTH, MidiFile, MidiFileError, Track, TrackEvent,
    write,
};

type TestResult = Result<(), Box<dyn Error>>;

/// The lowered bound, and one event past it.
const LIMIT: u32 = 16;
const PAST_LIMIT: u32 = LIMIT + 1;

/// The largest tick a file of `LIMIT` events holds: each event at the largest delta time after
/// the one before.
const LARGEST_TICK: u64 = LIMIT as u64 * MAX_VARIABLE_LENGTH as u64;

const END_OF_TRACK: [u8; 4] = [0x00, 0xFF, 0x2F, 0x00];

/// A file of format 1 at 96 ticks a quarter, of the tracks' bodies given.
fn file_of_tracks(bodies: &[Vec<u8>]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut file = b"MThd\x00\x00\x00\x06\x00\x01".to_vec();
    file.extend(u16::try_from(bodies.len())?.to_be_bytes());
    file.extend(96_u16.to_be_bytes());
    for body in bodies {
        file.extend(b"MTrk");
        file.extend(u32::try_from(body.len())?.to_be_bytes());
        file.extend(body);
    }
    Ok(file)
}

/// `count` program changes, which the reader drops: the first with its status, the others by
/// running status.
fn program_changes(count: u32) -> Vec<u8> {
    let mut body = Vec::new();
    for index in 0..count {
        if index == 0 {
            body.extend([0x00, 0xC0, 0x00]);
        } else {
            body.extend([0x00, 0x00]);
        }
    }
    body
}

fn with_end_of_track(mut body: Vec<u8>) -> Vec<u8> {
    body.extend(END_OF_TRACK);
    body
}

/// A file whose one track holds one note on, at `tick`.
fn one_note_at(tick: u64) -> MidiFile {
    MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 96,
        tracks: vec![Track {
            events: vec![TrackEvent {
                tick,
                kind: EventKind::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 100,
                },
            }],
        }],
    }
}

/// The tests below lower the bound for speed; this one pins the bound the reader uses.
#[test]
fn the_reader_takes_at_most_16_777_216_events() {
    assert_eq!(MAX_EVENTS, 1 << 24);
}

#[test]
fn a_file_holds_at_most_the_bound_of_events_dropped_ones_included() -> TestResult {
    let at_limit = file_of_tracks(&[with_end_of_track(program_changes(LIMIT))])?;
    assert_eq!(
        read_with_event_limit(&at_limit, LIMIT).map(|file| file.tracks),
        Ok(vec![Track::default()])
    );
    let past_limit = file_of_tracks(&[with_end_of_track(program_changes(PAST_LIMIT))])?;
    assert_eq!(
        read_with_event_limit(&past_limit, LIMIT),
        Err(MidiFileError::TooManyEvents)
    );
    Ok(())
}

#[test]
fn the_bound_counts_every_track_but_not_the_ends_of_track() -> TestResult {
    let half = with_end_of_track(program_changes(LIMIT / 2));
    let empty = END_OF_TRACK.to_vec();
    let at_limit = file_of_tracks(&[half.clone(), empty.clone(), half.clone(), empty])?;
    assert_eq!(
        read_with_event_limit(&at_limit, LIMIT).map(|file| file.tracks.len()),
        Ok(4)
    );
    let one_more = with_end_of_track(program_changes(1));
    let past_limit = file_of_tracks(&[half.clone(), half, one_more])?;
    assert_eq!(
        read_with_event_limit(&past_limit, LIMIT),
        Err(MidiFileError::TooManyEvents)
    );
    Ok(())
}

#[test]
fn the_largest_tick_a_file_holds_round_trips_and_one_more_is_refused() {
    // The writer bridges the gap with LIMIT - 1 empty text events, so the file holds LIMIT
    // events.
    let file = one_note_at(LARGEST_TICK);
    assert_eq!(read_with_event_limit(&write(&file), LIMIT), Ok(file));
    assert_eq!(
        read_with_event_limit(&write(&one_note_at(LARGEST_TICK.saturating_add(1))), LIMIT),
        Err(MidiFileError::TooManyEvents)
    );
}

#[test]
fn a_file_at_the_bound_without_ends_of_track_round_trips() -> TestResult {
    // LIMIT - 1 dropped events at the largest delta time, then a note on: the writer bridges the
    // same gap with as many text events, and adds the end of track, which the bound leaves aside.
    let mut body = Vec::new();
    for _ in 1..LIMIT {
        body.extend([0xFF, 0xFF, 0xFF, 0x7F, 0xF0, 0x01, 0xF7]);
    }
    body.extend([0xFF, 0xFF, 0xFF, 0x7F, 0x90, 60, 100]);
    let read = read_with_event_limit(&file_of_tracks(&[body])?, LIMIT)?;
    assert_eq!(
        read,
        MidiFile {
            format: Format::Parallel,
            ..one_note_at(LARGEST_TICK)
        }
    );
    assert_eq!(read_with_event_limit(&write(&read), LIMIT), Ok(read));
    Ok(())
}
