//! What the engine says of itself while it plays: [`EngineStats`], shared by the processor, its
//! backend and the controller, readable from any thread at any time.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};

use crate::callback_times::CallbackTimes;
use crate::time::SamplePosition;

/// The engine's counters, flags and histogram: atomics only, so the real-time thread updates them
/// without a lock and any thread reads them. Each value is read on its own; two reads may straddle
/// a period.
#[derive(Debug, Default)]
pub struct EngineStats {
    /// Each period's wall time, from the processor's entry to its exit.
    pub callback_times: CallbackTimes,
    periods: AtomicU64,
    block_start: AtomicI64,
    last_swap_applied: AtomicU64,
    last_swap_steady: AtomicU64,
    node_errors: AtomicU64,
    events_dropped: AtomicU64,
    non_finite_samples: AtomicU64,
    generation_mismatches: AtomicU64,
    missed_plugin_deadlines: AtomicU64,
    poisoned: AtomicBool,
    own_overruns: AtomicU64,
    frame_time_gaps: AtomicU64,
    server_xruns: AtomicU64,
    audio_thread_id: AtomicI32,
}

impl EngineStats {
    /// How many periods the processor has run: a heartbeat, which stops when the backend stops
    /// calling it.
    pub fn periods(&self) -> u64 {
        self.periods.load(Ordering::Relaxed)
    }

    /// The position of the first sample of the period the processor ran last, so that the
    /// controller can stamp events. Zero before the first period.
    pub fn block_start(&self) -> SamplePosition {
        SamplePosition(self.block_start.load(Ordering::Relaxed))
    }

    /// The sequence number of the last graph swap the processor applied; 0 before any.
    pub fn last_swap_applied(&self) -> u64 {
        self.last_swap_applied.load(Ordering::Acquire)
    }

    /// The sequence number of the last graph swap that reached its steady phase; 0 before any.
    /// The nodes that swap dropped have left the processor's arena.
    pub fn last_swap_steady(&self) -> u64 {
        self.last_swap_steady.load(Ordering::Acquire)
    }

    /// How many times a node returned an error.
    pub fn node_errors(&self) -> u64 {
        self.node_errors.load(Ordering::Relaxed)
    }

    /// How many events were dropped because no node of the current graph took them.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped.load(Ordering::Relaxed)
    }

    /// How many samples bound for the device were not finite, and were replaced by zero.
    pub fn non_finite_samples(&self) -> u64 {
        self.non_finite_samples.load(Ordering::Relaxed)
    }

    /// How many times a step of the graph found its node's slot empty or holding another
    /// generation, and wrote silence in its place.
    pub fn generation_mismatches(&self) -> u64 {
        self.generation_mismatches.load(Ordering::Relaxed)
    }

    /// How many plugin blocks missed their deadline. Always 0 until plugin hosts come (M1.6).
    pub fn missed_plugin_deadlines(&self) -> u64 {
        self.missed_plugin_deadlines.load(Ordering::Relaxed)
    }

    /// Whether a panic poisoned the processor: from then on it writes silence
    /// ([ADR-0022](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0022-a-panic-on-the-real-time-path-gives-silence.md)).
    pub fn poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Relaxed)
    }

    /// How many periods the processor itself finished after the device's deadline, as the backend
    /// counts them.
    pub fn own_overruns(&self) -> u64 {
        self.own_overruns.load(Ordering::Relaxed)
    }

    /// How many times the device's frame time did not follow on from the last period's, as the
    /// backend counts them.
    pub fn frame_time_gaps(&self) -> u64 {
        self.frame_time_gaps.load(Ordering::Relaxed)
    }

    /// How many xruns the audio server reported, whoever caused them.
    pub fn server_xruns(&self) -> u64 {
        self.server_xruns.load(Ordering::Relaxed)
    }

    /// The kernel's id of the audio thread, as the backend read it when the thread started; 0 when
    /// unknown.
    pub fn audio_thread_id(&self) -> i32 {
        self.audio_thread_id.load(Ordering::Relaxed)
    }

    /// Counts one period the processor finished after the device's deadline. Real-time safe.
    pub fn record_own_overrun(&self) {
        self.own_overruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one gap in the device's frame time. Real-time safe.
    pub fn record_frame_time_gap(&self) {
        self.frame_time_gaps.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one xrun reported by the audio server. Real-time safe.
    pub fn record_server_xrun(&self) {
        self.server_xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Records the kernel's id of the audio thread. Real-time safe.
    pub fn set_audio_thread_id(&self, id: i32) {
        self.audio_thread_id.store(id, Ordering::Relaxed);
    }

    /// Statistics at zero, every page of them written through `black_box`, so that the real-time
    /// thread never faults on them (see `engine/memory.rs`). Built off the real-time thread.
    pub(crate) fn new_written() -> Arc<Self> {
        let stats = Arc::new(Self::default());
        stats.callback_times.write_every_page();
        let zero = black_box(0);
        let counters = [
            &stats.periods,
            &stats.last_swap_applied,
            &stats.last_swap_steady,
            &stats.node_errors,
            &stats.events_dropped,
            &stats.non_finite_samples,
            &stats.generation_mismatches,
            &stats.missed_plugin_deadlines,
            &stats.own_overruns,
            &stats.frame_time_gaps,
            &stats.server_xruns,
        ];
        for counter in counters {
            counter.store(zero, Ordering::Relaxed);
        }
        stats.block_start.store(black_box(0), Ordering::Relaxed);
        stats.poisoned.store(black_box(false), Ordering::Relaxed);
        stats.audio_thread_id.store(black_box(0), Ordering::Relaxed);
        stats
    }

    pub(crate) fn record_period(&self) {
        self.periods.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn publish_block_start(&self, position: SamplePosition) {
        self.block_start.store(position.0, Ordering::Relaxed);
    }

    pub(crate) fn publish_swap_applied(&self, sequence: u64) {
        self.last_swap_applied.store(sequence, Ordering::Release);
    }

    /// Release: the controller reuses the dropped nodes' slots once it reads this.
    pub(crate) fn publish_swap_steady(&self, sequence: u64) {
        self.last_swap_steady.store(sequence, Ordering::Release);
    }

    pub(crate) fn count_node_errors(&self, count: u64) {
        if count > 0 {
            self.node_errors.fetch_add(count, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_events_dropped(&self, count: u64) {
        if count > 0 {
            self.events_dropped.fetch_add(count, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_non_finite_samples(&self, count: u64) {
        if count > 0 {
            self.non_finite_samples.fetch_add(count, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_generation_mismatches(&self, count: u64) {
        if count > 0 {
            self.generation_mismatches
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    pub(crate) fn set_poisoned(&self) {
        self.poisoned.store(true, Ordering::Relaxed);
    }
}
