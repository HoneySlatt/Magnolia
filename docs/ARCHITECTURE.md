# Architecture

The parts of Magnolia's code, what each one does and what depends on what. It describes the code
as it is now; the design it grows into is in the [ADRs](adr/README.md), and what is built next in
[`PLAN.md`](PLAN.md).

## The workspace

One Cargo workspace (`Cargo.toml`). Every crate lives in `crates/` and is named `magnolia-<part>`.
A part gets a crate of its own when it has a boundary of its own: a process, a platform, or a
dependency the rest must not see. The interface depends on the engine and the project, never the
reverse ([`AGENTS.md`](../AGENTS.md#architecture-rules)).

| Crate | Role | Depends on |
|---|---|---|
| `magnolia-engine` | The audio engine ([ADR-0007](adr/0007-the-audio-engine.md)). So far it holds the time model, in its module `time`. | nothing |

Each crate's fuzz targets live in its `fuzz/` folder, a workspace of its own, so that the
workspace's builds never compile libFuzzer. Its benchmarks live in `benches/`, built on the standard
library alone.

## The time model

`magnolia_engine::time` says where every position lies
([ADR-0018](adr/0018-the-time-model.md)). It has four grids, each a type that implements `Grid`:

| Grid | Positions | What it measures |
|---|---|---|
| `SampleRate` | `SamplePosition` | samples, what the audio device plays |
| `TimeUnits` | `TimePosition` | absolute time in time units, whatever the sample rate |
| `TempoMap` | `MusicalPosition` | ticks, laid on time units by tempo changes from the song start |
| `TimecodeClock` | `FramePosition` | timecode frames, laid on time units from a start label |

Every conversion goes through a `Moment`, the exact instant of a position, computed with integers:

```text
position on grid A --moment_of--> Moment --first_at_or_after (place)--> position on grid B
                                         --last_at_or_before (read)---->
```

`place` and `read` are the only two ways to convert, and the only steps that round. No conversion
goes through a third grid, so a position's frame or bar never depends on the sample rate. The
engine calls `TempoMap::ticks_in_block` for each block, then places each event of those ticks on
its sample.

A tempo map is built off the real-time thread and never changes; its lookups are binary searches
that neither allocate nor lock. `tests/time_over_ten_hours.rs` checks the rule on every grid over
ten hours; `tests/common/mod.rs` holds the checks it shares with the fuzz targets.

## Checking the real-time path

The engine's real-time functions will be marked
`#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]`. Built on the nightly with
`-Zsanitizer=realtime --cfg magnolia_rtsan`, RealtimeSanitizer ends the process at the first
allocation, free or system call made inside a marked function, on any thread
([ADR-0013](adr/0013-a-pinned-stable-toolchain-and-a-nightly-for-checks.md),
[ADR-0019](adr/0019-the-sanitizers-nightly-code-behind-a-cfg.md)). Elsewhere the attribute is
not there.

Its limit, measured on the pinned nightly: an uncontended `std::sync::Mutex` or `RwLock` and
`thread::sleep` pass unreported. std's locks on Linux use a futex directly, which the sanitizer sees
only when a lock is contended and waits; `thread::sleep` calls `clock_nanosleep`, which it does not
intercept. A clean run therefore does not prove that the real-time path takes no lock. What covers
the gap is known debt in the [PLAN](PLAN.md#known-debt).

Because the sanitizer ends the process, a test of it runs each case in a child process and reads
its exit status and standard error: `crates/magnolia-engine/tests/realtime_sanitizer.rs`. One of its
cases runs a block's conversions of time inside a marked function, and must stay silent.
