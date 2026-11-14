//! `magnolia play`: a test session played through the JACK backend, from a timeline position for
//! a length, stopped, then the engine's figures. `magnolia record` plays through the same pieces.

use std::error::Error;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use magnolia_engine::engine::Processor;
use magnolia_engine::stats::{EngineStats, PlayStart};
use magnolia_engine::time::{SamplePosition, SampleRate, TimeUnits, place};
use magnolia_jack::{
    CloseError, ConnectError, JackBackend, JackOptions, OpenError, RealtimeStatus,
};
use magnolia_session::{PlayReport, Session, SessionPlayer};

use crate::arguments::PlayOptions;
use crate::latency;
use crate::say;

/// The JACK client's name; the server adds a suffix when another client holds it.
const CLIENT_NAME: &str = "magnolia";
/// How long the disk reader may take to prime the play's first blocks, from a cold cache.
const PRIME_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the server may go without calling the engine before the play is given up.
const STALL_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest the main thread sleeps between two looks at the engine: while a long play rolls,
/// it wakes once a second.
const LONGEST_SLEEP: Duration = Duration::from_secs(1);
/// What a stop plays beyond the pre-roll's length: the stop's fade and the test synth's release,
/// 5 ms each, with room for the main thread's own lateness.
const STOP_TAIL: Duration = Duration::from_millis(100);

/// Plays the session `options` name through JACK, stops, waits for the stop's fade, closes the
/// client, then prints whether the audio thread ran in real time and the engine's and the disk
/// reader's figures. A session with input tracks monitors them, and prints the round trip's
/// latency before it plays.
///
/// # Errors
///
/// A plain message when the session does not open, when the server is missing, at another rate
/// or refuses the client, when the play does not start or the server stops calling the engine,
/// and when the engine or the backend's callback panicked during the play.
pub fn play(options: &PlayOptions) -> Result<(), Box<dyn Error>> {
    let session = Session::open(&options.session)?;
    let rate = session.sample_rate();
    let span = Span::new(options, rate, session.last_sample())?;
    let (mut player, processor) = SessionPlayer::new(&session)?;
    let backend = open_backend(options, processor)?;
    say(format_args!(
        "Playing {} on {}, as the client {:?}: {} frames at {} Hz, from {} s to {} s.",
        options.session.display(),
        server_name(options),
        backend.client_name(),
        backend.period_frames(),
        backend.server_sample_rate(),
        seconds(span.from, rate),
        seconds(span.end, rate),
    ));
    connect(options, &backend)?;
    if let Some(monitoring) = session.monitoring_latency() {
        say(latency::settle(&backend, monitoring, None, rate));
    }

    let stats = Arc::clone(player.stats());
    let watch = Watch {
        stats: &stats,
        backend: &backend,
        rate,
    };
    let rolled = roll(&mut player, &watch, span).map(drop);
    let closed = Closed::close(backend, &player);
    closed.print(&stats);
    rolled?;
    closed.outcome(&stats)
}

/// The server `options` name, for a person to read.
pub(crate) fn server_name(options: &PlayOptions) -> String {
    options.server.as_deref().map_or_else(
        || "the default JACK server".to_owned(),
        |name| format!("the JACK server {name:?}"),
    )
}

