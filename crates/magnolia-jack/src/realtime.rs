//! Whether the audio thread runs in real time, and how to grant it the rights when it does not
//! ([ADR-0007](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0007-the-audio-engine.md),
//! [ADR-0023](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0023-the-real-time-allowlist.md)).
//!
//! Only the kernel knows: rtkit changes a PipeWire thread's policy from outside the process, behind
//! the C library's cached copy, and the server's own answer says whether it runs in real time, not
//! whether the thread got the rights. So the backend asks the kernel for the audio thread's policy,
//! by the thread's id, from any other thread.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// What the backend knows of the audio thread's rights, and of the server that should grant them.
/// [`JackBackend::realtime_status`](crate::JackBackend::realtime_status) makes it;
/// [`report`](Self::report) says it in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeStatus {
    /// How the kernel schedules the audio thread.
    pub thread: ThreadScheduling,
    /// The server behind the loaded libjack.
    pub server: AudioServer,
    /// The path of the loaded libjack, if `/proc/self/maps` shows it.
    pub library: Option<PathBuf>,
    /// On JACK2 alone, whether the server itself runs in real time (`jackd -R`). PipeWire answers
    /// yes whatever its threads' rights, so it is not asked.
    pub server_runs_in_real_time: Option<bool>,
}

/// How the kernel schedules a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadScheduling {
    /// In real time: a period's work preempts every thread of normal priority.
    RealTime {
        /// The real-time policy.
        policy: RealtimePolicy,
        /// The priority within the policy: 1 to 99 for FIFO and round-robin, 0 for deadline.
        priority: i32,
    },
    /// At normal priority, shared with every other program.
    NotRealTime {
        /// The kernel's number for the policy: `SCHED_OTHER`, `SCHED_BATCH` or `SCHED_IDLE`.
        policy: i32,
    },
    /// Not known: the audio thread has not run a period yet, or it has ended.
    NotKnown,
}

/// A real-time scheduling policy of Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimePolicy {
    /// `SCHED_FIFO`, which JACK2 asks for.
    Fifo,
    /// `SCHED_RR`, which rtkit grants PipeWire's threads.
    RoundRobin,
    /// `SCHED_DEADLINE`.
    Deadline,
}

/// The server that serves the JACK API to Magnolia.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioServer {
    /// PipeWire, through its own libjack (`pw-jack`, or a system that installs it as libjack).
    PipeWire,
    /// JACK2's `jackd`, or any libjack that is not PipeWire's.
    Jack2,
}

impl ThreadScheduling {
    /// Asks the kernel how it schedules the thread whose id is `thread_id`, 0 meaning not known.
    /// Callable from any thread; two system calls that never block.
    pub(crate) fn of_thread(thread_id: i32) -> Self {
        if thread_id == 0 {
            return Self::NotKnown;
        }
        // SAFETY: `sched_getscheduler` reads the scheduling policy of a thread of this process by
        // its id and touches no memory of the caller's; an id that no longer exists gives -1.
        let policy = unsafe { libc::sched_getscheduler(thread_id) };
        if policy < 0 {
            return Self::NotKnown;
        }
        let mut parameters = libc::sched_param { sched_priority: 0 };
        // SAFETY: `parameters` is a valid, writable `sched_param` that outlives the call, which
        // writes it and nothing else.
        if unsafe { libc::sched_getparam(thread_id, &mut parameters) } < 0 {
            return Self::NotKnown;
        }
        Self::from_kernel(policy, parameters.sched_priority)
    }

    /// Decodes what `sched_getscheduler` and `sched_getparam` returned. The policy may carry the
    /// reset-on-fork flag, which rtkit sets: it is masked before the policy is read.
    pub(crate) fn from_kernel(policy: i32, priority: i32) -> Self {
        match policy & !libc::SCHED_RESET_ON_FORK {
            libc::SCHED_FIFO => Self::RealTime {
                policy: RealtimePolicy::Fifo,
                priority,
            },
            libc::SCHED_RR => Self::RealTime {
                policy: RealtimePolicy::RoundRobin,
                priority,
            },
            libc::SCHED_DEADLINE => Self::RealTime {
                policy: RealtimePolicy::Deadline,
                priority,
            },
            policy => Self::NotRealTime { policy },
        }
    }
}

