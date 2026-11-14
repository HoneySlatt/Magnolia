//! Magnolia's JACK backend: plays the engine's
//! [`Processor`](magnolia_engine::engine::Processor) through the JACK API, which PipeWire and
//! JACK2 both serve
//! ([ADR-0007](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0007-the-audio-engine.md)).
//!
//! [`JackBackend::open`] opens a client on a running server, with one port per engine channel, and
//! the server calls the processor once a period, capture and playback in one callback. The
//! backend counts its own overruns, gaps in the server's frame time and the server's xruns in the
//! engine's statistics; [`JackBackend::realtime_status`] says whether the audio thread runs in real
//! time, and how to grant it the rights when it does not; [`JackBackend::replace_processor`]
//! rebuilds a poisoned engine without closing the client. A latency callback reads the device's
//! latencies, [`JackBackend::device_latency`], and shows other clients the latency of Magnolia's
//! monitoring path, [`JackBackend::set_monitoring_latency`].
//!
//! libjack is loaded when the first client opens, so a program that never opens one runs without
//! it. Run through `pw-jack`, the backend plays on PipeWire; otherwise on whichever libjack the
//! library path finds first.
//!
//! ```no_run
//! use magnolia_engine::engine::{self, Destination, EngineConfig, Source};
//! use magnolia_engine::nodes::Sine;
//! use magnolia_engine::time::SampleRate;
//! use magnolia_jack::{JackBackend, JackOptions};
//!
//! let rate = SampleRate::new(48_000).unwrap();
//! let (mut controller, processor) = engine::new(EngineConfig::new(rate, 1_024, 0, 2)).unwrap();
//! let sine = controller.add_node(Box::new(Sine::new(rate, 440.0, 0.1))).unwrap();
//! controller.connect(Source::Node(sine, 0), Destination::Playback(0));
//! controller.commit().unwrap();
//!
//! let backend = JackBackend::open(JackOptions {
//!     client_name: "magnolia".to_owned(),
//!     server_name: None,
//!     processor,
//! })?;
//! println!("{}", backend.realtime_status());
//! let processor = backend.close()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// The sanitizer's attribute compiles only on the nightly, behind this cfg (ADR-0019).
#![cfg_attr(magnolia_rtsan, feature(sanitize))]

mod backend;
mod callback;
mod latency;
mod notifications;
mod realtime;

pub use backend::{
    CloseError, ConnectError, JackBackend, JackOptions, LatencyError, OpenError, ReplaceError,
};
pub use realtime::{AudioServer, RealtimePolicy, RealtimeStatus, ThreadScheduling};
