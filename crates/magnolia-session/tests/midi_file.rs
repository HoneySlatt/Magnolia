//! The MIDI file reader and writer through their public interface: round trips of both formats and
//! every event kind, ticks past what a delta time holds, files built by hand for running status
//! and the cases fuzzing finds only by luck, and the plain errors of malformed files. The bound on
//! a file's events is tested inside the module (`src/midi_file/tests.rs`).

use magnolia_session::midi_file::{
    self, EventKind, Format, MidiFile, MidiFileError, Track, TrackEvent,
};

/// The largest delta time, the largest 4-byte variable-length quantity.
const MAX_DELTA: u64 = 0x0FFF_FFFF;

/// An end of track at a delta time of 0.
const END_OF_TRACK: [u8; 4] = [0x00, 0xFF, 0x2F, 0x00];

// Building files by hand.

/// A chunk: its id, its body's length, big-endian, and its body.
fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len()).expect("a small chunk");
    [id.as_slice(), &length.to_be_bytes(), body].concat()
}

/// An `MThd` chunk of 6 bytes.
fn header(format: u16, track_count: u16, division: u16) -> Vec<u8> {
    let body = [
        format.to_be_bytes(),
        track_count.to_be_bytes(),
        division.to_be_bytes(),
    ]
    .concat();
    chunk(b"MThd", &body)
}

/// A file of format 1 at 960 ticks a quarter, its header counting the `MTrk` chunks given.
fn file_of_tracks(track_bodies: &[&[u8]]) -> Vec<u8> {
    let count = u16::try_from(track_bodies.len()).expect("a few tracks");
    let mut file = header(1, count, 960);
    for body in track_bodies {
        file.extend(chunk(b"MTrk", body));
    }
    file
}

/// The events of a file of one track, its body as given.
fn read_track(body: &[u8]) -> Result<Vec<TrackEvent>, MidiFileError> {
    let file = midi_file::read(&file_of_tracks(&[body]))?;
    let [track] = <[Track; 1]>::try_from(file.tracks).expect("one track");
    Ok(track.events)
}

fn at(tick: u64, kind: EventKind) -> TrackEvent {
    TrackEvent { tick, kind }
}

fn note_on(channel: u8, key: u8, velocity: u8) -> EventKind {
    EventKind::NoteOn {
        channel,
        key,
        velocity,
    }
}

fn note_off(channel: u8, key: u8, velocity: u8) -> EventKind {
    EventKind::NoteOff {
        channel,
        key,
        velocity,
    }
}

fn controller(channel: u8, number: u8, value: u8) -> EventKind {
    EventKind::Controller {
        channel,
        number,
        value,
    }
}

fn tempo(microseconds_per_quarter: u32) -> EventKind {
    EventKind::Tempo {
        microseconds_per_quarter,
    }
}

fn round_trip(file: &MidiFile) -> MidiFile {
    midi_file::read(&midi_file::write(file)).expect("a written file reads")
}

// Round trips.

/// One track of every event kind at the ends of its range, running status in use.
fn every_kind_of_event() -> Track {
    Track {
        events: vec![
            at(0, tempo(500_000)),
            at(
                0,
                EventKind::TimeSignature {
                    numerator: 7,
                    denominator_power: 3,
                },
            ),
            at(0, note_on(0, 60, 100)),
            at(0, note_on(0, 64, 1)),
            at(0, note_on(0, 67, 127)),
            at(480, note_off(0, 60, 0)),
            at(480, note_off(0, 64, 64)),
            at(480, note_off(0, 67, 127)),
            at(500, controller(15, 7, 0)),
            at(500, controller(15, 127, 127)),
            at(
                600,
                EventKind::PitchBend {
                    channel: 3,
                    value: -8_192,
                },
            ),
            at(
                601,
                EventKind::PitchBend {
                    channel: 3,
                    value: 0,
                },
            ),
            at(
                602,
                EventKind::PitchBend {
                    channel: 3,
                    value: 8_191,
                },
            ),
            at(700, tempo(1)),
            at(700, tempo(0x00FF_FFFF)),
            at(700, note_on(9, 0, 127)),
            at(
                701,
                EventKind::TimeSignature {
                    numerator: 255,
                    denominator_power: 0,
                },
            ),
            at(701, note_on(9, 127, 127)),
            at(10_000, note_off(9, 0, 0)),
            at(10_000, note_off(9, 127, 0)),
        ],
    }
}

