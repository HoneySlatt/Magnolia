//! Recording (ADR-0030, ADR-0031, ADR-0032): a take holds the timeline samples of its play, placed
//! by the round trip, bit for bit, whatever the period lengths, a pre-roll, a swap or a play while
//! rolling; a ring that overflows leaves zeros in place of what it dropped and every later frame in
//! place; a change of latency, a disarm, a write error end a take; a graph's panic does not; a stop
//! before anything played makes no file; a take starts at the timeline's zero at the earliest; many
//! short takes keep their frames apart while the recorder drains; 24-bit takes; recovery, the
//! folder's lock and the takes' numbers; and a recording killed mid-take, recovered whole.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use magnolia_engine::engine::{Destination, Source};
use magnolia_engine::nodes::TestLatency;
use magnolia_engine::record::{
    RecordConfig, RecordError, Recorder, TakeEnd, TakeFormat, TakeName, TakeSummary, recover,
};
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{Bext, SampleFormat, WavWriter, read_info};

use crate::disk_support::TempDir;
use crate::record_support::{
    PRE_ROLL, RecordRig, TimelinePlayer, files_in, read_take, signal, take_path, timeline_value,
};
use crate::support::{Constant, FADE, Panicking, rate};
use crate::timeline_support::Random;

/// Where every play of the placement scenario starts on the timeline.
const FROM: i64 = 1_000;

/// The engine samples at which the placement scenario's commands go, at the first period that
/// starts there or after: a play, a swap during it, a play while rolling from the same sample, a
/// stop; and its end, past every take's tail.
const PLAY_AT: i64 = 2_000;
const SWAP_AT: i64 = 12_000;
const REPLAY_AT: i64 = 20_000;
const STOP_AT: i64 = 32_000;
const END_AT: i64 = 52_000;

/// How the placement scenario's round trip is given: as the device's latency, or as the
/// recorder's extra latency.
#[derive(Clone, Copy, Debug)]
enum RoundTrip {
    Device,
    Extra,
}

/// Periods of 1 to 4,096 frames, drawn from `seed`, up to [`END_AT`].
fn random_periods(seed: u64) -> Vec<usize> {
    let mut random = Random(seed);
    let mut periods = Vec::new();
    let mut total = 0;
    while total < END_AT as usize {
        let frames = random.up_to(4_096);
        periods.push(frames);
        total += frames;
    }
    periods
}

/// What a pass of the placement scenario gives: the rig, and each play's number and engine sample
/// at which the output played its first timeline sample.
struct Pass {
    rig: RecordRig,
    plays: Vec<(u64, i64)>,
}

/// Plays the placement scenario over `periods`, capture channel 0 holding `capture(engine sample)`;
/// with `record`, track 1 armed on it, placed by a round trip of `delay` given as `record` says.
fn placement_pass(
    periods: &[usize],
    capture: &dyn Fn(i64) -> f32,
    record: Option<(RoundTrip, u32)>,
) -> Pass {
    let extra = match record {
        Some((RoundTrip::Extra, delay)) => delay,
        _ => 0,
    };
    let mut rig = RecordRig::new("placement", 1, |config| config.extra_latency = extra);
    rig.add_timeline_and_pre_roll();
    if let Some((round_trip, delay)) = record {
        rig.arm(1, "voice", &[0]);
        if let RoundTrip::Device = round_trip {
            rig.publish_latency(delay / 3, delay - delay / 3);
        }
    }
    rig.rig.controller.commit().unwrap();
    let mut plays = Vec::new();
    let mut sent = [false; 4];
    for &frames in periods {
        let at = rig.position;
        let commands = [PLAY_AT, SWAP_AT, REPLAY_AT, STOP_AT];
        for (index, &command_at) in commands.iter().enumerate() {
            if at >= command_at && !sent[index] {
                sent[index] = true;
                match index {
                    0 | 2 => {
                        rig.rig.controller.play(SamplePosition(FROM)).unwrap();
                    }
                    1 => {
                        let silent = rig.rig.add(Constant::new(0.0));
                        rig.rig
                            .controller
                            .connect(Source::Node(silent, 0), Destination::Playback(0));
                        rig.rig.controller.commit().unwrap();
                    }
                    _ => rig.rig.controller.stop().unwrap(),
                }
            }
        }
        rig.period(frames, &|_, sample| capture(sample));
        if let Some(start) = rig.rig.controller.stats().play_start()
            && plays
                .last()
                .is_none_or(|&(sequence, _)| sequence != start.sequence)
        {
            plays.push((start.sequence, start.audible_at.0));
        }
    }
    Pass { rig, plays }
}

