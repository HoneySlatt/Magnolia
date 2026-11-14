//! The latency callback: how the backend learns the device's latencies from JACK, and tells other
//! clients the latency of Magnolia's monitoring path
//! ([ADR-0031](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)).
//!
//! JACK gives each port a capture and a playback latency range, in frames: how long ago what the
//! port carries entered the system, and how long until it leaves it. Before it calls a client's
//! latency callback, JACK recomputes that client's input ports' capture ranges, in capture mode,
//! or its output ports' playback ranges, in playback mode, from what they are connected to. The
//! callback then sets the other side itself, which JACK would otherwise copy across:
//!
//! - capture mode: every output port's capture range is the inputs' widest capture range plus
//!   the monitoring latency, since what enters `capture_1` leaves `playback_1` that much later;
//! - playback mode: every input port's playback range is the outputs' widest playback range plus
//!   the monitoring latency.
//!
//! The device's latencies, the largest capture range's maximum over the input ports and the
//! largest playback range's maximum over the output ports, go to [`LatencyShared`], which the
//! process callback publishes to the engine every period and the handle reads.
//!
//! JACK2 calls the callback on its notification thread, once in each mode, after every change of
//! the graph it runs and on `jack_recompute_total_latencies`. The callback's own code touches
//! atomics only: no lock, no allocation, no wait, as every notification of the backend. JACK2's
//! range calls may themselves sleep for 1.1 periods, off the real-time thread, while a change of
//! the graph is pending (`WaitGraphChange` in its `JackAPI.cpp`, read in JACK2 1.9.22 and seen by
//! RealtimeSanitizer): on its notification thread, which is not real-time, as JACK2 means them to.

// The callback runs on JACK's notification thread, and an unwinding panic would abort the process
// at the C boundary: no panicking code (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use jack::Client;
use jack::jack_sys::{
    JackCaptureLatency, JackPlaybackLatency, jack_latency_callback_mode_t, jack_latency_range_t,
    jack_port_get_latency_range, jack_port_set_latency_range, jack_port_t,
    jack_set_latency_callback,
};
use magnolia_engine::stats::DeviceLatency;

use crate::notifications::Shared;

/// What the latency callback shares with the handle and the process callback: atomics only.
#[derive(Debug, Default)]
pub(crate) struct LatencyShared {
    /// The device's capture latency, in frames, as the callback last read it.
    capture: AtomicU32,
    /// The device's playback latency, in frames, as the callback last read it.
    playback: AtomicU32,
    /// The latency Magnolia's monitoring path adds, in frames, which the handle sets.
    monitoring: AtomicU32,
    /// The last request for a run of the callback that the handle made; requests count from 1.
    requests: AtomicU64,
    /// The newest request the callback had seen when it began its last run in capture mode.
    capture_served: AtomicU64,
    /// The newest request the callback had seen when it began its last run in playback mode.
    playback_served: AtomicU64,
}

impl LatencyShared {
    /// The device's latencies, as the callback last read them; both 0 before its first run.
    /// Two loads: a reader between the callback's two modes sees one mode's new value beside the
    /// other's old one, as it would have read the ports' ranges themselves.
    pub(crate) fn device(&self) -> DeviceLatency {
        DeviceLatency {
            capture: self.capture.load(Ordering::Acquire),
            playback: self.playback.load(Ordering::Acquire),
        }
    }

    pub(crate) fn monitoring(&self) -> u32 {
        self.monitoring.load(Ordering::Acquire)
    }

    pub(crate) fn set_monitoring(&self, frames: u32) {
        self.monitoring.store(frames, Ordering::Release);
    }