#[test]
fn a_single_track_file_of_every_event_kind_round_trips() {
    let file = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 960,
        tracks: vec![every_kind_of_event()],
    };
    assert_eq!(round_trip(&file), file);
}

#[test]
fn a_parallel_file_of_several_tracks_round_trips() {
    let file = MidiFile {
        format: Format::Parallel,
        ticks_per_quarter: 32_767,
        tracks: vec![
            Track {
                events: vec![at(0, tempo(600_000)), at(3_840, tempo(400_000))],
            },
            every_kind_of_event(),
            Track::default(),
            Track {
                events: vec![at(1, note_on(1, 1, 1)), at(2, note_off(1, 1, 1))],
            },
        ],
    };
    assert_eq!(round_trip(&file), file);
}

#[test]
fn a_file_without_tracks_and_one_tick_a_quarter_round_trips() {
    for format in [Format::SingleTrack, Format::Parallel] {
        let file = MidiFile {
            format,
            ticks_per_quarter: 1,
            tracks: Vec::new(),
        };
        assert_eq!(round_trip(&file), file);
    }
}

#[test]
fn the_writer_writes_the_layout_with_running_status_cancelled_by_meta_events() {
    let file = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 96,
        tracks: vec![Track {
            events: vec![
                at(0, note_on(0, 60, 100)),
                at(0, note_on(0, 64, 100)),
                at(96, tempo(0x07_A1_20)),
                at(96, note_on(0, 67, 100)),
                at(200, note_off(0, 60, 64)),
                at(200, note_off(0, 64, 64)),
                at(
                    200,
                    EventKind::PitchBend {
                        channel: 0,
                        value: 0,
                    },
                ),
            ],
        }],
    };
    let track = [
        // Two notes on, the second by running status.
        [0x00, 0x90, 60, 100, 0x00, 64, 100].as_slice(),
        // A tempo change, which cancels running status, so the next note gives it again.
        &[0x60, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20],
        &[0x00, 0x90, 67, 100],
        // Notes off as 0x80, at a delta of 104, then a pitch bend at its centre, 0x2000.
        &[0x68, 0x80, 60, 64, 0x00, 64, 64],
        &[0x00, 0xE0, 0x00, 0x40],
        &END_OF_TRACK,
    ]
    .concat();
    let expected = [header(0, 1, 96), chunk(b"MTrk", &track)].concat();
    assert_eq!(midi_file::write(&file), expected);
}

#[test]
fn ticks_around_the_32_bit_limit_round_trip() {
    let u32_limit = u64::from(u32::MAX);
    let file = MidiFile {
        format: Format::Parallel,
        ticks_per_quarter: 15_360,
        tracks: vec![Track {
            events: vec![
                at(MAX_DELTA, note_on(0, 60, 100)),
                at(MAX_DELTA + 1, note_off(0, 60, 0)),
                at(u32_limit - 1, note_on(0, 61, 100)),
                at(u32_limit, note_off(0, 61, 0)),
                at(u32_limit + 1, note_on(0, 62, 100)),
                at(u32_limit + 1, note_off(0, 62, 0)),
                at(1 << 40, tempo(500_000)),
            ],
        }],
    };
    assert_eq!(round_trip(&file), file);
}

#[test]
fn a_gap_longer_than_a_delta_time_is_bridged_by_empty_text_events() {
    let one_event_at = |tick| MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 960,
        tracks: vec![Track {
            events: vec![at(tick, controller(0, 1, 2))],
        }],
    };
    let track = |delta_times: &[u8]| {
        let body = [delta_times, &[0xB0, 1, 2], &END_OF_TRACK].concat();
        [header(0, 1, 960), chunk(b"MTrk", &body)].concat()
    };
    // The largest delta time, 0x0FFFFFFF, written in four bytes, needs no text event.
    assert_eq!(
        midi_file::write(&one_event_at(MAX_DELTA)),
        track(&[0xFF, 0xFF, 0xFF, 0x7F])
    );
    // One tick more takes an empty text event at the largest delta time, then a delta of 1.
    assert_eq!(
        midi_file::write(&one_event_at(MAX_DELTA + 1)),
        track(&[0xFF, 0xFF, 0xFF, 0x7F, 0xFF, 0x01, 0x00, 0x01])
    );
}

// Running status, built by hand.