/// The placement scenario with a loop device that feeds the capture with the playback `delay`
/// frames later, and the round trip given as `delay`: each take, read back, holds the timeline's
/// values at its timeline samples, bit for bit, and exactly the samples its play made heard.
///
/// A device whose delay is shorter than a period cannot feed back the period it plays; the loop is
/// made of two passes, as the engine and its periods are deterministic: the first plays without
/// recording, the second records a capture that is the first's playback, `delay` frames later.
fn placement(round_trip: RoundTrip, delay: u32, seed: u64) {
    let periods = random_periods(seed);
    let silence = |_| 0.0;
    let first = placement_pass(&periods, &silence, None);
    let heard = first.rig.played.clone();
    let looped = |sample: i64| {
        usize::try_from(sample - i64::from(delay))
            .ok()
            .and_then(|index| heard.get(index).copied())
            .unwrap_or(0.0)
    };
    let mut second = placement_pass(&periods, &looped, Some((round_trip, delay)));
    let what = format!("round trip {delay} as {round_trip:?}");
    assert_eq!(
        second.rig.played, heard,
        "{what}: the playback is deterministic"
    );
    assert_eq!(second.plays, first.plays, "{what}: the plays start alike");
    assert_eq!(
        second.plays.len(),
        2,
        "{what}: a play, then a play while rolling"
    );
    let stats = second.rig.rig.controller.stats().clone();
    assert_eq!(
        stats.last_swap_applied(),
        2,
        "{what}: a swap came during the take"
    );
    let mut takes = second.rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(stats.capture_overruns(), 0, "{what}");
    assert_eq!(stats.missed_takes(), 0, "{what}");
    assert_eq!(takes.len(), 2, "{what}: both plays made a take: {takes:?}");
    let boundaries: BTreeSet<i64> = periods
        .iter()
        .scan(0_i64, |at, &frames| {
            *at += frames as i64;
            Some(*at)
        })
        .collect();
    for (take, &(play, audible_at)) in takes.iter().zip(&second.plays) {
        let what = format!("{what}, take of play {play}");
        assert_eq!(take.play, play, "{what}");
        assert_eq!(take.ended, TakeEnd::Stopped, "{what}");
        assert_eq!(take.first_sample, SamplePosition(FROM), "{what}");
        assert_eq!(take.round_trip, u64::from(delay), "{what}");
        assert_eq!(take.padded, 0, "{what}");
        assert!(take.error.is_none(), "{what}: {:?}", take.error);
        let (info, samples) = read_take(&take_path(take));
        assert!(!info.length_repaired, "{what}: the take was finished");
        assert_eq!(info.bext.unwrap().time_reference, FROM as u64, "{what}");
        assert_eq!(info.frames, take.frames, "{what}");
        for (frame, sample) in samples.iter().enumerate() {
            let expected = timeline_value(FROM + frame as i64);
            assert_eq!(
                sample.to_bits(),
                expected.to_bits(),
                "{what}: frame {frame} is {sample}, should be {expected}"
            );
        }
        // The take holds exactly what the play made heard: from its first sample, up to its stop.
        let length = take.frames as i64;
        let heard_at = |sample: i64| heard[sample as usize];
        assert_eq!(heard_at(audible_at), timeline_value(FROM), "{what}");
        assert_eq!(
            heard_at(audible_at + length - 1),
            timeline_value(FROM + length - 1),
            "{what}"
        );
        assert_eq!(
            heard_at(audible_at + length),
            0.0,
            "{what}: the play ended there"
        );
        assert!(
            length > i64::from(PRE_ROLL) + FADE as i64,
            "{what}: {length}"
        );
    }
    // The stop that ends the second take lands inside a period, cut there by the processor.
    let (_, audible_at) = second.plays[1];
    let end = audible_at + takes[1].frames as i64;
    assert!(
        !boundaries.contains(&end),
        "{what}: the stop at {end} lies on a period's edge"
    );
    assert_eq!(
        files_in(second.rig.dir.path()),
        ["1-voice-take1.wav", "1-voice-take2.wav"]
    );
}

#[test]
fn a_take_holds_its_plays_timeline_samples_placed_by_the_devices_round_trip() {
    for (index, delay) in [0, 1, 63, 64, 65, 1_000, 5_000].into_iter().enumerate() {
        placement(RoundTrip::Device, delay, 0x7A4E_0001 + index as u64);
    }
}

#[test]
fn a_take_is_placed_by_the_recorders_extra_latency_alike() {
    placement(RoundTrip::Extra, 1_000, 0x7A4E_0100);
    placement(RoundTrip::Extra, 63, 0x7A4E_0101);
}

/// The period of the scenarios below, and their capture: [`signal`] on every channel.
const PERIOD: usize = 256;

fn capture(channel: usize, sample: i64) -> f32 {
    signal(channel, sample)
}

/// The engine sample at which the output played the last play's first timeline sample.
fn audible_at(rig: &RecordRig) -> i64 {
    rig.rig
        .controller
        .stats()
        .play_start()
        .unwrap()
        .audible_at
        .0
}

/// Checks that a take's samples, of `channels` capture channels, are the capture from engine sample
/// `first` on, except the frames listed in `zeros`, which are zeros.
fn assert_capture(samples: &[f32], channels: &[usize], first: i64, zeros: &[std::ops::Range<u64>]) {
    for (frame, values) in samples.chunks(channels.len()).enumerate() {
        let frame = frame as u64;
        let dropped = zeros.iter().any(|range| range.contains(&frame));
        for (&channel, &value) in channels.iter().zip(values) {
            let expected = if dropped {
                0.0
            } else {
                signal(channel, first + frame as i64)
            };
            assert_eq!(
                value.to_bits(),
                expected.to_bits(),
                "frame {frame}, channel {channel}: {value}, should be {expected}"
            );
        }
    }
}