/// With `--connect`, connects the playback ports, then the capture ports, to the server's
/// physical ones, and waits for their latency. Connected ports play whether or not the server
/// reported their latency: a play only reports it, so its absence is a warning, not a failure.
pub(crate) fn connect(options: &PlayOptions, backend: &JackBackend) -> Result<(), Box<dyn Error>> {
    if !options.connect {
        return Ok(());
    }
    for kind in ["playback", "capture"] {
        let connected = if kind == "playback" {
            backend.connect_to_physical_playback()
        } else {
            backend.connect_to_physical_capture()
        };
        match connected {
            Ok(0) if kind == "capture" => {}
            Ok(connected) => say(format_args!(
                "Connected {connected} {kind} ports to the server's physical ones."
            )),
            Err(ConnectError::Latency(error)) => say(format_args!(
                "warning: connected the {kind} ports to the server's physical ones, but {error}."
            )),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// What a play leaves once its client is closed: what was read of the engine and the backend
/// while the audio thread still ran, and the processor the backend gave back.
pub(crate) struct Closed {
    realtime: RealtimeStatus,
    report: PlayReport,
    callback_panicked: bool,
    closed: Result<Processor, CloseError>,
}

impl Closed {
    /// Asks the kernel whether the audio thread runs in real time while it still runs, reads the
    /// player's report, then closes the client.
    pub(crate) fn close(backend: JackBackend, player: &SessionPlayer) -> Self {
        let realtime = backend.realtime_status();
        let report = player.report();
        let callback_panicked = backend.callback_panicked();
        Self {
            realtime,
            report,
            callback_panicked,
            closed: backend.close(),
        }
    }

    /// Prints the real-time report, the player's figures and the gaps in the server's frame time.
    pub(crate) fn print(&self, stats: &EngineStats) {
        say(&self.realtime);
        say(self.report.to_string().trim_end());
        say(format_args!(
            "gaps in the server's frame time: {}",
            stats.frame_time_gaps()
        ));
    }

    /// Whether the client closed, and neither the backend's callback nor the engine panicked.
    pub(crate) fn outcome(self, stats: &EngineStats) -> Result<(), Box<dyn Error>> {
        self.closed?;
        if self.callback_panicked {
            return Err(
                "the JACK backend's callback panicked: Magnolia played silence from then on".into(),
            );
        }
        if stats.poisoned() {
            return Err("the engine panicked: it played silence from then on".into());
        }
        Ok(())
    }
}

/// Opens the backend on the server `options` name, around the session's processor.
pub(crate) fn open_backend(
    options: &PlayOptions,
    processor: Processor,
) -> Result<JackBackend, Box<dyn Error>> {
    JackBackend::open(JackOptions {
        client_name: CLIENT_NAME.to_owned(),
        server_name: options.server.clone(),
        processor,
    })
    .map_err(|error| match error {
        // The backend's own message offers to make the engine at the server's rate, which a
        // session cannot.
        OpenError::RateDiffers { server, engine } => format!(
            "the JACK server runs at {server} Hz and the session at {engine} Hz: start the server \
             at {engine} Hz"
        )
        .into(),
        error => error.into(),
    })
}

/// Plays `span`: starts the play, waits until the output has played its last sample, stops, and
/// waits until the stop has faded out. Returns the play's sequence number.
pub(crate) fn roll(
    player: &mut SessionPlayer,
    watch: &Watch<'_>,
    span: Span,
) -> Result<u64, Box<dyn Error>> {
    let sequence = player.start(span.from, PRIME_TIMEOUT)?;
    let sent_at = watch.stats.block_start().0;
    let start = watch.play_start(sequence)?;
    let audible_at = start.audible_at.0;
    watch.until(audible_at.saturating_add(span.frames()))?;
    player.stop()?;
    let stopped_at = watch.stats.block_start().0;
    // The play pre-rolled by the session's largest compensation: about the samples from the
    // period it was sent in to its first audible sample. A stop plays as much again, then fades.
    let pre_roll = audible_at.saturating_sub(sent_at).max(0);
    let tail = pre_roll
        .saturating_add(samples_in(STOP_TAIL, watch.rate))
        .saturating_add(i64::from(watch.backend.period_frames()));
    watch.until(stopped_at.saturating_add(tail))?;
    Ok(sequence)
}

/// The timeline samples a play covers: from `from` up to `end`, which it does not play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) from: SamplePosition,
    pub(crate) end: SamplePosition,
}

impl Span {
    /// The span `options` ask for, on the session's samples: from `--from`, for `--seconds`, or up
    /// to the session's last sample and one second of tail, as the offline render. Each time is
    /// placed on the first sample at or after it.
    pub(crate) fn new(
        options: &PlayOptions,
        rate: SampleRate,
        last_sample: SamplePosition,
    ) -> Result<Self, String> {
        let beyond = || "the play would end beyond the timeline's last sample".to_owned();
        let from = place(options.from, &TimeUnits, &rate).ok_or_else(beyond)?;
        let end = match options.length {
            Some(length) => {
                place(length, &TimeUnits, &rate).and_then(|length| from.0.checked_add(length.0))
            }
            None => last_sample
                .0
                .checked_add(magnolia_session::tail_after_the_last_clip(rate).0),
        }
        .map(SamplePosition)
        .ok_or_else(beyond)?;
        if end <= from {
            return Err(format!(
                "--from {} s lies past the session's end and its second of tail, {} s: give \
                 --seconds to play there",
                seconds(from, rate),
                seconds(end, rate)
            ));
        }
        Ok(Self { from, end })
    }

    /// How many samples it covers.
    fn frames(self) -> i64 {
        self.end.0 - self.from.0
    }
}

/// A sample's time, in seconds to the millisecond, for a person to read.
pub(crate) fn seconds(sample: SamplePosition, rate: SampleRate) -> String {
    format!("{:.3}", sample.0 as f64 / f64::from(rate.hertz()))
}

/// The main thread's view of the engine while the server plays it.
pub(crate) struct Watch<'a> {
    pub(crate) stats: &'a EngineStats,
    pub(crate) backend: &'a JackBackend,
    /// The session's rate, which the server runs at too.
    pub(crate) rate: SampleRate,
}

impl Watch<'_> {
    /// Waits until the processor has applied play `sequence`, and returns where its first sample
    /// reaches the output.
    fn play_start(&self, sequence: u64) -> Result<PlayStart, Box<dyn Error>> {
        self.wait(|| {
            self.stats
                .play_start()
                .filter(|start| start.sequence == sequence)
                .ok_or(Duration::ZERO)
        })
    }

    /// Waits until the engine has started a period at or after engine sample `sample`: every
    /// sample before it has played.
    pub(crate) fn until(&self, sample: i64) -> Result<(), Box<dyn Error>> {
        self.wait(|| {
            let started = self.stats.block_start().0;
            if started >= sample {
                Ok(())
            } else {
                Err(duration_of(sample - started, self.rate))
            }
        })
    }

    /// Looks until `look` finds what it waits for; between two looks, sleeps as long as it
    /// asks, at least a period and at most [`LONGEST_SLEEP`]. Gives up when the server shuts the
    /// client down, when the backend's callback panicked, or when no period has run for
    /// [`STALL_TIMEOUT`].
    pub(crate) fn wait<T>(
        &self,
        mut look: impl FnMut() -> Result<T, Duration>,
    ) -> Result<T, Box<dyn Error>> {
        let mut periods = self.stats.periods();
        let mut last_period_seen = Instant::now();
        loop {
            let left = match look() {
                Ok(found) => return Ok(found),
                Err(left) => left,
            };
            if self.backend.server_shut_down() {
                return Err("the JACK server shut Magnolia's client down".into());
            }
            if self.backend.callback_panicked() {
                return Err("the JACK backend's callback panicked".into());
            }
            let now = self.stats.periods();
            if now != periods {
                periods = now;
                last_period_seen = Instant::now();
            } else if last_period_seen.elapsed() >= STALL_TIMEOUT {
                return Err(format!(
                    "the JACK server has not called Magnolia for {} s",
                    STALL_TIMEOUT.as_secs()
                )
                .into());
            }
            sleep(left.max(self.period()).min(LONGEST_SLEEP));
        }
    }

    pub(crate) fn period(&self) -> Duration {
        duration_of(i64::from(self.backend.period_frames()), self.rate)
    }
}

