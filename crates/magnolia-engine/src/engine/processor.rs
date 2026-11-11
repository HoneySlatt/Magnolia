//! The processor: the engine's real-time half, which a backend calls once a period.
//!
//! Each period it applies at most one swap the controller sent, then the transport's commands,
//! takes the events due, runs the graph's nodes in sub-blocks, each with its own events and those
//! its timeline sources play, and fills the device's playback buffers. It never allocates, frees,
//! locks or blocks; its one system call wakes the collector, which frees what it lets go.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

use rtrb::Consumer;

use super::arena::Arena;
use super::collector::{Garbage, GarbageOut};
use super::compiled::{Channels, CompiledGraph, Phase, Step, Sum, SwapMessage};
use super::events::EventInbox;
use super::fade::{FadeTable, Gain};
use super::graph::NodeId;
use super::pool::{self, BufferPool};
use super::sources::{NodeTimeline, SourceReader, Timeline};
use super::transport::TransportClock;
use crate::device::DeviceBuffers;
use crate::node::{Event, ProcessContext, ProcessStatus};
use crate::stats::EngineStats;
use crate::time::{SamplePosition, SampleRate};

/// The engine's real-time half, made with its [`Controller`](super::Controller) by
/// [`engine::new`](super::new).
///
/// A backend owns it and calls [`process`](Self::process) once a period, on its audio thread.
/// It is dropped off the real-time thread, when the backend closes; that ends the collector.
pub struct Processor {
    stats: Arc<EngineStats>,
    /// The engine's rate, at which the device must run.
    sample_rate: SampleRate,
    /// The most frames a node is given at once: a period is cut into sub-blocks no longer.
    largest_block: usize,
    capture_channels: usize,
    playback_channels: usize,
    graph: Box<CompiledGraph>,
    /// The phase of `graph` being played, and how many of its samples were played.
    phase: usize,
    phase_elapsed: usize,
    pool: BufferPool,
    arena: Arena,
    fade: FadeTable,
    events: EventInbox,
    sources: SourceReader,
    transport: TransportClock,
    messages: Consumer<Box<SwapMessage>>,
    garbage: GarbageOut,
    /// The first sample of the next period, on the engine's clock.
    position: i64,
    poisoned: bool,
}

/// What became of a step's node in a sub-block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    /// It ran, or slept through the sub-block; its events were delivered.
    Ran,
    /// It returned an error; its outputs were silenced.
    Failed,
    /// Its slot did not hold it; its outputs were silenced and its events lost.
    Missing,
}

/// The parts the engine builds a processor from.
pub(crate) struct ProcessorParts {
    pub(crate) stats: Arc<EngineStats>,
    pub(crate) sample_rate: SampleRate,
    pub(crate) largest_block: usize,
    pub(crate) capture_channels: usize,
    pub(crate) playback_channels: usize,
    pub(crate) graph: Box<CompiledGraph>,
    pub(crate) pool: BufferPool,
    pub(crate) arena: Arena,
    pub(crate) fade: FadeTable,
    pub(crate) events: EventInbox,
    pub(crate) sources: SourceReader,
    pub(crate) transport: TransportClock,
    pub(crate) messages: Consumer<Box<SwapMessage>>,
    pub(crate) garbage: GarbageOut,
}

impl Processor {
    pub(crate) fn new(parts: ProcessorParts) -> Self {
        Self {
            stats: parts.stats,
            sample_rate: parts.sample_rate,
            largest_block: parts.largest_block,
            capture_channels: parts.capture_channels,
            playback_channels: parts.playback_channels,
            graph: parts.graph,
            phase: 0,
            phase_elapsed: 0,
            pool: parts.pool,
            arena: parts.arena,
            fade: parts.fade,
            events: parts.events,
            sources: parts.sources,
            transport: parts.transport,
            messages: parts.messages,
            garbage: parts.garbage,
            position: 0,
            poisoned: false,
        }
    }

    /// The engine's statistics, which the backend also fills.
    pub fn stats(&self) -> &Arc<EngineStats> {
        &self.stats
    }

