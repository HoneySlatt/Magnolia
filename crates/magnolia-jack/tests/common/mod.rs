//! What the backend's test targets share: a JACK2 server of their own, on the dummy driver,
//! without real time, the refusal to run where a client would reach PipeWire, and the turn that
//! runs their tests one at a time, under a watchdog.
//!
//! A test that loaded PipeWire's libjack would talk to the owner's live PipeWire instead of the
//! test's own server: [`refuse_pipewire`] fails it first, with the reason.
//!
//! libjack never gives up on its server: a call waits for the server's answer for ever, and closing
//! a client joins JACK's threads of that client without a bound. So each test that starts a server
//! holds a [`Turn`], and marks what it does with [`step`]: a test that marks no step for
//! [`STUCK_AFTER`] is reported, with what each of its process's threads and its servers' threads
//! wait on, and its servers are stopped, which answers every JACK call waiting on them. A test that
//! still holds its turn [`GRACE`] later is stuck in its own process: the watchdog removes its
//! servers' files and aborts the process, whose core holds every thread's stack.

use std::env;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use jack::{Client, ClientOptions};

/// Waits a little, on a test's thread.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's thread waits for the JACK server and the audio thread; it never runs on the real-time path"
)]
pub fn pause(duration: Duration) {
    thread::sleep(duration);
}

/// Refuses to go on, with the reason, unless the libjack this process loads is JACK2's: first by
/// the library path, then by the library actually loaded, before any client opens. Loading the
/// library connects to nothing.
pub fn refuse_pipewire() {
    let path = env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
    let first = env::split_paths(&path)
        .map(|directory| directory.join("libjack.so.0"))
        .find(|library| library.exists());
    if let Some(library) = &first {
        let resolved = library.canonicalize().unwrap_or_else(|_| library.clone());
        assert!(
            !names_pipewire(&resolved),
            "refusing to run: LD_LIBRARY_PATH puts PipeWire's libjack first ({}), so the test's \
             clients would reach the owner's live PipeWire; run the tests in the development \
             shell, without pw-jack",
            resolved.display()
        );
    }
    if let Err(error) = jack::jack_sys::library() {
        panic!("libjack could not be loaded ({error}); run the tests in the development shell");
    }
    let maps = fs::read_to_string("/proc/self/maps").expect("/proc/self/maps is readable");
    let loaded: Vec<&str> = maps
        .lines()
        .filter_map(|line| line.find('/').map(|start| &line[start..]))
        .filter(|path| {
            Path::new(path)
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("libjack.so"))
        })
        .collect();
    assert!(!loaded.is_empty(), "no libjack is mapped after loading it");
    for library in loaded {
        assert!(
            !names_pipewire(Path::new(library)),
            "refusing to run: the libjack loaded is PipeWire's ({library}), so the test's clients \
             would reach the owner's live PipeWire"
        );
    }
}

fn names_pipewire(path: &Path) -> bool {
    path.to_string_lossy()
        .to_ascii_lowercase()
        .contains("pipewire")
}

