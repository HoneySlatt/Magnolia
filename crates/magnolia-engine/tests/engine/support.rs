//! What the engine's tests share: an engine on the device-less backend, the fade's formula
//! written out independently of the engine, and nodes that make each behaviour visible.

use std::f64::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, MutexGuard};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use magnolia_engine::engine::{self, Controller, EngineConfig, NodeId, SwapStatus, TimedEvent};
use magnolia_engine::node::{Event, EventKind, Node, ProcessContext, ProcessStatus};
use magnolia_engine::offline::OfflineBackend;
use magnolia_engine::time::{SamplePosition, SampleRate};

/// The tests' sample rate.
pub const RATE: u32 = 48_000;
/// The engine's largest block in the tests: periods above it are cut into sub-blocks.
pub const LARGEST_BLOCK: usize = 1_024;
/// A fade at 48 kHz: 5 ms.
pub const FADE: usize = 240;

pub fn rate() -> SampleRate {
    SampleRate::new(RATE).unwrap()
}

/// An engine with `playback` playback channels and `capture` capture channels, played by the
/// device-less backend on the test's thread.
pub struct Rig {
    pub controller: Controller,
    pub backend: OfflineBackend,
}

impl Rig {
    pub fn new(capture: usize, playback: usize) -> Self {
        Self::with_config(EngineConfig::new(rate(), LARGEST_BLOCK, capture, playback))
    }

    pub fn with_config(config: EngineConfig) -> Self {
        let (controller, processor) = engine::new(config).unwrap();
        Self {
            controller,
            backend: OfflineBackend::new(processor),
        }
    }

    pub fn add(&mut self, node: impl Node + 'static) -> NodeId {
        self.controller.add_node(Box::new(node)).unwrap()
    }

    /// Plays one period, then polls the controller, as a control thread would between periods;
    /// returns what playback channel 0 played.
    pub fn period(&mut self, frames: usize) -> Vec<f32> {
        let played = self.backend.play_period(frames, &[])[0].clone();
        self.controller.poll();
        played
    }

    /// Plays periods, polling after each, and returns what playback channel 0 played.
    pub fn play(&mut self, periods: &[usize]) -> Vec<f32> {
        periods
            .iter()
            .flat_map(|&frames| self.period(frames))
            .collect()
    }

    /// Plays periods of `frames`, polling after each, until every commit is steady; returns what
    /// playback channel 0 played.
    pub fn play_until_steady(&mut self, frames: usize) -> Vec<f32> {
        let mut played = Vec::new();
        for _ in 0..1_000 {
            let backend_played = self.backend.play_period(frames, &[])[0].clone();
            played.extend(backend_played);
            if self.controller.poll() == SwapStatus::Steady {
                return played;
            }
        }
        panic!("the swaps were not steady after 1,000 periods");
    }

    pub fn send(&mut self, node: NodeId, at: i64, index: u32, value: f64) {
        self.controller
            .send_event(TimedEvent {
                node,
                at: SamplePosition(at),
                kind: EventKind::Parameter { index, value },
            })
            .unwrap();
    }
}

/// The fade-in gain at sample `index` of a fade of `length` samples, from its formula:
/// `0.5 - 0.5 cos(π (index + 0.5) / length)`.
pub fn fade_in(index: usize, length: usize) -> f64 {
    0.5 - 0.5 * (PI * (index as f64 + 0.5) / length as f64).cos()
}

/// The fade-out gain: what the fade-in leaves.
pub fn fade_out(index: usize, length: usize) -> f64 {
    1.0 - fade_in(index, length)
}

/// A sine's sample `sample` after its first, at `frequency` hertz and amplitude `gain`, from its
/// formula.
pub fn sine(sample: usize, frequency: f64, gain: f64) -> f64 {
    let cycles = (sample as f64 * frequency / f64::from(RATE)).fract();
    gain * (TAU * cycles).sin()
}

/// Checks each sample against what it should be, within `tolerance`, and says where it is not.
pub fn assert_matches(played: &[f32], expected: &[f64], tolerance: f64, what: &str) {
    assert_eq!(played.len(), expected.len(), "{what}: lengths");
    for (sample, (&played, &expected)) in played.iter().zip(expected).enumerate() {
        let error = (f64::from(played) - expected).abs();
        assert!(
            error <= tolerance,
            "{what}: sample {sample} is {played}, should be {expected} (error {error:e})"
        );
    }
}

/// The largest step between two consecutive samples.
pub fn largest_step(samples: &[f64]) -> f64 {
    samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f64::max)
}

/// Waits until `condition` holds, up to five seconds.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's thread waits for the collector thread; it never runs on the real-time path"
)]
pub fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "waited 5 s for {what}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Something the test and a node both hold, the node on the processor's or the collector's
/// thread. A lock is fine here: the test's own nodes are never checked for real-time safety, and
/// the sanitizer's test does not use them.
#[expect(
    clippy::disallowed_types,
    reason = "test nodes share what they saw with the test's thread; no real-time code takes this lock"
)]
pub type Shared<T> = Arc<std::sync::Mutex<T>>;

#[expect(
    clippy::disallowed_types,
    reason = "test nodes share what they saw with the test's thread; no real-time code takes this lock"
)]
pub fn shared<T>(value: T) -> Shared<T> {
    Arc::new(std::sync::Mutex::new(value))
}

pub fn read<T>(shared: &Shared<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap()
}

/// One output holding a constant, parameter 0. At zero it writes silence and flags it silent. It
/// ignores every other kind of event.
pub struct Constant {
    value: f32,
}

