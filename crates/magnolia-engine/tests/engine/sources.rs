//! Timeline sources (ADR-0027): MIDI sequences on nodes play identically for any cut of the
//! periods, through latent and direct paths (the null); a sleeping synth wakes on its sequence's
//! next note; a play chases the bends and controllers set before its start, and leaves out the
//! notes that began before it; at one tick, a note off then a note on of the same key retriggers
//! the note; a note off and a note on of one key at one sample leave the note held, whatever the
//! order of the sequences, so that the stop ends it; overlapping notes of one key in one sequence
//! end where the next starts, so that a stop ends the last; at one sample a node gets note offs
//! first, then its timeline's events before the controller's; events past a node's room are
//! dropped and counted, and a note whose note on was dropped sends no note off; sequences and the
//! tempo map take effect at a commit; a stop ends the notes a node holds even if its sequences
//! changed.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use magnolia_engine::engine::{
    Destination, EngineConfig, GraphError, MidiClip, MidiNote, MidiSequence, NodeId, Source,
    TimedEvent,
};
use magnolia_engine::node::{Event, EventKind, Note};
use magnolia_engine::nodes::{TestLatency, TestSynth};
use magnolia_engine::time::{SamplePosition, TempoMap};

use crate::support::{Counted, FADE, Rig, rate, read};
use crate::timeline_support::{
    Listener, QUARTER, Random, audible_at, bend, changing_tempo, clip, controller, events_heard,
    first_sound, give, note, notes, play_from, sample_of, sequence, steady_tempo,
};

/// A clip of `quarters` quarters from `start`, of random notes on channels 0 and 1, some long enough
/// to cross the clip's end, a chord of 20 notes, which steals voices, and random bends and
/// controllers.
fn random_clip(random: &mut Random, start: i64, quarters: i64) -> MidiClip {
    let mut clip = clip(start, quarters * QUARTER);
    let eighth = QUARTER / 8;
    for step in 0..quarters * 8 {
        let tick = step * eighth + (random.next() % 1_000) as i64;
        clip.notes.push(MidiNote {
            tick,
            length: random.up_to(16) as i64 * eighth / 2,
            channel: (random.next() % 2) as u8,
            key: 36 + (random.next() % 60) as u8,
            velocity: random.up_to(100) as f32 / 100.0,
        });
        if step % 2 == 0 {
            let value = (random.next() % 2_001) as f32 / 1_000.0 - 1.0;
            clip.pitch_bends
                .push(bend(tick + eighth / 3, (step % 4 / 2) as u8, value));
        }
        if step % 3 == 0 {
            let value = (random.next() % 128) as f32 / 127.0;
            clip.controllers.push(controller(tick, 0, 1, value));
        }
    }
    let chord = (quarters / 2) * QUARTER;
    for key in 0..20 {
        clip.notes.push(note(chord, QUARTER, 40 + 2 * key));
    }
    clip
}

/// Two sequences of two overlapping clips each, from fixed seeds.
fn null_sequences() -> [Arc<MidiSequence>; 2] {
    [1_u64, 2].map(|seed| {
        let mut random = Random(0x9E37_79B9_7F4A_7C15 ^ seed);
        sequence(vec![
            random_clip(&mut random, 0, 6),
            random_clip(&mut random, 5 * QUARTER, 5),
        ])
    })
}

/// The samples the null compares, from the sample published for `from`.
const NULL_SAMPLES: usize = 230_000;

