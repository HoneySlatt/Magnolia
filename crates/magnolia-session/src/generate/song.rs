//! The song every generated session plays: its tempo map, its bars and their sections and chords,
//! and the notes of each part, in ticks. Every choice comes from a [`Random`] seeded by the part's
//! name, so the song is the same on every machine.

use magnolia_engine::time::{
    MusicalPosition, SampleRate, TICKS_PER_QUARTER, TempoMap, TimeUnits, place,
};

use super::{GenerateError, Random};
use crate::file::{MeterChange, TempoChange, TempoSection};

/// A quarter note, in ticks.
pub(super) const QUARTER: i64 = TICKS_PER_QUARTER;

/// An eighth and a sixteenth note, in ticks.
pub(super) const EIGHTH: i64 = QUARTER / 2;
pub(super) const SIXTEENTH: i64 = QUARTER / 4;

/// The part of the song a bar belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Section {
    Intro,
    Verse,
    Chorus,
    Bridge,
    Outro,
}

/// Every section but the intro.
pub(super) const AFTER_INTRO: &[Section] = &[
    Section::Verse,
    Section::Chorus,
    Section::Bridge,
    Section::Outro,
];

/// One bar of the song.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bar {
    /// Its first tick.
    pub(super) start: i64,
    /// Its length in ticks: its beats', or fewer for the song's last bar.
    pub(super) length: i64,
    /// The beats it holds, each a quarter note.
    pub(super) beats: i64,
    pub(super) section: Section,
    /// Its chord's keys, from low to high, around middle C.
    pub(super) chord: [u8; 3],
    /// The chord's root, in the bass's octave.
    pub(super) root: u8,
}

impl Bar {
    /// The tick after its last.
    pub(super) fn end(&self) -> i64 {
        self.start + self.length
    }
}

/// A note of a part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Note {
    pub(super) tick: i64,
    pub(super) length: i64,
    pub(super) key: u8,
    pub(super) velocity: u8,
}

/// The song: its tempo map, at 48 kHz, and its bars.
pub(super) struct Song {
    /// The tempo map as the session file gives it.
    pub(super) tempo: TempoSection,
    pub(super) map: TempoMap,
    pub(super) rate: SampleRate,
    pub(super) bars: Vec<Bar>,
    /// Whether vocal lines rest two bars in four, between phrases.
    pub(super) rests_between_phrases: bool,
}

/// The chords, one a bar, in turn: A minor, F major, C major, G major.
const CHORDS: [([u8; 3], u8); 4] = [
    ([57, 60, 64], 45),
    ([57, 60, 65], 41),
    ([55, 60, 64], 48),
    ([55, 59, 62], 43),
];

/// The A minor pentatonic scale, from A2 to A6: what melodies walk on.
const SCALE: [u8; 21] = [
    45, 48, 50, 52, 55, 57, 60, 62, 64, 67, 69, 72, 74, 76, 79, 81, 84, 86, 88, 91, 93,
];

impl Song {
    /// The reference projects' song: 3 minutes 30 seconds at 140 BPM in 4/4, 490 quarter notes,
    /// in sections of 8 and 16 bars.
    pub(super) fn reference() -> Result<Self, GenerateError> {
        let tempo = TempoSection {
            origin: 0,
            changes: vec![TempoChange {
                tick: 0,
                bpm: [140, 1],
            }],
            meters: vec![MeterChange {
                bar: 0,
                beats: 4,
                beat_unit: 4,
            }],
        };
        let section_of = |bar: usize| match bar {
            0..8 => Section::Intro,
            8..24 | 40..56 => Section::Verse,
            24..40 | 56..72 | 80..96 => Section::Chorus,
            72..80 => Section::Bridge,
            _ => Section::Outro,
        };
        let quarters: i64 = 490;
        // 122 bars of four quarters, then one of two, where the song ends.
        let bars = (0..(quarters + 3) / 4)
            .map(|index| (4, (quarters - 4 * index).min(4), section_of(index as usize)))
            .collect::<Vec<_>>();
        Self::new(tempo, &bars, true)
    }

    /// The small session's song: four bars, two at 140 BPM in 4/4, one at 93.5 BPM in 4/4 and
    /// one at 93.5 BPM in 3/4, about 8 seconds.
    pub(super) fn small() -> Result<Self, GenerateError> {
        let tempo = TempoSection {
            origin: 0,
            changes: vec![
                TempoChange {
                    tick: 0,
                    bpm: [140, 1],
                },
                TempoChange {
                    tick: 8 * QUARTER,
                    bpm: [187, 2],
                },
            ],
            meters: vec![
                MeterChange {
                    bar: 0,
                    beats: 4,
                    beat_unit: 4,
                },
                MeterChange {
                    bar: 3,
                    beats: 3,
                    beat_unit: 4,
                },
            ],
        };
        let bars = [
            (4, 4, Section::Verse),
            (4, 4, Section::Chorus),
            (4, 4, Section::Bridge),
            (3, 3, Section::Outro),
        ];
        Self::new(tempo, &bars, false)
    }

