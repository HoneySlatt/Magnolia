//! Building a session into an engine: [`Session::build`] makes the engine, the disk reader and the
//! graph of nodes that plays the session; built to record, it also starts a recorder and arms the
//! armed input tracks, in the same commit.

// A session comes from a file from outside; building it must never crash Magnolia, so nothing here
// may panic.
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use magnolia_engine::disk::{AudioTrack, DiskConfig, DiskError, DiskReader};
use magnolia_engine::engine::{
    self, ArmError, Controller, Destination, EngineConfig, EngineError, GraphError, NodeId,
    Processor, Source,
};
use magnolia_engine::node::Node;
use magnolia_engine::nodes::{Gain, TestLatency, TestSynth};
use magnolia_engine::record::{RecordConfig, RecordError, Recorder};
use magnolia_engine::time::SamplePosition;

use crate::file::{Insert, Instrument, Strip, Target};
use crate::load::{LoadedSource, Session};

/// The engine's largest block for a session: a longer period is cut into blocks of this many
/// frames.
pub const LARGEST_BLOCK: usize = 1_024;

/// Who fills a built session's disk streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderMode {
    /// The disk reader's own thread, `magnolia-disk`, which the players wake: to play.
    Thread,
    /// Its owner's thread, through [`DiskReader::fill`] before each block: to render offline.
    Offline,
}

/// What [`Session::build`] makes: the engine's two halves, the disk reader, and where the session
/// ends.
pub struct BuiltSession {
    /// The engine's controller: it has committed the session's graph and its tempo map.
    pub controller: Controller,
    /// The engine's processor, for a backend to play.
    pub processor: Processor,
    /// The disk reader that fills the audio tracks' streams.
    pub reader: DiskReader,
    /// The sample after the session's last clip: [`Session::last_sample`].
    pub last_sample: SamplePosition,
}

impl Session {
    /// Makes an engine that plays the session, its graph committed:
    ///
    /// - the engine at the session's rate, its playback channels the master's, its capture channels
    ///   as many as the highest that an input track names ([`Session::capture_channels`]), its
    ///   largest block [`LARGEST_BLOCK`];
    /// - the disk reader of its audio tracks, reading 10 seconds ahead, on its thread or not;
    /// - for each audio track its player, for each MIDI track its instrument playing its sequence,
    ///   for each input track its capture channels, one per channel; then the track's inserts in
    ///   series, its gain, into its output's input, and from after its gain one gain per send into
    ///   the send's destination's input;
    /// - for each bus, its inserts in series and its gain, into its output's input;
    /// - for the master, its inserts in series into the playback channels.
    ///
    /// What feeds a bus or the master feeds its input: its first insert, else its gain, or for the
    /// master the playback channels. A mono source feeds every channel of a stereo destination.
    /// An input track's path is live: it sounds whether the transport plays or not, and the engine
    /// never delays it to line it up with others. Without a device's capture, as offline, its
    /// capture channels are silent. It may be called again: each engine reads the same open
    /// files.
    ///
    /// # Errors
    ///
    /// A [`BuildError`] when the engine, the disk reader or the graph refuses what the session
    /// asks; after the checks of [`Session::open`], only a thread that does not start, or a
    /// session of more nodes than the engine's arena holds.
    pub fn build(&self, mode: ReaderMode) -> Result<BuiltSession, BuildError> {
        self.build_then(mode, |_| Ok(())).map(|(built, ())| built)
    }

    /// Builds the session as [`build`](Self::build) does, with a recorder made by `config`:
    /// started on the engine before its graph is made, it takes its folder's lock and recovers the
    /// folder; each of [`armed_tracks`](Self::armed_tracks) is armed with a tap of it, so that the
    /// graph's commit carries them and the first play records.
    pub(crate) fn build_recording(
        &self,
        mode: ReaderMode,
        config: RecordConfig,
    ) -> Result<(BuiltSession, Recorder), BuildError> {
        self.build_then(mode, |controller| {
            let mut recorder = Recorder::start(config, controller.stats())?;
            for track in self.armed_tracks() {
                let tap = recorder.tap(track)?;
                controller.arm(tap)?;
            }
            Ok(recorder)
        })
    }

