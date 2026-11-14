//! `magnolia record` on a JACK2 server of the test's own, on the dummy driver: it leaves one
//! finished take per armed track, placed at `--from`, as long as the span played, and prints the
//! round trip with the dummy's figures; a folder another recorder holds is a plain error, and a
//! recording killed mid-take is repaired by `magnolia recover`. `magnolia recover` repairs a take
//! cut by hand and leaves a third-party WAV file untouched. Every take goes into a temporary
//! folder of the test's own.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use magnolia_engine::wav::{Bext, SampleFormat, WavInfo, WavWriter, decode, read_info};
use magnolia_session::file;
use magnolia_session::generate::{Kind, generate};

use crate::common::{Server, pause, refuse_pipewire};
use crate::support::{TempDir, magnolia, magnolia_binary, one_at_a_time};

/// The small session's armed input track, `mic`, is its ninth track: its first take's file.
const MIC_TAKE: &str = "9-mic-take1.wav";

/// Generates the small session into `dir/session`, makes the recording folder `dir/takes`, and
/// returns the arguments that record the session there on `server`.
fn record_small(dir: &TempDir, server: &str) -> Vec<OsString> {
    generate(Kind::Small, &dir.path().join("session")).unwrap();
    fs::create_dir(dir.path().join("takes")).unwrap();
    vec![
        "record".into(),
        dir.path().join("session/session.json").into(),
        dir.path().join("takes").into(),
        "--server".into(),
        server.into(),
    ]
}

/// The WAV files of `folder`, by name, in order.
fn wav_files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".wav"))
        .collect();
    names.sort();
    names
}

fn info_of(path: &Path) -> WavInfo {
    read_info(&mut File::open(path).unwrap()).unwrap()
}

#[test]
fn record_leaves_a_finished_take_at_from_and_reports_the_round_trip() {
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let dir = TempDir::new("record");
    let mut arguments = record_small(&dir, &server.name);
    arguments.extend(["--connect", "--from", "1", "--seconds", "2"].map(OsString::from));
    let ran = magnolia(&arguments);
    assert_eq!(ran.code, Some(0), "{}\n{}", ran.stdout, ran.stderr);
    let stdout = &ran.stdout;
    for expected in [
        "Recording ",
        "256 frames at 48000 Hz, from 1.000 s to 3.000 s.",
        "Armed: track 9 \"mic\", from capture ports 1.",
        "Connected 2 playback ports to the server's physical ones.",
        "Connected 1 capture ports to the server's physical ones.",
        // Connected, the dummy driver's capture ports report one period, its playback ports two,
        // for its asynchronous mode's buffering.
        "  capture: 256 frames (5.333 ms), from the input to Magnolia",
        "  playback: 512 frames (10.667 ms), from Magnolia to the output",
        "  round trip: 768 frames (16.000 ms)",
        "  extra: 0 frames (0.000 ms)",
        "  monitoring path: 132 frames (2.750 ms)",
        "Takes are placed by 768 frames (16.000 ms)",
        "Takes: 1",
        "9-mic-take1.wav: track 9, from timeline sample 48000, ",
        "0 of them padded, placed by a round trip of 768 frames; it ended at the stop",
        "The recovery of the folder, before recording:",
        "  0 takes, 0 of them repaired; 0 files removed; 0 left untouched; 0 failed.",
        "recorder: 1 takes started, 1 finished; ",
        "0 write errors; 0 takes ended by a change of latency",
        "capture overruns: 0 frames; missed takes: 0",
        "periods: ",
        "gaps in the server's frame time: ",
    ] {
        assert!(
            stdout.contains(expected),
            "{expected:?} is missing from:\n{stdout}"
        );
    }

    // One finished take: the file made ahead of the next take is gone with the recorder.
    let takes = dir.path().join("takes");
    assert_eq!(wav_files(&takes), [MIC_TAKE]);
    let info = info_of(&takes.join(MIC_TAKE));
    assert_eq!(
        (info.sample_format, info.channels, info.sample_rate),
        (SampleFormat::Float32, 1, 48_000)
    );
    assert!(!info.length_repaired, "its sizes were written");
    assert_eq!(
        info.bext.unwrap().time_reference,
        48_000,
        "placed at --from"
    );
    // The span played: from --from to where the transport had read when the stop came, the two
    // seconds asked, the small session's pre-roll of 300 samples and a fade of 240, and the main
    // thread's lateness in asking the stop.
    let span = 2 * 48_000 + 300 + 240;
    assert!(
        (span..span + 24_000).contains(&info.frames),
        "{} frames for a span of {span}",
        info.frames
    );
    assert!(
        stdout.contains(&format!("{} frames, 0 of them padded", info.frames)),
        "{stdout}"
    );
}

