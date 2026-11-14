//! What the JACK backend's tests share: a JACK2 server of their own on the dummy driver, a plain
//! JACK client that plays a signal and records, an engine on the backend, and nodes that make each
//! behaviour visible.
//!
//! Every test refuses to run when the libjack it would load is PipeWire's
//! ([`refuse_pipewire`](crate::common::refuse_pipewire)).

use std::f64::consts::{PI, TAU};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jack::{
    AsyncClient, AudioIn, AudioOut, Client, ClientOptions, Control, Port, ProcessHandler,
    ProcessScope,
};
use magnolia_engine::engine::{self, Controller, EngineConfig};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::time::SampleRate;
use magnolia_jack::{JackBackend, JackOptions};

use crate::common::{Server, client_name, pause, step};
pub use crate::common::{one_at_a_time, this_thread_id};

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

/// The server's freewheel, turned on and off by a client of its own (`jack_set_freewheel`): the
/// server runs its cycles one after another, as fast as its clients go, with no deadline and
/// without skipping any, for the tests that need JACK's processing path and not its timing. On
/// JACK2's dummy driver it ran 80 to 240 times real time, measured. It disconnects the server's
/// own `system:` ports while it lasts.
///
/// JACK2 1.9.22 stalls for its freewheel driver's timeout, 10 s, when a client closes while it
/// freewheels, measured: so the client that turns freewheel on stays open until it turns it off,
/// and a test opens its other clients before [`start`](Self::start) and closes them after
/// [`stop`](Self::stop). Dropped, it turns freewheel off, on a panic too: declared after the
/// test's other clients, it is dropped before them.
pub struct Freewheel {
    /// `None` once freewheel is off.
    client: Option<Client>,
}

impl Freewheel {
    /// Turns freewheel on, through a client of its own on `server`.
    pub fn start(server: &Server) -> Self {
        step("opening the freewheel's client and turning freewheel on");
        let (client, _) = Client::new_with_server_name(
            &client_name("freewheel"),
            &server.name,
            ClientOptions::NO_START_SERVER,
        )
        .expect("the freewheel's client opens on the test's server");
        assert!(set_freewheel(&client, true), "the server freewheels");
        Self {
            client: Some(client),
        }
    }

    /// Turns freewheel off, then closes its client.
    pub fn stop(mut self) {
        assert!(self.turn_off(), "the server stopped freewheeling");
    }

    /// Turns freewheel off, once; returns whether the server agreed.
    fn turn_off(&mut self) -> bool {
        self.client.take().is_none_or(|client| {
            step("turning freewheel off and closing the freewheel's client");
            set_freewheel(&client, false)
        })
    }
}

impl Drop for Freewheel {
    fn drop(&mut self) {
        if !self.turn_off() {
            eprintln!("the server did not stop freewheeling");
        }
    }
}

/// Asks the server of `client` to freewheel, or to stop; returns whether it agreed.
fn set_freewheel(client: &Client, on: bool) -> bool {
    // SAFETY: the client is open, held by the caller; libjack is loaded since it opened. The call
    // takes no pointer but the client's.
    unsafe { jack::jack_sys::jack_set_freewheel(client.raw(), i32::from(on)) == 0 }
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
    /// First, so that it is dropped first, before the backend closes.
    _marks_the_close: MarksTheClose,
    pub controller: Controller,
    pub backend: JackBackend,
}

