//! The test session through its public interface: each refusal of the loader with the field it
//! names, the small session rendered and played to the same bits whatever the blocks and periods,
//! a play that stops, a start while a play rolls, and the generator, the same to the byte each
//! time.

use std::fs::{self, File};
use std::io::{BufWriter, Cursor};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use magnolia_engine::offline::OfflineBackend;
use magnolia_engine::time::{MusicalPosition, SamplePosition, place};
use magnolia_engine::wav::{SampleFormat, WavError, WavWriter, decode, read_info};
use magnolia_session::file::{self, FileError, Problem};
use magnolia_session::generate::{Kind, generate};
use magnolia_session::load::{
    AudioProblem, MAX_FILE_BYTES, MEMORY_BUDGET, MemoryEstimate, MidiProblem,
};
use magnolia_session::midi_file::{self, EventKind, Format, MidiFile, Track, TrackEvent};
use magnolia_session::{LoadError, RenderError, Session, SessionPlayer, render, render_to_file};
use serde_json::{Value, json};

/// A folder of its own under the system's temporary folder, removed with everything in it when
/// dropped.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "magnolia-session-test-{name}-{}-{number}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// Sessions built by hand.

/// Writes a WAV file of `frames` frames of a quiet ramp, 16-bit, into `dir`.
fn write_wav(dir: &TempDir, name: &str, channels: u16, rate: u32, frames: usize) {
    let path = dir.path().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = BufWriter::new(File::create(path).unwrap());
    let mut writer = WavWriter::new(file, SampleFormat::Int16, channels, rate).unwrap();
    let samples: Vec<f32> = (0..frames * usize::from(channels))
        .map(|index| (index % 100) as f32 / 1_000.0)
        .collect();
    writer.write_frames(&samples).unwrap();
    writer.finish().unwrap();
}

/// Writes a MIDI file of one note into `dir`.
fn write_midi(dir: &TempDir, name: &str) {
    let note = |tick, kind| TrackEvent { tick, kind };
    let file = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 960,
        tracks: vec![Track {
            events: vec![
                note(
                    0,
                    EventKind::NoteOn {
                        channel: 0,
                        key: 60,
                        velocity: 100,
                    },
                ),
                note(
                    960,
                    EventKind::NoteOff {
                        channel: 0,
                        key: 60,
                        velocity: 0,
                    },
                ),
            ],
        }],
    };
    let path = dir.path().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, midi_file::write(&file)).unwrap();
}

/// A session that loads: a mono audio track of one clip into a stereo bus, through a latent
/// insert, and a MIDI track; with its files written into `dir`.
fn valid_session(dir: &TempDir) -> Value {
    write_wav(dir, "audio/voice.wav", 1, 48_000, 48_000);
    write_midi(dir, "midi/bass.mid");
    json!({
        "format": "magnolia-test-session", "version": 1,
        "sample_rate": 48000,
        "tempo": { "origin": 0, "changes": [ { "tick": 0, "bpm": [140, 1] } ],
                   "meters": [ { "bar": 0, "beats": 4, "beat_unit": 4 } ] },
        "master": { "channels": 2, "inserts": [] },
        "buses": [ { "name": "vocals", "channels": 2,
                     "inserts": [ { "test_latency": { "samples": 64 } } ],
                     "output": "master", "gain": 1.0 } ],
        "tracks": [
            { "name": "voice", "kind": "audio", "channels": 1,
              "clips": [ { "file": "audio/voice.wav", "start": 0, "file_offset": 0,
                           "length": 24000, "gain": 1.0 } ],
              "inserts": [], "output": "vocals", "gain": 1.0,
              "sends": [ { "to": "master", "gain": 0.25 } ] },
            { "name": "bass", "kind": "midi", "instrument": { "test_synth": {} },
              "clips": [ { "file": "midi/bass.mid", "start": 0, "length": 9676800 } ],
              "inserts": [], "output": "master", "gain": 1.0, "sends": [] }
        ]
    })
}

/// Opens `session`, written as `session.json` into `dir`.
fn open(dir: &TempDir, session: &Value) -> Result<Session, LoadError> {
    let path = dir.path().join("session.json");
    fs::write(&path, serde_json::to_string_pretty(session).unwrap()).unwrap();
    Session::open(path)
}

/// The error of a session refused for its file's content.
fn refused(dir: &TempDir, session: &Value) -> FileError {
    match open(dir, session) {
        Err(LoadError::File { error, .. }) => {
            let display = error.to_string();
            assert!(
                display.starts_with(error.field()),
                "{display} names its field first"
            );
            error
        }
        other => panic!(
            "expected a refusal of the file, got {:?}",
            other.map(|_| ())
        ),
    }
}

#[test]
fn a_valid_session_opens() {
    let dir = TempDir::new("valid");
    let session = open(&dir, &valid_session(&dir)).unwrap();
    assert_eq!(session.sample_rate().hertz(), 48_000);
    assert_eq!(session.master_channels(), 2);
    // The MIDI clip ends after 2 quarters at 140 BPM: 41,142.86 samples, placed on the next.
    assert_eq!(session.last_sample(), SamplePosition(41_143));
}

#[test]
fn an_unknown_field_is_refused_by_its_path() {
    let dir = TempDir::new("unknown-field");
    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]["colour"] = json!("red");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[0].clips[0].colour");
    assert!(matches!(error.problem(), Problem::Json(_)));
    assert!(
        error.to_string().contains("unknown field `colour`"),
        "{error}"
    );
}