/// Waits until the take `path` holds more than `bytes` bytes, as a running recording writes it;
/// fails when `child` ends first, or after 30 s.
fn wait_for_frames(path: &Path, bytes: u64, child: &mut std::process::Child) {
    let start = Instant::now();
    loop {
        if fs::metadata(path).is_ok_and(|metadata| metadata.len() > bytes) {
            return;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("the recording ended with {status} before its take held frames");
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the take held no frames after 30 s"
        );
        pause(Duration::from_millis(20));
    }
}

#[test]
fn a_held_folder_refuses_another_recorder_and_a_killed_take_is_recovered() {
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let dir = TempDir::new("record-held");
    let mut arguments = record_small(&dir, &server.name);
    arguments.extend(["--seconds", "30", "--format", "int24"].map(OsString::from));
    let mut first = Command::new(magnolia_binary())
        .args(&arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let takes = dir.path().join("takes");
    // Past its header, and the first frames the recorder drains.
    wait_for_frames(&takes.join(MIC_TAKE), 8_192, &mut first);

    let held = "another recorder or recovery is using the recording folder";
    let second = magnolia(&arguments);
    let recovered = magnolia([OsString::from("recover"), takes.clone().into()]);
    // Killed as a crash would end it, before it could finish its take.
    first.kill().unwrap();
    first.wait().unwrap();
    for ran in [&second, &recovered] {
        assert_eq!(ran.code, Some(1), "{}\n{}", ran.stdout, ran.stderr);
        assert!(ran.stderr.contains(held), "{}", ran.stderr);
        assert!(!ran.stderr.contains("Usage"), "a failure is not a misuse");
    }

    let ran = magnolia([OsString::from("recover"), takes.clone().into()]);
    assert_eq!(ran.code, Some(0), "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains("1 takes, 1 of them repaired"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains("9-mic-take1.wav: track 9, take 1, from timeline sample 0, "),
        "{}",
        ran.stdout
    );
    // The file made ahead of the next take, empty, is removed.
    assert_eq!(wav_files(&takes), [MIC_TAKE]);
    let info = info_of(&takes.join(MIC_TAKE));
    assert_eq!(info.sample_format, SampleFormat::Int24);
    assert!(!info.length_repaired, "its sizes are written");
    assert!(info.frames > 0);
    assert_eq!(info.bext.unwrap().time_reference, 0);
}

#[test]
fn a_server_that_stops_mid_take_still_leaves_the_take_ended_and_listed() {
    let _turn = one_at_a_time();
    let mut server = Server::start(256);
    let dir = TempDir::new("record-server-stops");
    let mut arguments = record_small(&dir, &server.name);
    arguments.extend(["--seconds", "30"].map(OsString::from));
    let mut recording = Command::new(magnolia_binary())
        .args(&arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let takes = dir.path().join("takes");
    wait_for_frames(&takes.join(MIC_TAKE), 8_192, &mut recording);
    server.stop();
    let output = recording.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}\n{stderr}");
    // The play failed, but the take it held ended at what it recorded, and is listed: the
    // processor, which JACK2 keeps when its server dies, never lets its tap go, so closing the
    // recorder ends the take.
    assert!(stdout.contains("Takes: 1"), "{stdout}");
    assert!(
        stdout.contains("it ended when the recorder closed")
            || stdout.contains("it ended when the engine let its track go"),
        "{stdout}"
    );
    let info = info_of(&takes.join(MIC_TAKE));
    assert!(!info.length_repaired, "its sizes are written");
    assert!(info.frames > 0);
}

/// The samples of the take `recover` is given in the test below: a ramp of distinct values.
fn ramp(frames: usize) -> Vec<f32> {
    (0..frames).map(|frame| frame as f32 / 4_096.0).collect()
}

/// Writes a WAV file of 32-bit floats as a writer that died leaves it, its sizes unknown, then
/// half a frame of a second channel's sample: a crash cut it inside a sample.
fn write_cut_by_hand(path: &Path, bext: Option<&Bext<'_>>, samples: &[f32]) {
    let file = File::create(path).unwrap();
    let mut writer = match bext {
        Some(bext) => WavWriter::with_bext(file, SampleFormat::Float32, 1, 48_000, bext).unwrap(),
        None => WavWriter::new(file, SampleFormat::Float32, 1, 48_000).unwrap(),
    };
    writer.write_frames(samples).unwrap();
    // Dropped without `finish`: its RIFF and data sizes stay 0xFFFFFFFF.
    drop(writer);
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&[0x12, 0x34]).unwrap();
}

