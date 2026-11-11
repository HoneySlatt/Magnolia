//! A panic gives silence through JACK (ADR-0022): a node panics, the recorded playback is silent
//! from then on while the client stays active and connected, and the engine reports itself
//! poisoned. A new processor handed to the running backend brings the sound back without
//! reconnecting, and the old one is dropped on the handle's thread, never on the audio thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use magnolia_engine::engine::{Destination, Source};

use crate::common::Server;
use crate::support::{
    Constant, DropWitness, Panicking, Probe, Rig, new_engine, one_at_a_time, silence,
    this_thread_id, wait_for,
};

/// Four seconds at 48 kHz, room for every phase of the test.
const LENGTH: usize = 192_000;

#[test]
fn a_panic_gives_silence_and_a_new_processor_brings_the_sound_back_in_place() {
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let mut rig = Rig::open(&server, 0, 1);
    let armed = Arc::new(AtomicBool::new(false));
    let dropped_on = Arc::new(AtomicI32::new(0));
    let ones = rig.add(Panicking {
        armed: Arc::clone(&armed),
    });
    let witness = rig.add(DropWitness {
        dropped_on: Arc::clone(&dropped_on),
    });
    rig.controller
        .connect(Source::Node(ones, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(witness, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    let probe = Probe::open(&server, silence, LENGTH);
    let playback = rig.backend.playback_ports()[0].clone();
    probe.connect(&playback, &probe.input);
    wait_for("the ones", Duration::from_secs(10), || {
        probe.recorded() > 4_800
    });

    let stats = Arc::clone(rig.controller.stats());
    armed.store(true, Ordering::Relaxed);
    wait_for("the panic", Duration::from_secs(10), || stats.poisoned());
    let periods = stats.periods();
    let poisoned_at = probe.recorded();
    probe.wait_for_samples(poisoned_at + 24_000, Duration::from_secs(10));
    // The heartbeat goes on, the client stays active, and the connection stays.
    assert!(stats.periods() > periods + 50);
    assert!(!rig.backend.server_shut_down());
    let port = probe.client().port_by_name(&probe.input).unwrap();
    assert!(port.is_connected_to(&playback).unwrap());

    // A new engine, played by the same backend: a constant of one quarter.
    let (mut controller, processor) = new_engine(0, 1);
    let quarter = controller.add_node(Box::new(Constant(0.25))).unwrap();
    controller.connect(Source::Node(quarter, 0), Destination::Playback(0));
    controller.commit().unwrap();
    assert!(rig.backend.replace_processor(processor).is_ok());
    controller
        .wait_until_steady(Duration::from_secs(5))
        .expect("the new processor applies its graph");
    let replaced_at = probe.recorded();
    probe.wait_for_samples(replaced_at + 24_000, Duration::from_secs(10));

    // The old processor came back to the handle, which drops it here.
    rig.backend.poll();
    let audio_thread = controller.stats().audio_thread_id();
    assert_ne!(audio_thread, 0);
    assert_eq!(
        dropped_on.load(Ordering::Acquire),
        this_thread_id(),
        "the old processor's nodes were dropped on the handle's thread, not the audio thread {audio_thread}"
    );
    assert!(port.is_connected_to(&playback).unwrap());
    assert!(!controller.stats().poisoned());
    let recorded = probe.finish();

    // Silence until the probe was connected, ones until the panic, silence until the new
    // processor, then a quarter.
    let ones_start = recorded.iter().position(|&sample| sample == 1.0).unwrap();
    let ones_end = recorded.iter().rposition(|&sample| sample == 1.0).unwrap();
    assert!(
        recorded[ones_start..=ones_end]
            .iter()
            .all(|&sample| sample == 1.0),
        "ones without a break until the panic"
    );
    assert!(
        ones_end < poisoned_at,
        "ones after the panic, at {ones_end}"
    );
    let quarter_start = recorded.iter().position(|&sample| sample == 0.25).unwrap();
    assert!(
        recorded[ones_end + 1..quarter_start]
            .iter()
            .all(|&sample| sample == 0.0),
        "silence from the panic until the new processor"
    );
    assert!(quarter_start - ones_end > 24_000, "the silence lasted");
    assert!(
        recorded[quarter_start..]
            .iter()
            .all(|&sample| sample == 0.25),
        "the new processor's sound from then on"
    );
}