#[test]
fn a_ring_that_overflows_leaves_zeros_where_it_dropped_and_every_later_frame_in_place() {
    let mut rig = RecordRig::new("overrun", 2, |config| config.ring_frames = 4_096);
    rig.arm(3, "keys", &[1, 0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    // Paused once idle, the recorder drains nothing: 16 periods fill the ring, the next 24 are
    // dropped.
    rig.wait_idle();
    rig.recorder().pause(true);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..40 {
        rig.period(PERIOD, &capture);
    }
    let stats = rig.rig.controller.stats().clone();
    assert_eq!(stats.capture_overruns(), 24 * PERIOD as u64);
    rig.recorder().pause(false);
    rig.wait_idle();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
        rig.wait_idle();
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    rig.wait_idle();
    let fill = rig.recorder().fullest_fill(3).unwrap();
    let recorder_stats = (
        rig.recorder().stats().frames_padded(),
        rig.recorder().stats().gaps(),
    );
    let takes = rig.finish();
    assert_eq!(fill.frames, 4_096, "the ring filled up");
    assert_eq!(takes.len(), 1);
    let take = &takes[0];
    assert_eq!(take.ended, TakeEnd::Stopped);
    assert_eq!(
        recorder_stats,
        (24 * PERIOD as u64, 1),
        "padded frames, gaps"
    );
    assert_eq!(take.padded, 24 * PERIOD as u64);
    assert_eq!(
        stats.capture_overruns(),
        take.padded,
        "nothing dropped once drained"
    );
    let (info, samples) = read_take(&take_path(take));
    assert_eq!(info.channels, 2);
    assert_eq!(info.frames, take.frames);
    let dropped = 16 * PERIOD as u64..40 * PERIOD as u64;
    assert!(take.frames > dropped.end);
    assert_capture(&samples, &[1, 0], first, &[dropped]);
}

#[test]
fn a_change_of_the_devices_latency_ends_the_take_and_is_counted() {
    let mut rig = RecordRig::new("latency", 1, |_| {});
    rig.publish_latency(32, 64);
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig) + 96;
    let changed_at = rig.position;
    rig.publish_latency(32, 128);
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    // A play from now is placed by the new latency.
    rig.rig.controller.stop().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(48_000)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let second_first = audible_at(&rig) + 160;
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    rig.wait_idle();
    let latency_ends = rig.recorder().stats().latency_ends();
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(latency_ends, 1);
    assert_eq!(takes.len(), 2, "{takes:?}");
    let take = &takes[0];
    assert_eq!(take.ended, TakeEnd::LatencyChanged);
    assert_eq!(take.round_trip, 96);
    assert_eq!(
        take.frames,
        (changed_at - first) as u64,
        "it ends at the last frame copied"
    );
    let (_, samples) = read_take(&take_path(take));
    assert_capture(&samples, &[0], first, &[]);
    let take = &takes[1];
    assert_eq!(take.ended, TakeEnd::Stopped);
    assert_eq!(take.round_trip, 160);
    assert_eq!(take.first_sample, SamplePosition(48_000));
    let (info, samples) = read_take(&take_path(take));
    assert_eq!(info.bext.unwrap().time_reference, 48_000);
    assert_capture(&samples, &[0], second_first, &[]);
}

#[test]
fn a_graph_that_panics_mid_take_leaves_the_take_whole() {
    let mut rig = RecordRig::new("panic", 1, |_| {});
    // A round trip of 2,000 frames: the take records that long past the transport's stop.
    rig.publish_latency(1_000, 1_000);
    let armed = Arc::new(AtomicBool::new(false));
    let panicking = rig.rig.add(Panicking {
        armed: Arc::clone(&armed),
    });
    rig.rig
        .controller
        .connect(Source::Node(panicking, 0), Destination::Playback(0));
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.stop().unwrap();
    rig.period(PERIOD, &capture);
    armed.store(true, Ordering::Relaxed);
    rig.period(PERIOD, &capture);
    let stats = rig.rig.controller.stats().clone();
    assert!(stats.poisoned(), "the graph's panic poisoned the processor");
    assert!(!stats.tap_stopped(), "but not the tap");
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig) + 2_000;
    let takes = rig.finish();
    assert_eq!(takes.len(), 1);
    let take = &takes[0];
    assert_eq!(take.ended, TakeEnd::Stopped, "the take reached its length");
    // The stop came 20 periods into the play, plus the fade.
    assert_eq!(take.frames, 20 * PERIOD as u64 + FADE as u64);
    let (info, samples) = read_take(&take_path(take));
    assert!(!info.length_repaired);
    assert_capture(&samples, &[0], first, &[]);
}

#[test]
fn a_stop_before_anything_played_makes_no_take_and_leaves_no_file() {
    let mut rig = RecordRig::new("pre-roll", 1, |_| {});
    rig.add_timeline_and_pre_roll();
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    // A play and its stop in one period: nothing audible was read.
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    rig.rig.controller.stop().unwrap();
    for _ in 0..8 {
        rig.period(PERIOD, &capture);
    }
    assert_eq!(rig.rig.controller.stats().play_start().unwrap().sequence, 1);
    assert_eq!(
        files_in(rig.dir.path()),
        ["1-voice-take1.wav"],
        "the next take's file, made ahead"
    );
    rig.wait_idle();
    let started = rig.recorder().stats().takes_started();
    let takes = rig.finish();
    assert_eq!(started, 0);
    assert!(takes.is_empty(), "{takes:?}");
    assert!(
        files_in(rig.dir.path()).is_empty(),
        "the file made ahead was removed"
    );
}