/// Plays `sequences` from `from`: the first through a synth, a latency of 300 samples and the
/// playback channel, the second through a synth straight to it. Plays `idle` periods first, then
/// the play's, each period as long as `next_period` says, until the null's samples have played.
/// Returns what played from the first period, and the engine sample published for `from`.
fn render_null(
    sequences: &[Arc<MidiSequence>; 2],
    map: &Arc<TempoMap>,
    from: i64,
    idle: usize,
    next_period: &mut dyn FnMut() -> usize,
) -> (Vec<f32>, usize) {
    let mut rig = Rig::with_config(EngineConfig::new(rate(), 4_096, 0, 1));
    let latent = rig.add(TestSynth::new(rate()));
    let latency = rig.add(TestLatency::new(1, 300));
    let direct = rig.add(TestSynth::new(rate()));
    rig.controller
        .connect(Source::Node(latent, 0), Destination::Node(latency, 0));
    rig.controller
        .connect(Source::Node(latency, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(direct, 0), Destination::Playback(0));
    give(&mut rig, latent, vec![Arc::clone(&sequences[0])]);
    give(&mut rig, direct, vec![Arc::clone(&sequences[1])]);
    rig.controller.set_tempo_map(Arc::clone(map));
    rig.controller.commit().unwrap();
    let mut played = Vec::new();
    for _ in 0..idle {
        played.extend(rig.period(next_period()));
    }
    play_from(&mut rig, from);
    played.extend(rig.period(next_period()));
    let (_, audible) = audible_at(&rig);
    let audible = usize::try_from(audible).unwrap();
    while played.len() < audible + NULL_SAMPLES {
        played.extend(rig.period(next_period()));
    }
    assert_eq!(rig.controller.stats().events_dropped(), 0);
    (played, audible)
}

#[test]
fn sequences_through_latent_and_direct_paths_null_whatever_the_periods() {
    let map = changing_tempo();
    let sequences = null_sequences();
    // Between two ticks, after the start of notes that do not play, before the tempo changes.
    let from = sample_of(&map, 2 * QUARTER) + 1_234;
    assert!(sample_of(&map, 4 * QUARTER) < from + NULL_SAMPLES as i64);

    let (blocks, at_blocks) = render_null(&sequences, &map, from, 1, &mut || 4_096);
    let mut random = Random(0x0123_4567_89AB_CDEF);
    let (cut, at_cut) = render_null(&sequences, &map, from, 3, &mut || random.up_to(5_000));
    assert_eq!(at_blocks, 4_096 + 300);
    assert_ne!(
        at_cut, at_blocks,
        "the two renders start at different engine samples"
    );

    for (played, at) in [(&blocks, at_blocks), (&cut, at_cut)] {
        assert_eq!(
            first_sound(&played[..at]),
            None,
            "silent before the published sample"
        );
    }
    let bits = |played: &[f32], at: usize| -> Vec<u32> {
        played[at..at + NULL_SAMPLES]
            .iter()
            .map(|sample| sample.to_bits())
            .collect()
    };
    let (blocks, cut) = (bits(&blocks, at_blocks), bits(&cut, at_cut));
    let differing = blocks.iter().zip(&cut).position(|(a, b)| a != b);
    assert_eq!(differing, None, "the renders differ from that sample on");
    let sounding = blocks.iter().filter(|&&sample| sample != 0).count();
    assert!(sounding > NULL_SAMPLES / 2, "only {sounding} samples sound");
}

#[test]
fn a_sleeping_synth_wakes_on_its_sequences_next_note() {
    let map = steady_tempo();
    let mut rig = Rig::new(0, 1);
    let (synth, calls) = Counted::new(TestSynth::new(rate()));
    let synth = rig.add(synth);
    rig.controller
        .connect(Source::Node(synth, 0), Destination::Playback(0));
    let second = 4 * QUARTER + QUARTER / 3;
    give(
        &mut rig,
        synth,
        vec![notes(vec![
            note(0, QUARTER / 8, 60),
            note(second, QUARTER / 8, 64),
        ])],
    );
    rig.controller.set_tempo_map(Arc::clone(&map));
    rig.controller.commit().unwrap();
    play_from(&mut rig, 0);
    let mut played = Vec::new();
    let mut calls_after = Vec::new();
    for _ in 0..500 {
        played.extend(rig.period(256));
        calls_after.push(calls.load(Ordering::Relaxed));
    }

    let first_ends = sample_of(&map, QUARTER / 8) as usize + FADE - 1;
    let second = sample_of(&map, second) as usize;
    assert_eq!(first_sound(&played), Some(0));
    assert!(first_sound(&played[first_ends..second]).is_none());
    assert_ne!(played[second], 0.0);
    // Asleep between the notes: not called at all. The period of the second note calls it.
    let asleep = first_ends / 256 + 2;
    let woken = second / 256;
    assert_eq!(calls_after[woken - 1], calls_after[asleep]);
    assert_eq!(calls_after[woken], calls_after[woken - 1] + 1);
}

/// A synth and a listener, both on `sequences`, played from `from` at 120 quarters a minute;
/// returns what played from the published sample, and what the listener heard.
fn play_and_listen(
    sequences: &[Arc<MidiSequence>],
    from: i64,
    periods: usize,
) -> (Vec<f32>, Vec<(i64, Event)>) {
    let mut rig = Rig::new(0, 1);
    let synth = rig.add(TestSynth::new(rate()));
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    for node in [synth, listener] {
        rig.controller
            .connect(Source::Node(node, 0), Destination::Playback(0));
        give(&mut rig, node, sequences.to_vec());
    }
    rig.controller.set_tempo_map(steady_tempo());
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    play_from(&mut rig, from);
    let played = rig.play(&vec![512; periods]);
    let (_, at) = audible_at(&rig);
    let at = usize::try_from(at).unwrap() - 64;
    (played[at..].to_vec(), events_heard(&heard))
}

#[test]
fn a_play_chases_the_bends_and_controllers_set_before_its_start() {
    let map = steady_tempo();
    let from = sample_of(&map, 2 * QUARTER);
    let with = |bends: Vec<_>, key| {
        sequence(vec![MidiClip {
            notes: vec![note(2 * QUARTER, QUARTER, key)],
            pitch_bends: bends,
            controllers: vec![
                controller(QUARTER, 0, 7, 0.25),
                controller(QUARTER + QUARTER / 2, 0, 7, 0.5),
                controller(3 * QUARTER, 0, 7, 0.75),
                controller(QUARTER / 2, 1, 1, 1.0),
            ],
            ..clip(0, 10 * QUARTER)
        }])
    };
    // Up two semitones, set a quarter before the start, or at the start, or never; and the note
    // two semitones up, unbent.
    let (chased, heard) = play_and_listen(&[with(vec![bend(QUARTER, 0, 1.0)], 69)], from, 100);
    let (at_start, _) = play_and_listen(&[with(vec![bend(2 * QUARTER, 0, 1.0)], 69)], from, 100);
    let (unbent, _) = play_and_listen(&[with(Vec::new(), 69)], from, 100);
    let (higher, _) = play_and_listen(&[with(Vec::new(), 71)], from, 100);
    assert_ne!(first_sound(&chased), None);
    assert_eq!(
        chased, at_start,
        "the chased bend applies from the first sample"
    );
    assert_eq!(chased, higher, "two semitones up");
    assert_ne!(chased, unbent);

    // At the start, each lane's last value before it, then the note; later values as they come.
    let kinds_at = |at: i64| -> Vec<EventKind> {
        heard
            .iter()
            .filter(|&&(sample, _)| sample == at)
            .map(|(_, event)| event.kind)
            .collect()
    };
    let started = kinds_at(from);
    assert_eq!(
        started[..3],
        [
            EventKind::PitchBend {
                channel: 0,
                value: 1.0
            },
            EventKind::Controller {
                channel: 0,
                number: 7,
                value: 0.5
            },
            EventKind::Controller {
                channel: 1,
                number: 1,
                value: 1.0
            },
        ]
    );
    assert!(matches!(
        started[3..],
        [EventKind::NoteOn(Note { key: 69, .. })]
    ));
    assert_eq!(
        kinds_at(sample_of(&map, 3 * QUARTER)),
        [
            EventKind::NoteOff(Note {
                id: None,
                channel: 0,
                key: 69,
                velocity: 0.0
            }),
            EventKind::Controller {
                channel: 0,
                number: 7,
                value: 0.75
            }
        ]
    );
    assert_eq!(heard.first().map(|&(sample, _)| sample), Some(from));
}

#[test]
fn a_note_that_began_before_the_start_does_not_play_nor_end_another() {
    let map = steady_tempo();
    let from = sample_of(&map, 2 * QUARTER);
    // A note held across the start, and the same key struck after it, before the first one ends:
    // from two sequences, as one sequence ends a note where its key starts again.
    let sequences = [
        notes(vec![note(QUARTER, 2 * QUARTER, 60)]),
        notes(vec![note(2 * QUARTER + QUARTER / 2, QUARTER, 60)]),
    ];
    let (played, heard) = play_and_listen(&sequences, from, 200);
    let keys: Vec<(i64, bool)> = heard
        .iter()
        .map(|&(sample, event)| (sample, matches!(event.kind, EventKind::NoteOn(_))))
        .collect();
    let second_on = sample_of(&map, 2 * QUARTER + QUARTER / 2);
    let second_off = sample_of(&map, 3 * QUARTER + QUARTER / 2);
    assert_eq!(keys, [(second_on, true), (second_off, false)]);
    // Silent until the second note, which sounds through the first one's end.
    let relative = |sample: i64| (sample - from) as usize;
    assert_eq!(first_sound(&played), Some(relative(second_on)));
    let first_end = relative(sample_of(&map, 3 * QUARTER));
    assert!(
        played[first_end..first_end + 2 * FADE]
            .iter()
            .all(|&sample| sample != 0.0)
    );
}

#[test]
fn at_one_tick_a_note_off_then_a_note_on_of_the_same_key_retriggers_the_note() {
    let map = steady_tempo();
    let both = notes(vec![note(0, QUARTER, 60), note(QUARTER, QUARTER, 60)]);
    let second_alone = notes(vec![note(QUARTER, QUARTER, 60)]);
    let (retriggered, heard) = play_and_listen(&[both], 0, 120);
    let (alone, _) = play_and_listen(&[second_alone], 0, 120);
    let at = sample_of(&map, QUARTER);
    let at_one_tick: Vec<EventKind> = heard
        .iter()
        .filter(|&&(sample, _)| sample == at)
        .map(|(_, event)| event.kind)
        .collect();
    assert!(
        matches!(
            at_one_tick[..],
            [
                EventKind::NoteOff(Note { key: 60, .. }),
                EventKind::NoteOn(Note { key: 60, .. })
            ]
        ),
        "{at_one_tick:?}"
    );
    // Once the first note's release is over, the second plays alone, as if struck alone.
    let released = at as usize + FADE - 1;
    let end = sample_of(&map, 2 * QUARTER) as usize;
    assert_eq!(retriggered[released..end], alone[released..end]);
    assert!(
        retriggered[released..end]
            .iter()
            .all(|&sample| sample != 0.0)
    );
}

/// A synth and a listener, both on `sequences`, played from 0 in periods of 64 and stopped after
/// `stop_after` samples; returns what played from 40 periods after the stop, well past its
/// release, and the timeline samples of the note offs the listener heard.
fn play_then_stop(sequences: &[Arc<MidiSequence>], stop_after: usize) -> (Vec<f32>, Vec<i64>) {
    let mut rig = Rig::new(0, 1);
    let synth = rig.add(TestSynth::new(rate()));
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    for node in [synth, listener] {
        rig.controller
            .connect(Source::Node(node, 0), Destination::Playback(0));
        give(&mut rig, node, sequences.to_vec());
    }
    rig.controller.set_tempo_map(steady_tempo());
    rig.controller.commit().unwrap();
    play_from(&mut rig, 0);
    rig.play(&vec![64; stop_after / 64]);
    rig.controller.stop().unwrap();
    let after = rig.play(&[64; 100]);
    let note_offs = events_heard(&heard)
        .into_iter()
        .filter(|(_, event)| matches!(event.kind, EventKind::NoteOff(_)))
        .map(|(at, _)| at)
        .collect();
    (after[40 * 64..].to_vec(), note_offs)
}

#[test]
fn a_note_off_and_a_note_on_of_one_key_at_one_sample_leave_the_note_for_the_stop_to_end() {
    let map = steady_tempo();
    let (retriggered, stop_after) = (sample_of(&map, QUARTER), 36_032);
    let stop = stop_after as i64 + FADE as i64;
    // One key ended by one sequence and struck by another at one tick, in either order.
    let ends = notes(vec![note(0, QUARTER, 60)]);
    let strikes = notes(vec![note(QUARTER, 4 * QUARTER, 60)]);
    for order in [[&ends, &strikes], [&strikes, &ends]] {
        let sequences = order.map(Arc::clone);
        let (after, note_offs) = play_then_stop(&sequences, stop_after);
        assert_eq!(note_offs, [retriggered, stop]);
        assert_eq!(first_sound(&after), None);
    }
    // Two notes of one key from two sequences, the second struck less than a sample before the
    // first ends: both land on one sample, and both sounded, so each ends.
    let overlapping = [
        notes(vec![note(0, QUARTER, 60)]),
        notes(vec![note(QUARTER - 100, 4 * QUARTER, 60)]),
    ];
    assert_eq!(sample_of(&map, QUARTER - 100), retriggered);
    let (after, note_offs) = play_then_stop(&overlapping, stop_after);
    assert_eq!(note_offs, [retriggered, stop]);
    assert_eq!(first_sound(&after), None);
}

/// The note ons and note offs among `heard`, at their samples.
fn notes_heard(heard: &[(i64, Event)]) -> Vec<(i64, &'static str)> {
    heard
        .iter()
        .filter_map(|&(at, event)| match event.kind {
            EventKind::NoteOn(_) => Some((at, "on")),
            EventKind::NoteOff(_) => Some((at, "off")),
            _ => None,
        })
        .collect()
}

#[test]
fn overlapping_notes_of_one_key_in_one_sequence_end_where_the_next_starts() {
    let map = steady_tempo();
    // A from quarter 0 to 2, and B of the same key from quarter 1 to 3: A ends where B starts.
    let overlapping = notes(vec![
        note(0, 2 * QUARTER, 60),
        note(QUARTER, 2 * QUARTER, 60),
    ]);
    let at = |quarter: i64| sample_of(&map, quarter * QUARTER);
    let (_, heard) = play_and_listen(&[Arc::clone(&overlapping)], 0, 200);
    assert_eq!(
        notes_heard(&heard),
        [(0, "on"), (at(1), "off"), (at(1), "on"), (at(3), "off")]
    );
    // Stopped while B is held, after A's own end: the stop ends B, and the synth falls silent.
    let stop_after = 59_392;
    assert!((at(2)..at(3)).contains(&(stop_after as i64)));
    let (after, note_offs) = play_then_stop(&[overlapping], stop_after);
    assert_eq!(note_offs, [at(1), stop_after as i64 + FADE as i64]);
    assert_eq!(first_sound(&after), None);
}

#[test]
fn at_one_sample_note_offs_come_first_then_the_timelines_events_before_the_controllers() {
    let map = steady_tempo();
    let mut rig = Rig::new(0, 1);
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Playback(0));
    let half = QUARTER / 2;
    give(
        &mut rig,
        listener,
        vec![notes(vec![note(0, half, 60), note(half, QUARTER, 62)])],
    );
    rig.controller.set_tempo_map(Arc::clone(&map));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    play_from(&mut rig, 0);
    rig.play(&[64]);
    // The play started at engine sample 64, without compensation.
    let at = sample_of(&map, half);
    let controller_note = |key| Note {
        id: Some(7),
        channel: 0,
        key,
        velocity: 1.0,
    };
    let kinds = [
        EventKind::NoteOn(controller_note(70)),
        EventKind::NoteOff(controller_note(71)),
        EventKind::Controller {
            channel: 0,
            number: 1,
            value: 0.5,
        },
    ];
    for kind in kinds {
        let event = TimedEvent {
            node: listener,
            at: SamplePosition(64 + at),
            kind,
        };
        rig.controller.send_event(event).unwrap();
    }
    rig.play(&[512; 30]);
    let heard_at: Vec<EventKind> = events_heard(&heard)
        .into_iter()
        .filter(|&(sample, _)| sample == at)
        .map(|(_, event)| event.kind)
        .collect();
    let timeline_note = |key, velocity| Note {
        id: None,
        channel: 0,
        key,
        velocity,
    };
    assert_eq!(
        heard_at,
        [
            EventKind::NoteOff(timeline_note(60, 0.0)),
            kinds[1],
            EventKind::NoteOn(timeline_note(62, 0.8)),
            kinds[0],
            kinds[2],
        ]
    );
}

