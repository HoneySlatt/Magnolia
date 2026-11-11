//! What the JACK backend's tests share: a JACK2 server of their own on the dummy driver, a plain
//! JACK client that plays a signal and records, an engine on the backend, and nodes that make each
//! behaviour visible.
//!
//! Every test refuses to run when the libjack it would load is PipeWire's
//! ([`refuse_pipewire`](crate::common::refuse_pipewire)).

use std::f64::consts::{PI, TAU};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use jack::{
    AsyncClient, AudioIn, AudioOut, Client, ClientOptions, Control, Port, ProcessHandler,
    ProcessScope,
};
use magnolia_engine::engine::{self, Controller, EngineConfig};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::time::SampleRate;
use magnolia_jack::{JackBackend, JackOptions};

use crate::common::{Server, pause};

/// The tests' sample rate, the dummy driver's and the engine's.
pub const RATE: u32 = 48_000;
/// The engine's largest block in the tests.
pub const LARGEST_BLOCK: usize = 1_024;
/// A fade at 48 kHz: 5 ms.
pub const FADE: usize = 240;

pub fn rate() -> SampleRate {
    SampleRate::new(RATE).unwrap()
}

/// Waits until `condition` holds, up to `timeout`, looking every millisecond.
pub fn wait_for(what: &str, timeout: Duration, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < timeout, "waited {timeout:?} for {what}");
        pause(Duration::from_millis(1));
    }
}

/// Takes the lock that runs the JACK tests one at a time: each starts a server of its own, and
/// several at once would load the machine into xruns that the tests do not provoke themselves.
#[expect(
    clippy::disallowed_types,
    reason = "the tests' threads take turns; no real-time code takes this lock"
)]
pub fn one_at_a_time() -> MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether any process's command line holds `text`: a server started under a test's name.
pub fn a_process_names(text: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        fs::read(entry.path().join("cmdline"))
            .is_ok_and(|line| String::from_utf8_lossy(&line).contains(text))
    })
}

/// An engine played by the JACK backend on a test's server.
pub struct Rig {
    pub controller: Controller,
    pub backend: JackBackend,
}

impl Rig {
    /// An engine of `capture` and `playback` channels, opened on `server`.
    pub fn open(server: &Server, capture: usize, playback: usize) -> Self {
        let (controller, processor) = new_engine(capture, playback);
        let backend = JackBackend::open(JackOptions {
            client_name: "magnolia".to_owned(),
            server_name: Some(server.name.clone()),
            processor,
            sample_rate: rate(),
            capture_channels: capture,
            playback_channels: playback,
        })
        .expect("the backend opens on the test's server");
        Self {
            controller,
            backend,
        }
    }

    pub fn add(&mut self, node: impl Node + 'static) -> magnolia_engine::engine::NodeId {
        self.controller.add_node(Box::new(node)).unwrap()
    }

    /// Waits until every commit is steady.
    pub fn wait_until_steady(&mut self) {
        self.controller
            .wait_until_steady(Duration::from_secs(5))
            .expect("the swaps reached their steady phase");
    }

    /// Waits until the processor has run `periods` more periods.
    pub fn wait_periods(&self, periods: u64) {
        let stats = Arc::clone(self.controller.stats());
        let target = stats.periods() + periods;
        wait_for("periods", Duration::from_secs(10), || {
            stats.periods() >= target
        });
    }
}

/// A new engine at the tests' rate.
pub fn new_engine(capture: usize, playback: usize) -> (Controller, engine::Processor) {
    engine::new(EngineConfig::new(rate(), LARGEST_BLOCK, capture, playback)).unwrap()
}

/// A signal a probe plays: its sample at a count of samples from the probe's first period.
pub type Signal = fn(u64) -> f32;

/// Silence.
pub fn silence(_: u64) -> f32 {
    0.0
}

/// A ramp whose every sample says where it was played: sample `n` holds `n + 1`, exact in an `f32`
/// up to 2^24, 349 s at 48 kHz.
pub fn ramp(sample: u64) -> f32 {
    (sample + 1) as f32
}

