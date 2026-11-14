//! The null through JACK: the small test session, played by a `SessionPlayer` through the backend
//! on the test's own server, at 64 and at 256 frames, and recorded by a plain JACK client, equals
//! its offline render to the bit at every sample from the play's first audible one, silent before
//! it, without a disk underrun.
//!
//! The recorder keeps, with each period, the engine sample at which Magnolia's processor started
//! the period it copied: JACK runs the recorder after Magnolia in each cycle, since it reads
//! Magnolia's outputs. So every period recorded is compared with the render where the engine says
//! it lies, not where the signal seems to start. A cycle the server skips, for Magnolia or for the
//! recorder, leaves a hole or a repeat in the recording, never a period out of place. So takes are
//! played until each sample of the render has been heard in at least one, five takes at most.
//!
//! The play starts in real time; once the disk reader holds the whole session in memory, the
//! server freewheels (`Freewheel`): it runs the rest of the take as fast as the clients go,
//! without skipping a cycle, and the processor cannot outrun the reader. A server without real
//! time skips cycles only in the part played in real time, before the freewheel.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jack::{
    AsyncClient, AudioIn, Client, ClientOptions, Control, Port, ProcessHandler, ProcessScope,
};
use magnolia_engine::disk::DiskConfig;
use magnolia_engine::stats::EngineStats;
use magnolia_engine::time::SamplePosition;
use magnolia_engine::wav::{decode, read_info};
use magnolia_jack::{JackBackend, JackOptions};
use magnolia_session::generate::{Kind, generate};
use magnolia_session::{Session, SessionPlayer, render};

use crate::common::{Server, client_name, step};
use crate::support::{Freewheel, Gaps, one_at_a_time, wait_for};

/// What the recorder may record before the play's first audible sample: the disk reader's
/// priming and the pre-roll, far below 10 seconds at 48 kHz.
const LEAD: usize = 480_000;

/// How many takes may be played, at most, until every sample of the render has been heard.
const TAKES: usize = 5;

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
    // The reader then fills, before the freewheel, every frame the play reads.
    assert!(
        frames as u64 <= DiskConfig::new(session.sample_rate()).read_ahead,
        "the small session outlasts the disk reader's read-ahead"
    );
    assert!(
        rendered
            .iter()
            .all(|channel| channel.iter().any(|&sample| sample != 0.0)),
        "every channel of the render sounds"
    );

    let mut heard = vec![false; frames];
    let mut skipped = Vec::new();
    for take in 1..=TAKES {
        let played = play_through_jack(&session, period, frames);
        assert_eq!(played.disk_underruns, 0, "at {period} frames, take {take}");
        assert_eq!(played.channels.len(), rendered.len(), "the playback ports");
        compare(&played, &rendered, period, &mut heard);
        skipped.push(played.skipped_cycles);
        let unheard = heard.iter().filter(|&&heard| !heard).count();
        let set_aside = played.period_starts.iter().filter(|start| start.is_none());
        eprintln!(
            "the null at {period} frames, take {take}: {} cycles skipped, {} of {} periods set \
             aside, {unheard} samples not yet heard; torn down in {:?}",
            played.skipped_cycles,
            set_aside.count(),
            played.period_starts.len(),
            played.teardown
        );
        if unheard == 0 {
            return;
        }
    }
    let unheard = heard.iter().filter(|&&heard| !heard).count();
    panic!(
        "after {TAKES} takes at {period} frames, {unheard} samples of the render were never \
         heard; the server skipped cycles in each: {skipped:?}"
    );
}

/// Compares every period a take kept with the render, at the timeline sample its engine sample
/// gives: silent before the play's first audible sample, the render to the bit from there. Marks
/// the render's samples heard.
fn compare(played: &Played, rendered: &[Vec<f32>], period: u32, heard: &mut [bool]) {
    let period = period as usize;
    for (index, start) in played.period_starts.iter().enumerate() {
        // A period Magnolia was playing while the recorder copied it was not kept.
        let Some(start) = *start else {
            continue;
        };
        for frame in 0..period {
            let timeline = start + frame as i64 - played.audible_at;
            for (channel, recorded) in played.channels.iter().enumerate() {
                let sample = recorded[index * period + frame];
                match usize::try_from(timeline) {
                    Err(_) => assert!(
                        sample == 0.0,
                        "channel {channel} sounded {} samples before the play's first audible \
                         one, at {period} frames",
                        -timeline
                    ),
                    Ok(at) if at < heard.len() => assert!(
                        sample.to_bits() == rendered[channel][at].to_bits(),
                        "at {period} frames, channel {channel}, sample {at} of the play: {sample} \
                         where {} was rendered",
                        rendered[channel][at]
                    ),
                    Ok(_) => {}
                }
            }
            if let Ok(at) = usize::try_from(timeline)
                && at < heard.len()
            {
                heard[at] = true;
            }
        }
    }
}

/// What a take recorded, and what the engine and the server said of it.
struct Played {
    /// Each playback channel, period after period as recorded.
    channels: Vec<Vec<f32>>,
    /// The engine sample at which each recorded period starts; `None` for a period that Magnolia
    /// was playing while the recorder copied it.
    period_starts: Vec<Option<i64>>,
    /// The engine sample at which the output plays the play's first timeline sample.
    audible_at: i64,
    disk_underruns: u64,
    /// Cycles the server skipped while it recorded, Magnolia's or the recorder's: gaps in their
    /// frame times.
    skipped_cycles: u64,
    /// How long closing the recorder and the backend took.
    teardown: Duration,
}