#[test]
fn timeline_events_past_a_nodes_room_are_dropped_and_counted() {
    let map = steady_tempo();
    let mut config = EngineConfig::new(rate(), 1_024, 0, 1);
    config.event_capacity = 8;
    let mut rig = Rig::with_config(config);
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Playback(0));
    let chord: Vec<MidiNote> = (0..20)
        .map(|key| note(QUARTER, QUARTER / 2, 40 + key))
        .collect();
    give(&mut rig, listener, vec![notes(chord)]);
    rig.controller.set_tempo_map(Arc::clone(&map));
    rig.controller.commit().unwrap();
    play_from(&mut rig, 0);
    let stats = Arc::clone(rig.controller.stats());
    rig.play(&[1_024; 30]);
    // 20 note ons at one sample: 8 have room.
    assert_eq!(events_heard(&heard).len(), 8);
    assert_eq!(stats.events_dropped(), 12);
    rig.play(&[1_024; 20]);
    // The 8 notes that sounded end; the 12 never held send no note off, and take no room.
    assert_eq!(stats.events_dropped(), 12);
    let offs: Vec<u8> = events_heard(&heard)
        .into_iter()
        .filter_map(|(_, event)| match event.kind {
            EventKind::NoteOff(note) => Some(note.key),
            _ => None,
        })
        .collect();
    let ons: Vec<u8> = events_heard(&heard)
        .into_iter()
        .filter_map(|(_, event)| match event.kind {
            EventKind::NoteOn(note) => Some(note.key),
            _ => None,
        })
        .collect();
    assert_eq!(offs, ons);
    // Nothing is held, so the stop only releases the pedal and resets the bend.
    rig.controller.stop().unwrap();
    rig.play(&[1_024; 4]);
    let last: Vec<EventKind> = events_heard(&heard)
        .into_iter()
        .skip(16)
        .map(|(_, event)| event.kind)
        .collect();
    assert_eq!(
        last,
        [
            EventKind::Controller {
                channel: 0,
                number: 64,
                value: 0.0
            },
            EventKind::PitchBend {
                channel: 0,
                value: 0.0
            }
        ]
    );
}

