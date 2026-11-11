//! Events are sample-accurate: a parameter changes the output exactly at its sample, in any
//! sub-block; events at one sample keep their order; an event follows the swap that brings its
//! node; an event for a node gone is dropped and counted; a late event plays at once.
//!
//! The controller sends an event for a node only once the processor holds that node, so each test
//! plays a period after its first commit before it sends events on time.

use magnolia_engine::engine::{
    Commit, Destination, EngineConfig, SendEventError, Source, TimedEvent,
};
use magnolia_engine::node::EventKind;
use magnolia_engine::nodes::{Gain, Sine};
use magnolia_engine::time::SamplePosition;

use crate::support::{
    Constant, FADE, LARGEST_BLOCK, Recorder, Rig, assert_matches, fade_in, rate, read, sine,
};

#[test]
fn a_gain_event_changes_the_output_exactly_at_its_sample_in_any_sub_block() {
    // At 2,048 frames each period is cut into two sub-blocks of 1,024.
    for period in [100, 2_048] {
        for at in (0..2 * period).step_by(if period == 100 { 1 } else { 5 }) {
            let mut rig = Rig::new(0, 1);
            let one = rig.add(Constant::new(1.0));
            let gain = rig.add(Gain::new(1, 0.25));
            rig.controller
                .connect(Source::Node(one, 0), Destination::Node(gain, 0));
            rig.controller
                .connect(Source::Node(gain, 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            rig.play(&[period]);
            rig.send(gain, (period + at) as i64, Gain::GAIN, 0.75);
            let played = rig.play(&[period, period, period]);
            let expected: Vec<f64> = (0..played.len())
                .map(|n| if n < at { 0.25 } else { 0.75 })
                .collect();
            assert_matches(
                &played,
                &expected,
                0.0,
                &format!("period {period}, at {at}"),
            );
        }
    }
}

#[test]
fn a_sines_gain_event_takes_effect_at_its_sample() {
    let mut rig = Rig::new(0, 1);
    let tone = rig.add(Sine::new(rate(), 440.0, 0.5));
    rig.controller
        .connect(Source::Node(tone, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    rig.send(tone, 64 + 1_500, Sine::GAIN, 0.25);
    let played = rig.play(&[2_048]);
    let expected: Vec<f64> = (0..played.len())
        .map(|n| sine(64 + n, 440.0, if n < 1_500 { 0.5 } else { 0.25 }))
        .collect();
    assert_matches(&played, &expected, 1e-6, "sine's gain");
}

#[test]
fn two_events_at_one_sample_apply_in_the_order_they_were_sent() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.5));
    let (recorder, calls) = Recorder::new();
    let recorder = rig.add(recorder);
    rig.controller
        .connect(Source::Node(source, 0), Destination::Node(recorder, 0));
    rig.controller
        .connect(Source::Node(recorder, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    // Sent out of order in time, and three at sample 70: the last sent at 70 holds.
    for (at, value) in [(70, 0.1), (30, 0.2), (70, 0.9), (70, 0.4)] {
        rig.send(source, 64 + at, 0, value);
        rig.send(recorder, 64 + at, 0, value);
    }
    let played = rig.play(&[64, 64]);
    let expected: Vec<f64> = (0..128)
        .map(|n| match n {
            0..30 => 0.5,
            30..70 => 0.2,
            _ => 0.4,
        })
        .collect();
    assert_matches(&played, &expected, 1e-7, "events at one sample");
    let calls = read(&calls);
    assert_eq!(calls[1].events, [(30, 0.2)]);
    assert_eq!(calls[2].events, [(6, 0.1), (6, 0.9), (6, 0.4)]);
}

#[test]
fn an_event_sent_right_after_its_nodes_commit_is_not_dropped() {
    let mut rig = Rig::new(0, 1);
    rig.controller.commit().unwrap();
    rig.play(&[64, 64]);
    // A new node, connected and committed, then an event for it at once, stamped at the last
    // period's start. The controller holds it until the processor has applied the swap, at the
    // next period's start; it plays, late, from the period after: the node's value fades in from
    // zero, then from one half.
    let added = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(added, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    let at = rig.controller.stats().block_start().0;
    rig.send(added, at, 0, 0.5);
    let played = rig.play(&[64; 5]);
    let expected: Vec<f64> = (0..played.len())
        .map(|i| match i {
            0..64 => 0.0,
            _ if i < FADE => 0.5 * fade_in(i, FADE),
            _ => 0.5,
        })
        .collect();
    assert_matches(&played, &expected, 1e-6, "event after commit");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn an_event_for_a_node_whose_commit_is_deferred_follows_its_swap() {
    let mut rig = Rig::new(0, 1);
    let first = rig.add(Constant::new(1.0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    // A swap in flight, fading the first node in.
    rig.controller
        .connect(Source::Node(first, 0), Destination::Playback(0));
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    let added = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(added, 0), Destination::Playback(0));
    assert_eq!(rig.controller.commit().unwrap(), Commit::Deferred);
    rig.send(added, 0, 0, 0.5);
    rig.play_until_steady(64);
    // Steady: 1 from the first node, 0.5 from the second, whose event reached it.
    let played = rig.play(&[64]);
    assert_matches(&played, &[1.5; 64], 0.0, "event after a deferred commit");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn an_event_for_a_node_that_only_a_dips_second_phase_runs_waits_for_it() {
    let mut rig = Rig::new(0, 1);
    let old: Vec<_> = (0..4).map(|_| rig.add(Constant::new(1.0))).collect();
    rig.controller
        .connect(Source::Node(old[0], 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    for &node in &old {
        rig.controller.remove_node(node).unwrap();
    }
    let new: Vec<_> = (0..4).map(|_| rig.add(Constant::new(0.0))).collect();
    rig.controller
        .connect(Source::Node(new[0], 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.send(new[0], 64, 0, 1.0);
    let played = rig.play(&[64; 10]);
    // The dip: the old node fades out, then the new one fades in, already at 1.
    let expected: Vec<f64> = (0..played.len())
        .map(|i| match i {
            i if i < FADE => 1.0 - fade_in(i, FADE),
            i if i < 2 * FADE => fade_in(i - FADE, FADE),
            _ => 1.0,
        })
        .collect();
    assert_matches(&played, &expected, 1e-6, "event through a dip");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn an_event_for_a_removed_node_is_dropped_and_counted() {
    let mut rig = Rig::new(0, 1);
    let gone = rig.add(Constant::new(1.0));
    rig.controller
        .connect(Source::Node(gone, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64]);
    rig.controller.remove_node(gone).unwrap();
    rig.controller.commit().unwrap();
    rig.play_until_steady(64);
    let at = rig.controller.stats().block_start().0 + 64;
    rig.send(gone, at, 0, 0.5);
    rig.play(&[64]);
    assert_eq!(rig.controller.stats().events_dropped(), 1);
}

#[test]
fn a_late_event_plays_at_the_next_periods_first_sample() {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Constant::new(0.25));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.play(&[64, 64, 64]);
    rig.send(source, 10, 0, 0.75);
    let played = rig.play(&[64]);
    assert_matches(&played, &[0.75; 64], 0.0, "late event");
    assert_eq!(rig.controller.stats().events_dropped(), 0);
}

#[test]
fn a_full_event_queue_gives_the_event_back() {
    let mut config = EngineConfig::new(rate(), LARGEST_BLOCK, 0, 1);
    config.event_capacity = 4;
    let mut rig = Rig::with_config(config);
    let source = rig.add(Constant::new(0.0));
    rig.controller
        .connect(Source::Node(source, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let event = |at| TimedEvent {
        node: source,
        at: SamplePosition(at),
        kind: EventKind::Parameter {
            index: 0,
            value: 1.0,
        },
    };
    // Before the processor holds the node, the controller holds its events: as many as the queue.
    for at in 0..4 {
        rig.controller.send_event(event(at)).unwrap();
    }
    assert_eq!(
        rig.controller.send_event(event(4)),
        Err(SendEventError::QueueFull(event(4)))
    );
    // The swap applied, the poll after the period sends them; the queue is full in its turn.
    rig.play(&[64]);
    assert_eq!(
        rig.controller.send_event(event(64)),
        Err(SendEventError::QueueFull(event(64)))
    );
    // The processor takes the queued events at its next period, which makes room.
    rig.play(&[64]);
    rig.controller.send_event(event(128)).unwrap();
}
