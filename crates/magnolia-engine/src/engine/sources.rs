//! Timeline sources: the MIDI sequences attached to nodes, read for each sub-block at each node's
//! own transport position, and merged with the controller's events
//! ([ADR-0027](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0027-timeline-events-from-sources-on-nodes.md)).
//!
//! The controller holds the sources being edited ([`TimelineSources`]); each commit copies them
//! into the compiled graph, which the processor plays. For a node with sequences, while the
//! transport rolls, the processor's [`SourceReader`] finds the ticks placed in the samples the
//! node's position covers (`TempoMap::ticks_in_block`, then `place`), keeps those in the audible
//! region, and gives each event its offset in the sub-block:
//!
//! - at the region's start, each sequence first sends the last pitch bend per channel and the last
//!   value per channel and controller before it: the chase;
//! - a note that began before the region's start does not play, nor does its note off; nor does a
//!   note shorter than a sample, whose note off would otherwise come first;
//! - a note off goes only to a note the node holds, in the order the node receives its events: at
//!   one sample the note offs come first, so a note ended and struck again there stays held;
//! - when the node's position reaches the region's stop, or passes it in one jump, as a swap that
//!   raises its compensation makes it, the node gets a note off for every note it holds, then the
//!   sustain pedal (controller 64) at 0 and the pitch bend at 0 on each channel its sequences use;
//!   once per play, after which its sources send nothing more until the next play.
//!
//! What the node's sources hold lives in its slot of the arena ([`SourceState`]), so that it
//! survives a swap: a stop ends the node's notes even if its sequences changed meanwhile.
//!
//! A node's events at one offset come note offs first, then the timeline's before the
//! controller's, then in the order they came. At most `event_capacity` events come from a node's
//! sources in one sub-block; past that they are dropped and counted, so a session that drops some
//! no longer renders the same for every cut of the periods.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use super::events::rank_at_one_sample;
use super::graph::NodeId;
use super::memory::written_vec;
use super::sequence::{MidiSequence, channel_bit};
use crate::node::{Event, EventKind, Note, Transport};
use crate::time::{MusicalPosition, SamplePosition, SampleRate, TempoMap, place, read};

/// The timeline sources the controller holds, which a commit sends with its graph: each node's
/// sequences, and the tempo map that lays their ticks on the timeline. Without a tempo map,
/// sequences play nothing, not even at a stop; once set, a map is only ever replaced.
#[derive(Clone, Debug, Default)]
pub(crate) struct TimelineSources {
    pub(crate) sequences: BTreeMap<NodeId, Vec<Arc<MidiSequence>>>,
    pub(crate) tempo_map: Option<Arc<TempoMap>>,
}

/// The MIDI channels a note can be on.
const CHANNELS: usize = 16;

/// A set of notes, one bit per channel and key.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KeySet {
    /// How many bits are set, so that an empty set costs one comparison.
    count: u16,
    keys: [u128; CHANNELS],
}

impl KeySet {
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn contains(&self, channel: u8, key: u8) -> bool {
        self.keys
            .get(usize::from(channel))
            .is_some_and(|keys| keys & key_bit(key) != 0)
    }

    fn insert(&mut self, channel: u8, key: u8) {
        let Some(keys) = self.keys.get_mut(usize::from(channel)) else {
            return;
        };
        let bit = key_bit(key);
        if *keys & bit == 0 {
            *keys |= bit;
            self.count = self.count.saturating_add(1);
        }
    }

    /// Takes a note out of the set; returns whether it was in.
    fn remove(&mut self, channel: u8, key: u8) -> bool {
        let Some(keys) = self.keys.get_mut(usize::from(channel)) else {
            return false;
        };
        let bit = key_bit(key);
        if *keys & bit == 0 {
            return false;
        }
        *keys &= !bit;
        self.count = self.count.saturating_sub(1);
        true
    }
}

/// A key's bit among a channel's 128; keys above 127 share the last.
fn key_bit(key: u8) -> u128 {
    1_u128.checked_shl(u32::from(key.min(127))).unwrap_or(0)
}

