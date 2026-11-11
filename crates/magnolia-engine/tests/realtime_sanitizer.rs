//! Proves that RealtimeSanitizer stops a real-time function that allocates, on a thread of its
//! own as the audio thread will be, and stays silent on one that only works on memory allocated
//! beforehand. Without this proof, a check of the real-time path that finds nothing would mean
//! nothing. It also checks that a block's conversions of time, as the engine makes them, neither
//! allocate nor make a system call, and that the engine's processor stays silent through swaps,
//! events, removals, sleeping and failing nodes, swaps that add and remove a delay line, and MIDI
//! sequences played through the test synth, with play, stop and play while rolling.
//!
//! The processor's one allowed system call, the collector's wake, is suppressed by name
//! (`rtsan-suppressions.txt`, ADR-0023). Two cases prove the suppression right: the wake is a
//! system call the sanitizer stops without it, and a lock's wait made beside the wake is still
//! stopped with it.
//!
//! The sanitizer ends the process at its first report, so each case runs in a child process: this
//! same executable, started again with `MAGNOLIA_RTSAN_CASE` naming the case. The parent reads the
//! child's exit status and standard error.
//!
//! Meaningful only on the nightly toolchain, built with `-Zsanitizer=realtime --cfg magnolia_rtsan`
//! (ADR-0019); scripts/check.sh runs it so, and `cargo test` leaves it out.

#![cfg_attr(magnolia_rtsan, feature(sanitize))]

use std::env;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};
use std::time::Duration;

use magnolia_engine::engine::{
    self, ControllerPoint, Destination, EngineConfig, MidiClip, MidiNote, MidiSequence, NodeId,
    PitchBendPoint, Source, SwapStatus, TimedEvent, wake_collector,
};
use magnolia_engine::node::{EventKind, Node, Note, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::{Gain, Sine, TestLatency, TestSynth};
use magnolia_engine::offline::OfflineBackend;
use magnolia_engine::time::{
    Meter, MusicalPosition, SamplePosition, SampleRate, TICKS_PER_QUARTER, Tempo, TempoMap,
    TimePosition, place,
};

/// Names the case a child process runs; the parent leaves it unset.
const CASE_VARIABLE: &str = "MAGNOLIA_RTSAN_CASE";

/// The text of the sanitizer's report of a call that a real-time function must not make.
const VIOLATION: &str = "RealtimeSanitizer: unsafe-library-call";

/// Allocates in a real-time context, as a bug on the audio thread would.
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn allocate() -> usize {
    let buffer: Vec<f32> = Vec::with_capacity(black_box(64));
    black_box(buffer).capacity()
}

/// Works on memory allocated beforehand, as the audio thread must.
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn halve(buffer: &mut [f32]) {
    for sample in buffer.iter_mut() {
        *sample *= 0.5;
    }
}

/// Converts time as the engine does for each block: the ticks whose events play in the block, then
/// the sample of each event, on a tempo map built beforehand. Returns the sum of the samples.
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn convert_block(map: &TempoMap, rate: SampleRate, start: SamplePosition, frames: u32) -> i64 {
    let ticks = map
        .ticks_in_block(rate, start, frames)
        .expect("the block is in range");
    let step = ((ticks.end.0 - ticks.start.0) / 8).max(1);
    let mut sum = 0;
    let mut tick = ticks.start.0;
    while tick < ticks.end.0 {
        sum += place(MusicalPosition(tick), map, &rate)
            .expect("the tick is in range")
            .0;
        tick += step;
    }
    sum
}

/// Wakes a sleeping thread from a real-time context, as the processor wakes the collector.
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn wake(collector: &Thread) {
    wake_collector(collector);
}

/// Wakes a sleeping thread, then waits for a lock another thread holds, from a real-time context.
#[expect(
    clippy::disallowed_types,
    reason = "the case proves that the sanitizer still stops a lock's wait beside an allowed wake"
)]
#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]
fn wake_then_wait(collector: &Thread, lock: &std::sync::Mutex<()>) {
    wake_collector(collector);
    drop(black_box(lock.lock()));
}