#[test]
fn disarming_mid_take_ends_it_and_arming_mid_play_records_from_the_next_play() {
    let mut rig = RecordRig::new("disarm", 2, |_| {});
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    // Track 2 armed during the play records nothing of it.
    rig.arm(2, "guitar", &[1]);
    rig.rig.controller.disarm(1).unwrap();
    rig.rig.controller.commit().unwrap();
    let disarmed_at = rig.position;
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let second_first = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(takes.len(), 2, "{takes:?}");
    let take = &takes[0];
    assert_eq!(
        (take.track, take.play, take.ended),
        (1, 1, TakeEnd::Disarmed)
    );
    assert_eq!(
        take.frames,
        (disarmed_at - first) as u64,
        "it ends where the swap applied"
    );
    let (_, samples) = read_take(&take_path(take));
    assert_capture(&samples, &[0], first, &[]);
    let take = &takes[1];
    assert_eq!(
        (take.track, take.play, take.ended),
        (2, 2, TakeEnd::Stopped)
    );
    let (_, samples) = read_take(&take_path(take));
    assert_capture(&samples, &[1], second_first, &[]);
    assert_eq!(
        files_in(rig.dir.path()),
        ["1-voice-take1.wav", "2-guitar-take1.wav"]
    );
}

#[test]
fn a_second_recorder_or_a_recovery_is_refused_while_a_recorder_holds_the_folder() {
    let rig = RecordRig::new("lock", 1, |_| {});
    let config = RecordConfig::new(rig.dir.path(), rate());
    let second = Recorder::start(config.clone(), rig.rig.controller.stats());
    assert!(
        matches!(second, Err(RecordError::FolderLocked(_))),
        "{:?}",
        second.err()
    );
    assert!(matches!(
        recover(rig.dir.path()),
        Err(RecordError::FolderLocked(_))
    ));
    let dir = rig.dir.path().to_path_buf();
    let RecordRig {
        recorder,
        rig: engine,
        dir: _kept,
        ..
    } = rig;
    drop(recorder);
    // Released with the recorder.
    let third = Recorder::start(config, engine.controller.stats()).unwrap();
    drop(third);
    recover(&dir).unwrap();
}

/// Writes an unfinished WAV file of three mono frames, its sizes unknown, with a `bext` chunk of
/// `originator` if given.
fn unfinished_wav(path: &Path, originator: Option<&str>) -> Vec<u8> {
    let file = fs::File::create(path).unwrap();
    let mut writer = match originator {
        Some(originator) => {
            let bext = Bext {
                originator,
                time_reference: 96_000,
                ..Bext::default()
            };
            WavWriter::with_bext(file, SampleFormat::Float32, 1, 48_000, &bext).unwrap()
        }
        None => WavWriter::new(file, SampleFormat::Float32, 1, 48_000).unwrap(),
    };
    writer.write_frames(&[0.25, 0.5, 0.75]).unwrap();
    drop(writer);
    fs::read(path).unwrap()
}

#[test]
fn recovery_repairs_magnolias_takes_and_leaves_every_other_file_untouched() {
    let dir = TempDir::new("recovery");
    let path = |name: &str| dir.path().join(name);
    let others = [
        ("other.wav", unfinished_wav(&path("other.wav"), None)),
        (
            "4-bass-take2.wav",
            unfinished_wav(&path("4-bass-take2.wav"), None),
        ),
        (
            "5-pad-take1.wav",
            unfinished_wav(&path("5-pad-take1.wav"), Some("Pro Tools")),
        ),
    ];
    let not_wav = path("notes.txt");
    fs::write(&not_wav, b"").unwrap();
    let empty_other = path("empty.wav");
    fs::write(&empty_other, b"").unwrap();
    unfinished_wav(&path("1-voice-take3.wav"), Some("Magnolia"));
    unfinished_wav(&path("renamed.wav"), Some("Magnolia"));
    fs::write(path("1-voice-take4.wav"), b"").unwrap();

    let recovery = recover(dir.path()).unwrap();
    for (name, bytes) in &others {
        assert_eq!(&fs::read(path(name)).unwrap(), bytes, "{name} untouched");
    }
    assert!(not_wav.exists() && empty_other.exists());
    let mut skipped = recovery.skipped.clone();
    skipped.sort();
    assert_eq!(skipped, [path("4-bass-take2.wav"), path("5-pad-take1.wav")]);
    assert_eq!(recovery.removed, [path("1-voice-take4.wav")]);
    assert!(recovery.failed.is_empty(), "{:?}", recovery.failed);
    let mut takes = recovery.takes.clone();
    takes.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(takes.len(), 2);
    assert_eq!(takes[0].path, path("1-voice-take3.wav"));
    assert_eq!(
        takes[0].name,
        Some(TakeName {
            track: 1,
            name: "voice".into(),
            take: 3
        })
    );
    assert_eq!(takes[1].name, None);
    for take in &takes {
        assert_eq!(
            (take.first_sample, take.frames, take.repaired),
            (96_000, 3, true)
        );
        let info = read_info(&mut fs::File::open(&take.path).unwrap()).unwrap();
        assert!(!info.length_repaired, "it reads without repair now");
    }
    // A second recovery finds them finished.
    let again = recover(dir.path()).unwrap();
    assert!(again.takes.iter().all(|take| !take.repaired));
}