/// What a node's timeline sources keep from one sub-block to the next, in the node's slot of the
/// arena, so that it survives swaps.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SourceState {
    /// The notes its sources have started and not yet ended.
    held: KeySet,
    /// The last play whose stop the node has had; plays count from 1.
    stopped_play: u64,
}

/// An event a node's sources give it, before it is merged with the controller's.
#[derive(Clone, Copy, Debug)]
struct TimelineEvent {
    offset: u32,
    /// Note offs first at one offset.
    rank: u8,
    /// The order it was found in: chased values, then each sequence's events, then the stop's.
    order: u32,
    kind: EventKind,
}

/// What the processor knows of the timeline: the tempo map of the compiled graph it plays, and the
/// engine's rate.
#[derive(Clone, Copy)]
pub(crate) struct Timeline<'a> {
    pub(crate) tempo_map: Option<&'a TempoMap>,
    pub(crate) rate: SampleRate,
}

/// One node's timeline in one sub-block: its sequences, the transport at its input, and the
/// number of the play the transport plays.
#[derive(Clone, Copy)]
pub(crate) struct NodeTimeline<'a> {
    pub(crate) timeline: Timeline<'a>,
    pub(crate) sequences: &'a [Arc<MidiSequence>],
    pub(crate) transport: Transport,
    pub(crate) play: u64,
}

/// The processor's buffers for reading sources: the events found, and those merged with the
/// controller's. Every buffer is reserved and written beforehand.
pub(crate) struct SourceReader {
    found: Vec<TimelineEvent>,
    /// How many events a node's sources may give it in one sub-block.
    room: usize,
    merged: Vec<Event>,
    /// Events dropped for want of room, not yet counted in the statistics.
    dropped: u64,
}

