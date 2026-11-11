//! The generator of test sessions: the reference projects without their plugins
//! (`docs/reference-projects.md`), and a small session of a few seconds that holds every feature of
//! the file, for tests and CI.
//!
//! [`generate`] writes into an empty folder `session.json`, `audio/*.wav` (24-bit) and
//! `midi/*.mid`, the same to the byte on every machine: every choice comes from a SplitMix64
//! generator of its own, seeded per file from one fixed seed and the file's name, and the audio is
//! computed with +, -, *, / and `sqrt` alone, from tables computed with them, never with a function
//! of the C library whose precision differs between machines.
//!
//! The song is the reference projects': 3 minutes 30 seconds at 140 BPM in 4/4, 48 kHz, in
//! sections (intro, verses, choruses, a bridge, an outro) over four chords. Vocal takes sing its
//! melody, its harmonies or ad-libs in some sections, resting two bars in four, so that their files
//! hold silence between phrases and their clips leave gaps between sections. Stems and instruments
//! play from the start to the end.

mod audio;
mod midi;
mod song;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use magnolia_engine::wav::{SampleFormat, WavError, WavWriter};

use crate::file::{
    self, Bus, Clip, FileError, Insert, Instrument, MASTER, Master, SessionFile, Track, TrackKind,
    TrackSend,
};
use audio::{AudioPart, Synth};
use midi::MidiPart;
use song::{AFTER_INTRO, QUARTER, Section, Song};

/// The seed every file's generator starts from, mixed with the file's name: "Magnolia" in ASCII.
const SEED: u64 = 0x4D61_676E_6F6C_6961;

/// The ticks a quarter of the generated MIDI files: 960, which 4,838,400 divides, so they import
/// exactly. The small session's lead uses 1,000, which rounds on import.
const TICKS_PER_QUARTER: u16 = 960;

/// Which session to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The lowest tier's reference project without its plugins: 16 audio tracks (10 mono vocal
    /// takes, 2 stereo backing vocals, 4 stereo stems of the beat) and 8 MIDI tracks (drums, bass,
    /// chords, a lead with pitch bends, pads, three more parts of the beat); buses for the vocals,
    /// the drums and the beat, and a room bus fed by sends, where the reverb goes. 0.67 GB.
    Lowest,
    /// The high tier's reference project without its plugins and its live input: 48 audio tracks
    /// (vocal takes, backing vocals, recorded instruments, stems) and 24 MIDI tracks (the beat, a
    /// score's strings and brass), the same song. 2.18 GB.
    High,
    /// A session of about 8 seconds that holds every feature of the file: gaps between clips, a
    /// file offset, a start between two samples, gains, latent inserts on a bus and a track, sends,
    /// a bus into a bus, mono into stereo, MIDI with pitch bends and controllers, a MIDI file of
    /// 1,000 ticks a quarter, a clip cut short and a file played by two clips, a tempo change and a
    /// meter change.
    Small,
}

impl FromStr for Kind {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "lowest" => Ok(Self::Lowest),
            "high" => Ok(Self::High),
            "small" => Ok(Self::Small),
            _ => Err(format!("no session kind {name:?}: lowest, high or small")),
        }
    }
}

/// What [`generate`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generated {
    /// The audio files.
    pub audio_files: usize,
    /// The MIDI files.
    pub midi_files: usize,
    /// The bytes of every file, `session.json` included.
    pub bytes: u64,
}

