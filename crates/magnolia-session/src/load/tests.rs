//! A MIDI file made into a clip: ticks scaled from common and uncommon divisions, notes paired
//! first in first out across tracks, notes without their note off, and values scaled.

use std::error::Error;

use magnolia_engine::engine::{ControllerPoint, MidiClip, MidiNote, PitchBendPoint};
use magnolia_engine::time::{MusicalPosition, TICKS_PER_QUARTER};

use super::midi::{TicksOutOfRange, clip_from_file};
use crate::midi_file::{EventKind, Format, MidiFile, Track, TrackEvent};

fn on(tick: u64, channel: u8, key: u8, velocity: u8) -> TrackEvent {
    TrackEvent {
        tick,
        kind: EventKind::NoteOn {
            channel,
            key,
            velocity,
        },
    }
}

fn off(tick: u64, channel: u8, key: u8) -> TrackEvent {
    TrackEvent {
        tick,
        kind: EventKind::NoteOff {
            channel,
            key,
            velocity: 64,
        },
    }
}

fn file(ticks_per_quarter: u16, tracks: Vec<Vec<TrackEvent>>) -> MidiFile {
    MidiFile {
        format: Format::Parallel,
        ticks_per_quarter,
        tracks: tracks.into_iter().map(|events| Track { events }).collect(),
    }
}

type TestResult = Result<(), Box<dyn Error>>;

/// A clip from tick 0 that lasts long enough for every event of these tests.
fn clip_of(file: &MidiFile) -> Result<MidiClip, Box<dyn Error>> {
    clip_from_file(file, MusicalPosition(0), 1_000 * TICKS_PER_QUARTER)
        .map_err(|_| "a tick out of range".into())
}

fn note(tick: i64, length: i64, channel: u8, key: u8, velocity: f32) -> MidiNote {
    MidiNote {
        tick,
        length,
        channel,
        key,
        velocity,
    }
}

#[test]
fn common_divisions_scale_exactly_and_others_round_up() -> TestResult {
    // A note from tick 1 to tick 3 of the file, then one from a quarter to five quarters.
    let events = |division: u64| {
        vec![
            on(1, 0, 60, 127),
            off(3, 0, 60),
            on(division, 0, 62, 127),
            off(5 * division, 0, 62),
        ]
    };
    for division in [96_u16, 480, 960] {
        let clip = clip_of(&file(division, vec![events(u64::from(division))]))?;
        let tick = TICKS_PER_QUARTER / i64::from(division);
        assert_eq!(
            clip.notes,
            [
                note(tick, 2 * tick, 0, 60, 1.0),
                note(TICKS_PER_QUARTER, 4 * TICKS_PER_QUARTER, 0, 62, 1.0),
            ],
            "{division} ticks a quarter"
        );
    }
    // 1,000 does not divide 4,838,400: tick 1 lies at 4,838.4, placed on 4,839; tick 3 at
    // 14,515.2, placed on 14,516. A quarter, 1,000 ticks, is exact.
    let clip = clip_of(&file(1_000, vec![events(1_000)]))?;
    assert_eq!(
        clip.notes,
        [
            note(4_839, 14_516 - 4_839, 0, 60, 1.0),
            note(TICKS_PER_QUARTER, 4 * TICKS_PER_QUARTER, 0, 62, 1.0),
        ]
    );
    Ok(())
}

#[test]
fn a_repeated_note_ends_its_earliest_strike_first() -> TestResult {
    // Key 60 struck twice before either ends, then struck again on the tick that ends one: the
    // note offs end the strikes in their order.
    let events = vec![
        on(0, 0, 60, 100),
        on(10, 0, 60, 90),
        off(20, 0, 60),
        off(30, 0, 60),
        off(40, 0, 60),
        on(40, 0, 60, 80),
        off(50, 0, 60),
    ];
    let clip = clip_of(&file(960, vec![events]))?;
    let tick = TICKS_PER_QUARTER / 960;
    assert_eq!(
        clip.notes,
        [
            note(0, 20 * tick, 0, 60, 100.0 / 127.0),
            note(10 * tick, 20 * tick, 0, 60, 90.0 / 127.0),
            note(40 * tick, 10 * tick, 0, 60, 80.0 / 127.0),
        ],
        "the third note off ends nothing; the last note on starts after it"
    );
    Ok(())
}