    /// Builds the session, `before_the_graph` given the controller once the engine is made and
    /// before any node is: its edits go out with the graph's commit.
    fn build_then<T>(
        &self,
        mode: ReaderMode,
        before_the_graph: impl FnOnce(&mut Controller) -> Result<T, BuildError>,
    ) -> Result<(BuiltSession, T), BuildError> {
        let config = EngineConfig::new(
            self.rate,
            LARGEST_BLOCK,
            usize::from(self.capture_channels),
            usize::from(self.master_channels),
        );
        let (mut controller, processor) = engine::new(config)?;
        let made = before_the_graph(&mut controller)?;
        // The audio tracks in the session's order, with their strips: the reader makes one player
        // for each, in that order, so that zipping the players with the strips pairs them.
        let (audio_tracks, audio_strips): (Vec<AudioTrack>, Vec<&Strip>) = self
            .tracks
            .iter()
            .zip(&self.routing.tracks)
            .filter_map(|(track, strip)| match &track.source {
                LoadedSource::Audio(audio) => Some((audio.clone(), strip)),
                LoadedSource::Midi { .. } | LoadedSource::Input { .. } => None,
            })
            .unzip();
        let disk = DiskConfig::new(self.rate);
        let (reader, players) = match mode {
            ReaderMode::Thread => DiskReader::start(disk, &audio_tracks, controller.stats())?,
            ReaderMode::Offline => DiskReader::offline(disk, &audio_tracks, controller.stats())?,
        };

        let mut graph = Graph {
            controller: &mut controller,
        };
        let master = graph.master(self.master_channels, &self.master_inserts)?;
        let mut buses = Vec::with_capacity(self.routing.buses.len());
        for strip in &self.routing.buses {
            buses.push(graph.chain(strip)?);
        }
        let inputs = Inputs {
            master,
            buses: buses
                .iter()
                .zip(&self.routing.buses)
                .map(|(chain, strip)| Input::Node(chain.input, strip.channels))
                .collect(),
        };
        for (strip, chain) in self.routing.buses.iter().zip(&buses) {
            graph.feed(chain.output, strip.channels, inputs.of(strip.output)?);
        }
        for (player, strip) in players.into_iter().zip(audio_strips) {
            let source = graph.add(player)?;
            graph.track(Feed::Node(source), strip, &inputs)?;
        }
        for (track, strip) in self.tracks.iter().zip(&self.routing.tracks) {
            let LoadedSource::Midi {
                instrument,
                sequence,
            } = &track.source
            else {
                continue;
            };
            let source = match instrument {
                Instrument::TestSynth {} => graph.add(TestSynth::new(self.rate))?,
            };
            graph
                .controller
                .set_sequences(source, vec![Arc::clone(sequence)])?;
            graph.track(Feed::Node(source), strip, &inputs)?;
        }
        for (track, strip) in self.tracks.iter().zip(&self.routing.tracks) {
            if let LoadedSource::Input { capture, .. } = &track.source {
                graph.track(Feed::Capture(capture), strip, &inputs)?;
            }
        }
        controller.set_tempo_map(self.tempo_map.clone());
        controller.commit()?;
        let built = BuiltSession {
            controller,
            processor,
            reader,
            last_sample: self.last_sample,
        };
        Ok((built, made))
    }
}

/// Where a signal goes into a strip, and the strip's channels.
#[derive(Clone, Copy, Debug)]
enum Input {
    /// A node's inputs.
    Node(NodeId, u16),
    /// The playback channels.
    Playback(u16),
}

/// What feeds a track's chain.
#[derive(Clone, Copy, Debug)]
enum Feed<'a> {
    /// Its source's outputs: an audio track's player, a MIDI track's instrument.
    Node(NodeId),
    /// An input track's capture channels, one per channel of the track.
    Capture(&'a [usize]),
}

/// A chain of nodes: the first, which takes what feeds the strip, and the last, its gain.
#[derive(Clone, Copy, Debug)]
struct Chain {
    input: NodeId,
    output: NodeId,
}

/// The inputs of the master and the buses, by which outputs and sends name them.
struct Inputs {
    master: Input,
    /// Each bus's input, in the session's order.
    buses: Vec<Input>,
}

impl Inputs {
    fn of(&self, target: Target) -> Result<Input, BuildError> {
        match target {
            Target::Master => Ok(self.master),
            Target::Bus(index) => self
                .buses
                .get(index)
                .copied()
                .ok_or(BuildError::UnknownBus(index)),
        }
    }
}

/// The graph being built, through the controller.
struct Graph<'a> {
    controller: &'a mut Controller,
}