#[test]
fn running_status_carries_across_a_note_on_and_a_note_off_given_as_a_note_on() {
    let events = read_track(
        &[
            [0x00, 0x91, 60, 100].as_slice(),
            &[0x60, 60, 0],
            &[0x00, 0x81, 62, 70],
            &[0x10, 62, 71],
            &END_OF_TRACK,
        ]
        .concat(),
    );
    assert_eq!(
        events,
        Ok(vec![
            at(0, note_on(1, 60, 100)),
            at(96, note_off(1, 60, 0)),
            at(96, note_off(1, 62, 70)),
            at(112, note_off(1, 62, 71)),
        ])
    );
}

#[test]
fn a_note_on_of_velocity_0_reads_as_a_note_off() {
    let events = read_track(&[[0x05, 0x9F, 127, 0].as_slice(), &END_OF_TRACK].concat());
    assert_eq!(events, Ok(vec![at(5, note_off(15, 127, 0))]));
}

#[test]
fn running_status_carries_across_meta_events() {
    let events = read_track(
        &[
            [0x00, 0xB2, 7, 100].as_slice(),
            // A tempo change, a track name and a time signature, then controllers by running
            // status.
            &[0x00, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40],
            &[0x00, 7, 90],
            &[0x00, 0xFF, 0x03, 0x04, b'B', b'a', b's', b's'],
            &[0x00, 7, 80],
            &[0x0A, 0xFF, 0x58, 0x04, 0x03, 0x02, 0x18, 0x08],
            &[0x00, 7, 70],
            &END_OF_TRACK,
        ]
        .concat(),
    );
    assert_eq!(
        events,
        Ok(vec![
            at(0, controller(2, 7, 100)),
            at(0, tempo(1_000_000)),
            at(0, controller(2, 7, 90)),
            at(0, controller(2, 7, 80)),
            at(
                10,
                EventKind::TimeSignature {
                    numerator: 3,
                    denominator_power: 2,
                }
            ),
            at(10, controller(2, 7, 70)),
        ])
    );
}

#[test]
fn a_system_exclusive_event_cancels_running_status() {
    for status in [0xF0, 0xF7] {
        let events = read_track(
            &[
                [0x00, 0x90, 60, 100].as_slice(),
                &[0x00, status, 0x03, 0x7E, 0x09, 0xF7],
                &[0x00, 60, 0],
                &END_OF_TRACK,
            ]
            .concat(),
        );
        assert_eq!(events, Err(MidiFileError::MissingStatus { track: 1 }));
    }
}

#[test]
fn running_status_starts_afresh_in_each_track() {
    let first = [[0x00, 0x90, 60, 100].as_slice(), &END_OF_TRACK].concat();
    let second = [[0x00, 60, 0].as_slice(), &END_OF_TRACK].concat();
    assert_eq!(
        midi_file::read(&file_of_tracks(&[&first, &second])),
        Err(MidiFileError::MissingStatus { track: 2 })
    );
}

#[test]
fn a_track_cannot_start_with_a_data_byte() {
    let events = read_track(&[[0x00, 60, 100].as_slice(), &END_OF_TRACK].concat());
    assert_eq!(events, Err(MidiFileError::MissingStatus { track: 1 }));
}

#[test]
fn dropped_events_keep_their_delta_times_and_running_status() {
    let events = read_track(
        &[
            // A program change, then one by running status.
            [0x01, 0xC0, 5].as_slice(),
            &[0x01, 6],
            // Channel pressure, key pressure, system-exclusive data, a text event.
            &[0x01, 0xD0, 64],
            &[0x01, 0xA0, 60, 64],
            &[0x01, 0xF0, 0x02, 0x01, 0xF7],
            &[0x01, 0xFF, 0x01, 0x02, b'h', b'i'],
            // A note on after the drops, then one by running status.
            &[0x01, 0x90, 60, 100],
            &[0x01, 62, 100],
            &END_OF_TRACK,
        ]
        .concat(),
    );
    assert_eq!(
        events,
        Ok(vec![at(7, note_on(0, 60, 100)), at(8, note_on(0, 62, 100))])
    );
}

// Chunks and the header.

#[test]
fn an_unknown_chunk_before_a_track_is_skipped() {
    let track = [[0x00, 0x90, 60, 100].as_slice(), &END_OF_TRACK].concat();
    let file = [
        header(0, 1, 480),
        chunk(b"XFIH", b"anything, even MTrk"),
        chunk(b"MTrk", &track),
    ]
    .concat();
    assert_eq!(
        midi_file::read(&file),
        Ok(MidiFile {
            format: Format::SingleTrack,
            ticks_per_quarter: 480,
            tracks: vec![Track {
                events: vec![at(0, note_on(0, 60, 100))],
            }],
        })
    );
}