#[test]
fn a_newer_version_is_refused_as_such() {
    let dir = TempDir::new("version");
    let mut session = valid_session(&dir);
    session["version"] = json!(2);
    // A field the newer version might add does not hide the version.
    session["tracks"][0]["colour"] = json!("red");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "version");
    assert!(matches!(error.problem(), Problem::UnsupportedVersion(2)));
}

#[test]
fn overlapping_audio_clips_are_refused() {
    let dir = TempDir::new("overlap");
    let mut session = valid_session(&dir);
    // The second clip starts at sample 12,000, inside the first: a sample is 18,816,000 time units.
    session["tracks"][0]["clips"] = json!([
        { "file": "audio/voice.wav", "start": 12_000_i64 * 18_816_000, "file_offset": 0,
          "length": 100, "gain": 1.0 },
        { "file": "audio/voice.wav", "start": 0, "file_offset": 0, "length": 24000, "gain": 1.0 },
        { "file": "audio/voice.wav", "start": 24_000_i64 * 18_816_000, "file_offset": 0,
          "length": 100, "gain": 1.0 }
    ]);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[0].clips[0].start");
    assert!(
        matches!(error.problem(), Problem::ClipsOverlap { earlier: 1 }),
        "{error}"
    );
}

#[test]
fn a_missing_file_is_refused_with_its_path() {
    let dir = TempDir::new("missing");
    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]["file"] = json!("audio/missing.wav");
    let Err(LoadError::Audio {
        field,
        path,
        problem: AudioProblem::Unreadable(WavError::Io(error)),
    }) = open(&dir, &session)
    else {
        panic!("a missing audio file is refused");
    };
    assert_eq!(field, "tracks[0].clips[0].file");
    assert_eq!(path, dir.path().join("audio/missing.wav"));
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

    let mut session = valid_session(&dir);
    session["tracks"][1]["clips"][0]["file"] = json!("midi/missing.mid");
    let Err(LoadError::Midi {
        field,
        problem: MidiProblem::Unreadable(_),
        ..
    }) = open(&dir, &session)
    else {
        panic!("a missing MIDI file is refused");
    };
    assert_eq!(field, "tracks[1].clips[0].file");
}

#[test]
fn a_file_at_another_rate_is_refused() {
    let dir = TempDir::new("rate");
    let mut session = valid_session(&dir);
    write_wav(&dir, "audio/cd.wav", 1, 44_100, 48_000);
    session["tracks"][0]["clips"][0]["file"] = json!("audio/cd.wav");
    let Err(LoadError::Audio { field, problem, .. }) = open(&dir, &session) else {
        panic!("a file at 44.1 kHz in a session at 48 kHz is refused");
    };
    assert_eq!(field, "tracks[0].clips[0].file");
    assert!(matches!(
        problem,
        AudioProblem::RateMismatch {
            file_rate: 44_100,
            session_rate: 48_000
        }
    ));
}

#[test]
fn channels_that_do_not_match_are_refused() {
    let dir = TempDir::new("channels");
    // A stereo file on a mono track.
    let mut session = valid_session(&dir);
    write_wav(&dir, "audio/stereo.wav", 2, 48_000, 48_000);
    session["tracks"][0]["clips"][0]["file"] = json!("audio/stereo.wav");
    let Err(LoadError::Audio { problem, .. }) = open(&dir, &session) else {
        panic!("a stereo file on a mono track is refused");
    };
    assert!(matches!(
        problem,
        AudioProblem::ChannelMismatch {
            file_channels: 2,
            track_channels: 1
        }
    ));

    // A stereo instrument into a mono bus; mono into stereo is fine, as the valid session shows.
    let mut session = valid_session(&dir);
    session["buses"][0]["channels"] = json!(1);
    session["tracks"][0]["output"] = json!("master");
    session["tracks"][1]["output"] = json!("vocals");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[1].output");
    assert!(matches!(
        error.problem(),
        Problem::ChannelMismatch { from: 2, to: 1 }
    ));

    // Mono into a bus of four.
    let mut session = valid_session(&dir);
    session["buses"][0]["channels"] = json!(4);
    session["buses"][0]["output"] = json!("master");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "buses[0].output");
    assert!(matches!(
        error.problem(),
        Problem::ChannelMismatch { from: 4, to: 2 }
    ));
}

#[test]
fn a_cycle_of_buses_is_refused() {
    let dir = TempDir::new("cycle");
    let mut session = valid_session(&dir);
    session["buses"] = json!([
        { "name": "vocals", "channels": 2, "inserts": [], "output": "a", "gain": 1.0 },
        { "name": "a", "channels": 2, "inserts": [], "output": "b", "gain": 1.0 },
        { "name": "b", "channels": 2, "inserts": [], "output": "a", "gain": 1.0 }
    ]);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "buses[2].output");
    let Problem::BusCycle(buses) = error.problem() else {
        panic!("{error}")
    };
    assert_eq!(buses, &["a", "b"]);

    session["buses"][1]["output"] = json!("a");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "buses[1].output", "a bus into itself");
}

#[test]
fn a_clip_past_its_files_end_is_refused() {
    let dir = TempDir::new("past-end");
    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]["file_offset"] = json!(40_000);
    session["tracks"][0]["clips"][0]["length"] = json!(8_001);
    let Err(LoadError::Audio { field, problem, .. }) = open(&dir, &session) else {
        panic!("a clip past its file's end is refused");
    };
    assert_eq!(field, "tracks[0].clips[0].file");
    assert!(matches!(
        problem,
        AudioProblem::PastFileEnd {
            clip_end: 48_001,
            file_frames: 48_000
        }
    ));
    session["tracks"][0]["clips"][0]["length"] = json!(8_000);
    open(&dir, &session).unwrap();
}

