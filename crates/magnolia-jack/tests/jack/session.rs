//! The null through JACK: the small test session, played by a `SessionPlayer` through the backend
//! on the test's own server, at 64 and at 256 frames, and recorded by a plain JACK client, equals
//! its offline render to the bit from the play's first audible sample, without a disk underrun.
//!
//! The recorder reads, in each period, the engine sample at which Magnolia's processor started
//! that period: JACK runs the recorder after Magnolia in each cycle, since it reads Magnolia's
//! outputs. So the recording is aligned to the engine's samples, and the play's first audible
//! sample is where the engine says it is, not where the signal seems to start.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use jack::{
    AsyncClient, AudioIn, Client, ClientOptions, Control, Port, ProcessHandler, ProcessScope,
};
use magnolia_engine::stats::EngineStats;
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{decode, read_info};
use magnolia_jack::{JackBackend, JackOptions};
use magnolia_session::generate::{Kind, generate};
use magnolia_session::{Session, SessionPlayer, render};

use crate::common::{Server, Settings};
use crate::support::{Gaps, Take, first_unbroken_take, one_at_a_time, wait_for};

/// What the recorder may record before the play's first audible sample: the disk reader's
/// priming and the pre-roll, far below 10 seconds at 48 kHz.
const LEAD: usize = 480_000;

#[test]
fn the_small_session_plays_through_jack_at_64_frames_to_the_bit_what_it_renders() {
    plays_through_jack_what_it_renders(64);
}

#[test]
fn the_small_session_plays_through_jack_at_256_frames_to_the_bit_what_it_renders() {
    plays_through_jack_what_it_renders(256);
}

fn plays_through_jack_what_it_renders(period: u32) {
    let _turn = one_at_a_time();
    let dir = TempDir::new("null");
    generate(Kind::Small, dir.path()).unwrap();
    let session = Session::open(dir.path().join("session.json")).unwrap();
    let rendered = render_channels(&session);
    let frames = rendered[0].len();
    assert!(
        rendered
            .iter()
            .all(|channel| channel.iter().any(|&sample| sample != 0.0)),
        "every channel of the render sounds"
    );

    let played = first_unbroken_take(|| play_through_jack(&session, period, frames));

    assert_eq!(played.disk_underruns, 0, "at {period} frames");
    assert_eq!(played.channels.len(), rendered.len(), "the playback ports");
    // Each period recorded follows on from the one before, in the engine's samples: the recorded
    // sample `i` is the engine's sample `first + i`.
    for (index, pair) in played.period_starts.windows(2).enumerate() {
        assert_eq!(
            pair[1] - pair[0],
            i64::from(period),
            "the recorded period {index} is followed by engine sample {}, not the next",
            pair[1]
        );
    }
    let first = played.period_starts[0];
    let audible = usize::try_from(played.audible_at - first)
        .expect("the recording started before the play's first audible sample");
    for (channel, (recorded, rendered)) in played.channels.iter().zip(&rendered).enumerate() {
        assert!(
            recorded[..audible].iter().all(|&sample| sample == 0.0),
            "channel {channel} sounded before the play's first audible sample"
        );
        let heard = &recorded[audible..audible + frames];
        if let Some(frame) = heard
            .iter()
            .zip(rendered)
            .position(|(heard, rendered)| heard.to_bits() != rendered.to_bits())
        {
            panic!(
                "at {period} frames, channel {channel}, frame {frame} of the play: {} where {} was \
                 rendered",
                heard[frame], rendered[frame]
            );
        }
    }
}

/// What a take recorded, and what the engine said of it.
struct Played {
    /// Each playback channel, from the first period recorded.
    channels: Vec<Vec<f32>>,
    /// The engine sample at which each recorded period starts.
    period_starts: Vec<i64>,
    /// The engine sample at which the output plays the play's first timeline sample.
    audible_at: i64,
    disk_underruns: u64,
}