/// Writes the session of `kind` into `folder`, which must be empty or not exist yet: `session.json`
/// and the files it names, in `audio/` and `midi/`.
///
/// # Errors
///
/// [`GenerateError::FolderNotEmpty`] rather than write over anything; otherwise a file that could
/// not be written.
pub fn generate(kind: Kind, folder: &Path) -> Result<Generated, GenerateError> {
    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |error| GenerateError::Io { path, error }
    };
    match fs::read_dir(folder) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(GenerateError::FolderNotEmpty(folder.to_path_buf()));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(folder)(error)),
    }
    let song = match kind {
        Kind::Lowest | Kind::High => Song::reference()?,
        Kind::Small => Song::small()?,
    };
    let plan = match kind {
        Kind::Lowest => lowest(&song),
        Kind::High => high(&song),
        Kind::Small => small(&song),
    };
    let mut text = session_file(&song, &plan)
        .to_json()
        .map_err(GenerateError::Json)?;
    text.push('\n');
    // What the generator writes, the reader must read: checked before gigabytes are written.
    file::parse(&text).map_err(GenerateError::Session)?;

    for sub in ["audio", "midi"] {
        let path = folder.join(sub);
        fs::create_dir_all(&path).map_err(io_error(&path))?;
    }
    let synth = Synth::new(song.rate.hertz());
    let mut generated = Generated {
        audio_files: 0,
        midi_files: 0,
        bytes: 0,
    };
    for track in &plan.tracks {
        let path = folder.join(&track.file);
        let mut random = Random::for_name(&track.file);
        match track.content {
            Content::Audio(part) => {
                let channels = part.render(&song, &synth, &mut random);
                write_wav(&path, &channels, song.rate.hertz())?;
                generated.audio_files += 1;
            }
            Content::Midi(part, ticks_per_quarter) => {
                let file = part.file(&song, &mut random, ticks_per_quarter);
                fs::write(&path, crate::midi_file::write(&file)).map_err(io_error(&path))?;
                generated.midi_files += 1;
            }
        }
        generated.bytes += fs::metadata(&path).map_err(io_error(&path))?.len();
    }

    let path = folder.join("session.json");
    fs::write(&path, &text).map_err(io_error(&path))?;
    generated.bytes += text.len() as u64;
    Ok(generated)
}

/// The session file of `plan`.
fn session_file(song: &Song, plan: &Plan) -> SessionFile {
    SessionFile {
        format: file::FORMAT.to_owned(),
        version: file::VERSION,
        sample_rate: song.rate.hertz(),
        tempo: song.tempo.clone(),
        master: Master {
            channels: 2,
            inserts: plan.master_inserts.clone(),
        },
        buses: plan.buses.clone(),
        tracks: plan
            .tracks
            .iter()
            .map(|track| track.track.clone())
            .collect(),
    }
}