#[test]
fn take_numbers_continue_past_the_files_already_there() {
    let dir = TempDir::new("numbers");
    for name in [
        "1-voice-take1.wav",
        "1-voice-take7.wav",
        "1-other-take9.wav",
        "2-voice-take12.wav",
    ] {
        WavWriter::new(
            fs::File::create(dir.path().join(name)).unwrap(),
            SampleFormat::Int16,
            1,
            48_000,
        )
        .unwrap()
        .finish()
        .unwrap();
    }
    let mut rig = RecordRig::new("numbers", 1, |config| {
        config.folder = dir.path().to_path_buf()
    });
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    for _ in 0..2 {
        rig.rig.controller.play(SamplePosition(0)).unwrap();
        for _ in 0..4 {
            rig.period(PERIOD, &capture);
        }
        rig.rig.controller.stop().unwrap();
        for _ in 0..4 {
            rig.period(PERIOD, &capture);
        }
    }
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    let numbers: Vec<Option<u32>> = takes.iter().map(|take| take.take).collect();
    assert_eq!(numbers, [Some(8), Some(9)]);
    assert_eq!(
        files_in(dir.path()),
        [
            "1-other-take9.wav",
            "1-voice-take1.wav",
            "1-voice-take7.wav",
            "1-voice-take8.wav",
            "1-voice-take9.wav",
            "2-voice-take12.wav"
        ]
    );
}

#[test]
fn a_write_error_ends_the_take_reported_its_file_left_as_a_crash_leaves_it() {
    let mut rig = RecordRig::new("write-error", 1, |_| {});
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
        rig.wait_idle();
    }
    let written = rig.recorder().stats().frames_written();
    assert!(written > 0);
    rig.recorder().fail_writes(true);
    for _ in 0..40 {
        rig.period(PERIOD, &capture);
        rig.wait_idle();
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    rig.wait_idle();
    let first = audible_at(&rig);
    let stats = rig.recorder().stats();
    let (errors, written_after) = (stats.write_errors(), stats.frames_written());
    rig.recorder().fail_writes(false);
    let takes = rig.finish();
    assert!(errors >= 1);
    assert!(
        written_after < written + 4_096,
        "the take's later frames were dropped"
    );
    assert_eq!(
        rig.rig.controller.stats().capture_overruns(),
        0,
        "but drained"
    );
    assert_eq!(takes.len(), 1);
    let take = &takes[0];
    assert!(take.error.is_some());
    assert_eq!(take.ended, TakeEnd::Stopped);
    let (info, samples) = read_take(&take_path(take));
    assert!(
        info.length_repaired,
        "its sizes say unknown, as a crash leaves them"
    );
    assert!(
        info.frames >= written && info.frames <= written_after,
        "{}",
        info.frames
    );
    assert_capture(&samples, &[0], first, &[]);
}

/// Names the folder a child process records into; the parent's own run leaves it unset.
const CHILD_FOLDER: &str = "MAGNOLIA_RECORD_CHILD_FOLDER";

/// The capture channels the killed recording records.
const CHILD_CHANNELS: [usize; 3] = [0, 1, 2];

/// Where the killed recording's take starts on the timeline.
const CHILD_FROM: i64 = 48_000;

/// The child of [`a_recording_killed_mid_take_is_recovered_whole`]: records three channels of
/// [`signal`] as fast as it can, never paced in real time, printing `audible_at` once and then the
/// frames its recorder has written, until it is killed. Without its variable, as in the tests' own
/// run, it does nothing.
#[test]
fn recording_child() {
    let Some(folder) = std::env::var_os(CHILD_FOLDER) else {
        return;
    };
    let mut rig = RecordRig::new("child-unused", 3, |config| {
        config.folder = folder.into();
    });
    rig.arm(1, "voice", &CHILD_CHANNELS);
    rig.rig.controller.commit().unwrap();
    rig.period(1_024, &capture);
    rig.rig.controller.play(SamplePosition(CHILD_FROM)).unwrap();
    rig.period(1_024, &capture);
    println!("audible_at {}", audible_at(&rig));
    // Far more than the parent waits for: the child is killed long before it ends.
    for period in 0..100_000 {
        rig.period(1_024, &capture);
        if period % 4 == 3 {
            // Never ahead of the recorder: a frame dropped would be a zero, not the signal.
            rig.wait_idle();
            println!("written {}", rig.recorder().stats().frames_written());
        }
    }
}

