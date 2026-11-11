//! What the disk reader's tests share: a folder of their own for WAV files of seeded content, the
//! files read back whole, a reference mixed directly from them, and an engine whose audio tracks
//! each play on playback channels of their own.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use magnolia_engine::disk::{AudioClip, AudioFile, AudioTrack, DiskConfig, DiskReader};
use magnolia_engine::engine::{Cue, Destination, EngineConfig, NodeId, Source};
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{SampleFormat, WavWriter, decode, read_info};

use crate::support::{FADE, RATE, Rig, fade_in, rate};
use crate::timeline_support::Random;

/// A folder of its own under the system's temporary folder, removed with everything in it when
/// dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "magnolia-test-{name}-{}-{number}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Writes a WAV file of `frames` frames of `channels` channels at the tests' rate, its samples
/// drawn from `seed` between -0.9 and 0.9, and returns its path.
pub fn write_wav(
    dir: &TempDir,
    name: &str,
    format: SampleFormat,
    channels: u16,
    frames: usize,
    seed: u64,
) -> PathBuf {
    let mut random = Random(0x9E37_79B9_7F4A_7C15 ^ seed);
    let samples: Vec<f32> = (0..frames * usize::from(channels))
        .map(|_| ((random.next() >> 11) as f64 / (1_u64 << 53) as f64 * 1.8 - 0.9) as f32)
        .collect();
    write_samples(dir, name, format, channels, &samples)
}

/// Writes a WAV file of the interleaved samples given, and returns its path.
pub fn write_samples(
    dir: &TempDir,
    name: &str,
    format: SampleFormat,
    channels: u16,
    samples: &[f32],
) -> PathBuf {
    let path = dir.path().join(name);
    let file = BufWriter::new(File::create(&path).unwrap());
    let mut writer = WavWriter::new(file, format, channels, RATE).unwrap();
    writer.write_frames(samples).unwrap();
    writer.finish().unwrap();
    path
}

/// A file's samples, interleaved, as `decode` reads them, by the test's own reading of the whole
/// file.
pub fn read_back(path: &Path) -> Vec<f32> {
    let bytes = fs::read(path).unwrap();
    let info = read_info(&mut std::io::Cursor::new(&bytes)).unwrap();
    let data = &bytes[info.data_offset as usize..];
    let mut samples = vec![0.0; info.frames as usize * usize::from(info.channels)];
    decode(info.sample_format, info.channels, data, &mut samples);
    samples
}

/// A clip as a test describes it: its file, by its path, and where it lies.
#[derive(Clone, Debug)]
pub struct ClipSpec {
    pub path: PathBuf,
    pub start: i64,
    pub file_offset: u64,
    pub length: u64,
    pub gain: f32,
}

pub fn clip(path: &Path, start: i64, file_offset: u64, length: u64, gain: f32) -> ClipSpec {
    ClipSpec {
        path: path.to_path_buf(),
        start,
        file_offset,
        length,
        gain,
    }
}

/// A track as a test describes it.
#[derive(Clone, Debug)]
pub struct TrackSpec {
    pub channels: u16,
    pub clips: Vec<ClipSpec>,
}

/// The tracks the reader takes, each file opened once.
pub fn audio_tracks(specs: &[TrackSpec]) -> Vec<AudioTrack> {
    specs
        .iter()
        .map(|spec| AudioTrack {
            channels: spec.channels,
            clips: spec
                .clips
                .iter()
                .map(|clip| AudioClip {
                    file: AudioFile::open(&clip.path).unwrap(),
                    start: SamplePosition(clip.start),
                    file_offset: clip.file_offset,
                    length: clip.length,
                    gain: clip.gain,
                })
                .collect(),
        })
        .collect()
}

/// What a track plays from timeline sample `from` for `frames` frames, each channel apart, mixed
/// directly from its files: zeros, then each clip's frames times its gain, those past its file's
/// end silent; a clip that starts before `from` times the fade-in over the 5 ms from `from`.
pub fn reference(track: &TrackSpec, from: i64, frames: usize) -> Vec<Vec<f32>> {
    let channels = usize::from(track.channels);
    let mut played = vec![vec![0.0_f32; frames]; channels];
    for clip in &track.clips {
        let samples = read_back(&clip.path);
        let file_frames = samples.len() / channels;
        for (index, sample) in (from..from + frames as i64).enumerate() {
            if sample < clip.start || sample >= clip.start + clip.length as i64 {
                continue;
            }
            let file_frame = clip.file_offset as usize + (sample - clip.start) as usize;
            if file_frame >= file_frames {
                continue;
            }
            let fade = (clip.start < from && sample - from < FADE as i64)
                .then(|| fade_in((sample - from) as usize, FADE) as f32);
            for (channel, played) in played.iter_mut().enumerate() {
                let value = samples[file_frame * channels + channel] * clip.gain;
                played[index] += match fade {
                    Some(gain) => value * gain,
                    None => value,
                };
            }
        }
    }
    played
}

/// The tracks' reference, channel after channel, in the order the rig's playback channels play
/// them.
pub fn references(tracks: &[TrackSpec], from: i64, frames: usize) -> Vec<Vec<f32>> {
    tracks
        .iter()
        .flat_map(|track| reference(track, from, frames))
        .collect()
}

/// An engine playing audio tracks, each player's channels on playback channels of their own, in
/// the tracks' order.
pub struct DiskRig {
    pub rig: Rig,
    pub reader: DiskReader,
    pub players: Vec<NodeId>,
}

