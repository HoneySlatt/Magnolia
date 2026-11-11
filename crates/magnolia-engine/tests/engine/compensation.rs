//! Delay compensation aligns paths of different latencies to the sample (ADR-0025): a latent path
//! nulls against its direct path at every latency; nothing live is delayed; a node that reaches no
//! playback channel delays nothing; sources that meet a latent bus share one delay line, which a
//! commit keeps while its delay stays and removes with the bus; a latency above the largest is
//! taken as the largest and counted. Every graph plays in
//! periods of varied lengths, some above the engine's largest block.

use magnolia_engine::engine::{Destination, NodeId, Source};
use magnolia_engine::node::{MAX_LATENCY, Node, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::{Gain, Sine, TestLatency};

use crate::support::{Constant, LARGEST_BLOCK, Rig, rate};

/// Period lengths, in turn: within the largest block, above it, and one sample.
const PERIODS: [usize; 7] = [64, 1_000, 100, 2_048, 37, 4_096, 1];

/// The latencies the null is checked at: around a block of 64, and the largest.
const LATENCIES: [u32; 6] = [1, 63, 64, 65, 1_000, MAX_LATENCY];

/// Period lengths from `PERIODS`, in turn, that cover at least `frames` samples.
fn periods_covering(frames: usize) -> Vec<usize> {
    let mut periods = Vec::new();
    let mut covered = 0;
    for &period in PERIODS.iter().cycle() {
        if covered >= frames {
            break;
        }
        periods.push(period);
        covered += period;
    }
    periods
}

/// What a sine of 440 Hz at 0.5 plays alone over `periods`.
fn sine_alone(periods: &[usize]) -> Vec<f32> {
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.5));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(periods)
}

/// A sine split into a direct path through a gain and a path through a latency, both to the
/// playback channel; returns what played over `periods`, and the rig.
fn split_through_a_latency(latency: u32, gain: f32, periods: &[usize]) -> (Vec<f32>, Rig, NodeId) {
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.5));
    let direct = rig.add(Gain::new(1, gain));
    let latent = rig.add(TestLatency::new(1, latency));
    for node in [direct, latent] {
        rig.controller
            .connect(Source::Node(sine, 0), Destination::Node(node, 0));
        rig.controller
            .connect(Source::Node(node, 0), Destination::Playback(0));
    }
    // Committed before the first period, the graph plays at once, without a fade.
    rig.controller.commit().unwrap();
    let played = rig.play(periods);
    (played, rig, sine)
}

#[test]
fn a_latent_path_nulls_against_its_direct_path_at_every_latency() {
    for latency in LATENCIES {
        let delay = latency as usize;
        let periods = periods_covering(delay + 5_000);
        let (played, rig, sine) = split_through_a_latency(latency, -1.0, &periods);
        let what = format!("latency {latency}");
        assert!(
            played
                .iter()
                .all(|&sample| sample.to_bits() == 0.0_f32.to_bits()),
            "{what}: a sample is not +0.0"
        );
        // The direct path waits in one delay line for the latent one.
        assert_eq!(rig.controller.delay_lines().len(), 1, "{what}");
        let processor = rig.backend.processor();
        assert_eq!(processor.ahead_in(sine), Some(u64::from(latency)), "{what}");
        assert_eq!(processor.largest_ahead_in(), u64::from(latency), "{what}");

        // With a gain of +1 the two paths add up: the sine, twice, `latency` samples late. So the
        // null above is not two silences.
        let (played, _, _) = split_through_a_latency(latency, 1.0, &periods);
        let alone = sine_alone(&periods);
        for (sample, &played) in played.iter().enumerate() {
            let expected = sample
                .checked_sub(delay)
                .map_or(0.0, |earlier| alone[earlier] + alone[earlier]);
            assert_eq!(played, expected, "{what}, sample {sample}");
        }
        assert!(
            played[delay..].iter().any(|&sample| sample != 0.0),
            "{what}"
        );
    }
}

/// A capture signal that never repeats within a period: a ramp folded on itself.
fn capture_signal(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|sample| ((sample * 7_919) % 1_000) as f32 / 1_000.0 - 0.5)
        .collect()
}

#[test]
fn nothing_live_is_delayed() {
    for (first, second) in [(100, 37), (1, 2_000), (64, 64)] {
        let mut rig = Rig::new(1, 1);
        let a = rig.add(TestLatency::new(1, first));
        let b = rig.add(TestLatency::new(1, second));
        let edges = [
            (Source::Capture(0), Destination::Node(a, 0)),
            (Source::Node(a, 0), Destination::Playback(0)),
            (Source::Node(a, 0), Destination::Node(b, 0)),
            (Source::Node(b, 0), Destination::Playback(0)),
        ];
        for (source, destination) in edges {
            rig.controller.connect(source, destination);
        }
        rig.controller.commit().unwrap();
        let periods = periods_covering(first as usize + second as usize + 5_000);
        let capture = capture_signal(periods.iter().sum());
        let mut played = Vec::new();
        let mut position = 0;
        for &frames in &periods {
            let period = &capture[position..position + frames];
            played.extend_from_slice(&rig.backend.play_period(frames, &[period])[0]);
            position += frames;
        }

        // The capture comes `first` samples late through A, and `first + second` through A then
        // B; A's direct path is never delayed to meet B's.
        let what = format!("latencies {first} and {second}");
        let late = |sample: usize, by: u32| {
            sample
                .checked_sub(by as usize)
                .map_or(0.0, |earlier| capture[earlier])
        };
        for (sample, &played) in played.iter().enumerate() {
            let expected = late(sample, first) + late(sample, first + second);
            assert_eq!(played, expected, "{what}, sample {sample}");
        }
        assert!(rig.controller.delay_lines().is_empty(), "{what}");
        let processor = rig.backend.processor();
        assert_eq!(processor.ahead_in(a), Some(u64::from(first)), "{what}");
        assert_eq!(processor.ahead_in(b), Some(u64::from(second)), "{what}");
    }
}