impl SourceReader {
    /// A reader with room for `capacity` events from one node's sources in one sub-block, besides
    /// as many from the controller. Built off the real-time thread.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            found: written_vec(capacity),
            room: capacity,
            merged: written_vec(capacity.saturating_mul(2)),
            dropped: 0,
        }
    }

    /// How many events were dropped since the last call.
    pub(crate) fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    /// The events a node receives in a sub-block of `frames` frames: `controller`'s, the node's
    /// own from the controller, merged with those its sources play while the transport rolls.
    /// Updates what its sources hold.
    pub(crate) fn node_events<'a>(
        &'a mut self,
        controller: &'a [Event],
        node: NodeTimeline<'_>,
        state: &mut SourceState,
        frames: usize,
    ) -> &'a [Event] {
        let transport = node.transport;
        let nothing_to_read = node.sequences.is_empty() && state.held.is_empty();
        // After the stop's events, nothing more until the next play, even if a swap moves the
        // node's position back before the stop.
        if !transport.rolling || nothing_to_read || state.stopped_play == node.play {
            return controller;
        }
        // Without a tempo map nothing has played, so nothing is held either.
        let Some(map) = node.timeline.tempo_map else {
            return controller;
        };
        self.found.clear();
        let window = Window::new(transport, frames);
        let placer = Placer {
            map,
            rate: node.timeline.rate,
        };
        self.chase(&window, node.sequences, &placer);
        let ticks = window
            .audible_part()
            .and_then(|(start, frames)| map.ticks_in_block(placer.rate, start, frames));
        if let Some(ticks) = ticks {
            let maybe_held = may_be_held(&state.held, node.sequences, &ticks);
            for sequence in node.sequences {
                self.play(&window, sequence, &placer, &ticks, &maybe_held);
            }
        }
        self.found
            .sort_unstable_by_key(|event| (event.offset, event.rank, event.order));
        keep_in_order(&mut self.found, &mut state.held);
        self.end_at_stop(&window, node, state);
        if self.found.is_empty() {
            return controller;
        }
        merge(&self.found, controller, &mut self.merged);
        &self.merged
    }

    /// At the region's start, each sequence's last pitch bend per channel and last value per
    /// channel and controller before it.
    fn chase(&mut self, window: &Window, sequences: &[Arc<MidiSequence>], placer: &Placer<'_>) {
        let Some(offset) = window.offset_of(window.from) else {
            return;
        };
        // Every tick before this one is placed before the region's start.
        let Some(first_tick) = placer.first_tick_at_or_after(window.from) else {
            return;
        };
        for sequence in sequences {
            for lane in sequence.lanes() {
                if let Some(kind) = lane.last_before(first_tick) {
                    self.push(offset, kind);
                }
            }
        }
    }

    /// The events of `sequence` at `ticks`, those placed both in the sub-block and in the audible
    /// region. A note off is kept only if its note may be held: `may_be_held` holds the notes held
    /// at the sub-block's start and those struck in it, by any of the node's sequences.
    fn play(
        &mut self,
        window: &Window,
        sequence: &MidiSequence,
        placer: &Placer<'_>,
        ticks: &Range<MusicalPosition>,
        may_be_held: &KeySet,
    ) {
        for event in sequence.events_between(ticks.start.0, ticks.end.0) {
            let Some(sample) = placer.sample_of(event.tick) else {
                continue;
            };
            let Some(offset) = window.offset_of(sample) else {
                continue;
            };
            let plays = match event.kind {
                // A note shorter than a sample would end before it starts: note offs come first
                // at one sample.
                EventKind::NoteOn(_) => placer
                    .sample_of(event.paired_tick)
                    .is_some_and(|end| end > sample),
                EventKind::NoteOff(note) => {
                    let started = placer.sample_of(event.paired_tick);
                    let was_played = started.is_some_and(|on| on >= window.from && on < sample);
                    was_played && may_be_held.contains(note.channel, note.key)
                }
                EventKind::PitchBend { .. }
                | EventKind::Controller { .. }
                | EventKind::Parameter { .. } => true,
            };
            if plays {
                self.push(offset, event.kind);
            }
        }
    }

    /// Once the node's position reaches or has passed the region's stop, once per play: a note off
    /// for every held note, then the sustain pedal released and the pitch bend reset on each
    /// channel the sequences use. At the stop's offset, or at the sub-block's start when the
    /// position jumped past it.
    fn end_at_stop(&mut self, window: &Window, node: NodeTimeline<'_>, state: &mut SourceState) {
        let Some(stop) = window.stop.filter(|&stop| stop < window.end) else {
            return;
        };
        let offset = u32::try_from(stop.saturating_sub(window.start).max(0)).unwrap_or(0);
        state.stopped_play = node.play;
        for (channel, keys) in (0_u8..).zip(state.held.keys) {
            let mut left = keys;
            while left != 0 {
                let key = u8::try_from(left.trailing_zeros()).unwrap_or(u8::MAX);
                left &= left.wrapping_sub(1);
                let note = Note {
                    id: None,
                    channel,
                    key,
                    velocity: 0.0,
                };
                if self.push(offset, EventKind::NoteOff(note)) {
                    state.held.remove(channel, key);
                }
            }
        }
        let used = node
            .sequences
            .iter()
            .fold(0_u16, |used, sequence| used | sequence.channels());
        for channel in (0_u8..16).filter(|&channel| used & channel_bit(channel) != 0) {
            self.push(
                offset,
                EventKind::Controller {
                    channel,
                    number: SUSTAIN_PEDAL,
                    value: 0.0,
                },
            );
            self.push(
                offset,
                EventKind::PitchBend {
                    channel,
                    value: 0.0,
                },
            );
        }
    }

    /// Adds an event at `offset`; returns whether there was room, and counts it dropped if not.
    fn push(&mut self, offset: u32, kind: EventKind) -> bool {
        if self.found.len() >= self.room || self.found.spare_capacity_mut().is_empty() {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        let order = u32::try_from(self.found.len()).unwrap_or(u32::MAX);
        self.found.push(TimelineEvent {
            offset,
            rank: rank_at_one_sample(kind),
            order,
            kind,
        });
        true
    }
}