impl Rig {
    /// An engine of `capture` and `playback` channels, opened on `server`.
    pub fn open(server: &Server, capture: usize, playback: usize) -> Self {
        step("opening Magnolia's client");
        let (controller, processor) = new_engine(capture, playback);
        let backend = JackBackend::open(JackOptions {
            client_name: client_name("magnolia"),
            server_name: Some(server.name.clone()),
            processor,
        })
        .expect("the backend opens on the test's server");
        Self {
            _marks_the_close: MarksTheClose,
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

/// Marks, when dropped, the close of the backend that a [`Rig`] drops next.
struct MarksTheClose;

impl Drop for MarksTheClose {
    fn drop(&mut self) {
        step("closing Magnolia's client");
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
///
/// In freewheel, the server runs the graph as fast as its clients go, so a test's thread cannot
/// act at a point of the recording by waiting for it: the probe would be far past it. So the probe
/// **holds** once it has recorded as many samples as [`hold_at`](Self::hold_at) said: it waits in
/// its own period, and a freewheeling server, which waits for every client, waits with it, until
/// the test moves the hold further. It holds at most [`HOLD_LIMIT`], under JACK2's freewheel
/// timeout of 10 s, so that a test that fails while it holds never stalls the server.
pub struct Probe {
    client: AsyncClient<(), Recorder>,
    recorded: Arc<AtomicUsize>,
    gaps: Arc<AtomicU64>,
    hold: Arc<Hold>,
    pub output: String,
    pub input: String,
}

/// The longest a probe holds.
const HOLD_LIMIT: Duration = Duration::from_secs(5);

/// Where a probe holds, shared by the test's thread and the probe's.
struct Hold {
    /// The count of samples recorded at which the probe holds; `usize::MAX` for never.
    at: AtomicUsize,
    /// Whether the probe holds now.
    holding: AtomicBool,
    /// Whether a hold outlasted [`HOLD_LIMIT`].
    expired: AtomicBool,
}

struct Recorder {
    output: Port<AudioOut>,
    input: Port<AudioIn>,
    signal: Signal,
    played: u64,
    /// Its capacity is the length to record: it never grows on the audio thread.
    recording: Vec<f32>,
    recorded: Arc<AtomicUsize>,
    gaps: Gaps,
    hold: Arc<Hold>,
}

impl Recorder {
    /// Waits, at the start of a period, while the samples recorded reach the hold, up to
    /// [`HOLD_LIMIT`]. A test's client, never Magnolia's: it may wait.
    fn hold_if_asked(&self) {
        let recorded = self.recording.len();
        if self.hold.at.load(Ordering::Acquire) > recorded {
            return;
        }
        self.hold.holding.store(true, Ordering::Release);
        let start = Instant::now();
        while self.hold.at.load(Ordering::Acquire) <= recorded {
            if start.elapsed() > HOLD_LIMIT {
                self.hold.expired.store(true, Ordering::Release);
                break;
            }
            pause(Duration::from_micros(50));
        }
        self.hold.holding.store(false, Ordering::Release);
    }
}

impl ProcessHandler for Recorder {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        self.hold_if_asked();
        self.gaps.note(scope);
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
        step("opening the probe");
        let (client, _) = Client::new_with_server_name(
            &client_name("probe"),
            &server.name,
            ClientOptions::NO_START_SERVER,
        )
        .expect("the probe opens on the test's server");
        let output = client.register_port("out", AudioOut::default()).unwrap();
        let input = client.register_port("in", AudioIn::default()).unwrap();
        let (output_name, input_name) = (output.name().unwrap(), input.name().unwrap());
        let recorded = Arc::new(AtomicUsize::new(0));
        let gaps = Arc::new(AtomicU64::new(0));
        let hold = Arc::new(Hold {
            at: AtomicUsize::new(usize::MAX),
            holding: AtomicBool::new(false),
            expired: AtomicBool::new(false),
        });
        let recorder = Recorder {
            output,
            input,
            signal,
            played: 0,
            recording: Vec::with_capacity(length),
            recorded: Arc::clone(&recorded),
            gaps: Gaps::new(Arc::clone(&gaps)),
            hold: Arc::clone(&hold),
        };
        let client = client.activate_async((), recorder).unwrap();
        Self {
            client,
            recorded,
            gaps,
            hold,
            output: output_name,
            input: input_name,
        }
    }

    /// Makes the probe hold, at the start of a period, once it has recorded `samples`: a whole
    /// number of periods later, as it records whole periods. Moving the hold past what it has
    /// recorded lets a holding probe go on; `usize::MAX` never holds.
    pub fn hold_at(&self, samples: usize) {
        self.hold.at.store(samples, Ordering::Release);
    }

    /// Waits until the probe holds where [`hold_at`](Self::hold_at) last said, up to ten seconds,
    /// and returns how many samples it has recorded.
    pub fn wait_until_held(&self) -> usize {
        let at = self.hold.at.load(Ordering::Acquire);
        // A probe still in its last hold has recorded less than the new one asks.
        wait_for("the probe's hold", Duration::from_secs(10), || {
            self.hold.holding.load(Ordering::Acquire) && self.recorded() >= at
        });
        self.recorded()
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
        step("closing the probe");
        self.hold_at(usize::MAX);
        let (_, _, recorder) = self.client.deactivate().unwrap();
        assert!(
            !self.hold.expired.load(Ordering::Acquire),
            "the probe held longer than {HOLD_LIMIT:?}"
        );
        recorder.recording
    }
}

/// Counts a plain client's periods that do not follow on from the one before: cycles the server
/// skipped. The first period, and the first after a change of the period's length, are never gaps.
pub struct Gaps {
    /// The first frame and length of the last period.
    last_period: Option<(u32, u32)>,
    count: Arc<AtomicU64>,
}

impl Gaps {
    /// Counts into `count`.
    pub fn new(count: Arc<AtomicU64>) -> Self {
        Self {
            last_period: None,
            count,
        }
    }

    /// Notes the period the client runs now, on its audio thread.
    pub fn note(&mut self, scope: &ProcessScope) {
        let period = (scope.last_frame_time(), scope.n_frames());
        if let Some((start, frames)) = self.last_period
            && frames == period.1
            && period.0 != start.wrapping_add(frames)
        {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
        self.last_period = Some(period);
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

/// Whether a thread of this process has the kernel id `id`.
pub fn is_thread_of_this_process(id: i32) -> bool {
    PathBuf::from(format!("/proc/self/task/{id}")).exists()
}