/// A thread that sleeps until the process ends, for a case to wake.
#[expect(
    clippy::disallowed_methods,
    reason = "a thread of the test's that only sleeps; it never runs in a real-time context"
)]
fn sleeping_thread() -> Thread {
    let sleeper = thread::spawn(|| {
        loop {
            thread::park();
        }
    });
    // Long enough for the thread to be asleep, so that waking it is a system call.
    pause();
    sleeper.thread().clone()
}

/// Waits a tenth of a second, off the real-time context.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's main thread lets another thread fall asleep; it never runs in a real-time context"
)]
fn pause() {
    thread::sleep(Duration::from_millis(100));
}

/// A lock held by a thread of its own for five seconds, far longer than the case's wait takes to
/// be stopped, then released, so that a build in which nothing stops the wait still ends.
#[expect(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "the case needs a lock held elsewhere, to prove its wait is reported; no real-time context takes it but the case's"
)]
fn held_lock() -> Arc<std::sync::Mutex<()>> {
    let lock = Arc::new(std::sync::Mutex::new(()));
    let held = Arc::new(AtomicBool::new(false));
    let (holder_lock, holder_held) = (Arc::clone(&lock), Arc::clone(&held));
    thread::spawn(move || {
        let _guard = holder_lock.lock().expect("the lock is new");
        holder_held.store(true, Ordering::Release);
        thread::sleep(Duration::from_secs(5));
    });
    while !held.load(Ordering::Acquire) {
        thread::yield_now();
    }
    lock
}

/// One output that fails at every call.
struct Failing;

impl Node for Failing {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, _context: &mut ProcessContext<'_>) -> ProcessStatus {
        ProcessStatus::Error
    }
}

