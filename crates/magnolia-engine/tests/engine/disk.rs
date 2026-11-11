//! The disk reader and the audio players (ADR-0026): audio clips stream from WAV files through the
//! processor bit for bit as the test mixes them from the files itself, rendered offline and played
//! with the reader on its own thread; a play from inside a clip fades that clip in, and nothing
//! else; a stop fades out; an underrun is silence, counted; a locate discards the old generation; a
//! play while rolling fades out over the old play's own audio, then starts at its sample; silent
//! blocks flag the player's output silent; frames past a file's end are silence, counted; a player
//! through a latency reads ahead by that latency; a track that does not fit is refused.

use std::path::Path;
use std::time::Duration;

use magnolia_engine::disk::{AudioClip, AudioFile, AudioTrack, DiskConfig, DiskError, DiskReader};
use magnolia_engine::disk::{BLOCK_FRAMES, PrimeError};
use magnolia_engine::engine::{Destination, Source, TransportError};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::TestLatency;
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{SampleFormat, WavWriter, read_info};

use crate::disk_support::{
    DiskRig, Reader, TempDir, TrackSpec, assert_plays, audio_tracks, clip, read_back, reference,
    references, underruns, write_samples, write_wav,
};
use crate::support::{FADE, Shared, fade_in, rate, read, shared};
use crate::timeline_support::Random;

/// Six blocks ahead: a refill every three blocks or so, so that the tests wake the reader often.
const SHORT_READ_AHEAD: u64 = 6 * BLOCK_FRAMES as u64;

/// Two tracks: a stereo one of 16-bit, 24-bit and float files, with a gap, a clip at a file
/// offset, and gains of 1 and 0.5; a mono one of a 24-bit and a float file.
fn two_tracks(dir: &TempDir) -> Vec<TrackSpec> {
    let int16 = write_wav(dir, "int16.wav", SampleFormat::Int16, 2, 30_000, 1);
    let int24 = write_wav(dir, "int24.wav", SampleFormat::Int24, 2, 20_000, 2);
    let float = write_wav(dir, "float.wav", SampleFormat::Float32, 2, 25_000, 3);
    let mono24 = write_wav(dir, "mono24.wav", SampleFormat::Int24, 1, 40_000, 4);
    let mono32 = write_wav(dir, "mono32.wav", SampleFormat::Float32, 1, 15_000, 5);
    vec![
        TrackSpec {
            channels: 2,
            clips: vec![
                clip(&int16, 1_000, 0, 20_000, 1.0),
                // After a gap, from inside its file.
                clip(&int24, 30_000, 5_000, 12_000, 0.5),
                // Right after the last.
                clip(&float, 42_000, 1_234, 20_000, 1.0),
            ],
        },
        TrackSpec {
            channels: 1,
            clips: vec![
                clip(&mono24, 0, 7_777, 25_000, 0.5),
                clip(&mono32, 50_000, 0, 15_000, 1.0),
            ],
        },
    ]
}

/// Past every clip's end in `two_tracks`.
const TWO_TRACKS_LENGTH: usize = 70_000;

/// Periods of `frames` until `total` frames at least.
fn periods_of(frames: usize, total: usize) -> Vec<usize> {
    vec![frames; total.div_ceil(frames)]
}

#[test]
fn audio_clips_render_bit_for_bit_and_play_so_on_the_readers_thread() {
    let dir = TempDir::new("render");
    let tracks = two_tracks(&dir);
    let expected = references(&tracks, 0, TWO_TRACKS_LENGTH);

    let mut offline = DiskRig::new(&tracks, Reader::Offline, 4_096, SHORT_READ_AHEAD);
    offline.play_from(0);
    let rendered = offline.play(&periods_of(4_096, TWO_TRACKS_LENGTH));
    let at = offline.audible_at();
    assert_eq!(
        at, 0,
        "nothing latent: the output reaches the play's sample at once"
    );
    assert_plays(&rendered, at, &expected, "the offline render");
    assert_eq!(underruns(&offline), 0);
    assert!(offline.reader.stats().reads() > 0);
    assert_eq!(offline.reader.stats().unread_frames(), 0);
    assert!(
        rendered
            .iter()
            .any(|channel| channel.iter().any(|&sample| sample != 0.0)),
        "the clips sound"
    );

    let mut threaded = DiskRig::new(&tracks, Reader::Thread, 4_096, SHORT_READ_AHEAD);
    threaded.play_from(0);
    let mut random = Random(0x5EED_D15C);
    let mut periods = Vec::new();
    while periods.iter().sum::<usize>() < TWO_TRACKS_LENGTH {
        periods.push(random.up_to(5_000));
    }
    let played = threaded.play(&periods);
    assert_eq!(threaded.audible_at(), 0);
    assert_eq!(underruns(&threaded), 0);
    for (channel, (played, rendered)) in played.iter().zip(&rendered).enumerate() {
        let length = TWO_TRACKS_LENGTH;
        assert!(
            played[..length]
                .iter()
                .zip(&rendered[..length])
                .all(|(played, rendered)| played.to_bits() == rendered.to_bits()),
            "channel {channel}: played on the reader's thread as rendered"
        );
    }
    // Refills came while it played: the reader read far more than the read-ahead.
    let read = threaded.reader.stats().blocks_filled() * BLOCK_FRAMES as u64;
    assert!(read > 4 * SHORT_READ_AHEAD, "read {read} frames");
    let low_water = threaded.reader.low_water(0).unwrap();
    assert!(low_water > 0, "the player never ran dry");
}

