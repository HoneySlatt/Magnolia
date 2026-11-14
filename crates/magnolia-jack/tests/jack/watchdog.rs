//! The JACK tests' watchdog (`tests/common/mod.rs`): a test stuck in a JACK call is reported and
//! its server stopped, which answers the call; a test stuck in its own process is aborted. Each
//! stuck test runs in a child process, this test executable started again, with the watchdog's
//! limits shortened through `MAGNOLIA_JACK_TEST_WATCHDOG` and its core dump turned off. Without
//! that variable, as in the tests' own run, the stuck tests return at once.

use std::env;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output, Stdio};
use std::thread;

use jack::{Client, ClientOptions};

use crate::common::{Server, WATCHDOG_LIMITS, client_name, one_at_a_time, step};

/// A test stuck in a JACK call: its server, stopped by the kernel (SIGSTOP), answers nothing, so
/// a request waits for ever, until the watchdog kills the server, which SIGTERM cannot end while
/// it is stopped.
#[test]
fn stuck_on_its_server() {
    if env::var_os(WATCHDOG_LIMITS).is_none() {
        return;
    }
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let (client, _) = Client::new_with_server_name(
        &client_name("stuck"),
        &server.name,
        ClientOptions::NO_START_SERVER,
    )
    .expect("a client opens on the test's server");
    let pid = pid_of_server(&server.name);
    // SAFETY: `kill` sends a signal by pid and touches no memory; the pid is the server's this
    // test started, which runs until its guard stops it at the end of the test.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    step("connecting ports on a server that answers nothing");
    let connected = client.connect_ports_by_name("system:capture_1", "system:playback_1");
    println!("the request returned {connected:?}");
    step("closing the client on a server gone");
    drop(client);
}

/// A test stuck in its own process, which no server's end can answer.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test's thread waits for ever on purpose; it never runs on the real-time path"
)]
fn stuck_in_its_own_process() {
    if env::var_os(WATCHDOG_LIMITS).is_none() {
        return;
    }
    let _turn = one_at_a_time();
    step("waiting for what never comes");
    loop {
        thread::park();
    }
}

#[test]
fn a_test_stuck_on_its_server_is_reported_and_its_server_stopped() {
    let _turn = one_at_a_time();
    step("running a test stuck on its server, in a child process");
    let output = run_stuck_test("watchdog::stuck_on_its_server", "1,2");
    let (stdout, stderr) = texts(&output);
    assert!(
        output.status.success(),
        "the stuck test ended and passed: {}\n{stdout}\n{stderr}",
        output.status
    );
    assert!(stderr.contains("`watchdog::stuck_on_its_server` marked no step for"));
    assert!(
        stderr.contains("its last: connecting ports on a server that answers nothing"),
        "{stderr}"
    );
    assert!(stderr.contains("its servers are stopped now"), "{stderr}");
    assert!(stdout.contains("the request returned Err"), "{stdout}");
    assert!(
        !stderr.contains("stuck in its own process"),
        "the server's end answered the call: {stderr}"
    );
}

#[test]
fn a_test_stuck_in_its_own_process_is_aborted() {
    let _turn = one_at_a_time();
    step("running a test stuck in its own process, in a child process");
    let output = run_stuck_test("watchdog::stuck_in_its_own_process", "1,1");
    let (stdout, stderr) = texts(&output);
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGABRT),
        "{}\n{stdout}\n{stderr}",
        output.status
    );
    assert!(
        stderr.contains("its last: waiting for what never comes"),
        "{stderr}"
    );
    assert!(
        stderr.contains("it is stuck in its own process, which is aborted"),
        "{stderr}"
    );
}

/// Runs this executable's test `name` alone, with the watchdog's limits `limits`, and without a
/// core dump, since an abort is expected; returns what it did.
fn run_stuck_test(name: &str, limits: &str) -> Output {
    let child = Command::new(env::current_exe().expect("the test's executable has a path"))
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(WATCHDOG_LIMITS, limits)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the test's executable starts again");
    let no_core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let pid = libc::pid_t::try_from(child.id()).expect("a process id fits a pid_t");
    // SAFETY: `prlimit` sets a limit of the child just started, by its pid, from a structure that
    // outlives the call, and writes nothing back, given a null pointer. The child has not been
    // waited for, so its pid names it still; its watchdog aborts it a second at the earliest.
    unsafe { libc::prlimit(pid, libc::RLIMIT_CORE, &no_core, std::ptr::null_mut()) };
    child
        .wait_with_output()
        .expect("the stuck test's process is waited for")
}

fn texts(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The pid of the server `name`, found by its command line.
fn pid_of_server(name: &str) -> libc::pid_t {
    let arguments = format!("-n\0{name}\0");
    fs::read_dir("/proc")
        .expect("/proc is readable")
        .flatten()
        .find_map(|entry| {
            let line = fs::read(entry.path().join("cmdline")).ok()?;
            if !String::from_utf8_lossy(&line).contains(&arguments) {
                return None;
            }
            entry.file_name().to_str()?.parse().ok()
        })
        .expect("the server runs")
}