/// Plays the engine through what its processor does: a crossfade, a dip, a larger buffer pool,
/// events, nodes removed and handed to the collector, a node asleep and one failing, at periods
/// within the largest block and above it. Only the processor runs in a real-time context: the
/// controller and the backend allocate between its periods, as they may.
fn play_the_engine() {
    let rate = SampleRate::new(48_000).unwrap();
    let (mut controller, processor) = engine::new(EngineConfig::new(rate, 256, 1, 2)).unwrap();
    let mut backend = OfflineBackend::new(processor);
    let capture = vec![0.25; 1_024];
    let mut play = |controller: &mut engine::Controller, periods: usize| {
        for period in 0..periods {
            let frames = [64, 1_000, 100, 256][period % 4];
            backend.play_period(frames, &[&capture[..frames]]);
            controller.poll();
        }
    };
    let add = |controller: &mut engine::Controller, node: Box<dyn Node>| -> NodeId {
        controller.add_node(node).unwrap()
    };
    let event = |node: NodeId, at: i64, index: u32, value: f64| TimedEvent {
        node,
        at: SamplePosition(at),
        kind: EventKind::Parameter { index, value },
    };

    let a = add(&mut controller, Box::new(Sine::new(rate, 440.0, 0.5)));
    let b = add(&mut controller, Box::new(Sine::new(rate, 660.0, 0.5)));
    let x = add(&mut controller, Box::new(Gain::new(1, 0.5)));
    let y = add(&mut controller, Box::new(Gain::new(1, 0.8)));
    let asleep = add(&mut controller, Box::new(Gain::new(2, 1.0)));
    let failing = add(&mut controller, Box::new(Failing));
    let chain = |controller: &mut engine::Controller, nodes: [NodeId; 3], connect: bool| {
        let edges = [
            (Source::Node(nodes[0], 0), Destination::Node(nodes[1], 0)),
            (Source::Node(nodes[1], 0), Destination::Node(nodes[2], 0)),
            (Source::Node(nodes[2], 0), Destination::Playback(0)),
        ];
        for (source, destination) in edges {
            if connect {
                controller.connect(source, destination);
            } else {
                controller.disconnect(source, destination).unwrap();
            }
        }
    };
    chain(&mut controller, [a, x, y], true);
    controller.connect(Source::Capture(0), Destination::Playback(1));
    controller.connect(Source::Node(failing, 0), Destination::Playback(1));
    controller.connect(Source::Node(asleep, 1), Destination::Playback(1));
    controller.commit().unwrap();
    play(&mut controller, 8);

    // Events at many samples, some late, two at one sample.
    for (index, at) in [0, 10, 700, 701, 701, 1_500, 4_000].into_iter().enumerate() {
        let value = 0.1 * index as f64;
        controller.send_event(event(x, at, 0, value)).unwrap();
        controller
            .send_event(event(a, at, 0, 200.0 + 50.0 * value))
            .unwrap();
    }
    controller.send_event(event(asleep, 9_000, 0, 0.5)).unwrap();
    play(&mut controller, 8);

    // A crossfade: the second sine takes the first one's place.
    controller
        .disconnect(Source::Node(a, 0), Destination::Node(x, 0))
        .unwrap();
    controller.connect(Source::Node(b, 0), Destination::Node(x, 0));
    controller.commit().unwrap();
    play(&mut controller, 8);

    // A dip: the two gains change places.
    chain(&mut controller, [b, x, y], false);
    chain(&mut controller, [b, y, x], true);
    controller.commit().unwrap();
    play(&mut controller, 8);

    // A larger pool: twenty more tracks; then half of them removed, for the collector.
    let tracks: Vec<[NodeId; 2]> = (0..20)
        .map(|track| {
            let sine = add(
                &mut controller,
                Box::new(Sine::new(rate, 100.0 + 10.0 * f64::from(track), 0.01)),
            );
            let gain = add(&mut controller, Box::new(Gain::new(1, 0.5)));
            controller.connect(Source::Node(sine, 0), Destination::Node(gain, 0));
            controller.connect(Source::Node(gain, 0), Destination::Playback(0));
            [sine, gain]
        })
        .collect();
    controller.commit().unwrap();
    play(&mut controller, 8);
    for track in &tracks[..10] {
        for &node in track {
            controller.remove_node(node).unwrap();
        }
    }
    controller.commit().unwrap();
    play(&mut controller, 8);
    for track in &tracks[10..] {
        controller.send_event(event(track[1], 0, 0, 0.25)).unwrap();
    }
    play(&mut controller, 8);
    assert_eq!(controller.poll(), SwapStatus::Steady);
    let stats = controller.stats();
    assert!(stats.node_errors() > 0, "the failing node ran");
    assert_eq!(
        stats.last_swap_steady(),
        5,
        "every swap reached its steady phase"
    );
    assert!(!stats.poisoned());
    play_a_busy_collector();
}

/// Plays the engine through swaps that add a delay line and remove one: a sine's direct path meets
/// a latent path, which the compiler compensates with a delay line, then the latent node goes;
/// beside them, a live capture through a latency, which nothing delays. Only the processor runs in
/// a real-time context.
fn play_delay_lines() {
    let rate = SampleRate::new(48_000).unwrap();
    let (mut controller, processor) = engine::new(EngineConfig::new(rate, 256, 1, 2)).unwrap();
    let mut backend = OfflineBackend::new(processor);
    let capture = vec![0.25; 1_024];
    let mut play = |controller: &mut engine::Controller, periods: usize| {
        for period in 0..periods {
            let frames = [64, 1_000, 100, 256][period % 4];
            backend.play_period(frames, &[&capture[..frames]]);
            controller.poll();
        }
    };
    let add = |controller: &mut engine::Controller, node: Box<dyn Node>| -> NodeId {
        controller.add_node(node).unwrap()
    };
    let sine = add(&mut controller, Box::new(Sine::new(rate, 440.0, 0.5)));
    let live = add(&mut controller, Box::new(TestLatency::new(1, 64)));
    controller.connect(Source::Node(sine, 0), Destination::Playback(0));
    controller.connect(Source::Capture(0), Destination::Node(live, 0));
    controller.connect(Source::Node(live, 0), Destination::Playback(1));
    controller.commit().unwrap();
    play(&mut controller, 8);
    assert!(controller.delay_lines().is_empty());

    let latent = add(&mut controller, Box::new(TestLatency::new(1, 1_500)));
    controller.connect(Source::Node(sine, 0), Destination::Node(latent, 0));
    controller.connect(Source::Node(latent, 0), Destination::Playback(0));
    controller.commit().unwrap();
    play(&mut controller, 16);
    assert_eq!(
        controller.delay_lines().len(),
        1,
        "a swap added a delay line"
    );

    controller.remove_node(latent).unwrap();
    controller.commit().unwrap();
    play(&mut controller, 16);
    assert_eq!(controller.poll(), SwapStatus::Steady);
    assert!(
        controller.delay_lines().is_empty(),
        "a swap removed the delay line"
    );
    let stats = controller.stats();
    assert_eq!(stats.last_swap_steady(), 3, "every swap is steady");
    assert!(!stats.poisoned());
}

