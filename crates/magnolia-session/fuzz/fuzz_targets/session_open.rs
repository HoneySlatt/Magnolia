//! Fuzzes the test session's loader, `Session::open`, with arbitrary text, written as
//! `session.json` beside a few small WAV and MIDI files: the memory estimate, the audio files'
//! checks, the MIDI files made into clips, the clips into sequences, and input tracks. Nothing may
//! panic; a session that opens must fit its memory budget, end within 24 hours, and give each
//! armed track capture channels the engine has.
//!
//! The parser has its own target, `session_file`, which mostly builds sessions it must refuse. The
//! loader is reached only by sessions that parse, so this target mostly changes a session that
//! opens, [`BASE`], in a few places, each value mostly in range; one input in eight is raw text
//! (`session_text.rs`).
//!
//! The target writes its files before the first input, into a folder of its own under the system's
//! temporary folder, named by its process: WAV files at 48 kHz, mono and stereo, and one at
//! 44.1 kHz; a MIDI file of format 1 whose notes overlap on one key, within a track and across two,
//! one lacking its note off; and one of format 0 at 1,000 ticks a quarter, whose ticks round on
//! import. The folder, a few tens of kilobytes, stays after the run: libFuzzer ends the process
//! without Rust's destructors.

#![no_main]

mod session_text;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use magnolia_session::Session;
use magnolia_session::file::{self, Clip, Insert, SessionFile};
use magnolia_session::load::MEMORY_BUDGET;
use magnolia_session::midi_file::{self, EventKind, Format, MidiFile, Track, TrackEvent};

use session_text::{often, session_text};

/// The frames of each WAV file: a tenth of a second at 48 kHz.
const FRAMES: u64 = 4_800;

/// A session that opens with the folder's files: a mono track sent to a latent bus, a stereo track
/// of two clips of one file, two MIDI tracks, one of two overlapping clips of one file, and an
/// armed mono input track into the latent bus. The second stereo clip starts on sample 2,400, in
/// time units; MIDI clips last 2 or 4 quarters.
const BASE: &str = r#"{
  "format": "magnolia-test-session", "version": 2, "sample_rate": 48000,
  "tempo": { "origin": 0, "changes": [ { "tick": 0, "bpm": [140, 1] } ],
             "meters": [ { "bar": 0, "beats": 4, "beat_unit": 4 } ] },
  "master": { "channels": 2, "inserts": [] },
  "buses": [ { "name": "room", "channels": 2, "inserts": [ { "test_latency": { "samples": 64 } } ],
               "output": "master", "gain": 1.0 } ],
  "tracks": [
    { "name": "voice", "kind": "audio", "channels": 1,
      "clips": [ { "file": "audio/mono.wav", "start": 0, "file_offset": 0, "length": 4800,
                   "gain": 1.0 } ],
      "inserts": [], "output": "master", "gain": 1.0, "sends": [ { "to": "room", "gain": 0.5 } ] },
    { "name": "pair", "kind": "audio", "channels": 2,
      "clips": [ { "file": "audio/stereo.wav", "start": 0, "file_offset": 0, "length": 2400,
                   "gain": 1.0 },
                 { "file": "audio/stereo.wav", "start": 45158400000, "file_offset": 2400,
                   "length": 2400, "gain": 0.5 } ],
      "inserts": [], "output": "room", "gain": 1.0, "sends": [] },
    { "name": "keys", "kind": "midi", "instrument": { "test_synth": {} },
      "clips": [ { "file": "midi/parallel.mid", "start": 0, "length": 19353600 },
                 { "file": "midi/parallel.mid", "start": 9676800, "length": 19353600 } ],
      "inserts": [], "output": "master", "gain": 1.0, "sends": [] },
    { "name": "drums", "kind": "midi", "instrument": { "test_synth": {} },
      "clips": [ { "file": "midi/single.mid", "start": 0, "length": 9676800 } ],
      "inserts": [ { "test_latency": { "samples": 32 } } ], "output": "room", "gain": 1.0,
      "sends": [] },
    { "name": "mic", "kind": "input", "channels": 1, "inputs": [1], "armed": true,
      "inserts": [ { "test_latency": { "samples": 16 } } ], "output": "room", "gain": 1.0,
      "sends": [ { "to": "master", "gain": 0.5 } ] }
  ]
}"#;

/// The files a changed clip may name: the folder's, one it lacks, and one outside it.
const FILES: [&str; 7] = [
    "audio/mono.wav",
    "audio/stereo.wav",
    "audio/44100.wav",
    "midi/parallel.mid",
    "midi/single.mid",
    "missing.wav",
    "../outside.mid",
];

/// The longest a session lasts, in seconds (`file/check.rs`).
const DAY: i64 = 24 * 60 * 60;

fuzz_target!(init: set_up(), |input: &[u8]| open(input));

/// Writes this process's files, before the first input.
fn set_up() {
    let folder = folder();
    if let Err(error) = write_files(&folder) {
        panic!(
            "the target cannot write its files into {}: {error}",
            folder.display()
        );
    }
}