    /// The engine's rate: a backend runs the device at it, or refuses to play.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// The engine's capture channels: a backend gives it as many device inputs.
    pub fn capture_channels(&self) -> usize {
        self.capture_channels
    }

    /// The engine's playback channels: a backend gives it as many device outputs.
    pub fn playback_channels(&self) -> usize {
        self.playback_channels
    }

    /// How far ahead of the output a node's input runs in the phase being played, in samples, as
    /// its step records it; `None` for a node the phase does not run. For tests: the transport
    /// reads the steps themselves (ADR-0025).
    #[doc(hidden)]
    pub fn ahead_in(&self, node: NodeId) -> Option<u64> {
        let phase = self.graph.phase(self.phase)?;
        let step = phase.steps.iter().find(|step| step.node == node)?;
        Some(step.ahead_in)
    }

    /// The largest compensation of the graph being played, in samples: the largest `ahead_in` of
    /// its steady phase, by which the transport pre-rolls (ADR-0025). For tests.
    #[doc(hidden)]
    pub fn largest_ahead_in(&self) -> u64 {
        self.graph.largest_ahead_in
    }

    /// Processes one period: reads the device's capture buffers, fills its playback buffers.
    ///
    /// Real-time: it never allocates, frees, locks or blocks, and its only system call wakes the
    /// collector. A panic inside poisons the processor: this period and every later one write
    /// silence to every playback channel and return normally
    /// ([ADR-0022](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0022-a-panic-on-the-real-time-path-gives-silence.md)).
    /// Every period, poisoned or not, counts in the heartbeat and the histogram of callback times.
    #[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
    pub fn process(&mut self, device: &mut dyn DeviceBuffers) {
        let entry = Instant::now();
        let frames = device.frames();
        self.stats
            .publish_block_start(SamplePosition(self.position));
        if !self.poisoned {
            // After a panic the processor touches its graph, pool and arena no more, so a state
            // left half-changed by the unwinding is never read.
            let period = catch_unwind(AssertUnwindSafe(|| self.run_period(device)));
            if let Err(payload) = period {
                self.poisoned = true;
                self.stats.set_poisoned();
                self.garbage.release(Garbage::PanicPayload(payload));
            }
        }
        if self.poisoned {
            write_silence(device);
        }
        self.garbage.wake_if_pushed();
        self.position = self.position.saturating_add(as_samples(frames));
        self.stats.record_period();
        self.stats.callback_times.record(entry.elapsed());
    }

    fn run_period(&mut self, device: &mut dyn DeviceBuffers) {
        let frames = device.frames();
        self.garbage.flush_overflow();
        self.apply_swap();
        // After the swap, so that a play pre-rolls by the compensation of the graph about to play.
        self.transport
            .apply_commands(self.position, self.graph.largest_ahead_in, &self.stats);
        // Every queued event may be taken, even while a swap waits: the controller sends an event
        // for a node a swap brings only once the processor has applied that swap.
        self.events.pop();
        let mut done = 0;
        while done < frames {
            let length = self.sub_block_length(frames.saturating_sub(done));
            let start = self.position.saturating_add(as_samples(done));
            self.copy_capture_in(device, frames, done, length);
            self.process_sub_block(start, length);
            self.copy_playback_out(device, frames, done, length);
            done = done.saturating_add(length);
            self.advance_phase(length);
            self.transport.advance(length);
        }
    }

