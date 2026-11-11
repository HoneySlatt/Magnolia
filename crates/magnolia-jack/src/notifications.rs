//! What JACK tells the backend outside its process callback, and what the backend's three parts
//! share: the handle on the caller's thread, the notification handler, and the process callback.
//!
//! JACK calls the notification handler on threads of its own: JACK2 on its notification thread,
//! PipeWire on its real-time thread for an xrun, and the thread-init callback on the audio thread
//! itself. PipeWire also skips a process cycle while a handler runs. So every handler here touches
//! atomics only: no lock, no allocation, no wait.

// Its handlers and the thread-init callback run on JACK's threads, some of them real-time, every
// period or at its start: no panicking code (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use jack::{Client, ClientStatus, Control, Frames, NotificationHandler};

/// What the handle, the notification handler and the process callback share: atomics only.
#[derive(Debug)]
pub(crate) struct Shared {
    /// The kernel's id of the thread that runs the process callback; 0 until its first period.
    audio_thread_id: AtomicI32,
    /// How many xruns the server reported since the client was activated.
    server_xruns: AtomicU64,
    /// Whether the server shut the client down.
    shut_down: AtomicBool,
    /// The rate the server last reported.
    server_rate: AtomicU32,
    /// Whether the backend's own callback code panicked, outside the processor.
    callback_panicked: AtomicBool,
}

impl Shared {
    /// The state of a client whose server runs at `server_rate`.
    pub(crate) fn new(server_rate: u32) -> Self {
        Self {
            audio_thread_id: AtomicI32::new(0),
            server_xruns: AtomicU64::new(0),
            shut_down: AtomicBool::new(false),
            server_rate: AtomicU32::new(server_rate),
            callback_panicked: AtomicBool::new(false),
        }
    }

    pub(crate) fn audio_thread_id(&self) -> i32 {
        self.audio_thread_id.load(Ordering::Relaxed)
    }

    pub(crate) fn set_audio_thread_id(&self, id: i32) {
        self.audio_thread_id.store(id, Ordering::Relaxed);
    }

    pub(crate) fn server_xruns(&self) -> u64 {
        self.server_xruns.load(Ordering::Relaxed)
    }

    pub(crate) fn shut_down(&self) -> bool {
        self.shut_down.load(Ordering::Acquire)
    }

    pub(crate) fn server_rate(&self) -> u32 {
        self.server_rate.load(Ordering::Relaxed)
    }

    pub(crate) fn callback_panicked(&self) -> bool {
        self.callback_panicked.load(Ordering::Relaxed)
    }

    pub(crate) fn set_callback_panicked(&self) {
        self.callback_panicked.store(true, Ordering::Relaxed);
    }
}

/// The backend's notification handler.
pub(crate) struct Notifications {
    shared: Arc<Shared>,
}

impl Notifications {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl NotificationHandler for Notifications {
    /// JACK calls it once on each thread it starts for the client: on the audio thread before its
    /// first period, and, with JACK2, on the notification thread too. So the id goes into the
    /// thread's own storage, where only the audio thread's process callback reads it back
    /// ([`thread_id_stored_by_thread_init`]).
    fn thread_init(&self, _: &Client) {
        THREAD_ID.set(read_audio_thread_id());
    }

    /// Async-signal-safe: one atomic store.
    unsafe fn shutdown(&mut self, _: ClientStatus, _: &str) {
        self.shared.shut_down.store(true, Ordering::Release);
    }

    /// While the rate differs from the engine's, the process callback writes silence.
    fn sample_rate(&mut self, _: &Client, rate: Frames) -> Control {
        self.shared.server_rate.store(rate, Ordering::Relaxed);
        Control::Continue
    }

    /// Any xrun of the server's graph, whoever caused it. PipeWire reports several between two
    /// cycles as one, and never the first.
    fn xrun(&mut self, _: &Client) -> Control {
        self.shared.server_xruns.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

thread_local! {
    /// The kernel's id of this thread, as the thread-init callback read it; 0 on a thread where it
    /// did not run. A constant initializer without a destructor: reading it is a plain load, with
    /// no lazy registration, so the process callback may read it.
    static THREAD_ID: Cell<i32> = const { Cell::new(0) };
}

/// The kernel's id of the calling thread, if the thread-init callback ran on it; 0 otherwise.
/// Real-time safe: a load from the thread's own storage.
pub(crate) fn thread_id_stored_by_thread_init() -> i32 {
    THREAD_ID.get()
}

/// Reads the kernel's id of the calling thread, the audio thread's when the thread-init callback
/// calls it: `gettid`, the one system call the backend makes on the audio thread, once, before its
/// first period, and outside the process callback
/// ([ADR-0023](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0023-the-real-time-allowlist.md)).
/// Never inlined, so that it shows on a call stack.
#[inline(never)]
fn read_audio_thread_id() -> i32 {
    // SAFETY: `gettid` takes no argument, touches no memory of the caller's and cannot fail.
    unsafe { libc::gettid() }
}