/// Plays the session from sample 0 through the backend on a server of its own, at `period`
/// frames, and records its playback until `frames` samples from its first audible one.
fn play_through_jack(session: &Session, period: u32, frames: usize) -> Take<Played> {
    // Synchronous: without real time, the default mode skips a cycle every few seconds at 64
    // frames, and a take lasts nine.
    let server = Server::start_with(Settings {
        rate: 48_000,
        period,
        synchronous: true,
    });
    let (mut player, processor) = SessionPlayer::new(session).unwrap();
    let channels = processor.playback_channels();
    let backend = JackBackend::open(JackOptions {
        client_name: "magnolia".to_owned(),
        server_name: Some(server.name.clone()),
        processor,
    })
    .expect("the backend opens on the test's server");
    let stats = Arc::clone(player.stats());
    let recorder = Recorder::open(&server, Arc::clone(&stats), channels, frames + LEAD, period);
    for (output, input) in backend.playback_ports().iter().zip(&recorder.inputs) {
        recorder.connect(output, input);
    }
    // Armed once the connections hold, so that the server already runs Magnolia first.
    let target = stats.periods() + 8;
    wait_for("periods", Duration::from_secs(10), || {
        stats.periods() >= target
    });
    let gaps = stats.frame_time_gaps();
    recorder.arm();

    let sequence = player
        .start(SamplePosition(0), Duration::from_secs(10))
        .expect("the play starts");
    wait_for("the play's start", Duration::from_secs(10), || {
        stats
            .play_start()
            .is_some_and(|start| start.sequence == sequence)
    });
    let audible_at = stats.play_start().unwrap().audible_at.0;
    let end = audible_at + frames as i64;
    wait_for("the recording", Duration::from_secs(30), || {
        let through = recorder.recorded_through();
        assert!(
            through.is_some_and(|through| through >= end) || !recorder.is_full(),
            "the recording was full before the play's end: it started {} samples before the \
             play's first audible sample",
            recorder
                .first_period_start()
                .map_or(0, |first| audible_at - first)
        );
        through.is_some_and(|through| through >= end)
    });
    let disk_underruns = player.report().disk_underruns;
    let skipped_cycles = stats.frame_time_gaps() - gaps + recorder.gaps();
    let (channels, period_starts) = recorder.finish();
    player.stop().unwrap();
    backend.close().expect("the backend closes");
    Take {
        result: Played {
            channels,
            period_starts,
            audible_at,
            disk_underruns,
        },
        skipped_cycles,
    }
}

/// Renders the session in blocks of 4,096 frames, and returns each channel.
fn render_channels(session: &Session) -> Vec<Vec<f32>> {
    let mut bytes = Cursor::new(Vec::new());
    let report = render(session, 4_096, &mut bytes).unwrap();
    assert_eq!(report.underruns, 0);
    let bytes = bytes.into_inner();
    let info = read_info(&mut Cursor::new(&bytes)).unwrap();
    let channels = usize::from(info.channels);
    let mut interleaved = vec![0.0; info.frames as usize * channels];
    decode(
        info.sample_format,
        info.channels,
        &bytes[info.data_offset as usize..],
        &mut interleaved,
    );
    (0..channels)
        .map(|channel| {
            interleaved
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect()
        })
        .collect()
}

/// A plain JACK client, apart from Magnolia, that records its inputs in whole periods once armed,
/// with the engine sample at which Magnolia's processor started each period it records.
struct Recorder {
    client: AsyncClient<(), Recording>,
    armed: Arc<AtomicBool>,
    recorded: Arc<AtomicUsize>,
    /// The engine sample of the first period recorded; `i64::MIN` before it.
    first_period_start: Arc<AtomicI64>,
    gaps: Arc<AtomicU64>,
    capacity: usize,
    period: usize,
    inputs: Vec<String>,
}

struct Recording {
    inputs: Vec<Port<AudioIn>>,
    engine: Arc<EngineStats>,
    armed: Arc<AtomicBool>,
    /// Each input's samples; their capacity is the length to record, so they never grow on the
    /// audio thread.
    channels: Vec<Vec<f32>>,
    /// The engine sample at which each period recorded starts.
    period_starts: Vec<i64>,
    recorded: Arc<AtomicUsize>,
    first_period_start: Arc<AtomicI64>,
    gaps: Gaps,
}