#[test]
fn a_play_from_inside_a_clip_fades_that_clip_in_and_nothing_else() {
    let dir = TempDir::new("fade-in");
    let float = write_wav(&dir, "float.wav", SampleFormat::Float32, 1, 60_000, 6);
    let int16 = write_wav(&dir, "int16.wav", SampleFormat::Int16, 1, 60_000, 7);
    let from = 9_950;
    let tracks = vec![
        TrackSpec {
            channels: 1,
            clips: vec![
                // Crosses the play's sample, and ends inside the fade.
                clip(&float, 0, 0, 10_000, 1.0),
                // Starts inside the fade: not faded.
                clip(&int16, 10_100, 0, 9_000, 1.0),
            ],
        },
        // Starts on the play's sample: not faded.
        TrackSpec {
            channels: 1,
            clips: vec![clip(&int16, from, 300, 5_000, 0.5)],
        },
        // Crosses the play's sample: faded over the whole fade.
        TrackSpec {
            channels: 1,
            clips: vec![clip(&float, -20_000, 0, 50_000, 0.5)],
        },
    ];
    let mut rig = DiskRig::new(&tracks, Reader::Offline, 1_024, SHORT_READ_AHEAD);
    rig.play_from(from);
    let played = rig.play(&periods_of(1_024, 20_000));
    let at = rig.audible_at();
    assert_plays(&played, at, &references(&tracks, from, 15_000), "the play");

    let float_samples = read_back(&float);
    let int16_samples = read_back(&int16);
    // The crossing clip, faded from the play's sample: its first frame barely sounds.
    let first = float_samples[from as usize] * fade_in(0, FADE) as f32;
    assert_eq!(played[0][at].to_bits(), first.to_bits());
    assert!(played[0][at].abs() < float_samples[from as usize].abs());
    // The clip that starts inside the fade, and the one that starts on the sample, unfaded.
    assert_eq!(played[0][at + 150].to_bits(), int16_samples[0].to_bits());
    assert_eq!(
        played[1][at].to_bits(),
        (int16_samples[300] * 0.5).to_bits()
    );
    // The long crossing clip, faded over the whole fade, then whole.
    let into_file = (from + 20_000) as usize;
    for offset in [0, 100, FADE - 1] {
        let faded = float_samples[into_file + offset] * 0.5 * fade_in(offset, FADE) as f32;
        assert_eq!(played[2][at + offset].to_bits(), faded.to_bits());
    }
    assert_eq!(
        played[2][at + FADE].to_bits(),
        (float_samples[into_file + FADE] * 0.5).to_bits()
    );
}

#[test]
fn an_underrun_is_silence_counted_and_the_rest_plays() {
    let dir = TempDir::new("underrun");
    let file = write_wav(&dir, "long.wav", SampleFormat::Int24, 2, 100_000, 8);
    let tracks = vec![TrackSpec {
        channels: 2,
        clips: vec![clip(&file, 0, 0, 100_000, 1.0)],
    }];
    let read_ahead = 4 * BLOCK_FRAMES as u64;
    let mut rig = DiskRig::new(&tracks, Reader::Thread, 1_024, read_ahead);
    rig.play_from(0);
    let mut played = rig.play(&[1_024; 20]);
    assert_eq!(underruns(&rig), 0);
    assert!(rig.reader.wait_idle(Duration::from_secs(10)));

    // The reader stalls, as a disk might: the player plays what it holds, then silence.
    rig.reader.pause(true);
    for _ in 0..30 {
        let period = rig.rig.backend.play_period(1_024, &[]);
        for (all, channel) in played.iter_mut().zip(period) {
            all.extend_from_slice(channel);
        }
    }
    let lacked = underruns(&rig);
    rig.reader.pause(false);
    let after = rig.play(&[1_024; 20]);
    for (all, channel) in played.iter_mut().zip(after) {
        all.extend(channel);
    }

    let expected = references(&tracks, 0, 70 * 1_024);
    let stalled = 50 * 1_024;
    let first_missing = played[0]
        .iter()
        .zip(&expected[0])
        .position(|(played, expected)| played.to_bits() != expected.to_bits())
        .unwrap();
    assert!(
        (20 * 1_024..stalled).contains(&first_missing),
        "it played what it held: {first_missing}"
    );
    assert_eq!(first_missing % BLOCK_FRAMES, 0, "up to a block's end");
    for channel in &played {
        assert!(
            channel[first_missing..stalled]
                .iter()
                .all(|&sample| sample.to_bits() == 0),
            "the frames it lacked are silence"
        );
    }
    assert_eq!(
        lacked,
        (stalled - first_missing) as u64,
        "each lacked frame counted"
    );
    // Once the reader goes on, the rest plays as mixed from the file.
    let rest: Vec<Vec<f32>> = expected
        .iter()
        .map(|channel| channel[stalled..].to_vec())
        .collect();
    assert_plays(&played, stalled, &rest, "after the stall");
    assert_eq!(underruns(&rig), lacked);
}

