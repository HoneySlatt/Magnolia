//! Events are sample-accurate: a parameter changes the output exactly at its sample, in any
//! sub-block; events at one sample keep their order, note offs first; an event follows the swap
//! that brings its node; an event for a node gone is dropped and counted; a late event plays at
//! once; the processor never holds more events than its pending buffer, so one due now never waits
//! behind events far ahead.
//!
//! The controller sends an event for a node only once the processor holds that node, so each test
//! plays a period after its first commit before it sends events on time.

use magnolia_engine::engine::{
    Commit, Destination, EngineConfig, SendEventError, Source, TimedEvent,
};
use magnolia_engine::node::{Event, EventKind, Note};
use magnolia_engine::nodes::{Gain, Sine};
use magnolia_engine::time::SamplePosition;

use crate::support::{
    Constant, FADE, LARGEST_BLOCK, Recorder, Rig, assert_matches, fade_in, rate, read, sine,
};

#[test]
fn a_gain_event_changes_the_output_exactly_at_its_sample_in_any_sub_block() {
    // At 2,048 frames each period is cut into two sub-blocks of 1,024.
    for period in [100, 2_048] {
        for at in (0..2 * period).step_by(if period == 100 { 1 } else { 5 }) {
            let mut rig = Rig::new(0, 1);
            let one = rig.add(Constant::new(1.0));
            let gain = rig.add(Gain::new(1, 0.25));
            rig.controller
                .connect(Source::Node(one, 0), Destination::Node(gain, 0));
            rig.controller
                .connect(Source::Node(gain, 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            rig.play(&[period]);
            rig.send(gain, (period + at) as i64, Gain::GAIN, 0.75);
            let played = rig.play(&[period, period, period]);
            let expected: Vec<f64> = (0..played.len())
                .map(|n| if n < at { 0.25 } else { 0.75 })
                .collect();
            assert_matches(
                &played,
                &expected,
                0.0,
                &format!("period {period}, at {at}"),
            );
        }
    }
}

#[test]
fn a_sines_gain_event_takes_effect_at_its_sample() {
    let mut rig = Rig::new(0, 1);
    let tone = rig.add(Sine::new(rate(), 440.0, 0.5));
    rig.controller
        .connect(Source::Node(tone, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    rig.send(tone, 64 + 1_500, Sine::GAIN, 0.25);
    let played = rig.play(&[2_048]);
    let expected: Vec<f64> = (0..played.len())
        .map(|n| sine(64 + n, 440.0, if n < 1_500 { 0.5 } else { 0.25 }))
        .collect();
    assert_matches(&played, &expected, 1e-6, "sine's gain");
}

#[test]
fn two_events_at_one_sample_apply_in_the_order_they_were_sent() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.5));
    let (recorder, calls) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    // Sent out of order in time, and three at sample 70: the last sent at 70 holds.
    for (at, value) in [(70, 0.1), (30, 0.2), (70, 0.9), (70, 0.4)] {
        rig.send(source, 64 + at, 0, value);
        rig.send(recorder, 64 + at, 0, value);
    }
    let played = rig.play(&[64, 64]);
    let expected: Vec<f64> = (0..128)
        .map(|n| match n {
            0..30 => 0.5,
            30..70 => 0.2,
            _ => 0.4,
        })
        .collect();
    assert_matches(&played, &expected, 1e-7, "events at one sample");
    let calls = read(&calls);
    assert_eq!(calls[1].events, [(30, 0.2)]);
    assert_eq!(calls[2].events, [(6, 0.1), (6, 0.9), (6, 0.4)]);
}

#[test]
fn an_event_sent_right_after_its_nodes_commit_is_not_dropped() {
    let mut rig = Rig::new(0, 1);
    rig.controller.commit().unwrap();
    rig.play(&[64, 64]);
    // A new node, connected and committed, then an event for it at once, stamped at the last
    // period's start. The controller holds it until the processor has applied the swap, at the
    // next period's start; it plays, late, from the period after: the node's value fades in from
    // zero, then from one half.
    let added = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(added, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    let at = rig.controller.stats().block_start().0;
    rig.send(added, at, 0, 0.5);
    let played = rig.play(&[64; 5]);
    let expected: Vec<f64> = (0..played.len())
        .map(|i| match i {
            0..64 => 0.0,
            _ if i < FADE => 0.5 * fade_in(i, FADE),
            _ => 0.5,
        })
        .collect();
    assert_matches(&played, &expected, 1e-6, "event after commit");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn an_event_for_a_node_whose_commit_is_deferred_follows_its_swap() {
    let mut rig = Rig::new(0, 1);
    let first = rig.add(Constant::new(1.0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    // A swap in flight, fading the first node in.
    rig.controller
        .connect(Source::Node(first, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    let added = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(added, 0), Destination::Playback(0));
    assert_eq!(rig.controller.commit().unwrap(), Commit::Deferred);
    rig.send(added, 0, 0, 0.5);
    rig.play_until_steady(64);
    // Steady: 1 from the first node, 0.5 from the second, whose event reached it.
    let played = rig.play(&[64]);
    assert_matches(&played, &[1.5; 64], 0.0, "event after a deferred commit");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn an_event_for_a_node_that_only_a_dips_second_phase_runs_waits_for_it() {
    let mut rig = Rig::new(0, 1);
    let old: Vec<_> = (0..4).map(|_| rig.add(Constant::new(1.0))).collect();
    rig.controller
        .connect(Source::Node(old[0], 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    for &node in &old {
        rig.controller.remove_node(node).unwrap();
    }
    let new: Vec<_> = (0..4).map(|_| rig.add(Constant::new(0.0))).collect();
    rig.controller
        .connect(Source::Node(new[0], 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.send(new[0], 64, 0, 1.0);
    let played = rig.play(&[64; 10]);
    // The dip: the old node fades out, then the new one fades in, already at 1.
    let expected: Vec<f64> = (0..played.len())
        .map(|i| match i {
            i if i < FADE => 1.0 - fade_in(i, FADE),
            i if i < 2 * FADE => fade_in(i - FADE, FADE),
            _ => 1.0,
        })
        .collect();
    assert_matches(&played, &expected, 1e-6, "event through a dip");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn an_event_for_a_removed_node_is_dropped_and_counted() {
    let mut rig = Rig::new(0, 1);
    let gone = rig.add(Constant::new(1.0));
    rig.controller
        .connect(Source::Node(gone, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    rig.controller.remove_node(gone).unwrap();
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    let at = rig.controller.stats().block_start().0 + 64;
    rig.send(gone, at, 0, 0.5);
    rig.play(&[64]);
    assert_eq!(rig.controller.stats().events_dropped(), 1);
}

#[test]
fn a_late_event_plays_at_the_next_periods_first_sample() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.25));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64, 64, 64]);
    rig.send(source, 10, 0, 0.75);
    let played = rig.play(&[64]);
    assert_matches(&played, &[0.75; 64], 0.0, "late event");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn a_full_event_queue_gives_the_event_back() {
    let mut config = EngineConfig::new(rate(), LARGEST_BLOCK, 0, 1);
    config.event_capacity = 4;
    let mut rig = Rig::with_config(config);
    let source = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let event = |at| TimedEvent {
        node: source,
        at: SamplePosition(at),
        kind: EventKind::Parameter {
            index: 0,
            value: 1.0,
        },
    };
    // Before the processor holds the node, the controller holds its events: as many as the queue.
    for at in 0..4 {
        rig.controller.send_event(event(at)).unwrap();
    }
    assert_eq!(
        rig.controller.send_event(event(4)),
        Err(SendEventError::QueueFull(event(4)))
    );
    // The swap applied, the poll after the period sends them; the queue is full in its turn.
    rig.play(&[64]);
    assert_eq!(
        rig.controller.send_event(event(64)),
        Err(SendEventError::QueueFull(event(64)))
    );
    // The processor takes the queued events at its next period, which makes room.
    rig.play(&[64]);
    rig.controller.send_event(event(128)).unwrap();
}

/// A note on key `key`, without id, at velocity 0.5.
fn note(key: u8) -> Note {
    Note {
        id: None,
        channel: 0,
        key,
        velocity: 0.5,
    }
}

#[test]
fn events_at_one_sample_reach_a_node_note_offs_first_then_in_the_order_sent() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.5));
    let (recorder, calls) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    let kinds = [
        (10, EventKind::NoteOn(note(60))),
        (
            10,
            EventKind::Parameter {
                index: 0,
                value: 0.25,
            },
        ),
        (10, EventKind::NoteOff(note(60))),
        (5, EventKind::NoteOn(note(62))),
        (
            10,
            EventKind::Controller {
                channel: 0,
                number: 64,
                value: 1.0,
            },
        ),
        (10, EventKind::NoteOff(note(62))),
        (
            10,
            EventKind::PitchBend {
                channel: 0,
                value: -0.5,
            },
        ),
        (20, EventKind::NoteOff(note(64))),
        (20, EventKind::NoteOn(note(64))),
    ];
    for (offset, kind) in kinds {
        rig.controller
            .send_event(TimedEvent {
                node: recorder,
                at: SamplePosition(64 + offset),
                kind,
            })
            .unwrap();
    }
    rig.play(&[64]);
    let event = |offset, kind| Event { offset, kind };
    assert_eq!(
        read(&calls)[1].all_events,
        [
            event(5, EventKind::NoteOn(note(62))),
            event(10, EventKind::NoteOff(note(60))),
            event(10, EventKind::NoteOff(note(62))),
            event(10, EventKind::NoteOn(note(60))),
            event(
                10,
                EventKind::Parameter {
                    index: 0,
                    value: 0.25,
                }
            ),
            event(
                10,
                EventKind::Controller {
                    channel: 0,
                    number: 64,
                    value: 1.0,
                }
            ),
            event(
                10,
                EventKind::PitchBend {
                    channel: 0,
                    value: -0.5,
                }
            ),
            event(20, EventKind::NoteOff(note(64))),
            event(20, EventKind::NoteOn(note(64))),
        ]
    );
}

#[test]
fn an_event_due_now_never_waits_behind_events_far_ahead() {
    let mut config = EngineConfig::new(rate(), LARGEST_BLOCK, 0, 1);
    config.event_capacity = 4;
    let mut rig = Rig::with_config(config);
    let source = rig.add(Constant::new(0.25));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let stats = rig.controller.stats().clone();
    let mut position = 64;
    rig.play(&[64]);
    // Four events far ahead fill the processor's pending buffer at its next period.
    for far in 0..4 {
        rig.send(source, 20_000 + far, 0, 0.5);
    }
    rig.play(&[64]);
    position += 64;
    assert_eq!(stats.events_taken(), 0);
    // A fifth, due now, is refused: queued, it would wait behind them.
    let now = TimedEvent {
        node: source,
        at: SamplePosition(position + 7),
        kind: EventKind::Parameter {
            index: 0,
            value: 0.75,
        },
    };
    assert_eq!(
        rig.controller.send_event(now),
        Err(SendEventError::QueueFull(now))
    );
    // Once the periods reach them, the processor takes them out, which makes room.
    let periods = [2_048, 100, 4_096, 64, 3_000];
    for &frames in periods.iter().cycle() {
        if stats.events_taken() == 4 {
            break;
        }
        rig.play(&[frames]);
        position += frames as i64;
    }
    assert!(position > 20_000);
    // An event due now then plays at its sample, here in the second sub-block of a long period.
    rig.send(source, position + 1_500, 0, 0.75);
    let played = rig.play(&[2_048]);
    let expected: Vec<f64> = (0..2_048)
        .map(|n| if n < 1_500 { 0.5 } else { 0.75 })
        .collect();
    assert_matches(&played, &expected, 0.0, "an event due now");
    assert_eq!(stats.events_taken(), 5);
    assert_eq!(stats.events_dropped(), 0);
}

#[test]
fn a_late_note_on_and_its_note_off_keep_their_order() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.5));
    let (recorder, calls) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64, 64]);
    // The engine is at sample 128: a note played at 10 and released at 20 comes late, whole.
    for (at, kind) in [
        (10, EventKind::NoteOn(note(60))),
        (20, EventKind::NoteOff(note(60))),
    ] {
        rig.controller
            .send_event(TimedEvent {
                node: recorder,
                at: SamplePosition(at),
                kind,
            })
            .unwrap();
    }
    rig.play(&[64]);
    // Both at the period's first sample, still in the order of their own samples: the note off
    // first would leave the note hanging.
    assert_eq!(
        read(&calls).last().unwrap().all_events,
        [
            Event {
                offset: 0,
                kind: EventKind::NoteOn(note(60))
            },
            Event {
                offset: 0,
                kind: EventKind::NoteOff(note(60))
            },
        ]
    );
}

#[test]
fn notes_held_through_a_dips_first_phase_keep_their_order() {
    let mut rig = Rig::new(0, 1);
    let old: Vec<_> = (0..4).map(|_| rig.add(Constant::new(1.0))).collect();
    rig.controller
        .connect(Source::Node(old[0], 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    for &node in &old {
        rig.controller.remove_node(node).unwrap();
    }
    // Four new nodes for four old ones: the swap dips, and the recorder runs from its second
    // phase only.
    let source = rig.add(Constant::new(0.5));
    let (recorder, calls) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.add(Constant::new(0.0));
    rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    // A note played and released before the swap is applied: the controller holds both, and the
    // processor takes them late, then keeps them through the dip's first phase.
    for (at, kind) in [
        (20, EventKind::NoteOn(note(62))),
        (30, EventKind::NoteOff(note(62))),
    ] {
        rig.controller
            .send_event(TimedEvent {
                node: recorder,
                at: SamplePosition(at),
                kind,
            })
            .unwrap();
    }
    rig.play(&[64; 8]);
    let calls = read(&calls);
    assert_eq!(
        calls[0].all_events,
        [
            Event {
                offset: 0,
                kind: EventKind::NoteOn(note(62))
            },
            Event {
                offset: 0,
                kind: EventKind::NoteOff(note(62))
            },
        ]
    );
    assert!(calls[1..].iter().all(|call| call.all_events.is_empty()));
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn held_events_count_against_the_processor_only_once_pushed() {
    let mut config = EngineConfig::new(rate(), LARGEST_BLOCK, 0, 1);
    config.event_capacity = 4;
    let mut rig = Rig::with_config(config);
    let old = rig.add(Constant::new(0.25));
    rig.controller
        .connect(Source::Node(old, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    let new = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(new, 0), Destination::Playback(0));
    let event = |node, at| TimedEvent {
        node,
        at: SamplePosition(at),
        kind: EventKind::Parameter {
            index: 0,
            value: 0.5,
        },
    };
    // The new node is not sent yet: the controller holds its events, as many as the capacity.
    for far in 0..4 {
        rig.controller.send_event(event(new, 50_000 + far)).unwrap();
    }
    let refused = event(new, 60_000);
    assert_eq!(
        rig.controller.send_event(refused),
        Err(SendEventError::QueueFull(refused))
    );
    // The held events take none of the processor's room: four more go to the node it holds.
    for far in 0..4 {
        rig.controller.send_event(event(old, 40_000 + far)).unwrap();
    }
    let refused = event(old, 40_100);
    assert_eq!(
        rig.controller.send_event(refused),
        Err(SendEventError::QueueFull(refused))
    );
    // Once the swap is applied, the held events wait for room, and are not lost: they go once
    // the processor has taken the first four, at their sample. Until then the controller is not
    // steady.
    rig.controller.commit().unwrap();
    rig.play_until_steady(LARGEST_BLOCK);
    let stats = rig.controller.stats().clone();
    assert_eq!(stats.events_taken(), 4);
    assert!(stats.block_start().0 + LARGEST_BLOCK as i64 > 40_003);
    while stats.events_taken() < 8 && stats.block_start().0 < 200_000 {
        rig.play(&[4_096]);
    }
    assert_eq!(stats.events_taken(), 8);
    assert_eq!(stats.events_dropped(), 0);
}