/// How a rig's disk reader runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// On a thread of its own.
    Thread,
    /// On the test's thread, filled before each period.
    Offline,
}

impl DiskRig {
    /// A rig of `tracks`, its engine's largest block `largest_block`, reading `read_ahead` frames
    /// ahead. `route` connects each player, given its first playback channel; `None` connects each
    /// channel straight.
    pub fn new(
        tracks: &[TrackSpec],
        reader: Reader,
        largest_block: usize,
        read_ahead: u64,
    ) -> Self {
        Self::routed(tracks, reader, largest_block, read_ahead, |_, _, _| None)
    }

    /// As `new`, with `route(rig, track, player)` adding the nodes a track's player goes through:
    /// it returns the node that takes the player's place towards the playback channels, or `None`
    /// for the player itself.
    pub fn routed(
        tracks: &[TrackSpec],
        reader: Reader,
        largest_block: usize,
        read_ahead: u64,
        mut route: impl FnMut(&mut Rig, usize, NodeId) -> Option<NodeId>,
    ) -> Self {
        let playback: usize = tracks.iter().map(|track| usize::from(track.channels)).sum();
        let mut rig = Rig::with_config(EngineConfig::new(rate(), largest_block, 0, playback));
        let mut config = DiskConfig::new(rate());
        config.read_ahead = read_ahead;
        let audio = audio_tracks(tracks);
        let made = match reader {
            Reader::Thread => DiskReader::start(config, &audio, rig.controller.stats()),
            Reader::Offline => DiskReader::offline(config, &audio, rig.controller.stats()),
        };
        let (reader, players) = made.unwrap();
        let mut ids = Vec::new();
        let mut channel = 0;
        for (index, (player, track)) in players.into_iter().zip(tracks).enumerate() {
            let id = rig.add(player);
            let out = route(&mut rig, index, id).unwrap_or(id);
            for track_channel in 0..usize::from(track.channels) {
                rig.controller.connect(
                    Source::Node(out, track_channel),
                    Destination::Playback(channel),
                );
                channel += 1;
            }
            ids.push(id);
        }
        rig.controller.commit().unwrap();
        Self {
            rig,
            reader,
            players: ids,
        }
    }

    /// Locates the reader at `from`, waits until it is primed, and plays the cue.
    pub fn play_from(&mut self, from: i64) -> Cue {
        let cue = self.reader.locate(SamplePosition(from));
        self.reader
            .wait_primed(cue, Duration::from_secs(10))
            .unwrap();
        self.rig.controller.play(cue).unwrap();
        cue
    }

    /// Plays periods of the lengths given, on every playback channel: before each, an offline
    /// reader fills, and a reader with a thread is waited for until it has done all it was asked.
    pub fn play(&mut self, periods: &[usize]) -> Vec<Vec<f32>> {
        let mut played: Vec<Vec<f32>> = Vec::new();
        for &frames in periods {
            if !self.reader.fill() {
                assert!(
                    self.reader.wait_idle(Duration::from_secs(10)),
                    "the reader did not finish"
                );
            }
            let period = self.rig.backend.play_period(frames, &[]);
            played.resize(period.len(), Vec::new());
            for (all, channel) in played.iter_mut().zip(period) {
                all.extend_from_slice(channel);
            }
            self.rig.controller.poll();
        }
        played
    }

    /// Plays periods of the lengths given, paced by the clock as a device would pace them, without
    /// waiting for the reader; returns what every playback channel played.
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's thread paces its periods as a device would; it stands for a backend"
    )]
    pub fn play_in_real_time(&mut self, periods: &[usize]) -> Vec<Vec<f32>> {
        let start = Instant::now();
        let mut played: Vec<Vec<f32>> = Vec::new();
        let mut frames_so_far = 0;
        for &frames in periods {
            let due = start + Duration::from_secs_f64(frames_so_far as f64 / f64::from(RATE));
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            let period = self.rig.backend.play_period(frames, &[]);
            played.resize(period.len(), Vec::new());
            for (all, channel) in played.iter_mut().zip(period) {
                all.extend_from_slice(channel);
            }
            self.rig.controller.poll();
            frames_so_far += frames;
        }
        played
    }

    /// Where the last play's first sample reached the output, on the engine's clock.
    pub fn audible_at(&self) -> usize {
        self.rig
            .controller
            .stats()
            .play_start()
            .unwrap()
            .audible_at
            .0 as usize
    }
}

/// Checks that `played` holds `expected` bit for bit from `at` on, channel by channel, and says
/// where it does not.
pub fn assert_plays(played: &[Vec<f32>], at: usize, expected: &[Vec<f32>], what: &str) {
    assert_eq!(played.len(), expected.len(), "{what}: channels");
    for (channel, (played, expected)) in played.iter().zip(expected).enumerate() {
        let played = &played[at..at + expected.len()];
        if let Some(index) = played
            .iter()
            .zip(expected)
            .position(|(played, expected)| played.to_bits() != expected.to_bits())
        {
            panic!(
                "{what}: channel {channel}, frame {index} from the start played {}, expected {}",
                played[index], expected[index]
            );
        }
    }
}

/// How many frames the rig's players lacked.
pub fn underruns(rig: &DiskRig) -> u64 {
    rig.rig.controller.stats().disk_underruns()
}
