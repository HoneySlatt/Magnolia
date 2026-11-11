//! The command line run as a user runs it: `magnolia render` writes the file the library renders,
//! `magnolia play` plays on a JACK2 server of the test's own and prints its figures, and every
//! misuse ends with a plain error and a non-zero exit, never a panic.
//!
//! The tests of `play` start their own `jackd` on the dummy driver, without real time and without
//! a sound card, through the JACK backend's own test server, and refuse to run when the libjack
//! they would load is PipeWire's, which would reach the owner's live PipeWire.

// The JACK backend's test server, shared rather than copied: how it starts, stops and cleans up
// after a JACK2 server is subtle (CONTRIBUTING.md, "The JACK backend's tests").
#[path = "../../../magnolia-jack/tests/common/mod.rs"]
mod common;
mod play;
mod render;
mod support;