    /// Applies the swap the controller queued, if any, when the collector's queue has room for
    /// everything it replaces; otherwise the swap stays queued for a later period.
    fn apply_swap(&mut self) {
        if self.messages.peek().is_err() || !self.garbage.can_take_swap() {
            return;
        }
        let Ok(mut message) = self.messages.pop() else {
            return;
        };
        for (index, chunk) in &mut message.chunks {
            self.arena.install_chunk(*index, chunk);
        }
        for (id, node) in &mut message.nodes {
            self.arena.install_node(*id, node);
        }
        if let Some(pool) = message.pool.take() {
            let old = std::mem::replace(&mut self.pool, pool);
            self.garbage.release(Garbage::Pool(old));
        }
        // The old pool, the used message and the old graph are the three items `can_take_swap`
        // made room for. A graph steady at once releases its dropped nodes after them, so they may
        // wait in an overflow slot.
        let new_graph = message.graph.take();
        self.garbage.release(Garbage::Message(message));
        if let Some(graph) = new_graph {
            let old = std::mem::replace(&mut self.graph, graph);
            self.garbage.release(Garbage::Graph(old));
            // Before the first period nothing has sounded, not even silence, so there is nothing to
            // fade from: the graph starts at its steady phase. Once periods have played, a swap
            // fades, the first commit's included (ADR-0024).
            self.phase = if self.position == 0 {
                self.graph.steady_index()
            } else {
                0
            };
            self.phase_elapsed = 0;
            self.stats.publish_swap_applied(self.graph.sequence);
            if self.graph.is_steady(self.phase) {
                self.reach_steady();
            }
        }
    }

    /// The next sub-block's length: what is left of the period, cut at the largest block, at the
    /// end of the current phase, and where the transport stops.
    fn sub_block_length(&self, remaining: usize) -> usize {
        let mut length = remaining.min(self.largest_block);
        if let Some(phase_length) = self.graph.phase(self.phase).and_then(|phase| phase.length) {
            length = length.min(phase_length.saturating_sub(self.phase_elapsed));
        }
        if let Some(until_stop) = self.transport.frames_until_stop() {
            length = length.min(until_stop);
        }
        // Never zero, so that the period always moves on.
        length.max(1)
    }

    fn advance_phase(&mut self, length: usize) {
        let Some(phase_length) = self.graph.phase(self.phase).and_then(|phase| phase.length) else {
            return;
        };
        self.phase_elapsed = self.phase_elapsed.saturating_add(length);
        if self.phase_elapsed >= phase_length {
            self.phase = self.phase.saturating_add(1);
            self.phase_elapsed = 0;
            if self.graph.is_steady(self.phase) {
                self.reach_steady();
            }
        }
    }

    /// The graph has reached its steady phase: moves the nodes it dropped out of the arena, sends
    /// them to the collector, and tells the controller, which may then reuse their slots.
    fn reach_steady(&mut self) {
        let Self {
            graph,
            arena,
            garbage,
            stats,
            ..
        } = self;
        for &node in &graph.dropped {
            // Reserved for every dropped node: the push never allocates.
            if graph.dropped_nodes.spare_capacity_mut().is_empty() {
                break;
            }
            if let Some(node) = arena.take_node(node) {
                graph.dropped_nodes.push(node);
            }
        }
        let nodes = std::mem::take(&mut graph.dropped_nodes);
        if nodes.capacity() > 0 {
            garbage.release(Garbage::Nodes(nodes));
        }
        stats.publish_swap_steady(graph.sequence);
    }

    fn copy_capture_in(
        &mut self,
        device: &dyn DeviceBuffers,
        frames: usize,
        done: usize,
        length: usize,
    ) {
        let BufferPool {
            stride,
            outputs,
            output_silence,
            ..
        } = &mut self.pool;
        for channel in 0..self.capture_channels {
            let target = pool::channel_mut(outputs, *stride, channel, length);
            let source = device.capture(channel);
            // A buffer shorter than the period is silence (ADR-0022).
            let stretch = source
                .get(done..)
                .and_then(|source| source.get(..length))
                .filter(|_| source.len() >= frames);
            match stretch {
                Some(source) => {
                    for (target, source) in target.iter_mut().zip(source) {
                        *target = *source;
                    }
                }
                None => target.fill(0.0),
            }
            if let Some(flag) = output_silence.get_mut(channel) {
                *flag = stretch.is_none();
            }
        }
    }