/// The frames a play from `from` plays over `length`, faded out over the fade before `stop`, the
/// first sample past the audible region; silence from `stop` on.
fn stopped(track: &TrackSpec, from: i64, stop: i64, length: usize) -> Vec<Vec<f32>> {
    let mut played = reference(track, from, length);
    for channel in &mut played {
        for (index, sample) in channel.iter_mut().enumerate() {
            let at = from + index as i64;
            let into_fade = at - (stop - FADE as i64);
            if at >= stop {
                *sample = 0.0;
            } else if into_fade >= 0 {
                *sample *= 1.0 - fade_in(into_fade as usize, FADE) as f32;
            }
        }
    }
    played
}

/// One mono track, one clip of a long float file from the timeline's zero.
fn long_track(dir: &TempDir) -> TrackSpec {
    let file = write_wav(dir, "long.wav", SampleFormat::Float32, 1, 300_000, 9);
    TrackSpec {
        channels: 1,
        clips: vec![clip(&file, 0, 0, 300_000, 1.0)],
    }
}

/// Plays periods and appends what they played to `all`.
fn play_into(rig: &mut DiskRig, all: &mut [Vec<f32>], periods: &[usize]) {
    for (all, channel) in all.iter_mut().zip(rig.play(periods)) {
        all.extend(channel);
    }
}

#[test]
fn a_stop_fades_out_and_a_locate_discards_the_old_generation() {
    let dir = TempDir::new("locate");
    let track = long_track(&dir);
    let tracks = vec![track.clone()];
    // Ten slots of 4,096 frames: the first play's generation still holds the blocks 8,100 frames
    // after its sample when it stops.
    let mut rig = DiskRig::new(&tracks, Reader::Offline, 256, 8 * BLOCK_FRAMES as u64);
    let mut played = vec![Vec::new()];
    let first_from = 10_000;
    let first = rig.play_from(first_from);
    play_into(&mut rig, &mut played, &[256; 20]);
    rig.rig.controller.stop().unwrap();
    // Nothing latent: the stop ends a fade after the clock's place when it applies.
    let stop = first_from + 20 * 256 + FADE as i64;
    play_into(&mut rig, &mut played, &[256; 4]);
    let first_at = rig.audible_at();
    assert_plays(
        &played,
        first_at,
        &stopped(&track, first_from, stop, 24 * 256),
        "the first play and its stop",
    );

    // A play from inside what the first play's generation still holds, sent before the reader
    // fills for it: the slots hold the right blocks of the old generation, which it must never
    // play. It waits, silent and lacking nothing, until the reader has primed its cue, then starts
    // from its own sample.
    let second_from = first_from + 8_100;
    let second = rig.reader.locate(SamplePosition(second_from));
    let sequence = rig.rig.controller.play(second).unwrap();
    let period = rig.rig.backend.play_period(256, &[])[0].clone();
    played[0].extend(&period);
    assert!(period.iter().all(|&sample| sample.to_bits() == 0));
    let last_start = rig.rig.controller.stats().play_start().unwrap();
    assert_ne!(last_start.sequence, sequence, "the play waits for its cue");
    assert_eq!(underruns(&rig), 0);
    let waited = played[0].len();
    play_into(&mut rig, &mut played, &[256; 40]);
    let second_at = rig.audible_at();
    assert_eq!(
        second_at, waited,
        "it starts at the period after the reader filled"
    );
    assert_plays(
        &played,
        second_at,
        &[reference(&track, second_from, 30 * 256)[0].clone()],
        "the second play, from its own sample",
    );
    assert_eq!(underruns(&rig), 0);

    // A cue plays once.
    assert_eq!(
        rig.rig.controller.play(first),
        Err(TransportError::CueAlreadyPlayed)
    );
    // A cue that a later locate replaced is primed no more, and plays silence, counted: the slots
    // hold the right blocks of the later generation, which it must never play.
    let replaced = rig.reader.locate(SamplePosition(59_900));
    let latest = rig.reader.locate(SamplePosition(60_000));
    assert_eq!(
        rig.reader.wait_primed(replaced, Duration::from_secs(1)),
        Err(PrimeError::Superseded)
    );
    rig.rig.controller.stop().unwrap();
    play_into(&mut rig, &mut played, &[256; 4]);
    rig.rig.controller.play(replaced).unwrap();
    let before = played[0].len();
    play_into(&mut rig, &mut played, &[256; 8]);
    let replaced_at = rig.audible_at();
    assert!(
        played[0][replaced_at..before + 8 * 256]
            .iter()
            .all(|&sample| sample.to_bits() == 0)
    );
    assert!(underruns(&rig) > 0);
    // The latest cue plays.
    rig.reader
        .wait_primed(latest, Duration::from_secs(1))
        .unwrap();
    rig.rig.controller.play(latest).unwrap();
    play_into(&mut rig, &mut played, &[256; 30]);
    assert_plays(
        &played,
        rig.audible_at(),
        &[reference(&track, 60_000, 20 * 256)[0].clone()],
        "the latest cue",
    );
}