#[test]
fn sequences_and_the_tempo_map_take_effect_at_a_commit_and_need_a_node() {
    let mut rig = Rig::new(0, 1);
    let ghost = NodeId {
        slot: 99,
        generation: 0,
    };
    assert_eq!(
        rig.controller.set_sequences(ghost, Vec::new()),
        Err(GraphError::UnknownNode(ghost))
    );
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    // Neither sequences nor a tempo map yet: nothing plays.
    play_from(&mut rig, 0);
    let one_note = notes(vec![note(QUARTER / 4, QUARTER / 4, 60)]);
    give(&mut rig, listener, vec![Arc::clone(&one_note)]);
    rig.play(&[1_024; 10]);
    assert!(events_heard(&heard).is_empty());
    // Committed without a tempo map: still nothing, not even at a stop.
    rig.controller.commit().unwrap();
    rig.play(&[1_024; 10]);
    rig.controller.stop().unwrap();
    rig.play(&[1_024; 4]);
    assert!(events_heard(&heard).is_empty());
    // With one: the note plays from the next play.
    rig.controller.set_tempo_map(steady_tempo());
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    play_from(&mut rig, 0);
    rig.play(&[1_024; 20]);
    assert_eq!(events_heard(&heard).len(), 2);
    // A node removed takes its sequences with it.
    rig.controller.remove_node(listener).unwrap();
    assert_eq!(
        rig.controller.set_sequences(listener, vec![one_note]),
        Err(GraphError::UnknownNode(listener))
    );
}

#[test]
fn a_stop_ends_the_notes_a_node_holds_even_after_its_sequences_changed() {
    let map = steady_tempo();
    let mut rig = Rig::new(0, 1);
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Playback(0));
    give(
        &mut rig,
        listener,
        vec![notes(vec![note(0, 100 * QUARTER, 60)])],
    );
    rig.controller.set_tempo_map(map);
    rig.controller.commit().unwrap();
    play_from(&mut rig, 0);
    rig.play(&[1_024; 10]);
    // The sequence goes while its note is held; the stop still ends the note.
    rig.controller.set_sequences(listener, Vec::new()).unwrap();
    rig.controller.commit().unwrap();
    rig.play_until_steady(1_024);
    rig.controller.stop().unwrap();
    rig.play(&[1_024; 4]);
    let kinds: Vec<EventKind> = events_heard(&heard)
        .into_iter()
        .map(|(_, event)| event.kind)
        .collect();
    assert!(
        matches!(
            kinds[..],
            [
                EventKind::NoteOn(Note { key: 60, .. }),
                EventKind::NoteOff(Note { key: 60, .. })
            ]
        ),
        "{kinds:?}"
    );
    let calls = read(&heard);
    assert!(!calls.last().unwrap().transport.rolling);
}