    fn process_sub_block(&mut self, start: i64, length: usize) {
        let Self {
            sample_rate,
            graph,
            phase,
            phase_elapsed,
            pool,
            arena,
            fade,
            events,
            sources,
            transport,
            stats,
            ..
        } = self;
        let Some(phase) = graph.phase(*phase) else {
            return;
        };
        let taken = events.take_due(start.saturating_add(as_samples(length)), phase);
        stats.count_events_taken(taken as u64);
        let timeline = Timeline {
            tempo_map: graph.tempo_map.as_deref(),
            rate: *sample_rate,
        };
        let mut delivered: usize = 0;
        let mut errors: u64 = 0;
        let mut mismatches: u64 = 0;
        for step in &phase.steps {
            for sum in phase.sums(step.sums) {
                sum_into(pool, phase, sum, fade, *phase_elapsed, length);
            }
            let node_events = events.events_of(step.node, start);
            let event_count = node_events.len();
            let node_timeline = NodeTimeline {
                timeline,
                sequences: graph.sequences(step.sources),
                transport: transport.at(step.ahead_in),
                play: transport.play(),
            };
            let step_events = StepEvents {
                controller: node_events,
                timeline: node_timeline,
                sources,
            };
            match run_step(step, pool, arena, step_events, length) {
                StepOutcome::Ran => delivered = delivered.saturating_add(event_count),
                StepOutcome::Failed => {
                    delivered = delivered.saturating_add(event_count);
                    errors = errors.saturating_add(1);
                }
                StepOutcome::Missing => mismatches = mismatches.saturating_add(1),
            }
        }
        for sum in phase.sums(phase.playback_sums) {
            sum_into(pool, phase, sum, fade, *phase_elapsed, length);
        }
        let dropped = taken.saturating_sub(delivered);
        stats.count_events_dropped((dropped as u64).saturating_add(sources.take_dropped()));
        stats.count_node_errors(errors);
        stats.count_generation_mismatches(mismatches);
    }

    fn copy_playback_out(
        &self,
        device: &mut dyn DeviceBuffers,
        frames: usize,
        done: usize,
        length: usize,
    ) {
        let mut non_finite: u64 = 0;
        for channel in 0..self.playback_channels {
            let source = pool::channel(&self.pool.inputs, self.pool.stride, channel, length);
            let target = device.playback(channel);
            // A buffer shorter than the period is left as it is (ADR-0022).
            if target.len() < frames {
                continue;
            }
            let Some(target) = target
                .get_mut(done..)
                .and_then(|target| target.get_mut(..length))
            else {
                continue;
            };
            for (target, &sample) in target.iter_mut().zip(source) {
                if sample.is_finite() {
                    // Adding +0.0 turns a -0.0 into +0.0 and leaves every other sample as it is,
                    // so that silence reaches the device with the same bits whatever made it.
                    *target = sample + 0.0;
                } else {
                    *target = 0.0;
                    non_finite = non_finite.saturating_add(1);
                }
            }
        }
        self.stats.count_non_finite_samples(non_finite);
    }
}

/// Sums a channel's edges, each with its gain, into its buffer. With no edge sounding, the
/// channel holds zeros and is flagged silent.
fn sum_into(
    pool: &mut BufferPool,
    phase: &Phase,
    sum: &Sum,
    fade: &FadeTable,
    elapsed: usize,
    length: usize,
) {
    let BufferPool {
        stride,
        inputs,
        input_silence,
        outputs,
        output_silence,
    } = pool;
    let target = pool::channel_mut(inputs, *stride, sum.target, length);
    let mut sounding = false;
    for term in phase.terms(sum) {
        if output_silence.get(term.source).copied().unwrap_or(true) {
            continue;
        }
        let gain = fade.gain(term.gain, elapsed, length);
        if matches!(gain, Gain::Zero) {
            continue;
        }
        let source = pool::channel(outputs, *stride, term.source, length);
        mix(target, source, gain, sounding);
        sounding = true;
    }
    if !sounding {
        target.fill(0.0);
    }
    if let Some(flag) = input_silence.get_mut(sum.target) {
        *flag = !sounding;
    }
}

