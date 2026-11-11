//! The process callback: what runs on the audio thread once a period.
//!
//! It wraps the client's ports in a [`DeviceBuffers`] and calls [`Processor::process`], and around
//! it: swaps in a processor the handle sent, counts gaps in the server's frame time, its own
//! overruns of the period's deadline, and the server's xruns, and writes silence while the server's
//! rate differs from the engine's. It never allocates, frees, locks or blocks: what it lets go goes
//! back to the handle, which drops it.

// The process callback runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;

use jack::{AudioIn, AudioOut, Client, Control, Port, ProcessHandler, ProcessScope};
use magnolia_engine::device::DeviceBuffers;
use magnolia_engine::engine::Processor;
use rtrb::{Consumer, Producer, PushError};

use crate::notifications::{Shared, thread_id_stored_by_thread_init};

/// The backend's process handler, owned by the audio thread while the client is active.
pub(crate) struct Callback {
    /// Boxed, so that a swap moves a pointer.
    processor: Box<Processor>,
    capture: Vec<Port<AudioIn>>,
    playback: Vec<Port<AudioOut>>,
    /// Processors the handle sent to replace this one.
    replacements: Consumer<Box<Processor>>,
    /// Processors this callback replaced, for the handle to drop.
    replaced: Producer<Box<Processor>>,
    shared: Arc<Shared>,
    engine_rate: u32,
    frame_clock: FrameClock,
    /// The audio thread's id once known; 0 before.
    audio_thread_id: i32,
    /// The server's xruns already counted in the processor's statistics.
    xruns_counted: u64,
}

/// What a callback is made of.
pub(crate) struct CallbackParts {
    pub(crate) processor: Box<Processor>,
    pub(crate) capture: Vec<Port<AudioIn>>,
    pub(crate) playback: Vec<Port<AudioOut>>,
    pub(crate) replacements: Consumer<Box<Processor>>,
    pub(crate) replaced: Producer<Box<Processor>>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) engine_rate: u32,
}

impl Callback {
    pub(crate) fn new(parts: CallbackParts) -> Self {
        Self {
            processor: parts.processor,
            capture: parts.capture,
            playback: parts.playback,
            replacements: parts.replacements,
            replaced: parts.replaced,
            shared: parts.shared,
            engine_rate: parts.engine_rate,
            frame_clock: FrameClock::default(),
            audio_thread_id: 0,
            xruns_counted: 0,
        }
    }

    /// The processor being played, given back when the client is deactivated.
    pub(crate) fn into_processor(self) -> Processor {
        *self.processor
    }

    /// Swaps in the processor the handle sent, if any, when the queue back to the handle has room
    /// for the one it replaces: the old processor is never dropped here.
    fn swap_in_replacement(&mut self) {
        if self.replacements.is_empty() || self.replaced.slots() == 0 {
            return;
        }
        let Ok(replacement) = self.replacements.pop() else {
            return;
        };
        let old = std::mem::replace(&mut self.processor, replacement);
        // The new engine's statistics start from this period, but for the thread's id.
        self.xruns_counted = self.shared.server_xruns();
        if self.audio_thread_id != 0 {
            self.processor
                .stats()
                .set_audio_thread_id(self.audio_thread_id);
        }
        if let Err(PushError::Full(old)) = self.replaced.push(old) {
            // Never happens: room was checked above, and this callback is the queue's only
            // producer. Leaking it beats freeing memory on the real-time thread.
            std::mem::forget(old);
        }
    }

    /// Publishes the audio thread's id, once the thread-init callback has stored it on this
    /// thread.
    fn learn_audio_thread_id(&mut self) {
        if self.audio_thread_id != 0 {
            return;
        }
        let id = thread_id_stored_by_thread_init();
        if id == 0 {
            return;
        }
        self.audio_thread_id = id;
        self.shared.set_audio_thread_id(id);
        self.processor.stats().set_audio_thread_id(id);
    }

    /// Counts in the processor's statistics the xruns the notification handler saw since the last
    /// period.
    fn count_server_xruns(&mut self) {
        let total = self.shared.server_xruns();
        while self.xruns_counted < total {
            self.processor.stats().record_server_xrun();
            self.xruns_counted = self.xruns_counted.saturating_add(1);
        }
    }
}

impl ProcessHandler for Callback {
    /// One period. Real-time: no allocation, no lock, and no system call but the processor's wake
    /// of its collector; RealtimeSanitizer checks it (`tests/realtime_sanitizer.rs`).
    #[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
    fn process(&mut self, client: &Client, scope: &ProcessScope) -> Control {
        let entry = client.time();
        let cycle = scope.cycle_times().ok();
        self.swap_in_replacement();
        self.learn_audio_thread_id();
        match cycle {
            Some(cycle) => {
                if self
                    .frame_clock
                    .follow(cycle.current_frames, scope.n_frames())
                {
                    self.processor.stats().record_frame_time_gap();
                }
            }
            None => self.frame_clock.forget(),
        }
        self.count_server_xruns();
        if self.shared.server_rate() == self.engine_rate {
            let mut device = PortBuffers {
                scope,
                capture: &self.capture,
                playback: &mut self.playback,
            };
            self.processor.process(&mut device);
        } else {
            // At another rate the engine would play at the wrong pitch: silence until the rate is
            // back, without calling the processor, whose heartbeat then stops.
            for port in &mut self.playback {
                port.as_mut_slice(scope).fill(0.0);
            }
        }
        if let Some(cycle) = cycle
            && is_own_overrun(entry, client.time(), cycle.next_usecs, cycle.period_usecs)
        {
            self.processor.stats().record_own_overrun();
        }
        Control::Continue
    }
}