/// A child process records through the device-less backend, as fast as it can; the parent kills it
/// (SIGKILL) once it has written 200,000 frames, then recovers the folder: the take keeps whole
/// frames only, at least those written before the kill, each the signal captured, its time
/// reference, and reads without repair afterwards.
#[test]
fn a_recording_killed_mid_take_is_recovered_whole() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let dir = TempDir::new("killed");
    let errors = dir.path().join("child-errors.txt");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "record::recording_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_FOLDER, dir.path())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(&errors).unwrap())
        .spawn()
        .unwrap();
    let mut first = None;
    let mut written = 0_u64;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        // The test harness writes the test's name before its first line, on the same line.
        if let Some((_, at)) = line.split_once("audible_at ") {
            first = Some(at.parse::<i64>().unwrap());
        }
        if let Some(frames) = line.strip_prefix("written ") {
            written = frames.parse().unwrap();
            if written >= 200_000 {
                break;
            }
        }
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(
        written >= 200_000,
        "the child wrote {written} frames, then {status}: {}",
        fs::read_to_string(&errors).unwrap_or_default()
    );
    assert!(!status.success(), "it was killed");
    let first = first.expect("the child printed where its take starts");

    let recovery = recover(dir.path()).unwrap();
    assert!(recovery.failed.is_empty(), "{:?}", recovery.failed);
    assert_eq!(
        recovery.removed,
        [dir.path().join("1-voice-take2.wav")],
        "made ahead"
    );
    assert_eq!(recovery.takes.len(), 1);
    let take = &recovery.takes[0];
    assert_eq!(take.path, dir.path().join("1-voice-take1.wav"));
    assert!(take.repaired, "the writer died before writing its sizes");
    assert!(
        take.frames >= written,
        "{} frames kept, {written} written",
        take.frames
    );
    assert_eq!(take.first_sample, CHILD_FROM as u64);
    let (info, samples) = read_take(&take.path);
    assert!(!info.length_repaired, "it reads without repair now");
    assert_eq!(info.frames, take.frames);
    assert_eq!(info.bext.unwrap().time_reference, CHILD_FROM as u64);
    let length = fs::metadata(&take.path).unwrap().len();
    let data_end = info.data_offset + info.frames * 12;
    assert!(
        length == data_end || length == data_end + (data_end & 1),
        "whole frames only"
    );
    assert_capture(&samples, &CHILD_CHANNELS, first, &[]);
}