/// Three sines, each to the playback channel and to a bus that delays by 500 samples.
struct Bus {
    rig: Rig,
    sources: Vec<NodeId>,
    bus: NodeId,
}

impl Bus {
    fn new() -> Self {
        let mut rig = Rig::new(0, 1);
        let bus = rig.add(TestLatency::new(1, 500));
        let sources: Vec<NodeId> = [220.0, 330.0, 550.0]
            .into_iter()
            .map(|frequency| rig.add(Sine::new(rate(), frequency, 0.25)))
            .collect();
        for &source in &sources {
            rig.controller
                .connect(Source::Node(source, 0), Destination::Playback(0));
            rig.controller
                .connect(Source::Node(source, 0), Destination::Node(bus, 0));
        }
        rig.controller
            .connect(Source::Node(bus, 0), Destination::Playback(0));
        rig.controller.commit().unwrap();
        Self { rig, sources, bus }
    }
}

#[test]
fn sources_that_meet_a_latent_bus_share_one_delay_line_kept_while_its_delay_stays() {
    let before = periods_covering(3_000);
    let after = periods_covering(6_000);

    // What the bus plays without the second commit.
    let mut reference = Bus::new();
    let mut expected = reference.rig.play(&before);
    expected.extend(reference.rig.play(&after));

    let mut bus = Bus::new();
    let mut played = bus.rig.play(&before);
    let lines = bus.rig.controller.delay_lines();
    assert_eq!(lines.len(), 1, "one delay line for the three direct paths");
    let processor = bus.rig.backend.processor();
    assert_eq!(processor.ahead_in(lines[0]), Some(500));
    for &source in &bus.sources {
        assert_eq!(processor.ahead_in(source), Some(500));
    }

    // A fourth source, silent, joins the bus and the playback channel: its direct path needs the
    // same delay, so it goes into the same delay line, which keeps its state. The swap crossfades
    // the new edges in; the sound does not change by a bit.
    let fourth = bus.rig.add(Constant::new(0.0));
    bus.rig
        .controller
        .connect(Source::Node(fourth, 0), Destination::Playback(0));
    bus.rig
        .controller
        .connect(Source::Node(fourth, 0), Destination::Node(bus.bus, 0));
    bus.rig.controller.commit().unwrap();
    played.extend(bus.rig.play(&after));
    assert_eq!(bus.rig.controller.delay_lines(), lines);
    assert_eq!(played.len(), expected.len());
    for (sample, (&played, &expected)) in played.iter().zip(&expected).enumerate() {
        assert_eq!(played, expected, "sample {sample}");
    }

    // Without the bus, nothing needs a delay: the delay line goes.
    bus.rig.controller.remove_node(bus.bus).unwrap();
    bus.rig.controller.commit().unwrap();
    bus.rig.play_until_steady(LARGEST_BLOCK);
    assert!(bus.rig.controller.delay_lines().is_empty());
    let processor = bus.rig.backend.processor();
    assert_eq!(processor.ahead_in(bus.sources[0]), Some(0));
    assert_eq!(processor.ahead_in(lines[0]), None);
    assert_eq!(processor.largest_ahead_in(), 0);
}

/// One input passed to one output, which declares the largest latency there is.
struct Overstated;

impl Node for Overstated {
    fn input_channels(&self) -> usize {
        1
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn latency(&self) -> u32 {
        u32::MAX
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        let input = context.input(0);
        context.output_mut(0).copy_from_slice(input);
        ProcessStatus::Continue
    }
}

#[test]
fn a_latency_above_the_largest_is_taken_as_the_largest_and_counted() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.5));
    let overstated = rig.add(Overstated);
    assert_eq!(rig.controller.stats().latencies_capped(), 1);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(overstated, 0));
    rig.controller
        .connect(Source::Node(overstated, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    let largest = u64::from(MAX_LATENCY);
    let processor = rig.backend.processor();
    assert_eq!(processor.ahead_in(overstated), Some(largest));
    assert_eq!(processor.ahead_in(source), Some(largest));
    assert_eq!(processor.largest_ahead_in(), largest);
    assert_eq!(rig.controller.delay_lines().len(), 1);
    assert_eq!(rig.controller.stats().latencies_capped(), 1);
}

#[test]
fn a_latent_node_that_reaches_no_playback_channel_delays_nothing() {
    let periods = [1_024, 1_024, 37, 2_048];
    let mut rig = Rig::new(0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.5));
    // A chain of latent nodes that ends nowhere, beside the sine's direct path.
    let dangling = rig.add(TestLatency::new(1, 1_000));
    let further = rig.add(TestLatency::new(1, 300));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Node(dangling, 0));
    rig.controller
        .connect(Source::Node(dangling, 0), Destination::Node(further, 0));
    rig.controller.commit().unwrap();
    let played = rig.play(&periods);
    // Nobody hears the chain: the sine plays at once, as alone.
    assert_eq!(played, sine_alone(&periods));
    assert!(rig.controller.delay_lines().is_empty());
    let processor = rig.backend.processor();
    assert_eq!(processor.ahead_in(sine), Some(0));
    assert_eq!(processor.largest_ahead_in(), 0);
}
