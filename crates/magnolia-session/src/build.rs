//! Building a session into an engine: [`Session::build`] makes the engine, the disk reader and the
//! graph of nodes that plays the session.

use std::error::Error;
use std::fmt;

use magnolia_engine::disk::{AudioTrack, DiskConfig, DiskError, DiskReader};
use magnolia_engine::engine::{
    self, Controller, Destination, EngineConfig, EngineError, GraphError, NodeId, Processor, Source,
};
use magnolia_engine::node::Node;
use magnolia_engine::nodes::{Gain, TestLatency, TestSynth};
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
    /// - the engine at the session's rate, its playback channels the master's, without capture,
    ///   its largest block [`LARGEST_BLOCK`];
    /// - the disk reader of its audio tracks, reading 10 seconds ahead, on its thread or not;
    /// - for each audio track its player, for each MIDI track its instrument playing its sequence;
    ///   then the track's inserts in series, its gain, into its output's input, and from after its
    ///   gain one gain per send into the send's destination's input;
    /// - for each bus, its inserts in series and its gain, into its output's input;
    /// - for the master, its inserts in series into the playback channels.
    ///
    /// What feeds a bus or the master feeds its input: its first insert, else its gain, or for the
    /// master the playback channels. A mono source feeds every channel of a stereo destination.
    /// It may be called again: each engine reads the same open files.
    ///
    /// # Errors
    ///
    /// A [`BuildError`] when the engine, the disk reader or the graph refuses what the session
    /// asks; after the checks of [`Session::open`], only a thread that does not start, or a
    /// session of more nodes than the engine's arena holds.
    pub fn build(&self, mode: ReaderMode) -> Result<BuiltSession, BuildError> {
        let config = EngineConfig::new(
            self.rate,
            LARGEST_BLOCK,
            0,
            usize::from(self.master_channels),
        );
        let (mut controller, processor) = engine::new(config)?;
        let audio_tracks: Vec<AudioTrack> = self
            .tracks
            .iter()
            .filter_map(|track| match &track.source {
                LoadedSource::Audio(audio) => Some(audio.clone()),
                LoadedSource::Midi { .. } => None,
            })
            .collect();
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
            graph.feed(chain.output, strip.channels, inputs.of(strip.output));
        }
        let mut players = players.into_iter();
        for (track, strip) in self.tracks.iter().zip(&self.routing.tracks) {
            let source = match &track.source {
                LoadedSource::Audio(_) => {
                    let player = players
                        .next()
                        .expect("the disk reader makes one player per audio track, in order");
                    graph.add(player)?
                }
                LoadedSource::Midi {
                    instrument,
                    sequence,
                } => {
                    let node = match instrument {
                        Instrument::TestSynth {} => graph.add(TestSynth::new(self.rate))?,
                    };
                    graph
                        .controller
                        .set_sequences(node, vec![sequence.clone()])?;
                    node
                }
            };
            let chain = graph.chain(strip)?;
            graph.feed(
                source,
                strip.channels,
                Input::Node(chain.input, strip.channels),
            );
            graph.feed(chain.output, strip.channels, inputs.of(strip.output));
            for &(to, gain) in &strip.sends {
                let send = graph.add(Gain::new(usize::from(strip.channels), gain))?;
                graph.feed(
                    chain.output,
                    strip.channels,
                    Input::Node(send, strip.channels),
                );
                graph.feed(send, strip.channels, inputs.of(to));
            }
        }
        controller.set_tempo_map(self.tempo_map.clone());
        controller.commit()?;
        Ok(BuiltSession {
            controller,
            processor,
            reader,
            last_sample: self.last_sample,
        })
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
    fn of(&self, target: Target) -> Input {
        match target {
            Target::Master => self.master,
            // The routing resolves names to the session's own buses, from the same list.
            Target::Bus(index) => self.buses[index],
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
        }
    }
}

impl Error for BuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Engine(error) => Some(error),
            Self::Disk(error) => Some(error),
            Self::Graph(error) => Some(error),
        }
    }
}