    /// A new request for a run of the callback: its number, which [`served`](Self::served) takes.
    /// Make it before asking JACK for the run, so that a run JACK begins after that sees it.
    pub(crate) fn new_request(&self) -> u64 {
        self.requests
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    /// Whether the callback has run in both modes since request `request` was made. libjack
    /// recomputes the ports' ranges before it calls the callback, so a run that an earlier change
    /// started may mark the request served with ranges of the graph before it. This holds on
    /// JACK2 because its recompute is synchronous: `jack_recompute_total_latencies` returns only
    /// after a run that began after the request has stored its figures, and runs come one after
    /// the other on one thread. A server that recomputes asynchronously could give stale figures.
    pub(crate) fn served(&self, request: u64) -> bool {
        self.capture_served.load(Ordering::Acquire) >= request
            && self.playback_served.load(Ordering::Acquire) >= request
    }
}

/// A latency range, as JACK gives it: the fewest and the most frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Range {
    pub(crate) min: u32,
    pub(crate) max: u32,
}

impl Range {
    /// The widest range over `ranges`: the smallest minimum and the largest maximum, as JACK's own
    /// default takes them; `(0, 0)` when there is none, as for a port connected to nothing.
    pub(crate) fn widest(ranges: impl IntoIterator<Item = Range>) -> Self {
        ranges
            .into_iter()
            .reduce(|widest, range| Range {
                min: widest.min.min(range.min),
                max: widest.max.max(range.max),
            })
            .unwrap_or_default()
    }

    /// The range `frames` later, held at `u32::MAX`.
    pub(crate) fn later_by(self, frames: u32) -> Self {
        Range {
            min: self.min.saturating_add(frames),
            max: self.max.saturating_add(frames),
        }
    }
}

/// One of the client's ports, by libjack's pointer, for the latency callback.
#[derive(Clone, Copy, Debug)]
struct PortPointer(NonNull<jack_port_t>);

// SAFETY: the pointer is a handle that libjack gave for one of the client's ports; JACK2's is the
// port's number, cast. This type never dereferences it: it only hands it back to libjack's latency
// range calls, which JACK documents for the latency callback, on whichever thread JACK runs it.
// Moving or sharing the handle between threads touches no memory behind it.
unsafe impl Send for PortPointer {}
// SAFETY: as for `Send` above: shared, the handle is only read and passed to libjack.
unsafe impl Sync for PortPointer {}

impl PortPointer {
    /// The port's range in `mode`.
    fn range(self, mode: jack_latency_callback_mode_t) -> Range {
        let mut range = jack_latency_range_t::default();
        // SAFETY: the port is one of the client's, registered until the client closes, and the
        // callback that calls this runs only while the client is open (`LentContext`); `range` is
        // a valid place for libjack to write the range.
        unsafe { jack_port_get_latency_range(self.0.as_ptr(), mode, &raw mut range) };
        // The fields are copied out of the packed struct, never borrowed.
        Range {
            min: range.min,
            max: range.max,
        }
    }

    /// Sets the port's range in `mode`.
    fn set_range(self, mode: jack_latency_callback_mode_t, range: Range) {
        let mut range = jack_latency_range_t {
            min: range.min,
            max: range.max,
        };
        // SAFETY: as in `range`; libjack reads `range` and keeps no pointer to it.
        unsafe { jack_port_set_latency_range(self.0.as_ptr(), mode, &raw mut range) };
    }
}

/// The latency callback's context: Magnolia's ports, by libjack's pointers, and what the backend's
/// parts share. Lent to JACK by [`lend`]; read through shared references only.
pub(crate) struct LatencyContext {
    capture_ports: Box<[PortPointer]>,
    playback_ports: Box<[PortPointer]>,
    shared: Arc<Shared>,
}

