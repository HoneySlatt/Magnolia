//! MIDI sequences: one track's clips, flattened into events sorted by tick, as the processor reads
//! them for each sub-block
//! ([ADR-0027](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0027-timeline-events-from-sources-on-nodes.md)).
//!
//! A sequence is built off the real-time thread and never changes; the controller attaches it to a
//! node, and the compiled graph carries it to the processor, which only reads it. Its lookups are
//! binary searches that neither allocate nor lock.

// The processor reads sequences in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::node::{EventKind, Note};
use crate::time::MusicalPosition;

/// A MIDI clip: notes, pitch bends and controller changes, laid on the timeline from its start.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiClip {
    /// Where the clip starts, in ticks of the tempo map.
    pub start: MusicalPosition,
    /// How long it lasts, in ticks. Nothing at or past its end plays; a note held past it ends
    /// there.
    pub length: i64,
    /// Its notes, in any order.
    pub notes: Vec<MidiNote>,
    /// Its pitch bends, in any order.
    pub pitch_bends: Vec<PitchBendPoint>,
    /// Its controllers' changes, in any order.
    pub controllers: Vec<ControllerPoint>,
}

/// A note of a clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiNote {
    /// Where it starts, in ticks from its clip's start, from 0.
    pub tick: i64,
    /// How long it lasts, in ticks: at least 1.
    pub length: i64,
    /// The MIDI channel, from 0 to 15.
    pub channel: u8,
    /// The key, from 0 to 127.
    pub key: u8,
    /// How hard it is struck, from 0 to 1.
    pub velocity: f32,
}

/// A pitch bend of a clip: the wheel moves there and stays until the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchBendPoint {
    /// Where, in ticks from its clip's start, from 0.
    pub tick: i64,
    /// The MIDI channel, from 0 to 15.
    pub channel: u8,
    /// The wheel's position, from -1 to 1.
    pub value: f32,
}

/// A controller's change in a clip: the controller moves there and stays until the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControllerPoint {
    /// Where, in ticks from its clip's start, from 0.
    pub tick: i64,
    /// The MIDI channel, from 0 to 15.
    pub channel: u8,
    /// The controller's number, from 0 to 127, such as 64 for the sustain pedal.
    pub number: u8,
    /// Its position, from 0 to 1.
    pub value: f32,
}

/// Why a [`MidiSequence`] could not be built from clips. Each names the clip, by its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceError {
    /// The clip's length is negative.
    NegativeClipLength {
        /// The clip's index.
        clip: usize,
    },
    /// A note, a bend or a controller's change of the clip has a negative tick: it lies before the
    /// clip's start.
    BeforeClipStart {
        /// The clip's index.
        clip: usize,
    },
    /// A note of the clip lasts no tick.
    EmptyNote {
        /// The clip's index.
        clip: usize,
    },
    /// A channel above 15, or a key or a controller's number above 127.
    MidiNumberOutOfRange {
        /// The clip's index.
        clip: usize,
    },
    /// A velocity or a controller's value outside 0 to 1, or a bend outside -1 to 1.
    ValueOutOfRange {
        /// The clip's index.
        clip: usize,
    },
    /// The clip, or something in it, lies beyond the ticks' range.
    TickOutOfRange {
        /// The clip's index.
        clip: usize,
    },
}

impl fmt::Display for SequenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeClipLength { clip } => {
                write!(formatter, "clip {clip} has a negative length")
            }
            Self::BeforeClipStart { clip } => {
                write!(formatter, "clip {clip} holds an event before its start")
            }
            Self::EmptyNote { clip } => write!(formatter, "clip {clip} holds a note of no length"),
            Self::MidiNumberOutOfRange { clip } => write!(
                formatter,
                "clip {clip} holds a channel above 15, or a key or a controller above 127"
            ),
            Self::ValueOutOfRange { clip } => write!(
                formatter,
                "clip {clip} holds a velocity, a bend or a controller's value out of its range"
            ),
            Self::TickOutOfRange { clip } => {
                write!(formatter, "clip {clip} lies beyond the ticks' range")
            }
        }
    }
}

impl Error for SequenceError {}