/// A name no other server of this machine holds: the process's id and a count.
pub fn unique_server_name() -> String {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    format!(
        "magnolia-test-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A JACK client's name that no other process of this machine uses: `base`, then the process's id.
/// JACK2 names a client's notification socket in `/dev/shm` after the client alone, whatever its
/// server, so two processes that open clients of one name at once, each on a server of its own,
/// take each other's sockets: an open fails, or a client's notifications reach the other process.
pub fn client_name(base: &str) -> String {
    format!("{base}-{}", std::process::id())
}

/// A JACK2 server of the test's own, on the dummy driver, without real time: `jackd -n <name>
/// -r -d dummy -r 48000 -p <period>`. Dropping it stops it, by its pid, on a panic too. While it
/// runs, the watchdog of the test's [`Turn`] may stop it too.
///
/// A server that dies without stopping cleanly keeps its entry in JACK2's registry of servers,
/// which holds eight, and its shared memory, until a server of the same name starts again: after a
/// kill, the guard starts one under the same name and stops it cleanly. It also removes the socket
/// and the semaphores that the dead server and its clients left in `/dev/shm`, named after it.
pub struct Server {
    pub name: String,
    /// `None` once stopped or killed.
    child: Option<Child>,
    /// Whether it died without stopping cleanly; the watchdog sets it too when it kills the server.
    killed: Arc<AtomicBool>,
}

impl Server {
    /// Starts a server under a new name, after [`refuse_pipewire`], and waits until a client can
    /// open on it.
    pub fn start(period: u32) -> Self {
        Self::start_at(48_000, period)
    }

    /// Starts a server as [`start`](Self::start) does, at `rate` Hz.
    pub fn start_at(rate: u32, period: u32) -> Self {
        Self::start_named(unique_server_name(), rate, period)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn start_named(name: String, rate: u32, period: u32) -> Result<Self, String> {
        refuse_pipewire();
        step(format!("starting jackd {name}"));
        let child = jackd(&name, rate, period).spawn().map_err(|error| {
            format!("jackd did not start ({error}): run the tests in the development shell")
        })?;
        let killed = Arc::new(AtomicBool::new(false));
        watch_server(&name, child.id(), &killed);
        let mut server = Self {
            name,
            child: Some(child),
            killed,
        };
        server.wait_until_serving()?;
        Ok(server)
    }

    fn wait_until_serving(&mut self) -> Result<(), String> {
        let start = Instant::now();
        loop {
            let probe = Client::new_with_server_name(
                &client_name("probe"),
                &self.name,
                ClientOptions::NO_START_SERVER,
            );
            if probe.is_ok() {
                return Ok(());
            }
            if let Some(child) = &mut self.child
                && let Ok(Some(status)) = child.try_wait()
            {
                self.child = None;
                return Err(format!(
                    "jackd {} exited with {status} before it served",
                    self.name
                ));
            }
            if start.elapsed() > Duration::from_secs(10) {
                self.kill();
                return Err(format!("jackd {} did not serve within 10 s", self.name));
            }
            pause(Duration::from_millis(20));
        }
    }

    /// Kills the server with SIGKILL, as a crash would, and waits for it.
    pub fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        unwatch_server(child.id());
        child.kill().ok();
        child.wait().ok();
        self.killed.store(true, Ordering::Release);
    }

    /// Stops the server with SIGTERM, as a user would, and waits for it; kills it if it has not
    /// stopped within ten seconds.
    pub fn stop(&mut self) {
        let Some(child) = &mut self.child else {
            return;
        };
        step(format!("stopping jackd {}", self.name));
        // Before the child is waited for: the watchdog never signals a pid that may be reused.
        unwatch_server(child.id());
        signal(child.id(), libc::SIGTERM);
        let start = Instant::now();
        while child.try_wait().ok().flatten().is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                eprintln!(
                    "jackd {} did not stop within 10 s of SIGTERM; killing it",
                    self.name
                );
                self.kill();
                return;
            }
            pause(Duration::from_millis(5));
        }
        self.child = None;
    }
}