/// A plain JACK client, apart from Magnolia: it plays a signal on its output port and records its
/// input port, from its first period, up to a set length.
pub struct Probe {
    client: AsyncClient<(), Recorder>,
    recorded: Arc<AtomicUsize>,
    gaps: Arc<AtomicU64>,
    pub output: String,
    pub input: String,
}

struct Recorder {
    output: Port<AudioOut>,
    input: Port<AudioIn>,
    signal: Signal,
    played: u64,
    /// Its capacity is the length to record: it never grows on the audio thread.
    recording: Vec<f32>,
    recorded: Arc<AtomicUsize>,
    /// The first frame and length of its last period, to count its own gaps.
    last_period: Option<(u32, u32)>,
    gaps: Arc<AtomicU64>,
}

impl ProcessHandler for Recorder {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        let period = (scope.last_frame_time(), scope.n_frames());
        if let Some((start, frames)) = self.last_period
            && frames == period.1
            && period.0 != start.wrapping_add(frames)
        {
            self.gaps.fetch_add(1, Ordering::Relaxed);
        }
        self.last_period = Some(period);
        for sample in self.output.as_mut_slice(scope) {
            *sample = (self.signal)(self.played);
            self.played += 1;
        }
        let room = self.recording.capacity() - self.recording.len();
        let input = self.input.as_slice(scope);
        self.recording
            .extend_from_slice(&input[..input.len().min(room)]);
        self.recorded.store(self.recording.len(), Ordering::Release);
        Control::Continue
    }
}

impl Probe {
    /// Opens a probe on `server` that plays `signal` and records up to `length` samples.
    pub fn open(server: &Server, signal: Signal, length: usize) -> Self {
        let (client, _) =
            Client::new_with_server_name("probe", &server.name, ClientOptions::NO_START_SERVER)
                .expect("the probe opens on the test's server");
        let output = client.register_port("out", AudioOut::default()).unwrap();
        let input = client.register_port("in", AudioIn::default()).unwrap();
        let (output_name, input_name) = (output.name().unwrap(), input.name().unwrap());
        let recorded = Arc::new(AtomicUsize::new(0));
        let gaps = Arc::new(AtomicU64::new(0));
        let recorder = Recorder {
            output,
            input,
            signal,
            played: 0,
            recording: Vec::with_capacity(length),
            recorded: Arc::clone(&recorded),
            last_period: None,
            gaps: Arc::clone(&gaps),
        };
        let client = client.activate_async((), recorder).unwrap();
        Self {
            client,
            recorded,
            gaps,
            output: output_name,
            input: input_name,
        }
    }

    pub fn client(&self) -> &Client {
        self.client.as_client()
    }

    pub fn connect(&self, source: &str, destination: &str) {
        self.client()
            .connect_ports_by_name(source, destination)
            .unwrap_or_else(|error| panic!("{source} -> {destination}: {error}"));
    }

    /// How many samples it has recorded.
    pub fn recorded(&self) -> usize {
        self.recorded.load(Ordering::Acquire)
    }

    /// Waits until it has recorded `length` samples, up to `timeout`.
    pub fn wait_for_samples(&self, length: usize, timeout: Duration) {
        wait_for("the probe's recording", timeout, || {
            self.recorded() >= length
        });
    }

    /// How many of its periods did not follow on from the one before: cycles the server
    /// skipped.
    pub fn gaps(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }

    /// Deactivates the probe and returns what it recorded.
    pub fn finish(self) -> Vec<f32> {
        let (_, _, recorder) = self.client.deactivate().unwrap();
        recorder.recording
    }
}

/// A recording taken through the server, and whether the server broke it.
pub struct Take<T> {
    pub result: T,
    /// Cycles the server skipped while it recorded, Magnolia's or the probe's: gaps in their frame
    /// times.
    pub skipped_cycles: u64,
}

/// How many times a scenario is played, at most, for a take the server did not break.
const TAKES: usize = 5;

