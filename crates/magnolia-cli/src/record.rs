//! `magnolia record`: a test session played through the JACK backend as `play` plays it, its
//! armed input tracks recorded into takes in a folder, then the takes, the recovery made at the
//! start, the recorder's figures and the engine's.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use magnolia_engine::record::{RecordTrack, Recorder, TakeEnd, TakeSummary};
use magnolia_engine::stats::EngineStats;
use magnolia_session::{RecordSettings, Session, SessionRecorder};

use crate::arguments::RecordOptions;
use crate::latency;
use crate::play::{Closed, Span, Watch, connect, open_backend, roll, seconds, server_name};
use crate::recover::print_recovery;
use crate::say;

/// How long the recorder may take, once a take's last frame is captured, to write it, sync it and
/// write its sizes: far more than a disk that keeps up needs.
const TAKE_END_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the main thread looks whether the takes have ended.
const TAKE_LOOK: Duration = Duration::from_millis(10);

/// Records the session `options` name: starts the recorder on the folder, which recovers it and
/// holds its lock, opens the backend, connects it if asked, sets the monitoring path's latency and
/// prints the round trip; plays as `play` does, stops, waits until every armed track's take has
/// ended, closes the client, then prints the takes, the recovery, the recorder's figures and the
/// engine's.
///
/// # Errors
///
/// A plain message when the session does not open or has no armed input track, when another
/// recorder or a recovery holds the folder, or it cannot be used, for whatever `play` fails on,
/// when a take does not end in time; and, after printing, when a take's writing failed, a change
/// of the device's latency ended a take, or the capture tap panicked.
pub fn record(options: &RecordOptions) -> Result<(), Box<dyn Error>> {
    let play = &options.play;
    let session = Session::open(&play.session)?;
    let tracks = session.armed_tracks();
    if tracks.is_empty() {
        return Err(format!(
            "{} has no armed input track: there is nothing to record",
            play.session.display()
        )
        .into());
    }
    let rate = session.sample_rate();
    let span = Span::new(play, rate, session.last_sample())?;
    let settings = RecordSettings {
        folder: options.folder.clone(),
        extra_latency: options.extra_latency,
        format: options.format,
    };
    let (mut recording, processor) = SessionRecorder::new(&session, settings)?;
    let backend = open_backend(play, processor)?;
    say(format_args!(
        "Recording {} into {} on {}, as the client {:?}: {} frames at {} Hz, from {} s to {} s.",
        play.session.display(),
        options.folder.display(),
        server_name(play),
        backend.client_name(),
        backend.period_frames(),
        backend.server_sample_rate(),
        seconds(span.from, rate),
        seconds(span.end, rate),
    ));
    for track in &tracks {
        say(format_args!(
            "Armed: track {} {:?}, from capture ports {}.",
            track.number,
            track.name,
            numbered_from_one(&track.capture_channels)
        ));
    }
    connect(play, &backend)?;
    let monitoring = session.monitoring_latency().unwrap_or(0);
    let latency = latency::settle(&backend, monitoring, Some(options.extra_latency), rate);
    say(latency);

    let stats = Arc::clone(recording.player.stats());
    let watch = Watch {
        stats: &stats,
        backend: &backend,
        rate,
    };
    let rolled = roll(&mut recording.player, &watch, span).and_then(|sequence| {
        wait_for_takes(
            &recording.recorder,
            &watch,
            sequence,
            &tracks,
            options.extra_latency,
        )
    });
    let closed = Closed::close(backend, &recording.player);

    let takes = recording.recorder.takes();
    print_takes(&takes);
    say("The recovery of the folder, before recording:");
    print_recovery(recording.recorder.recovery());
    print_recorder(&recording.recorder, &tracks, &stats);
    closed.print(&stats);
    rolled?;
    closed.outcome(&stats)?;
    takes_outcome(&takes, &stats)
}