impl AudioServer {
    /// The server behind a libjack: PipeWire when its path names PipeWire, as every distribution's
    /// and Nix's do; JACK2 otherwise.
    pub(crate) fn of_library(library: Option<&Path>) -> Self {
        let is_pipewire = library.is_some_and(|path| {
            path.to_string_lossy()
                .to_ascii_lowercase()
                .contains("pipewire")
        });
        if is_pipewire {
            Self::PipeWire
        } else {
            Self::Jack2
        }
    }
}

impl fmt::Display for AudioServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PipeWire => "PipeWire",
            Self::Jack2 => "JACK2",
        })
    }
}

impl fmt::Display for RealtimePolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fifo => "SCHED_FIFO",
            Self::RoundRobin => "SCHED_RR",
            Self::Deadline => "SCHED_DEADLINE",
        })
    }
}

/// The name of a policy that is not real time.
fn other_policy_name(policy: i32) -> String {
    match policy {
        libc::SCHED_OTHER => "SCHED_OTHER".to_owned(),
        libc::SCHED_BATCH => "SCHED_BATCH".to_owned(),
        libc::SCHED_IDLE => "SCHED_IDLE".to_owned(),
        policy => format!("policy {policy}"),
    }
}

/// The path of the libjack this process loaded, read from `/proc/self/maps`.
pub(crate) fn loaded_libjack() -> Option<PathBuf> {
    let maps = fs::read_to_string("/proc/self/maps").ok()?;
    libjack_in_maps(&maps)
}

/// The first mapped file whose name starts with `libjack.so` in the text of a `maps` file, whose
/// lines end with the mapped file's path, the only field that holds a `/`.
fn libjack_in_maps(maps: &str) -> Option<PathBuf> {
    maps.lines()
        .filter_map(|line| line.find('/').and_then(|start| line.get(start..)))
        .map(Path::new)
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("libjack.so"))
        })
        .map(Path::to_path_buf)
}

/// How a user grants a real-time priority limit, which both servers need.
const RTPRIO_LIMIT: &str = "for example, add the user to the `audio` group, give that group \
    `rtprio 95` in a file of `/etc/security/limits.d/` (on NixOS, `security.pam.loginLimits`), \
    then log in again";

impl RealtimeStatus {
    /// The status in plain words, for the user: whether the audio thread runs in real time, under
    /// which server, and, when it does not, how to grant the rights on that server.
    pub fn report(&self) -> String {
        let server = self.server_and_library();
        match self.thread {
            ThreadScheduling::RealTime { policy, priority } => {
                format!(
                    "The audio thread runs in real time ({policy}, priority {priority}), under {server}."
                )
            }
            ThreadScheduling::NotKnown => format!(
                "Whether the audio thread runs in real time is not known yet, under {server}: it \
                 has not run a period, or it has ended. Ask again once it plays."
            ),
            ThreadScheduling::NotRealTime { policy } => format!(
                "The audio thread does not run in real time ({}), under {server}: the sound may \
                 break up whenever the machine is busy. {}",
                other_policy_name(policy),
                self.how_to_grant()
            ),
        }
    }

    /// The server's name, and the library's path when known.
    fn server_and_library(&self) -> String {
        match &self.library {
            Some(library) => format!("{} (libjack: {})", self.server, library.display()),
            None => self.server.to_string(),
        }
    }

    /// How the server in use grants real time to its clients' audio threads.
    fn how_to_grant(&self) -> String {
        match self.server {
            AudioServer::PipeWire => format!(
                "PipeWire raises its clients' audio threads itself, through rtkit or within the \
                 user's real-time priority limit (RLIMIT_RTPRIO). Either run the rtkit daemon \
                 (on NixOS, `security.rtkit.enable`), or give the user that limit: {RTPRIO_LIMIT}. \
                 rtkit grants the rights within about a second of the start: if Magnolia has just \
                 started, ask again."
            ),
            AudioServer::Jack2 => {
                let server = match self.server_runs_in_real_time {
                    Some(false) => "This server runs without it: restart it with `jackd -R`. ",
                    Some(true) => "This server runs in real time, so the user lacks the rights. ",
                    None => "",
                };
                format!(
                    "JACK2 gives its clients' audio threads real time only when the server runs in \
                     real time, and only within the user's real-time priority limit \
                     (RLIMIT_RTPRIO). {server}The user needs that limit: {RTPRIO_LIMIT}."
                )
            }
        }
    }
}

