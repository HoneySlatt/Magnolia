//! Plays a quiet sine through the JACK backend, to listen to it and to measure the engine.
//!
//! ```sh
//! pw-jack -p 64 cargo run --release -p magnolia-jack --example play -- --connect
//! ```
//!
//! It plays a sine at -20 dBFS on two playback ports, and every few seconds commits a graph change
//! that crossfades to another sine and back, to listen for clicks. `--load` adds chains of a sine
//! and a gain, summed far below the sine, as a synthetic load. After one second it prints whether
//! the audio thread runs in real time; at the end, the engine's figures.
//!
//! Options:
//!
//! - `--seconds N`: how long to play, 10 by default;
//! - `--server NAME`: the JACK server to play on, the default one otherwise;
//! - `--connect`: connect the playback ports to the first physical playback ports; off by default;
//! - `--swap-every SECONDS`: how often to swap sines, 2 by default, 0 never;
//! - `--load N`: how many sine and gain chains to add, 0 by default.

use std::env;
use std::error::Error;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use magnolia_engine::engine::{self, Controller, Destination, EngineConfig, NodeId, Source};
use magnolia_engine::nodes::{Gain, Sine};
use magnolia_engine::time::SampleRate;
use magnolia_jack::{JackBackend, JackOptions, OpenError};

/// The sine's amplitude: -20 dBFS, so that a listener is never surprised.
const AMPLITUDE: f32 = 0.1;
/// The two sines a swap crossfades between.
const FREQUENCIES: [f64; 2] = [440.0, 660.0];
/// The load's chains together stay below -40 dBFS.
const LOAD_LEVEL: f32 = 0.01;
/// The engine's playback channels.
const PLAYBACK: usize = 2;
/// The engine's largest block: any server period up to it is one block.
const LARGEST_BLOCK: usize = 4_096;

const USAGE: &str =
    "usage: play [--seconds N] [--server NAME] [--connect] [--swap-every SECONDS] [--load N]";

/// The command line's options.
struct Options {
    seconds: f64,
    server: Option<String>,
    connect: bool,
    swap_every: f64,
    load: usize,
}

impl Options {
    fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            seconds: 10.0,
            server: None,
            connect: false,
            swap_every: 2.0,
            load: 0,
        };
        while let Some(argument) = arguments.next() {
            let mut value = |name: &str| {
                arguments
                    .next()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match argument.as_str() {
                "--seconds" => options.seconds = number(&value("--seconds")?)?,
                "--server" => options.server = Some(value("--server")?),
                "--connect" => options.connect = true,
                "--swap-every" => options.swap_every = number(&value("--swap-every")?)?,
                "--load" => {
                    let load = value("--load")?;
                    options.load = load
                        .parse()
                        .map_err(|_| format!("--load takes a whole number, not {load:?}"))?;
                }
                other => return Err(format!("unknown option {other:?}")),
            }
        }
        Ok(options)
    }
}

/// A number of seconds, finite and not negative.
fn number(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number >= 0.0)
        .ok_or_else(|| format!("expected a number of seconds, not {text:?}"))
}

/// The engine's graph: two sines, one of them sounding, and the load.
struct Graph {
    sines: [NodeId; 2],
    sounding: usize,
}

impl Graph {
    /// Builds the graph at `rate` and commits it: the first sine on every playback channel, and
    /// `load` chains of a sine and a gain, summed quietly.
    fn build(
        controller: &mut Controller,
        rate: SampleRate,
        load: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let sines = [
            controller.add_node(Box::new(Sine::new(rate, FREQUENCIES[0], AMPLITUDE)))?,
            controller.add_node(Box::new(Sine::new(rate, FREQUENCIES[1], AMPLITUDE)))?,
        ];
        let graph = Self { sines, sounding: 0 };
        graph.connect(controller, 0);
        let level = LOAD_LEVEL / load.max(1) as f32;
        for chain in 0..load {
            let frequency = 100.0 + 7.0 * chain as f64;
            let sine = controller.add_node(Box::new(Sine::new(rate, frequency, 1.0)))?;
            let gain = controller.add_node(Box::new(Gain::new(1, level)))?;
            controller.connect(Source::Node(sine, 0), Destination::Node(gain, 0));
            controller.connect(
                Source::Node(gain, 0),
                Destination::Playback(chain % PLAYBACK),
            );
        }
        controller.commit()?;
        Ok(graph)
    }

    fn connect(&self, controller: &mut Controller, sine: usize) {
        for channel in 0..PLAYBACK {
            controller.connect(
                Source::Node(self.sines[sine], 0),
                Destination::Playback(channel),
            );
        }
    }

    /// Commits a crossfade from the sine sounding to the other.
    fn swap(&mut self, controller: &mut Controller) -> Result<(), Box<dyn Error>> {
        for channel in 0..PLAYBACK {
            controller.disconnect(
                Source::Node(self.sines[self.sounding], 0),
                Destination::Playback(channel),
            )?;
        }
        self.sounding = 1 - self.sounding;
        self.connect(controller, self.sounding);
        controller.commit()?;
        Ok(())
    }
}

