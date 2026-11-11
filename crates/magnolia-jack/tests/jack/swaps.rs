//! A swap makes no click through JACK: a plain JACK client records Magnolia's playback across a
//! crossfade between two sines, and no step between two samples exceeds the crossfade's bound, as
//! the engine's own swap tests compute it.

use std::time::Duration;

use magnolia_engine::engine::{Commit, Destination, Source};
use magnolia_engine::nodes::Sine;

use crate::common::Server;
use crate::support::{
    Probe, Rig, Take, crossfade_step_bound, first_unbroken_take, largest_step, one_at_a_time, rate,
    silence,
};

const FIRST: (f64, f64) = (440.0, 0.5);
const SECOND: (f64, f64) = (660.0, 0.4);

/// One and a half seconds at 48 kHz.
const LENGTH: usize = 72_000;

/// Records Magnolia's playback from silence, through the first sine's start, then across the
/// crossfade to the second sine.
fn record_a_crossfade() -> Take<Vec<f32>> {
    let server = Server::start(1_024);
    let mut rig = Rig::open(&server, 0, 1);
    let first = rig.add(Sine::new(rate(), FIRST.0, FIRST.1 as f32));
    let second = rig.add(Sine::new(rate(), SECOND.0, SECOND.1 as f32));
    let probe = Probe::open(&server, silence, LENGTH);
    probe.connect(&rig.backend.playback_ports()[0], &probe.input);
    probe.wait_for_samples(4_800, Duration::from_secs(10));
    let gaps = rig.controller.stats().frame_time_gaps();

    // The first sine starts at its phase 0, from silence: no step beyond its own.
    rig.controller
        .connect(Source::Node(first, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    probe.wait_for_samples(LENGTH / 2, Duration::from_secs(10));

    rig.controller
        .disconnect(Source::Node(first, 0), Destination::Playback(0))
        .unwrap();
    rig.controller
        .connect(Source::Node(second, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    rig.wait_until_steady();
    probe.wait_for_samples(LENGTH, Duration::from_secs(10));
    let stats = rig.controller.stats();
    assert_eq!(stats.last_swap_steady(), 2);
    assert_eq!(stats.non_finite_samples(), 0);
    let skipped_cycles = stats.frame_time_gaps() - gaps + probe.gaps();
    Take {
        result: probe.finish(),
        skipped_cycles,
    }
}

#[test]
fn a_crossfade_between_two_sines_never_steps_beyond_the_fades_bound() {
    let _turn = one_at_a_time();
    let recorded = first_unbroken_take(record_a_crossfade);

    let bound = crossfade_step_bound(FIRST, SECOND);
    let (step, at) = largest_step(&recorded);
    assert!(
        step <= bound,
        "a step of {step} at sample {at}, beyond the crossfade's bound {bound}"
    );
    // Both sines played: the first's peak before the swap, the second's at the end.
    let peak = |samples: &[f32]| {
        samples
            .iter()
            .fold(0.0f32, |peak, &sample| peak.max(sample.abs()))
    };
    let before = &recorded[LENGTH / 2 - 4_800..LENGTH / 2];
    let end = &recorded[LENGTH - 4_800..];
    assert!((f64::from(peak(before)) - FIRST.1).abs() < 0.01);
    assert!((f64::from(peak(end)) - SECOND.1).abs() < 0.01);
    // Without its fade, the same swap would step by up to both sines' amplitudes.
    assert!(FIRST.1 + SECOND.1 > 4.0 * bound);
}