/// `jackd -n <name> -r -d dummy -r <rate> -p <period>`, its output discarded.
fn jackd(name: &str, rate: u32, period: u32) -> Command {
    let mut command = Command::new("jackd");
    command
        .args(["-n", name, "-r", "-d", "dummy", "-r"])
        .arg(rate.to_string())
        .arg("-p")
        .arg(period.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Sends `signal` to the process `pid`, a server this process started and has not waited for:
/// the guard stops watching a server before it waits for it, so its pid cannot have been reused.
fn signal(pid: u32, signal: libc::c_int) {
    let pid = libc::pid_t::try_from(pid).expect("a process id fits a pid_t");
    // SAFETY: `kill` sends a signal by pid and touches no memory; the pid is a child of this
    // process that has not been waited for, so it names that child still.
    unsafe { libc::kill(pid, signal) };
}

/// Whether the process `pid`, a child of this process not yet waited for, ends within `limit`:
/// a zombie then, until its guard waits for it.
fn ends_within(pid: u32, limit: Duration) -> bool {
    let start = Instant::now();
    loop {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // The state follows the process's name, which ends at the last parenthesis.
        if stat
            .rsplit_once(") ")
            .is_none_or(|(_, rest)| rest.starts_with('Z'))
        {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        pause(Duration::from_millis(20));
    }
}

/// Removes the socket and the semaphores named after the server `name` in `/dev/shm`.
fn remove_leftovers(name: &str) {
    let Ok(entries) = fs::read_dir("/dev/shm") else {
        return;
    };
    // SAFETY: `getuid` cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    let socket = format!("jack_{name}_{uid}_");
    let semaphore = format!("jack_sem.{uid}_{name}_");
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.starts_with(&socket) || file.starts_with(&semaphore) {
            fs::remove_file(entry.path()).ok();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        remove_leftovers(&self.name);
        if !self.killed.load(Ordering::Acquire) {
            return;
        }
        // A server of the same name reclaims the dead one's entry and shared memory.
        step(format!(
            "reclaiming the entry of the dead jackd {}",
            self.name
        ));
        match Self::start_named(self.name.clone(), 48_000, 1_024) {
            Ok(mut again) => {
                again.stop();
                remove_leftovers(&again.name);
                // Never reclaimed in turn, so that a server that will not stop cannot recur.
                again.killed.store(false, Ordering::Release);
            }
            Err(error) => {
                eprintln!("the registry entry of a dead jackd was not reclaimed: {error}")
            }
        }
    }
}

/// How long a test holding its turn may go without marking a step before the watchdog reports it
/// and stops its servers. A step's own waits are bounded at 10 s or so, and a whole run of the
/// JACK tests took 9 to 54 s on a loaded machine: a test that moves never reaches it.
const STUCK_AFTER: Duration = Duration::from_secs(120);

/// How long the watchdog gives each of its actions to take effect: a server to end after SIGTERM,
/// before SIGKILL; then the test to move on or give its turn back, its JACK calls answered by its
/// servers' end, before the process is aborted.
const GRACE: Duration = Duration::from_secs(10);

/// Gives the watchdog other limits than [`STUCK_AFTER`] and [`GRACE`]: `<stuck after>,<grace>`, in
/// seconds. For the watchdog's own tests, which set it on the child processes they run.
pub const WATCHDOG_LIMITS: &str = "MAGNOLIA_JACK_TEST_WATCHDOG";

/// A test's turn among the tests of its process that start a JACK server: they run one at a time,
/// each under the watchdog. Dropping it, on a panic too, gives the turn back.
pub struct Turn {
    _lock: MutexGuard<'static, ()>,
}

/// Takes the turn once the test before has given it back, and has the watchdog watch the test:
/// several servers at once would load the machine into xruns that the tests do not provoke.
#[expect(
    clippy::disallowed_types,
    reason = "the tests' threads take turns; no real-time code takes this lock"
)]
pub fn one_at_a_time() -> Turn {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    start_watchdog();
    let (stuck_after, grace) = watchdog_limits();
    *watched() = Some(Watched {
        test: thread::current().name().unwrap_or("a test").to_owned(),
        thread: this_thread_id(),
        step: "taking its turn".to_owned(),
        since: Instant::now(),
        stuck_after,
        grace,
        servers: Vec::new(),
        reported: None,
    });
    Turn { _lock: lock }
}

impl Drop for Turn {
    /// Ends the watch, before the field gives the turn back.
    fn drop(&mut self) {
        *watched() = None;
    }
}

/// The watchdog's limits: [`WATCHDOG_LIMITS`] when set and well formed, [`STUCK_AFTER`] and
/// [`GRACE`] otherwise.
fn watchdog_limits() -> (Duration, Duration) {
    let seconds = |text: &str| Duration::try_from_secs_f64(text.parse().ok()?).ok();
    env::var(WATCHDOG_LIMITS)
        .ok()
        .and_then(|limits| {
            let (stuck_after, grace) = limits.split_once(',')?;
            Some((seconds(stuck_after)?, seconds(grace)?))
        })
        .unwrap_or((STUCK_AFTER, GRACE))
}

/// Marks what the test holding the turn does now, from its own thread: the watchdog reports a
/// test whose last step has lasted [`STUCK_AFTER`]. Ignored on any other thread.
pub fn step(what: impl Into<String>) {
    let mut watched = watched();
    if let Some(watched) = watched
        .as_mut()
        .filter(|watched| watched.thread == this_thread_id())
    {
        watched.step = what.into();
        watched.since = Instant::now();
    }
}

/// The kernel's id of the calling thread.
pub fn this_thread_id() -> i32 {
    // SAFETY: `gettid` takes no argument, touches no memory and cannot fail.
    unsafe { libc::gettid() }
}

/// What the watchdog watches: the test that holds the turn.
struct Watched {
    /// The test, by its thread's name, and that thread's kernel id.
    test: String,
    thread: i32,
    /// Its last step, and when it was marked.
    step: String,
    since: Instant,
    /// The watchdog's limits for it.
    stuck_after: Duration,
    grace: Duration,
    /// The servers it started and has not stopped.
    servers: Vec<WatchedServer>,
    /// When the watchdog reported it, unless it has marked a step since.
    reported: Option<Instant>,
}

/// A server of the test that holds the turn.
struct WatchedServer {
    name: String,
    pid: u32,
    /// Its guard's flag, which the watchdog sets when it kills the server, so that the guard
    /// reclaims the server's registry entry.
    killed: Arc<AtomicBool>,
}

/// The watch, which the tests' threads and the watchdog share.
#[expect(
    clippy::disallowed_types,
    reason = "the tests' threads and their watchdog share the watch; no real-time code takes this lock"
)]
fn watched() -> MutexGuard<'static, Option<Watched>> {
    static WATCHED: std::sync::Mutex<Option<Watched>> = std::sync::Mutex::new(None);
    WATCHED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Has the watchdog watch a server that the test holding the turn started, from its thread.
