//! The engine's own nodes: their output does not depend on how blocks are cut, a sine never
//! sleeps, and a sine and a gain ignore the events that are not their parameters'. A node is never
//! given more than the engine's largest block.

use magnolia_engine::engine::{Destination, EngineConfig, Source, TimedEvent};
use magnolia_engine::node::{EventKind, Note};
use magnolia_engine::nodes::{Gain, Sine};
use magnolia_engine::time::SamplePosition;

use std::sync::atomic::Ordering;

use crate::support::{Constant, Counted, Recorder, Rig, rate, read};

/// A sine at 440 Hz through a gain of 0.5, with events at a few samples, played in a first period
/// of 64 frames, then in `periods`.
fn sine_through_a_gain(periods: &[usize]) -> Vec<f32> {
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.8));
    let gain = rig.add(Gain::new(1, 0.5));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    // The same first period in every cut, after which the processor holds the nodes, so that the
    // events below are sent on time.
    let mut played = rig.play(&[64]);
    rig.send(sine, 333, Sine::FREQUENCY, 1_234.5);
    rig.send(gain, 1_500, Gain::GAIN, 0.25);
    rig.send(sine, 2_900, Sine::GAIN, f64::NAN);
    played.extend(rig.play(periods));
    played
}

#[test]
fn a_sine_and_a_gains_output_does_not_depend_on_how_blocks_are_cut() {
    let reference = sine_through_a_gain(&[4_096]);
    let cuts: [&[usize]; 4] = [
        &[64; 64],
        &[100; 41],
        &[1, 2_047, 2_048],
        &[333, 1_167, 1, 2_595],
    ];
    for periods in cuts {
        let played = sine_through_a_gain(periods);
        assert_eq!(
            played[..reference.len()],
            reference[..],
            "periods {periods:?}"
        );
    }
}

#[test]
fn a_sine_never_sleeps_even_at_zero_gain() {
    let mut rig = Rig::new(0, 1);
    let (sine, calls) = Counted::new(Sine::new(rate(), 440.0, 0.0));
    let sine = rig.add(sine);
    let (recorder, recorded) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let played = rig.play(&[64; 3]);
    assert!(played.iter().all(|&sample| sample == 0.0));
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert!(read(&recorded).iter().all(|call| !call.input_silent));
}

#[test]
fn a_node_is_never_given_more_than_the_largest_block() {
    // 100 frames, whose stride rounds up to 112: the sub-blocks follow the largest block.
    let mut rig = Rig::with_config(EngineConfig::new(rate(), 100, 0, 1));
    let source = rig.add(Constant::new(1.0));
    let (recorder, recorded) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[300, 250, 37]);
    let frames: Vec<usize> = read(&recorded).iter().map(|call| call.frames).collect();
    assert_eq!(frames, [100, 100, 100, 100, 100, 50, 37]);
}

/// A sine through a gain, as `sine_through_a_gain` plays it, with notes, bends and controllers
/// sent to both at a few samples when `with_other_events`.
fn sine_through_a_gain_with_other_events(with_other_events: bool) -> Vec<f32> {
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.8));
    let gain = rig.add(Gain::new(1, 0.5));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let mut played = rig.play(&[64]);
    rig.send(gain, 1_500, Gain::GAIN, 0.25);
    if with_other_events {
        let note = Note {
            id: Some(7),
            channel: 1,
            key: 60,
            velocity: 1.0,
        };
        let kinds = [
            EventKind::NoteOn(note),
            EventKind::NoteOff(note),
            EventKind::PitchBend {
                channel: 1,
                value: 1.0,
            },
            EventKind::Controller {
                channel: 1,
                number: 7,
                value: 0.0,
            },
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            for node in [sine, gain] {
                let at = SamplePosition(100 + 700 * index as i64);
                rig.controller
                    .send_event(TimedEvent { node, at, kind })
                    .unwrap();
            }
        }
    }
    played.extend(rig.play(&[1_000, 2_048, 37]));
    played
}

#[test]
fn a_sine_and_a_gain_ignore_notes_bends_and_controllers() {
    assert_eq!(
        sine_through_a_gain_with_other_events(true),
        sine_through_a_gain_with_other_events(false)
    );
}