#[test]
fn a_tempo_of_zero_is_refused() {
    let dir = TempDir::new("tempo");
    let mut session = valid_session(&dir);
    session["tempo"]["changes"][0]["bpm"] = json!([0, 1]);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tempo.changes[0].bpm");
    assert!(matches!(error.problem(), Problem::OutOfRange { .. }));

    session["tempo"]["changes"][0]["bpm"] = json!([140, 0]);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tempo.changes[0].bpm", "a denominator of 0");
}

#[test]
fn values_out_of_their_range_are_refused_by_their_path() {
    let dir = TempDir::new("ranges");
    type Change = fn(&mut Value);
    let cases: [(&str, Change); 7] = [
        ("tracks[0].channels", |session| {
            session["tracks"][0]["channels"] = json!(70_000)
        }),
        ("sample_rate", |session| {
            session["sample_rate"] = json!(44_056)
        }),
        ("buses[0].inserts[0].test_latency.samples", |session| {
            session["buses"][0]["inserts"][0]["test_latency"]["samples"] = json!(65_537)
        }),
        ("tracks[0].clips[0].gain", |session| {
            session["tracks"][0]["clips"][0]["gain"] = json!(1e40)
        }),
        ("tracks[0].clips[0].start", |session| {
            session["tracks"][0]["clips"][0]["start"] = json!(-1)
        }),
        ("tracks[1].clips[0].length", |session| {
            session["tracks"][1]["clips"][0]["length"] = json!(0)
        }),
        ("master.channels", |session| {
            session["master"]["channels"] = json!(1)
        }),
    ];
    for (expected, change) in cases {
        let mut session = valid_session(&dir);
        change(&mut session);
        assert_eq!(refused(&dir, &session).field(), expected);
    }
}

#[test]
fn names_and_paths_are_checked() {
    let dir = TempDir::new("names");
    let mut session = valid_session(&dir);
    session["tracks"][0]["sends"][0]["to"] = json!("reverb");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[0].sends[0].to");
    assert!(matches!(error.problem(), Problem::UnknownDestination(name) if name == "reverb"));

    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]["file"] = json!("/tmp/voice.wav");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[0].clips[0].file");
    assert!(matches!(error.problem(), Problem::AbsolutePath));

    // A MIDI track takes no channels, an audio clip needs its gain.
    let mut session = valid_session(&dir);
    session["tracks"][1]["channels"] = json!(2);
    assert_eq!(refused(&dir, &session).field(), "tracks[1].channels");
    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]
        .as_object_mut()
        .unwrap()
        .remove("gain");
    assert_eq!(refused(&dir, &session).field(), "tracks[0].clips[0].gain");
}

#[test]
fn a_session_file_past_its_limit_is_refused_unread() {
    let dir = TempDir::new("large");
    let path = dir.path().join("session.json");
    // Sparse: it takes no room on disk.
    File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    assert!(matches!(
        Session::open(&path),
        Err(LoadError::TooLarge { .. })
    ));
}

#[test]
fn paths_stay_inside_the_sessions_folder() {
    let dir = TempDir::new("paths");
    for path in [
        "../voice.wav",
        "audio/../../voice.wav",
        "x/../audio/voice.wav",
    ] {
        let mut session = valid_session(&dir);
        session["tracks"][0]["clips"][0]["file"] = json!(path);
        let error = refused(&dir, &session);
        assert_eq!(error.field(), "tracks[0].clips[0].file", "{path}");
        assert!(
            matches!(error.problem(), Problem::PathLeavesFolder),
            "{path}: {error}"
        );
    }
    let mut session = valid_session(&dir);
    session["tracks"][1]["clips"][0]["file"] = json!("../bass.mid");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[1].clips[0].file");
    assert!(matches!(error.problem(), Problem::PathLeavesFolder));
    // `.` is no step at all: these name the files of the valid session.
    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]["file"] = json!("./audio/./voice.wav");
    session["tracks"][1]["clips"][0]["file"] = json!("midi//bass.mid");
    open(&dir, &session).unwrap();
}

#[test]
fn an_empty_path_and_bus_names_taken_are_refused() {
    let dir = TempDir::new("taken");
    let mut session = valid_session(&dir);
    session["tracks"][0]["clips"][0]["file"] = json!("");
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[0].clips[0].file");
    assert!(matches!(error.problem(), Problem::EmptyPath));

    let bus = |name: &str| {
        json!({
            "name": name, "channels": 2, "inserts": [], "output": "master", "gain": 1.0
        })
    };
    let mut session = valid_session(&dir);
    session["buses"] = json!([bus("vocals"), bus("master")]);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "buses[1].name");
    assert!(matches!(error.problem(), Problem::NameTaken(name) if name == "master"));

    session["buses"] = json!([bus("vocals"), bus("vocals")]);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "buses[1].name");
    assert!(matches!(error.problem(), Problem::NameTaken(name) if name == "vocals"));
}

/// A session of one mono audio track whose clips each play one frame of their own file, from
/// sample `index`, without files on disk: for the checks of `parse`.
fn session_of_audio_files(files: &[String]) -> String {
    let clips: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            json!({ "file": file, "start": index as i64 * 18_816_000, "file_offset": 0,
                    "length": 1, "gain": 1.0 })
        })
        .collect();
    json!({
        "format": "magnolia-test-session", "version": 1, "sample_rate": 48000,
        "tempo": { "origin": 0, "changes": [ { "tick": 0, "bpm": [140, 1] } ],
                   "meters": [ { "bar": 0, "beats": 4, "beat_unit": 4 } ] },
        "master": { "channels": 2, "inserts": [] },
        "buses": [],
        "tracks": [ { "name": "takes", "kind": "audio", "channels": 1, "clips": clips,
                      "inserts": [], "output": "master", "gain": 1.0, "sends": [] } ]
    })
    .to_string()
}

