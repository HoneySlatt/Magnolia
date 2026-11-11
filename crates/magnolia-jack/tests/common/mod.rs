//! What the backend's test targets share: a JACK2 server of their own, on the dummy driver,
//! without real time, and the refusal to run where a client would reach PipeWire.
//!
//! A test that loaded PipeWire's libjack would talk to the owner's live PipeWire instead of the
//! test's own server: [`refuse_pipewire`] fails it first, with the reason.

use std::env;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
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

/// A JACK2 server of the test's own, on the dummy driver, without real time: `jackd -n <name>
/// -r -d dummy -r 48000 -p <period>`. Dropping it stops it, by its pid, on a panic too.
///
/// A server that dies without stopping cleanly keeps its entry in JACK2's registry of servers,
/// which holds eight, and its shared memory, until a server of the same name starts again: after a
/// kill, the guard starts one under the same name and stops it cleanly. It also removes the socket
/// and the semaphores that the dead server and its clients left in `/dev/shm`, named after it.
pub struct Server {
    pub name: String,
    /// `None` once stopped or killed.
    child: Option<Child>,
    /// Whether it died without stopping cleanly.
    killed: bool,
}

impl Server {
    /// Starts a server under a new name, after [`refuse_pipewire`], and waits until a client can
    /// open on it.
    pub fn start(period: u32) -> Self {
        Self::start_named(unique_server_name(), period).unwrap_or_else(|error| panic!("{error}"))
    }

    fn start_named(name: String, period: u32) -> Result<Self, String> {
        refuse_pipewire();
        let child = Command::new("jackd")
            .args(["-n", &name, "-r", "-d", "dummy", "-r", "48000", "-p"])
            .arg(period.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                format!("jackd did not start ({error}): run the tests in the development shell")
            })?;
        let mut server = Self {
            name,
            child: Some(child),
            killed: false,
        };
        server.wait_until_serving()?;
        Ok(server)
    }

    fn wait_until_serving(&mut self) -> Result<(), String> {
        let start = Instant::now();
        loop {
            let probe =
                Client::new_with_server_name("probe", &self.name, ClientOptions::NO_START_SERVER);
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
        child.kill().ok();
        child.wait().ok();
        self.killed = true;
    }

    /// Stops the server with SIGTERM, as a user would, and waits for it; kills it if it has not
    /// stopped within ten seconds.
    pub fn stop(&mut self) {
        let Some(child) = &mut self.child else {
            return;
        };
        let pid = libc::pid_t::try_from(child.id()).expect("a process id fits a pid_t");
        // SAFETY: `kill` sends a signal to the process this guard started, by its pid, and
        // touches no memory; the child has not been waited for, so its pid was not reused.
        unsafe { libc::kill(pid, libc::SIGTERM) };
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

    /// Removes the socket and the semaphores named after the server in `/dev/shm`.
    fn remove_leftovers(&self) {
        let Ok(entries) = fs::read_dir("/dev/shm") else {
            return;
        };
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let socket = format!("jack_{}_{uid}_", self.name);
        let semaphore = format!("jack_sem.{uid}_{}_", self.name);
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with(&socket) || file.starts_with(&semaphore) {
                fs::remove_file(entry.path()).ok();
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        self.remove_leftovers();
        if !self.killed {
            return;
        }
        // A server of the same name reclaims the dead one's entry and shared memory.
        match Self::start_named(self.name.clone(), 1_024) {
            Ok(mut again) => {
                again.stop();
                again.remove_leftovers();
                // Never reclaimed in turn, so that a server that will not stop cannot recur.
                again.killed = false;
            }
            Err(error) => {
                eprintln!("the registry entry of a dead jackd was not reclaimed: {error}")
            }
        }
    }
}
