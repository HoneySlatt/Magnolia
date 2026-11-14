//! The memory a session asks for, estimated before it is allocated, and the budget it must fit:
//! [`MEMORY_BUDGET`]. A session file of a few hundred bytes can ask for gigabytes, through wide
//! tracks, long latencies on wide strips, many clips of one dense MIDI file, or many armed tracks;
//! the loader refuses it with a plain error that says what takes the memory, rather than let the
//! engine allocate it.

use std::fmt;
use std::mem::size_of;
use std::path::PathBuf;

use magnolia_engine::disk::{BLOCK_FRAMES, DiskConfig};
use magnolia_engine::record::RecordConfig;
use magnolia_engine::time::SampleRate;

use super::latency::{Demands, latency_samples};
use crate::build::LARGEST_BLOCK;
use crate::file::{Insert, Routing, Target, TrackSource};
use crate::midi_file::TrackEvent;

/// The most memory a test session may ask for: 1 GiB. The high tier's reference project asks for
/// about a seventh of it.
pub const MEMORY_BUDGET: u64 = 1 << 30;

/// The bytes of one sample, in a stream's slot, a delay's history or the engine's buffers.
const SAMPLE_BYTES: u64 = size_of::<f32>() as u64;

/// The bytes one event of a MIDI file costs each clip that plays it, at the peak of loading: the
/// clip's note or point, the sequence's events and lanes, their vectors' spare room, and the copy
/// the clip is merged from. Set above the 78 bytes measured at the peak of 400 clips of a file of
/// 40,000 events.
const MIDI_EVENT_BYTES: u64 = 128;

/// The bytes of an armed track's ring of marks, beside its ring of frames: 256 marks
/// (`record/tap.rs` in the engine), each a position and a take's start, end or gap, under 64 bytes.
const MARK_RING_BYTES: u64 = 256 * 64;

/// What a session asks for, in bytes, by what takes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryEstimate {
    /// The audio tracks' streams: each track's channels times its slots' frames, the read-ahead
    /// rounded up to whole blocks of the disk reader, and two blocks more, as the engine rounds.
    pub streams: u64,
    /// The histories of the test latencies and of the delay lines the engine puts where paths of
    /// different latencies meet: channels times samples of delay.
    pub delays: u64,
    /// The engine's buffers: one channel of its largest block for every input and output of every
    /// node, and for every capture and playback channel.
    pub buffers: u64,
    /// The armed input tracks' recording rings: each track's ring of frames, as long as a
    /// recorder's by default ([`RecordConfig::new`]), times its channels, and its ring of marks.
    pub rings: u64,
    /// The MIDI files, each parsed once, and the events every clip expands to.
    pub midi: u64,
}

impl MemoryEstimate {
    /// Everything, in bytes.
    pub fn total(&self) -> u64 {
        self.streams
            .saturating_add(self.delays)
            .saturating_add(self.buffers)
            .saturating_add(self.rings)
            .saturating_add(self.midi)
    }

    /// Whether it fits [`MEMORY_BUDGET`].
    pub(crate) fn fits(&self) -> bool {
        self.total() <= MEMORY_BUDGET
    }

    /// What a session of these strips asks for, before its MIDI files are read: its streams, its
    /// delays, its buffers and its recording rings. `sources` gives what each track plays, in the
    /// order of `routing.tracks`; `capture_channels` are the engine's.
    pub(crate) fn of_session(
        rate: SampleRate,
        master: (u16, &[Insert]),
        routing: &Routing,
        sources: &[TrackSource],
        capture_channels: u16,
    ) -> Self {
        let (master_channels, master_inserts) = master;
        let read_ahead = DiskConfig::new(rate).read_ahead;
        let block = BLOCK_FRAMES as u64;
        let stream_frames = read_ahead
            .div_ceil(block)
            .saturating_add(2)
            .saturating_mul(block);
        let ring_frames = RecordConfig::new(PathBuf::new(), rate).ring_frames;
        let mut stream_samples: u64 = 0;
        let mut delay_samples: u64 = 0;
        let mut rings: u64 = 0;
        // The capture and playback channels, then every node's inputs and outputs.
        let mut buffer_channels =
            u64::from(master_channels).saturating_add(u64::from(capture_channels));
        let mut add = |(buffers, delays): (u64, u64)| {
            buffer_channels = buffer_channels.saturating_add(buffers);
            delay_samples = delay_samples.saturating_add(delays);
        };
        add(chain(master_channels, master_inserts, 0));
        for bus in &routing.buses {
            add(chain(bus.channels, &bus.inserts, 1));
        }
        for (strip, source) in routing.tracks.iter().zip(sources) {
            let channels = u64::from(strip.channels);
            match source {
                TrackSource::Audio { .. } => {
                    stream_samples =
                        stream_samples.saturating_add(stream_frames.saturating_mul(channels));
                }
                TrackSource::Input { armed: true, .. } => {
                    let frames = ring_frames.saturating_mul(channels);
                    rings = rings.saturating_add(
                        frames
                            .saturating_mul(SAMPLE_BYTES)
                            .saturating_add(MARK_RING_BYTES),
                    );
                }
                TrackSource::Midi(_) | TrackSource::Input { armed: false, .. } => {}
            }
            // The source's outputs, which an input track takes from the capture channels, its
            // chain, then a gain of its channels per send.
            if !matches!(source, TrackSource::Input { .. }) {
                add((channels, 0));
            }
            add(chain(strip.channels, &strip.inserts, 1));
            add(chain(strip.channels, &[], strip.sends.len()));
        }
        for (channels, delay) in delay_lines(master_channels, master_inserts, routing, sources) {
            add(chain(channels, &[], 1));
            add((0, u64::from(channels).saturating_mul(delay)));
        }
        let stride = LARGEST_BLOCK.next_multiple_of(16) as u64;
        Self {
            streams: stream_samples.saturating_mul(SAMPLE_BYTES),
            delays: delay_samples.saturating_mul(SAMPLE_BYTES),
            buffers: buffer_channels
                .saturating_mul(stride)
                .saturating_mul(SAMPLE_BYTES),
            rings,
            midi: 0,
        }
    }