/// The notes that may be held during a sub-block: those held at its start, and those any of the
/// node's sequences strike at `ticks`. A note off for any other note is for a note never held, as
/// when its note on was dropped: it takes no room.
fn may_be_held(
    held: &KeySet,
    sequences: &[Arc<MidiSequence>],
    ticks: &Range<MusicalPosition>,
) -> KeySet {
    let mut notes = *held;
    for sequence in sequences {
        for event in sequence.events_between(ticks.start.0, ticks.end.0) {
            if let EventKind::NoteOn(note) = event.kind {
                notes.insert(note.channel, note.key);
            }
        }
    }
    notes
}

/// Walks the sorted events in the order the node receives them, updating the notes it holds: a
/// note on holds its note; a note off ends a held note, and is dropped, uncounted, for a note not
/// held, which never sounded.
fn keep_in_order(found: &mut Vec<TimelineEvent>, held: &mut KeySet) {
    found.retain(|event| match event.kind {
        EventKind::NoteOn(note) => {
            held.insert(note.channel, note.key);
            true
        }
        EventKind::NoteOff(note) => held.remove(note.channel, note.key),
        EventKind::PitchBend { .. }
        | EventKind::Controller { .. }
        | EventKind::Parameter { .. } => true,
    });
}

/// The sustain pedal's controller number.
const SUSTAIN_PEDAL: u8 = 64;

/// The samples a node's position covers in one sub-block, and the audible region.
struct Window {
    /// The first sample, at the node's input.
    start: i64,
    /// The sub-block's end, excluded.
    end: i64,
    from: i64,
    stop: Option<i64>,
}

impl Window {
    fn new(transport: Transport, frames: usize) -> Self {
        let start = transport.position.0;
        let frames = i64::try_from(frames).unwrap_or(i64::MAX);
        Self {
            start,
            end: start.saturating_add(frames),
            from: transport.audible.start.0,
            stop: transport.audible.stop.map(|stop| stop.0),
        }
    }

    /// A sample's offset in the sub-block, if it lies in it.
    fn offset_of(&self, sample: i64) -> Option<u32> {
        if !(self.start..self.end).contains(&sample) {
            return None;
        }
        u32::try_from(sample.saturating_sub(self.start)).ok()
    }

    /// The part of the sub-block in the audible region: its first sample and length.
    fn audible_part(&self) -> Option<(SamplePosition, u32)> {
        let start = self.start.max(self.from);
        let end = self.stop.map_or(self.end, |stop| self.end.min(stop));
        let frames = u32::try_from(end.checked_sub(start)?).ok()?;
        (frames > 0).then_some((SamplePosition(start), frames))
    }
}

/// Places ticks on samples through a tempo map (ADR-0018).
struct Placer<'a> {
    map: &'a TempoMap,
    rate: SampleRate,
}

impl Placer<'_> {
    /// The sample a tick plays at.
    fn sample_of(&self, tick: i64) -> Option<i64> {
        place(MusicalPosition(tick), self.map, &self.rate).map(|sample| sample.0)
    }

    /// The first tick placed at or after `sample`: one past the last read before it.
    fn first_tick_at_or_after(&self, sample: i64) -> Option<i64> {
        let before = SamplePosition(sample.checked_sub(1)?);
        read(before, &self.rate, self.map)?.0.checked_add(1)
    }
}

/// Merges a node's timeline events and its controller events into `merged`, each list in its
/// order: at one offset and rank, the timeline's first. `merged` has room for both.
fn merge(timeline: &[TimelineEvent], controller: &[Event], merged: &mut Vec<Event>) {
    merged.clear();
    let mut timeline = timeline.iter().peekable();
    let mut controller = controller.iter().peekable();
    loop {
        let from_timeline = match (timeline.peek(), controller.peek()) {
            (Some(ours), Some(theirs)) => {
                (ours.offset, ours.rank) <= (theirs.offset, rank_at_one_sample(theirs.kind))
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return,
        };
        let next = if from_timeline {
            timeline.next().map(|event| Event {
                offset: event.offset,
                kind: event.kind,
            })
        } else {
            controller.next().copied()
        };
        // Reserved for both lists: the push never allocates.
        if let Some(event) = next
            && !merged.spare_capacity_mut().is_empty()
        {
            merged.push(event);
        }
    }
}