#[test]
fn a_header_longer_than_6_bytes_has_its_extra_bytes_skipped() {
    let body = [0x00, 0x01, 0x00, 0x00, 0x01, 0xE0, 0xAA, 0xBB];
    let file = chunk(b"MThd", &body);
    assert_eq!(
        midi_file::read(&file),
        Ok(MidiFile {
            format: Format::Parallel,
            ticks_per_quarter: 480,
            tracks: Vec::new(),
        })
    );
}

#[test]
fn a_header_shorter_than_6_bytes_is_refused() {
    let file = chunk(b"MThd", &[0x00, 0x00, 0x00, 0x00, 0x01]);
    assert_eq!(
        midi_file::read(&file),
        Err(MidiFileError::HeaderTooShort(5))
    );
}

#[test]
fn a_file_that_does_not_start_with_its_header_is_not_a_midi_file() {
    for file in [
        Vec::new(),
        b"MTh".to_vec(),
        b"RIFF\x00\x00\x00\x00RMID".to_vec(),
        chunk(b"MTrk", &END_OF_TRACK),
    ] {
        assert_eq!(midi_file::read(&file), Err(MidiFileError::NotMidiFile));
    }
}

#[test]
fn an_smpte_division_is_refused() {
    // 25 frames a second (-25 in the high byte), 40 ticks a frame.
    let division = u16::from_be_bytes([0xE7, 40]);
    assert_eq!(
        midi_file::read(&header(1, 0, division)),
        Err(MidiFileError::TimecodeDivision(division))
    );
    assert_eq!(
        midi_file::read(&header(1, 0, 0x8000)),
        Err(MidiFileError::TimecodeDivision(0x8000))
    );
}

#[test]
fn a_division_of_0_ticks_is_refused() {
    assert_eq!(
        midi_file::read(&header(1, 0, 0)),
        Err(MidiFileError::ZeroTicksPerQuarter)
    );
}

#[test]
fn format_2_and_unknown_formats_are_refused() {
    for format in [2, 3, u16::MAX] {
        assert_eq!(
            midi_file::read(&header(format, 0, 480)),
            Err(MidiFileError::UnsupportedFormat(format))
        );
    }
}

#[test]
fn a_single_track_file_whose_header_counts_two_tracks_is_read_as_it_says() {
    let track = [[0x00, 0x90, 60, 100].as_slice(), &END_OF_TRACK].concat();
    let file = [
        header(0, 2, 96),
        chunk(b"MTrk", &track),
        chunk(b"MTrk", &END_OF_TRACK),
    ]
    .concat();
    let read = midi_file::read(&file).expect("a file read as its header says");
    assert_eq!(read.format, Format::SingleTrack);
    assert_eq!(read.tracks.len(), 2);
    assert_eq!(round_trip(&read), read);
}

#[test]
fn fewer_tracks_than_the_header_counts_is_an_error() {
    let file = [
        header(1, 3, 96),
        chunk(b"MTrk", &END_OF_TRACK),
        chunk(b"JUNK", &[]),
    ]
    .concat();
    assert_eq!(
        midi_file::read(&file),
        Err(MidiFileError::MissingTracks {
            expected: 3,
            found: 1,
        })
    );
}

#[test]
fn tracks_past_the_headers_count_are_not_read() {
    let file = [
        header(1, 1, 96),
        chunk(b"MTrk", &END_OF_TRACK),
        // Malformed, but never read.
        chunk(b"MTrk", &[0x00, 60]),
        b"garbage".to_vec(),
    ]
    .concat();
    assert_eq!(
        midi_file::read(&file).map(|file| file.tracks),
        Ok(vec![Track::default()])
    );
}

#[test]
fn a_track_whose_length_runs_past_the_file_is_truncated() {
    let mut file = file_of_tracks(&[&[0x00, 0x90, 60, 100, 0x00, 0xFF, 0x2F, 0x00]]);
    file.truncate(file.len() - 1);
    assert_eq!(midi_file::read(&file), Err(MidiFileError::Truncated));
}

#[test]
fn a_file_cut_inside_a_chunks_header_is_truncated() {
    let mut file = file_of_tracks(&[&END_OF_TRACK]);
    // Inside the MTrk chunk's length, then inside the header's.
    file.truncate(header(1, 1, 960).len() + 6);
    assert_eq!(midi_file::read(&file), Err(MidiFileError::Truncated));
    assert_eq!(
        midi_file::read(&header(1, 0, 960)[..10]),
        Err(MidiFileError::Truncated)
    );
}