impl fmt::Display for RealtimeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.report())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{AudioServer, RealtimePolicy, RealtimeStatus, ThreadScheduling, libjack_in_maps};

    #[test]
    fn round_robin_with_reset_on_fork_is_real_time() {
        assert_eq!(libc::SCHED_RR | libc::SCHED_RESET_ON_FORK, 0x4000_0002);
        assert_eq!(
            ThreadScheduling::from_kernel(0x4000_0002, 20),
            ThreadScheduling::RealTime {
                policy: RealtimePolicy::RoundRobin,
                priority: 20
            }
        );
        assert_eq!(
            ThreadScheduling::from_kernel(libc::SCHED_FIFO, 70),
            ThreadScheduling::RealTime {
                policy: RealtimePolicy::Fifo,
                priority: 70
            }
        );
    }

    #[test]
    fn normal_policies_are_not_real_time_with_or_without_reset_on_fork() {
        for policy in [libc::SCHED_OTHER, libc::SCHED_BATCH, libc::SCHED_IDLE] {
            for flag in [0, libc::SCHED_RESET_ON_FORK] {
                assert_eq!(
                    ThreadScheduling::from_kernel(policy | flag, 0),
                    ThreadScheduling::NotRealTime { policy }
                );
            }
        }
    }

    #[test]
    fn this_thread_is_known_and_an_unknown_id_is_not() {
        // SAFETY: `gettid` takes no argument and cannot fail.
        let this_thread = unsafe { libc::gettid() };
        assert_ne!(
            ThreadScheduling::of_thread(this_thread),
            ThreadScheduling::NotKnown
        );
        assert_eq!(ThreadScheduling::of_thread(0), ThreadScheduling::NotKnown);
        assert_eq!(
            ThreadScheduling::of_thread(i32::MAX),
            ThreadScheduling::NotKnown
        );
    }

    #[test]
    fn the_server_is_named_by_its_librarys_path() {
        let maps = "\
7f0000000000-7f0000001000 r--p 00000000 00:1f 123 /nix/store/abc-glibc-2.44/lib/libc.so.6
7f0000002000-7f0000003000 r-xp 00001000 00:1f 456 /nix/store/def-pipewire-1.4.2-jack/lib/libjack.so.0.3.1402
7f0000004000-7f0000005000 rw-p 00000000 00:00 0 [heap]
";
        let library = libjack_in_maps(maps);
        assert_eq!(
            library.as_deref(),
            Some(Path::new(
                "/nix/store/def-pipewire-1.4.2-jack/lib/libjack.so.0.3.1402"
            ))
        );
        assert_eq!(
            AudioServer::of_library(library.as_deref()),
            AudioServer::PipeWire
        );
        let debian = PathBuf::from("/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0");
        assert_eq!(
            AudioServer::of_library(Some(&debian)),
            AudioServer::PipeWire
        );
        let jack2 = PathBuf::from("/usr/lib/libjack.so.0.1.0");
        assert_eq!(AudioServer::of_library(Some(&jack2)), AudioServer::Jack2);
        assert_eq!(AudioServer::of_library(None), AudioServer::Jack2);
        let others = "7f00-7f01 r-xp 0 00:1f 9 /usr/lib/libjackserver.so.0\n";
        assert_eq!(libjack_in_maps(others), None);
    }

    #[test]
    fn each_server_says_how_to_grant_the_rights() {
        let status = |server, server_runs_in_real_time| RealtimeStatus {
            thread: ThreadScheduling::NotRealTime {
                policy: libc::SCHED_OTHER,
            },
            server,
            library: None,
            server_runs_in_real_time,
        };
        let pipewire = status(AudioServer::PipeWire, None).report();
        assert!(pipewire.contains("does not run in real time (SCHED_OTHER)"));
        assert!(pipewire.contains("under PipeWire"));
        assert!(pipewire.contains("rtkit") && pipewire.contains("RLIMIT_RTPRIO"));
        let jack2 = status(AudioServer::Jack2, Some(false)).report();
        assert!(jack2.contains("under JACK2"));
        assert!(jack2.contains("jackd -R") && jack2.contains("RLIMIT_RTPRIO"));
        let real_time = RealtimeStatus {
            thread: ThreadScheduling::RealTime {
                policy: RealtimePolicy::RoundRobin,
                priority: 20,
            },
            ..status(AudioServer::PipeWire, None)
        };
        assert_eq!(
            real_time.report(),
            "The audio thread runs in real time (SCHED_RR, priority 20), under PipeWire."
        );
    }
}
