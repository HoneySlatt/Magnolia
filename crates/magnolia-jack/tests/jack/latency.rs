//! The device's latencies, read in the backend's latency callback (ADR-0031), on JACK2's dummy
//! driver at 64 and at 256 frames. In its default, asynchronous mode, JACK2 reports one period on
//! its `system:` capture ports and two on its playback ports, the second for the asynchronous
//! mode's buffering: so, connected to them, Magnolia's capture port reports one period of capture
//! latency and its playback port two of playback. `JackBackend::device_latency` and the processor's
//! `EngineStats` give the same, a processor swapped in included. With a monitoring latency `L`, a
//! client reading Magnolia's output port's capture range and its input port's playback range sees
//! `L` more. These need the server's `system:` ports, which a freewheeling server disconnects: they
//! run in real time.

use std::time::Duration;

use jack::{Client, ClientOptions, LatencyType};
use magnolia_engine::stats::DeviceLatency;

use crate::common::Server;
use crate::support::{Rig, new_engine, one_at_a_time, wait_for};

/// How long a test waits for the latency callback.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The latency of Magnolia's monitoring path the tests set, in frames.
const MONITORING: u32 = 100;

#[test]
fn the_dummy_drivers_latencies_are_read_and_reported_at_64_frames() {
    reads_and_reports_the_dummy_drivers_latencies(64);
}

#[test]
fn the_dummy_drivers_latencies_are_read_and_reported_at_256_frames() {
    reads_and_reports_the_dummy_drivers_latencies(256);
}

/// A port's latency range in `mode`, as another client reads it.
fn range(client: &Client, port: &str, mode: LatencyType) -> (u32, u32) {
    client
        .port_by_name(port)
        .unwrap_or_else(|| panic!("no port {port}"))
        .get_latency_range(mode)
        .unwrap()
}

fn reads_and_reports_the_dummy_drivers_latencies(period: u32) {
    let _turn = one_at_a_time();
    let server = Server::start(period);
    let mut rig = Rig::open(&server, 1, 1);
    let capture = rig.backend.capture_ports()[0].clone();
    let playback = rig.backend.playback_ports()[0].clone();
    let (patchbay, _) =
        Client::new_with_server_name("patchbay", &server.name, ClientOptions::NO_START_SERVER)
            .expect("a second client opens");

    // Connected to nothing, the ports have no latency; the server runs the callback when asked,
    // though Magnolia's client is connected to no other.
    assert_eq!(
        rig.backend.wait_for_latency(&[], TIMEOUT).unwrap(),
        DeviceLatency::default()
    );

    // Connected by another client, the capture port's latency once the callback has run.
    patchbay
        .connect_ports_by_name("system:capture_1", &capture)
        .unwrap();
    let latency = rig
        .backend
        .wait_for_latency(&[("system:capture_1", &capture)], TIMEOUT)
        .unwrap();
    assert_eq!(
        latency,
        DeviceLatency {
            capture: period,
            playback: 0
        }
    );

    // Connected by the backend, which waits for the callback itself.
    assert_eq!(rig.backend.connect_to_physical_playback().unwrap(), 1);
    let expected = DeviceLatency {
        capture: period,
        playback: 2 * period,
    };
    assert_eq!(rig.backend.device_latency(), expected);
    assert_eq!(expected.round_trip(), u64::from(3 * period));
    let stats = std::sync::Arc::clone(rig.controller.stats());
    rig.wait_periods(2);
    assert_eq!(stats.device_latency(), expected, "the engine's statistics");

    // What other clients read: the device's ranges on each side, carried across Magnolia with no
    // monitoring latency, then with one.
    let device = |client: &Client| {
        (
            range(client, &capture, LatencyType::Capture),
            range(client, &playback, LatencyType::Playback),
        )
    };
    let across = |client: &Client| {
        (
            range(client, &playback, LatencyType::Capture),
            range(client, &capture, LatencyType::Playback),
        )
    };
    let (one, two) = (period, 2 * period);
    assert_eq!(device(&patchbay), ((one, one), (two, two)));
    assert_eq!(across(&patchbay), ((one, one), (two, two)));
    rig.backend.set_monitoring_latency(MONITORING).unwrap();
    let (one_later, two_later) = (one + MONITORING, two + MONITORING);
    assert_eq!(
        across(&patchbay),
        ((one_later, one_later), (two_later, two_later)),
        "the output's capture range and the input's playback range, {MONITORING} frames later"
    );
    assert_eq!(device(&patchbay), ((one, one), (two, two)));
    assert_eq!(rig.backend.device_latency(), expected);

    // A processor swapped in has them from its first period.
    let (new_controller, processor) = new_engine(1, 1);
    assert!(rig.backend.replace_processor(processor).is_ok());
    let new_stats = std::sync::Arc::clone(new_controller.stats());
    wait_for("the new processor's periods", TIMEOUT, || {
        new_stats.periods() > 0
    });
    assert_eq!(new_stats.device_latency(), expected, "the new processor's");

    // A disconnection reaches the callback as a connection does.
    patchbay
        .disconnect_ports_by_name("system:capture_1", &capture)
        .unwrap();
    wait_for("the capture latency's fall", TIMEOUT, || {
        rig.backend.device_latency().capture == 0
    });
    wait_for("the engine's capture latency's fall", TIMEOUT, || {
        new_stats.device_latency().capture == 0
    });
    assert_eq!(rig.backend.device_latency().playback, two);
    rig.backend.poll();
}