/// Makes the engine at `hertz` and opens the backend on it.
fn open(options: &Options, hertz: u32) -> Result<(Controller, Graph, JackBackend), Box<dyn Error>> {
    let rate = SampleRate::new(hertz).ok_or_else(|| format!("{hertz} Hz is not a sample rate"))?;
    let (mut controller, processor) =
        engine::new(EngineConfig::new(rate, LARGEST_BLOCK, 0, PLAYBACK))?;
    let graph = Graph::build(&mut controller, rate, options.load)?;
    let backend = JackBackend::open(JackOptions {
        client_name: "magnolia-play".to_owned(),
        server_name: options.server.clone(),
        processor,
        sample_rate: rate,
        capture_channels: 0,
        playback_channels: PLAYBACK,
    })?;
    Ok((controller, graph, backend))
}

/// Sleeps the main thread until `deadline`, in one wait: at rest, nothing wakes it.
#[expect(
    clippy::disallowed_methods,
    reason = "the example's main thread waits between its steps; it never runs on the real-time path"
)]
fn sleep_until(deadline: Instant) {
    thread::sleep(deadline.saturating_duration_since(Instant::now()));
}

fn main() -> ExitCode {
    let options = match Options::parse(env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match play(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn play(options: &Options) -> Result<(), Box<dyn Error>> {
    let (mut controller, mut graph, mut backend) = match open(options, 48_000) {
        Ok(opened) => opened,
        // The engine follows the server's rate.
        Err(error) => match error.downcast_ref::<OpenError>() {
            Some(&OpenError::RateDiffers { server, .. }) => open(options, server)?,
            _ => return Err(error),
        },
    };
    let start = Instant::now();
    let rate = backend.server_sample_rate();
    let period = backend.period_frames();
    println!(
        "Playing a {} Hz sine at -20 dBFS on {} (client {}), {period} frames at {rate} Hz, \
         {} load chains, for {} s.",
        FREQUENCIES[0],
        options.server.as_deref().unwrap_or("the default server"),
        backend.client_name(),
        options.load,
        options.seconds
    );
    if options.connect {
        let connected = backend.connect_to_physical_playback()?;
        println!("Connected {connected} playback ports to the physical ones.");
    }

    let end = start + Duration::from_secs_f64(options.seconds);
    sleep_until((start + Duration::from_secs(1)).min(end));
    // rtkit grants PipeWire's threads their rights within about a second: asked after it.
    println!("{}", backend.realtime_status());
    println!(
        "The audio thread's id: {}.",
        controller.stats().audio_thread_id()
    );

    let swap_every = Duration::from_secs_f64(options.swap_every);
    let mut swaps = 0;
    let mut next_swap = start + swap_every;
    while options.swap_every > 0.0 && next_swap < end {
        sleep_until(next_swap);
        graph.swap(&mut controller)?;
        controller.wait_until_steady(Duration::from_secs(1)).ok();
        backend.poll();
        swaps += 1;
        next_swap += swap_every;
    }
    sleep_until(end);

    report(&controller, &backend, swaps);
    backend.close()?;
    Ok(())
}

/// Prints the engine's figures.
fn report(controller: &Controller, backend: &JackBackend, swaps: u32) {
    let stats = controller.stats();
    let times = stats.callback_times.snapshot();
    let rate = f64::from(backend.server_sample_rate().max(1));
    let period = Duration::from_secs_f64(f64::from(backend.period_frames()) / rate);
    let micros = |time: Option<Duration>| {
        time.map_or("none".to_owned(), |time| {
            format!("{:.1} µs", time.as_secs_f64() * 1e6)
        })
    };
    println!("Periods: {} ({swaps} swaps).", stats.periods());
    println!(
        "Callback times: {} callbacks, median {}, 99.9th percentile {}, maximum {}; over 50 % of \
         the period ({:.1} µs): {:.3} %, over 85 %: {:.3} %.",
        times.count(),
        micros(times.quantile(1, 2)),
        micros(times.quantile(999, 1_000)),
        micros(Some(times.maximum())),
        period.as_secs_f64() * 1e6,
        100.0 * times.share_over(period.mul_f64(0.5)),
        100.0 * times.share_over(period.mul_f64(0.85)),
    );
    println!(
        "Own overruns: {}. Gaps in the frame time: {}. Server xruns: {}.",
        stats.own_overruns(),
        stats.frame_time_gaps(),
        stats.server_xruns()
    );
    println!(
        "Node errors: {}. Non-finite samples: {}. Poisoned: {}.{}",
        stats.node_errors(),
        stats.non_finite_samples(),
        if stats.poisoned() { "yes" } else { "no" },
        if backend.server_shut_down() {
            " The server shut the client down."
        } else {
            ""
        }
    );
}
