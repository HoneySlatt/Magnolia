//! The JACK backend's behaviours, each on a JACK2 server of the test's own, on the dummy driver,
//! without real time and without a sound card: duplex audio at a constant latency, a swap without
//! a click, the report of missing real-time rights, the counters of overruns, gaps and xruns, a
//! panic's silence and the processor rebuilt in place, a server killed under the backend, and the
//! error of a missing server.
//!
//! Each test starts its own `jackd` under a name of its own, one test at a time, and stops it by its
//! pid at the end, on a panic too. Each refuses to run when the libjack it would load is
//! PipeWire's, which would reach the owner's live PipeWire.

#[path = "../common/mod.rs"]
mod common;
mod counters;
mod duplex;
mod open_errors;
mod panics;
mod rights;
mod shutdown;
mod support;
mod swaps;
