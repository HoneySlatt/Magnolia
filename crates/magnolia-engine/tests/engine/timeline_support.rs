//! What the transport's and the timeline sources' tests share: tempo maps, clips and sequences, a
//! node that records what it hears, renders through the synth, and pseudo-random numbers from a
//! fixed seed.

use std::sync::Arc;

use magnolia_engine::engine::{
    ControllerPoint, MidiClip, MidiNote, MidiSequence, NodeId, PitchBendPoint,
};
use magnolia_engine::node::{Event, Node, ProcessContext, ProcessStatus, Transport};
use magnolia_engine::time::{
    Meter, MusicalPosition, SamplePosition, TICKS_PER_QUARTER, Tempo, TempoMap, TimePosition, place,
};

use crate::support::{Rig, Shared, rate, read, shared};

/// A quarter note, in ticks.
pub const QUARTER: i64 = TICKS_PER_QUARTER;

/// 120 quarters a minute from the timeline's zero: a quarter lasts 24,000 samples at 48 kHz.
pub fn steady_tempo() -> Arc<TempoMap> {
    let tempo = Tempo::from_bpm(120, 1).unwrap();
    Arc::new(TempoMap::constant(TimePosition(0), tempo, Meter::new(4, 4).unwrap()).unwrap())
}

/// 120 quarters a minute, then 93.5 from the fourth quarter: a tempo whose quarter is not a whole
/// number of samples.
pub fn changing_tempo() -> Arc<TempoMap> {
    let changes = [
        (MusicalPosition(0), Tempo::from_bpm(120, 1).unwrap()),
        (
            MusicalPosition(4 * QUARTER),
            Tempo::from_bpm(187, 2).unwrap(),
        ),
    ];
    Arc::new(TempoMap::new(TimePosition(0), &changes, &[(0, Meter::new(4, 4).unwrap())]).unwrap())
}

/// The sample a tick plays at.
pub fn sample_of(map: &TempoMap, tick: i64) -> i64 {
    place(MusicalPosition(tick), map, &rate()).unwrap().0
}

/// An empty clip from `start` for `length` ticks.
pub fn clip(start: i64, length: i64) -> MidiClip {
    MidiClip {
        start: MusicalPosition(start),
        length,
        notes: Vec::new(),
        pitch_bends: Vec::new(),
        controllers: Vec::new(),
    }
}

/// A note on channel 0 at velocity 0.8.
pub fn note(tick: i64, length: i64, key: u8) -> MidiNote {
    MidiNote {
        tick,
        length,
        channel: 0,
        key,
        velocity: 0.8,
    }
}

pub fn bend(tick: i64, channel: u8, value: f32) -> PitchBendPoint {
    PitchBendPoint {
        tick,
        channel,
        value,
    }
}

pub fn controller(tick: i64, channel: u8, number: u8, value: f32) -> ControllerPoint {
    ControllerPoint {
        tick,
        channel,
        number,
        value,
    }
}

/// One clip from tick 0, long enough for everything, holding `notes`.
pub fn notes(notes: Vec<MidiNote>) -> Arc<MidiSequence> {
    sequence(vec![MidiClip {
        notes,
        ..clip(0, 1_000 * QUARTER)
    }])
}

pub fn sequence(clips: Vec<MidiClip>) -> Arc<MidiSequence> {
    Arc::new(MidiSequence::new(&clips).unwrap())
}

/// What a listener heard in one call.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub frames: usize,
    pub transport: Transport,
    pub events: Vec<Event>,
}

impl Heard {
    /// The timeline sample of an event at the node's input.
    pub fn sample_of(&self, event: &Event) -> i64 {
        self.transport.position.0 + i64::from(event.offset)
    }
}

/// A node without input and with one silent output, which records each call: the transport it
/// saw, and its events. It never sleeps, so it hears every sub-block.
pub struct Listener {
    heard: Shared<Vec<Heard>>,
}

impl Listener {
    pub fn new() -> (Self, Shared<Vec<Heard>>) {
        let heard = shared(Vec::new());
        (
            Self {
                heard: Arc::clone(&heard),
            },
            heard,
        )
    }
}

impl Node for Listener {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        read(&self.heard).push(Heard {
            frames: context.frames(),
            transport: context.transport(),
            events: context.events().to_vec(),
        });
        context.output_mut(0).fill(0.0);
        context.set_output_silent(0, true);
        ProcessStatus::Continue
    }
}

/// Every event a listener heard, with its timeline sample at the node's input.
pub fn events_heard(heard: &Shared<Vec<Heard>>) -> Vec<(i64, Event)> {
    read(heard)
        .iter()
        .flat_map(|call| {
            call.events
                .iter()
                .map(|event| (call.sample_of(event), *event))
        })
        .collect()
}

/// Plays periods on every playback channel, polling after each, and returns what each played.
pub fn play_channels(rig: &mut Rig, periods: &[usize]) -> Vec<Vec<f32>> {
    let mut played: Vec<Vec<f32>> = Vec::new();
    for &frames in periods {
        let period = rig.backend.play_period(frames, &[]);
        played.resize(period.len(), Vec::new());
        for (all, channel) in played.iter_mut().zip(period) {
            all.extend_from_slice(channel);
        }
        rig.controller.poll();
    }
    played
}

/// Gives a node its sequences, as one commit sends them.
pub fn give(rig: &mut Rig, node: NodeId, sequences: Vec<Arc<MidiSequence>>) {
    rig.controller.set_sequences(node, sequences).unwrap();
}

/// Where the last play started, by the engine's statistics.
pub fn audible_at(rig: &Rig) -> (u64, i64) {
    let start = rig.controller.stats().play_start().unwrap();
    (start.sequence, start.audible_at.0)
}

/// The first sample of `samples` that is not +0.0, if any.
pub fn first_sound(samples: &[f32]) -> Option<usize> {
    samples
        .iter()
        .position(|&sample| sample.to_bits() != 0.0_f32.to_bits())
}

/// A play from a sample.
pub fn play_from(rig: &mut Rig, from: i64) -> u64 {
    rig.controller.play(SamplePosition(from)).unwrap()
}

/// A generator of pseudo-random numbers from a fixed seed (xorshift64*), so that every run cuts
/// the same periods.
pub struct Random(pub u64);

impl Random {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number from 1 to `most`.
    pub fn up_to(&mut self, most: usize) -> usize {
        1 + (self.next() % most as u64) as usize
    }
}
