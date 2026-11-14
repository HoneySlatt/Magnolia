//! What the recorder's tests share: an engine with a recorder in a folder of its own, nodes and
//! signals whose every sample is known, and the takes read back.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use magnolia_engine::engine::{Destination, NodeId, Source};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::TestLatency;
use magnolia_engine::record::{RecordConfig, RecordTrack, Recorder, TakeSummary, Tap};
use magnolia_engine::stats::DeviceLatency;
use magnolia_engine::wav::{WavInfo, read_info};

use crate::disk_support::{TempDir, read_back};
use crate::support::{Constant, Rig, rate};

/// The latency of a latent path elsewhere in the graph, by which every play pre-rolls.
pub const PRE_ROLL: u32 = 300;

/// The value the timeline plays at sample `sample`: exact in `f32`, never zero, and different for
/// every sample within 2^22 of one another, so that a frame out of place shows.
pub fn timeline_value(sample: i64) -> f32 {
    (sample.rem_euclid(1 << 22) + 1) as f32 * (1.0 / (1 << 23) as f32)
}

/// The value a capture channel holds at an engine sample: exact, never zero, different for each
/// channel and each sample within 2^20.
pub fn signal(channel: usize, sample: i64) -> f32 {
    (sample.rem_euclid(1 << 20) + channel as i64 * (1 << 20) + 1) as f32 * (1.0 / (1 << 24) as f32)
}

/// One output that plays [`timeline_value`] at each timeline sample of the audible region it
/// reaches while the transport rolls, and silence elsewhere.
pub struct TimelinePlayer;

impl Node for TimelinePlayer {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let transport = context.transport();
        let start = transport.audible.start.0;
        let stop = transport.audible.stop.map_or(i64::MAX, |stop| stop.0);
        let output = context.output_mut(0);
        for (offset, sample) in output.iter_mut().enumerate() {
            let at = transport.position.0 + offset as i64;
            *sample = if transport.rolling && (start..stop).contains(&at) {
                timeline_value(at)
            } else {
                0.0
            };
        }
        ProcessStatus::Continue
    }
}

/// An engine with a recorder in a folder of its own, played period by period by the device-less
/// backend, which remembers what playback channel 0 played at each engine sample.
pub struct RecordRig {
    pub rig: Rig,
    pub recorder: Option<Recorder>,
    pub dir: TempDir,
    /// The engine sample of the next period's first frame.
    pub position: i64,
    /// What playback channel 0 played, from engine sample 0.
    pub played: Vec<f32>,
    capture_channels: usize,
}

impl RecordRig {
    /// An engine of `capture_channels` capture channels and one playback channel, with a recorder
    /// whose configuration `configure` may change.
    pub fn new(
        name: &str,
        capture_channels: usize,
        configure: impl FnOnce(&mut RecordConfig),
    ) -> Self {
        let dir = TempDir::new(name);
        let rig = Rig::new(capture_channels, 1);
        let mut config = RecordConfig::new(dir.path(), rate());
        configure(&mut config);
        let recorder = Recorder::start(config, rig.controller.stats()).unwrap();
        Self {
            rig,
            recorder: Some(recorder),
            dir,
            position: 0,
            played: Vec::new(),
            capture_channels,
        }
    }

    pub fn recorder(&self) -> &Recorder {
        self.recorder.as_ref().unwrap()
    }

    /// Makes the tap of track `number`, named `name`, on the capture channels given.
    pub fn tap(&mut self, number: u32, name: &str, channels: &[usize]) -> Tap {
        self.recorder
            .as_mut()
            .unwrap()
            .tap(RecordTrack {
                number,
                name: name.to_owned(),
                capture_channels: channels.to_vec(),
            })
            .unwrap()
    }

    /// Arms track `number`, named `name`, on the capture channels given; the next commit sends it.
    pub fn arm(&mut self, number: u32, name: &str, channels: &[usize]) {
        let tap = self.tap(number, name, channels);
        self.rig.controller.arm(tap).unwrap();
    }

    /// Adds a [`TimelinePlayer`] on playback channel 0, and a latent path that pre-rolls every play
    /// by [`PRE_ROLL`] samples.
    pub fn add_timeline_and_pre_roll(&mut self) -> NodeId {
        let player = self.rig.add(TimelinePlayer);
        self.rig
            .controller
            .connect(Source::Node(player, 0), Destination::Playback(0));
        let silent = self.rig.add(Constant::new(0.0));
        let latent = self.rig.add(TestLatency::new(1, PRE_ROLL));
        self.rig
            .controller
            .connect(Source::Node(silent, 0), Destination::Node(latent, 0));
        self.rig
            .controller
            .connect(Source::Node(latent, 0), Destination::Playback(0));
        player
    }

    /// Publishes the device's latencies, as a backend does.
    pub fn publish_latency(&self, capture: u32, playback: u32) {
        self.rig
            .controller
            .stats()
            .publish_device_latency(DeviceLatency { capture, playback });
    }

    /// Plays one period of `frames`, each capture channel holding `capture(channel, engine
    /// sample)`, then polls the controller.
    pub fn period(&mut self, frames: usize, capture: &dyn Fn(usize, i64) -> f32) {
        let buffers: Vec<Vec<f32>> = (0..self.capture_channels)
            .map(|channel| {
                (0..frames)
                    .map(|offset| capture(channel, self.position + offset as i64))
                    .collect()
            })
            .collect();
        let buffers: Vec<&[f32]> = buffers.iter().map(Vec::as_slice).collect();
        let played = self.rig.backend.play_period(frames, &buffers)[0].clone();
        self.played.extend(played);
        self.position += frames as i64;
        self.rig.controller.poll();
    }

    /// Waits until the recorder has done all it was asked.
    pub fn wait_idle(&self) {
        assert!(
            self.recorder().wait_idle(Duration::from_secs(20)),
            "the recorder did not become idle"
        );
    }

    /// Waits until the recorder is idle, then ends it, and returns the takes it reported.
    pub fn finish(&mut self) -> Vec<TakeSummary> {
        self.wait_idle();
        let takes = self.recorder().takes();
        drop(self.recorder.take());
        takes
    }
}

/// A take's file read back: what its header says, and its samples, interleaved.
pub fn read_take(path: &Path) -> (WavInfo, Vec<f32>) {
    let info = read_info(&mut fs::File::open(path).unwrap()).unwrap();
    (info, read_back(path))
}

/// The files of a folder, by name, sorted, the lock file left out.
pub fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// The path of the take of a summary, which must have a file.
pub fn take_path(take: &TakeSummary) -> PathBuf {
    take.path.clone().expect("the take has a file")
}