/// A clip of `quarters` quarters from `start`: a note every eighth, some long enough to cross the
/// clip's end, a chord of twenty notes that steals voices, bends and controllers.
fn busy_clip(start: i64, quarters: i64) -> MidiClip {
    let eighth = TICKS_PER_QUARTER / 8;
    let mut notes: Vec<MidiNote> = (0..quarters * 8)
        .map(|step| MidiNote {
            tick: step * eighth,
            length: (1 + step % 7) * eighth,
            channel: (step % 2) as u8,
            key: 40 + (step * 7 % 48) as u8,
            velocity: 0.75,
        })
        .collect();
    notes.extend((0..20).map(|key| MidiNote {
        tick: quarters / 2 * TICKS_PER_QUARTER,
        length: TICKS_PER_QUARTER,
        channel: 0,
        key: 30 + key,
        velocity: 0.5,
    }));
    MidiClip {
        start: MusicalPosition(start),
        length: quarters * TICKS_PER_QUARTER,
        notes,
        pitch_bends: (0..quarters * 4)
            .map(|step| PitchBendPoint {
                tick: step * 2 * eighth + 1,
                channel: (step % 2) as u8,
                value: if step % 2 == 0 { 0.5 } else { -0.25 },
            })
            .collect(),
        controllers: (0..quarters * 2)
            .map(|step| ControllerPoint {
                tick: step * 4 * eighth,
                channel: 0,
                number: if step % 2 == 0 { 64 } else { 1 },
                value: if step % 4 < 2 { 1.0 } else { 0.0 },
            })
            .collect(),
    }
}