fn watch_server(name: &str, pid: u32, killed: &Arc<AtomicBool>) {
    let mut watched = watched();
    if let Some(watched) = watched
        .as_mut()
        .filter(|watched| watched.thread == this_thread_id())
    {
        watched.servers.push(WatchedServer {
            name: name.to_owned(),
            pid,
            killed: Arc::clone(killed),
        });
    }
}

/// Stops watching the server `pid`, before its guard signals it and waits for it.
fn unwatch_server(pid: u32) {
    if let Some(watched) = watched().as_mut() {
        watched.servers.retain(|server| server.pid != pid);
    }
}

/// Starts the watchdog's thread, once per process.
fn start_watchdog() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !STARTED.swap(true, Ordering::AcqRel) {
        thread::Builder::new()
            .name("jack-test-watchdog".to_owned())
            .spawn(watch)
            .expect("the JACK tests' watchdog starts");
    }
}

/// The watchdog's thread: looks at the watch ten times a second for the process's life. A test
/// that marks no step for [`STUCK_AFTER`] is reported and its servers stopped; one that neither
/// moves on nor gives its turn back [`GRACE`] later is stuck in its own process, which is aborted.
fn watch() {
    loop {
        pause(Duration::from_millis(100));
        let mut watched = watched();
        let Some(test) = watched.as_mut() else {
            continue;
        };
        match test.reported {
            Some(reported) if test.since > reported => test.reported = None,
            Some(reported) if reported.elapsed() >= test.grace => abort_stuck(test),
            None if test.since.elapsed() >= test.stuck_after => {
                eprintln!("{}", stuck_report(test));
                for server in &test.servers {
                    stop_from_the_watchdog(server, test.grace);
                }
                test.reported = Some(Instant::now());
            }
            _ => {}
        }
    }
}