    /// A song of the bars given as their beats, the quarters they last, and their section.
    fn new(
        tempo: TempoSection,
        bars: &[(i64, i64, Section)],
        rests_between_phrases: bool,
    ) -> Result<Self, GenerateError> {
        let map = tempo.map().map_err(GenerateError::Session)?;
        let rate = SampleRate::new(48_000).ok_or(GenerateError::Rate)?;
        let mut start = 0;
        let bars = bars
            .iter()
            .enumerate()
            .map(|(index, &(beats, quarters, section))| {
                let (chord, root) = CHORDS[index % CHORDS.len()];
                let bar = Bar {
                    start,
                    length: quarters * QUARTER,
                    beats,
                    section,
                    chord,
                    root,
                };
                start = bar.end();
                bar
            })
            .collect();
        Ok(Self {
            tempo,
            map,
            rate,
            bars,
            rests_between_phrases,
        })
    }

    /// The tick after the song's last.
    pub(super) fn end(&self) -> i64 {
        self.bars.last().map_or(0, Bar::end)
    }

    /// The sample a tick plays at: the first at or after it.
    pub(super) fn sample_of(&self, tick: i64) -> i64 {
        place(MusicalPosition(tick), &self.map, &self.rate).map_or(0, |sample| sample.0)
    }

    /// The time position of a tick, in time units: the first at or after it.
    pub(super) fn time_of(&self, tick: i64) -> i64 {
        place(MusicalPosition(tick), &self.map, &TimeUnits).map_or(0, |time| time.0)
    }

    /// The bars of `sections`.
    fn bars_in<'a>(&'a self, sections: &'a [Section]) -> impl Iterator<Item = &'a Bar> {
        self.bars
            .iter()
            .filter(move |bar| sections.contains(&bar.section))
    }

    /// Each run of consecutive bars of `sections`, as its first tick and the tick after its last.
    pub(super) fn spans_of(&self, sections: &[Section]) -> Vec<(i64, i64)> {
        let mut spans: Vec<(i64, i64)> = Vec::new();
        for bar in self.bars_in(sections) {
            match spans.last_mut() {
                Some((_, end)) if *end == bar.start => *end = bar.end(),
                _ => spans.push((bar.start, bar.end())),
            }
        }
        spans
    }

    /// A drum kit's part on General MIDI's keys: kick (36), snare (38), closed hat (42), open hat
    /// (46), crash (49). The intro holds only hats; the chorus doubles them; a crash marks each
    /// section's start. `busy` adds kicks off the beat.
    pub(super) fn drums(&self, random: &mut Random, busy: bool) -> Vec<Note> {
        let mut notes = Vec::new();
        let mut previous = None;
        for bar in &self.bars {
            if previous != Some(bar.section) && bar.section != Section::Intro {
                notes.push(note(bar.start, SIXTEENTH, 49, 110));
            }
            previous = Some(bar.section);
            let hat_step = if bar.section == Section::Chorus {
                SIXTEENTH
            } else {
                EIGHTH
            };
            for hat in steps(bar, hat_step) {
                let open = hat + hat_step == bar.end() && bar.section != Section::Intro;
                let key = if open { 46 } else { 42 };
                notes.push(note(hat, hat_step, key, 60 + random.below(40) as u8));
            }
            if bar.section == Section::Intro {
                continue;
            }
            for beat in 0..bar.beats {
                let tick = bar.start + beat * QUARTER;
                if tick >= bar.end() {
                    break;
                }
                let key = if beat % 2 == 0 { 36 } else { 38 };
                notes.push(note(tick, SIXTEENTH, key, 100 + random.below(27) as u8));
                if busy && beat % 2 == 1 && random.below(3) == 0 && tick + EIGHTH < bar.end() {
                    notes.push(note(tick + EIGHTH, SIXTEENTH, 36, 90));
                }
            }
        }
        sorted(notes)
    }

    /// A bass line: the chord's root in eighths, now and then an octave up or a rest, from the
    /// verse on; `octave` moves it by octaves.
    pub(super) fn bass(&self, random: &mut Random, octave: i8) -> Vec<Note> {
        let mut notes = Vec::new();
        for bar in self.bars.iter().filter(|bar| bar.section != Section::Intro) {
            for tick in steps(bar, EIGHTH) {
                match random.below(8) {
                    0 => {}
                    1 => notes.push(note(tick, EIGHTH, shift(bar.root, 12 + 12 * octave), 100)),
                    _ => notes.push(note(tick, EIGHTH, shift(bar.root, 12 * octave), 90)),
                }
            }
        }
        sorted(notes)
    }