/// The engine's capture channels as the JACK ports number them, from 1.
fn numbered_from_one(channels: &[usize]) -> String {
    channels
        .iter()
        .map(|channel| channel.saturating_add(1).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Waits until each of `tracks` has a take of play `sequence` that has ended: its last frame is
/// captured a round trip after the output played the stop, which the roll has waited for; then
/// the recorder writes it, syncs it and writes its sizes.
fn wait_for_takes(
    recorder: &Recorder,
    watch: &Watch<'_>,
    sequence: u64,
    tracks: &[RecordTrack],
    extra_latency: u32,
) -> Result<(), Box<dyn Error>> {
    let round_trip = watch
        .stats
        .device_latency()
        .round_trip()
        .saturating_add(u64::from(extra_latency));
    let captured_at = watch
        .stats
        .block_start()
        .0
        .saturating_add(i64::try_from(round_trip).unwrap_or(i64::MAX))
        .saturating_add(i64::from(watch.backend.period_frames()));
    watch.until(captured_at)?;
    let ended = || {
        let takes = recorder.takes();
        tracks.iter().all(|track| {
            takes
                .iter()
                .any(|take| take.track == track.number && take.play == sequence)
        })
    };
    let deadline = Instant::now() + TAKE_END_TIMEOUT;
    watch.wait(|| {
        if ended() || recorder.has_failed() || Instant::now() >= deadline {
            Ok(())
        } else {
            Err(TAKE_LOOK)
        }
    })?;
    if recorder.has_failed() {
        return Err(
            "the recorder's thread panicked: the takes under way stay as a crash leaves \
                    them, and `magnolia recover` repairs them"
                .into(),
        );
    }
    if !ended() {
        return Err(format!(
            "the takes did not end within {} s of their last frame: the disk may be stalled",
            TAKE_END_TIMEOUT.as_secs()
        )
        .into());
    }
    Ok(())
}

/// Prints each take that ended: its file, track, place, length and how it ended.
fn print_takes(takes: &[TakeSummary]) {
    say(format_args!("Takes: {}", takes.len()));
    for take in takes {
        let file = take.path.as_ref().map_or_else(
            || "no file, as it recorded no frame".to_owned(),
            |path| path.display().to_string(),
        );
        say(format_args!(
            "  {file}: track {}, from timeline sample {}, {} frames, {} of them padded, placed by \
             a round trip of {} frames; {}",
            take.track,
            take.first_sample.0,
            take.frames,
            take.padded,
            take.round_trip,
            how_it_ended(take.ended)
        ));
        if let Some(error) = &take.error {
            say(format_args!(
                "    its writing failed: {error}; its file is left as a crash leaves it, which \
                 `magnolia recover` repairs"
            ));
        }
    }
}

/// How a take ended, for a person to read.
fn how_it_ended(ended: TakeEnd) -> &'static str {
    match ended {
        TakeEnd::Stopped => "it ended at the stop",
        TakeEnd::Disarmed => "it ended when its track was disarmed",
        TakeEnd::LatencyChanged => "it ended when the device's latency changed",
        TakeEnd::TapDropped => "it ended when the engine let its track go",
        TakeEnd::TapStopped => "it ended when the capture tap panicked",
        TakeEnd::RecorderClosed => "it ended when the recorder closed",
    }
}

/// Prints the recorder's figures, each armed track's rings at their fullest, and the capture's
/// counters of the engine.
fn print_recorder(recorder: &Recorder, tracks: &[RecordTrack], engine: &EngineStats) {
    let stats = recorder.stats();
    say(format_args!(
        "recorder: {} takes started, {} finished; {} frames written, {} padded in {} gaps; {} \
         write errors; {} takes ended by a change of latency",
        stats.takes_started(),
        stats.takes_finished(),
        stats.frames_written(),
        stats.frames_padded(),
        stats.gaps(),
        stats.write_errors(),
        stats.latency_ends()
    ));
    say(format_args!(
        "recorder's passes: {}, {:?} in all, the longest {:?}; syncs: {}, {:?} in all, the \
         longest {:?}",
        stats.passes(),
        stats.pass_time(),
        stats.longest_pass(),
        stats.syncs(),
        stats.sync_time(),
        stats.longest_sync()
    ));
    for track in tracks {
        if let Some(fill) = recorder.fullest_fill(track.number) {
            say(format_args!(
                "rings of track {} at their fullest: {} frames, {} marks",
                track.number, fill.frames, fill.marks
            ));
        }
    }
    say(format_args!(
        "capture overruns: {} frames; missed takes: {}",
        engine.capture_overruns(),
        engine.missed_takes()
    ));
}

/// Whether every take ended whole: none whose writing failed, none that a change of latency ended,
/// and a capture tap that never panicked.
fn takes_outcome(takes: &[TakeSummary], stats: &EngineStats) -> Result<(), Box<dyn Error>> {
    if stats.tap_stopped() {
        return Err("the capture tap panicked: the takes under way ended at what they held".into());
    }
    if takes.iter().any(|take| take.error.is_some()) {
        return Err("a take's writing failed: `magnolia recover` repairs what it wrote".into());
    }
    if takes
        .iter()
        .any(|take| take.ended == TakeEnd::LatencyChanged)
    {
        return Err(
            "the device's latency changed during a take, which ended it there, rather \
                    than place the rest of it wrong"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_channels_are_shown_as_the_ports_number_them() {
        assert_eq!(numbered_from_one(&[0]), "1");
        assert_eq!(numbered_from_one(&[3, 1]), "4, 2");
    }
}
