//! The backend's counters: a node that busy-waits past the period's deadline makes the backend
//! count its own overruns; JACK2 then skips the client's next cycles, which shows as gaps in the
//! frame time; and the server reports xruns. These need the server's timing: they run in real time.
//! A freewheeling server has no deadline, and the backend counts nothing there. The gap arithmetic,
//! its wrap included, is tested in `src/callback.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use magnolia_engine::engine::{Destination, Source};
use magnolia_engine::nodes::Sine;

use crate::common::Server;
use crate::support::{Freewheel, Rig, Slow, one_at_a_time, rate, wait_for};

/// The server's period: 5.3 ms at 48 kHz.
const PERIOD: u32 = 256;

#[test]
fn a_node_past_the_deadline_counts_overruns_gaps_and_xruns() {
    let _turn = one_at_a_time();
    let server = Server::start(PERIOD);
    let mut rig = Rig::open(&server, 0, 1);
    let slow_calls = Arc::new(AtomicU32::new(0));
    let slow = rig.add(Slow {
        spin: Duration::from_millis(12),
        slow_calls: Arc::clone(&slow_calls),
    });
    rig.controller
        .connect(Source::Node(slow, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    rig.wait_periods(20);
    let stats = Arc::clone(rig.controller.stats());
    let (overruns, gaps, xruns) = (
        stats.own_overruns(),
        stats.frame_time_gaps(),
        stats.server_xruns(),
    );

    slow_calls.store(20, Ordering::Relaxed);
    wait_for("the slow calls", Duration::from_secs(10), || {
        slow_calls.load(Ordering::Relaxed) == 0
    });
    rig.wait_periods(20);
    assert!(
        stats.own_overruns() >= overruns + 10,
        "own overruns: {} before, {} after",
        overruns,
        stats.own_overruns()
    );
    assert!(
        stats.frame_time_gaps() > gaps,
        "gaps: {} before, {} after",
        gaps,
        stats.frame_time_gaps()
    );
    assert!(
        stats.server_xruns() > xruns,
        "server xruns: {} before, {} after",
        xruns,
        stats.server_xruns()
    );
    assert!(!rig.backend.server_shut_down());
}

/// Periods played in freewheel, once under way: 27 s at 64 frames, a fraction of a second in
/// freewheel.
const FREEWHEEL_PERIODS: u64 = 20_000;

#[test]
fn a_freewheeling_server_counts_no_overrun_gap_or_xrun() {
    let _turn = one_at_a_time();
    let server = Server::start(64);
    let mut rig = Rig::open(&server, 0, 1);
    let sine = rig.add(Sine::new(rate(), 440.0, 0.1));
    rig.controller
        .connect(Source::Node(sine, 0), Destination::Playback(0));
    rig.controller.commit().unwrap();
    rig.wait_until_steady();
    let stats = Arc::clone(rig.controller.stats());
    let counters = || {
        (
            stats.own_overruns(),
            stats.frame_time_gaps(),
            stats.server_xruns(),
        )
    };

    // Read once freewheel is under way: before it, a server without real time on a busy machine
    // may skip cycles and report xruns, whose notifications arrive a little later.
    let freewheel = Freewheel::start(&server);
    rig.wait_periods(5_000);
    let under_way = counters();
    // Without a deadline, the times of a freewheeling server's cycles said, before the backend
    // knew of freewheel, that thousands of its periods overran.
    rig.wait_periods(FREEWHEEL_PERIODS);
    let freewheeling = counters();
    freewheel.stop();
    // The backend plays on once the server is back in real time.
    rig.wait_periods(20);
    eprintln!(
        "own overruns, gaps and xruns: {under_way:?} once freewheel was under way, \
         {freewheeling:?} {FREEWHEEL_PERIODS} periods later, {:?} back in real time",
        counters()
    );
    assert_eq!(freewheeling, under_way, "in freewheel");
    assert!(!rig.backend.server_shut_down());
}