    /// Each bar's chord in `sections`, `octave` octaves from middle C: held the whole bar, or
    /// struck as stabs on the off-beats.
    pub(super) fn chords(&self, sections: &[Section], octave: i8, stabs: bool) -> Vec<Note> {
        let mut notes = Vec::new();
        for bar in self.bars_in(sections) {
            let strikes: Vec<(i64, i64)> = if stabs {
                steps(bar, QUARTER)
                    .map(|beat| (beat + EIGHTH, SIXTEENTH))
                    .filter(|&(tick, _)| tick < bar.end())
                    .collect()
            } else {
                vec![(bar.start, bar.length)]
            };
            for (tick, length) in strikes {
                for key in bar.chord {
                    notes.push(note(tick, length, shift(key, 12 * octave), 80));
                }
            }
        }
        sorted(notes)
    }

    /// One tone of each bar's chord in `sections`, held the whole bar: the `voice`th from the
    /// bottom, `octave` octaves from middle C. A section of strings or brass.
    pub(super) fn chord_tone(&self, sections: &[Section], voice: usize, octave: i8) -> Vec<Note> {
        self.bars_in(sections)
            .map(|bar| {
                let key = bar.chord[voice % bar.chord.len()];
                note(bar.start, bar.length, shift(key, 12 * octave), 70)
            })
            .collect()
    }

    /// Each bar's chord in `sections` played up and down in sixteenths, `octave` octaves from
    /// middle C.
    pub(super) fn arpeggio(&self, sections: &[Section], octave: i8) -> Vec<Note> {
        let order = [0, 1, 2, 1];
        let mut notes = Vec::new();
        for bar in self.bars_in(sections) {
            for (step, tick) in steps(bar, SIXTEENTH).enumerate() {
                let key = bar.chord[order[step % order.len()]];
                notes.push(note(tick, SIXTEENTH, shift(key, 12 * octave), 75));
            }
        }
        notes
    }

    /// A melody in `sections`, walking the A minor pentatonic scale from the degree `low` up to
    /// `high`, then moved by `harmony` degrees of the scale; with
    /// [`rests_between_phrases`](Self::rests_between_phrases), it rests two bars in four. The walk
    /// depends only on `random`, so two parts seeded alike sing the same line, moved apart by their
    /// harmony.
    pub(super) fn melody(
        &self,
        random: &mut Random,
        sections: &[Section],
        (low, high): (usize, usize),
        harmony: isize,
    ) -> Vec<Note> {
        // Rhythms of a bar, in eighths.
        const RHYTHMS: [&[i64]; 4] = [&[2, 2, 2, 2], &[1, 1, 2, 4], &[3, 1, 2, 2], &[4, 2, 1, 1]];
        let mut notes = Vec::new();
        let mut degree = (low + high) / 2;
        for (index, bar) in self.bars.iter().enumerate() {
            // The walk goes through every bar, sung or not, drawing the same numbers, so that a
            // line does not depend on the sections asked for.
            let rhythm = RHYTHMS[random.below(RHYTHMS.len() as u64) as usize];
            let sung =
                !(self.rests_between_phrases && index % 4 >= 2) && sections.contains(&bar.section);
            let mut tick = bar.start;
            for &eighths in rhythm {
                let (step, accent) = (random.below(5), random.below(30));
                let length = eighths * EIGHTH;
                if tick + length > bar.end() {
                    continue;
                }
                degree = (degree + step as usize).saturating_sub(2).clamp(low, high);
                if sung {
                    let key = SCALE[degree.saturating_add_signed(harmony).min(SCALE.len() - 1)];
                    notes.push(note(tick, length, key, 90 + accent as u8));
                }
                tick += length;
            }
        }
        notes
    }
}

/// A note, `semitones` from `key`, kept within MIDI's keys.
fn shift(key: u8, semitones: i8) -> u8 {
    key.saturating_add_signed(semitones).min(127)
}

fn note(tick: i64, length: i64, key: u8, velocity: u8) -> Note {
    Note {
        tick,
        length,
        key,
        velocity,
    }
}

/// The ticks of a bar every `step`.
fn steps(bar: &Bar, step: i64) -> impl Iterator<Item = i64> + use<> {
    let end = bar.end();
    (bar.start..end).step_by(step as usize)
}

/// Notes sorted by tick, those of one tick in the order given.
fn sorted(mut notes: Vec<Note>) -> Vec<Note> {
    notes.sort_by_key(|note| note.tick);
    notes
}
