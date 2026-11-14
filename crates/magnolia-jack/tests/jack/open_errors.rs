//! Errors on open and on replacing a processor: without a server, the backend says so and what to
//! do, and never starts one; a server at another rate, or a replacement that does not match the
//! client, is refused.

use magnolia_jack::{JackBackend, JackOptions, OpenError, ReplaceError};

use crate::common::{Server, client_name, refuse_pipewire, step, unique_server_name};
use crate::support::{Rig, a_process_names, new_engine, one_at_a_time};

#[test]
fn no_server_running_gives_the_no_server_error_and_never_starts_one() {
    let _turn = one_at_a_time();
    refuse_pipewire();
    let name = unique_server_name();
    let (_controller, processor) = new_engine(0, 2);
    let opened = JackBackend::open(JackOptions {
        client_name: client_name("magnolia"),
        server_name: Some(name.clone()),
        processor,
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

#[test]
fn a_server_at_another_rate_gives_the_rate_error() {
    let _turn = one_at_a_time();
    let server = Server::start_at(44_100, 256);
    let (_controller, processor) = new_engine(0, 2);
    step("opening Magnolia's client, which closes it on the rate's error");
    let opened = JackBackend::open(JackOptions {
        client_name: client_name("magnolia"),
        server_name: Some(server.name.clone()),
        processor,
    });
    let Err(error) = opened else {
        panic!("a 48 kHz engine opened on a 44.1 kHz server");
    };
    assert!(
        matches!(
            error,
            OpenError::RateDiffers {
                server: 44_100,
                engine: 48_000
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_replacement_with_other_channels_is_given_back_unused() {
    let _turn = one_at_a_time();
    let server = Server::start(256);
    let mut rig = Rig::open(&server, 0, 2);
    let (_other_controller, other) = new_engine(0, 1);
    let Err(error) = rig.backend.replace_processor(other) else {
        panic!("a processor of one channel replaced one of two");
    };
    assert!(matches!(error, ReplaceError::Differs(_)), "{error:?}");
    assert_eq!(error.into_processor().playback_channels(), 1);
}
