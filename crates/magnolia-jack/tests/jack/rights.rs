//! Missing real-time rights are reported: on a server started without real time, the audio
//! thread runs at normal priority, and the report says so, names JACK2 and says how to grant the
//! rights. The thread's policy is asked of the kernel, by the id the thread-init callback read.

use magnolia_jack::{AudioServer, ThreadScheduling};

use crate::common::Server;
use crate::support::{Rig, is_thread_of_this_process, one_at_a_time, this_thread_id};

#[test]
fn a_server_without_real_time_gives_a_report_that_names_jack2_and_says_how_to_grant_it() {
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let rig = Rig::open(&server, 0, 2);
    rig.wait_periods(4);

    let audio_thread = rig.controller.stats().audio_thread_id();
    assert_ne!(audio_thread, 0, "the audio thread's id was published");
    assert_ne!(audio_thread, this_thread_id());
    assert!(is_thread_of_this_process(audio_thread));

    let status = rig.backend.realtime_status();
    assert!(
        matches!(status.thread, ThreadScheduling::NotRealTime { policy } if policy == libc::SCHED_OTHER),
        "{status:?}"
    );
    assert_eq!(status.server, AudioServer::Jack2);
    assert_eq!(status.server_runs_in_real_time, Some(false));
    assert!(status.library.is_some(), "the loaded libjack is named");
    let report = status.report();
    for words in [
        "does not run in real time",
        "under JACK2",
        "jackd -R",
        "RLIMIT_RTPRIO",
        "audio",
    ] {
        assert!(report.contains(words), "{words:?} missing from: {report}");
    }
    assert_eq!(status.to_string(), report);
}