#[test]
fn a_session_reads_at_most_512_audio_files_each_counted_once() {
    let mut files: Vec<String> = (0..512).map(|index| format!("a{index}.wav")).collect();
    file::parse(&session_of_audio_files(&files)).unwrap();
    // The same files again, spelt otherwise, are no more files.
    files.extend(["./a0.wav".to_owned(), "a1.wav".to_owned()]);
    file::parse(&session_of_audio_files(&files)).unwrap();
    files.push("a512.wav".to_owned());
    let error = file::parse(&session_of_audio_files(&files)).unwrap_err();
    assert_eq!(error.field(), "tracks[0].clips[514].file");
    assert!(matches!(error.problem(), Problem::TooManyAudioFiles));
}

#[test]
fn a_midi_file_past_its_limit_is_refused_unread() {
    let dir = TempDir::new("large-midi");
    let mut session = valid_session(&dir);
    let path = dir.path().join("midi/large.mid");
    // Sparse: it takes no room on disk.
    File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    session["tracks"][1]["clips"][0]["file"] = json!("midi/large.mid");
    let Err(LoadError::Midi {
        field,
        problem: MidiProblem::TooLarge,
        ..
    }) = open(&dir, &session)
    else {
        panic!("a MIDI file past 64 MiB is refused");
    };
    assert_eq!(field, "tracks[1].clips[0].file");
}

#[test]
fn a_midi_clip_ends_within_2_to_the_41_samples() {
    let dir = TempDir::new("long-midi");
    let mut session = valid_session(&dir);
    // At 1 BPM, 2^60 ticks last 450,000 years.
    session["tempo"]["changes"][0]["bpm"] = json!([1, 1]);
    session["tracks"][1]["clips"][0]["length"] = json!(1_u64 << 60);
    let error = refused(&dir, &session);
    assert_eq!(error.field(), "tracks[1].clips[0].length");
    assert!(matches!(error.problem(), Problem::OutOfRange { .. }));
    // 2^41 samples at 140 BPM are 2^41 / 48,000 * 140 / 60 quarters, about 1.07e8.
    session["tempo"]["changes"][0]["bpm"] = json!([140, 1]);
    session["tracks"][1]["clips"][0]["length"] = json!(100_000_000_u64 * 4_838_400);
    open(&dir, &session).unwrap();
}

// The memory budget.

/// The estimate of a session refused for asking more memory than the budget.
fn over_budget(result: Result<Session, LoadError>) -> MemoryEstimate {
    match result {
        Err(LoadError::OverBudget(estimate)) => {
            assert!(estimate.total() > MEMORY_BUDGET);
            estimate
        }
        Err(error) => panic!("expected a refusal for memory, got: {error}"),
        Ok(_) => panic!("expected a refusal for memory, the session opened"),
    }
}

/// A session of nothing but `master_channels` playback channels.
fn bare_session(master_channels: u16) -> Value {
    json!({
        "format": "magnolia-test-session", "version": 1, "sample_rate": 48000,
        "tempo": { "origin": 0, "changes": [ { "tick": 0, "bpm": [140, 1] } ],
                   "meters": [ { "bar": 0, "beats": 4, "beat_unit": 4 } ] },
        "master": { "channels": master_channels, "inserts": [] },
        "buses": [], "tracks": []
    })
}

/// The frames of an audio track's stream: 10 s at 48 kHz in blocks of 4,096, and two more.
const STREAM_FRAMES: u64 = (480_000_u64.div_ceil(4_096) + 2) * 4_096;

#[test]
fn the_estimate_counts_streams_and_delays_as_the_engine_allocates_them() {
    let dir = TempDir::new("estimate");
    generate(Kind::Small, dir.path()).unwrap();
    let session = Session::open(dir.path().join("session.json")).unwrap();
    let estimate = session.memory_estimate();
    // Two mono tracks and two stereo ones; a bus of two channels 64 samples late, and a mono
    // track 300 samples late; no delay line, as no track's sends go later than its output.
    assert_eq!(estimate.streams, 6 * STREAM_FRAMES * 4);
    assert_eq!(estimate.delays, (2 * 64 + 300) * 4);
    assert!(estimate.total() < MEMORY_BUDGET / 10, "{estimate}");
}

#[test]
fn wide_audio_tracks_are_refused_before_their_streams_are_made() {
    let dir = TempDir::new("wide-tracks");
    write_wav(&dir, "audio/wide.wav", 1_024, 48_000, 1);
    let mut session = bare_session(1_024);
    session["tracks"] = json!([{
        "name": "wide", "kind": "audio", "channels": 1024,
        "clips": [ { "file": "audio/wide.wav", "start": 0, "file_offset": 0, "length": 1,
                     "gain": 1.0 } ],
        "inserts": [], "output": "master", "gain": 1.0, "sends": []
    }]);
    let estimate = over_budget(open(&dir, &session));
    assert_eq!(estimate.streams, 1_024 * STREAM_FRAMES * 4);
    let message = LoadError::OverBudget(estimate).to_string();
    // 1,024 channels of 491,520 frames of 4 bytes: 1.875 GiB.
    assert!(message.contains("1.9 GiB of audio streams"), "{message}");
}