impl ProcessHandler for Recording {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        self.gaps.note(scope);
        if !self.armed.load(Ordering::Acquire) {
            return Control::Continue;
        }
        let frames = scope.n_frames() as usize;
        let full = self
            .channels
            .iter()
            .any(|channel| channel.capacity() - channel.len() < frames)
            || self.period_starts.len() == self.period_starts.capacity();
        if full {
            return Control::Continue;
        }
        // Magnolia's processor ran this cycle's period before this client, and published where
        // it started.
        let start = self.engine.block_start().0;
        if self.period_starts.is_empty() {
            self.first_period_start.store(start, Ordering::Relaxed);
        }
        self.period_starts.push(start);
        for (channel, input) in self.channels.iter_mut().zip(&self.inputs) {
            channel.extend_from_slice(input.as_slice(scope));
        }
        let recorded = self.channels.first().map_or(0, Vec::len);
        self.recorded.store(recorded, Ordering::Release);
        Control::Continue
    }
}

impl Recorder {
    /// Opens a recorder of `channels` inputs on `server`, which records up to `capacity` samples
    /// of each in periods of `period` frames, reading where Magnolia's periods start in `engine`.
    fn open(
        server: &Server,
        engine: Arc<EngineStats>,
        channels: usize,
        capacity: usize,
        period: u32,
    ) -> Self {
        let (client, _) =
            Client::new_with_server_name("recorder", &server.name, ClientOptions::NO_START_SERVER)
                .expect("the recorder opens on the test's server");
        let ports: Vec<Port<AudioIn>> = (1..=channels)
            .map(|channel| {
                client
                    .register_port(&format!("in_{channel}"), AudioIn::default())
                    .unwrap()
            })
            .collect();
        let inputs = ports.iter().map(|port| port.name().unwrap()).collect();
        let period = period as usize;
        let armed = Arc::new(AtomicBool::new(false));
        let recorded = Arc::new(AtomicUsize::new(0));
        let first_period_start = Arc::new(AtomicI64::new(i64::MIN));
        let gaps = Arc::new(AtomicU64::new(0));
        let recording = Recording {
            inputs: ports,
            engine,
            armed: Arc::clone(&armed),
            channels: (0..channels)
                .map(|_| Vec::with_capacity(capacity))
                .collect(),
            period_starts: Vec::with_capacity(capacity.div_ceil(period)),
            recorded: Arc::clone(&recorded),
            first_period_start: Arc::clone(&first_period_start),
            gaps: Gaps::new(Arc::clone(&gaps)),
        };
        let client = client.activate_async((), recording).unwrap();
        Self {
            client,
            armed,
            recorded,
            first_period_start,
            gaps,
            capacity,
            period,
            inputs,
        }
    }

    fn connect(&self, source: &str, destination: &str) {
        self.client
            .as_client()
            .connect_ports_by_name(source, destination)
            .unwrap_or_else(|error| panic!("{source} -> {destination}: {error}"));
    }

    /// Records from its next period on.
    fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    /// The engine sample of the first period recorded.
    fn first_period_start(&self) -> Option<i64> {
        let first = self.first_period_start.load(Ordering::Relaxed);
        (first != i64::MIN).then_some(first)
    }

    /// The engine sample after the last one recorded.
    fn recorded_through(&self) -> Option<i64> {
        let recorded = self.recorded.load(Ordering::Acquire);
        if recorded == 0 {
            return None;
        }
        // Stored before the count, which was read with acquire.
        self.first_period_start()
            .map(|first| first + recorded as i64)
    }

    /// Whether it has no room left for another period.
    fn is_full(&self) -> bool {
        self.capacity - self.recorded.load(Ordering::Acquire) < self.period
    }

    /// Cycles the server skipped for it.
    fn gaps(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }

    /// Deactivates it and returns each channel and where each period recorded starts.
    fn finish(self) -> (Vec<Vec<f32>>, Vec<i64>) {
        let (_, _, recording) = self.client.deactivate().unwrap();
        (recording.channels, recording.period_starts)
    }
}

/// A folder of its own under the system's temporary folder, removed with everything in it when
/// dropped.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "magnolia-jack-test-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
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
