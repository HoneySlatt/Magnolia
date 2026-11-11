//! The transport (ADR-0025): a play pre-rolls by the graph's largest compensation, so that every
//! path sounds the play's first sample together, at the command's period start plus that
//! compensation; a stop asked before anything audible was read stops at once, silent; a stop while
//! rolling ends every path on the same timeline sample, with its notes ended, the sustain pedal
//! released and the pitch bend reset, also when the stop ends on a period's boundary, or comes in a
//! later period of the pre-roll; a node whose compensation a swap moves past a pending stop still
//! gets the stop's events, once; a play while rolling stops first, then plays; a full queue of
//! commands is refused; a swap keeps the running clock.

use std::sync::Arc;

use magnolia_engine::engine::{Destination, MidiSequence, Source, TransportError};
use magnolia_engine::node::{AudibleRegion, Event, EventKind, Note};
use magnolia_engine::nodes::{TestLatency, TestSynth};
use magnolia_engine::time::{SamplePosition, TempoMap};

use crate::support::{FADE, Rig, Shared, rate, read};
use crate::timeline_support::{
    Heard, Listener, QUARTER, audible_at, bend, clip, controller, events_heard, first_sound, give,
    note, notes, play_channels, play_from, sample_of, sequence, steady_tempo,
};

/// The latency on the latent path: the graph's largest compensation.
const LATENCY: u32 = 300;
const COMPENSATION: i64 = LATENCY as i64;

/// Two paths, each with its own synth playing one sequence: through a latency to playback channel
/// 0, and straight to playback channel 1. Beside each synth, a listener hears the same sequence at
/// the same distance ahead of the output.
struct TwoPaths {
    rig: Rig,
    /// Hears the timeline `LATENCY` samples ahead of the output.
    latent: Shared<Vec<Heard>>,
    /// Hears the timeline as the output plays it.
    direct: Shared<Vec<Heard>>,
}

impl TwoPaths {
    fn new(sequence: Arc<MidiSequence>, map: Arc<TempoMap>) -> Self {
        let mut rig = Rig::new(0, 2);
        let latent_synth = rig.add(TestSynth::new(rate()));
        let direct_synth = rig.add(TestSynth::new(rate()));
        let (latent_listener, latent) = Listener::new();
        let latent_listener = rig.add(latent_listener);
        let (direct_listener, direct) = Listener::new();
        let direct_listener = rig.add(direct_listener);
        for (source, playback) in [(latent_synth, 0), (latent_listener, 0)] {
            let latency = rig.add(TestLatency::new(1, LATENCY));
            rig.controller
                .connect(Source::Node(source, 0), Destination::Node(latency, 0));
            rig.controller
                .connect(Source::Node(latency, 0), Destination::Playback(playback));
        }
        for source in [direct_synth, direct_listener] {
            rig.controller
                .connect(Source::Node(source, 0), Destination::Playback(1));
        }
        for source in [latent_synth, direct_synth, latent_listener, direct_listener] {
            give(&mut rig, source, vec![Arc::clone(&sequence)]);
        }
        rig.controller.set_tempo_map(map);
        rig.controller.commit().unwrap();
        Self {
            rig,
            latent,
            direct,
        }
    }

    /// Plays `count` periods of `frames` on both channels.
    fn play(&mut self, count: usize, frames: usize) -> Vec<Vec<f32>> {
        play_channels(&mut self.rig, &vec![frames; count])
    }
}

fn silent(samples: &[f32]) -> bool {
    first_sound(samples).is_none()
}

