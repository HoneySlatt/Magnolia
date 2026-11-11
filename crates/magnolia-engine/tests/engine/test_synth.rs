//! The test synth: a voice is a sine and a sawtooth at half its level, under a linear attack and
//! release of 5 ms each that end exactly at zero, on both outputs, from its event's sample;
//! velocity scales it, and a velocity under a millionth does not sound, so that no sample is
//! subnormal; a pitch bend moves it by up to two semitones; a 17th note steals the oldest voice;
//! malformed notes are ignored; it sleeps when no voice sounds; its output does not depend on how
//! blocks are cut.

use std::f64::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use magnolia_engine::engine::{Destination, EngineConfig, NodeId, Source, TimedEvent};
use magnolia_engine::node::{EventKind, Note};
use magnolia_engine::nodes::TestSynth;
use magnolia_engine::time::SamplePosition;

use crate::support::{Counted, FADE, RATE, Rig, rate};
use crate::timeline_support::{Random, first_sound, play_channels};

/// A synth, counted, its two outputs on two playback channels, after a first period of 64 frames:
/// the processor holds it, so events sent now arrive on time.
fn synth_rig() -> (Rig, NodeId, Arc<AtomicUsize>) {
    synth_rig_with(EngineConfig::new(rate(), 1_024, 0, 2))
}

fn synth_rig_with(config: EngineConfig) -> (Rig, NodeId, Arc<AtomicUsize>) {
    let mut rig = Rig::with_config(config);
    let (synth, calls) = Counted::new(TestSynth::new(rate()));
    let synth = rig.add(synth);
    for channel in 0..2 {
        rig.controller
            .connect(Source::Node(synth, channel), Destination::Playback(channel));
    }
    rig.controller.commit().unwrap();
    play_channels(&mut rig, &[64]);
    (rig, synth, calls)
}

fn send(rig: &mut Rig, node: NodeId, at: i64, kind: EventKind) {
    rig.controller
        .send_event(TimedEvent {
            node,
            at: SamplePosition(at),
            kind,
        })
        .unwrap();
}

fn on(channel: u8, key: u8, velocity: f32) -> EventKind {
    EventKind::NoteOn(Note {
        id: None,
        channel,
        key,
        velocity,
    })
}

fn off(channel: u8, key: u8) -> EventKind {
    EventKind::NoteOff(Note {
        id: None,
        channel,
        key,
        velocity: 0.0,
    })
}

/// 2^64, the phase units in a cycle.
const CYCLE: f64 = 18_446_744_073_709_551_616.0;

/// One voice of `key` at `velocity`, struck at sample 0 and released at `released`, written from
/// its definition: the sine plus half a sawtooth, a phase advanced by a whole step of 2^-64 cycles,
/// the envelope one step of 1/240 a sample up to full, then down to zero.
fn voice(key: u8, velocity: f32, released: usize, frames: usize) -> Vec<f32> {
    let frequency = 440.0 * ((f64::from(key) - 69.0) / 12.0).exp2();
    let step = ((frequency / f64::from(RATE)).rem_euclid(1.0) * CYCLE) as u64;
    let mut level = 0_u32;
    let mut phase = 0_u64;
    (0..frames)
        .map(|sample| {
            level = if sample < released {
                (level + 1).min(FADE as u32)
            } else {
                level.saturating_sub(1)
            };
            let gain = f64::from(level) / FADE as f64 * f64::from(velocity);
            let cycles = phase as f64 / CYCLE;
            phase = phase.wrapping_add(step);
            if sample >= released && level == 0 {
                return 0.0;
            }
            (gain * ((TAU * cycles).sin() + 0.5 * (2.0 * cycles - 1.0))) as f32
        })
        .collect()
}

#[test]
fn a_voice_rises_and_falls_in_5_ms_from_its_events_samples_on_both_outputs() {
    let (mut rig, synth, _) = synth_rig();
    // Struck at sample 100, released 1,000 samples later.
    send(&mut rig, synth, 100, on(0, 69, 0.5));
    send(&mut rig, synth, 1_100, off(0, 69));
    let played = play_channels(&mut rig, &[2_048]);
    assert_eq!(played[0], played[1]);
    let expected = voice(69, 0.5, 1_000, 1_400);
    assert_eq!(first_sound(&played[0]), Some(100 - 64));
    assert_eq!(played[0][36..36 + 1_400], expected[..]);
    // The release reaches exactly zero, a fade after the note off, and stays there.
    assert_ne!(expected[1_000 + FADE - 2], 0.0);
    assert!(
        played[0][36 + 1_000 + FADE - 1..]
            .iter()
            .all(|&sample| sample == 0.0)
    );
    assert!(
        played[0]
            .iter()
            .all(|&sample| sample == 0.0 || sample.is_normal())
    );
}

#[test]
fn velocity_scales_a_voice() {
    let render = |velocity| {
        let (mut rig, synth, _) = synth_rig();
        send(&mut rig, synth, 100, on(0, 60, velocity));
        send(&mut rig, synth, 1_100, off(0, 60));
        play_channels(&mut rig, &[2_048]).swap_remove(0)
    };
    let (full, half) = (render(1.0), render(0.5));
    assert!(full.iter().any(|&sample| sample != 0.0));
    for (full, half) in full.iter().zip(&half) {
        assert_eq!(*half, full * 0.5);
    }
}