#[test]
fn recover_repairs_a_take_cut_by_hand_and_leaves_a_third_party_wav_untouched() {
    let dir = TempDir::new("recover");
    let take = dir.path().join("1-mic-take1.wav");
    let samples = ramp(1_000);
    let bext = Bext {
        originator: "Magnolia",
        originator_reference: "a test's take",
        time_reference: 4_800,
        ..Bext::default()
    };
    write_cut_by_hand(&take, Some(&bext), &samples);
    let cut = fs::read(&take).unwrap();
    // Another program's file, named as a take and cut the same way, without Magnolia's mark.
    let other = dir.path().join("2-other-take1.wav");
    write_cut_by_hand(&other, None, &samples);
    let other_bytes = fs::read(&other).unwrap();

    let recover = [OsString::from("recover"), dir.path().into()];
    let ran = magnolia(&recover);
    assert_eq!(ran.code, Some(0), "{}\n{}", ran.stdout, ran.stderr);
    for expected in [
        "Recovered ",
        "  1 takes, 1 of them repaired; 0 files removed; 1 left untouched; 0 failed.",
        "1-mic-take1.wav: track 1, take 1, from timeline sample 4800, 1000 frames, repaired",
        "2-other-take1.wav: left untouched, as it is not Magnolia's take",
    ] {
        assert!(
            ran.stdout.contains(expected),
            "{expected:?} is missing from:\n{}",
            ran.stdout
        );
    }
    assert!(
        fs::read(&other).unwrap() == other_bytes,
        "the third-party file was touched"
    );

    // Cut to its last whole frame, its sizes written: every frame written, nothing more.
    let repaired = fs::read(&take).unwrap();
    assert_eq!(repaired.len(), cut.len() - 2);
    let info = read_info(&mut Cursor::new(&repaired)).unwrap();
    assert_eq!(info.frames, 1_000);
    assert!(!info.length_repaired, "its sizes are written");
    let mut read = vec![0.0; 1_000];
    decode(
        info.sample_format,
        1,
        &repaired[info.data_offset as usize..],
        &mut read,
    );
    assert_eq!(read, samples);

    // Recovered again, it is whole already.
    let ran = magnolia(&recover);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert!(
        ran.stdout.contains("1000 frames, whole already"),
        "{}",
        ran.stdout
    );
}

#[test]
fn record_refuses_a_session_without_an_armed_track_and_a_missing_folder() {
    // Both are refused before a client opens; were one to open, under PipeWire's libjack it would
    // reach the owner's live PipeWire, whatever the server's name.
    refuse_pipewire();
    let dir = TempDir::new("record-refused");
    generate(Kind::Small, &dir.path().join("session")).unwrap();
    let path = dir.path().join("session/session.json");
    let mut session = file::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    for track in &mut session.tracks {
        if track.armed.is_some() {
            track.armed = Some(false);
        }
    }
    let unarmed = dir.path().join("session/unarmed.json");
    fs::write(&unarmed, session.to_json().unwrap()).unwrap();
    let takes = dir.path().join("takes");
    fs::create_dir(&takes).unwrap();

    // Refused before it touches the folder, or any server.
    let ran = magnolia([
        OsString::from("record"),
        unarmed.into(),
        takes.clone().into(),
        "--server".into(),
        "magnolia-test-never-started".into(),
    ]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(
        ran.stderr
            .contains("has no armed input track: there is nothing to record"),
        "{}",
        ran.stderr
    );
    assert_eq!(
        fs::read_dir(&takes).unwrap().count(),
        0,
        "the folder is left as it was"
    );

    let missing = dir.path().join("missing");
    let ran = magnolia([
        OsString::from("record"),
        path.into(),
        missing.clone().into(),
        "--server".into(),
        "magnolia-test-never-started".into(),
    ]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(ran.stderr.contains("could not be used"), "{}", ran.stderr);
    assert!(!missing.exists(), "a missing folder is not made");
}
