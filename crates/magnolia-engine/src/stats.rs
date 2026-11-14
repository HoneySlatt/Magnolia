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
use crate::seqlock::SeqLock;
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
    events_taken: AtomicU64,
    events_dropped: AtomicU64,
    latencies_capped: AtomicU64,
    non_finite_samples: AtomicU64,
    generation_mismatches: AtomicU64,
    missed_plugin_deadlines: AtomicU64,
    disk_underruns: AtomicU64,
    capture_overruns: AtomicU64,
    missed_takes: AtomicU64,
    plays_refused: AtomicU64,
    tap_stopped: AtomicBool,
    /// The last generation of disk streams handed out, by any disk reader of this engine.
    generations: AtomicU64,
    /// The latest generation a disk reader has primed.
    primed_generation: AtomicU64,
    poisoned: AtomicBool,
    own_overruns: AtomicU64,
    frame_time_gaps: AtomicU64,
    server_xruns: AtomicU64,
    audio_thread_id: AtomicI32,
    /// The device's latencies, the capture's in the high 32 bits and the playback's in the low 32,
    /// in one word, so that the real-time thread reads both at once without waiting.
    device_latency: AtomicU64,
    /// The last play's start: its sequence number, then the engine sample at which the output
    /// reaches its first timeline sample, as an `i64`'s bits. The processor writes it, never
    /// waiting; a reader that meets a write tries again.
    play_start: SeqLock<2>,
}

/// Where the last play started: the play's sequence number, as
/// [`Controller::play`](crate::engine::Controller::play) returned it, and the engine sample at
/// which the output reaches the timeline sample the play asked for: the period that applied the
/// command starts there, plus the graph's largest compensation, by which the transport pre-rolls
/// ([ADR-0025](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0025-delay-compensation-by-reading-ahead.md)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayStart {
    /// The play's sequence number.
    pub sequence: u64,
    /// The engine sample at which the output plays the play's first timeline sample.
    pub audible_at: SamplePosition,
}

/// The device's latencies as the audio server reports them, in frames
/// ([ADR-0031](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)):
/// how long a captured frame took to reach the engine, and how long a frame the engine plays takes
/// to reach the output. Both 0 for a device without latency, as the device-less backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceLatency {
    /// From the input to the engine's capture buffers, in frames.
    pub capture: u32,
    /// From the engine's playback buffers to the output, in frames.
    pub playback: u32,
}

impl DeviceLatency {
    /// The round trip the server reports: the capture latency plus the playback latency.
    pub fn round_trip(self) -> u64 {
        u64::from(self.capture).saturating_add(u64::from(self.playback))
    }

    fn packed(self) -> u64 {
        (u64::from(self.capture) << 32) | u64::from(self.playback)
    }

    fn unpacked(word: u64) -> Self {
        Self {
            capture: (word >> 32) as u32,
            playback: word as u32,
        }
    }
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

    /// How many events the processor has taken out of its buffer of pending events, delivered or
    /// dropped. The controller sends no more events than that buffer holds beyond this count
    /// ([ADR-0027](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0027-timeline-events-from-sources-on-nodes.md)).
    pub fn events_taken(&self) -> u64 {
        self.events_taken.load(Ordering::Relaxed)
    }