#[test]
fn a_play_pre_rolls_so_that_every_path_sounds_its_first_note_together() {
    let map = steady_tempo();
    let from = sample_of(&map, QUARTER);
    for period in [64, 1_000] {
        let mut paths = TwoPaths::new(notes(vec![note(QUARTER, QUARTER, 60)]), map.clone());
        let idle = paths.play(3, period);
        assert!(idle.iter().all(|channel| silent(channel)));
        let sequence = play_from(&mut paths.rig, from);
        let command = 3 * period as i64;
        let played = paths.play(20, period);

        // The output reaches `from` at the command's period start plus the compensation.
        assert_eq!(audible_at(&paths.rig), (sequence, command + COMPENSATION));
        assert_eq!(
            first_sound(&played[0]),
            Some(LATENCY as usize),
            "period {period}"
        );
        assert_eq!(
            first_sound(&played[1]),
            Some(LATENCY as usize),
            "period {period}"
        );
        assert_eq!(
            played[0], played[1],
            "period {period}: the two paths are in line"
        );

        // The latent source read `from` at once; the direct one, a compensation later.
        let heard_from = |heard: &Shared<Vec<Heard>>| {
            let calls = read(heard);
            let first = calls.iter().find(|call| call.transport.rolling).unwrap();
            (first.transport.position.0, first.transport.audible)
        };
        let audible = AudibleRegion {
            start: SamplePosition(from),
            stop: None,
        };
        assert_eq!(heard_from(&paths.latent), (from, audible));
        assert_eq!(heard_from(&paths.direct), (from - COMPENSATION, audible));
        for heard in [&paths.latent, &paths.direct] {
            let events = events_heard(heard);
            assert!(
                matches!(events[..], [(at, Event { kind: EventKind::NoteOn(_), .. })] if at == from),
                "{events:?}"
            );
        }
    }
}

#[test]
fn a_stop_asked_before_anything_audible_was_read_stops_at_once_silent() {
    let map = steady_tempo();
    let from = sample_of(&map, QUARTER);
    let mut paths = TwoPaths::new(notes(vec![note(QUARTER, QUARTER, 60)]), map);
    paths.play(2, 64);
    // Both apply at the next period's start: the play pre-rolls, and the stop finds that no node
    // has read `from` yet.
    play_from(&mut paths.rig, from);
    paths.rig.controller.stop().unwrap();
    let played = paths.play(40, 64);
    assert!(played.iter().all(|channel| silent(channel)));
    for heard in [&paths.latent, &paths.direct] {
        assert!(events_heard(heard).is_empty());
        let calls = read(heard);
        let last = calls.last().unwrap();
        assert!(!last.transport.rolling);
        assert_eq!(
            last.transport.audible,
            AudibleRegion {
                start: SamplePosition(from),
                stop: Some(SamplePosition(from)),
            }
        );
        assert!(calls.iter().all(|call| !call.transport.rolling));
    }

    // The next play plays.
    let sequence = play_from(&mut paths.rig, from);
    assert_eq!(sequence, 2);
    let played = paths.play(20, 64);
    assert_eq!(first_sound(&played[0]), Some(LATENCY as usize));
    assert_eq!(first_sound(&played[1]), Some(LATENCY as usize));
}

/// Two held notes on two channels, the second with its sustain pedal down, the first bent.
fn held_notes() -> Arc<MidiSequence> {
    let held = |channel, key| magnolia_engine::engine::MidiNote {
        channel,
        ..note(QUARTER, 8 * QUARTER, key)
    };
    sequence(vec![magnolia_engine::engine::MidiClip {
        notes: vec![held(0, 60), held(1, 64)],
        pitch_bends: vec![bend(QUARTER + QUARTER / 2, 0, 0.5)],
        controllers: vec![controller(QUARTER + QUARTER / 4, 1, 64, 1.0)],
        ..clip(0, 100 * QUARTER)
    }])
}

/// What a stop sends at its sample to a node playing `held_notes`: the notes ended, then on each
/// channel the sustain pedal released and the bend reset.
fn stop_events() -> Vec<EventKind> {
    let off = |channel, key| {
        EventKind::NoteOff(Note {
            id: None,
            channel,
            key,
            velocity: 0.0,
        })
    };
    let pedal_up = |channel| EventKind::Controller {
        channel,
        number: 64,
        value: 0.0,
    };
    let bend_reset = |channel| EventKind::PitchBend {
        channel,
        value: 0.0,
    };
    vec![
        off(0, 60),
        off(1, 64),
        pedal_up(0),
        bend_reset(0),
        pedal_up(1),
        bend_reset(1),
    ]
}

/// The events a listener heard at timeline sample `at`.
fn heard_at(heard: &Shared<Vec<Heard>>, at: i64) -> Vec<EventKind> {
    events_heard(heard)
        .into_iter()
        .filter(|&(sample, _)| sample == at)
        .map(|(_, event)| event.kind)
        .collect()
}