#[test]
fn notes_pair_per_channel_and_key_across_the_files_tracks() -> TestResult {
    // The same key on two channels, struck in one track and ended in another.
    let first = vec![on(0, 0, 64, 127), on(0, 1, 64, 127)];
    let second = vec![off(5, 1, 64), off(9, 0, 64)];
    let clip = clip_of(&file(960, vec![first, second]))?;
    let tick = TICKS_PER_QUARTER / 960;
    assert_eq!(
        clip.notes,
        [note(0, 9 * tick, 0, 64, 1.0), note(0, 5 * tick, 1, 64, 1.0)]
    );
    Ok(())
}

#[test]
fn a_note_without_its_note_off_ends_at_the_clips_end() -> TestResult {
    let quarter = TICKS_PER_QUARTER;
    let events = vec![
        on(0, 0, 60, 127),
        on(960, 0, 62, 127),
        on(4 * 960, 0, 64, 127),
    ];
    let clip = clip_from_file(&file(960, vec![events]), MusicalPosition(7), 4 * quarter)
        .map_err(|_| "a tick out of range")?;
    assert_eq!(clip.start, MusicalPosition(7));
    assert_eq!(
        clip.notes,
        [
            note(0, 4 * quarter, 0, 60, 1.0),
            note(quarter, 3 * quarter, 0, 62, 1.0)
        ],
        "the note struck on the clip's end would last no tick: it does not play"
    );
    Ok(())
}

#[test]
fn a_note_that_lasts_no_tick_does_not_play() -> TestResult {
    // On 1,000 ticks a quarter, ticks 1 and 1 end on one tick; the note off still ends the note.
    let events = vec![on(1, 0, 60, 127), off(1, 0, 60), off(2, 0, 60)];
    let clip = clip_of(&file(1_000, vec![events]))?;
    assert_eq!(clip.notes, []);
    Ok(())
}

#[test]
fn values_scale_and_tempo_and_meter_are_ignored() -> TestResult {
    let events = vec![
        TrackEvent {
            tick: 0,
            kind: EventKind::Tempo {
                microseconds_per_quarter: 250_000,
            },
        },
        TrackEvent {
            tick: 0,
            kind: EventKind::TimeSignature {
                numerator: 7,
                denominator_power: 3,
            },
        },
        TrackEvent {
            tick: 0,
            kind: EventKind::PitchBend {
                channel: 2,
                value: -8_192,
            },
        },
        TrackEvent {
            tick: 1,
            kind: EventKind::PitchBend {
                channel: 2,
                value: 8_191,
            },
        },
        TrackEvent {
            tick: 2,
            kind: EventKind::Controller {
                channel: 3,
                number: 64,
                value: 127,
            },
        },
        TrackEvent {
            tick: 2,
            kind: EventKind::Controller {
                channel: 3,
                number: 1,
                value: 0,
            },
        },
    ];
    let clip = clip_of(&file(96, vec![events]))?;
    let tick = TICKS_PER_QUARTER / 96;
    assert_eq!(
        clip.pitch_bends,
        [
            PitchBendPoint {
                tick: 0,
                channel: 2,
                value: -1.0
            },
            PitchBendPoint {
                tick,
                channel: 2,
                value: 8_191.0 / 8_192.0
            },
        ]
    );
    assert_eq!(
        clip.controllers,
        [
            ControllerPoint {
                tick: 2 * tick,
                channel: 3,
                number: 64,
                value: 1.0
            },
            ControllerPoint {
                tick: 2 * tick,
                channel: 3,
                number: 1,
                value: 0.0
            },
        ]
    );
    assert_eq!(clip.notes, []);
    Ok(())
}

#[test]
fn a_tick_past_the_ticks_range_is_refused() {
    // A file's ticks stay below 2^52; scaled from 1 tick a quarter they pass 2^63.
    let events = vec![on(1 << 51, 0, 60, 127)];
    assert_eq!(
        clip_from_file(&file(1, vec![events]), MusicalPosition(0), 1),
        Err(TicksOutOfRange)
    );
}