impl LatencyContext {
    /// The context of a client whose input ports are `capture_ports` and output ports
    /// `playback_ports`, by libjack's pointers, which must stay registered as long as the client
    /// is open: the backend never unregisters them.
    pub(crate) fn new(
        capture_ports: impl IntoIterator<Item = *mut jack_port_t>,
        playback_ports: impl IntoIterator<Item = *mut jack_port_t>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            capture_ports: port_pointers(capture_ports),
            playback_ports: port_pointers(playback_ports),
            shared,
        }
    }

    /// One run in capture mode: the inputs' capture ranges, which JACK has just recomputed, give
    /// the device's capture latency and, plus the monitoring latency, the outputs' capture range.
    fn on_capture(&self) {
        let latency = self.shared.latency();
        let request = latency.requests.load(Ordering::Acquire);
        let inputs = Range::widest(
            self.capture_ports
                .iter()
                .map(|port| port.range(JackCaptureLatency)),
        );
        let outputs = inputs.later_by(latency.monitoring());
        for port in &self.playback_ports {
            port.set_range(JackCaptureLatency, outputs);
        }
        latency.capture.store(inputs.max, Ordering::Release);
        latency.capture_served.fetch_max(request, Ordering::AcqRel);
    }

    /// One run in playback mode: the outputs' playback ranges, which JACK has just recomputed,
    /// give the device's playback latency and, plus the monitoring latency, the inputs' playback
    /// range.
    fn on_playback(&self) {
        let latency = self.shared.latency();
        let request = latency.requests.load(Ordering::Acquire);
        let outputs = Range::widest(
            self.playback_ports
                .iter()
                .map(|port| port.range(JackPlaybackLatency)),
        );
        let inputs = outputs.later_by(latency.monitoring());
        for port in &self.capture_ports {
            port.set_range(JackPlaybackLatency, inputs);
        }
        latency.playback.store(outputs.max, Ordering::Release);
        latency.playback_served.fetch_max(request, Ordering::AcqRel);
    }
}

/// The pointers of registered ports, which libjack never gives null.
fn port_pointers(ports: impl IntoIterator<Item = *mut jack_port_t>) -> Box<[PortPointer]> {
    ports
        .into_iter()
        .filter_map(NonNull::new)
        .map(PortPointer)
        .collect()
}

/// The latency callback, as libjack calls it, with the context [`lend`] gave it.
///
/// JACK2 cancels the thread that runs it when the client closes (`pthread_cancel`, in
/// `JackSocketClientChannel::Stop`), which may land while libjack sleeps in a range call. The
/// forced unwind then crosses this function's frames, which hold nothing to drop: keep them so.
unsafe extern "C" fn latency_callback(mode: jack_latency_callback_mode_t, context: *mut c_void) {
    // SAFETY: `context` is the pointer `lend` registered, to a `LatencyContext` that stays
    // allocated and unmoved as long as JACK may call this: the backend frees it only once the
    // client is deactivated, its callback cleared and the client closed (`LentContext::take_back`),
    // and never otherwise. Nothing borrows it mutably: every field is read, and the state that
    // changes is atomic.
    let Some(context) = (unsafe { context.cast::<LatencyContext>().as_ref() }) else {
        return;
    };
    // Compared, not matched: the modes are libjack's constants, with its names.
    if mode == JackCaptureLatency {
        context.on_capture();
    } else if mode == JackPlaybackLatency {
        context.on_playback();
    }
}

/// The latency callback's context while JACK holds it: allocated by [`lend`], freed by
/// [`take_back`](Self::take_back) once JACK can no longer call the callback. Dropped otherwise,
/// it stays allocated for the process's life, as JACK may still point into it.
pub(crate) struct LentContext(NonNull<LatencyContext>);

// SAFETY: it owns the context as a `Box` would, and `LatencyContext` is `Send` and `Sync`: JACK's
// thread reads it through shared references while the handle, on any thread, holds this.
unsafe impl Send for LentContext {}

