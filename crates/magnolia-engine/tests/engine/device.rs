//! The device's buffers: capture reaches the graph, and a buffer shorter than the period is never
//! a panic: silence on capture, left as it is on playback.

use magnolia_engine::device::DeviceBuffers;
use magnolia_engine::engine::{self, Destination, EngineConfig, Source};

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
