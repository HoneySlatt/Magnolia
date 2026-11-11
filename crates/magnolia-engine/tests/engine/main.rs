//! The engine's behaviours, each played through the device-less backend: graph swaps without a
//! click against an oracle, delay compensation, sample-accurate events, what the processor does
//! with each node's status, the collector, page faults during a swap, the controller's checks and
//! the engine's statistics.

mod collector;
mod compensation;
mod controller;
mod device;
mod events;
mod nodes;
mod page_faults;
mod stats;
mod statuses;
mod support;
mod swaps;