#[test]
fn a_play_while_rolling_fades_out_over_the_old_audio_then_starts_at_its_sample() {
    let dir = TempDir::new("rolling");
    let track = long_track(&dir);
    let tracks = vec![track.clone()];
    // Far ahead; and a hundred frames ahead, inside the stop's fade, where the new play's first
    // block is the very block the old play fades out over.
    for jump in [60_000, 100] {
        for reader in [Reader::Offline, Reader::Thread] {
            let mut rig = DiskRig::new(&tracks, reader, 256, 8 * BLOCK_FRAMES as u64);
            let mut played = vec![Vec::new()];
            let first_from = 10_000;
            rig.play_from(first_from);
            play_into(&mut rig, &mut played, &[256; 40]);
            let first_at = rig.audible_at();
            // Nothing latent: the clock at the next period is the sample the output plays then.
            let command_at = played[0].len();
            let clock = first_from + (command_at - first_at) as i64;
            let second_from = clock + jump;
            let cue = rig.reader.locate(SamplePosition(second_from));
            rig.rig.controller.play(cue).unwrap();
            // Periods shorter than the fade: the reader runs between the stop's periods, while
            // the old play still reads the slots it keeps for it.
            play_into(&mut rig, &mut played, &[64; 200]);
            let second_at = rig.audible_at();
            let what = format!("{reader:?}, a jump of {jump}");

            // The old play, then its fade over its own audio up to the stop, then silence.
            let stop = clock + FADE as i64;
            let old = stopped(&track, first_from, stop, second_at - first_at);
            assert_plays(&played, first_at, &old, &format!("{what}: the old play"));
            // The new play from its own sample, faded in, as the clip crosses it.
            let new = reference(&track, second_from, played[0].len() - second_at);
            assert_plays(&played, second_at, &new, &format!("{what}: the new play"));
            assert_eq!(underruns(&rig), 0, "{what}");
        }
    }
}

/// One input passed through to one output, recording where each call lay on the timeline and
/// whether its input was silent.
struct Watcher {
    calls: Shared<Vec<(i64, usize, bool)>>,
}

impl Node for Watcher {
    fn input_channels(&self) -> usize {
        1
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let transport = context.transport();
        let silent = context.input_is_silent(0);
        let position = if transport.rolling {
            transport.position.0
        } else {
            i64::MIN
        };
        read(&self.calls).push((position, context.frames(), silent));
        let input = context.input(0);
        context.output_mut(0).copy_from_slice(input);
        context.set_output_silent(0, silent);
        ProcessStatus::Continue
    }
}

#[test]
fn silent_blocks_flag_the_players_output_silent() {
    let dir = TempDir::new("silent");
    let sound = write_wav(&dir, "sound.wav", SampleFormat::Int16, 1, 8_192, 10);
    let zeros = write_samples(&dir, "zeros.wav", SampleFormat::Int16, 1, &[0.0; 8_192]);
    let tracks = vec![TrackSpec {
        channels: 1,
        clips: vec![
            clip(&sound, 0, 0, 8_192, 1.0),
            // A gap of three blocks, then a clip of digital silence.
            clip(&zeros, 20_480, 0, 8_192, 1.0),
        ],
    }];
    let calls = shared(Vec::new());
    let mut watcher = Some(Watcher {
        calls: calls.clone(),
    });
    let mut rig = DiskRig::routed(
        &tracks,
        Reader::Offline,
        1_024,
        SHORT_READ_AHEAD,
        |rig, _, player| {
            let watcher = rig.add(watcher.take().unwrap());
            rig.controller
                .connect(Source::Node(player, 0), Destination::Node(watcher, 0));
            Some(watcher)
        },
    );
    rig.play(&[1_024; 2]);
    rig.play_from(0);
    rig.play(&[1_024; 40]);
    let calls = read(&calls);
    assert!(calls[..2].iter().all(|&(_, _, silent)| silent), "stopped");
    let mut seen = [false; 2];
    for &(position, frames, silent) in &calls[2..] {
        assert_eq!(frames, 1_024);
        let sounding = position < 8_192;
        assert_eq!(silent, !sounding, "the call at {position}");
        seen[usize::from(sounding)] = true;
    }
    assert_eq!(seen, [true, true]);
    assert_eq!(underruns(&rig), 0);
}