/// Whether a callback that ran from `entry` to `exit`, in the server's microseconds, overran the
/// period on its own: it started in time and ended after the next period's estimated start
/// (`next_usecs`), or its work alone took longer than a period. A callback the server started
/// after its deadline was late by the server's doing, not its own: an upstream client that
/// overran, or a clock estimate that lags the wall clock, as JACK2's dummy driver's does at small
/// periods without real time.
fn is_own_overrun(entry: u64, exit: u64, next_usecs: u64, period_usecs: f32) -> bool {
    let ended_late = entry <= next_usecs && exit > next_usecs;
    let took_a_period = exit.saturating_sub(entry) as f32 > period_usecs;
    ended_late || took_a_period
}

/// The client's ports for one period, as the processor reads and writes them.
struct PortBuffers<'a> {
    scope: &'a ProcessScope,
    capture: &'a [Port<AudioIn>],
    playback: &'a mut [Port<AudioOut>],
}

impl DeviceBuffers for PortBuffers<'_> {
    fn frames(&self) -> usize {
        self.scope.n_frames() as usize
    }

    fn capture_channels(&self) -> usize {
        self.capture.len()
    }

    fn playback_channels(&self) -> usize {
        self.playback.len()
    }

    /// Empty for a port that no longer exists, which the processor takes as silence.
    fn capture(&self, channel: usize) -> &[f32] {
        self.capture
            .get(channel)
            .map(|port| port.as_slice(self.scope))
            .unwrap_or_default()
    }

    /// Empty for a port that no longer exists, which the processor leaves alone.
    fn playback(&mut self, channel: usize) -> &mut [f32] {
        self.playback
            .get_mut(channel)
            .map(|port| port.as_mut_slice(self.scope))
            .unwrap_or_default()
    }
}

/// Follows the server's frame time from period to period, to find gaps: a period whose first frame
/// does not follow on from the last period's, as when the server skipped this client's cycles.
#[derive(Debug, Default)]
pub(crate) struct FrameClock {
    last: Option<Period>,
}

/// A period as the server timed it.
#[derive(Clone, Copy, Debug)]
struct Period {
    /// The frame time of its first frame.
    start: u32,
    frames: u32,
}

impl FrameClock {
    /// Notes a period whose first frame is at frame time `start`, `frames` long, and returns
    /// whether it leaves a gap after the last one. The frame time is a 32-bit count that wraps, so
    /// the comparison wraps too. The first period, and the first after a change of the period's
    /// length, are never gaps.
    pub(crate) fn follow(&mut self, start: u32, frames: u32) -> bool {
        let gap = self.last.is_some_and(|last| {
            last.frames == frames && start != last.start.wrapping_add(last.frames)
        });
        self.last = Some(Period { start, frames });
        gap
    }

    /// Forgets the last period, when a period's frame time could not be read: the next one is
    /// never a gap.
    pub(crate) fn forget(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameClock, is_own_overrun};

    #[test]
    fn an_own_overrun_ends_after_the_deadline_it_started_before_or_takes_a_period() {
        let (next, period) = (10_000, 1_333.3);
        assert!(!is_own_overrun(9_000, 9_900, next, period), "in time");
        assert!(
            !is_own_overrun(9_000, 10_000, next, period),
            "at the deadline"
        );
        assert!(
            is_own_overrun(9_000, 10_001, next, period),
            "past the deadline"
        );
        assert!(
            !is_own_overrun(10_100, 10_200, next, period),
            "started late by the server's doing"
        );
        assert!(
            is_own_overrun(10_100, 11_500, next, period),
            "started late, then took longer than a period"
        );
        assert!(
            !is_own_overrun(9_000, 8_000, next, period),
            "a clock that went back"
        );
    }

    #[test]
    fn periods_that_follow_on_are_no_gap() {
        let mut clock = FrameClock::default();
        assert!(!clock.follow(1_000, 64), "the first period");
        assert!(!clock.follow(1_064, 64));
        assert!(!clock.follow(1_128, 64));
    }

    #[test]
    fn a_skipped_period_a_repeated_one_and_a_jump_back_are_gaps() {
        let mut clock = FrameClock::default();
        clock.follow(0, 64);
        assert!(clock.follow(128, 64), "one period skipped");
        assert!(clock.follow(128, 64), "the same period again");
        assert!(clock.follow(64, 64), "a jump back");
        assert!(!clock.follow(128, 64));
    }

    #[test]
    fn the_frame_time_wraps_without_a_gap() {
        let mut clock = FrameClock::default();
        let start = u32::MAX - 63;
        clock.follow(start - 64, 64);
        assert!(!clock.follow(start, 64));
        assert!(!clock.follow(0, 64), "the 32-bit frame time wrapped");
        assert!(!clock.follow(64, 64));
    }

    #[test]
    fn a_gap_across_the_wrap_is_counted() {
        let mut clock = FrameClock::default();
        clock.follow(u32::MAX - 63, 64);
        assert!(clock.follow(64, 64), "one period skipped across the wrap");
        clock.forget();
        clock.follow(u32::MAX - 31, 64);
        assert!(clock.follow(0, 64), "32 frames early, across the wrap");
    }

    #[test]
    fn a_change_of_period_and_a_forgotten_period_are_no_gap() {
        let mut clock = FrameClock::default();
        clock.follow(0, 64);
        assert!(!clock.follow(1_000, 128), "the period changed");
        assert!(clock.follow(5_000, 128));
        clock.forget();
        assert!(!clock.follow(9_000, 128), "the last period was forgotten");
    }
}
