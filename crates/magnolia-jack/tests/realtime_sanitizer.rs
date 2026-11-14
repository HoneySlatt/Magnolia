//! Proves that RealtimeSanitizer finds nothing in the JACK backend's process callback while it
//! plays the engine on a JACK2 server: periods, a crossfade, a dip, events, capture, a processor
//! replaced in place, the device's latencies published as connections and the monitoring latency
//! change. The latency callback is not checked: JACK2 runs it on its notification thread, which is
//! not real-time, and its own range calls may sleep there (`src/latency.rs`). A first case proves
//! the sanitizer active in this build: an allocation in a function marked real-time is stopped. Another shows that it does not intercept `gettid`, which
//! the thread-init callback calls once on the audio thread, outside any real-time context: the
//! call needs no suppression.
//!
//! The callback runs on the audio thread JACK starts; the sanitizer checks every call made inside
//! it, libjack's included. The processor's one allowed system call, the collector's wake, is
//! suppressed by name (`rtsan-suppressions.txt`, ADR-0023).
//!
//! The sanitizer ends the process at its first report, so each case runs in a child process: this
//! same executable, started again with `MAGNOLIA_RTSAN_CASE` naming the case, and
//! `MAGNOLIA_RTSAN_SERVER` naming the server the parent started. The parent reads the child's exit
//! status and standard error, then stops the server.
//!
//! Meaningful only on the nightly toolchain, built with `-Zsanitizer=realtime --cfg magnolia_rtsan`
//! (ADR-0019); scripts/check.sh runs it so, and `cargo test` leaves it out.

#![cfg_attr(magnolia_rtsan, feature(sanitize))]

mod common;

use std::env;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Output};
use std::time::{Duration, Instant};

use jack::{Client, ClientOptions};
use magnolia_engine::engine::{self, Controller, Destination, EngineConfig, Source, TimedEvent};
use magnolia_engine::node::EventKind;
use magnolia_engine::nodes::{Gain, Sine};
use magnolia_engine::time::{SamplePosition, SampleRate};
use magnolia_jack::{JackBackend, JackOptions};

use common::{Server, pause, refuse_pipewire};

/// Names the case a child process runs; the parent leaves it unset.
const CASE_VARIABLE: &str = "MAGNOLIA_RTSAN_CASE";
/// Names the server a child process plays on.
const SERVER_VARIABLE: &str = "MAGNOLIA_RTSAN_SERVER";

/// The text of the sanitizer's report of a call that a real-time function must not make.
const VIOLATION: &str = "RealtimeSanitizer: unsafe-library-call";

/// Allocates in a real-time context, as a bug on the audio thread would.
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn allocate() -> usize {
    let buffer: Vec<f32> = Vec::with_capacity(black_box(64));
    black_box(buffer).capacity()
}

/// Reads the calling thread's id from a real-time context, as the thread-init callback reads the
/// audio thread's, though outside one.
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn read_thread_id() -> i32 {
    // SAFETY: `gettid` takes no argument, touches no memory and cannot fail.
    unsafe { libc::gettid() }
}

/// Runs `work` on a new thread and waits for it.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's main thread waits for the case's thread; it never runs in a real-time context"
)]
fn on_new_thread(work: impl FnOnce() + Send + 'static) {
    std::thread::spawn(work)
        .join()
        .expect("the test's thread panicked");
}

/// Waits until the processor has run `periods` more periods, up to ten seconds.
fn play(controller: &mut Controller, periods: u64) {
    let stats = std::sync::Arc::clone(controller.stats());
    let target = stats.periods() + periods;
    let start = Instant::now();
    while stats.periods() < target {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the server stopped calling"
        );
        controller.poll();
        pause(Duration::from_millis(1));
    }
}

/// A new engine at 48 kHz, of one capture and two playback channels.
fn new_engine() -> (Controller, engine::Processor) {
    let rate = SampleRate::new(48_000).unwrap();
    engine::new(EngineConfig::new(rate, 256, 1, 2)).unwrap()
}