#[test]
fn a_stop_while_rolling_ends_every_path_on_the_same_timeline_sample() {
    let map = steady_tempo();
    // The notes start 100 samples after the play's start.
    let from = sample_of(&map, QUARTER) - 100;
    let mut paths = TwoPaths::new(held_notes(), map.clone());
    paths.play(2, 64);
    play_from(&mut paths.rig, from);
    let start = 2 * 64;
    let mut played = paths.play(1_000, 64);
    paths.rig.controller.stop().unwrap();
    let command = start + 1_000 * 64;
    played
        .iter_mut()
        .zip(paths.play(100, 64))
        .for_each(|(all, more)| all.extend(more));

    // Every node reads up to what the most latent one read, plus a fade.
    let read_up_to = from - COMPENSATION + (command - start) + COMPENSATION;
    let stop = read_up_to + FADE as i64;
    for heard in [&paths.latent, &paths.direct] {
        assert_eq!(heard_at(heard, stop), stop_events());
        let after: Vec<_> = events_heard(heard)
            .into_iter()
            .filter(|&(sample, _)| sample > stop)
            .collect();
        assert!(after.is_empty(), "{after:?}");
        let calls = read(heard);
        let last = calls.last().unwrap();
        assert!(!last.transport.rolling);
        assert_eq!(last.transport.audible.stop, Some(SamplePosition(stop)));
    }
    // The transport stops once its clock, the output's sample, has played the stop: the direct
    // listener's last rolling call ends on it, and its next starts after it, stopped.
    let direct = read(&paths.direct);
    let last_rolling = direct
        .iter()
        .rposition(|call| call.transport.rolling)
        .unwrap();
    let ended = &direct[last_rolling];
    assert_eq!(ended.transport.position.0 + ended.frames as i64, stop + 1);
    assert_eq!(direct[last_rolling + 1].transport.position.0, stop + 1);

    // Both paths sound the same, and fall silent together, a release after the stop reaches the
    // output.
    assert_eq!(played[0], played[1]);
    let stop_heard = (stop - from + COMPENSATION) as usize;
    for channel in &played {
        assert_ne!(channel[stop_heard - 1], 0.0);
        assert_ne!(channel[stop_heard + FADE - 2], 0.0);
        assert!(silent(&channel[stop_heard + FADE - 1..]));
    }
}

#[test]
fn a_stop_in_a_later_period_of_the_pre_roll_plays_a_period_and_a_fade() {
    let map = steady_tempo();
    let from = sample_of(&map, QUARTER);
    let mut paths = TwoPaths::new(notes(vec![note(QUARTER, QUARTER, 60)]), map);
    paths.play(2, 64);
    play_from(&mut paths.rig, from);
    let mut played = paths.play(1, 64);
    // The second period of the pre-roll: the most latent node has read a period past `from`.
    paths.rig.controller.stop().unwrap();
    played
        .iter_mut()
        .zip(paths.play(40, 64))
        .for_each(|(all, more)| all.extend(more));
    let stop = from + 64 + FADE as i64;
    for heard in [&paths.latent, &paths.direct] {
        assert_eq!(
            heard_at(heard, stop),
            [
                EventKind::NoteOff(Note {
                    id: None,
                    channel: 0,
                    key: 60,
                    velocity: 0.0
                }),
                EventKind::Controller {
                    channel: 0,
                    number: 64,
                    value: 0.0
                },
                EventKind::PitchBend {
                    channel: 0,
                    value: 0.0
                },
            ]
        );
    }
    // The note sounds from the compensation on, for a period and a fade, then releases.
    assert_eq!(played[0], played[1]);
    let heard_stop = LATENCY as usize + 64 + FADE;
    for channel in &played {
        assert_eq!(first_sound(channel), Some(LATENCY as usize));
        assert_ne!(channel[heard_stop + FADE - 2], 0.0);
        assert!(silent(&channel[heard_stop + FADE - 1..]));
    }
}

#[test]
fn a_stop_that_ends_on_a_period_boundary_lets_a_waiting_play_start_with_the_next_period() {
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
    rig.controller.set_tempo_map(steady_tempo());
    rig.controller.commit().unwrap();
    play_from(&mut rig, 0);
    // Without compensation and in periods of a fade plus one sample, the stop asked at the start
    // of the 11th period is its last sample: the transport stops on the boundary.
    let period = FADE + 1;
    rig.play(&[period; 10]);
    let second = play_from(&mut rig, 0);
    rig.play(&[period; 3]);
    let stop = 10 * period as i64 + FADE as i64;
    assert_eq!(audible_at(&rig), (second, 11 * period as i64));
    let calls = read(&heard);
    let ended = &calls[10];
    assert!(ended.transport.rolling);
    assert_eq!(ended.transport.audible.stop, Some(SamplePosition(stop)));
    assert_eq!(ended.transport.position.0 + ended.frames as i64, stop + 1);
    assert!(
        matches!(ended.events[..], [Event { offset, kind: EventKind::NoteOff(_) }, ..] if offset == FADE as u32)
    );
    // The next period plays from the start again, the note struck anew.
    let restarted = &calls[11];
    assert!(restarted.transport.rolling);
    assert_eq!(restarted.transport.position.0, 0);
    assert!(matches!(
        restarted.events[..],
        [Event {
            offset: 0,
            kind: EventKind::NoteOn(_)
        }]
    ));
}