#[test]
fn frames_past_a_files_end_are_silence_counted() {
    let dir = TempDir::new("short");
    let short = write_wav(&dir, "short.wav", SampleFormat::Int16, 1, 6_000, 11);
    let tracks = vec![TrackSpec {
        channels: 1,
        clips: vec![
            // Its last 4,000 frames lie past the file's end.
            clip(&short, 0, 0, 10_000, 1.0),
            // Wholly past the file's end.
            clip(&short, 20_000, 50_000, 3_000, 0.5),
        ],
    }];
    let mut rig = DiskRig::new(&tracks, Reader::Offline, 1_024, SHORT_READ_AHEAD);
    rig.play_from(0);
    let played = rig.play(&periods_of(1_024, 30_000));
    let expected = references(&tracks, 0, 30_000);
    assert_plays(&played, rig.audible_at(), &expected, "the short file");
    assert!(played[0][6_000..30_000].iter().all(|&sample| sample == 0.0));
    assert!(played[0][..6_000].iter().any(|&sample| sample != 0.0));
    assert_eq!(rig.reader.stats().unread_frames(), 4_000 + 3_000);
    assert_eq!(rig.reader.stats().read_errors(), 0);
    assert_eq!(underruns(&rig), 0);
}

#[test]
fn a_player_through_a_latency_reads_ahead_by_it_and_nulls_against_one_without() {
    const LATENCY: u32 = 3_000;
    let dir = TempDir::new("latency");
    let file = write_wav(&dir, "stereo.wav", SampleFormat::Int24, 2, 60_000, 12);
    let track = TrackSpec {
        channels: 2,
        clips: vec![clip(&file, 0, 0, 60_000, 1.0)],
    };
    let tracks = vec![track.clone(), track.clone()];
    let mut latent_player = None;
    for reader in [Reader::Offline, Reader::Thread] {
        let mut rig = DiskRig::routed(
            &tracks,
            reader,
            1_024,
            SHORT_READ_AHEAD,
            |rig, index, player| {
                (index == 0).then(|| {
                    latent_player = Some(player);
                    let latency = rig.add(TestLatency::new(2, LATENCY));
                    for channel in 0..2 {
                        rig.controller.connect(
                            Source::Node(player, channel),
                            Destination::Node(latency, channel),
                        );
                    }
                    latency
                })
            },
        );
        let from = 5_000;
        rig.play_from(from);
        let mut random = Random(0xC0FF_EE00);
        let periods: Vec<usize> = (0..40).map(|_| random.up_to(2_000)).collect();
        let played = rig.play(&periods);
        let at = rig.audible_at();
        assert_eq!(at, LATENCY as usize, "the play pre-rolls by the latency");
        let processor = rig.rig.backend.processor();
        assert_eq!(
            processor.ahead_in(latent_player.unwrap()),
            Some(u64::from(LATENCY))
        );
        assert_eq!(processor.ahead_in(rig.players[1]), Some(0));
        let what = format!("{reader:?}");
        for channel in 0..2 {
            assert!(
                played[channel]
                    .iter()
                    .zip(&played[channel + 2])
                    .all(|(latent, direct)| latent.to_bits() == direct.to_bits()),
                "{what}: channel {channel} nulls against its twin"
            );
        }
        let expected = reference(&track, from, played[0].len() - at);
        assert_plays(&played[..2], at, &expected, &what);
        assert_eq!(underruns(&rig), 0, "{what}");
    }
}

/// A WAV file at another rate than the engine's.
fn write_at(dir: &TempDir, name: &str, hertz: u32) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut writer = WavWriter::new(file, SampleFormat::Int16, 1, hertz).unwrap();
    writer.write_frames(&[0.25; 100]).unwrap();
    writer.finish().unwrap();
    path
}

fn audio_clip(path: &Path, start: i64, length: u64, gain: f32) -> AudioClip {
    AudioClip {
        file: AudioFile::open(path).unwrap(),
        start: SamplePosition(start),
        file_offset: 0,
        length,
        gain,
    }
}

