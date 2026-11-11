//! The audio content of generated files: voices, backing vocals, drums, bass, keys, guitar, piano
//! and noise, rendered from the song's notes.
//!
//! Everything written is computed with +, -, *, / and `sqrt` on `f64`, and with the floor and
//! conversions that IEEE 754 makes exact, in a fixed order: no `sin`, `exp` or `powf`, whose
//! precision differs between C libraries. The sine comes from a table computed by its Taylor
//! series, the equal temperament from a twelfth root of two found by bisection, and decays from
//! one multiplication a sample. So every machine writes the same bytes.

use std::f64::consts::{PI, TAU};

use super::Random;
use super::song::{AFTER_INTRO, Note, Section, Song};

/// Entries in the sine table's cycle; the table holds one more, the first again, to interpolate.
const TABLE_SIZE: usize = 4_096;

/// Oscillators and pitches, computed once.
pub(super) struct Synth {
    /// One cycle of a sine, and its first value again.
    table: Vec<f64>,
    /// Each MIDI key's frequency in hertz, equal temperament, key 69 at 440 Hz.
    frequencies: [f64; 128],
    /// The sample rate, in hertz.
    rate: f64,
}

impl Synth {
    pub(super) fn new(rate: u32) -> Self {
        let table = (0..=TABLE_SIZE)
            .map(|index| {
                let x = TAU * index as f64 / TABLE_SIZE as f64;
                taylor_sine(if x > PI { x - TAU } else { x })
            })
            .collect();
        let semitone = twelfth_root_of_two();
        let mut frequencies = [0.0; 128];
        frequencies[69] = 440.0;
        for key in 70..128 {
            frequencies[key] = frequencies[key - 1] * semitone;
        }
        for key in (0..69).rev() {
            frequencies[key] = frequencies[key + 1] / semitone;
        }
        Self {
            table,
            frequencies,
            rate: f64::from(rate),
        }
    }

    /// The sine of `cycles` whole turns, interpolated in the table.
    pub(super) fn sine(&self, cycles: f64) -> f64 {
        let position = cycles * TABLE_SIZE as f64;
        let floor = position.floor();
        let index = (floor as i64).rem_euclid(TABLE_SIZE as i64) as usize;
        let (low, high) = (self.table[index], self.table[index + 1]);
        low + (high - low) * (position - floor)
    }

    /// A sum of harmonics of a phase in turns: `weights[k]` for harmonic `k + 1`.
    fn harmonics(&self, phase: f64, weights: &[f64]) -> f64 {
        let mut sum = 0.0;
        for (index, weight) in weights.iter().enumerate() {
            let turns = phase * (index + 1) as f64;
            sum += weight * self.sine(turns - turns.floor());
        }
        sum
    }

    pub(super) fn frequency(&self, key: u8) -> f64 {
        self.frequencies[usize::from(key.min(127))]
    }

    /// Samples in `seconds`.
    fn samples(&self, seconds: f64) -> usize {
        (seconds * self.rate) as usize
    }
}

/// sin(x) for x from -π to π, by its Taylor series to x^59, far below an `f64`'s precision there.
fn taylor_sine(x: f64) -> f64 {
    let square = x * x;
    let mut term = x;
    let mut sum = x;
    for n in 1..30 {
        let n = f64::from(n);
        term *= -square / ((2.0 * n) * (2.0 * n + 1.0));
        sum += term;
    }
    sum
}