/// Plays the engine through the backend on `server`: a sine and the capture, events, a
/// crossfade, a dip, then a new processor swapped in place and played. Only the process callback
/// runs in a real-time context; this thread, the controller and the handle allocate as they may.
fn play_the_backend(server: &str) {
    refuse_pipewire();
    let rate = SampleRate::new(48_000).unwrap();
    let (mut controller, processor) = new_engine();
    let a = controller
        .add_node(Box::new(Sine::new(rate, 440.0, 0.1)))
        .unwrap();
    let b = controller
        .add_node(Box::new(Sine::new(rate, 660.0, 0.1)))
        .unwrap();
    let x = controller.add_node(Box::new(Gain::new(1, 0.5))).unwrap();
    let y = controller.add_node(Box::new(Gain::new(1, 0.8))).unwrap();
    let chain = |controller: &mut Controller, nodes: [_; 3], connect: bool| {
        let edges = [
            (Source::Node(nodes[0], 0), Destination::Node(nodes[1], 0)),
            (Source::Node(nodes[1], 0), Destination::Node(nodes[2], 0)),
            (Source::Node(nodes[2], 0), Destination::Playback(0)),
        ];
        for (source, destination) in edges {
            if connect {
                controller.connect(source, destination);
            } else {
                controller.disconnect(source, destination).unwrap();
            }
        }
    };
    chain(&mut controller, [a, x, y], true);
    controller.connect(Source::Capture(0), Destination::Playback(1));
    controller.commit().unwrap();

    let mut backend = JackBackend::open(JackOptions {
        client_name: "magnolia".to_owned(),
        server_name: Some(server.to_owned()),
        processor,
    })
    .expect("the backend opens on the test's server");
    // The dummy driver's capture reaches the engine, its playback takes the engine's.
    let (patchbay, _) =
        Client::new_with_server_name("patchbay", server, ClientOptions::NO_START_SERVER)
            .expect("a second client opens");
    let capture = backend.capture_ports()[0].clone();
    patchbay
        .connect_ports_by_name("system:capture_1", &capture)
        .unwrap();
    let latency = backend
        .wait_for_latency(&[("system:capture_1", &capture)], Duration::from_secs(5))
        .expect("the latency callback ran");
    assert_eq!(latency.capture, 64);
    assert_eq!(backend.connect_to_physical_playback().unwrap(), 2);
    backend
        .set_monitoring_latency(100)
        .expect("the latency callback ran");
    play(&mut controller, 20);
    assert_eq!(
        controller.stats().device_latency(),
        backend.device_latency()
    );

    let start = controller.stats().block_start().0;
    for (index, offset) in [0, 10, 700, 701, 701, 1_500, 4_000].into_iter().enumerate() {
        let value = 0.1 * index as f64;
        for (node, parameter) in [(x, 0), (a, 0)] {
            let value = if node == a {
                200.0 + 50.0 * value
            } else {
                value
            };
            controller
                .send_event(TimedEvent {
                    node,
                    at: SamplePosition(start + offset),
                    kind: EventKind::Parameter {
                        index: parameter,
                        value,
                    },
                })
                .unwrap();
        }
    }
    play(&mut controller, 20);

    // A crossfade: the second sine takes the first one's place.
    controller
        .disconnect(Source::Node(a, 0), Destination::Node(x, 0))
        .unwrap();
    controller.connect(Source::Node(b, 0), Destination::Node(x, 0));
    controller.commit().unwrap();
    play(&mut controller, 20);
    // A dip: the two gains change places.
    chain(&mut controller, [b, x, y], false);
    chain(&mut controller, [b, y, x], true);
    controller.commit().unwrap();
    play(&mut controller, 20);
    let stats = controller.stats();
    assert_eq!(
        stats.last_swap_steady(),
        3,
        "every swap reached its steady phase"
    );
    assert!(stats.audio_thread_id() != 0);
    assert!(!stats.poisoned());

    // A new processor, swapped in place by the callback, its old one sent back to this thread.
    let (mut new_controller, new_processor) = new_engine();
    let sine = new_controller
        .add_node(Box::new(Sine::new(rate, 330.0, 0.1)))
        .unwrap();
    new_controller.connect(Source::Node(sine, 0), Destination::Playback(0));
    new_controller.commit().unwrap();
    assert!(backend.replace_processor(new_processor).is_ok());
    play(&mut new_controller, 20);
    backend.poll();
    assert_eq!(new_controller.stats().last_swap_steady(), 1);
    assert!(new_controller.stats().audio_thread_id() != 0);
    backend.close().expect("the backend closes");
}