// Tracks.

#[test]
fn a_track_without_an_end_of_track_ends_with_its_chunk() {
    let events = read_track(&[0x00, 0x90, 60, 100, 0x10, 60, 0]);
    assert_eq!(
        events,
        Ok(vec![at(0, note_on(0, 60, 100)), at(16, note_off(0, 60, 0))])
    );
}

#[test]
fn bytes_after_the_end_of_track_are_not_read() {
    let events = read_track(
        &[
            [0x00, 0x90, 60, 100].as_slice(),
            &[0x00, 0xFF, 0x2F, 0x01, 0x00],
            &[0x00, 60, 0, 0xF1, 0xFF],
        ]
        .concat(),
    );
    assert_eq!(events, Ok(vec![at(0, note_on(0, 60, 100))]));
}

#[test]
fn a_truncated_variable_length_quantity_cuts_its_track_short() {
    for body in [
        [0x81].as_slice(),
        &[0xFF, 0xFF, 0xFF],
        &[0x00, 0xFF, 0x01, 0x82],
        &[0x00, 0xF0, 0x81, 0x80],
    ] {
        assert_eq!(
            read_track(body),
            Err(MidiFileError::TrackCutShort { track: 1 })
        );
    }
}

#[test]
fn a_variable_length_quantity_of_5_bytes_is_refused() {
    for body in [
        [0x80, 0x80, 0x80, 0x80, 0x00, 0x90, 60, 100].as_slice(),
        &[0x00, 0xFF, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F],
        &[0x00, 0xF0, 0x81, 0x80, 0x80, 0x80, 0x00],
    ] {
        assert_eq!(
            read_track(body),
            Err(MidiFileError::VariableLengthTooLong { track: 1 })
        );
    }
}

#[test]
fn an_event_that_runs_past_its_track_cuts_it_short() {
    for body in [
        // A note on without its velocity, a delta time without its event, a meta event without its
        // type, a text event and a system-exclusive event longer than the track.
        [0x00, 0x90, 60].as_slice(),
        &[0x00, 0x90, 60, 100, 0x00],
        &[0x00, 0xFF],
        &[0x00, 0xFF, 0x01, 0x05, b'a'],
        &[0x00, 0xF0, 0x05, 0x01, 0xF7],
    ] {
        assert_eq!(
            read_track(body),
            Err(MidiFileError::TrackCutShort { track: 1 })
        );
    }
}

#[test]
fn a_status_byte_inside_a_channel_message_is_refused() {
    assert_eq!(
        read_track(&[0x00, 0x90, 60, 0x80, 0x00]),
        Err(MidiFileError::StatusInsideMessage {
            track: 1,
            status: 0x80,
        })
    );
    assert_eq!(
        read_track(&[0x00, 0xE0, 0xFF, 0x40]),
        Err(MidiFileError::StatusInsideMessage {
            track: 1,
            status: 0xFF,
        })
    );
}

#[test]
fn system_common_and_real_time_status_bytes_are_refused() {
    for status in [0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF8, 0xFA, 0xFE] {
        assert_eq!(
            read_track(&[0x00, status, 0x00, 0x00]),
            Err(MidiFileError::UnsupportedStatus { track: 1, status })
        );
    }
}

#[test]
fn a_tempo_or_time_signature_shorter_than_the_standard_is_refused() {
    assert_eq!(
        read_track(&[0x00, 0xFF, 0x51, 0x02, 0x07, 0xA1]),
        Err(MidiFileError::MetaTooShort {
            track: 1,
            meta_type: 0x51,
            length: 2,
        })
    );
    assert_eq!(
        read_track(&[0x00, 0xFF, 0x58, 0x03, 0x04, 0x02, 0x18]),
        Err(MidiFileError::MetaTooShort {
            track: 1,
            meta_type: 0x58,
            length: 3,
        })
    );
}

#[test]
fn a_tempo_or_time_signature_longer_than_the_standard_has_its_extra_bytes_skipped() {
    let events = read_track(
        &[
            [0x00, 0xFF, 0x51, 0x04, 0x07, 0xA1, 0x20, 0x99].as_slice(),
            &[0x00, 0xFF, 0x58, 0x05, 0x06, 0x03, 0x24, 0x08, 0x99],
            &END_OF_TRACK,
        ]
        .concat(),
    );
    assert_eq!(
        events,
        Ok(vec![
            at(0, tempo(500_000)),
            at(
                0,
                EventKind::TimeSignature {
                    numerator: 6,
                    denominator_power: 3,
                }
            ),
        ])
    );
}

