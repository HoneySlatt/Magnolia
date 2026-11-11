//! The MIDI parts of generated sessions, and the Standard MIDI Files that hold them: format 1, a
//! first track with the song's tempo and meters, which REAPER reads and Magnolia ignores, then the
//! part's track.

use super::Random;
use super::song::{EIGHTH, Note, QUARTER, SIXTEENTH, Section, Song};
use crate::midi_file::{EventKind, Format, MidiFile, Track, TrackEvent};

/// What a MIDI track plays.
#[derive(Clone, Copy, Debug)]
pub(super) enum MidiPart {
    /// A drum kit, on MIDI channel 10; `busy` adds kicks off the beat.
    Drums { busy: bool },
    /// A bass line, `octave` octaves from its usual register.
    Bass { octave: i8 },
    /// The chords in `sections`: held, or struck as stabs on the off-beats.
    Chords {
        sections: &'static [Section],
        octave: i8,
        stabs: bool,
    },
    /// One tone of each chord held a bar, for a section of strings or brass, swelling with the
    /// expression controller (11) through each bar.
    ChordTone {
        sections: &'static [Section],
        voice: usize,
        octave: i8,
    },
    /// The chords as arpeggios in sixteenths.
    Arpeggio {
        sections: &'static [Section],
        octave: i8,
    },
    /// A melody, `harmony` degrees of its scale from the lead's line, with a scoop of the pitch
    /// bend into every third note.
    Lead {
        sections: &'static [Section],
        harmony: isize,
    },
    /// The chords held, with the sustain pedal (64) down through each bar, the modulation wheel
    /// (1) in a slow triangle and the expression (11) swelling.
    Pads { sections: &'static [Section] },
}

/// What a part plays: notes, pitch bends from -8,192 to 8,191, and controllers' changes as
/// (tick, number, value).
#[derive(Default)]
struct Played {
    notes: Vec<Note>,
    bends: Vec<(i64, i16)>,
    controllers: Vec<(i64, u8, u8)>,
}

impl MidiPart {
    /// The part's file at `ticks_per_quarter`; `random` gives what is particular to it.
    pub(super) fn file(self, song: &Song, random: &mut Random, ticks_per_quarter: u16) -> MidiFile {
        let channel = match self {
            Self::Drums { .. } => 9,
            _ => 0,
        };
        let played = self.played(song, random);
        let at = |tick: i64| file_tick(tick, ticks_per_quarter);
        // At one tick: note offs, then controllers and bends, then note ons; each in its order.
        let mut events: Vec<(u64, u8, EventKind)> = Vec::new();
        for note in &played.notes {
            let key = note.key;
            events.push((
                at(note.tick),
                2,
                EventKind::NoteOn {
                    channel,
                    key,
                    velocity: note.velocity.clamp(1, 127),
                },
            ));
            events.push((
                at(note.tick + note.length),
                0,
                EventKind::NoteOff {
                    channel,
                    key,
                    velocity: 64,
                },
            ));
        }
        for &(tick, value) in &played.bends {
            events.push((at(tick), 1, EventKind::PitchBend { channel, value }));
        }
        for &(tick, number, value) in &played.controllers {
            let kind = EventKind::Controller {
                channel,
                number,
                value,
            };
            events.push((at(tick), 1, kind));
        }
        events.sort_by_key(|&(tick, rank, _)| (tick, rank));
        let part = Track {
            events: events
                .into_iter()
                .map(|(tick, _, kind)| TrackEvent { tick, kind })
                .collect(),
        };
        MidiFile {
            format: Format::Parallel,
            ticks_per_quarter,
            tracks: vec![conductor(song, ticks_per_quarter), part],
        }
    }