fn refusal(config: DiskConfig, tracks: &[AudioTrack]) -> DiskError {
    let stats =
        magnolia_engine::engine::new(magnolia_engine::engine::EngineConfig::new(rate(), 64, 0, 1))
            .unwrap()
            .0
            .stats()
            .clone();
    match DiskReader::offline(config, tracks, &stats) {
        Ok(_) => panic!("the reader took what it should refuse"),
        Err(error) => error,
    }
}

#[test]
fn a_track_that_does_not_fit_the_engine_is_refused_plainly() {
    let dir = TempDir::new("refused");
    let mono = write_wav(&dir, "mono.wav", SampleFormat::Int16, 1, 1_000, 13);
    let stereo = write_wav(&dir, "stereo.wav", SampleFormat::Int16, 2, 1_000, 14);
    let other_rate = write_at(&dir, "44100.wav", 44_100);
    let config = DiskConfig::new(rate());
    let track = |clips| AudioTrack { channels: 1, clips };

    let error = refusal(config, &[track(vec![audio_clip(&other_rate, 0, 100, 1.0)])]);
    assert!(matches!(
        error,
        DiskError::RateMismatch {
            file_rate: 44_100,
            engine_rate: 48_000,
            ..
        }
    ));
    assert!(error.to_string().contains("44100 Hz"), "{error}");

    let error = refusal(config, &[track(vec![audio_clip(&stereo, 0, 100, 1.0)])]);
    assert!(matches!(
        error,
        DiskError::ChannelsMismatch {
            file_channels: 2,
            track_channels: 1,
            ..
        }
    ));

    // Given out of order: the reader sorts them, then finds the overlap.
    let overlapping = vec![
        audio_clip(&mono, 1_000, 500, 1.0),
        audio_clip(&mono, 0, 1_001, 1.0),
    ];
    let error = refusal(config, &[track(overlapping)]);
    assert!(matches!(
        error,
        DiskError::ClipsOverlap {
            track: 0,
            first: 1,
            second: 0
        }
    ));
    // Touching is not overlapping.
    let touching = vec![
        audio_clip(&mono, 1_000, 500, 1.0),
        audio_clip(&mono, 0, 1_000, 1.0),
    ];
    let stats =
        magnolia_engine::engine::new(magnolia_engine::engine::EngineConfig::new(rate(), 64, 0, 1))
            .unwrap()
            .0
            .stats()
            .clone();
    assert!(DiskReader::offline(config, &[track(touching)], &stats).is_ok());

    let error = refusal(config, &[track(vec![audio_clip(&mono, 0, 100, f32::NAN)])]);
    assert!(matches!(error, DiskError::GainNotFinite { .. }));
    let error = refusal(config, &[track(vec![audio_clip(&mono, 1 << 42, 100, 1.0)])]);
    assert!(matches!(error, DiskError::ClipOutOfRange { .. }));
    let error = refusal(
        config,
        &[AudioTrack {
            channels: 0,
            clips: Vec::new(),
        }],
    );
    assert!(matches!(error, DiskError::ChannelsOutOfRange { .. }));
    for read_ahead in [
        0,
        DiskConfig::MIN_READ_AHEAD - 1,
        DiskConfig::MAX_READ_AHEAD + 1,
    ] {
        let config = DiskConfig {
            read_ahead,
            ..config
        };
        assert!(matches!(
            refusal(config, &[]),
            DiskError::ReadAheadOutOfRange(_)
        ));
    }
}

#[test]
fn a_track_without_clips_plays_silence() {
    let tracks = vec![TrackSpec {
        channels: 2,
        clips: Vec::new(),
    }];
    let mut rig = DiskRig::new(&tracks, Reader::Thread, 256, SHORT_READ_AHEAD);
    rig.play_from(0);
    let played = rig.play(&[256; 8]);
    assert!(played.iter().flatten().all(|&sample| sample.to_bits() == 0));
    assert_eq!(underruns(&rig), 0);
    assert_eq!(rig.reader.stats().reads(), 0);
}