#[test]
fn a_pitch_bend_reads_its_low_seven_bits_first() {
    let bend = |value| EventKind::PitchBend { channel: 4, value };
    let events = read_track(
        &[
            [0x00, 0xE4, 0x00, 0x00].as_slice(),
            &[0x00, 0x00, 0x40],
            &[0x00, 0x7F, 0x7F],
            &[0x00, 0x01, 0x40],
            &[0x00, 0x7F, 0x3F],
            &END_OF_TRACK,
        ]
        .concat(),
    );
    assert_eq!(
        events,
        Ok(vec![
            at(0, bend(-8_192)),
            at(0, bend(0)),
            at(0, bend(8_191)),
            at(0, bend(1)),
            at(0, bend(-1)),
        ])
    );
}

// The writer, given what the reader never returns.

#[test]
fn the_writer_clamps_fields_out_of_their_range() {
    let file = MidiFile {
        format: Format::Parallel,
        ticks_per_quarter: 0,
        tracks: vec![Track {
            events: vec![
                at(0, note_on(16, 128, 255)),
                at(0, note_off(255, 200, 128)),
                at(0, controller(99, 255, 128)),
                at(
                    0,
                    EventKind::PitchBend {
                        channel: 16,
                        value: i16::MIN,
                    },
                ),
                at(
                    0,
                    EventKind::PitchBend {
                        channel: 0,
                        value: i16::MAX,
                    },
                ),
                at(0, tempo(u32::MAX)),
            ],
        }],
    };
    assert_eq!(
        round_trip(&file),
        MidiFile {
            format: Format::Parallel,
            ticks_per_quarter: 1,
            tracks: vec![Track {
                events: vec![
                    at(0, note_on(15, 127, 127)),
                    at(0, note_off(15, 127, 127)),
                    at(0, controller(15, 127, 127)),
                    at(
                        0,
                        EventKind::PitchBend {
                            channel: 15,
                            value: -8_192,
                        },
                    ),
                    at(
                        0,
                        EventKind::PitchBend {
                            channel: 0,
                            value: 8_191,
                        },
                    ),
                    at(0, tempo(0x00FF_FFFF)),
                ],
            }],
        }
    );
    let too_many_ticks = MidiFile {
        ticks_per_quarter: 0x8000,
        tracks: Vec::new(),
        ..file
    };
    assert_eq!(round_trip(&too_many_ticks).ticks_per_quarter, 0x7FFF);
}

#[test]
fn the_writer_writes_a_note_on_of_velocity_0_as_it_is_which_reads_as_a_note_off() {
    let file = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 96,
        tracks: vec![Track {
            events: vec![at(3, note_on(2, 60, 0))],
        }],
    };
    assert_eq!(
        round_trip(&file).tracks,
        vec![Track {
            events: vec![at(3, note_off(2, 60, 0))],
        }]
    );
}

#[test]
fn the_writer_writes_an_event_earlier_than_the_one_before_at_that_ones_tick() {
    let file = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 96,
        tracks: vec![Track {
            events: vec![
                at(100, note_on(0, 60, 100)),
                at(50, note_off(0, 60, 0)),
                at(120, note_on(0, 61, 100)),
            ],
        }],
    };
    assert_eq!(
        round_trip(&file).tracks,
        vec![Track {
            events: vec![
                at(100, note_on(0, 60, 100)),
                at(100, note_off(0, 60, 0)),
                at(120, note_on(0, 61, 100)),
            ],
        }]
    );
}

#[test]
fn the_writer_writes_at_most_65535_tracks() {
    let file = MidiFile {
        format: Format::Parallel,
        ticks_per_quarter: 96,
        tracks: vec![Track::default(); 65_536],
    };
    let read = round_trip(&file);
    assert_eq!(read.tracks.len(), 65_535);
}

#[test]
fn errors_read_as_plain_sentences() {
    assert_eq!(
        MidiFileError::MissingStatus { track: 2 }.to_string(),
        "track 2 of the MIDI file starts an event with a data byte where no running status holds"
    );
    assert_eq!(
        MidiFileError::UnsupportedFormat(2).to_string(),
        "the MIDI file is of format 2; Magnolia reads formats 0 and 1"
    );
}