/// Writes interleaved 24-bit frames of `channels` to `path`.
fn write_wav(path: &Path, channels: &[Vec<f32>], rate: u32) -> Result<(), GenerateError> {
    let wav_error = |error| GenerateError::Wav {
        path: path.to_path_buf(),
        error,
    };
    let file = File::create(path).map_err(|error| GenerateError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    let count = u16::try_from(channels.len()).unwrap_or(u16::MAX);
    let mut writer = WavWriter::new(BufWriter::new(file), SampleFormat::Int24, count, rate)
        .map_err(wav_error)?;
    let frames = channels.first().map_or(0, Vec::len);
    let mut interleaved = Vec::new();
    for start in (0..frames).step_by(4_096) {
        interleaved.clear();
        for frame in start..(start + 4_096).min(frames) {
            interleaved.extend(channels.iter().map(|channel| channel[frame]));
        }
        writer.write_frames(&interleaved).map_err(wav_error)?;
    }
    writer.finish().map_err(wav_error)?;
    Ok(())
}

/// A pseudo-random generator: SplitMix64, seeded from [`SEED`] and a name, so that each file draws
/// its own numbers, the same on every machine.
pub(crate) struct Random(u64);

impl Random {
    /// The generator of `name`: [`SEED`] mixed with the name's FNV-1a hash.
    pub(crate) fn for_name(name: &str) -> Self {
        let hash = name.bytes().fold(0xCBF2_9CE4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
        });
        Self(SEED ^ hash)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number from 0 to `bound - 1`; `bound` must not be 0.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// A number from 0 to 1, 1 excluded, a multiple of 2^-53.
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// A number from -1 to 1, 1 excluded.
    pub(crate) fn bipolar(&mut self) -> f64 {
        2.0 * self.unit() - 1.0
    }
}

/// A session to write: its master's inserts, its buses, and its tracks with what their files hold.
struct Plan {
    master_inserts: Vec<Insert>,
    buses: Vec<Bus>,
    tracks: Vec<TrackPlan>,
}

/// A track of a plan.
struct TrackPlan {
    /// The track as the session holds it.
    track: Track,
    /// Its file, relative to the session's folder.
    file: String,
    /// What its file holds.
    content: Content,
}

enum Content {
    Audio(AudioPart),
    /// A MIDI part, and its file's ticks a quarter.
    Midi(MidiPart, u16),
}

impl Plan {
    fn new() -> Self {
        Self {
            master_inserts: Vec::new(),
            buses: Vec::new(),
            tracks: Vec::new(),
        }
    }

    /// Adds a stereo bus.
    fn bus(&mut self, name: &str, output: &str, inserts: Vec<Insert>, gain: f32) {
        self.buses.push(Bus {
            name: name.to_owned(),
            channels: 2,
            inserts,
            output: output.to_owned(),
            gain,
        });
    }

    /// Adds an audio track whose file `audio/<name>.wav` holds `part`, one clip over the whole
    /// song unless changed.
    fn audio(&mut self, song: &Song, name: &str, part: AudioPart, output: &str) -> &mut TrackPlan {
        let file = format!("audio/{}.wav", slug(name));
        let clips = vec![audio_clip(song, &file, 0, song.end(), 1.0)];
        self.push(TrackPlan {
            track: Track {
                name: name.to_owned(),
                kind: TrackKind::Audio,
                channels: Some(part.channels()),
                instrument: None,
                clips,
                inserts: Vec::new(),
                output: output.to_owned(),
                gain: 0.35,
                sends: Vec::new(),
            },
            file,
            content: Content::Audio(part),
        })
    }

    /// Adds a MIDI track, into the test synth, whose file `midi/<name>.mid` holds `part`, one clip
    /// over the whole song unless changed.
    fn midi(&mut self, song: &Song, name: &str, part: MidiPart, output: &str) -> &mut TrackPlan {
        let file = format!("midi/{}.mid", slug(name));
        let clips = vec![midi_clip(&file, 0, song.end())];
        self.push(TrackPlan {
            track: Track {
                name: name.to_owned(),
                kind: TrackKind::Midi,
                channels: None,
                instrument: Some(Instrument::TestSynth {}),
                clips,
                inserts: Vec::new(),
                output: output.to_owned(),
                gain: 0.06,
                sends: Vec::new(),
            },
            file,
            content: Content::Midi(part, TICKS_PER_QUARTER),
        })
    }

    fn push(&mut self, track: TrackPlan) -> &mut TrackPlan {
        self.tracks.push(track);
        let last = self.tracks.len() - 1;
        &mut self.tracks[last]
    }
}

impl TrackPlan {
    fn gain(&mut self, gain: f32) -> &mut Self {
        self.track.gain = gain;
        self
    }

    fn send(&mut self, to: &str, gain: f32) -> &mut Self {
        self.track.sends.push(TrackSend {
            to: to.to_owned(),
            gain,
        });
        self
    }

    fn insert(&mut self, insert: Insert) -> &mut Self {
        self.track.inserts.push(insert);
        self
    }

    /// Gives a MIDI track's file `ticks` ticks a quarter.
    fn ticks_per_quarter(&mut self, ticks: u16) -> &mut Self {
        if let Content::Midi(_, ticks_per_quarter) = &mut self.content {
            *ticks_per_quarter = ticks;
        }
        self
    }

    /// Replaces its clips by those `clips` makes of its file.
    fn clips(&mut self, clips: impl FnOnce(&str) -> Vec<Clip>) -> &mut Self {
        self.track.clips = clips(&self.file);
        self
    }

    /// Replaces its clips by one clip per run of bars of `sections`.
    fn clips_in(&mut self, song: &Song, sections: &[Section]) -> &mut Self {
        self.clips(|file| {
            song.spans_of(sections)
                .into_iter()
                .map(|(from, to)| audio_clip(song, file, from, to, 1.0))
                .collect()
        })
    }
}

/// An audio clip of `file` from tick `from` to tick `to`, which plays the file's frames at those
/// samples: its file is laid on the timeline.
fn audio_clip(song: &Song, file: &str, from: i64, to: i64, gain: f32) -> Clip {
    let start = song.sample_of(from);
    Clip {
        file: file.to_owned(),
        start: song.time_of(from),
        file_offset: Some(start as u64),
        length: (song.sample_of(to) - start) as u64,
        gain: Some(gain),
    }
}

/// A MIDI clip of `file` from tick `start`, `length` ticks long.
fn midi_clip(file: &str, start: i64, length: i64) -> Clip {
    Clip {
        file: file.to_owned(),
        start,
        file_offset: None,
        length: length as u64,
        gain: None,
    }
}

/// A file's name from a track's: lowercase, spaces as dashes.
fn slug(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

/// What vocal take `take` sings: the lead and its double, harmonies above and below, the bridge,
/// ad-libs an octave up in the outro.
fn vocal_take(take: usize) -> AudioPart {
    use Section::{Bridge, Chorus, Outro, Verse};
    let (sections, harmony): (&'static [Section], isize) = match take % 10 {
        0 | 1 => (&[Verse, Chorus], 0),
        2 => (&[Chorus], 2),
        3 => (&[Chorus], -3),
        4 => (&[Bridge], 0),
        5 => (&[Bridge], 2),
        6 | 7 => (&[Outro], 5),
        8 => (&[Verse], 2),
        _ => (&[Chorus], -5),
    };
    AudioPart::Voice { sections, harmony }
}

/// The eight MIDI parts of the beat, which both reference projects hold.
fn beat_parts(plan: &mut Plan, song: &Song) {
    use Section::{Bridge, Chorus, Verse};
    plan.midi(song, "drums", MidiPart::Drums { busy: false }, "drums");
    plan.midi(song, "bass", MidiPart::Bass { octave: 0 }, "beat");
    let chords = MidiPart::Chords {
        sections: AFTER_INTRO,
        octave: 0,
        stabs: false,
    };
    plan.midi(song, "chords", chords, "beat").gain(0.04);
    let lead = MidiPart::Lead {
        sections: &[Verse, Chorus],
        harmony: 0,
    };
    plan.midi(song, "lead", lead, "beat").send("room", 0.2);
    let pads = MidiPart::Pads {
        sections: AFTER_INTRO,
    };
    plan.midi(song, "pads", pads, "beat").gain(0.03);
    let arp = MidiPart::Arpeggio {
        sections: &[Chorus, Bridge],
        octave: 1,
    };
    plan.midi(song, "arp", arp, "beat").gain(0.05);
    let counter = MidiPart::Lead {
        sections: &[Chorus, Bridge],
        harmony: -3,
    };
    plan.midi(song, "counter", counter, "beat").gain(0.05);
    let plucks = MidiPart::Chords {
        sections: &[Verse, Chorus],
        octave: 1,
        stabs: true,
    };
    plan.midi(song, "plucks", plucks, "beat").gain(0.04);
}

/// The four stereo stems of the beat, named after `set`.
fn stems(plan: &mut Plan, song: &Song, set: &str) {
    let parts = [
        ("drum stem", AudioPart::DrumStem, "drums"),
        ("bass stem", AudioPart::BassStem, "beat"),
        ("keys stem", AudioPart::KeysStem, "beat"),
        ("fx stem", AudioPart::FxStem, "beat"),
    ];
    for (name, part, output) in parts {
        plan.audio(song, &format!("{name}{set}"), part, output)
            .gain(0.3);
    }
}

/// The lowest tier's reference project, without its plugins.
fn lowest(song: &Song) -> Plan {
    let mut plan = Plan::new();
    plan.bus("vocals", MASTER, Vec::new(), 1.0);
    plan.bus("drums", "beat", Vec::new(), 1.0);
    plan.bus("beat", MASTER, Vec::new(), 1.0);
    plan.bus("room", MASTER, vec![Insert::Gain { value: 0.8 }], 1.0);
    for take in 0..10 {
        let part = vocal_take(take);
        let AudioPart::Voice { sections, .. } = part else {
            continue;
        };
        plan.audio(song, &format!("vocal take {:02}", take + 1), part, "vocals")
            .clips_in(song, sections)
            .send("room", 0.15);
    }
    for (pair, sections) in [&[Section::Chorus][..], &[Section::Chorus, Section::Bridge]]
        .into_iter()
        .enumerate()
    {
        plan.audio(
            song,
            &format!("backing vocals {:02}", pair + 1),
            AudioPart::Backing { sections },
            "vocals",
        )
        .clips_in(song, sections)
        .send("room", 0.25);
    }
    stems(&mut plan, song, "");
    beat_parts(&mut plan, song);
    plan
}

/// The high tier's reference project, without its plugins and its live input.
fn high(song: &Song) -> Plan {
    use Section::{Bridge, Chorus, Outro, Verse};
    let mut plan = Plan::new();
    // Three times the lowest tier's tracks, which double each other: the buses bring the master
    // back to its level.
    let level = 0.4;
    plan.bus("vocals", MASTER, Vec::new(), level);
    plan.bus("backing", MASTER, Vec::new(), level);
    plan.bus("instruments", MASTER, Vec::new(), level);
    plan.bus("drums", "beat", Vec::new(), 1.0);
    plan.bus("beat", MASTER, Vec::new(), level);
    plan.bus("score", MASTER, Vec::new(), level);
    plan.bus("room", MASTER, vec![Insert::Gain { value: 0.8 }], level);
    plan.bus("hall", MASTER, vec![Insert::Gain { value: 0.7 }], level);
    for take in 0..20 {
        let part = vocal_take(take);
        let AudioPart::Voice { sections, .. } = part else {
            continue;
        };
        plan.audio(song, &format!("vocal take {:02}", take + 1), part, "vocals")
            .clips_in(song, sections)
            .gain(0.3)
            .send("room", 0.15);
    }
    for pair in 0..8 {
        let sections: &'static [Section] = if pair % 2 == 0 {
            &[Chorus]
        } else {
            &[Chorus, Bridge]
        };
        plan.audio(
            song,
            &format!("backing vocals {:02}", pair + 1),
            AudioPart::Backing { sections },
            "backing",
        )
        .clips_in(song, sections)
        .gain(0.3)
        .send("hall", 0.25);
    }
    for take in 1..=4 {
        plan.audio(
            song,
            &format!("guitar {take:02}"),
            AudioPart::Guitar,
            "instruments",
        )
        .clips_in(song, &[Verse, Chorus])
        .gain(0.3);
        plan.audio(
            song,
            &format!("piano {take:02}"),
            AudioPart::Piano,
            "instruments",
        )
        .gain(0.25)
        .send("room", 0.1);
    }
    for set in 1..=3 {
        stems(&mut plan, song, &format!(" {set}"));
    }
    beat_parts(&mut plan, song);
    let more: [(&str, MidiPart, &str); 4] = [
        ("percussion", MidiPart::Drums { busy: true }, "drums"),
        (
            "stab",
            MidiPart::Chords {
                sections: &[Chorus],
                octave: 1,
                stabs: true,
            },
            "beat",
        ),
        ("sub", MidiPart::Bass { octave: -1 }, "beat"),
        (
            "bell",
            MidiPart::Arpeggio {
                sections: &[Verse, Outro],
                octave: 2,
            },
            "beat",
        ),
    ];
    for (name, part, output) in more {
        plan.midi(song, name, part, output).gain(0.05);
    }
    let score: [(&str, &'static [Section], usize, i8); 10] = [
        ("violins 1", AFTER_INTRO, 2, 1),
        ("violins 2", AFTER_INTRO, 1, 1),
        ("violas", AFTER_INTRO, 0, 0),
        ("cellos", AFTER_INTRO, 0, -1),
        ("basses", AFTER_INTRO, 0, -2),
        ("trumpets", &[Chorus], 2, 0),
        ("horns", &[Chorus, Bridge], 1, 0),
        ("trombones", &[Chorus], 0, -1),
        ("tuba", &[Chorus], 0, -2),
        ("brass swells", &[Bridge, Outro], 1, 0),
    ];
    for (name, sections, voice, octave) in score {
        let part = MidiPart::ChordTone {
            sections,
            voice,
            octave,
        };
        plan.midi(song, name, part, "score")
            .gain(0.04)
            .send("hall", 0.2);
    }
    let string_pad = MidiPart::Pads {
        sections: AFTER_INTRO,
    };
    plan.midi(song, "string pad", string_pad, "score")
        .gain(0.02);
    let brass_stabs = MidiPart::Chords {
        sections: &[Chorus],
        octave: 0,
        stabs: true,
    };
    plan.midi(song, "brass stabs", brass_stabs, "score")
        .gain(0.03);
    plan
}

/// The small session, every feature of the file in about 8 seconds.
fn small(song: &Song) -> Plan {
    let all: &'static [Section] = &[
        Section::Verse,
        Section::Chorus,
        Section::Bridge,
        Section::Outro,
    ];
    let bar = |index: usize| song.bars[index];
    let mut plan = Plan::new();
    plan.master_inserts = vec![Insert::Gain { value: 0.9 }];
    plan.bus("vocals", MASTER, Vec::new(), 0.9);
    // A latent bus, into another bus.
    plan.bus(
        "drums",
        "beat",
        vec![Insert::TestLatency { samples: 64 }],
        1.0,
    );
    plan.bus("beat", MASTER, vec![Insert::Gain { value: 0.8 }], 1.0);
    plan.bus("room", MASTER, vec![Insert::Gain { value: 0.5 }], 1.0);

    // Mono into a stereo bus, two clips with a gap of a bar between them, a send.
    let vocal = AudioPart::Voice {
        sections: all,
        harmony: 0,
    };
    plan.audio(song, "vocal", vocal, "vocals")
        .clips(|file| {
            vec![
                audio_clip(song, file, 0, bar(0).end(), 1.0),
                audio_clip(song, file, bar(2).start, bar(3).end(), 0.7),
            ]
        })
        .send("room", 0.3);
    // A latent track, whose clip starts between two samples and plays its file from an offset:
    // the file's first bar, 1,000 frames in, during the second bar.
    let double = AudioPart::Voice {
        sections: all,
        harmony: 2,
    };
    plan.audio(song, "double", double, "vocals")
        .clips(|file| {
            let start = song.sample_of(bar(1).start);
            let length = song.sample_of(bar(1).end()) - start - 1;
            vec![Clip {
                file: file.to_owned(),
                start: song.time_of(bar(1).start) + 1,
                file_offset: Some(1_000),
                length: length as u64,
                gain: Some(0.5),
            }]
        })
        .insert(Insert::TestLatency { samples: 300 })
        .gain(0.8);
    plan.audio(song, "drum loop", AudioPart::DrumStem, "drums");
    plan.audio(song, "keys", AudioPart::KeysStem, "beat")
        .insert(Insert::Gain { value: 0.7 })
        .gain(0.8);

    plan.midi(song, "drums", MidiPart::Drums { busy: true }, "drums");
    plan.midi(song, "bass", MidiPart::Bass { octave: 0 }, "beat");
    // Bends from a file of 1,000 ticks a quarter, the clip cut a quarter before the song's end.
    let lead = MidiPart::Lead {
        sections: all,
        harmony: 0,
    };
    plan.midi(song, "lead", lead, MASTER)
        .ticks_per_quarter(1_000)
        .clips(|file| vec![midi_clip(file, 0, song.end() - QUARTER)])
        .send("room", 0.2);
    // Controllers; one file played by two clips, the second from the third bar.
    plan.midi(song, "pads", MidiPart::Pads { sections: all }, "beat")
        .clips(|file| {
            vec![
                midi_clip(file, 0, bar(1).end()),
                midi_clip(file, bar(2).start, bar(3).end() - bar(2).start),
            ]
        })
        .gain(0.1);
    plan
}

/// Why a session could not be generated.
#[derive(Debug)]
pub enum GenerateError {
    /// The folder holds something already; nothing is written over.
    FolderNotEmpty(PathBuf),
    /// A folder or a file could not be made or written.
    Io {
        /// The folder or the file.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
    /// An audio file could not be written.
    Wav {
        /// The file.
        path: PathBuf,
        /// Why.
        error: WavError,
    },
    /// The session could not be written as JSON.
    Json(serde_json::Error),
    /// The session written does not read back: a fault of the generator.
    Session(FileError),
    /// The rate of the reference projects is not one the time model takes: a fault of the
    /// generator.
    Rate,
}

impl fmt::Display for GenerateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FolderNotEmpty(path) => write!(
                formatter,
                "{} is not empty; the generator writes only into an empty or new folder",
                path.display()
            ),
            Self::Io { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::Wav { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::Json(error) => write!(formatter, "the session could not be written: {error}"),
            Self::Session(error) => {
                write!(
                    formatter,
                    "the session generated does not read back: {error}"
                )
            }
            Self::Rate => formatter.write_str("the generator's sample rate is refused"),
        }
    }
}

impl Error for GenerateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::Wav { error, .. } => Some(error),
            Self::Json(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::FolderNotEmpty(_) | Self::Rate => None,
        }
    }
}
