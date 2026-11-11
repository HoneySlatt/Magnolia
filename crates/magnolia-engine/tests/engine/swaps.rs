//! A graph swap makes no click: every sample played across a crossfade and a dip equals, within
//! 1e-6, what the fade's formula and the sines give, computed here apart from the engine. Each
//! swap lands at every offset of a block, at periods from 32 frames to 2,048, above the engine's
//! largest block. A node inserted or removed in a crossfade keeps the level. A hard switch played
//! through the engine, without fades, shows the step the fades remove.

use magnolia_engine::engine::{Commit, Destination, Source};
use magnolia_engine::nodes::{Gain, Sine};

use crate::support::{
    FADE, Rig, Switch, assert_matches, fade_in, fade_out, largest_step, rate, sine,
};

/// The periods each swap is played at.
const PERIODS: [usize; 6] = [32, 64, 100, 128, 1_024, 2_048];

/// How far a sample may stray from its oracle.
const TOLERANCE: f64 = 1e-6;

/// The swaps' two sines.
const FIRST: (f64, f64) = (440.0, 0.5);
const SECOND: (f64, f64) = (660.0, 0.4);
const THIRD: (f64, f64) = (550.0, 0.3);

/// The largest step a crossfade of the first two sines can make: each sine's own steepest step,
/// plus the fade's steepest step, π / 2 per fade's length, across their largest gap.
fn crossfade_step_bound() -> f64 {
    let steepest = |(frequency, gain): (f64, f64)| {
        gain * std::f64::consts::TAU * frequency / f64::from(crate::support::RATE)
    };
    let fade = std::f64::consts::PI / (2.0 * FADE as f64);
    steepest(FIRST) + steepest(SECOND) + (FIRST.1 + SECOND.1) * fade
}

/// Plays a period of `period` frames, then `offset` frames, so that a swap committed next lands
/// at `offset` within the second block; returns what was played and where the swap lands.
fn play_up_to_offset(rig: &mut Rig, period: usize, offset: usize) -> (Vec<f32>, usize) {
    let periods: Vec<usize> = [period, offset]
        .into_iter()
        .filter(|&frames| frames > 0)
        .collect();
    (rig.play(&periods), period + offset)
}

/// Periods that finish the block the swap landed in, then cover `length` samples past it.
fn periods_after(period: usize, offset: usize, length: usize) -> Vec<usize> {
    let mut periods = vec![period - offset];
    while periods.iter().sum::<usize>() < length + period {
        periods.push(period);
    }
    periods
}

