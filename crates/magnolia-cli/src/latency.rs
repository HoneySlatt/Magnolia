//! The round trip's latency, which `play` and `record` report before they play a session with
//! input tracks
//! ([ADR-0031](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)):
//! the device's capture and playback latencies as the server reports them, their sum, the
//! recorder's extra latency, and the latency of Magnolia's monitoring path.

use std::fmt;

use magnolia_engine::stats::DeviceLatency;
use magnolia_engine::time::SampleRate;
use magnolia_jack::JackBackend;

use crate::say;

/// The latencies that place a take and that a singer hears, in frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatencyReport {
    /// The device's latencies, as the server reported them last.
    pub device: DeviceLatency,
    /// The latency of the slowest path from an input track to the playback channels.
    pub monitoring: u64,
    /// The recorder's latency beyond the server's, given by `--extra-latency`; `None` when nothing
    /// records.
    pub extra: Option<u32>,
    /// The rate the frames are counted at.
    pub rate: SampleRate,
}

impl LatencyReport {
    /// What places a take: the round trip and the extra.
    pub fn placement(&self) -> u64 {
        self.device
            .round_trip()
            .saturating_add(u64::from(self.extra.unwrap_or(0)))
    }
}

/// Sets the latency of Magnolia's monitoring path, `monitoring` frames, on `backend`, which asks
/// the server to recompute the latencies and waits until it has, then returns the latencies the
/// server reports. A server that did not report them in time is a warning, said on standard
/// output: the report then gives the last latencies the server reported, maybe stale. `extra` is
/// the recorder's, `None` when nothing records.
pub fn settle(
    backend: &JackBackend,
    monitoring: u64,
    extra: Option<u32>,
    rate: SampleRate,
) -> LatencyReport {
    let frames = u32::try_from(monitoring).unwrap_or(u32::MAX);
    if let Err(error) = backend.set_monitoring_latency(frames) {
        say(format_args!(
            "warning: the monitoring path's latency was not set: {error}; the latencies below \
             may be stale."
        ));
    }
    LatencyReport {
        device: backend.device_latency(),
        monitoring,
        extra,
        rate,
    }
}

impl fmt::Display for LatencyReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device = self.device;
        writeln!(formatter, "The round trip, as the JACK server reports it:")?;
        writeln!(
            formatter,
            "  capture: {}, from the input to Magnolia",
            Frames(device.capture.into(), self.rate)
        )?;
        writeln!(
            formatter,
            "  playback: {}, from Magnolia to the output",
            Frames(device.playback.into(), self.rate)
        )?;
        writeln!(
            formatter,
            "  round trip: {}, the capture's and the playback's",
            Frames(device.round_trip(), self.rate)
        )?;
        if let Some(extra) = self.extra {
            writeln!(
                formatter,
                "  extra: {}, the loopback's latency beyond the server's, given by \
                 --extra-latency",
                Frames(extra.into(), self.rate)
            )?;
        }
        write!(
            formatter,
            "  monitoring path: {}, the slowest from an input track to the playback ports",
            Frames(self.monitoring, self.rate)
        )?;
        if self.extra.is_some() {
            write!(
                formatter,
                "\nTakes are placed by {}, the round trip and the extra: a take's first frame is \
                 the one captured that long after the output played its first timeline sample.",
                Frames(self.placement(), self.rate)
            )?;
        }
        Ok(())
    }
}

/// A count of frames, and its length in milliseconds at a rate.
struct Frames(u64, SampleRate);

impl fmt::Display for Frames {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(frames, rate) = *self;
        let milliseconds = frames as f64 * 1_000.0 / f64::from(rate.hertz());
        write!(formatter, "{frames} frames ({milliseconds:.3} ms)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(extra: Option<u32>) -> LatencyReport {
        LatencyReport {
            device: DeviceLatency {
                capture: 256,
                playback: 512,
            },
            monitoring: 132,
            extra,
            rate: SampleRate::new(48_000).unwrap(),
        }
    }

    #[test]
    fn the_report_gives_each_latency_in_frames_and_milliseconds() {
        let recording = report(Some(48)).to_string();
        for line in [
            "  capture: 256 frames (5.333 ms), from the input to Magnolia",
            "  playback: 512 frames (10.667 ms), from Magnolia to the output",
            "  round trip: 768 frames (16.000 ms), the capture's and the playback's",
            "  extra: 48 frames (1.000 ms), the loopback's latency",
            "  monitoring path: 132 frames (2.750 ms), the slowest from an input track",
            "Takes are placed by 816 frames (17.000 ms), the round trip and the extra",
        ] {
            assert!(
                recording.contains(line),
                "{line:?} is missing from:\n{recording}"
            );
        }
        // A play records nothing: no extra, no takes.
        let playing = report(None).to_string();
        assert!(playing.contains("round trip: 768 frames"), "{playing}");
        assert!(
            !playing.contains("extra") && !playing.contains("Takes"),
            "{playing}"
        );
    }
}