/// What the watchdog says of a test that marked no step for too long: its last step, and where
/// the kernel says each thread of the process and of its servers waits.
fn stuck_report(test: &Watched) -> String {
    let mut report = format!(
        "the JACK test `{}` marked no step for {:?}; its last: {}. Its threads wait on:\n{}",
        test.test,
        test.since.elapsed(),
        test.step,
        threads_of("self", test.thread)
    );
    for server in &test.servers {
        report.push_str(&format!(
            "the threads of its jackd {}, pid {}, wait on:\n{}",
            server.name,
            server.pid,
            threads_of(&server.pid.to_string(), 0)
        ));
    }
    report.push_str(
        "its servers are stopped now, which answers every JACK call waiting on them: the test \
         fails then, or the process is aborted",
    );
    report
}

/// One line per thread of the process `process`, `self` or a pid: its kernel id, its name, the
/// kernel function it waits in and its system call's number; `test` marks the test's thread.
fn threads_of(process: &str, test: i32) -> String {
    let Ok(tasks) = fs::read_dir(format!("/proc/{process}/task")) else {
        return "  (unreadable)\n".to_owned();
    };
    let mut lines = String::new();
    for task in tasks.flatten() {
        let read = |file: &str| {
            fs::read_to_string(task.path().join(file))
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let id = task.file_name().to_string_lossy().into_owned();
        let mark = if id == test.to_string() {
            ", the test's"
        } else {
            ""
        };
        let syscall = read("syscall");
        lines.push_str(&format!(
            "  thread {id} `{}`{mark}: in {}, system call {}\n",
            read("comm"),
            read("wchan"),
            syscall.split(' ').next().unwrap_or("?")
        ));
    }
    lines
}

/// Stops a server of the watched test from the watchdog: SIGTERM, then SIGKILL if it has not ended
/// within `grace`, which its guard learns through its flag, so that it reclaims the server's entry.
/// Its guard still waits for it.
fn stop_from_the_watchdog(server: &WatchedServer, grace: Duration) {
    signal(server.pid, libc::SIGTERM);
    if !ends_within(server.pid, grace) {
        signal(server.pid, libc::SIGKILL);
        server.killed.store(true, Ordering::Release);
        // A killed process ends at once, unless the kernel holds it in an uninterruptible wait.
        ends_within(server.pid, grace);
    }
}

/// Aborts the process of a test that neither moved on nor gave its turn back after its servers
/// were stopped: it is stuck in its own process. The core the abort leaves holds every thread's
/// stack. The guards of its servers never run: their files are removed here, but a server that had
/// to be killed keeps its entry in JACK2's registry, which needs libjack to free, and the stuck
/// test may hold libjack's locks; the message says how to free it.
fn abort_stuck(test: &Watched) -> ! {
    eprintln!(
        "the JACK test `{}` neither moved on nor gave its turn back {:?} after its servers were \
         stopped: it is stuck in its own process, which is aborted; its core holds every thread's \
         stack (`coredumpctl debug {}`). Its threads wait on:\n{}",
        test.test,
        test.grace,
        std::process::id(),
        threads_of("self", test.thread)
    );
    for server in &test.servers {
        remove_leftovers(&server.name);
        if server.killed.load(Ordering::Acquire) {
            eprintln!(
                "jackd {} was killed: its entry in JACK2's registry stays until a server of its \
                 name starts and stops cleanly (`jackd -n {} -d dummy`, then Ctrl-C)",
                server.name, server.name
            );
        }
    }
    std::process::abort();
}