/// Sets the latency callback of `client`, which must not be active yet, with `context`; returns
/// the context lent, or libjack's error code.
pub(crate) fn lend(client: &Client, context: LatencyContext) -> Result<LentContext, i32> {
    let context = NonNull::from(Box::leak(Box::new(context)));
    // SAFETY: the client is open and not yet active, as libjack requires to set a callback; the
    // context stays allocated until `take_back`, after JACK can no longer call the callback, and
    // `latency_callback` reads it as a `LatencyContext`.
    let code = unsafe {
        jack_set_latency_callback(
            client.raw(),
            Some(latency_callback),
            context.as_ptr().cast::<c_void>(),
        )
    };
    if code != 0 {
        // SAFETY: libjack refused the callback, so it holds no pointer to the context, which
        // `Box::leak` made just above and nothing else holds.
        drop(unsafe { Box::from_raw(context.as_ptr()) });
        return Err(code);
    }
    Ok(LentContext(context))
}

impl LentContext {
    /// Leaves the context allocated for the process's life, when the client could not be closed:
    /// JACK may still call the callback, which points into it.
    pub(crate) fn leave_to_jack(self) {
        // Nothing to do but say so: the handle has no destructor, so dropping it frees nothing.
    }

    /// Clears the latency callback of `client`, deactivated, closes the client, and frees the
    /// context. `client` must be the client's last handle, its ports dropped: dropping it then
    /// closes the client, which on JACK2 stops its notification thread and waits for it
    /// (`JackClient::Close`), so that a run of the callback begun before the deactivation has
    /// ended. JACK2 calls a client's callbacks only while it is active. The `jack` crate's own
    /// clearing on deactivation does not know of this callback, which it never set.
    pub(crate) fn take_back(self, client: Client) {
        // SAFETY: the client is open and inactive, as libjack requires to set a callback; with no
        // callback, libjack keeps no pointer to the context.
        let cleared =
            unsafe { jack_set_latency_callback(client.raw(), None, std::ptr::null_mut()) };
        drop(client);
        if cleared != 0 {
            // libjack kept the callback: closing the client has most likely stopped it, but
            // another handle could keep the client open, so the context stays allocated.
            self.leave_to_jack();
            return;
        }
        // SAFETY: the client is inactive and its callback cleared, so JACK no longer calls it, and
        // dropping its last handle closed it, which stopped and joined the thread that called it:
        // nothing points into the context but this, which `lend` made from a `Box`.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

#[cfg(test)]
mod tests {
    use super::{LatencyShared, Range};

    #[test]
    fn the_widest_range_takes_the_smallest_minimum_and_the_largest_maximum() {
        let ranges = [
            Range { min: 64, max: 128 },
            Range { min: 32, max: 96 },
            Range { min: 80, max: 256 },
        ];
        assert_eq!(Range::widest(ranges), Range { min: 32, max: 256 });
        assert_eq!(
            Range::widest([Range { min: 5, max: 7 }]),
            Range { min: 5, max: 7 }
        );
    }

    #[test]
    fn no_port_gives_no_latency() {
        assert_eq!(Range::widest([]), Range { min: 0, max: 0 });
    }

    #[test]
    fn a_range_later_by_a_latency_is_held_at_the_largest_frame_count() {
        assert_eq!(
            Range { min: 64, max: 128 }.later_by(100),
            Range { min: 164, max: 228 }
        );
        assert_eq!(
            Range {
                min: 10,
                max: u32::MAX - 1
            }
            .later_by(5),
            Range {
                min: 15,
                max: u32::MAX
            }
        );
    }

    #[test]
    fn a_request_is_served_once_both_modes_have_run_since() {
        let latency = LatencyShared::default();
        let first = latency.new_request();
        assert_eq!(first, 1);
        assert!(!latency.served(first));
        latency
            .capture_served
            .fetch_max(first, std::sync::atomic::Ordering::AcqRel);
        assert!(!latency.served(first), "the capture mode alone");
        latency
            .playback_served
            .fetch_max(first, std::sync::atomic::Ordering::AcqRel);
        assert!(latency.served(first));
        let second = latency.new_request();
        assert!(!latency.served(second), "a run that began before it");
    }
}