/// Plays the session from sample 0 through the backend on a server of its own, at `period`
/// frames, and records its playback until `frames` samples from its first audible one.
fn play_through_jack(session: &Session, period: u32, frames: usize) -> Played {
    let server = Server::start(period);
    let (mut player, processor) = SessionPlayer::new(session).unwrap();
    let channels = processor.playback_channels();
    step("opening Magnolia's client");
    let backend = JackBackend::open(JackOptions {
        client_name: client_name("magnolia"),
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
    // Once idle after the play's start, the reader holds every frame up to the last clip's end.
    assert!(
        player.reader().wait_idle(Duration::from_secs(10)),
        "the disk reader filled the play's frames"
    );
    let freewheel = Freewheel::start(&server);
    wait_for("the recording", Duration::from_secs(30), || {
        let through = recorder.last_period_end();
        assert!(
            through.is_some_and(|through| through >= end) || !recorder.is_full(),
            "the recording was full before the play's end"
        );
        through.is_some_and(|through| through >= end)
    });
    let disk_underruns = player.report().disk_underruns;
    let skipped_cycles = stats.frame_time_gaps() - gaps + recorder.gaps();
    // Before any client closes, which would stall a freewheeling server.
    freewheel.stop();
    let tearing_down = Instant::now();
    let (channels, period_starts) = recorder.finish();
    player.stop().unwrap();
    step("closing Magnolia's client");
    backend.close().expect("the backend closes");
    let teardown = tearing_down.elapsed();
    Played {
        channels,
        period_starts,
        audible_at,
        disk_underruns,
        skipped_cycles,
        teardown,
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
/// each with the engine sample at which Magnolia's processor started the period it copied.
struct Recorder {
    client: AsyncClient<(), Recording>,
    armed: Arc<AtomicBool>,
    /// The engine sample after the last period recorded, kept or not; `i64::MIN` before it.
    last_period_end: Arc<AtomicI64>,
    /// The periods recorded, kept or not.
    periods_recorded: Arc<AtomicUsize>,
    /// The most periods it records.
    room: usize,
    gaps: Arc<AtomicU64>,
    inputs: Vec<String>,
}

struct Recording {
    inputs: Vec<Port<AudioIn>>,
    engine: Arc<EngineStats>,
    armed: Arc<AtomicBool>,
    /// The frames of every period: the server's own, which never changes on the dummy driver.
    period: usize,
    /// Each input's samples; their capacity is the length to record, so they never grow on the
    /// audio thread.
    channels: Vec<Vec<f32>>,
    /// Where each period recorded starts, as `Played::period_starts`.
    period_starts: Vec<Option<i64>>,
    last_period_end: Arc<AtomicI64>,
    periods_recorded: Arc<AtomicUsize>,
    gaps: Gaps,
}

impl ProcessHandler for Recording {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        self.gaps.note(scope);
        let frames = scope.n_frames() as usize;
        let full = self.period_starts.len() == self.period_starts.capacity();
        if !self.armed.load(Ordering::Acquire) || full || frames != self.period {
            return Control::Continue;
        }
        // Magnolia's processor publishes where a period starts as it begins it, and counts the
        // period as it ends it, every period of the server's length from engine sample 0. Its
        // outputs hold the period that starts at `start` when it had ended that period and
        // began no other while they were copied. That the second look sees a period begun
        // during the copy rests on x86's order of stores and of loads, Magnolia's only target.
        let before = (self.engine.periods(), self.engine.block_start().0);
        for (channel, input) in self.channels.iter_mut().zip(&self.inputs) {
            channel.extend_from_slice(input.as_slice(scope));
        }
        let after = (self.engine.periods(), self.engine.block_start().0);
        let (periods, start) = before;
        let ended = periods > 0 && start == (periods as i64 - 1) * self.period as i64;
        self.period_starts
            .push((ended && before == after).then_some(start));
        self.last_period_end
            .store(start + self.period as i64, Ordering::Release);
        self.periods_recorded
            .store(self.period_starts.len(), Ordering::Release);
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
        step("opening the recorder");
        let (client, _) = Client::new_with_server_name(
            &client_name("recorder"),
            &server.name,
            ClientOptions::NO_START_SERVER,
        )
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
        let last_period_end = Arc::new(AtomicI64::new(i64::MIN));
        let periods_recorded = Arc::new(AtomicUsize::new(0));
        let room = capacity / period;
        let gaps = Arc::new(AtomicU64::new(0));
        let recording = Recording {
            inputs: ports,
            engine,
            armed: Arc::clone(&armed),
            period,
            channels: (0..channels)
                .map(|_| Vec::with_capacity(capacity))
                .collect(),
            period_starts: Vec::with_capacity(room),
            last_period_end: Arc::clone(&last_period_end),
            periods_recorded: Arc::clone(&periods_recorded),
            gaps: Gaps::new(Arc::clone(&gaps)),
        };
        let client = client.activate_async((), recording).unwrap();
        Self {
            client,
            armed,
            last_period_end,
            periods_recorded,
            room,
            gaps,
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

    /// The engine sample after the last period recorded, kept or not.
    fn last_period_end(&self) -> Option<i64> {
        let end = self.last_period_end.load(Ordering::Acquire);
        (end != i64::MIN).then_some(end)
    }

    /// Whether it has no room left for another period.
    fn is_full(&self) -> bool {
        self.periods_recorded.load(Ordering::Acquire) >= self.room
    }

    /// Cycles the server skipped for it.
    fn gaps(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }

    /// Deactivates it and returns each channel and where each period recorded starts.
    fn finish(self) -> (Vec<Vec<f32>>, Vec<Option<i64>>) {
        step("closing the recorder");
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