#[test]
fn long_latencies_on_a_wide_bus_are_refused() {
    let dir = TempDir::new("wide-latency");
    let mut session = bare_session(1_024);
    session["buses"] = json!([{
        "name": "wide", "channels": 1024,
        "inserts": vec![json!({ "test_latency": { "samples": 65536 } }); 4],
        "output": "master", "gain": 1.0
    }]);
    let estimate = over_budget(open(&dir, &session));
    assert_eq!(estimate.delays, 4 * 65_536 * 1_024 * 4);
}

#[test]
fn a_send_later_than_its_output_counts_its_delay_line() {
    let dir = TempDir::new("delay-line");
    let mut session = valid_session(&dir);
    // The voice goes to the master, and sends to a bus of two channels 64 samples late: the
    // engine delays its path to the master by 64 samples, on the master's two channels.
    session["tracks"][0]["output"] = json!("master");
    session["tracks"][0]["sends"] = json!([ { "to": "vocals", "gain": 0.5 } ]);
    let estimate = open(&dir, &session).unwrap().memory_estimate();
    assert_eq!(estimate.delays, (2 * 64 + 2 * 64) * 4);
}

/// Writes a MIDI file of `notes` sixteenth notes at 960 ticks a quarter: twice as many events.
fn write_dense_midi(dir: &TempDir, name: &str, notes: u64) {
    let mut events = Vec::new();
    for index in 0..notes {
        let (channel, key) = (0, 60);
        events.push(TrackEvent {
            tick: index * 240,
            kind: EventKind::NoteOn {
                channel,
                key,
                velocity: 100,
            },
        });
        events.push(TrackEvent {
            tick: index * 240 + 120,
            kind: EventKind::NoteOff {
                channel,
                key,
                velocity: 0,
            },
        });
    }
    let file = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 960,
        tracks: vec![Track { events }],
    };
    let path = dir.path().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, midi_file::write(&file)).unwrap();
}

/// A session of one MIDI track whose clips play `files`, each a quarter long.
fn midi_clips_session(files: &[&str]) -> Value {
    let clips: Vec<Value> = files
        .iter()
        .map(|file| json!({ "file": file, "start": 0, "length": 4_838_400 }))
        .collect();
    let mut session = bare_session(2);
    session["tracks"] = json!([{
        "name": "dense", "kind": "midi", "instrument": { "test_synth": {} },
        "clips": clips, "inserts": [], "output": "master", "gain": 1.0, "sends": []
    }]);
    session
}

#[test]
fn many_clips_of_one_dense_midi_file_are_refused_before_they_expand() {
    let dir = TempDir::new("dense-midi");
    write_dense_midi(&dir, "midi/dense.mid", 20_000);
    let estimate = over_budget(open(&dir, &midi_clips_session(&["midi/dense.mid"; 400])));
    assert!(estimate.midi > MEMORY_BUDGET, "{estimate}");
}

#[test]
fn a_midi_file_is_read_once_however_its_clips_spell_it() {
    let dir = TempDir::new("spellings");
    write_dense_midi(&dir, "midi/dense.mid", 1_000);
    let one = open(&dir, &midi_clips_session(&["midi/dense.mid"]))
        .unwrap()
        .memory_estimate()
        .midi;
    let three = open(
        &dir,
        &midi_clips_session(&["midi/dense.mid", "./midi/dense.mid", "midi/./dense.mid"]),
    )
    .unwrap()
    .memory_estimate()
    .midi;
    // One file of 2,000 events, parsed once; each clip expands all its events.
    let parsed = 2_000 * size_of::<TrackEvent>() as u64;
    let per_clip = one - parsed;
    assert!(per_clip > 0);
    assert_eq!(three, parsed + 3 * per_clip);
}

// The small session, rendered and played.

/// Renders `session` in blocks of `block_frames`, and returns each channel and the report.
fn render_channels(session: &Session, block_frames: usize) -> (Vec<Vec<f32>>, u64, u64) {
    let mut bytes = Cursor::new(Vec::new());
    let report = render(session, block_frames, &mut bytes).unwrap();
    assert_eq!(report.underruns, 0, "blocks of {block_frames}");
    let bytes = bytes.into_inner();
    let info = read_info(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(info.sample_format, SampleFormat::Float32);
    assert_eq!(info.frames, report.frames);
    let channels = usize::from(info.channels);
    let mut interleaved = vec![0.0; info.frames as usize * channels];
    decode(
        info.sample_format,
        info.channels,
        &bytes[info.data_offset as usize..],
        &mut interleaved,
    );
    let split = (0..channels)
        .map(|channel| {
            interleaved
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect()
        })
        .collect();
    (split, report.frames, report.largest_compensation)
}

/// Says where two signals differ, if they do, bit for bit.
fn assert_same_bits(actual: &[Vec<f32>], expected: &[Vec<f32>], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: channels");
    for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual.len(), expected.len(), "{what}: frames");
        if let Some(frame) = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| actual.to_bits() != expected.to_bits())
        {
            panic!(
                "{what}: channel {channel}, frame {frame}: {} where {} was rendered",
                actual[frame], expected[frame]
            );
        }
    }
}

/// SplitMix64, for the periods' lengths.
struct Periods(u64);

impl Periods {
    /// A length from 1 to 5,000 frames.
    fn next(&mut self) -> usize {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as usize % 5_000 + 1
    }
}

