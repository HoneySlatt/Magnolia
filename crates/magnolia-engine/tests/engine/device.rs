//! The device's buffers: capture reaches the graph, a buffer shorter than the period is never a
//! panic (silence on capture, left as it is on playback), and silence reaches the device as +0.0.

use magnolia_engine::device::DeviceBuffers;
use magnolia_engine::engine::{self, Destination, EngineConfig, Source};
use magnolia_engine::node::{Node, ProcessContext, ProcessStatus};
use magnolia_engine::nodes::Gain;

use crate::support::{Constant, LARGEST_BLOCK, Rig, assert_matches, rate};

#[test]
fn capture_reaches_playback_and_a_short_capture_buffer_is_silence() {
    let mut rig = Rig::new(2, 1);
    rig.controller
        .connect(Source::Capture(0), Destination::Playback(0));
    rig.controller
        .connect(Source::Capture(1), Destination::Playback(0));
    rig.controller.commit().unwrap();
    let ramp: Vec<f32> = (0..64).map(|n| n as f32 / 64.0).collect();
    let short = [1.0; 63];
    let played = rig.backend.play_period(64, &[&ramp, &short])[0].clone();
    let expected: Vec<f64> = ramp.iter().map(|&sample| f64::from(sample)).collect();
    assert_matches(&played, &expected, 0.0, "capture");
}

/// A device whose second playback buffer is one sample short.
struct ShortPlayback {
    playback: [Vec<f32>; 2],
}

impl DeviceBuffers for ShortPlayback {
    fn frames(&self) -> usize {
        64
    }

    fn capture_channels(&self) -> usize {
        0
    }

    fn playback_channels(&self) -> usize {
        2
    }

    fn capture(&self, _channel: usize) -> &[f32] {
        &[]
    }

    fn playback(&mut self, channel: usize) -> &mut [f32] {
        self.playback
            .get_mut(channel)
            .map(Vec::as_mut_slice)
            .unwrap_or_default()
    }
}

#[test]
fn a_short_playback_buffer_is_left_as_it_is() {
    let config = EngineConfig::new(rate(), LARGEST_BLOCK, 0, 2);
    let (mut controller, mut processor) = engine::new(config).unwrap();
    let one = controller.add_node(Box::new(Constant::new(1.0))).unwrap();
    for channel in 0..2 {
        controller.connect(Source::Node(one, 0), Destination::Playback(channel));
    }
    controller.commit().unwrap();
    let mut device = ShortPlayback {
        playback: [vec![0.5; 64], vec![0.5; 63]],
    };
    processor.process(&mut device);
    assert_eq!(device.playback[0], [1.0; 64]);
    assert_eq!(device.playback[1], [0.5; 63]);
}

/// One output of -0.0, which it does not flag silent, as a node that multiplies silence by a
/// negative gain would.
struct NegativeZero;

impl Node for NegativeZero {
    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        1
    }

    fn process(&mut self, context: &mut ProcessContext<'_>) -> ProcessStatus {
        context.output_mut(0).fill(-0.0);
        ProcessStatus::Continue
    }
}

#[test]
fn a_negative_zero_reaches_the_device_as_positive_zero() {
    let mut rig = Rig::new(0, 2);
    let negative = rig.add(NegativeZero);
    let gain = rig.add(Gain::new(1, 1.0));
    rig.controller
        .connect(Source::Node(negative, 0), Destination::Playback(0));
    rig.controller
        .connect(Source::Node(negative, 0), Destination::Node(gain, 0));
    rig.controller
        .connect(Source::Node(gain, 0), Destination::Playback(1));
    rig.controller.commit().unwrap();
    for frames in [64, 2_048, 1] {
        for (channel, played) in rig.backend.play_period(frames, &[]).iter().enumerate() {
            assert_eq!(played.len(), frames);
            assert!(
                played
                    .iter()
                    .all(|sample| sample.to_bits() == 0.0_f32.to_bits()),
                "channel {channel}, a period of {frames}: {:?}",
                &played[..frames.min(4)]
            );
        }
    }
}