#[test]
fn a_velocity_under_a_millionth_does_not_sound() {
    let render = |velocity| {
        let (mut rig, synth, calls) = synth_rig();
        send(&mut rig, synth, 100, on(0, 69, velocity));
        let played = play_channels(&mut rig, &[1_024; 2]).swap_remove(0);
        (played, calls.load(Ordering::Relaxed))
    };
    for velocity in [1.0e-37, 1.0e-12, 0.99e-6] {
        let (played, calls) = render(velocity);
        assert!(played.iter().all(|&sample| sample == 0.0), "{velocity}");
        // It woke for the note, found no voice to sound, and slept again.
        assert_eq!(calls, 2, "{velocity}");
    }
    let (played, _) = render(1.0e-6);
    assert!(played.iter().any(|&sample| sample != 0.0));
    assert!(played.iter().all(|&sample| !sample.is_subnormal()));
}

#[test]
fn a_pitch_bend_moves_its_channels_voices_by_up_to_two_semitones() {
    let render = |key, bend: Option<(i64, f32)>| {
        let (mut rig, synth, _) = synth_rig();
        if let Some((at, value)) = bend {
            send(
                &mut rig,
                synth,
                at,
                EventKind::PitchBend { channel: 3, value },
            );
        }
        send(&mut rig, synth, 100, on(3, key, 0.8));
        play_channels(&mut rig, &[4_096]).swap_remove(0)
    };
    assert_eq!(render(69, Some((80, 1.0))), render(71, None));
    assert_eq!(render(69, Some((80, -1.0))), render(67, None));
    // Past its range, a bend is taken at its end.
    assert_eq!(render(69, Some((80, 7.0))), render(71, None));
    // A bend while the note sounds changes it from its sample.
    let (unbent, bent) = (render(69, None), render(69, Some((2_000, 0.5))));
    let at = 2_000 - 64;
    assert_eq!(unbent[..at], bent[..at]);
    assert_ne!(unbent[at + 1..at + 100], bent[at + 1..at + 100]);
}

/// Sixteen notes, keys 40 to 55, ten samples apart, a 17th note on key 70 if `seventeenth`, and a
/// note off for `released` at sample 1,000.
fn sixteen_notes(seventeenth: bool, released: Option<u8>) -> Vec<f32> {
    let (mut rig, synth, _) = synth_rig();
    for key in 40..56 {
        send(
            &mut rig,
            synth,
            100 + 10 * i64::from(key - 40),
            on(0, key, 0.5),
        );
    }
    if seventeenth {
        send(&mut rig, synth, 400, on(0, 70, 0.5));
    }
    if let Some(key) = released {
        send(&mut rig, synth, 1_000, off(0, key));
    }
    play_channels(&mut rig, &[2_048]).swap_remove(0)
}

#[test]
fn a_seventeenth_note_steals_the_oldest_voice() {
    // With sixteen notes, every one holds its voice: releasing the first changes the sound.
    assert_ne!(sixteen_notes(false, Some(40)), sixteen_notes(false, None));
    // The seventeenth takes the first note's voice: releasing the first changes nothing, releasing
    // the second does.
    let stolen = sixteen_notes(true, None);
    assert_eq!(sixteen_notes(true, Some(40)), stolen);
    assert_ne!(sixteen_notes(true, Some(41)), stolen);
}

#[test]
fn notes_on_a_channel_out_of_range_or_of_a_velocity_not_a_number_are_ignored() {
    let (mut rig, synth, _) = synth_rig();
    send(&mut rig, synth, 100, on(16, 60, 0.5));
    send(&mut rig, synth, 100, on(0, 60, f32::NAN));
    send(&mut rig, synth, 100, on(0, 61, f32::INFINITY));
    send(
        &mut rig,
        synth,
        100,
        EventKind::PitchBend {
            channel: 0,
            value: f32::NAN,
        },
    );
    let played = play_channels(&mut rig, &[1_024]);
    assert!(played.iter().all(|channel| first_sound(channel).is_none()));
}

#[test]
fn it_sleeps_while_no_voice_sounds() {
    let (mut rig, synth, calls) = synth_rig();
    // Its first call found no voice: asleep, it is not called again.
    play_channels(&mut rig, &[64; 10]);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    // A note wakes it; its release ends at sample 1,000 + a fade, in the period from 1,024; the
    // period after finds no voice, and it sleeps again.
    send(&mut rig, synth, 704, on(0, 60, 0.5));
    send(&mut rig, synth, 1_000, off(0, 60));
    play_channels(&mut rig, &[64; 30]);
    let woken = calls.load(Ordering::Relaxed);
    assert_eq!(woken, 1 + (1_280 - 704) / 64 + 1);
    play_channels(&mut rig, &[64; 10]);
    assert_eq!(calls.load(Ordering::Relaxed), woken);
}

#[test]
fn its_output_does_not_depend_on_how_blocks_are_cut() {
    let render = |periods: &mut dyn FnMut() -> usize| {
        let config = EngineConfig::new(rate(), 4_096, 0, 2);
        let (mut rig, synth, _) = synth_rig_with(config);
        let mut random = Random(42);
        for index in 0..40_i64 {
            let at = 100 + 1_000 * index + (random.next() % 900) as i64;
            let key = 40 + (random.next() % 40) as u8;
            let channel = (random.next() % 2) as u8;
            send(&mut rig, synth, at, on(channel, key, 0.75));
            send(&mut rig, synth, at + 3_000, off(channel, key));
            let value = (random.next() % 2_001) as f32 / 1_000.0 - 1.0;
            send(
                &mut rig,
                synth,
                at + 500,
                EventKind::PitchBend { channel, value },
            );
        }
        let mut played = Vec::new();
        while played.len() < 46_000 {
            played.extend(rig.period(periods()));
        }
        played.truncate(46_000);
        played
    };
    let whole = render(&mut || 4_096);
    let mut random = Random(7);
    let cut = render(&mut || random.up_to(5_000));
    assert!(whole.iter().any(|&sample| sample != 0.0));
    assert_eq!(whole, cut);
}