/// Runs one case in this process, as a child of the parent's.
fn run_case(case: &str) -> ExitCode {
    match case {
        "allocate" => on_new_thread(|| {
            black_box(allocate());
        }),
        "gettid" => on_new_thread(|| {
            black_box(read_thread_id());
        }),
        "backend" => {
            let server = env::var(SERVER_VARIABLE).expect("the parent names the server");
            play_the_backend(&server);
        }
        _ => {
            eprintln!("unknown case {case:?}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// The sanitizer's options: stop at the first report, symbolize the reports with `addr2line`
/// (ADR-0023) so that the suppressions see function names, and the workspace's suppressions.
fn sanitizer_options() -> String {
    let suppressions = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../rtsan-suppressions.txt")
        .canonicalize()
        .expect("rtsan-suppressions.txt is at the workspace's root");
    format!(
        "halt_on_error=true:allow_addr2line=true:suppressions={}",
        suppressions
            .to_str()
            .expect("the workspace's path is UTF-8")
    )
}

/// Starts this executable again on one case, under the sanitizer's options.
fn run_child(case: &str, server: &str) -> Output {
    let executable = env::current_exe().expect("the test's executable has no path");
    Command::new(executable)
        .env(CASE_VARIABLE, case)
        .env(SERVER_VARIABLE, server)
        .env("RTSAN_OPTIONS", sanitizer_options())
        .output()
        .expect("the child process did not start")
}

/// What the sanitizer must do with a case's child for the case to pass.
#[derive(Clone, Copy)]
enum Expected {
    /// Stop the child at its call to `malloc`, with a report.
    Stopped,
    /// Let the child exit successfully, without a report.
    Silent,
}

/// Runs one case in a child process and checks its outcome; on failure, says why and shows the
/// child's standard error.
fn check(case: &str, expected: Expected, server: &str) -> bool {
    let child = run_child(case, server);
    let stderr = String::from_utf8_lossy(&child.stderr);
    let reported = stderr.contains(VIOLATION);
    let passed = match expected {
        Expected::Stopped => !child.status.success() && reported && stderr.contains("`malloc`"),
        Expected::Silent => child.status.success() && !reported,
    };
    println!("test {case} ... {}", if passed { "ok" } else { "FAILED" });
    if !passed {
        match expected {
            Expected::Stopped => println!(
                "  expected the sanitizer to stop the child at its call to `malloc`; was the \
                 test built with -Zsanitizer=realtime --cfg magnolia_rtsan (scripts/check.sh)?"
            ),
            Expected::Silent => println!("  expected the child to exit successfully, unreported"),
        }
        println!(
            "  the child exited with {}; its standard error:",
            child.status
        );
        println!("{stderr}");
    }
    passed
}

fn main() -> ExitCode {
    if let Some(case) = env::var_os(CASE_VARIABLE) {
        return run_case(&case.to_string_lossy());
    }
    let server = Server::start(64);
    let results = [
        check("allocate", Expected::Stopped, &server.name),
        check("gettid", Expected::Silent, &server.name),
        check("backend", Expected::Silent, &server.name),
    ];
    drop(server);
    if results.iter().all(|&passed| passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
