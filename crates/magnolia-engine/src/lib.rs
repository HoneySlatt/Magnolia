//! Magnolia's audio engine: the real-time path that plays, records and monitors
//! ([ADR-0007](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0007-the-audio-engine.md)).
//!
//! - [`engine`]: a graph of [`node`]s, edited and compiled by the controller, swapped without a
//!   click, played by the processor on the real-time thread, freed by the collector; the transport,
//!   and the MIDI sequences that nodes play from the timeline;
//! - [`nodes`]: the engine's own nodes, a sine, a gain, a test latency and a test synth;
//! - [`disk`]: the disk reader, which streams audio clips from WAV files ahead of the playhead, and
//!   the audio players, the nodes that play those streams;
//! - [`record`]: the capture tap, which copies armed tracks' capture on the real-time thread, the
//!   recorder, which writes it into WAV takes that survive a crash, and their recovery;
//! - [`device`] and [`offline`]: what a backend lends the processor each period, and a backend
//!   without a device, for tests;
//! - [`stats`] and [`callback_times`]: what the engine reports of itself while it plays;
//! - [`time`]: where every position lies, exactly;
//! - [`wav`]: WAV files, read, decoded and written.
//!
//! Its functions marked real-time are checked by RealtimeSanitizer in CI
//! (`tests/realtime_sanitizer.rs`).

// The sanitizer's attribute compiles only on the nightly, behind this cfg (ADR-0019).
#![cfg_attr(magnolia_rtsan, feature(sanitize))]

pub mod callback_times;
pub mod device;
pub mod disk;
pub mod engine;
pub mod node;
pub mod nodes;
pub mod offline;
pub mod record;
mod seqlock;
pub mod stats;
pub mod time;
pub mod wav;