/// A play while rolling, its cue sent at once after its locate, with the reader on its thread and
/// the periods paced by the clock, never waiting for the reader: a jump of 100 frames, inside the
/// old play's last block; of 1 s and 1.5 s; and of 120 blocks, onto the old play's own slot. Each
/// new play starts once its cue is primed, exactly at its sample, and nothing underruns.
#[test]
fn a_play_while_rolling_starts_once_primed_exactly_at_its_sample() {
    let dir = TempDir::new("primed-jumps");
    let file = write_wav(&dir, "long.wav", SampleFormat::Float32, 1, 1_400_000, 14);
    let track = TrackSpec {
        channels: 1,
        clips: vec![clip(&file, 0, 0, 1_400_000, 1.0)],
    };
    let tracks = vec![track.clone()];
    let read_ahead = DiskConfig::new(rate()).read_ahead;
    for jump in [100, 48_000, 72_000, 120 * BLOCK_FRAMES as i64] {
        let mut rig = DiskRig::new(&tracks, Reader::Thread, 256, read_ahead);
        let first_from = 0;
        rig.play_from(first_from);
        let mut played = rig.play_in_real_time(&[256; 40]);
        let first_at = rig.audible_at();
        let command_at = played[0].len();
        let clock = first_from + (command_at - first_at) as i64;
        let second_from = clock + jump;
        let cue = rig.reader.locate(SamplePosition(second_from));
        rig.rig.controller.play(cue).unwrap();
        for (all, channel) in played.iter_mut().zip(rig.play_in_real_time(&[256; 120])) {
            all.extend(channel);
        }
        let second_at = rig.audible_at();
        let what = format!("a jump of {jump}");
        let stats = rig.rig.controller.stats();
        assert!(
            second_at > command_at,
            "{what}: the new play started; primed {} of {}, refills {}, blocks {}, idle {}",
            stats.primed_generation(),
            cue.generation(),
            rig.reader.stats().refills(),
            rig.reader.stats().blocks_filled(),
            rig.reader.wait_idle(Duration::from_millis(1))
        );
        assert_eq!(underruns(&rig), 0, "{what}");
        let stop = clock + FADE as i64;
        let old = stopped(&track, first_from, stop, second_at - first_at);
        assert_plays(&played, first_at, &old, &format!("{what}: the old play"));
        let new = reference(&track, second_from, played[0].len() - second_at);
        assert_plays(&played, second_at, &new, &format!("{what}: the new play"));
    }
}

/// A play while rolling in a session whose largest compensation, two latencies of 50,000 in series,
/// passes `MAX_LATENCY`: the track that is not latent reads 100,000 frames and a fade past its
/// position before its stop. Its old play fades out over its own audio, and both tracks stay in
/// line through the stop and the new play.
#[test]
fn the_old_play_keeps_all_its_compensation_reads_while_rolling_into_a_new_play() {
    const LATENCY: u32 = 50_000;
    let dir = TempDir::new("long-compensation");
    let file = write_wav(&dir, "long.wav", SampleFormat::Float32, 1, 1_000_000, 15);
    let track = TrackSpec {
        channels: 1,
        clips: vec![clip(&file, 0, 0, 1_000_000, 1.0)],
    };
    let tracks = vec![track.clone(), track.clone()];
    for reader in [Reader::Offline, Reader::Thread] {
        let mut rig = DiskRig::routed(
            &tracks,
            reader,
            1_024,
            40 * BLOCK_FRAMES as u64,
            |rig, index, player| {
                (index == 0).then(|| {
                    let first = rig.add(TestLatency::new(1, LATENCY));
                    let second = rig.add(TestLatency::new(1, LATENCY));
                    rig.controller
                        .connect(Source::Node(player, 0), Destination::Node(first, 0));
                    rig.controller
                        .connect(Source::Node(first, 0), Destination::Node(second, 0));
                    second
                })
            },
        );
        rig.play_from(0);
        let mut played = rig.play(&[1_024; 160]);
        let first_at = rig.audible_at();
        let command_at = played[1].len();
        let clock = (command_at - first_at) as i64;
        let second_from = clock + 19 * BLOCK_FRAMES as i64;
        let cue = rig.reader.locate(SamplePosition(second_from));
        rig.rig.controller.play(cue).unwrap();
        // The old play stops 100,240 frames on; the new one pre-rolls 100,000.
        play_into(&mut rig, &mut played, &[256; 900]);
        let second_at = rig.audible_at();
        let what = format!("{reader:?}");
        assert!(second_at > command_at, "{what}: the new play started");
        assert_eq!(underruns(&rig), 0, "{what}");
        assert!(
            played[0]
                .iter()
                .zip(&played[1])
                .all(|(latent, direct)| latent.to_bits() == direct.to_bits()),
            "{what}: the two tracks stay in line"
        );
        let stop = clock + 2 * i64::from(LATENCY) + FADE as i64;
        let old = stopped(&track, 0, stop, second_at - first_at);
        assert_plays(
            &played[1..],
            first_at,
            &old,
            &format!("{what}: the old play"),
        );
        let new = reference(&track, second_from, played[1].len() - second_at);
        assert_plays(
            &played[1..],
            second_at,
            &new,
            &format!("{what}: the new play"),
        );
    }
}

