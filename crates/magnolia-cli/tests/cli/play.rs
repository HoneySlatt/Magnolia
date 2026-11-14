//! `magnolia play` on a JACK2 server of the test's own, on the dummy driver: it plays for the
//! seconds asked, exits with 0 and prints its figures, the round trip's latency first, as the small
//! session has an input track; a server at another rate, or none, is a plain error. None of these
//! runs passes `--connect`.

use std::ffi::OsString;

use magnolia_session::generate::{Kind, generate};

use crate::common::{Server, refuse_pipewire};
use crate::support::{TempDir, magnolia, one_at_a_time};

/// Generates the small session into `dir` and returns the arguments that play it on `server`.
fn play_small(dir: &TempDir, server: &str) -> Vec<OsString> {
    generate(Kind::Small, dir.path()).unwrap();
    vec![
        "play".into(),
        dir.path().join("session.json").into(),
        "--server".into(),
        server.into(),
    ]
}

#[test]
fn play_plays_for_the_seconds_asked_and_prints_its_figures() {
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let dir = TempDir::new("play");
    let mut arguments = play_small(&dir, &server.name);
    arguments.extend(["--from", "1.5", "--seconds", "3"].map(OsString::from));
    let ran = magnolia(&arguments);
    assert_eq!(ran.code, Some(0), "{}\n{}", ran.stdout, ran.stderr);
    let stdout = &ran.stdout;
    for expected in [
        "Playing ",
        "256 frames at 48000 Hz, from 1.500 s to 4.500 s.",
        // Ports connected to nothing have no latency; the mic's path runs through its 32 samples,
        // then the room's 100.
        "  capture: 0 frames (0.000 ms)",
        "  playback: 0 frames (0.000 ms)",
        "  round trip: 0 frames (0.000 ms)",
        "  monitoring path: 132 frames (2.750 ms)",
        // The real-time report: on JACK2 without -R, the audio thread does not run in real time.
        "real time",
        "periods: ",
        "callback: median ",
        ", 99.9 % ",
        ", longest ",
        "disk underruns: ",
        "events dropped: 0",
        "node errors: 0",
        "non-finite samples: 0",
        "own overruns: ",
        "server xruns: ",
        "disk: ",
        " refills",
        "low-water mark of \"vocal\"",
        "low-water mark of \"keys\"",
        "gaps in the server's frame time: ",
    ] {
        assert!(
            stdout.contains(expected),
            "{expected:?} is missing from:\n{stdout}"
        );
    }
    // Three seconds at 256 frames are 562 periods, and the play waits for its start and its stop.
    assert!(
        !stdout.contains("extra") && !stdout.contains("Takes"),
        "a play records nothing:\n{stdout}"
    );
    let periods: u64 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("periods: "))
        .and_then(|periods| periods.parse().ok())
        .expect("the periods are printed");
    assert!(periods >= 562, "{periods} periods for three seconds");
}

#[test]
fn a_server_at_another_rate_is_a_plain_error() {
    let _turn = one_at_a_time();
    let server = Server::start_at(44_100, 256);
    let dir = TempDir::new("play-rate");
    let ran = magnolia(play_small(&dir, &server.name));
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(
        ran.stderr.contains(
            "the JACK server runs at 44100 Hz and the session at 48000 Hz: start the server at \
             48000 Hz"
        ),
        "{}",
        ran.stderr
    );
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
}

#[test]
fn no_server_is_a_plain_error() {
    // Under PipeWire's libjack, the client would reach the owner's live PipeWire, whatever the
    // server's name.
    refuse_pipewire();
    let dir = TempDir::new("play-none");
    let absent = format!("magnolia-test-absent-{}", std::process::id());
    let ran = magnolia(play_small(&dir, &absent));
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(
        ran.stderr.contains(&format!(
            "no JACK server is running under the name {absent:?}"
        )),
        "{}",
        ran.stderr
    );
}