/// One track's MIDI clips, flattened into one list of events sorted by tick, and the pitch bends
/// and controllers' values that a play chases at its start.
///
/// Each note becomes a note on at its start and a note off at its end or at its clip's end,
/// whichever comes first; a note that starts at or past its clip's end, and a bend or a
/// controller's change there, do not play. At one tick, the note offs come first, so that a note
/// ended and started again there sounds; then the bends and controllers' changes, so that a note
/// starts with them; then the note ons; each group in the order of the clips, then of their lists.
///
/// It also keeps, per channel, its pitch bends and, per channel and controller, the controller's
/// changes, sorted by tick: at a play's start, each sends the last value before the start
/// (ADR-0027). Built off the real-time thread; immutable once built.
#[derive(Debug)]
pub struct MidiSequence {
    events: Box<[SequenceEvent]>,
    lanes: Box<[Lane]>,
    /// One bit per MIDI channel that some event plays on.
    channels: u16,
}

/// One event of a sequence, at its tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SequenceEvent {
    pub(crate) tick: i64,
    pub(crate) kind: EventKind,
    /// For a note on, its note off's tick; for a note off, its note on's tick; otherwise the
    /// event's own tick.
    pub(crate) paired_tick: i64,
}

/// One channel's pitch bends, or one controller's changes on one channel, sorted by tick.
#[derive(Debug)]
pub(crate) struct Lane {
    kind: LaneKind,
    points: Box<[LanePoint]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LaneKind {
    PitchBend { channel: u8 },
    Controller { channel: u8, number: u8 },
}

#[derive(Clone, Copy, Debug)]
struct LanePoint {
    tick: i64,
    value: f32,
}

impl Lane {
    /// The lane's last value before `tick`, as the event that sets it; `None` if the lane has
    /// none. Of several at one tick, the last one listed.
    pub(crate) fn last_before(&self, tick: i64) -> Option<EventKind> {
        let before = self.points.partition_point(|point| point.tick < tick);
        let point = before
            .checked_sub(1)
            .and_then(|index| self.points.get(index))?;
        Some(match self.kind {
            LaneKind::PitchBend { channel } => EventKind::PitchBend {
                channel,
                value: point.value,
            },
            LaneKind::Controller { channel, number } => EventKind::Controller {
                channel,
                number,
                value: point.value,
            },
        })
    }
}

/// Where an event goes among the events of one tick: note offs, then bends and controllers'
/// changes, then note ons.
fn rank_at_one_tick(kind: EventKind) -> u8 {
    match kind {
        EventKind::NoteOff(_) => 0,
        EventKind::PitchBend { .. }
        | EventKind::Controller { .. }
        | EventKind::Parameter { .. } => 1,
        EventKind::NoteOn(_) => 2,
    }
}

impl MidiSequence {
    /// The sequence of `clips`, or why they cannot make one.
    pub fn new(clips: &[MidiClip]) -> Result<MidiSequence, SequenceError> {
        let mut events = Vec::new();
        let mut lanes: BTreeMap<LaneKind, Vec<LanePoint>> = BTreeMap::new();
        let mut channels = 0_u16;
        for (index, clip) in clips.iter().enumerate() {
            let in_clip = ClipTicks::new(clip, index)?;
            for note in &clip.notes {
                in_clip.check_tick(note.tick)?;
                if note.length < 1 {
                    return Err(SequenceError::EmptyNote { clip: index });
                }
                in_clip.check_channel(note.channel)?;
                in_clip.check_number(note.key)?;
                in_clip.check_value(note.velocity, 0.0)?;
                if note.tick >= clip.length {
                    continue;
                }
                let end = note.tick.saturating_add(note.length).min(clip.length);
                let (on, off) = (in_clip.absolute(note.tick)?, in_clip.absolute(end)?);
                let played = Note {
                    id: None,
                    channel: note.channel,
                    key: note.key,
                    velocity: note.velocity,
                };
                events.push(SequenceEvent {
                    tick: on,
                    kind: EventKind::NoteOn(played),
                    paired_tick: off,
                });
                events.push(SequenceEvent {
                    tick: off,
                    kind: EventKind::NoteOff(Note {
                        velocity: 0.0,
                        ..played
                    }),
                    paired_tick: on,
                });
                channels |= channel_bit(note.channel);
            }
            for bend in &clip.pitch_bends {
                in_clip.check_tick(bend.tick)?;
                in_clip.check_channel(bend.channel)?;
                in_clip.check_value(bend.value, -1.0)?;
                if bend.tick >= clip.length {
                    continue;
                }
                let tick = in_clip.absolute(bend.tick)?;
                let channel = bend.channel;
                events.push(SequenceEvent {
                    tick,
                    kind: EventKind::PitchBend {
                        channel,
                        value: bend.value,
                    },
                    paired_tick: tick,
                });
                lanes
                    .entry(LaneKind::PitchBend { channel })
                    .or_default()
                    .push(LanePoint {
                        tick,
                        value: bend.value,
                    });
                channels |= channel_bit(channel);
            }
            for change in &clip.controllers {
                in_clip.check_tick(change.tick)?;
                in_clip.check_channel(change.channel)?;
                in_clip.check_number(change.number)?;
                in_clip.check_value(change.value, 0.0)?;
                if change.tick >= clip.length {
                    continue;
                }
                let tick = in_clip.absolute(change.tick)?;
                let (channel, number) = (change.channel, change.number);
                events.push(SequenceEvent {
                    tick,
                    kind: EventKind::Controller {
                        channel,
                        number,
                        value: change.value,
                    },
                    paired_tick: tick,
                });
                lanes
                    .entry(LaneKind::Controller { channel, number })
                    .or_default()
                    .push(LanePoint {
                        tick,
                        value: change.value,
                    });
                channels |= channel_bit(channel);
            }
        }
        // Stable sorts: at one tick and rank, the order of the clips and their lists stays.
        events.sort_by_key(|event| (event.tick, rank_at_one_tick(event.kind)));
        let lanes = lanes
            .into_iter()
            .map(|(kind, mut points)| {
                points.sort_by_key(|point| point.tick);
                Lane {
                    kind,
                    points: points.into(),
                }
            })
            .collect();
        Ok(MidiSequence {
            events: events.into(),
            lanes,
            channels,
        })
    }

