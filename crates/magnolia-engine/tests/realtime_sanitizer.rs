//! Proves that RealtimeSanitizer stops a real-time function that allocates, on a thread of its
//! own as the audio thread will be, and stays silent on one that only works on memory allocated
//! beforehand. Without this proof, a check of the real-time path that finds nothing would mean
//! nothing.
//!
//! The sanitizer ends the process at its first report, so each case runs in a child process: this
//! same executable, started again with `MAGNOLIA_RTSAN_CASE` naming the case. The parent reads the
//! child's exit status and standard error.
//!
//! Built only with the `realtime-sanitizer` feature, on the nightly toolchain, with
//! `-Zsanitizer=realtime`; scripts/check.sh runs it.

#![feature(sanitize)]

use std::env;
use std::hint::black_box;
use std::process::{Command, ExitCode, Output};
use std::thread;

/// Names the case a child process runs; the parent leaves it unset.
const CASE_VARIABLE: &str = "MAGNOLIA_RTSAN_CASE";

/// The text of the sanitizer's report of a call that a real-time function must not make.
const VIOLATION: &str = "RealtimeSanitizer: unsafe-library-call";

/// Allocates in a real-time context, as a bug on the audio thread would.
#[sanitize(realtime = "nonblocking")]
fn allocate() -> usize {
    let buffer: Vec<f32> = Vec::with_capacity(black_box(64));
    black_box(buffer).capacity()
}

/// Works on memory allocated beforehand, as the audio thread must.
#[sanitize(realtime = "nonblocking")]
fn halve(buffer: &mut [f32]) {
    for sample in buffer.iter_mut() {
        *sample *= 0.5;
    }
}

/// Runs `work` on a new thread and waits for it, as the audio thread will run. Only the functions
/// marked `nonblocking` that `work` calls run in the real-time context; the rest of `work` does not.
fn on_new_thread(work: impl FnOnce() + Send + 'static) {
    thread::spawn(work)
        .join()
        .expect("the test's thread panicked");
}

/// Runs one case in this process, as a child of the parent's.
fn run_case(case: &str) -> ExitCode {
    match case {
        "allocate" => on_new_thread(|| {
            black_box(allocate());
        }),
        "halve" => on_new_thread(|| {
            let mut buffer = vec![1.0; 64];
            halve(&mut buffer);
            assert!(buffer.iter().all(|&sample| sample == 0.5));
        }),
        _ => {
            eprintln!("unknown case {case:?}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Starts this executable again on one case, the sanitizer told to stop at its first report.
fn run_child(case: &str) -> Output {
    let executable = env::current_exe().expect("the test's executable has no path");
    Command::new(executable)
        .env(CASE_VARIABLE, case)
        .env("RTSAN_OPTIONS", "halt_on_error=true")
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
fn check(case: &str, expected: Expected) -> bool {
    let child = run_child(case);
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
                "  expected the sanitizer to stop the child at its call to `malloc`; \
                 was the test built with -Zsanitizer=realtime?"
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
    let results = [
        check("allocate", Expected::Stopped),
        check("halve", Expected::Silent),
    ];
    if results.iter().all(|&passed| passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