/// A disk reader made again on the same engine, as a session whose clips change must be until
/// Stage 2: its generations follow the engine's, so its first cue plays.
#[test]
fn a_reader_made_again_on_the_same_engine_plays_its_first_cue() {
    let dir = TempDir::new("rebuilt");
    let track = long_track(&dir);
    let tracks = vec![track.clone()];
    let mut rig = DiskRig::new(&tracks, Reader::Thread, 256, 8 * BLOCK_FRAMES as u64);
    for from in [0, 10_000, 20_000] {
        rig.play_from(from);
        rig.play(&[256; 8]);
        rig.rig.controller.stop().unwrap();
        rig.play(&[256; 8]);
    }
    let mut config = DiskConfig::new(rate());
    config.read_ahead = 8 * BLOCK_FRAMES as u64;
    let (reader, players) =
        DiskReader::start(config, &audio_tracks(&tracks), rig.rig.controller.stats()).unwrap();
    for old in std::mem::take(&mut rig.players) {
        rig.rig.controller.remove_node(old).unwrap();
    }
    for player in players {
        let id = rig.rig.add(player);
        rig.rig
            .controller
            .connect(Source::Node(id, 0), Destination::Playback(0));
        rig.players.push(id);
    }
    rig.rig.controller.commit().unwrap();
    rig.reader = reader;
    // The swap that brings the new players crossfades: let it end before the play.
    rig.play(&[256; 8]);
    let from = 30_000;
    let cue = rig.reader.locate(SamplePosition(from));
    rig.reader
        .wait_primed(cue, Duration::from_secs(10))
        .unwrap();
    assert!(
        rig.rig.controller.play(cue).is_ok(),
        "the new reader's first cue plays"
    );
    let mut played = vec![Vec::new()];
    let before = underruns(&rig);
    play_into(&mut rig, &mut played, &[256; 40]);
    // Every period so far is 256 frames: 56 before these.
    let at = rig.audible_at() - 56 * 256;
    assert_plays(
        &played,
        at,
        &[reference(&track, from, 20 * 256)[0].clone()],
        "the new reader",
    );
    assert_eq!(underruns(&rig), before);
}

#[test]
fn a_file_cut_short_after_it_was_opened_reads_as_silence_counted() {
    let dir = TempDir::new("cut-short");
    let path = write_wav(&dir, "cut.wav", SampleFormat::Int16, 1, 20_000, 16);
    let tracks = vec![TrackSpec {
        channels: 1,
        clips: vec![clip(&path, 0, 0, 20_000, 1.0)],
    }];
    let mut rig = DiskRig::new(&tracks, Reader::Offline, 1_024, SHORT_READ_AHEAD);
    // The reader has opened the file and read its header; then another program cuts it short.
    let data_offset = read_info(&mut std::fs::File::open(&path).unwrap())
        .unwrap()
        .data_offset;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(data_offset + 8_000 * 2)
        .unwrap();
    rig.play_from(0);
    let played = rig.play(&periods_of(1_024, 30_000));
    let at = rig.audible_at();
    assert_plays(
        &played,
        at,
        &references(&tracks, 0, 30_000),
        "the file cut short",
    );
    assert!(
        played[0][at..at + 8_000]
            .iter()
            .any(|&sample| sample != 0.0)
    );
    assert!(played[0][at + 8_000..].iter().all(|&sample| sample == 0.0));
    assert_eq!(rig.reader.stats().unread_frames(), 12_000);
    assert_eq!(rig.reader.stats().read_errors(), 0);
    assert_eq!(underruns(&rig), 0);
}

/// A reader without a thread cannot prime a cue whose first frames go to slots that a rolling play
/// still reads: `wait_primed` says so. Played anyway, the cue starts once the render has filled it,
/// exactly at its sample.
#[test]
fn wait_primed_says_when_a_rolling_play_holds_the_slots_a_cue_needs() {
    let dir = TempDir::new("not-primed");
    let track = long_track(&dir);
    let tracks = vec![track.clone()];
    let mut rig = DiskRig::new(&tracks, Reader::Offline, 256, 8 * BLOCK_FRAMES as u64);
    rig.play_from(10_000);
    let mut played = vec![Vec::new()];
    play_into(&mut rig, &mut played, &[256; 20]);
    let clock = 10_000 + 20 * 256;
    let cue = rig.reader.locate(SamplePosition(clock + 100));
    assert_eq!(
        rig.reader.wait_primed(cue, Duration::from_secs(1)),
        Err(PrimeError::NotPrimed)
    );
    rig.rig.controller.play(cue).unwrap();
    play_into(&mut rig, &mut played, &[256; 40]);
    let at = rig.audible_at();
    let new = reference(&track, clock + 100, played[0].len() - at);
    assert_plays(&played, at, &new, "the cue, once filled");
    assert_eq!(underruns(&rig), 0);
}