    /// The events whose tick lies from `start` up to `end`, in order.
    pub(crate) fn events_between(&self, start: i64, end: i64) -> &[SequenceEvent] {
        let first = self.events.partition_point(|event| event.tick < start);
        let last = self.events.partition_point(|event| event.tick < end);
        self.events.get(first..last).unwrap_or(&[])
    }

    /// Each channel's pitch bends and each controller's changes, for the chase.
    pub(crate) fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    /// One bit per MIDI channel that some event plays on: bit 0 for channel 0.
    pub(crate) fn channels(&self) -> u16 {
        self.channels
    }
}

/// A channel's bit among 16; none for a channel above 15.
pub(crate) fn channel_bit(channel: u8) -> u16 {
    1_u16.checked_shl(u32::from(channel)).unwrap_or(0)
}

/// One clip's ticks, checked: its start and length, and its index for the errors.
struct ClipTicks {
    start: i64,
    index: usize,
}

impl ClipTicks {
    fn new(clip: &MidiClip, index: usize) -> Result<Self, SequenceError> {
        if clip.length < 0 {
            return Err(SequenceError::NegativeClipLength { clip: index });
        }
        let clip_ticks = Self {
            start: clip.start.0,
            index,
        };
        clip_ticks.absolute(clip.length)?;
        Ok(clip_ticks)
    }

    /// The tick on the tempo map of a tick from the clip's start.
    fn absolute(&self, tick: i64) -> Result<i64, SequenceError> {
        self.start
            .checked_add(tick)
            .ok_or(SequenceError::TickOutOfRange { clip: self.index })
    }

    fn check_tick(&self, tick: i64) -> Result<(), SequenceError> {
        if tick < 0 {
            return Err(SequenceError::BeforeClipStart { clip: self.index });
        }
        Ok(())
    }

    fn check_channel(&self, channel: u8) -> Result<(), SequenceError> {
        if channel > 15 {
            return Err(SequenceError::MidiNumberOutOfRange { clip: self.index });
        }
        Ok(())
    }

    fn check_number(&self, number: u8) -> Result<(), SequenceError> {
        if number > 127 {
            return Err(SequenceError::MidiNumberOutOfRange { clip: self.index });
        }
        Ok(())
    }