#[test]
fn a_swap_that_moves_a_nodes_position_past_a_pending_stop_still_ends_its_notes() {
    let mut rig = Rig::new(0, 1);
    let synth = rig.add(TestSynth::new(rate()));
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    for node in [synth, listener] {
        rig.controller
            .connect(Source::Node(node, 0), Destination::Playback(0));
        give(
            &mut rig,
            node,
            vec![notes(vec![note(0, 100 * QUARTER, 60)])],
        );
    }
    rig.controller.set_tempo_map(steady_tempo());
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    play_from(&mut rig, 0);
    rig.play(&[64; 20]);
    rig.controller.stop().unwrap();
    // The stop applies: without compensation, it lies a fade ahead.
    rig.play(&[64]);
    let stop = 20 * 64 + FADE as i64;
    // Both nodes now feed a latency of 1,000: their positions jump past the stop.
    let latency = rig.add(TestLatency::new(1, 1_000));
    for node in [synth, listener] {
        rig.controller
            .disconnect(Source::Node(node, 0), Destination::Playback(0))
            .unwrap();
        rig.controller
            .connect(Source::Node(node, 0), Destination::Node(latency, 0));
    }
    rig.controller
        .connect(Source::Node(latency, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    let played = rig.play(&[64; 200]);
    // The listener got the stop's events once, at the first sub-block past the stop, at offset 0.
    let calls = read(&heard);
    let with_stop: Vec<&Heard> = calls
        .iter()
        .filter(|call| {
            call.events
                .iter()
                .any(|event| matches!(event.kind, EventKind::NoteOff(_)))
        })
        .collect();
    assert_eq!(with_stop.len(), 1);
    assert!(with_stop[0].transport.position.0 > stop);
    assert_eq!(with_stop[0].events[0].offset, 0);
    assert!(!calls.last().unwrap().transport.rolling);
    // The synth's note ends: silence once the release has come through the latency.
    assert!(silent(&played[100 * 64..]));
}

#[test]
fn an_old_graph_node_already_past_a_stop_gets_its_stop_events_during_the_swap() {
    let mut rig = Rig::new(0, 1);
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    let latency = rig.add(TestLatency::new(1, 1_000));
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Node(latency, 0));
    rig.controller
        .connect(Source::Node(latency, 0), Destination::Playback(0));
    give(
        &mut rig,
        listener,
        vec![notes(vec![note(0, 100 * QUARTER, 60)])],
    );
    rig.controller.set_tempo_map(steady_tempo());
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    play_from(&mut rig, 0);
    rig.play(&[64; 40]);
    assert_eq!(rig.backend.processor().largest_ahead_in(), 1_000);
    // The latent path goes, and the stop applies in the same period: the compensation is then 0,
    // so the stop lies a fade ahead of the output, while the listener, which the crossfade still
    // runs a thousand samples ahead, has passed it.
    rig.controller.remove_node(listener).unwrap();
    rig.controller.remove_node(latency).unwrap();
    rig.controller.commit().unwrap();
    rig.controller.stop().unwrap();
    rig.play(&[64; 10]);
    let calls = read(&heard);
    let stop = calls.last().unwrap().transport.audible.stop.unwrap().0;
    let with_stop: Vec<&Heard> = calls
        .iter()
        .filter(|call| !call.events.is_empty() && call.transport.audible.stop.is_some())
        .collect();
    assert_eq!(with_stop.len(), 1);
    assert!(with_stop[0].transport.position.0 > stop);
    assert_eq!(
        with_stop[0].events,
        [Event {
            offset: 0,
            kind: EventKind::NoteOff(Note {
                id: None,
                channel: 0,
                key: 60,
                velocity: 0.0
            })
        }]
    );
}

#[test]
fn a_play_while_rolling_stops_first_then_plays_from_its_own_start() {
    let map = steady_tempo();
    let first_from = sample_of(&map, QUARTER) - 100;
    let second_from = sample_of(&map, 5 * QUARTER);
    let sequence = sequence(vec![magnolia_engine::engine::MidiClip {
        notes: vec![
            note(QUARTER, 8 * QUARTER, 60),
            note(5 * QUARTER, QUARTER, 67),
        ],
        ..clip(0, 100 * QUARTER)
    }]);
    let mut paths = TwoPaths::new(sequence, map);
    paths.play(2, 64);
    play_from(&mut paths.rig, first_from);
    let mut played = paths.play(500, 64);
    let command = (2 + 500) * 64;
    let second = play_from(&mut paths.rig, second_from);
    played
        .iter_mut()
        .zip(paths.play(200, 64))
        .for_each(|(all, more)| all.extend(more));

    // The stop ends where a stop would, when the output reaches it; the play waits for the first
    // period after.
    let stop = first_from + (command - 2 * 64) + FADE as i64;
    let stop_ends = command + COMPENSATION + FADE as i64 + 1;
    let next_period = (stop_ends as u64).next_multiple_of(64) as i64;
    assert_eq!(audible_at(&paths.rig), (second, next_period + COMPENSATION));
    for heard in [&paths.latent, &paths.direct] {
        let at_stop = heard_at(heard, stop);
        assert!(
            matches!(at_stop[..], [EventKind::NoteOff(Note { key: 60, .. }), ..]),
            "{at_stop:?}"
        );
        let calls = read(heard);
        let restarted = calls
            .iter()
            .find(|call| call.transport.audible.start.0 == second_from)
            .unwrap();
        assert!(restarted.transport.rolling);
    }
    // The first note ends, then silence, then the second play's note on both paths at once.
    assert_eq!(played[0], played[1]);
    let heard_from = (next_period + COMPENSATION - 2 * 64) as usize;
    // The output reaches the stop a sample before the transport stops; the release lasts a fade.
    let first_released = (stop_ends - 1 - 2 * 64) as usize + FADE - 1;
    for channel in &played {
        assert_ne!(channel[first_released - 1], 0.0);
        assert!(silent(&channel[first_released..heard_from]));
        assert_ne!(channel[heard_from], 0.0);
    }
}

#[test]
fn a_full_queue_of_transport_commands_is_refused_until_the_next_period() {
    let mut rig = Rig::new(0, 1);
    rig.controller.commit().unwrap();
    for expected in [1, 2] {
        assert_eq!(rig.controller.play(SamplePosition(0)), Ok(expected));
        rig.controller.stop().unwrap();
    }
    assert_eq!(rig.controller.stop(), Err(TransportError::QueueFull));
    assert_eq!(
        rig.controller.play(SamplePosition(0)),
        Err(TransportError::QueueFull)
    );
    rig.play(&[64]);
    assert_eq!(rig.controller.play(SamplePosition(0)), Ok(3));
    // Applied at the next period's start, without compensation.
    rig.play(&[64]);
    assert_eq!(audible_at(&rig), (3, 64));
}

#[test]
fn a_swap_while_rolling_keeps_the_running_clock() {
    let mut rig = Rig::new(0, 1);
    let (listener, heard) = Listener::new();
    let listener = rig.add(listener);
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    play_from(&mut rig, 10_000);
    rig.play(&[64; 10]);
    // A latent path appears: the graph's compensation grows from 0 to 300, the listener's with it.
    let latency = rig.add(TestLatency::new(1, LATENCY));
    rig.controller
        .connect(Source::Node(listener, 0), Destination::Node(latency, 0));
    rig.controller
        .connect(Source::Node(latency, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    rig.play(&[64; 4]);
    assert_eq!(rig.backend.processor().largest_ahead_in(), 300);
    let calls = read(&heard);
    let rolling: Vec<&Heard> = calls.iter().filter(|call| call.transport.rolling).collect();
    assert_eq!(rolling[0].transport.position.0, 10_000);
    // The output's clock ran on by each sub-block; the listener's own position jumped once, by
    // the compensation it gained.
    let jumps: Vec<i64> = rolling
        .windows(2)
        .map(|pair| {
            let expected = pair[0].transport.position.0 + pair[0].frames as i64;
            pair[1].transport.position.0 - expected
        })
        .filter(|&jump| jump != 0)
        .collect();
    assert_eq!(jumps, [COMPENSATION]);
}
