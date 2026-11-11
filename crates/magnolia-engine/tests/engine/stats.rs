//! The engine's statistics follow its periods: the heartbeat counts them, the block's start
//! advances by each period's frames, and the histogram holds one time per period.

use crate::support::Rig;

#[test]
fn the_heartbeat_block_start_and_histogram_follow_each_period() {
    let mut rig = Rig::new(0, 1);
    rig.controller.commit().unwrap();
    let stats = rig.controller.stats().clone();
    assert_eq!(stats.periods(), 0);
    assert_eq!(stats.callback_times.snapshot().count(), 0);
    let mut start = 0;
    for (index, frames) in [64, 100, 2_048, 1, 0, 37].into_iter().enumerate() {
        rig.play(&[frames]);
        let periods = index as u64 + 1;
        assert_eq!(stats.periods(), periods);
        assert_eq!(stats.block_start().0, start, "period {index}");
        assert_eq!(stats.callback_times.snapshot().count(), periods);
        start += frames as i64;
    }
    assert!(stats.callback_times.snapshot().maximum() > std::time::Duration::ZERO);
    assert_eq!(stats.missed_plugin_deadlines(), 0);
    assert_eq!(stats.last_swap_applied(), 1);
    assert_eq!(stats.last_swap_steady(), 1);
}

#[test]
fn the_backends_counters_start_at_zero_and_count() {
    let rig = Rig::new(0, 1);
    let stats = rig.backend.processor().stats();
    assert_eq!(
        (
            stats.own_overruns(),
            stats.frame_time_gaps(),
            stats.server_xruns(),
            stats.audio_thread_id()
        ),
        (0, 0, 0, 0)
    );
    stats.record_own_overrun();
    stats.record_frame_time_gap();
    stats.record_frame_time_gap();
    stats.record_server_xrun();
    stats.set_audio_thread_id(4_242);
    let seen = rig.controller.stats();
    assert_eq!(
        (
            seen.own_overruns(),
            seen.frame_time_gaps(),
            seen.server_xruns(),
            seen.audio_thread_id()
        ),
        (1, 2, 1, 4_242)
    );
}