/// Plays MIDI sequences through two test synths, one through a latency, under a tempo map with
/// changes: a play, a stop, plays while rolling, the controller's notes beside the timeline's, and
/// a commit that replaces a sequence while notes are held. Only the processor runs in a real-time
/// context.
fn play_sequences() {
    let rate = SampleRate::new(48_000).unwrap();
    let (mut controller, processor) = engine::new(EngineConfig::new(rate, 256, 0, 1)).unwrap();
    let mut backend = OfflineBackend::new(processor);
    let mut sounding = 0;
    let mut play = |controller: &mut engine::Controller, periods: usize| {
        for period in 0..periods {
            let frames = [64, 1_000, 100, 256][period % 4];
            let played = backend.play_period(frames, &[]);
            sounding += played[0].iter().filter(|&&sample| sample != 0.0).count();
            controller.poll();
        }
    };
    let quarter = TICKS_PER_QUARTER;
    let sequence = |clips: &[MidiClip]| Arc::new(MidiSequence::new(clips).unwrap());
    let first = sequence(&[busy_clip(0, 6), busy_clip(5 * quarter, 4)]);
    let second = sequence(&[busy_clip(quarter, 3)]);
    let latent_synth = controller.add_node(Box::new(TestSynth::new(rate))).unwrap();
    let latency = controller
        .add_node(Box::new(TestLatency::new(1, 300)))
        .unwrap();
    let direct_synth = controller.add_node(Box::new(TestSynth::new(rate))).unwrap();
    controller.connect(Source::Node(latent_synth, 0), Destination::Node(latency, 0));
    controller.connect(Source::Node(latency, 0), Destination::Playback(0));
    controller.connect(Source::Node(direct_synth, 1), Destination::Playback(0));
    controller
        .set_sequences(latent_synth, vec![Arc::clone(&first), Arc::clone(&second)])
        .unwrap();
    controller
        .set_sequences(direct_synth, vec![Arc::clone(&second)])
        .unwrap();
    controller.set_tempo_map(Arc::new(tempo_map()));
    controller.commit().unwrap();
    play(&mut controller, 4);

    // From a sample between ticks, after bends and controllers to chase.
    controller.play(SamplePosition(30_001)).unwrap();
    play(&mut controller, 40);
    let note = Note {
        id: Some(1),
        channel: 2,
        key: 72,
        velocity: 1.0,
    };
    let at = controller.stats().block_start().0 + 2_000;
    for (offset, kind) in [
        (0, EventKind::NoteOn(note)),
        (900, EventKind::NoteOff(note)),
    ] {
        let event = TimedEvent {
            node: direct_synth,
            at: SamplePosition(at + offset),
            kind,
        };
        controller.send_event(event).unwrap();
    }
    play(&mut controller, 20);
    controller.stop().unwrap();
    play(&mut controller, 20);
    controller.play(SamplePosition(0)).unwrap();
    play(&mut controller, 40);
    // A play while rolling, and a sequence replaced while its notes are held.
    controller.play(SamplePosition(100_000)).unwrap();
    play(&mut controller, 20);
    controller
        .set_sequences(latent_synth, vec![Arc::clone(&second)])
        .unwrap();
    controller.commit().unwrap();
    play(&mut controller, 40);
    controller.stop().unwrap();
    play(&mut controller, 20);
    assert!(sounding > 10_000, "the sequences played");
    assert_eq!(controller.poll(), SwapStatus::Steady);
    let stats = controller.stats();
    assert_eq!(stats.play_start().map(|start| start.sequence), Some(3));
    assert_eq!(stats.events_dropped(), 0);
    assert!(!stats.poisoned());
}

/// A silent node whose drop keeps the collector busy until `open` is set, or ten seconds pass;
/// `entered` tells the test it has started.
struct BlockingDrop {
    entered: Arc<AtomicBool>,
    open: Arc<AtomicBool>,
}

impl Node for BlockingDrop {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        context.output_mut(0).fill(0.0);
        context.set_output_silent(0, true);
        ProcessStatus::Continue
    }
}

impl Drop for BlockingDrop {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        for _ in 0..100 {
            if self.open.load(Ordering::Acquire) {
                return;
            }
            pause();
        }
    }
}