/// Opens the session `input` stands for, written into this process's folder.
fn open(input: &[u8]) {
    let Some(text) = session_text(input, changed_session) else {
        return;
    };
    let path = folder().join("session.json");
    fs::write(&path, text.as_bytes()).expect("the target's folder takes the session file");
    let Ok(session) = Session::open(&path) else {
        return;
    };
    let estimate = session.memory_estimate();
    assert!(
        estimate.total() <= MEMORY_BUDGET,
        "a session over the budget opened: {estimate}"
    );
    let rate = i64::from(session.sample_rate().hertz());
    assert!(
        session.last_sample().0 <= rate * DAY,
        "a session longer than a day opened: it ends on sample {}",
        session.last_sample().0
    );
    let capture_channels = usize::from(session.capture_channels());
    for track in session.armed_tracks() {
        assert!(
            !track.capture_channels.is_empty()
                && track
                    .capture_channels
                    .iter()
                    .all(|&channel| channel < capture_channels),
            "armed track {} records capture channels {:?} of an engine of {capture_channels}",
            track.number,
            track.capture_channels
        );
    }
}

/// [`BASE`], changed in up to eight places that `input` picks.
fn changed_session(input: &mut Unstructured<'_>) -> Result<SessionFile> {
    let mut session = file::parse(BASE).expect("the base session parses");
    for _ in 0..input.int_in_range(0..=8)? {
        change(&mut session, input)?;
    }
    Ok(session)
}

/// Changes one thing of `session`, mostly within the ranges a session keeps to: a clip's start
/// within the first 888 quarters, or milliseconds; an audio clip within its file; a MIDI clip up to
/// 1,775 quarters long; an input track's capture channels among the first four.
fn change(session: &mut SessionFile, input: &mut Unstructured<'_>) -> Result<()> {
    match input.int_in_range(0..=11)? {
        0 => {
            session.sample_rate = often(input, |input| {
                input.choose(&[44_100, 48_000, 96_000]).copied()
            })?;
        }
        1 => {
            let bpm = [
                often(input, |input| input.int_in_range(1..=1_000))?,
                often(input, |input| input.int_in_range(1..=4))?,
            ];
            if let Some(change) = session.tempo.changes.first_mut() {
                change.bpm = bpm;
            }
        }
        2 => {
            if let Some(clip) = pick_clip(session, input)? {
                clip.start = often(input, |input| input.int_in_range(0..=1 << 32))?;
            }
        }
        3 => {
            if let Some(clip) = pick_clip(session, input)? {
                let longest = if clip.file_offset.is_some() {
                    FRAMES
                } else {
                    1 << 33
                };
                clip.length = often(input, |input| input.int_in_range(1..=longest))?;
            }
        }
        4 => {
            if let Some(clip) = pick_clip(session, input)?
                && clip.file_offset.is_some()
            {
                clip.file_offset = Some(often(input, |input| input.int_in_range(0..=FRAMES))?);
            }
        }
        5 => {
            if let Some(clip) = pick_clip(session, input)? {
                clip.file = (*input.choose(&FILES)?).to_owned();
            }
        }
        // A copy of a clip on its track: audio clips that overlap, MIDI notes struck together.
        6 => {
            if let Some(track) =
                pick(session.tracks.len(), input)?.and_then(|index| session.tracks.get_mut(index))
                && let Some(clips) = &mut track.clips
                && let Some(clip) = pick(clips.len(), input)?
                    .and_then(|index| clips.get(index))
                    .cloned()
            {
                clips.push(clip);
            }
        }
        7 => {
            if let Some(index) = pick(session.tracks.len(), input)? {
                session.tracks.remove(index);
            }
        }
        8 => {
            if let Some(track) =
                pick(session.tracks.len(), input)?.and_then(|index| session.tracks.get_mut(index))
                && track.channels.is_some()
            {
                track.channels = Some(often(input, |input| input.int_in_range(1..=2))?);
            }
        }
        9 => {
            if let Some(track) =
                pick(session.tracks.len(), input)?.and_then(|index| session.tracks.get_mut(index))
            {
                track.inserts.push(Insert::TestLatency {
                    samples: often(input, |input| input.int_in_range(0..=70_000))?,
                });
            }
        }
        // An input track's capture channels: shared with another, too many or too few, or high.
        10 => {
            if let Some(track) =
                pick(session.tracks.len(), input)?.and_then(|index| session.tracks.get_mut(index))
                && track.inputs.is_some()
            {
                let count = input.int_in_range(0..=3)?;
                track.inputs = Some(
                    (0..count)
                        .map(|_| often(input, |input| input.int_in_range(1..=4)))
                        .collect::<Result<_>>()?,
                );
            }
        }
        // A copy of a track, an input among them, which then shares its capture channels.
        _ => {
            if let Some(track) = pick(session.tracks.len(), input)?
                .and_then(|index| session.tracks.get(index))
                .cloned()
            {
                session.tracks.push(track);
            }
        }
    }
    Ok(())
}