/// Plays `scenario`, each time on a server of its own, until a take in which the server skipped
/// no cycle, and returns it. A server without real time on a busy machine may skip a client's
/// cycle; the clients after it then read its last period again, a click that is the server's
/// doing and that the backend counts as a gap. Such a take says nothing of Magnolia; the test
/// fails if every take was broken so.
pub fn first_unbroken_take<T>(scenario: impl Fn() -> Take<T>) -> T {
    let mut skipped = Vec::new();
    for _ in 0..TAKES {
        let take = scenario();
        if take.skipped_cycles == 0 {
            return take.result;
        }
        eprintln!(
            "the server skipped {} cycles in this take; taking another",
            take.skipped_cycles
        );
        skipped.push(take.skipped_cycles);
    }
    panic!("the server skipped cycles in each of {TAKES} takes: {skipped:?}");
}

/// The steepest step a sine of `frequency` hertz and amplitude `gain` makes between two samples.
pub fn steepest(frequency: f64, gain: f64) -> f64 {
    gain * TAU * frequency / f64::from(RATE)
}

/// The largest step a crossfade between two sines can make, as the engine's own swap tests compute
/// it: each sine's steepest step, plus the fade's steepest step, π / 2 per fade's length, across
/// their largest gap.
pub fn crossfade_step_bound(first: (f64, f64), second: (f64, f64)) -> f64 {
    let fade = PI / (2.0 * FADE as f64);
    steepest(first.0, first.1) + steepest(second.0, second.1) + (first.1 + second.1) * fade
}

/// The largest step between two consecutive samples, and where it starts.
pub fn largest_step(samples: &[f32]) -> (f64, usize) {
    samples
        .windows(2)
        .enumerate()
        .map(|(index, pair)| ((f64::from(pair[1]) - f64::from(pair[0])).abs(), index))
        .fold(
            (0.0, 0),
            |largest, step| {
                if step.0 > largest.0 { step } else { largest }
            },
        )
}

/// One output holding a constant.
pub struct Constant(pub f32);

impl Node for Constant {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        context.output_mut(0).fill(self.0);
        ProcessStatus::Continue
    }
}

/// One output of ones that panics once its flag is raised.
pub struct Panicking {
    pub armed: Arc<AtomicBool>,
}

impl Node for Panicking {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        assert!(
            !self.armed.load(Ordering::Relaxed),
            "a test node's panic, on purpose"
        );
        context.output_mut(0).fill(1.0);
        ProcessStatus::Continue
    }
}

/// One silent output that busy-waits `spin` in each call while its count of calls to slow down
/// lasts: a node that misses the period's deadline.
pub struct Slow {
    pub spin: Duration,
    pub slow_calls: Arc<AtomicU32>,
}

impl Node for Slow {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let left = self.slow_calls.load(Ordering::Relaxed);
        if left > 0 {
            self.slow_calls.store(left - 1, Ordering::Relaxed);
            let start = Instant::now();
            while start.elapsed() < self.spin {
                std::hint::spin_loop();
            }
        }
        context.output_mut(0).fill(0.0);
        context.set_output_silent(0, true);
        ProcessStatus::Continue
    }
}

/// A silent node that records the kernel's id of the thread that dropped it.
pub struct DropWitness {
    pub dropped_on: Arc<AtomicI32>,
}

impl Node for DropWitness {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        context.output_mut(0).fill(0.0);
        context.set_output_silent(0, true);
        ProcessStatus::Continue
    }
}

impl Drop for DropWitness {
    fn drop(&mut self) {
        self.dropped_on.store(this_thread_id(), Ordering::Release);
    }
}

/// The kernel's id of the calling thread.
pub fn this_thread_id() -> i32 {
    // SAFETY: `gettid` takes no argument, touches no memory and cannot fail.
    unsafe { libc::gettid() }
}

/// Whether a thread of this process has the kernel id `id`.
pub fn is_thread_of_this_process(id: i32) -> bool {
    PathBuf::from(format!("/proc/self/task/{id}")).exists()
}