impl Constant {
    pub fn new(value: f32) -> Self {
        Self { value }
    }
}

impl Node for Constant {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let events = context.events();
        let output = context.output_mut(0);
        let mut start = 0;
        for event in events {
            let end = event.offset as usize;
            output[start..end].fill(self.value);
            if let EventKind::Parameter { value, .. } = event.kind {
                self.value = value as f32;
            }
            start = end;
        }
        output[start..].fill(self.value);
        let silent = output.iter().all(|&sample| sample == 0.0);
        context.set_output_silent(0, silent);
        ProcessStatus::Continue
    }
}

/// What a recorder saw in one call.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub frames: usize,
    pub input_silent: bool,
    /// The parameters' changes: each one's offset and value.
    pub events: Vec<(u32, f64)>,
    /// Every event, of any kind, in the order the node received them.
    pub all_events: Vec<Event>,
}

/// One input passed through to one output, recording each call.
pub struct Recorder {
    calls: Shared<Vec<Call>>,
}

impl Recorder {
    pub fn new() -> (Self, Shared<Vec<Call>>) {
        let calls = shared(Vec::new());
        (
            Self {
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

impl Node for Recorder {
    fn input_channels(&self) -> usize {
        1
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let events = context
            .events()
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::Parameter { value, .. } => Some((event.offset, value)),
                _ => None,
            })
            .collect();
        read(&self.calls).push(Call {
            frames: context.frames(),
            input_silent: context.input_is_silent(0),
            events,
            all_events: context.events().to_vec(),
        });
        let input = context.input(0);
        context.output_mut(0).copy_from_slice(input);
        let silent = context.input_is_silent(0);
        context.set_output_silent(0, silent);
        ProcessStatus::Continue
    }
}

/// Another node, counting its calls.
pub struct Counted<N> {
    node: N,
    calls: Arc<AtomicUsize>,
}

impl<N> Counted<N> {
    pub fn new(node: N) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                node,
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

impl<N: Node> Node for Counted<N> {
    fn input_channels(&self) -> usize {
        self.node.input_channels()
    }

    fn output_channels(&self) -> usize {
        self.node.output_channels()
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.node.process(context)
    }
}

/// One output, filled with ones, and an error each call.
pub struct Failing;

impl Node for Failing {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        context.output_mut(0).fill(1.0);
        ProcessStatus::Error
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

/// One output that writes, by turns, a NaN, an infinity, a negative infinity and a one.
pub struct NonFinite;

impl Node for NonFinite {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let pattern = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0];
        for (sample, value) in context.output_mut(0).iter_mut().zip(pattern.iter().cycle()) {
            *sample = *value;
        }
        ProcessStatus::Continue
    }
}

/// The id and name of the thread that dropped a node, once one has.
pub type DroppedBy = Shared<Option<(ThreadId, Option<String>)>>;

/// A silent node that records which thread dropped it.
pub struct DropWitness {
    dropped_by: DroppedBy,
}

impl DropWitness {
    pub fn new() -> (Self, DroppedBy) {
        let dropped_by = shared(None);
        (
            Self {
                dropped_by: Arc::clone(&dropped_by),
            },
            dropped_by,
        )
    }
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
        let thread = thread::current();
        *read(&self.dropped_by) = Some((thread.id(), thread.name().map(str::to_owned)));
    }
}

/// What a test holds to keep the collector busy: a node whose drop waits until the test opens the
/// gate, so that whatever the processor hands the collector meanwhile stays in its queue.
#[derive(Clone)]
pub struct Gate {
    entered: Arc<AtomicBool>,
    open: Arc<AtomicBool>,
}

impl Gate {
    pub fn new() -> Self {
        Self {
            entered: Arc::new(AtomicBool::new(false)),
            open: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A silent node whose drop waits for this gate.
    pub fn node(&self) -> BlockingDrop {
        BlockingDrop { gate: self.clone() }
    }

    /// Waits until the collector is inside the node's drop.
    pub fn wait_until_entered(&self) {
        wait_for("the collector to drop the blocking node", || {
            self.entered.load(Ordering::Acquire)
        });
    }

    pub fn open(&self) {
        self.open.store(true, Ordering::Release);
    }
}

/// A silent node whose drop blocks the collector until its gate opens, or ten seconds pass.
pub struct BlockingDrop {
    gate: Gate,
}

impl Node for BlockingDrop {
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

impl Drop for BlockingDrop {
    #[expect(
        clippy::disallowed_methods,
        reason = "the collector thread waits for the test's gate; it never runs on the real-time path"
    )]
    fn drop(&mut self) {
        self.gate.entered.store(true, Ordering::Release);
        let start = Instant::now();
        while !self.gate.open.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Two inputs, one output: passes input 0 until an event for parameter 0 switches it, at its
/// sample, to the input the event's value names. A swap made without any fade.
pub struct Switch {
    input: usize,
}

impl Switch {
    pub fn new() -> Self {
        Self { input: 0 }
    }
}

impl Node for Switch {
    fn input_channels(&self) -> usize {
        2
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let events = context.events();
        let inputs = [context.input(0), context.input(1)];
        let output = context.output_mut(0);
        let mut start = 0;
        for event in events {
            let end = event.offset as usize;
            output[start..end].copy_from_slice(&inputs[self.input][start..end]);
            if let EventKind::Parameter { value, .. } = event.kind {
                self.input = if value == 0.0 { 0 } else { 1 };
            }
            start = end;
        }
        output[start..].copy_from_slice(&inputs[self.input][start..]);
        ProcessStatus::Continue
    }
}