#[test]
fn the_small_session_plays_to_the_bit_what_it_renders() {
    let dir = TempDir::new("null");
    generate(Kind::Small, dir.path()).unwrap();
    let session = Session::open(dir.path().join("session.json")).unwrap();

    let (rendered, frames, compensation) = render_channels(&session, 4_096);
    let last = u64::try_from(session.last_sample().0).unwrap();
    assert_eq!(
        frames,
        last + 48_000,
        "to the last clip's end and a second of tail"
    );
    assert_eq!(
        compensation, 300,
        "the double's latency, the session's largest"
    );
    assert!(
        rendered
            .iter()
            .all(|channel| channel.iter().any(|&sample| sample != 0.0)),
        "every channel sounds"
    );
    for block in [64, 1_000] {
        let (again, _, _) = render_channels(&session, block);
        assert_same_bits(&again, &rendered, &format!("a render in blocks of {block}"));
    }

    let (mut player, processor) = SessionPlayer::new(&session).unwrap();
    let mut backend = OfflineBackend::new(processor);
    player
        .start(SamplePosition(0), Duration::from_secs(10))
        .unwrap();
    let mut periods = Periods(0x5E55_1011);
    let mut played = vec![Vec::new(); rendered.len()];
    while (played[0].len() as u64) < compensation + frames {
        assert!(
            player.reader().wait_idle(Duration::from_secs(10)),
            "the disk reader filled what it was asked"
        );
        let period = backend.play_period(periods.next(), &[]);
        for (all, channel) in played.iter_mut().zip(period) {
            all.extend_from_slice(channel);
        }
    }
    let audible = player.stats().play_start().unwrap().audible_at.0 as usize;
    assert_eq!(
        audible as u64, compensation,
        "the play started at the first period"
    );
    let heard: Vec<Vec<f32>> = played
        .iter()
        .map(|channel| channel[audible..audible + frames as usize].to_vec())
        .collect();
    assert_same_bits(&heard, &rendered, "the play in random periods");

    let report = player.report();
    assert_eq!(report.disk_underruns, 0);
    assert_eq!(report.events_dropped, 0);
    // Each stream holds its whole track, shorter than the read-ahead: no player ever found its
    // stream short of the track's end, so none has a mark.
    let names: Vec<&str> = report
        .low_water
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["vocal", "double", "drum loop", "keys"]);
    assert!(
        report.low_water.iter().all(|(_, mark)| mark.is_none()),
        "{report}"
    );
    assert!(report.to_string().contains("low-water mark of \"vocal\""));

    // A stop ends every path: after the fades and the releases, silence.
    player.stop().unwrap();
    let mut last_period = Vec::new();
    for _ in 0..50 {
        last_period = backend.play_period(1_024, &[]).to_vec();
    }
    assert!(
        last_period
            .iter()
            .all(|channel| channel.iter().all(|&sample| sample == 0.0)),
        "silence after a stop"
    );
}

// The graph a session builds, through impulses.

/// Writes a WAV file of floats, silent but for `impulses`, each a frame, a channel and a value.
fn write_impulses(
    dir: &TempDir,
    name: &str,
    channels: u16,
    frames: usize,
    impulses: &[(usize, usize, f32)],
) {
    let mut samples = vec![0.0_f32; frames * usize::from(channels)];
    for &(frame, channel, value) in impulses {
        samples[frame * usize::from(channels) + channel] = value;
    }
    let path = dir.path().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = BufWriter::new(File::create(path).unwrap());
    let mut writer = WavWriter::new(file, SampleFormat::Float32, channels, 48_000).unwrap();
    writer.write_frames(&samples).unwrap();
    writer.finish().unwrap();
}

/// Each sample that is not zero, with its frame, from `from` up to `to`.
fn sounding(channel: &[f32], from: usize, to: usize) -> Vec<(usize, f32)> {
    channel[from..to]
        .iter()
        .enumerate()
        .filter(|(_, sample)| **sample != 0.0)
        .map(|(frame, sample)| (from + frame, *sample))
        .collect()
}