/// How long `samples` play at `rate`, to the microsecond below; none for a negative count.
pub(crate) fn duration_of(samples: i64, rate: SampleRate) -> Duration {
    let samples = u64::try_from(samples).unwrap_or(0);
    Duration::from_micros(samples.saturating_mul(1_000_000) / u64::from(rate.hertz()))
}

/// How many samples play at `rate` in `duration`, to the sample below.
fn samples_in(duration: Duration, rate: SampleRate) -> i64 {
    let samples = duration.as_micros() * u128::from(rate.hertz()) / 1_000_000;
    i64::try_from(samples).unwrap_or(i64::MAX)
}

/// Sleeps the main thread.
#[expect(
    clippy::disallowed_methods,
    reason = "the command line's main thread waits while the server plays; it never runs on the real-time path"
)]
fn sleep(duration: Duration) {
    thread::sleep(duration);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use magnolia_engine::time::{TIME_UNITS_PER_SECOND, TimePosition};

    use super::*;

    fn options(from: TimePosition, length: Option<TimePosition>) -> PlayOptions {
        PlayOptions {
            session: PathBuf::from("session.json"),
            server: None,
            connect: false,
            from,
            length,
        }
    }

    fn rate() -> SampleRate {
        SampleRate::new(48_000).unwrap()
    }

    const SECOND: i64 = TIME_UNITS_PER_SECOND;

    #[test]
    fn a_play_goes_to_the_sessions_end_and_a_second_of_tail_by_default() {
        let span = Span::new(
            &options(TimePosition(0), None),
            rate(),
            SamplePosition(400_000),
        );
        assert_eq!(
            span,
            Ok(Span {
                from: SamplePosition(0),
                end: SamplePosition(448_000)
            })
        );
        let from = TimePosition(SECOND * 3 / 2);
        let span = Span::new(&options(from, None), rate(), SamplePosition(400_000)).unwrap();
        assert_eq!(span.from, SamplePosition(72_000));
        assert_eq!(span.frames(), 448_000 - 72_000);
    }

    #[test]
    fn seconds_give_the_length_from_where_the_play_starts() {
        let span = Span::new(
            &options(TimePosition(SECOND), Some(TimePosition(2 * SECOND))),
            rate(),
            SamplePosition(0),
        );
        assert_eq!(
            span,
            Ok(Span {
                from: SamplePosition(48_000),
                end: SamplePosition(144_000)
            })
        );
    }

    #[test]
    fn a_time_between_samples_is_placed_on_the_next() {
        let between = TimePosition(rate().time_units_per_sample() * 10 + 1);
        let span = Span::new(
            &options(between, Some(TimePosition(1))),
            rate(),
            SamplePosition(0),
        )
        .unwrap();
        assert_eq!(span.from, SamplePosition(11));
        assert_eq!(span.frames(), 1);
    }

    #[test]
    fn a_start_past_the_end_needs_its_length() {
        let from = TimePosition(10 * SECOND);
        let error = Span::new(&options(from, None), rate(), SamplePosition(400_000)).unwrap_err();
        assert!(
            error.contains("--from 10.000 s lies past the session's end"),
            "{error}"
        );
        assert!(
            Span::new(
                &options(from, Some(TimePosition(SECOND))),
                rate(),
                SamplePosition(400_000)
            )
            .is_ok()
        );
    }

    #[test]
    fn durations_and_samples_convert_at_the_sessions_rate() {
        assert_eq!(duration_of(48_000, rate()), Duration::from_secs(1));
        assert_eq!(duration_of(64, rate()), Duration::from_micros(1_333));
        assert_eq!(duration_of(-5, rate()), Duration::ZERO);
        assert_eq!(
            duration_of(i64::MAX, rate()),
            Duration::from_micros(u64::MAX / 48_000)
        );
        assert_eq!(samples_in(Duration::from_millis(100), rate()), 4_800);
        assert_eq!(samples_in(Duration::MAX, rate()), i64::MAX);
        assert_eq!(seconds(SamplePosition(72_000), rate()), "1.500");
    }
}