#[test]
fn a_crossfade_between_two_sines_follows_the_fade_at_every_period_and_offset() {
    for period in PERIODS {
        for offset in 0..period {
            let mut rig = Rig::new(0, 1);
            let first = rig.add(Sine::new(rate(), FIRST.0, FIRST.1 as f32));
            let second = rig.add(Sine::new(rate(), SECOND.0, SECOND.1 as f32));
            rig.controller
                .connect(Source::Node(first, 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            let (mut played, swap) = play_up_to_offset(&mut rig, period, offset);

            rig.controller
                .disconnect(Source::Node(first, 0), Destination::Playback(0))
                .unwrap();
            rig.controller
                .connect(Source::Node(second, 0), Destination::Playback(0));
            assert_eq!(
                rig.controller.commit().unwrap(),
                Commit::Sent { sequence: 2 }
            );
            played.extend(rig.play(&periods_after(period, offset, FADE)));

            let expected: Vec<f64> = (0..played.len())
                .map(|n| {
                    let a = sine(n, FIRST.0, FIRST.1);
                    let b = sine(n, SECOND.0, SECOND.1);
                    match n.checked_sub(swap) {
                        None => a,
                        Some(i) if i < FADE => a * fade_out(i, FADE) + b * fade_in(i, FADE),
                        Some(_) => b,
                    }
                })
                .collect();
            let what = format!("crossfade, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
            let played: Vec<f64> = played.iter().map(|&sample| f64::from(sample)).collect();
            assert!(largest_step(&played) <= crossfade_step_bound(), "{what}");
        }
    }
}

#[test]
fn a_hard_switch_through_the_engine_steps_far_more_than_a_crossfade() {
    // The same two sines, switched at the same samples as the crossfades above, by a node that
    // passes one input, then the other, without a fade.
    let mut largest_hard_step: f64 = 0.0;
    for period in PERIODS {
        for offset in (0..period).step_by((period / 16).max(1)) {
            let mut rig = Rig::new(0, 1);
            let first = rig.add(Sine::new(rate(), FIRST.0, FIRST.1 as f32));
            let second = rig.add(Sine::new(rate(), SECOND.0, SECOND.1 as f32));
            let switch = rig.add(Switch::new());
            for (input, sine) in [first, second].into_iter().enumerate() {
                rig.controller
                    .connect(Source::Node(sine, 0), Destination::Node(switch, input));
            }
            rig.controller
                .connect(Source::Node(switch, 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            let mut played = rig.play(&[period]);
            let swap = period + offset;
            rig.send(switch, swap as i64, 0, 1.0);
            played.extend(rig.play(&[period, period]));

            let expected: Vec<f64> = (0..played.len())
                .map(|n| {
                    let (frequency, gain) = if n < swap { FIRST } else { SECOND };
                    sine(n, frequency, gain)
                })
                .collect();
            let what = format!("hard switch, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
            let played: Vec<f64> = played.iter().map(|&sample| f64::from(sample)).collect();
            largest_hard_step = largest_hard_step.max(largest_step(&played));
        }
    }
    // Without a fade, the switch steps by most of both sines' amplitudes.
    assert!(
        largest_hard_step > 4.0 * crossfade_step_bound(),
        "the hard switch stepped by {largest_hard_step} at most"
    );
}

/// A sine at 440 Hz on the playback channel, with a gain of 1 inserted in its path, or removed
/// from it, by a swap that lands at `offset` of a block of `period` frames. Returns what was
/// played.
fn insert_or_remove_a_unity_gain(period: usize, offset: usize, insert: bool) -> Vec<f32> {
    let mut rig = Rig::new(0, 1);
    let source = rig.add(Sine::new(rate(), FIRST.0, FIRST.1 as f32));
    let direct = (Source::Node(source, 0), Destination::Playback(0));
    let through = |rig: &mut Rig| {
        let gain = rig.add(Gain::new(1, 1.0));
        rig.controller
            .connect(Source::Node(source, 0), Destination::Node(gain, 0));
        rig.controller
            .connect(Source::Node(gain, 0), Destination::Playback(0));
        gain
    };
    let gain = if insert {
        rig.controller.connect(direct.0, direct.1);
        None
    } else {
        Some(through(&mut rig))
    };
    rig.controller.commit().unwrap();
    let (mut played, _) = play_up_to_offset(&mut rig, period, offset);

    match gain {
        None => {
            rig.controller.disconnect(direct.0, direct.1).unwrap();
            through(&mut rig);
        }
        Some(gain) => {
            rig.controller.remove_node(gain).unwrap();
            rig.controller.connect(direct.0, direct.1);
        }
    }
    assert_eq!(
        rig.controller.commit().unwrap(),
        Commit::Sent { sequence: 2 }
    );
    played.extend(rig.play(&periods_after(period, offset, FADE)));
    played
}

#[test]
fn inserting_a_unity_gain_in_a_crossfade_keeps_the_level_at_every_period_and_offset() {
    // The direct edge fades out while the edge into the new gain fades in; the gain's own output
    // is constant, so its path carries one fade.
    for period in PERIODS {
        for offset in 0..period {
            let played = insert_or_remove_a_unity_gain(period, offset, true);
            let expected: Vec<f64> = (0..played.len())
                .map(|n| sine(n, FIRST.0, FIRST.1))
                .collect();
            let what = format!("insert, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
        }
    }
}

#[test]
fn removing_a_unity_gain_in_a_crossfade_keeps_the_level_at_every_period_and_offset() {
    // The gain's output fades out while the direct edge fades in; the edge into the dropped gain
    // is constant, so its path carries one fade.
    for period in PERIODS {
        for offset in 0..period {
            let played = insert_or_remove_a_unity_gain(period, offset, false);
            let expected: Vec<f64> = (0..played.len())
                .map(|n| sine(n, FIRST.0, FIRST.1))
                .collect();
            let what = format!("remove, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
        }
    }
}

#[test]
fn a_reordered_insert_dips_through_silence_without_a_gap_at_every_period_and_offset() {
    // A sine through two gains, A -> X -> Y -> out, reordered to A -> Y -> X -> out. The union
    // of both would hold a cycle, so the swap dips: each phase fades three edges in series.
    let (x_gain, y_gain) = (0.5, 0.8);
    let through = x_gain * y_gain;
    for period in PERIODS {
        for offset in 0..period {
            let mut rig = Rig::new(0, 1);
            let a = rig.add(Sine::new(rate(), FIRST.0, FIRST.1 as f32));
            let x = rig.add(Gain::new(1, x_gain as f32));
            let y = rig.add(Gain::new(1, y_gain as f32));
            let chain = |rig: &mut Rig, middle: [_; 2], connect: bool| {
                let ends = [
                    (Source::Node(a, 0), Destination::Node(middle[0], 0)),
                    (Source::Node(middle[0], 0), Destination::Node(middle[1], 0)),
                    (Source::Node(middle[1], 0), Destination::Playback(0)),
                ];
                for (source, destination) in ends {
                    if connect {
                        rig.controller.connect(source, destination);
                    } else {
                        rig.controller.disconnect(source, destination).unwrap();
                    }
                }
            };
            chain(&mut rig, [x, y], true);
            rig.controller.commit().unwrap();
            let (mut played, swap) = play_up_to_offset(&mut rig, period, offset);

            chain(&mut rig, [x, y], false);
            chain(&mut rig, [y, x], true);
            assert_eq!(
                rig.controller.commit().unwrap(),
                Commit::Sent { sequence: 2 }
            );
            played.extend(rig.play(&periods_after(period, offset, 2 * FADE)));

            let expected: Vec<f64> = (0..played.len())
                .map(|n| {
                    let source = through * sine(n, FIRST.0, FIRST.1);
                    match n.checked_sub(swap) {
                        None => source,
                        Some(i) if i < FADE => source * fade_out(i, FADE).powi(3),
                        Some(i) if i < 2 * FADE => source * fade_in(i - FADE, FADE).powi(3),
                        Some(_) => source,
                    }
                })
                .collect();
            let what = format!("dip, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
        }
    }
}

#[test]
fn a_dips_two_fades_meet_at_one_sample_at_every_period_and_offset() {
    // Four constant nodes replaced by four others: the union would hold twice the nodes, so the
    // swap dips. Each phase fades one edge, so the fades meet at about 1e-5 of the signal: one
    // sample of silence between them would stand out against the tolerance.
    for period in PERIODS {
        for offset in 0..period {
            let mut rig = Rig::new(0, 1);
            let old: Vec<_> = (0..4)
                .map(|_| rig.add(crate::support::Constant::new(1.0)))
                .collect();
            rig.controller
                .connect(Source::Node(old[0], 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            let (mut played, swap) = play_up_to_offset(&mut rig, period, offset);

            for &node in &old {
                rig.controller.remove_node(node).unwrap();
            }
            let new: Vec<_> = (0..4)
                .map(|_| rig.add(crate::support::Constant::new(1.0)))
                .collect();
            rig.controller
                .connect(Source::Node(new[0], 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            played.extend(rig.play(&periods_after(period, offset, 2 * FADE)));

            let expected: Vec<f64> = (0..played.len())
                .map(|n| match n.checked_sub(swap) {
                    None => 1.0,
                    Some(i) if i < FADE => fade_out(i, FADE),
                    Some(i) if i < 2 * FADE => fade_in(i - FADE, FADE),
                    Some(_) => 1.0,
                })
                .collect();
            let what = format!("dip's gap, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
            let silent = played.iter().position(|&sample| sample == 0.0);
            assert_eq!(silent, None, "{what}: a sample of silence");
        }
    }
}

#[test]
fn a_swap_committed_during_a_fade_waits_for_it_then_poll_sends_it() {
    for period in PERIODS {
        for offset in [0, 1, period / 3, period - 1] {
            let mut rig = Rig::new(0, 1);
            let sines: Vec<_> = [FIRST, SECOND, THIRD]
                .into_iter()
                .map(|(frequency, gain)| rig.add(Sine::new(rate(), frequency, gain as f32)))
                .collect();
            rig.controller
                .connect(Source::Node(sines[0], 0), Destination::Playback(0));
            rig.controller.commit().unwrap();
            let (mut played, first_swap) = play_up_to_offset(&mut rig, period, offset);

            let swap_to = |rig: &mut Rig, from: usize, to: usize| {
                rig.controller
                    .disconnect(Source::Node(sines[from], 0), Destination::Playback(0))
                    .unwrap();
                rig.controller
                    .connect(Source::Node(sines[to], 0), Destination::Playback(0));
                rig.controller.commit().unwrap()
            };
            assert_eq!(swap_to(&mut rig, 0, 1), Commit::Sent { sequence: 2 });
            assert_eq!(swap_to(&mut rig, 1, 2), Commit::Deferred);

            // The first swap is steady in the period that plays its fade's last sample; the
            // poll after that period sends the second, which lands at the next period's start.
            let mut periods = vec![period - offset];
            while first_swap + periods.iter().sum::<usize>() < first_swap + FADE {
                periods.push(period);
            }
            let second_swap = first_swap + periods.iter().sum::<usize>();
            let mut landed_at = None;
            for frames in periods.iter().copied().chain(std::iter::repeat(period)) {
                let stats = rig.controller.stats().clone();
                played.extend(rig.period(frames));
                if stats.last_swap_applied() == 3 && landed_at.is_none() {
                    landed_at = Some(stats.block_start().0 as usize);
                }
                rig.controller.poll();
                if played.len() >= second_swap + FADE + period {
                    break;
                }
            }
            assert_eq!(
                landed_at,
                Some(second_swap),
                "period {period}, offset {offset}"
            );

            let expected: Vec<f64> = (0..played.len())
                .map(|n| {
                    let [a, b, c] =
                        [FIRST, SECOND, THIRD].map(|(frequency, gain)| sine(n, frequency, gain));
                    if n < first_swap {
                        a
                    } else if n < first_swap + FADE {
                        let i = n - first_swap;
                        a * fade_out(i, FADE) + b * fade_in(i, FADE)
                    } else if n < second_swap {
                        b
                    } else if n < second_swap + FADE {
                        let i = n - second_swap;
                        b * fade_out(i, FADE) + c * fade_in(i, FADE)
                    } else {
                        c
                    }
                })
                .collect();
            let what = format!("two swaps, period {period}, offset {offset}");
            assert_matches(&played, &expected, TOLERANCE, &what);
        }
    }
}