#[test]
fn impulses_take_the_paths_the_session_describes() {
    let dir = TempDir::new("impulses");
    // A mono impulse at 1,000; a stereo one at 3,000 on the left; a mono one at the file's first
    // frame, its clip starting one time unit after sample 5,000.
    write_impulses(&dir, "audio/t1.wav", 1, 48_000, &[(1_000, 0, 1.0)]);
    write_impulses(&dir, "audio/t2.wav", 2, 48_000, &[(3_000, 0, 1.0)]);
    write_impulses(&dir, "audio/t3.wav", 1, 1_000, &[(0, 0, 1.0)]);
    // A note in a file of 1,000 ticks a quarter, at its tick 1,001: a quarter and 4.8384 ticks of
    // the time model, placed on the tick at or after.
    let midi = MidiFile {
        format: Format::SingleTrack,
        ticks_per_quarter: 1_000,
        tracks: vec![Track {
            events: vec![
                TrackEvent {
                    tick: 1_001,
                    kind: EventKind::NoteOn {
                        channel: 0,
                        key: 69,
                        velocity: 127,
                    },
                },
                TrackEvent {
                    tick: 1_500,
                    kind: EventKind::NoteOff {
                        channel: 0,
                        key: 69,
                        velocity: 0,
                    },
                },
            ],
        }],
    };
    fs::create_dir_all(dir.path().join("midi")).unwrap();
    fs::write(dir.path().join("midi/n.mid"), midi_file::write(&midi)).unwrap();
    let session = json!({
        "format": "magnolia-test-session", "version": 1, "sample_rate": 48000,
        "tempo": { "origin": 0, "changes": [ { "tick": 0, "bpm": [140, 1] } ],
                   "meters": [ { "bar": 0, "beats": 4, "beat_unit": 4 } ] },
        "master": { "channels": 2, "inserts": [] },
        "buses": [
            { "name": "late", "channels": 2, "inserts": [ { "test_latency": { "samples": 500 } } ],
              "output": "master", "gain": 1.0 },
            { "name": "inner", "channels": 2, "inserts": [ { "test_latency": { "samples": 200 } } ],
              "output": "outer", "gain": 1.0 },
            { "name": "outer", "channels": 2, "inserts": [], "output": "master", "gain": 0.5 }
        ],
        "tracks": [
            { "name": "t1", "kind": "audio", "channels": 1,
              "clips": [ { "file": "audio/t1.wav", "start": 0, "file_offset": 0, "length": 48000,
                           "gain": 1.0 } ],
              "inserts": [], "output": "master", "gain": 0.5,
              "sends": [ { "to": "late", "gain": 0.25 } ] },
            { "name": "t2", "kind": "audio", "channels": 2,
              "clips": [ { "file": "audio/t2.wav", "start": 0, "file_offset": 0, "length": 48000,
                           "gain": 1.0 } ],
              "inserts": [], "output": "inner", "gain": 1.0, "sends": [] },
            { "name": "t3", "kind": "audio", "channels": 1,
              "clips": [ { "file": "audio/t3.wav", "start": 5_000_i64 * 18_816_000 + 1,
                           "file_offset": 0, "length": 1000, "gain": 1.0 } ],
              "inserts": [], "output": "master", "gain": 1.0, "sends": [] },
            { "name": "m", "kind": "midi", "instrument": { "test_synth": {} },
              "clips": [ { "file": "midi/n.mid", "start": 0, "length": 4_838_400 * 4 } ],
              "inserts": [], "output": "outer", "gain": 1.0, "sends": [] }
        ]
    });
    let session = open(&dir, &session).unwrap();
    let (out, _, compensation) = render_channels(&session, 4_096);
    // The late bus, 500 samples, is the longest path; every path reaches the output on time.
    assert_eq!(compensation, 500);
    // The mono t1 feeds both channels: 0.5 to the master, and its send, taken after its gain,
    // 0.5 * 0.25 through the late bus, compensated onto the same sample. The stereo t2 goes
    // through the inner bus into the outer one, at 0.5, on its left channel alone. t3's clip,
    // starting between samples 5,000 and 5,001, plays on 5,001, on both channels.
    let expected_left = [(1_000, 0.5 + 0.5 * 0.25), (3_000, 0.5), (5_001, 1.0)];
    let expected_right = [(1_000, 0.5 + 0.5 * 0.25), (5_001, 1.0)];
    // The note: tick 1,001 of 1,000 a quarter is tick ceil(1,001 * 4,838.4) = 4,843,239, which
    // places on sample 20,593 at 140 BPM.
    let tick = (1_001_u64 * 4_838_400).div_ceil(1_000) as i64;
    assert_eq!(tick, 4_843_239);
    let note = place(
        MusicalPosition(tick),
        &file::TempoSection {
            origin: 0,
            changes: vec![file::TempoChange {
                tick: 0,
                bpm: [140, 1],
            }],
            meters: vec![file::MeterChange {
                bar: 0,
                beats: 4,
                beat_unit: 4,
            }],
        }
        .map()
        .unwrap(),
        &session.sample_rate(),
    )
    .unwrap();
    assert_eq!(note, SamplePosition(20_593));
    for (channel, expected) in [&expected_left[..], &expected_right[..]]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            sounding(&out[channel], 0, 20_593),
            expected,
            "channel {channel} before the note"
        );
        let first = sounding(&out[channel], 20_593, out[channel].len());
        assert_eq!(
            first.first().map(|&(frame, _)| frame),
            Some(20_593),
            "the note starts on its sample"
        );
    }
}

#[test]
fn a_compensation_longer_than_the_read_ahead_aligns_its_paths() {
    let dir = TempDir::new("long-compensation");
    write_impulses(&dir, "audio/a.wav", 1, 960_000, &[(600_000, 0, 1.0)]);
    let track = |name: &str, inserts: Value| {
        json!({ "name": name, "kind": "audio", "channels": 1,
                "clips": [ { "file": "audio/a.wav", "start": 0, "file_offset": 0,
                             "length": 960000, "gain": 1.0 } ],
                "inserts": inserts, "output": "master", "gain": 1.0, "sends": [] })
    };
    let mut session = bare_session(2);
    // 8 × 65,536 = 524,288 samples, past the read-ahead of 480,000.
    session["tracks"] = json!([
        track(
            "latent",
            json!(vec![json!({ "test_latency": { "samples": 65536 } }); 8])
        ),
        track("plain", json!([]))
    ]);
    let session = open(&dir, &session).unwrap();
    let (out, _, compensation) = render_channels(&session, 4_096);
    assert_eq!(compensation, 524_288);
    for channel in &out {
        assert_eq!(sounding(channel, 0, channel.len()), [(600_000, 2.0)]);
    }
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test's thread paces its periods as a device would; it stands for a backend"
)]
fn a_start_while_a_play_rolls_stops_that_play_first() {
    let dir = TempDir::new("restart");
    generate(Kind::Small, dir.path()).unwrap();
    let session = Session::open(dir.path().join("session.json")).unwrap();
    let (mut player, processor) = SessionPlayer::new(&session).unwrap();
    let mut backend = OfflineBackend::new(processor);
    assert_eq!(
        player
            .start(SamplePosition(0), Duration::from_secs(10))
            .unwrap(),
        1
    );
    backend.play(&[1_024; 20]);
    // A rolling play keeps the slots of everything it may still read, which a cue from 4 s in
    // needs too: without a stop, it would hold them until it reached them, seconds later.
    let playing = AtomicBool::new(true);
    let started = thread::scope(|scope| {
        scope.spawn(|| {
            // A device's periods of 256 frames, at its pace, while the player starts again.
            while playing.load(Ordering::Acquire) {
                backend.play_period(256, &[]);
                thread::sleep(Duration::from_micros(5_333));
            }
        });
        let started = player.start(SamplePosition(192_000), Duration::from_secs(2));
        playing.store(false, Ordering::Release);
        started
    });
    assert_eq!(started.unwrap(), 2);
    backend.play(&[1_024; 8]);
    assert_eq!(
        player.stats().play_start().unwrap().sequence,
        2,
        "the second play started"
    );
}