/// An index below `len` that `input` picks; `None` when `len` is 0.
fn pick(len: usize, input: &mut Unstructured<'_>) -> Result<Option<usize>> {
    if len == 0 {
        return Ok(None);
    }
    input.int_in_range(0..=len - 1).map(Some)
}

/// One of the session's clips, of any track, that `input` picks.
fn pick_clip<'s>(
    session: &'s mut SessionFile,
    input: &mut Unstructured<'_>,
) -> Result<Option<&'s mut Clip>> {
    let count = session.tracks.iter().map(|track| track.clips().len()).sum();
    let Some(index) = pick(count, input)? else {
        return Ok(None);
    };
    Ok(session
        .tracks
        .iter_mut()
        .flat_map(|track| track.clips.iter_mut().flatten())
        .nth(index))
}

/// This process's folder. Named by the process, so that libFuzzer's parallel jobs never share one.
fn folder() -> PathBuf {
    std::env::temp_dir().join(format!("magnolia-fuzz-session-open-{}", std::process::id()))
}

/// Writes the WAV and MIDI files into `folder`, made anew.
fn write_files(folder: &Path) -> io::Result<()> {
    // A folder an earlier process of the same id left.
    match fs::remove_dir_all(folder) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::create_dir_all(folder.join("audio"))?;
    fs::create_dir_all(folder.join("midi"))?;
    fs::write(folder.join("audio/mono.wav"), wav(1, 48_000))?;
    fs::write(folder.join("audio/stereo.wav"), wav(2, 48_000))?;
    fs::write(folder.join("audio/44100.wav"), wav(1, 44_100))?;
    fs::write(
        folder.join("midi/parallel.mid"),
        midi_file::write(&parallel()),
    )?;
    fs::write(folder.join("midi/single.mid"), midi_file::write(&single()))?;
    Ok(())
}

/// A WAV file of [`FRAMES`] frames of 16-bit PCM at `rate`, a quiet ramp on each channel. Written
/// here rather than by the engine's `WavWriter`, which would make the engine a dependency of this
/// crate.
fn wav(channels: u16, rate: u32) -> Vec<u8> {
    let frame_bytes = 2 * channels;
    let data_bytes = FRAMES as u32 * u32::from(frame_bytes);
    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    // PCM.
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(frame_bytes)).to_le_bytes());
    bytes.extend_from_slice(&frame_bytes.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in 0..FRAMES * u64::from(channels) {
        bytes.extend_from_slice(&((sample % 100) as i16 * 30).to_le_bytes());
    }
    bytes
}

fn at(tick: u64, kind: EventKind) -> TrackEvent {
    TrackEvent { tick, kind }
}

fn on(channel: u8, key: u8) -> EventKind {
    EventKind::NoteOn {
        channel,
        key,
        velocity: 100,
    }
}

fn off(channel: u8, key: u8) -> EventKind {
    EventKind::NoteOff {
        channel,
        key,
        velocity: 0,
    }
}

/// Format 1 at 960 ticks a quarter: a tempo track, then two tracks whose notes overlap on one key,
/// with bends and the sustain pedal; a note that is never ended, and a note off that ends none.
fn parallel() -> MidiFile {
    let tempo = EventKind::Tempo {
        microseconds_per_quarter: 500_000,
    };
    let bend = |value| EventKind::PitchBend { channel: 0, value };
    let pedal = |value| EventKind::Controller {
        channel: 0,
        number: 64,
        value,
    };
    MidiFile {
        format: Format::Parallel,
        ticks_per_quarter: 960,
        tracks: vec![
            Track {
                events: vec![at(0, tempo)],
            },
            Track {
                events: vec![
                    at(0, on(0, 60)),
                    at(0, on(1, 64)),
                    at(240, bend(4_096)),
                    at(480, on(0, 60)),
                    at(480, pedal(127)),
                    at(720, bend(-8_192)),
                    at(960, off(0, 60)),
                    at(960, off(1, 64)),
                    at(1_200, pedal(0)),
                    at(1_440, off(0, 60)),
                    at(1_920, on(0, 67)),
                    at(2_000, off(0, 50)),
                ],
            },
            Track {
                events: vec![at(240, on(0, 60)), at(600, off(0, 60))],
            },
        ],
    }
}

/// Format 0 at 1,000 ticks a quarter, which the time model's ticks do not divide: drums on channel
/// 10, a note one tick long, and a volume change.
fn single() -> MidiFile {
    MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 1_000,
        tracks: vec![Track {
            events: vec![
                at(0, on(9, 36)),
                at(250, off(9, 36)),
                at(333, on(9, 38)),
                at(334, off(9, 38)),
                at(
                    500,
                    EventKind::Controller {
                        channel: 9,
                        number: 7,
                        value: 100,
                    },
                ),
                at(1_000, on(9, 36)),
            ],
        }],
    }
}
