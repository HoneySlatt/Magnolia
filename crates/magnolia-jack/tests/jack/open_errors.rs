//! Errors on open: without a server, the backend says so and what to do, and never starts one.

use magnolia_jack::{JackBackend, JackOptions, OpenError};

use crate::common::{refuse_pipewire, unique_server_name};
use crate::support::{a_process_names, new_engine, one_at_a_time, rate};

#[test]
fn no_server_running_gives_the_no_server_error_and_never_starts_one() {
    let _turn = one_at_a_time();
    refuse_pipewire();
    let name = unique_server_name();
    let (_controller, processor) = new_engine(0, 2);
    let opened = JackBackend::open(JackOptions {
        client_name: "magnolia".to_owned(),
        server_name: Some(name.clone()),
        processor,
        sample_rate: rate(),
        capture_channels: 0,
        playback_channels: 2,
    });
    let Err(error) = opened else {
        panic!("a backend opened without a server");
    };
    assert!(
        matches!(&error, OpenError::NoServer { server_name } if server_name.as_deref() == Some(name.as_str())),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(text.contains("no JACK server is running") && text.contains(&name));
    assert!(text.contains("never starts a server"));
    assert!(!a_process_names(&name), "a server was started under {name}");
}