/// 2^(1/12), by bisection on x^12 = 2, x^12 computed by squaring.
fn twelfth_root_of_two() -> f64 {
    let (mut low, mut high) = (1.0_f64, 1.1_f64);
    for _ in 0..80 {
        let middle = (low + high) / 2.0;
        let square = middle * middle;
        let fourth = square * square;
        if fourth * fourth * fourth < 2.0 {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

/// What an audio track plays.
#[derive(Clone, Copy, Debug)]
pub(super) enum AudioPart {
    /// A sung line, mono: the song's melody in `sections`, moved by `harmony` degrees of its scale.
    Voice {
        sections: &'static [Section],
        harmony: isize,
    },
    /// Backing vocals, stereo: the chords' tones held in `sections`.
    Backing { sections: &'static [Section] },
    /// The beat's drums, stereo.
    DrumStem,
    /// The beat's bass, stereo.
    BassStem,
    /// The beat's chords, struck on the off-beats, stereo.
    KeysStem,
    /// Risers before each section and impacts at its start, stereo.
    FxStem,
    /// A guitar strumming the chords on the off-beats, mono.
    Guitar,
    /// A piano playing arpeggios, stereo.
    Piano,
}

impl AudioPart {
    /// The channels of its file.
    pub(super) fn channels(self) -> u16 {
        match self {
            Self::Voice { .. } | Self::Guitar => 1,
            _ => 2,
        }
    }

    /// Its samples over the whole song, each channel apart. `random` gives what is particular to
    /// its file: its noise, its timing; the song's lines come from seeds of their own.
    pub(super) fn render(self, song: &Song, synth: &Synth, random: &mut Random) -> Vec<Vec<f32>> {
        let frames = usize::try_from(song.sample_of(song.end())).unwrap_or_default();
        let mut out = vec![vec![0.0_f32; frames]; usize::from(self.channels())];
        let mut render = Render {
            song,
            synth,
            random,
            out: &mut out,
        };
        match self {
            Self::Voice { sections, harmony } => {
                let line = song.melody(
                    &mut Random::for_name("vocal line"),
                    sections,
                    (4, 14),
                    harmony,
                );
                for note in line {
                    render.voice(note);
                }
            }
            Self::Backing { sections } => {
                for voice in 0..3 {
                    for note in song.chord_tone(sections, voice, 0) {
                        render.ooh(note, voice as f64 / 2.0);
                    }
                }
            }
            Self::DrumStem => {
                for note in song.drums(&mut Random::for_name("drum pattern"), true) {
                    render.drum(note);
                }
            }
            Self::BassStem => {
                for note in song.bass(&mut Random::for_name("bass line"), -1) {
                    render.pluck(note, 0.5, &[1.0, 0.5, 0.25], 0.999_95);
                }
            }
            Self::KeysStem => {
                for (index, note) in song.chords(AFTER_INTRO, 0, true).into_iter().enumerate() {
                    render.pluck(note, (index % 3) as f64 / 2.0, &[0.6, 0.25, 0.1], 0.999_8);
                }
            }
            Self::FxStem => render.effects(),
            Self::Guitar => {
                let chords = song.chords(&[Section::Verse, Section::Chorus], -1, true);
                for (index, note) in chords.into_iter().enumerate() {
                    // Strummed: each chord's tones come low to high, each 10 ms after the one
                    // below.
                    let strum = song.rate.hertz() as i64 / 100 * (index % 3) as i64;
                    render.pluck_at(note, strum, 0.5, &[0.5, 0.3, 0.2, 0.12, 0.08], 0.999_9);
                }
            }
            Self::Piano => {
                for (index, note) in song.arpeggio(AFTER_INTRO, 1).into_iter().enumerate() {
                    let pan = if index % 2 == 0 { 0.3 } else { 0.7 };
                    render.pluck(note, pan, &[0.4, 0.15, 0.05], 0.999_85);
                }
            }
        }
        out
    }
}

/// Renders notes into a file's channels.
struct Render<'a> {
    song: &'a Song,
    synth: &'a Synth,
    random: &'a mut Random,
    out: &'a mut Vec<Vec<f32>>,
}

impl Render<'_> {
    /// The sample of a tick.
    fn sample_of(&self, tick: i64) -> usize {
        usize::try_from(self.song.sample_of(tick)).unwrap_or_default()
    }

    /// Adds `sample` at frame `frame`, panned from 0 (left) to 1 (right) at constant power on a
    /// stereo file; a mono file takes it whole.
    fn add(&mut self, frame: usize, sample: f64, pan: f64) {
        match self.out.as_mut_slice() {
            [mono] => {
                if let Some(slot) = mono.get_mut(frame) {
                    *slot += sample as f32;
                }
            }
            [left, right] => {
                if let (Some(left), Some(right)) = (left.get_mut(frame), right.get_mut(frame)) {
                    *left += (sample * (1.0 - pan).sqrt()) as f32;
                    *right += (sample * pan.sqrt()) as f32;
                }
            }
            _ => {}
        }
    }

    /// A sung note: six harmonics with vibrato that sets in after 150 ms, a breath of noise,
    /// 30 ms of attack and 80 ms of release.
    fn voice(&mut self, note: Note) {
        const HARMONICS: [f64; 6] = [1.0, 0.55, 0.3, 0.22, 0.12, 0.08];
        let start = self.sample_of(note.tick);
        let end = self.sample_of(note.tick + note.length);
        let attack = self.synth.samples(0.03);
        let release = self.synth.samples(0.08);
        let vibrato_delay = self.synth.samples(0.15);
        let vibrato_rate = (5.0 + self.random.unit()) / self.synth.rate;
        let step = self.synth.frequency(note.key) / self.synth.rate;
        let level = 0.12 * f64::from(note.velocity) / 127.0;
        let (mut phase, mut vibrato_phase, mut breath) = (0.0, 0.0, 0.0);
        for frame in start..end + release {
            let age = frame - start;
            let envelope = if frame >= end {
                1.0 - (frame - end) as f64 / release as f64
            } else if age < attack {
                age as f64 / attack as f64
            } else {
                1.0
            };
            let depth = if age > vibrato_delay { 0.006 } else { 0.0 };
            phase += step * (1.0 + depth * self.synth.sine(vibrato_phase));
            phase -= phase.floor();
            vibrato_phase += vibrato_rate;
            vibrato_phase -= vibrato_phase.floor();
            breath += 0.05 * (self.random.bipolar() - breath);
            let sample = self.synth.harmonics(phase, &HARMONICS) + 0.3 * breath;
            self.add(frame, level * envelope * sample, 0.5);
        }
    }

    /// A held "ooh": three soft harmonics, a slow attack and release, panned at `pan`.
    fn ooh(&mut self, note: Note, pan: f64) {
        const HARMONICS: [f64; 3] = [1.0, 0.3, 0.1];
        let start = self.sample_of(note.tick);
        let end = self.sample_of(note.tick + note.length);
        let attack = self.synth.samples(0.15);
        let release = self.synth.samples(0.3);
        let step = self.synth.frequency(note.key) / self.synth.rate;
        let level = 0.08;
        let mut phase = 0.0;
        for frame in start..end + release {
            let age = frame - start;
            let envelope = if frame >= end {
                1.0 - (frame - end) as f64 / release as f64
            } else {
                (age as f64 / attack as f64).min(1.0)
            };
            phase += step;
            phase -= phase.floor();
            let sample = self.synth.harmonics(phase, &HARMONICS);
            self.add(frame, level * envelope * sample, pan);
        }
    }

    /// A plucked or struck note at its tick: its harmonics decaying by `decay` a sample, for a
    /// second at most, ended 20 ms after its note.
    fn pluck(&mut self, note: Note, pan: f64, harmonics: &[f64], decay: f64) {
        self.pluck_at(note, 0, pan, harmonics, decay);
    }

    /// A pluck `delay` samples after its note's tick.
    fn pluck_at(&mut self, note: Note, delay: i64, pan: f64, harmonics: &[f64], decay: f64) {
        let start = self.sample_of(note.tick) + delay as usize;
        let end = self.sample_of(note.tick + note.length) + delay as usize;
        let fade = self.synth.samples(0.02);
        let longest = self.synth.samples(1.0);
        let step = self.synth.frequency(note.key) / self.synth.rate;
        let mut level = f64::from(note.velocity) / 127.0 * 0.3;
        let mut phase = 0.0;
        for frame in start..(end + fade).min(start + longest) {
            let envelope = if frame >= end {
                1.0 - (frame - end) as f64 / fade as f64
            } else {
                1.0
            };
            phase += step;
            phase -= phase.floor();
            let sample = self.synth.harmonics(phase, harmonics);
            self.add(frame, level * envelope * sample, pan);
            level *= decay;
        }
    }

    /// A drum's hit, by its General MIDI key.
    fn drum(&mut self, note: Note) {
        let start = self.sample_of(note.tick);
        let velocity = f64::from(note.velocity) / 127.0;
        match note.key {
            36 => self.kick(start, 0.5 * velocity),
            38 => self.snare(start, 0.3 * velocity),
            46 => self.noise_hit(start, 0.08 * velocity, 0.999_75, 0.4, 0.7),
            49 => self.noise_hit(start, 0.12 * velocity, 0.999_95, 1.5, 0.3),
            _ => self.noise_hit(start, 0.08 * velocity, 0.998_5, 0.1, 0.7),
        }
    }

    /// A kick: a sine that falls from 155 Hz to 45 Hz, decaying.
    fn kick(&mut self, start: usize, level: f64) {
        let (mut sweep, mut amplitude, mut phase) = (1.0, level, 0.0);
        for frame in start..start + self.synth.samples(0.5) {
            phase += (45.0 + 110.0 * sweep) / self.synth.rate;
            phase -= phase.floor();
            self.add(frame, amplitude * self.synth.sine(phase), 0.5);
            sweep *= 0.999;
            amplitude *= 0.999_85;
        }
    }

    /// A snare: noise and a 190 Hz tone, decaying.
    fn snare(&mut self, start: usize, level: f64) {
        let (mut noise, mut tone, mut phase) = (level, 0.6 * level, 0.0);
        for frame in start..start + self.synth.samples(0.25) {
            phase += 190.0 / self.synth.rate;
            phase -= phase.floor();
            let sample = noise * self.random.bipolar() + tone * self.synth.sine(phase);
            self.add(frame, sample, 0.45);
            noise *= 0.999_6;
            tone *= 0.999;
        }
    }

    /// Noise without its lows, decaying by `decay` a sample for `seconds`: a hat or a cymbal.
    fn noise_hit(&mut self, start: usize, level: f64, decay: f64, seconds: f64, pan: f64) {
        let (mut amplitude, mut previous) = (level, 0.0);
        for frame in start..start + self.synth.samples(seconds) {
            let noise = self.random.bipolar();
            self.add(frame, amplitude * (noise - previous) / 2.0, pan);
            previous = noise;
            amplitude *= decay;
        }
    }

    /// Before each section but the first, two bars of noise that rise and brighten; at its start,
    /// a low impact. Each channel draws its own noise.
    fn effects(&mut self) {
        let bars = &self.song.bars;
        let changes: Vec<(i64, i64)> = (1..bars.len())
            .filter(|&index| bars[index].section != bars[index - 1].section)
            .map(|index| (bars[index.saturating_sub(2)].start, bars[index].start))
            .collect();
        for (from, to) in changes {
            let (start, end) = (self.sample_of(from), self.sample_of(to));
            let mut filtered = [0.0; 2];
            for frame in start..end {
                let rise = (frame - start) as f64 / (end - start) as f64;
                let brightness = 0.02 + 0.5 * rise;
                for (channel, filtered) in filtered.iter_mut().enumerate() {
                    *filtered += brightness * (self.random.bipolar() - *filtered);
                    self.add(frame, 0.2 * rise * *filtered, channel as f64);
                }
            }
            let mut amplitude = 0.3;
            let mut phase = 0.0;
            for frame in end..end + self.synth.samples(1.0) {
                phase += 55.0 / self.synth.rate;
                phase -= phase.floor();
                self.add(frame, amplitude * self.synth.sine(phase), 0.5);
                amplitude *= 0.999_9;
            }
        }
    }
}