/// Fifty short takes, each a play while rolling over the last, with a round trip longer than a
/// period so that each take's tail is still captured while the next play starts: the recorder,
/// draining on its own thread while the tap pushes, never lets a take's frames into another.
#[test]
fn many_short_takes_drained_while_the_tap_pushes_keep_their_frames_apart() {
    let mut rig = RecordRig::new("many-takes", 2, |_| {});
    rig.publish_latency(150, 450);
    rig.arm(1, "voice", &[0, 1]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    let mut random = Random(0x5AFE_7A4E);
    let mut plays = Vec::new();
    for play in 0..50_i64 {
        rig.rig
            .controller
            .play(SamplePosition(play * 10_000))
            .unwrap();
        for _ in 0..random.up_to(6) {
            rig.period(random.up_to(700), &capture);
            if let Some(start) = rig.rig.controller.stats().play_start()
                && plays
                    .last()
                    .is_none_or(|&(sequence, _)| sequence != start.sequence)
            {
                plays.push((start.sequence, start.audible_at.0));
            }
        }
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..8 {
        rig.period(PERIOD, &capture);
        if let Some(start) = rig.rig.controller.stats().play_start()
            && plays
                .last()
                .is_none_or(|&(sequence, _)| sequence != start.sequence)
        {
            plays.push((start.sequence, start.audible_at.0));
        }
    }
    let overruns = rig.rig.controller.stats().capture_overruns();
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(overruns, 0);
    assert_eq!(
        takes.len(),
        plays.len(),
        "every play that started made a take"
    );
    assert!(plays.len() >= 30, "most plays started: {}", plays.len());
    for (take, &(play, audible_at)) in takes.iter().zip(&plays) {
        assert_eq!(take.play, play);
        assert_eq!(take.ended, TakeEnd::Stopped, "play {play}");
        assert_eq!(
            take.first_sample,
            SamplePosition((play as i64 - 1) * 10_000)
        );
        let (info, samples) = read_take(&take_path(take));
        assert_eq!(info.frames, take.frames, "play {play}");
        assert_capture(&samples, &[0, 1], audible_at + 600, &[]);
    }
}

#[test]
fn a_take_in_24_bit_integers_holds_the_capture_rounded_to_24_bits() {
    let mut rig = RecordRig::new("int24", 1, |config| config.format = TakeFormat::Int24);
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..8 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..2 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    let takes = rig.finish();
    let (info, samples) = read_take(&take_path(&takes[0]));
    assert_eq!(info.sample_format, SampleFormat::Int24);
    assert_eq!(info.frames, takes[0].frames);
    for (frame, &sample) in samples.iter().enumerate() {
        let captured = signal(0, first + frame as i64);
        let expected = (captured * 8_388_608.0).round() / 8_388_608.0;
        assert_eq!(sample, expected, "frame {frame}");
    }
}

/// A play from before the timeline's zero records from zero: the take's time reference is 0, its
/// earlier frames dropped. A play whose audible region lies wholly before zero makes no take.
#[test]
fn a_take_starts_at_the_timelines_zero_at_the_earliest() {
    let mut rig = RecordRig::new("before-zero", 1, |_| {});
    rig.publish_latency(100, 100);
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    // Wholly before zero: stopped at -10,000 + 3 periods + a fade at most.
    rig.rig.controller.play(SamplePosition(-10_000)).unwrap();
    for _ in 0..3 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..12 {
        rig.period(PERIOD, &capture);
    }
    // Across zero.
    rig.rig.controller.play(SamplePosition(-1_000)).unwrap();
    for _ in 0..12 {
        rig.period(PERIOD, &capture);
    }
    let audible = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    let takes = rig.finish();
    assert_eq!(takes.len(), 1, "{takes:?}");
    let take = &takes[0];
    assert_eq!((take.play, take.first_sample), (2, SamplePosition(0)));
    // The stop came 12 periods into the play, plus the fade: the region's samples from zero.
    assert_eq!(take.frames, 12 * PERIOD as u64 + FADE as u64 - 1_000);
    let (info, samples) = read_take(&take_path(take));
    assert_eq!(info.bext.unwrap().time_reference, 0);
    // Timeline sample 0 played at `audible + 1,000`, captured a round trip later.
    assert_capture(&samples, &[0], audible + 1_000 + 200, &[]);
}

/// Checks that a take of `play` ended at its stop and holds the capture of channel 0 from engine
/// sample `first` on.
fn assert_take_of(take: &TakeSummary, play: u64, first: i64) {
    assert_eq!(
        (take.play, take.ended),
        (play, TakeEnd::Stopped),
        "{take:?}"
    );
    assert!(take.error.is_none() && take.frames > 0, "{take:?}");
    let (info, samples) = read_take(&take_path(take));
    assert_eq!(info.frames, take.frames);
    assert_capture(&samples, &[0], first, &[]);
}

/// A play during a long pre-roll, then a swap that removes the latent path, then a play while
/// rolling: the transport stops the first play at once, nothing audible read under the new
/// pre-roll, and starts the second in the same period. The tap never sees the first play's stop;
/// it drops the first play's window, which reached no frame, as the second opens. Each take is its
/// own play's, whole and exact, a third play's too; no take of the first play.
#[test]
fn a_play_stopped_at_once_by_a_play_while_rolling_leaves_no_window_open() {
    let mut rig = RecordRig::new("stopped-at-once", 1, |_| {});
    let player = rig.rig.add(TimelinePlayer);
    rig.rig
        .controller
        .connect(Source::Node(player, 0), Destination::Playback(0));
    let silent = rig.rig.add(Constant::new(0.0));
    let latent = rig.rig.add(TestLatency::new(1, 4_000));
    rig.rig
        .controller
        .connect(Source::Node(silent, 0), Destination::Node(latent, 0));
    rig.rig
        .controller
        .connect(Source::Node(latent, 0), Destination::Playback(0));
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    rig.period(PERIOD, &capture);
    rig.period(PERIOD, &capture);
    rig.rig.controller.remove_node(latent).unwrap();
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..40 {
        rig.period(PERIOD, &capture);
    }
    let second = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    for _ in 0..40 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let third = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let overruns = rig.rig.controller.stats().capture_overruns();
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(overruns, 0);
    assert_eq!(takes.len(), 2, "{takes:#?}");
    assert_take_of(&takes[0], 2, second);
    assert_take_of(&takes[1], 3, third);
}

/// The same with a period of no frame between a play and a play while rolling: the first play is
/// stopped at once and the second starts on the same engine sample; one take, the second play's,
/// then a third play's, each whole.
#[test]
fn a_period_of_no_frame_between_two_plays_leaves_no_window_open() {
    let mut rig = RecordRig::new("zero-period", 1, |_| {});
    rig.add_timeline_and_pre_roll();
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    rig.period(0, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let second = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let third = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(takes.len(), 2, "{takes:#?}");
    assert_take_of(&takes[0], 2, second);
    assert_take_of(&takes[1], 3, third);
}

/// A graph that panics before the stop: the transport still takes the stop, and the take ends at
/// its window's end, whole, as if the graph had played on.
#[test]
fn a_stop_after_the_graphs_panic_still_ends_the_take_whole() {
    let mut rig = RecordRig::new("panic-then-stop", 1, |_| {});
    rig.publish_latency(1_000, 1_000);
    let armed = Arc::new(AtomicBool::new(false));
    let panicking = rig.rig.add(Panicking {
        armed: Arc::clone(&armed),
    });
    rig.rig
        .controller
        .connect(Source::Node(panicking, 0), Destination::Playback(0));
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    // The panic falls in the middle of a period: the clock moves over the rest of it all the same.
    armed.store(true, Ordering::Relaxed);
    rig.period(300, &capture);
    assert!(rig.rig.controller.stats().poisoned());
    for _ in 0..5 {
        rig.period(PERIOD, &capture);
    }
    rig.rig.controller.stop().unwrap();
    let stop_at = rig.position;
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let takes = rig.finish();
    assert_eq!(takes.len(), 1, "{takes:?}");
    let take = &takes[0];
    assert_eq!(take.ended, TakeEnd::Stopped);
    // The stop's timeline sample: the clock at the stop's period, plus the fade; no pre-roll.
    assert_eq!(take.frames, (stop_at - first) as u64 + FADE as u64);
    let (info, samples) = read_take(&take_path(take));
    assert!(!info.length_repaired);
    assert_capture(&samples, &[0], first + 2_000, &[]);
}

/// After the graph's panic, a play is refused and counted, and makes no take (ADR-0034): played
/// while rolling, it still ends the play under way, as a stop does, whose take ends whole.
#[test]
fn a_play_after_the_graphs_panic_is_refused_and_ends_the_play_under_way() {
    let mut rig = RecordRig::new("panic-then-play", 1, |_| {});
    let armed = Arc::new(AtomicBool::new(false));
    let panicking = rig.rig.add(Panicking {
        armed: Arc::clone(&armed),
    });
    rig.rig
        .controller
        .connect(Source::Node(panicking, 0), Destination::Playback(0));
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    armed.store(true, Ordering::Relaxed);
    rig.period(PERIOD, &capture);
    assert!(rig.rig.controller.stats().poisoned());

    // A play while rolling: the play under way stops there, and no new one starts.
    rig.rig.controller.play(SamplePosition(48_000)).unwrap();
    let stop_at = rig.position;
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    // A play while stopped: refused too.
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..20 {
        rig.period(PERIOD, &capture);
    }
    let stats = std::sync::Arc::clone(rig.rig.controller.stats());
    assert_eq!(stats.plays_refused(), 2);
    assert_eq!(
        stats.play_start().unwrap().sequence,
        1,
        "no play started after the panic"
    );
    let takes = rig.finish();
    assert_eq!(takes.len(), 1, "{takes:?}");
    let take = &takes[0];
    assert_eq!(take.ended, TakeEnd::Stopped);
    assert_eq!(take.frames, (stop_at - first) as u64 + FADE as u64);
    let (info, samples) = read_take(&take_path(take));
    assert!(!info.length_repaired);
    assert_capture(&samples, &[0], first, &[]);
}

/// A graph that panics, then the processor dropped, as ADR-0022's rebuild replaces it: its taps go
/// with it, and the take under way ends at what it holds.
#[test]
fn a_processor_dropped_after_the_graphs_panic_ends_the_take() {
    let mut rig = RecordRig::new("panic-then-drop", 1, |_| {});
    let armed = Arc::new(AtomicBool::new(false));
    let panicking = rig.rig.add(Panicking {
        armed: Arc::clone(&armed),
    });
    rig.rig
        .controller
        .connect(Source::Node(panicking, 0), Destination::Playback(0));
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    for _ in 0..10 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    armed.store(true, Ordering::Relaxed);
    for _ in 0..5 {
        rig.period(PERIOD, &capture);
    }
    let end = rig.position;
    let RecordRig {
        rig: engine,
        recorder,
        dir: _dir,
        ..
    } = rig;
    let recorder = recorder.unwrap();
    drop(engine.backend);
    assert!(recorder.wait_idle(std::time::Duration::from_secs(20)));
    let takes = recorder.takes();
    drop(recorder);
    assert_eq!(takes.len(), 1, "{takes:?}");
    let take = &takes[0];
    assert_eq!(take.ended, TakeEnd::TapDropped);
    assert_eq!(
        take.frames,
        (end - first) as u64,
        "every frame captured before the drop"
    );
    let (info, samples) = read_take(&take_path(take));
    assert!(!info.length_repaired);
    assert_capture(&samples, &[0], first, &[]);
}

/// A sync of a take's data that fails ends the take with the error, its later frames dropped, its
/// sizes left unknown, as a crash leaves them.
#[test]
fn a_failed_sync_ends_the_take_with_its_error() {
    let mut rig = RecordRig::new("sync-error", 1, |_| {});
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    rig.period(PERIOD, &capture);
    rig.recorder().fail_syncs(true);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    // A second and a half of audio: the sync asked after the first second fails.
    for _ in 0..280 {
        rig.period(PERIOD, &capture);
        rig.wait_idle();
    }
    rig.rig.controller.stop().unwrap();
    for _ in 0..4 {
        rig.period(PERIOD, &capture);
    }
    let first = audible_at(&rig);
    rig.wait_idle();
    let errors = rig.recorder().stats().write_errors();
    rig.recorder().fail_syncs(false);
    let takes = rig.finish();
    assert!(errors >= 1);
    assert_eq!(rig.rig.controller.stats().capture_overruns(), 0);
    assert_eq!(takes.len(), 1);
    let take = &takes[0];
    let error = take
        .error
        .as_ref()
        .expect("the take reports the failed sync");
    assert!(error.to_string().contains("sync"), "{error}");
    // Ended at the pass after the failed sync, well before the stop's 71,680 frames.
    assert!(
        (48_000..=48_000 + 2 * 4_096).contains(&take.frames),
        "{}",
        take.frames
    );
    let (info, samples) = read_take(&take_path(take));
    assert!(info.length_repaired, "its sizes say unknown");
    assert_eq!(info.frames, take.frames);
    assert_capture(&samples, &[0], first, &[]);
}
