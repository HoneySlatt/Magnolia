//! What the processor does with each node's status, and with what a node writes: a sleeping node
//! is skipped until a source sounds or an event comes; an erroring node is silenced, counted and
//! called again; a panic poisons the processor; non-finite samples never reach the device.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use magnolia_engine::engine::{Destination, Source};
use magnolia_engine::nodes::Gain;

use crate::support::{
    Constant, Counted, Failing, NonFinite, Panicking, Recorder, Rig, assert_matches, read,
};

#[test]
fn a_gain_sleeps_while_its_inputs_are_silent_and_wakes_when_a_source_sounds() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.0));
    let (gain, gain_calls) = Counted::new(Gain::new(1, 0.5));
    let gain = rig.add(gain);
    let (recorder, recorded) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();

    // Called once, it finds its input silent and sleeps; then it is skipped.
    let played = rig.play(&[64; 4]);
    assert_matches(&played, &[0.0; 256], 0.0, "asleep");
    assert_eq!(gain_calls.load(Ordering::Relaxed), 1);
    assert!(read(&recorded).iter().all(|call| call.input_silent));

    // An event for the gain wakes it for one sub-block, though its input is still silent.
    let at = rig.controller.stats().block_start().0 + 64;
    rig.send(gain, at, Gain::GAIN, 2.0);
    rig.play(&[64; 2]);
    assert_eq!(gain_calls.load(Ordering::Relaxed), 2);

    // The source sounds from sample 10 of the next period: the gain runs again, at its new gain.
    let at = rig.controller.stats().block_start().0 + 64 + 10;
    rig.send(source, at, 0, 0.25);
    let played = rig.play(&[64]);
    let expected: Vec<f64> = (0..64).map(|n| if n < 10 { 0.0 } else { 0.5 }).collect();
    assert_matches(&played, &expected, 0.0, "awake");
    assert_eq!(gain_calls.load(Ordering::Relaxed), 3);
    assert!(!read(&recorded).last().unwrap().input_silent);
}

#[test]
fn an_erroring_nodes_outputs_are_silent_and_counted_and_it_is_called_again() {
    let mut rig = Rig::new(0, 1);
    let (failing, calls) = Counted::new(Failing);
    let failing = rig.add(failing);
    let (recorder, recorded) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(failing, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let played = rig.play(&[64; 5]);
    assert_matches(&played, &[0.0; 320], 0.0, "discarded");
    assert_eq!(calls.load(Ordering::Relaxed), 5);
    assert_eq!(rig.controller.stats().node_errors(), 5);
    assert!(read(&recorded).iter().all(|call| call.input_silent));
}

#[test]
fn a_panicking_node_poisons_the_processor_which_then_plays_silence() {
    let mut rig = Rig::new(0, 2);
    let armed = Arc::new(AtomicBool::new(false));
    let node = rig.add(Panicking {
        armed: Arc::clone(&armed),
    });
    rig.controller
        .connect(Source::Node(node, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    assert_matches(&rig.play(&[64]), &[1.0; 64], 0.0, "before");
    assert!(!rig.controller.stats().poisoned());

    armed.store(true, Ordering::Relaxed);
    // Every period from the panic on returns, with silence on every playback channel.
    for period in 0..3 {
        let played = rig.backend.play_period(64, &[]);
        for channel in played {
            assert_matches(
                channel,
                &[0.0; 64],
                0.0,
                &format!("poisoned, period {period}"),
            );
        }
    }
    let stats = rig.controller.stats();
    assert!(stats.poisoned());
    assert_eq!(stats.periods(), 4);
    assert_eq!(stats.callback_times.snapshot().count(), 4);
}

#[test]
fn non_finite_samples_never_reach_the_device_and_are_counted() {
    let mut rig = Rig::new(0, 1);
    let node = rig.add(NonFinite);
    let gain = rig.add(Gain::new(1, 0.5));
    rig.controller
        .connect(Source::Node(node, 0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let played = rig.play(&[64, 100]);
    // Each sub-block starts the node's pattern again: NaN, infinity, minus infinity, one.
    let expected: Vec<f64> = (0..64)
        .chain(0..100)
        .map(|n| if n % 4 == 3 { 0.5 } else { 0.0 })
        .collect();
    assert_matches(&played, &expected, 0.0, "non-finite");
    assert_eq!(rig.controller.stats().non_finite_samples(), 48 + 75);
}