    fn played(self, song: &Song, random: &mut Random) -> Played {
        match self {
            Self::Drums { busy } => Played {
                notes: song.drums(random, busy),
                ..Played::default()
            },
            Self::Bass { octave } => Played {
                notes: song.bass(random, octave),
                ..Played::default()
            },
            Self::Chords {
                sections,
                octave,
                stabs,
            } => Played {
                notes: song.chords(sections, octave, stabs),
                ..Played::default()
            },
            Self::Arpeggio { sections, octave } => Played {
                notes: song.arpeggio(sections, octave),
                ..Played::default()
            },
            Self::ChordTone {
                sections,
                voice,
                octave,
            } => {
                let notes = song.chord_tone(sections, voice, octave);
                let controllers = notes
                    .iter()
                    .flat_map(|note| swell(note.tick, note.length, 11))
                    .collect();
                Played {
                    notes,
                    controllers,
                    ..Played::default()
                }
            }
            Self::Lead { sections, harmony } => {
                let line = &mut Random::for_name("lead line");
                let notes = song.melody(line, sections, (9, 18), harmony);
                let bends = notes
                    .iter()
                    .step_by(3)
                    .flat_map(|note| scoop(note.tick, note.length))
                    .collect();
                Played {
                    notes,
                    bends,
                    ..Played::default()
                }
            }
            Self::Pads { sections } => {
                let notes = song.chords(sections, -1, false);
                let mut controllers = Vec::new();
                for (index, &(start, end)) in song.spans_of(sections).iter().enumerate() {
                    let offset = index as i64;
                    controllers.extend(swell(start, end - start, 11));
                    for (step, tick) in (start..end).step_by(EIGHTH as usize).enumerate() {
                        // A triangle over 32 eighths: 0 up to 127 and down again.
                        let phase = (step as i64 + offset) % 32;
                        let value = if phase < 16 {
                            phase * 8
                        } else {
                            (32 - phase) * 8
                        };
                        controllers.push((tick, 1, value.min(127) as u8));
                    }
                }
                for note in notes.iter().step_by(3) {
                    controllers.push((note.tick, 64, 127));
                    controllers.push((note.tick + note.length - SIXTEENTH, 64, 0));
                }
                Played {
                    notes,
                    controllers,
                    ..Played::default()
                }
            }
        }
        .humanized(random)
    }
}

impl Played {
    /// Moves each velocity by a little noise from `random`, as a player's hand would.
    fn humanized(mut self, random: &mut Random) -> Self {
        for note in &mut self.notes {
            let nudge = random.below(9) as i16 - 4;
            note.velocity = (i16::from(note.velocity) + nudge).clamp(1, 127) as u8;
        }
        self
    }
}

/// A bend from half the wheel down up to rest over an eighth, or the note if shorter, a point
/// every 32nd of a quarter: the points fall between the ticks of a file of 1,000 ticks a quarter,
/// which then rounds them.
fn scoop(tick: i64, length: i64) -> Vec<(i64, i16)> {
    let span = length.min(EIGHTH);
    let step = QUARTER / 32;
    let mut points: Vec<(i64, i16)> = (0..span)
        .step_by(step as usize)
        .map(|offset| (tick + offset, (-4_096 + 4_096 * offset / span) as i16))
        .collect();
    points.push((tick + span, 0));
    points
}

/// The expression controller (`number`) rising from 40 to 120 through the first half of a span,
/// then back, a point every eighth.
fn swell(start: i64, length: i64, number: u8) -> Vec<(i64, u8, u8)> {
    let half = (length / 2).max(1);
    (0..length)
        .step_by(EIGHTH as usize)
        .map(|offset| {
            let distance = (offset - half).abs();
            let value = 120 - 80 * distance / half;
            (start + offset, number, value.clamp(0, 127) as u8)
        })
        .collect()
}

/// The first track: the song's tempo changes and meters, at their ticks.
fn conductor(song: &Song, ticks_per_quarter: u16) -> Track {
    let mut events = Vec::new();
    for change in &song.tempo.changes {
        let [numerator, denominator] = change.bpm;
        // The quarter note in microseconds, to the nearest.
        let microseconds =
            (60_000_000 * u64::from(denominator) + u64::from(numerator) / 2) / u64::from(numerator);
        events.push(TrackEvent {
            tick: file_tick(change.tick, ticks_per_quarter),
            kind: EventKind::Tempo {
                microseconds_per_quarter: microseconds as u32,
            },
        });
    }
    for meter in &song.tempo.meters {
        let tick = usize::try_from(meter.bar)
            .ok()
            .and_then(|bar| song.bars.get(bar))
            .map_or(0, |bar| bar.start);
        events.push(TrackEvent {
            tick: file_tick(tick, ticks_per_quarter),
            kind: EventKind::TimeSignature {
                numerator: meter.beats as u8,
                denominator_power: meter.beat_unit.trailing_zeros() as u8,
            },
        });
    }
    events.sort_by_key(|event| event.tick);
    Track { events }
}

/// A tick of the time model as a tick of a file at `ticks_per_quarter`: the last at or before it,
/// exact on the grids the parts use.
fn file_tick(tick: i64, ticks_per_quarter: u16) -> u64 {
    (i128::from(tick) * i128::from(ticks_per_quarter) / i128::from(QUARTER)) as u64
}