/// Plays a processor whose collector is kept busy: what it hands over then fills the queue and an
/// overflow slot, the next swap waits, events go on, and once the collector drains, the
/// overflow is sent and the swap applied.
fn play_a_busy_collector() {
    let rate = SampleRate::new(48_000).unwrap();
    let mut config = EngineConfig::new(rate, 256, 0, 1);
    config.garbage_capacity = EngineConfig::MIN_GARBAGE_CAPACITY;
    let (mut controller, processor) = engine::new(config).unwrap();
    let mut backend = OfflineBackend::new(processor);
    let mut play = |controller: &mut engine::Controller, periods: usize| {
        for _ in 0..periods {
            backend.play_period(64, &[]);
            controller.poll();
        }
    };
    let (entered, open) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let source = controller
        .add_node(Box::new(Sine::new(rate, 440.0, 0.5)))
        .unwrap();
    let blocking = controller
        .add_node(Box::new(BlockingDrop {
            entered: Arc::clone(&entered),
            open: Arc::clone(&open),
        }))
        .unwrap();
    let spare = controller.add_node(Box::new(Gain::new(1, 1.0))).unwrap();
    controller.connect(Source::Node(source, 0), Destination::Playback(0));
    controller.commit().unwrap();
    play(&mut controller, 4);

    // The collector drops the blocking node last of what swap 2 hands it, and stays there. Swap 2
    // may wait for the collector to drop what swap 1 replaced: play until the collector is in.
    controller.remove_node(blocking).unwrap();
    controller.commit().unwrap();
    for _ in 0..100 {
        play(&mut controller, 1);
        if entered.load(Ordering::Acquire) {
            break;
        }
        pause();
    }
    assert!(
        entered.load(Ordering::Acquire),
        "the collector is in the blocking drop"
    );
    // Swap 3 grows the pool and drops a node at once: the queue fills, and the dropped node waits
    // in an overflow slot, which the next periods fail to flush.
    for _ in 0..40 {
        controller.add_node(Box::new(Gain::new(1, 1.0))).unwrap();
    }
    controller.remove_node(spare).unwrap();
    controller.commit().unwrap();
    play(&mut controller, 1);
    // Swap 4 waits for the collector; its new node's event waits in the controller, while an
    // event for the source plays.
    let added = controller
        .add_node(Box::new(Sine::new(rate, 660.0, 0.5)))
        .unwrap();
    controller.connect(Source::Node(added, 0), Destination::Playback(0));
    controller.commit().unwrap();
    let next = controller.stats().block_start().0 + 64;
    let gain = |node, value| TimedEvent {
        node,
        at: SamplePosition(next + 10),
        kind: EventKind::Parameter { index: 1, value },
    };
    controller.send_event(gain(added, 0.25)).unwrap();
    controller.send_event(gain(source, 0.25)).unwrap();
    play(&mut controller, 8);
    assert_eq!(controller.stats().last_swap_applied(), 3, "swap 4 waited");

    // The collector looks at the gate every tenth of a second: wait as long between tries.
    open.store(true, Ordering::Release);
    for _ in 0..100 {
        play(&mut controller, 8);
        if controller.poll() == SwapStatus::Steady {
            break;
        }
        pause();
    }
    assert_eq!(controller.stats().last_swap_steady(), 4, "swap 4 is steady");
    assert_eq!(controller.stats().events_dropped(), 0);
}

/// A tempo map with a few changes, built off the real-time context.
fn tempo_map() -> TempoMap {
    let tempo = |numerator, denominator| Tempo::from_bpm(numerator, denominator).unwrap();
    let changes = [
        (MusicalPosition(0), tempo(174, 1)),
        (MusicalPosition(10_000_000), tempo(187, 2)),
        (MusicalPosition(20_000_000), tempo(61, 1)),
    ];
    TempoMap::new(TimePosition(0), &changes, &[(0, Meter::new(4, 4).unwrap())]).unwrap()
}

/// Runs `work` on a new thread and waits for it, as the audio thread will run. Only the functions
/// marked `nonblocking` that `work` calls run in the real-time context; the rest of `work` does not.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's main thread waits for the case's thread; it never runs in a real-time context"
)]
fn on_new_thread(work: impl FnOnce() + Send + 'static) {
    thread::spawn(work)
        .join()
        .expect("the test's thread panicked");
}

