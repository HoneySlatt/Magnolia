//! A MIDI file made into a clip of the engine's: its tracks merged, its ticks scaled to the time
//! model's, its notes paired.

use std::collections::VecDeque;

use magnolia_engine::engine::{ControllerPoint, MidiClip, MidiNote, PitchBendPoint};
use magnolia_engine::time::{MusicalPosition, TICKS_PER_QUARTER};

use crate::midi_file::{EventKind, MidiFile};

/// A MIDI file holds an event whose tick, scaled to the time model's, lies past the ticks' range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TicksOutOfRange;

/// The keys a MIDI channel has, and the channels: the queues of notes struck and not yet ended.
const KEYS: usize = 128;
const CHANNELS: usize = 16;

/// The clip of `length` ticks from `start` that plays `file`:
///
/// - every track of the file merged, at one tick in the order of the tracks, then of the file;
/// - each tick scaled from the file's division to [`TICKS_PER_QUARTER`] by the first tick at or
///   after it, the time model's rule for placing: exact for every common division, which divides
///   it, rounded up for others, such as 1,000;
/// - each note off ending the earliest note of its channel and key still held, first in, first
///   out; a note off that ends none is ignored, a note that lasts no tick after scaling does not
///   play, and a note without its note off ends at the clip's end; the track's sequence then ends
///   each note where a later note of its channel and key starts
///   ([`MidiSequence`](magnolia_engine::engine::MidiSequence)), so that notes of one key that
///   overlap play one after the other, as in a piano roll;
/// - velocities divided by 127, controllers' values by 127, pitch bends by 8,192 and kept from -1
///   to 1;
/// - the file's tempo and time signatures ignored: the session's tempo map lays the clip on the
///   timeline.
///
/// Events at or past the clip's end stay in the clip; the engine's sequence leaves them out.
pub(crate) fn clip_from_file(
    file: &MidiFile,
    start: MusicalPosition,
    length: i64,
) -> Result<MidiClip, TicksOutOfRange> {
    let division = u128::from(file.ticks_per_quarter);
    let mut events: Vec<(u64, usize, EventKind)> = file
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(track, events)| {
            events
                .events
                .iter()
                .map(move |event| (event.tick, track, event.kind))
        })
        .collect();
    // Stable: at one tick and track, the file's order stays.
    events.sort_by_key(|&(tick, track, _)| (tick, track));

    let mut clip = MidiClip {
        start,
        length,
        notes: Vec::new(),
        pitch_bends: Vec::new(),
        controllers: Vec::new(),
    };
    // Per channel and key, the notes held, by their index in `clip.notes`.
    let mut held: Vec<VecDeque<usize>> = vec![VecDeque::new(); CHANNELS * KEYS];
    for (file_tick, _, kind) in events {
        let tick = scaled(file_tick, division).ok_or(TicksOutOfRange)?;
        match kind {
            EventKind::NoteOn {
                channel,
                key,
                velocity,
            } => {
                if let Some(queue) = held.get_mut(held_index(channel, key)) {
                    queue.push_back(clip.notes.len());
                    clip.notes.push(MidiNote {
                        tick,
                        // Set by its note off, or at the end.
                        length: 0,
                        channel,
                        key,
                        velocity: f32::from(velocity) / 127.0,
                    });
                }
            }
            EventKind::NoteOff { channel, key, .. } => {
                let note = held
                    .get_mut(held_index(channel, key))
                    .and_then(VecDeque::pop_front)
                    .and_then(|index| clip.notes.get_mut(index));
                if let Some(note) = note {
                    // Ticks only grow, so the difference is never negative.
                    note.length = tick.saturating_sub(note.tick);
                }
            }
            EventKind::PitchBend { channel, value } => clip.pitch_bends.push(PitchBendPoint {
                tick,
                channel,
                value: (f32::from(value) / 8_192.0).clamp(-1.0, 1.0),
            }),
            EventKind::Controller {
                channel,
                number,
                value,
            } => clip.controllers.push(ControllerPoint {
                tick,
                channel,
                number,
                value: f32::from(value) / 127.0,
            }),
            EventKind::Tempo { .. } | EventKind::TimeSignature { .. } => {}
        }
    }
    for index in held.into_iter().flatten() {
        if let Some(note) = clip.notes.get_mut(index) {
            note.length = length.saturating_sub(note.tick);
        }
    }
    // A note of no tick, or one held from the clip's end on, plays nothing.
    clip.notes.retain(|note| note.length >= 1);
    Ok(clip)
}

/// A file's tick scaled to the time model's: the first tick at or after it.
fn scaled(file_tick: u64, division: u128) -> Option<i64> {
    // A file's tick is below 2^52 and the ticks a quarter below 2^23: no overflow.
    let exact = u128::from(file_tick).checked_mul(TICKS_PER_QUARTER as u128)?;
    let ticks = exact
        .checked_add(division.checked_sub(1)?)?
        .checked_div(division)?;
    i64::try_from(ticks).ok()
}

/// The queue of a channel and key, in `held`.
fn held_index(channel: u8, key: u8) -> usize {
    usize::from(channel)
        .saturating_mul(KEYS)
        .saturating_add(usize::from(key))
}