impl Graph<'_> {
    fn add(&mut self, node: impl Node + 'static) -> Result<NodeId, GraphError> {
        self.controller.add_node(Box::new(node))
    }

    /// A track's chain after what `feeds` it, into its output's input, and from after its gain
    /// one gain per send into the send's input.
    fn track(&mut self, feeds: Feed<'_>, strip: &Strip, inputs: &Inputs) -> Result<(), BuildError> {
        let chain = self.chain(strip)?;
        match feeds {
            Feed::Node(source) => self.feed(
                source,
                strip.channels,
                Input::Node(chain.input, strip.channels),
            ),
            Feed::Capture(capture) => {
                for (channel, &captured) in capture.iter().enumerate() {
                    self.controller.connect(
                        Source::Capture(captured),
                        Destination::Node(chain.input, channel),
                    );
                }
            }
        }
        self.feed(chain.output, strip.channels, inputs.of(strip.output)?);
        for &(to, gain) in &strip.sends {
            let send = self.add(Gain::new(usize::from(strip.channels), gain))?;
            self.feed(
                chain.output,
                strip.channels,
                Input::Node(send, strip.channels),
            );
            self.feed(send, strip.channels, inputs.of(to)?);
        }
        Ok(())
    }

    /// The master's inserts in series into the playback channels, and the input that feeds them.
    fn master(&mut self, channels: u16, inserts: &[Insert]) -> Result<Input, GraphError> {
        let mut nodes = Vec::with_capacity(inserts.len());
        for insert in inserts {
            nodes.push(self.insert(channels, *insert)?);
        }
        self.series(&nodes, channels);
        Ok(match (nodes.first(), nodes.last()) {
            (Some(&first), Some(&last)) => {
                self.feed(last, channels, Input::Playback(channels));
                Input::Node(first, channels)
            }
            _ => Input::Playback(channels),
        })
    }

    /// A strip's inserts in series, then its gain.
    fn chain(&mut self, strip: &Strip) -> Result<Chain, GraphError> {
        let mut nodes = Vec::with_capacity(strip.inserts.len().saturating_add(1));
        for insert in &strip.inserts {
            nodes.push(self.insert(strip.channels, *insert)?);
        }
        let gain = self.add(Gain::new(usize::from(strip.channels), strip.gain))?;
        nodes.push(gain);
        self.series(&nodes, strip.channels);
        Ok(Chain {
            input: nodes.first().copied().unwrap_or(gain),
            output: gain,
        })
    }

    fn insert(&mut self, channels: u16, insert: Insert) -> Result<NodeId, GraphError> {
        let channels = usize::from(channels);
        match insert {
            Insert::TestLatency { samples } => self.add(TestLatency::new(channels, samples)),
            Insert::Gain { value } => self.add(Gain::new(channels, value)),
        }
    }

    /// Connects each node to the next, channel to channel.
    fn series(&mut self, nodes: &[NodeId], channels: u16) {
        for pair in nodes.windows(2) {
            if let [from, to] = *pair {
                self.feed(from, channels, Input::Node(to, channels));
            }
        }
    }

    /// Connects `from`'s `channels` outputs to `to`: channel to channel, or one channel to every
    /// channel of `to`.
    fn feed(&mut self, from: NodeId, channels: u16, to: Input) {
        let to_channels = match to {
            Input::Node(_, channels) | Input::Playback(channels) => channels,
        };
        let destination = |channel: u16| match to {
            Input::Node(node, _) => Destination::Node(node, usize::from(channel)),
            Input::Playback(_) => Destination::Playback(usize::from(channel)),
        };
        for to_channel in 0..to_channels {
            let from_channel = if channels == 1 { 0 } else { to_channel };
            if from_channel < channels {
                self.controller.connect(
                    Source::Node(from, usize::from(from_channel)),
                    destination(to_channel),
                );
            }
        }
    }
}

/// Why a session could not be built into an engine.
#[derive(Debug)]
pub enum BuildError {
    /// The engine could not be made.
    Engine(EngineError),
    /// The disk reader could not be made.
    Disk(DiskError),
    /// The graph refused a node or an edge: the arena has no slot left for a session this large.
    Graph(GraphError),
    /// An output or a send names a bus, by its index, that the session does not hold: a fault of
    /// the loader, which resolves names to the session's own buses.
    UnknownBus(usize),
    /// The recorder could not start, or make an armed track's tap: its folder is held by another
    /// recorder, cannot be used, or a take's file cannot be made.
    Record(RecordError),
    /// The engine refused to arm a track: a fault of the loader, which gives each armed track
    /// capture channels the engine has, once.
    Arm(ArmError),
}

impl From<RecordError> for BuildError {
    fn from(error: RecordError) -> Self {
        Self::Record(error)
    }
}

impl From<ArmError> for BuildError {
    fn from(error: ArmError) -> Self {
        Self::Arm(error)
    }
}

impl From<EngineError> for BuildError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<DiskError> for BuildError {
    fn from(error: DiskError) -> Self {
        Self::Disk(error)
    }
}

impl From<GraphError> for BuildError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(error) => write!(formatter, "the engine could not be made: {error}"),
            Self::Disk(error) => write!(formatter, "the disk reader could not be made: {error}"),
            Self::Graph(error) => write!(formatter, "the session's graph was refused: {error}"),
            Self::UnknownBus(index) => write!(
                formatter,
                "the session's routing names bus {index}, which the session does not hold"
            ),
            Self::Record(error) => write!(formatter, "the recorder could not start: {error}"),
            Self::Arm(error) => write!(formatter, "a track could not be armed: {error}"),
        }
    }
}

impl Error for BuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Engine(error) => Some(error),
            Self::Disk(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::Record(error) => Some(error),
            Self::Arm(error) => Some(error),
            Self::UnknownBus(_) => None,
        }
    }
}