/// Runs one case in this process, as a child of the parent's.
fn run_case(case: &str) -> ExitCode {
    match case {
        "allocate" => on_new_thread(|| {
            black_box(allocate());
        }),
        "halve" => on_new_thread(|| {
            let mut buffer = vec![1.0; 64];
            halve(&mut buffer);
            assert!(buffer.iter().all(|&sample| sample == 0.5));
        }),
        "convert" => on_new_thread(|| {
            let map = tempo_map();
            let rate = SampleRate::new(48_000).unwrap();
            for block in 0..1_000 {
                black_box(convert_block(&map, rate, SamplePosition(block * 64), 64));
            }
        }),
        "engine" => on_new_thread(play_the_engine),
        "delay_lines" => on_new_thread(play_delay_lines),
        "sequences" => on_new_thread(play_sequences),
        "wake" => {
            let collector = sleeping_thread();
            on_new_thread(move || wake(&collector));
        }
        "wake_then_wait" => {
            let collector = sleeping_thread();
            let lock = held_lock();
            on_new_thread(move || wake_then_wait(&collector, &lock));
        }
        _ => {
            eprintln!("unknown case {case:?}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// The sanitizer's options for a case: stop at the first report, and symbolize the reports with
/// `addr2line` (ADR-0023), so that the suppressions see function names. `suppressed` adds the
/// workspace's suppressions.
fn sanitizer_options(suppressed: bool) -> String {
    let mut options = String::from("halt_on_error=true:allow_addr2line=true");
    if suppressed {
        let suppressions = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../rtsan-suppressions.txt")
            .canonicalize()
            .expect("rtsan-suppressions.txt is at the workspace's root");
        options.push_str(":suppressions=");
        options.push_str(
            suppressions
                .to_str()
                .expect("the workspace's path is UTF-8"),
        );
    }
    options
}

/// Starts this executable again on one case, under the sanitizer's options.
fn run_child(case: &str, options: &str) -> Output {
    let executable = env::current_exe().expect("the test's executable has no path");
    Command::new(executable)
        .env(CASE_VARIABLE, case)
        .env("RTSAN_OPTIONS", options)
        .output()
        .expect("the child process did not start")
}

/// What the sanitizer must do with a case's child for the case to pass.
#[derive(Clone, Copy)]
enum Expected {
    /// Stop the child at its call to `malloc`, with a report.
    Stopped,
    /// Stop the child at a system call made from the function named, with a report.
    StoppedAtSystemCall { from: &'static str },
    /// Let the child exit successfully, without a report.
    Silent,
}

/// Runs one case in a child process under the workspace's suppressions and checks its outcome.
fn check(case: &str, expected: Expected) -> bool {
    check_under(case, expected, &sanitizer_options(true), case)
}

/// Runs one case in a child process under `options` and checks its outcome, reporting it as
/// `name`; on failure, says why and shows the child's standard error.
fn check_under(case: &str, expected: Expected, options: &str, name: &str) -> bool {
    let child = run_child(case, options);
    let stderr = String::from_utf8_lossy(&child.stderr);
    let reported = stderr.contains(VIOLATION);
    let stopped = !child.status.success() && reported;
    let passed = match expected {
        Expected::Stopped => stopped && stderr.contains("`malloc`"),
        Expected::StoppedAtSystemCall { from } => {
            stopped && stderr.contains("`syscall`") && stderr.contains(from)
        }
        Expected::Silent => child.status.success() && !reported,
    };
    println!("test {name} ... {}", if passed { "ok" } else { "FAILED" });
    if !passed {
        match expected {
            Expected::Stopped => println!(
                "  expected the sanitizer to stop the child at its call to `malloc`; was the \
                 test built with -Zsanitizer=realtime --cfg magnolia_rtsan (scripts/check.sh)?"
            ),
            Expected::StoppedAtSystemCall { from } => println!(
                "  expected the sanitizer to stop the child at a system call from {from}; was the \
                 test built with -Zsanitizer=realtime --cfg magnolia_rtsan (scripts/check.sh)?"
            ),
            Expected::Silent => println!("  expected the child to exit successfully, unreported"),
        }
        println!(
            "  the child exited with {}; its standard error:",
            child.status
        );
        println!("{stderr}");
    }
    passed
}

fn main() -> ExitCode {
    if let Some(case) = env::var_os(CASE_VARIABLE) {
        return run_case(&case.to_string_lossy());
    }
    // Without the sanitizer, nothing stops `wake_then_wait`'s wait: its lock is held for seconds.
    // A build that does not stop an allocation says so at once and runs nothing else.
    if !check("allocate", Expected::Stopped) {
        println!("  RealtimeSanitizer is not active in this build; the other cases are not run");
        return ExitCode::FAILURE;
    }
    let results = [
        check("halve", Expected::Silent),
        check("convert", Expected::Silent),
        check("engine", Expected::Silent),
        check("delay_lines", Expected::Silent),
        check("sequences", Expected::Silent),
        // The wake is a system call the sanitizer stops, until the suppressions name it.
        check_under(
            "wake",
            Expected::StoppedAtSystemCall { from: "futex_wake" },
            &sanitizer_options(false),
            "wake_unsuppressed",
        ),
        check("wake", Expected::Silent),
        // A lock's wait beside the wake is still stopped: the suppression hides the wake alone.
        check(
            "wake_then_wait",
            Expected::StoppedAtSystemCall { from: "futex_wait" },
        ),
    ];
    if results.iter().all(|&passed| passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
