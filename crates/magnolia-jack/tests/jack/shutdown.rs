//! The server stopped or killed under a playing backend: the backend reports it, nothing hangs or
//! crashes, and `close` returns.

use std::thread;
use std::time::{Duration, Instant};

use magnolia_engine::engine::{Destination, Source};
use magnolia_engine::nodes::Sine;
use magnolia_jack::{CloseError, JackBackend};

use crate::common::{Server, pause};
use crate::support::{Rig, one_at_a_time, rate, wait_for};

/// A rig playing a sine on a server of its own.
fn playing() -> (Server, Rig) {
    let server = Server::start(256);
    let mut rig = Rig::open(&server, 0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.1));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    rig.wait_periods(10);
    (server, rig)
}

/// Closes the backend on a thread of its own and waits for it, up to ten seconds, so that a close
/// that hangs fails the test rather than blocking it.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's thread waits for a closing thread; it never runs on the real-time path"
)]
fn close_within_ten_seconds(backend: JackBackend) -> Result<(), CloseError> {
    let closing = thread::spawn(move || backend.close().map(drop));
    let start = Instant::now();
    while !closing.is_finished() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "close did not return within 10 s"
        );
        pause(Duration::from_millis(5));
    }
    closing.join().expect("close did not panic")
}

#[test]
fn a_server_killed_mid_run_is_reported_and_close_returns() {
    let _turn = one_at_a_time();
    let (mut server, rig) = playing();
    assert!(!rig.backend.server_shut_down());
    server.kill();
    wait_for("the shutdown's report", Duration::from_secs(10), || {
        rig.backend.server_shut_down()
    });
    // Asking after the server's death neither hangs nor crashes.
    let _ = rig.backend.realtime_status().report();
    let closed = close_within_ten_seconds(rig.backend);
    // JACK2 cannot deactivate a client whose server is gone: the processor is kept.
    assert_eq!(closed, Err(CloseError::NotDeactivated));
}

#[test]
fn a_server_stopped_mid_run_is_reported_and_close_returns() {
    let _turn = one_at_a_time();
    let (mut server, rig) = playing();
    server.stop();
    wait_for("the shutdown's report", Duration::from_secs(10), || {
        rig.backend.server_shut_down()
    });
    let periods = rig.controller.stats().periods();
    pause(Duration::from_millis(100));
    assert_eq!(
        rig.controller.stats().periods(),
        periods,
        "no period after the shutdown"
    );
    let closed = close_within_ten_seconds(rig.backend);
    assert_eq!(closed, Err(CloseError::NotDeactivated));
}
