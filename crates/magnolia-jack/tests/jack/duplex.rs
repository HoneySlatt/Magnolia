//! Duplex: a plain JACK client plays a ramp into Magnolia's capture port; the engine routes capture
//! to playback through a gain of one half; the client records Magnolia's playback. Every sample
//! comes back halved, at one latency from the first to the last. Once connected, the server
//! freewheels: the test needs JACK's processing path, not its timing.

use std::time::Duration;

use magnolia_engine::engine::{Destination, Source};
use magnolia_engine::nodes::Gain;

use crate::common::Server;
use crate::support::{Freewheel, Probe, Rig, Take, first_unbroken_take, one_at_a_time, ramp};

/// Two seconds at 48 kHz.
const LENGTH: usize = 96_000;

/// Records Magnolia's playback of the probe's ramp, through the engine's gain.
fn record_the_ramp() -> Take<Vec<f32>> {
    let server = Server::start(1_024);
    let mut rig = Rig::open(&server, 1, 1);
    let gain = rig.add(Gain::new(1, 0.5));
    rig.controller
        .connect(Source::Capture(0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    let gaps = rig.controller.stats().frame_time_gaps();

    let probe = Probe::open(&server, ramp, LENGTH);
    probe.connect(&probe.output, &rig.backend.capture_ports()[0]);
    probe.connect(&rig.backend.playback_ports()[0], &probe.input);
    let freewheel = Freewheel::start(&server);
    probe.wait_for_samples(LENGTH, Duration::from_secs(20));
    // Read before the server leaves freewheel, which may count a gap of its own.
    let stats = rig.controller.stats();
    let skipped_cycles = stats.frame_time_gaps() - gaps + probe.gaps();
    freewheel.stop();
    assert!(!stats.poisoned());
    Take {
        result: probe.finish(),
        skipped_cycles,
    }
}

#[test]
fn a_signal_comes_back_sample_for_sample_at_a_constant_latency() {
    let _turn = one_at_a_time();
    let recorded = first_unbroken_take(record_the_ramp);

    // Until both connections hold, the probe records silence; then each sample is half the ramp's
    // sample played `latency` samples earlier, `2 * recorded - 1` being the ramp's count.
    let first = recorded
        .iter()
        .position(|&sample| sample != 0.0)
        .expect("the signal came back");
    let played_at = (2.0 * f64::from(recorded[first]) - 1.0) as usize;
    let latency = first
        .checked_sub(played_at)
        .expect("the signal came back after it was played");
    assert!(
        first + 48_000 < LENGTH,
        "the signal came back late: sample {first}"
    );
    for (index, &sample) in recorded.iter().enumerate().skip(first) {
        let expected = 0.5 * ramp((index - latency) as u64);
        assert_eq!(
            sample, expected,
            "sample {index}: latency {latency} from sample {first} on"
        );
    }
    assert!(latency <= 2 * 1_024, "a latency of {latency} samples");
}