    /// Adds a MIDI file of `events` events, parsed and kept while the session loads.
    pub(crate) fn add_midi_file(&mut self, events: u64) {
        let parsed = events.saturating_mul(size_of::<TrackEvent>() as u64);
        self.midi = self.midi.saturating_add(parsed);
    }

    /// Adds a clip that plays a MIDI file of `events` events.
    pub(crate) fn add_midi_clip(&mut self, events: u64) {
        self.midi = self
            .midi
            .saturating_add(events.saturating_mul(MIDI_EVENT_BYTES));
    }
}

impl fmt::Display for MemoryEstimate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} in all: {} of audio streams, {} of delays, {} of the engine's buffers, {} of \
             recording rings, {} of MIDI events",
            Bytes(self.total()),
            Bytes(self.streams),
            Bytes(self.delays),
            Bytes(self.buffers),
            Bytes(self.rings),
            Bytes(self.midi)
        )
    }
}

/// A count of bytes, shown in the largest binary unit that keeps it at 1 or more.
struct Bytes(u64);

impl fmt::Display for Bytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
        if self.0 < 1_024 {
            return write!(formatter, "{} bytes", self.0);
        }
        let mut value = self.0 as f64 / 1_024.0;
        let mut unit = UNITS.iter().peekable();
        while value >= 1_024.0 && unit.len() > 1 {
            value /= 1_024.0;
            unit.next();
        }
        write!(formatter, "{value:.1} {}", unit.peek().unwrap_or(&&"TiB"))
    }
}

/// What a chain of `inserts` and `gains` more nodes of `channels` channels asks for: the channels
/// of its nodes' inputs and outputs in the engine's buffers, and the samples its test latencies
/// hold.
fn chain(channels: u16, inserts: &[Insert], gains: usize) -> (u64, u64) {
    let nodes = (inserts.len() as u64).saturating_add(gains as u64);
    let channels = u64::from(channels);
    (
        nodes.saturating_mul(2).saturating_mul(channels),
        latency_samples(inserts).saturating_mul(channels),
    )
}

/// Where a track's gain feeds: a bus's or the master's input, or one of its sends' gains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Destination {
    Target(Target),
    Send { track: usize, send: usize },
}

/// The delay lines the engine adds to a session's graph, each as its channels and its delay, by
/// the engine's rule (`docs/ARCHITECTURE.md`, *Delay compensation*) applied to the graph a session
/// builds. A strip's input runs ahead by the latency of everything after it to the playback
/// channels. A bus feeds one input, so it needs no delay line; a track's gain feeds its output and
/// each send's gain, and runs as far ahead as the most demanding of them: every other one gets a
/// delay line of the difference, one per destination and delay. An input track is live: nothing
/// out of it is delayed, so it gets none. `sources` gives what each track plays, in the order of
/// `routing.tracks`.
fn delay_lines(
    master_channels: u16,
    master_inserts: &[Insert],
    routing: &Routing,
    sources: &[TrackSource],
) -> Vec<(u16, u64)> {
    let demands = Demands::new(master_inserts, &routing.buses);
    let channels_of = |target: Target| match target {
        Target::Master => master_channels,
        Target::Bus(index) => routing.buses.get(index).map_or(0, |bus| bus.channels),
    };
    let mut lines = std::collections::BTreeMap::new();
    for (track, (strip, source)) in routing.tracks.iter().zip(sources).enumerate() {
        if let TrackSource::Input { .. } = source {
            continue;
        }
        let mut destinations = vec![(
            Destination::Target(strip.output),
            demands.of(strip.output),
            channels_of(strip.output),
        )];
        for (send, &(to, _)) in strip.sends.iter().enumerate() {
            destinations.push((
                Destination::Send { track, send },
                demands.of(to),
                strip.channels,
            ));
        }
        let ahead = destinations
            .iter()
            .map(|&(_, demand, _)| demand)
            .max()
            .unwrap_or(0);
        for (destination, demand, channels) in destinations {
            let delay = ahead.saturating_sub(demand);
            if delay > 0 {
                lines.insert((destination, delay), channels);
            }
        }
    }
    lines
        .into_iter()
        .map(|((_, delay), channels)| (channels, delay))
        .collect()
}