    /// How many events were dropped: the controller's that no node of the current graph took, and
    /// those past the room a node's timeline sources have in one sub-block
    /// ([`EngineConfig::event_capacity`](crate::engine::EngineConfig::event_capacity)).
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped.load(Ordering::Relaxed)
    }

    /// How many nodes declared a latency above [`MAX_LATENCY`](crate::node::MAX_LATENCY), which
    /// the engine took as `MAX_LATENCY`.
    pub fn latencies_capped(&self) -> u64 {
        self.latencies_capped.load(Ordering::Relaxed)
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

    /// How many frames the audio players lacked, summed over every player: frames of a play's
    /// audible region that their disk stream did not hold ready when the player reached them, and
    /// which it played as silence
    /// ([ADR-0026](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0026-one-stream-per-audio-track.md)).
    pub fn disk_underruns(&self) -> u64 {
        self.disk_underruns.load(Ordering::Relaxed)
    }

    /// How many frames the capture tap dropped because an armed track's ring had no room for them:
    /// the recorder's thread had not emptied it in time. The recorder writes each as a zero, so that
    /// the rest of the take stays in place
    /// ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).
    pub fn capture_overruns(&self) -> u64 {
        self.capture_overruns.load(Ordering::Relaxed)
    }

    /// How many plays made no take on an armed track because its tap already held
    /// [`MAX_WINDOWS`](crate::record::MAX_WINDOWS) takes waiting for their frames: plays and stops
    /// that came faster than the round trip, many times over.
    pub fn missed_takes(&self) -> u64 {
        self.missed_takes.load(Ordering::Relaxed)
    }

    /// How many plays the transport refused because the graph had panicked: after that panic a
    /// stop still ends the play under way and its takes, but no play starts on a silent output
    /// ([ADR-0034](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0034-no-play-after-the-graphs-panic.md)).
    pub fn plays_refused(&self) -> u64 {
        self.plays_refused.load(Ordering::Relaxed)
    }

    /// Whether a panic stopped the capture tap: it records nothing more, while the graph plays on
    /// ([ADR-0032](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0032-recording-by-a-capture-tap-of-the-processor.md)).
    /// A recorder that reads it ends its open takes at what they hold.
    pub fn tap_stopped(&self) -> bool {
        self.tap_stopped.load(Ordering::Acquire)
    }

    /// The latest generation of disk streams that a disk reader has primed: every stream holds what
    /// a play from its cue reads first. A play of a later generation waits for it
    /// ([`Controller::play`](crate::engine::Controller::play)); 0 before any.
    pub fn primed_generation(&self) -> u64 {
        self.primed_generation.load(Ordering::Acquire)
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

    /// The device's latencies, as the backend last published them. Real-time safe: one atomic load.
    pub fn device_latency(&self) -> DeviceLatency {
        DeviceLatency::unpacked(self.device_latency.load(Ordering::Acquire))
    }

    /// Where the last play started, once a play has; the controller numbers plays from 1.
    pub fn play_start(&self) -> Option<PlayStart> {
        let [sequence, audible_at] = self.play_start.read();
        (sequence > 0).then_some(PlayStart {
            sequence,
            audible_at: SamplePosition(audible_at as i64),
        })
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

    /// Publishes the device's latencies, as the audio server reports them. The backend calls it
    /// every period, so that a processor swapped in has them; both reach a reader together.
    /// Real-time safe.
    pub fn publish_device_latency(&self, latency: DeviceLatency) {
        self.device_latency
            .store(latency.packed(), Ordering::Release);
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
            &stats.events_taken,
            &stats.events_dropped,
            &stats.latencies_capped,
            &stats.non_finite_samples,
            &stats.generation_mismatches,
            &stats.missed_plugin_deadlines,
            &stats.disk_underruns,
            &stats.capture_overruns,
            &stats.missed_takes,
            &stats.plays_refused,
            &stats.generations,
            &stats.primed_generation,
            &stats.own_overruns,
            &stats.frame_time_gaps,
            &stats.server_xruns,
            &stats.device_latency,
        ];
        for counter in counters {
            counter.store(zero, Ordering::Relaxed);
        }
        stats.block_start.store(black_box(0), Ordering::Relaxed);
        stats.poisoned.store(black_box(false), Ordering::Relaxed);
        stats.tap_stopped.store(black_box(false), Ordering::Relaxed);
        stats.audio_thread_id.store(black_box(0), Ordering::Relaxed);
        stats
    }

    pub(crate) fn record_period(&self) {
        self.periods.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn publish_block_start(&self, position: SamplePosition) {
        self.block_start.store(position.0, Ordering::Relaxed);
    }

    /// Publishes a play's start. Never waits: the readers are the ones that try again. Only the
    /// processor writes it.
    pub(crate) fn publish_play_start(&self, sequence: u64, audible_at: SamplePosition) {
        self.play_start.write([sequence, audible_at.0 as u64]);
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

    /// Counts events taken out of the pending buffer. The count only grows, after the events have
    /// left the buffer, so the controller never reads more room than there is: relaxed is enough.
    pub(crate) fn count_events_taken(&self, count: u64) {
        if count > 0 {
            self.events_taken.fetch_add(count, Ordering::Relaxed);
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

    pub(crate) fn count_disk_underruns(&self, frames: u64) {
        if frames > 0 {
            self.disk_underruns.fetch_add(frames, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_capture_overruns(&self, frames: u64) {
        if frames > 0 {
            self.capture_overruns.fetch_add(frames, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_missed_take(&self) {
        self.missed_takes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_play_refused(&self) {
        self.plays_refused.fetch_add(1, Ordering::Relaxed);
    }

    /// Release: a recorder that reads it set then finds in its rings everything the tap pushed.
    pub(crate) fn set_tap_stopped(&self) {
        self.tap_stopped.store(true, Ordering::Release);
    }

    /// A new generation of disk streams, above every one handed out before on this engine, so that
    /// a disk reader made again, or a second one, never reuses one. From 1.
    pub(crate) fn next_generation(&self) -> u64 {
        self.generations
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    /// Publishes that a disk reader has primed `generation`. Never lowers what was published.
    pub(crate) fn publish_primed(&self, generation: u64) {
        self.primed_generation
            .fetch_max(generation, Ordering::Release);
    }

    pub(crate) fn count_latency_capped(&self) {
        self.latencies_capped.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn set_poisoned(&self) {
        self.poisoned.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceLatency, EngineStats};

    #[test]
    fn the_device_latency_reads_back_as_published_whole() {
        let stats = EngineStats::new_written();
        assert_eq!(stats.device_latency(), DeviceLatency::default());
        for latency in [
            DeviceLatency {
                capture: 64,
                playback: 128,
            },
            DeviceLatency {
                capture: u32::MAX,
                playback: 0,
            },
            DeviceLatency {
                capture: 0,
                playback: u32::MAX,
            },
        ] {
            stats.publish_device_latency(latency);
            assert_eq!(stats.device_latency(), latency);
        }
    }

    #[test]
    fn the_round_trip_sums_both_sides_without_overflow() {
        let latency = DeviceLatency {
            capture: u32::MAX,
            playback: u32::MAX,
        };
        assert_eq!(latency.round_trip(), 2 * u64::from(u32::MAX));
    }
}
