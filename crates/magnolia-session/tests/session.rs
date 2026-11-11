//! The test session through its public interface: each refusal of the loader with the field it
//! names, the small session rendered and played to the same bits whatever the blocks and periods,
//! a play that stops, a start while a play rolls, and the generator, the same to the byte each
//! time.

use std::fs::{self, File};
use std::io::{BufWriter, Cursor};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use magnolia_engine::offline::OfflineBackend;
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{SampleFormat, WavError, WavWriter, decode, read_info};
use magnolia_session::file::{FileError, Problem};
use magnolia_session::generate::{Kind, generate};
use magnolia_session::load::{AudioProblem, MAX_FILE_BYTES, MidiProblem};
use magnolia_session::midi_file::{self, EventKind, Format, MidiFile, Track, TrackEvent};
use magnolia_session::{LoadError, Session, SessionPlayer, render};
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