/// Writes `source` times `gain` into `target`, or adds it when `accumulate`.
fn mix(target: &mut [f32], source: &[f32], gain: Gain<'_>, accumulate: bool) {
    let samples = target.iter_mut().zip(source);
    match (gain, accumulate) {
        (Gain::Zero, _) => {}
        (Gain::One, false) => samples.for_each(|(target, source)| *target = *source),
        (Gain::One, true) => samples.for_each(|(target, source)| *target += *source),
        (Gain::FadeIn(gains), false) => samples
            .zip(gains)
            .for_each(|((target, source), gain)| *target = source * gain),
        (Gain::FadeIn(gains), true) => samples
            .zip(gains)
            .for_each(|((target, source), gain)| *target += source * gain),
        (Gain::FadeOut(gains), false) => samples
            .zip(gains)
            .for_each(|((target, source), gain)| *target = source * (1.0 - gain)),
        (Gain::FadeOut(gains), true) => samples
            .zip(gains)
            .for_each(|((target, source), gain)| *target += source * (1.0 - gain)),
    }
}

/// What a step's node may receive in a sub-block: the controller's events for it, and its timeline
/// sources, which the reader turns into events.
struct StepEvents<'a> {
    controller: &'a [Event],
    timeline: NodeTimeline<'a>,
    sources: &'a mut SourceReader,
}

/// Runs one step's node over its channels, unless it sleeps through the sub-block, and acts on
/// what it returns. A sleeping node with an event, from the controller or its timeline sources,
/// runs.
fn run_step(
    step: &Step,
    pool: &mut BufferPool,
    arena: &mut Arena,
    events: StepEvents<'_>,
    length: usize,
) -> StepOutcome {
    let Some(occupied) = arena.occupied_mut(step.node) else {
        silence(pool, step.outputs, length);
        return StepOutcome::Missing;
    };
    let StepEvents {
        controller,
        timeline,
        sources,
    } = events;
    let transport = timeline.transport;
    let events = sources.node_events(controller, timeline, occupied.sources, length);
    let BufferPool {
        stride,
        inputs,
        input_silence,
        outputs,
        output_silence,
    } = &mut *pool;
    let input_flags = pool::flags(input_silence, step.inputs);
    let inputs_silent = input_flags.iter().all(|&silent| silent);
    if *occupied.sleeping && inputs_silent && events.is_empty() {
        silence(pool, step.outputs, length);
        return StepOutcome::Ran;
    }
    let output_flags = pool::flags_mut(output_silence, step.outputs);
    output_flags.fill(false);
    let mut context = ProcessContext::new(
        length,
        *stride,
        pool::channels(inputs, *stride, step.inputs),
        input_flags,
        pool::channels_mut(outputs, *stride, step.outputs),
        output_flags,
        events,
    )
    .with_transport(transport);
    let status = occupied.node.process(&mut context);
    *occupied.sleeping = status == ProcessStatus::Sleep;
    if status == ProcessStatus::Error {
        silence(pool, step.outputs, length);
        return StepOutcome::Failed;
    }
    StepOutcome::Ran
}

/// Writes zeros to a node's output channels and flags them silent.
fn silence(pool: &mut BufferPool, channels: Channels, length: usize) {
    let stride = pool.stride;
    let samples = pool::channels_mut(&mut pool.outputs, stride, channels);
    for channel in samples.chunks_mut(stride.max(1)) {
        channel.get_mut(..length).unwrap_or_default().fill(0.0);
    }
    pool::flags_mut(&mut pool.output_silence, channels).fill(true);
}

/// Writes silence to every playback channel of the device.
fn write_silence(device: &mut dyn DeviceBuffers) {
    for channel in 0..device.playback_channels() {
        device.playback(channel).fill(0.0);
    }
}

/// A count of frames as a distance on the timeline. A period is far shorter than `i64::MAX`.
fn as_samples(frames: usize) -> i64 {
    i64::try_from(frames).unwrap_or(i64::MAX)
}
