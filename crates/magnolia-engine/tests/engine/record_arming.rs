//! Arming and disarming through the controller (ADR-0032): every refusal gives the tap back; a
//! track disarmed and armed again in one commit ends its take and records the next play with its
//! new tap; a commit deferred behind a swap in flight carries its taps when it goes.

use magnolia_engine::engine::{ArmError, Commit, Destination, Source};
use magnolia_engine::nodes::Sine;
use magnolia_engine::record::{RecordConfig, RecordTrack, Recorder, TakeEnd};
use magnolia_engine::time::{SamplePosition, SampleRate};

use crate::disk_support::TempDir;
use crate::record_support::{RecordRig, files_in, read_take, signal, take_path};
use crate::support::rate;

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

/// Plays `count` periods.
fn periods(rig: &mut RecordRig, count: usize) {
    for _ in 0..count {
        rig.period(PERIOD, &capture);
    }
}

/// Checks that a take's mono samples are capture channel `channel` from engine sample `first` on.
fn assert_channel(samples: &[f32], channel: usize, first: i64) {
    for (frame, &sample) in samples.iter().enumerate() {
        let expected = signal(channel, first + frame as i64);
        assert_eq!(sample.to_bits(), expected.to_bits(), "frame {frame}");
    }
}

#[test]
fn arming_refuses_what_cannot_record_and_gives_the_tap_back() {
    let mut rig = RecordRig::new("arm-errors", 2, |_| {});

    // A capture channel the engine lacks.
    let tap = rig.tap(1, "voice", &[0, 2]);
    let Err(ArmError::ChannelOutOfRange {
        tap,
        channel,
        capture_channels,
    }) = rig.rig.controller.arm(tap)
    else {
        panic!("channel 2 of 2 is refused");
    };
    assert_eq!((tap.track(), channel, capture_channels), (1, 2, 2));
    drop(tap);

    // A recorder at another rate.
    let other_dir = TempDir::new("arm-errors-rate");
    let other_rate = SampleRate::new(44_100).unwrap();
    let mut other = Recorder::start(
        RecordConfig::new(other_dir.path(), other_rate),
        rig.rig.controller.stats(),
    )
    .unwrap();
    let tap = other
        .tap(RecordTrack {
            number: 4,
            name: "other".into(),
            capture_channels: vec![0],
        })
        .unwrap();
    let error = rig.rig.controller.arm(tap).unwrap_err();
    assert!(error.to_string().contains("44100 Hz"), "{error}");
    let ArmError::RateMismatch { tap, engine_rate } = error else {
        panic!("another rate is refused");
    };
    assert_eq!(
        (tap.track(), tap.sample_rate(), engine_rate),
        (4, other_rate, 48_000)
    );
    drop(tap);
    drop(other);

    // A track armed twice: the second tap comes back, and arms once the first is disarmed.
    rig.arm(1, "voice", &[0]);
    let second = rig.tap(1, "voice", &[1]);
    let Err(ArmError::AlreadyArmed(second)) = rig.rig.controller.arm(second) else {
        panic!("a track armed already is refused");
    };
    assert_eq!(second.capture_channels(), &[1]);
    rig.rig.controller.disarm(1).unwrap();
    rig.rig.controller.arm(second).unwrap();

    // A track never armed.
    let error = rig.rig.controller.disarm(7).unwrap_err();
    assert!(matches!(error, ArmError::NotArmed(7)), "{error}");
    assert_eq!(error.to_string(), "track 7 is not armed");

    // What was armed in the end records: track 1 on channel 1.
    rig.rig.controller.commit().unwrap();
    periods(&mut rig, 1);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    periods(&mut rig, 4);
    let first = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    periods(&mut rig, 4);
    let takes = rig.finish();
    assert_eq!(takes.len(), 1);
    let (_, samples) = read_take(&take_path(&takes[0]));
    assert_channel(&samples, 1, first);
}

/// One commit disarms track 1 mid-take and arms it again on another channel: the take ends where
/// the swap applied, and the next play records the new channel. The new tap numbers its takes past
/// the old tap's file made ahead, which the old track removes as it leaves: a gap in the numbers.
#[test]
fn a_track_disarmed_and_armed_again_in_one_commit_records_with_its_new_tap() {
    let mut rig = RecordRig::new("rearm", 2, |_| {});
    rig.arm(1, "voice", &[0]);
    rig.rig.controller.commit().unwrap();
    periods(&mut rig, 1);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    periods(&mut rig, 10);
    let first = audible_at(&rig);
    rig.wait_idle();
    rig.rig.controller.disarm(1).unwrap();
    let tap = rig.tap(1, "voice", &[1]);
    rig.rig.controller.arm(tap).unwrap();
    rig.rig.controller.commit().unwrap();
    let swapped_at = rig.position;
    periods(&mut rig, 10);
    rig.rig.controller.stop().unwrap();
    periods(&mut rig, 4);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    periods(&mut rig, 10);
    let second = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    periods(&mut rig, 4);
    let mut takes = rig.finish();
    takes.sort_by_key(|take| take.play);
    assert_eq!(takes.len(), 2, "{takes:?}");
    assert_eq!((takes[0].play, takes[0].ended), (1, TakeEnd::Disarmed));
    assert_eq!(takes[0].frames, (swapped_at - first) as u64);
    let (_, samples) = read_take(&take_path(&takes[0]));
    assert_channel(&samples, 0, first);
    assert_eq!((takes[1].play, takes[1].ended), (2, TakeEnd::Stopped));
    let (_, samples) = read_take(&take_path(&takes[1]));
    assert_channel(&samples, 1, second);
    assert_eq!(takes[0].take, Some(1));
    assert_eq!(takes[1].take, Some(3), "past the old tap's file made ahead");
    assert_eq!(
        files_in(rig.dir.path()),
        ["1-voice-take1.wav", "1-voice-take3.wav"]
    );
}

/// A commit made while a swap is in flight waits; when it goes, it carries the tap armed with it,
/// which then records.
#[test]
fn a_deferred_commit_carries_its_taps_when_it_goes() {
    let mut rig = RecordRig::new("deferred", 1, |_| {});
    rig.rig.controller.commit().unwrap();
    periods(&mut rig, 1);
    // A swap that adds an edge crossfades: it stays in flight for a fade.
    let sine = rig.rig.add(Sine::new(rate(), 440.0, 0.1));
    rig.rig
        .controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    assert!(matches!(
        rig.rig.controller.commit().unwrap(),
        Commit::Sent { .. }
    ));
    rig.arm(1, "voice", &[0]);
    assert_eq!(rig.rig.controller.commit().unwrap(), Commit::Deferred);
    // Periods shorter than the fade: the deferred commit goes once the crossfade is steady.
    for _ in 0..8 {
        rig.period(64, &capture);
    }
    assert_eq!(rig.rig.controller.stats().last_swap_applied(), 3);
    rig.rig.controller.play(SamplePosition(0)).unwrap();
    periods(&mut rig, 6);
    let first = audible_at(&rig);
    rig.rig.controller.stop().unwrap();
    periods(&mut rig, 4);
    let takes = rig.finish();
    assert_eq!(takes.len(), 1);
    let (_, samples) = read_take(&take_path(&takes[0]));
    assert_channel(&samples, 0, first);
}