    /// Checks a value from `lowest` to 1; a value that is not a number is out of range.
    fn check_value(&self, value: f32, lowest: f32) -> Result<(), SequenceError> {
        if !(lowest..=1.0).contains(&value) {
            return Err(SequenceError::ValueOutOfRange { clip: self.index });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    /// An empty clip.
    fn clip(start: i64, length: i64) -> MidiClip {
        MidiClip {
            start: MusicalPosition(start),
            length,
            notes: Vec::new(),
            pitch_bends: Vec::new(),
            controllers: Vec::new(),
        }
    }

    fn note(tick: i64, length: i64, key: u8) -> MidiNote {
        MidiNote {
            tick,
            length,
            channel: 0,
            key,
            velocity: 0.5,
        }
    }

    fn kinds(sequence: &MidiSequence) -> Vec<(i64, &'static str, u8)> {
        sequence
            .events_between(i64::MIN, i64::MAX)
            .iter()
            .map(|event| match event.kind {
                EventKind::NoteOn(note) => (event.tick, "on", note.key),
                EventKind::NoteOff(note) => (event.tick, "off", note.key),
                EventKind::PitchBend { .. } => (event.tick, "bend", 0),
                EventKind::Controller { number, .. } => (event.tick, "controller", number),
                EventKind::Parameter { .. } => (event.tick, "parameter", 0),
            })
            .collect()
    }

    #[test]
    fn notes_end_at_their_clips_end_and_note_offs_come_first_at_one_tick()
    -> Result<(), Box<dyn Error>> {
        let clip = MidiClip {
            notes: vec![
                note(50, 50, 61),
                note(0, 50, 60),
                note(90, i64::MAX, 62),
                note(100, 5, 63),
            ],
            pitch_bends: vec![PitchBendPoint {
                tick: 50,
                channel: 3,
                value: -1.0,
            }],
            controllers: vec![ControllerPoint {
                tick: 100,
                channel: 0,
                number: 7,
                value: 1.0,
            }],
            ..clip(1_000, 100)
        };
        let sequence = MidiSequence::new(&[clip])?;
        assert_eq!(
            kinds(&sequence),
            [
                (1_000, "on", 60),
                (1_050, "off", 60),
                (1_050, "bend", 0),
                (1_050, "on", 61),
                (1_090, "on", 62),
                (1_100, "off", 61),
                (1_100, "off", 62),
            ]
        );
        // Note 62 ends at the clip's end; its note off knows its note on's tick.
        let off = sequence
            .events_between(1_100, 1_101)
            .get(1)
            .ok_or("two note offs at the clip's end")?;
        assert_eq!(off.paired_tick, 1_090);
        assert_eq!(sequence.channels(), 0b1001);
        Ok(())
    }

    #[test]
    fn the_chase_finds_the_last_value_before_a_tick() -> Result<(), Box<dyn Error>> {
        let bend = |tick, value| PitchBendPoint {
            tick,
            channel: 2,
            value,
        };
        let clip = MidiClip {
            pitch_bends: vec![bend(500, 0.5), bend(100, -0.5), bend(500, 0.75)],
            ..clip(0, 1_000)
        };
        let sequence = MidiSequence::new(&[clip])?;
        let lane = sequence.lanes().first().ok_or("one lane")?;
        assert_eq!(lane.last_before(100), None);
        let bent = |value| Some(EventKind::PitchBend { channel: 2, value });
        assert_eq!(lane.last_before(101), bent(-0.5));
        assert_eq!(lane.last_before(500), bent(-0.5));
        // Two at one tick: the last one listed holds.
        assert_eq!(lane.last_before(501), bent(0.75));
        Ok(())
    }

    #[test]
    fn malformed_clips_are_refused_by_their_index() {
        let with_note = |note| MidiClip {
            notes: vec![note],
            ..clip(0, 100)
        };
        let cases = [
            (clip(0, -1), SequenceError::NegativeClipLength { clip: 1 }),
            (
                with_note(note(-1, 10, 60)),
                SequenceError::BeforeClipStart { clip: 1 },
            ),
            (
                with_note(note(0, 0, 60)),
                SequenceError::EmptyNote { clip: 1 },
            ),
            (
                with_note(note(0, 10, 128)),
                SequenceError::MidiNumberOutOfRange { clip: 1 },
            ),
            (
                with_note(MidiNote {
                    channel: 16,
                    ..note(0, 10, 60)
                }),
                SequenceError::MidiNumberOutOfRange { clip: 1 },
            ),
            (
                with_note(MidiNote {
                    velocity: f32::NAN,
                    ..note(0, 10, 60)
                }),
                SequenceError::ValueOutOfRange { clip: 1 },
            ),
            (clip(i64::MAX, 1), SequenceError::TickOutOfRange { clip: 1 }),
            (
                MidiClip {
                    pitch_bends: vec![PitchBendPoint {
                        tick: 0,
                        channel: 0,
                        value: 1.5,
                    }],
                    ..clip(0, 10)
                },
                SequenceError::ValueOutOfRange { clip: 1 },
            ),
        ];
        for (malformed, error) in cases {
            let refused = MidiSequence::new(&[clip(0, 0), malformed]).map(|_| ());
            assert_eq!(refused, Err(error));
        }
    }
}