#[test]
fn the_generator_writes_the_same_bytes_each_time() {
    let first = TempDir::new("generate-first");
    let second = TempDir::new("generate-second");
    let made = generate(Kind::Small, first.path()).unwrap();
    assert_eq!(generate(Kind::Small, second.path()).unwrap(), made);
    assert_eq!((made.audio_files, made.midi_files), (4, 4));

    let files = |dir: &TempDir| {
        let mut files = Vec::new();
        for sub in ["", "audio", "midi"] {
            for entry in fs::read_dir(dir.path().join(sub)).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    files.push(path.strip_prefix(dir.path()).unwrap().to_path_buf());
                }
            }
        }
        files.sort();
        files
    };
    let names = files(&first);
    assert_eq!(names, files(&second));
    assert_eq!(names.len(), 9);
    let mut bytes = 0;
    for name in &names {
        let (one, two) = (
            fs::read(first.path().join(name)).unwrap(),
            fs::read(second.path().join(name)).unwrap(),
        );
        assert!(
            one == two,
            "{} differs between two generations",
            name.display()
        );
        bytes += one.len() as u64;
    }
    assert_eq!(bytes, made.bytes);
}

#[test]
fn the_generator_writes_into_an_empty_folder_only() {
    let dir = TempDir::new("generate-full");
    fs::write(dir.path().join("keep.txt"), "the user's").unwrap();
    assert!(generate(Kind::Small, dir.path()).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "the user's"
    );
    assert!(!dir.path().join("audio").exists());
}

#[test]
fn a_render_into_a_file_appears_whole_or_not_at_all() {
    let dir = TempDir::new("render-file");
    generate(Kind::Small, dir.path().join("session").as_path()).unwrap();
    let session = Session::open(dir.path().join("session/session.json")).unwrap();
    let partial = dir.path().join(".out.wav.partial");

    // A render that fails leaves nothing behind.
    let out = dir.path().join("out.wav");
    assert!(matches!(
        render_to_file(&session, 0, &out),
        Err(RenderError::BlockOutOfRange { .. })
    ));
    assert!(!out.exists() && !partial.exists());

    let report = render_to_file(&session, 4_096, &out).unwrap();
    let info = read_info(&mut File::open(&out).unwrap()).unwrap();
    assert_eq!(info.frames, report.frames);
    assert!(!partial.exists());

    // A file in the way is never written over.
    let before = fs::read(&out).unwrap();
    assert!(matches!(
        render_to_file(&session, 4_096, &out),
        Err(RenderError::OutputExists(_))
    ));
    assert_eq!(fs::read(&out).unwrap(), before);
    assert!(!partial.exists());
}

#[test]
fn a_render_takes_blocks_up_to_half_the_read_ahead() {
    let dir = TempDir::new("render-blocks");
    generate(Kind::Small, dir.path()).unwrap();
    let session = Session::open(dir.path().join("session.json")).unwrap();
    for frames in [0, 240_001] {
        assert!(
            matches!(
                render(&session, frames, Cursor::new(Vec::new())),
                Err(RenderError::BlockOutOfRange { frames: refused, longest: 240_000 })
                    if refused == frames
            ),
            "{frames}"
        );
    }
    let (longest, _, _) = render_channels(&session, 240_000);
    let (usual, _, _) = render_channels(&session, 4_096);
    assert_same_bits(&longest, &usual, "a render in blocks of 240,000");
}

/// FNV-1a over each file's name, then its length and bytes, in the order of their names.
fn digest(dir: &Path) -> u64 {
    let mut files = Vec::new();
    for sub in ["", "audio", "midi"] {
        for entry in fs::read_dir(dir.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                files.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    files.sort();
    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    let mut eat = |bytes: &[u8]| {
        for &byte in bytes {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    for name in files {
        let bytes = fs::read(dir.join(&name)).unwrap();
        eat(name.to_string_lossy().as_bytes());
        eat(&(bytes.len() as u64).to_le_bytes());
        eat(&bytes);
    }
    hash
}

/// The digest of the small session's files, as this generator writes them on every machine.
///
/// It fails when the generator's output changes. If that change is meant (a new feature of the
/// small session, a change of the format or of the synthesis), look at the new files, check that
/// they differ only as meant, then put the new digest the failure prints here, in the same commit
/// as the change, and say so in its message. If it is not meant, the generator has lost its
/// determinism: a function whose precision differs between machines, or an order that does.
const SMALL_SESSION_DIGEST: u64 = 0xCC4B_07FD_B27C_153A;

#[test]
fn the_small_session_is_the_same_on_every_machine() {
    let dir = TempDir::new("digest");
    generate(Kind::Small, dir.path()).unwrap();
    let digest = digest(dir.path());
    assert_eq!(
        digest, SMALL_SESSION_DIGEST,
        "the small session's digest is {digest:#018X}"
    );
}
