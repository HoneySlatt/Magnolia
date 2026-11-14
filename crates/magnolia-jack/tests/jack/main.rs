//! The JACK backend's behaviours, each on a JACK2 server of the test's own, on the dummy driver,
//! without real time and without a sound card: duplex audio at a constant latency, a swap without
//! a click, the report of missing real-time rights, the counters of overruns, gaps and xruns, a
//! panic's silence and the processor rebuilt in place, a server killed under the backend, the
//! error of a missing server, the test session played to the bit what it renders offline, the
//! device's latencies, a monitoring path that adds nothing to a direct connection, and the
//! watchdog that ends a test stuck in a JACK call.
//!
//! Each test starts its own `jackd` under a name of its own, one test at a time, and stops it by its
//! pid at the end, on a panic too. A watchdog bounds each test, since a JACK call never times out
//! (`common::one_at_a_time`). Each refuses to run when the libjack it would load is
//! PipeWire's, which would reach the owner's live PipeWire. The tests that need JACK's processing
//! path and not its timing run their server in freewheel, as fast as the clients go
//! (`support::Freewheel`); those that need its timing or its `system:` ports run in real time.

#[path = "../common/mod.rs"]
mod common;
mod counters;
mod duplex;
mod latency;
mod open_errors;
mod panics;
mod rights;
mod round_trip;
mod session;
mod shutdown;
mod support;
mod swaps;
mod watchdog;
