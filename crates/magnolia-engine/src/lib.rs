//! Magnolia's audio engine: the real-time path that plays, records and monitors
//! ([ADR-0007](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0007-the-audio-engine.md)).
//!
//! - [`engine`]: a graph of [`node`]s, edited and compiled by the controller, swapped without a
//!   click, played by the processor on the real-time thread, freed by the collector;
//! - [`nodes`]: the engine's own nodes, a sine, a gain and a test latency;
//! - [`device`] and [`offline`]: what a backend lends the processor each period, and a backend
//!   without a device, for tests;
//! - [`stats`] and [`callback_times`]: what the engine reports of itself while it plays;
//! - [`time`]: where every position lies, exactly.
//!
//! Its functions marked real-time are checked by RealtimeSanitizer in CI
//! (`tests/realtime_sanitizer.rs`).

// The sanitizer's attribute compiles only on the nightly, behind this cfg (ADR-0019).
#![cfg_attr(magnolia_rtsan, feature(sanitize))]

pub mod callback_times;
pub mod device;
pub mod engine;
pub mod node;
pub mod nodes;
pub mod offline;
pub mod stats;
pub mod time;
