//! `magnolia play` on a JACK2 server of the test's own, on the dummy driver: it plays for the
//! seconds asked, exits with 0 and prints its figures; a server at another rate, or none, is a
//! plain error. None of these runs passes `--connect`.

use std::ffi::OsString;
use std::sync::{MutexGuard, PoisonError};

use magnolia_session::generate::{Kind, generate};

use crate::common::{Server, refuse_pipewire};
use crate::support::{TempDir, magnolia};

/// Takes the lock that runs these tests one at a time: each starts a server of its own, and
/// several at once would load the machine for nothing.
#[expect(
    clippy::disallowed_types,
    reason = "the tests' threads take turns; no real-time code takes this lock"
)]
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

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
