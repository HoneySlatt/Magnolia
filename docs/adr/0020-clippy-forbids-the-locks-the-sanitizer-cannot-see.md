# 0020. Clippy forbids the locks the sanitizer cannot see

- Status: Accepted
- Date: 2026-11-11

## Context

The real-time path never allocates, locks or blocks ([ADR-0007](0007-the-audio-engine.md)).
RealtimeSanitizer checks it in CI: it ends the process at the first allocation, free or system call
inside a function marked real-time.

Measured on the pinned nightly in M1.1, it misses three things
([ARCHITECTURE](../ARCHITECTURE.md#checking-the-real-time-path)):

- an uncontended `std::sync::Mutex` or `RwLock`. On Linux, std's locks call the futex directly,
  without the C library, so the sanitizer sees them only when a lock is contended and waits;
- `std::thread::sleep`, which calls `clock_nanosleep`, a call the sanitizer does not intercept;
- the same for every std type built on those futexes: `Condvar`, `Barrier`, `Once`, `OnceLock`
  and `LazyLock` while another thread initialises them, the channels of `std::sync::mpsc`, and
  `thread::park`.

A lock that is never contended in a test can be contended on a user's machine, where it stalls the
audio thread behind a thread of normal priority. A clean run of the sanitizer therefore does not
prove that the real-time path takes no lock.

## Decision

The owner's choice of 2026-11-11.

**Clippy forbids std's blocking types and calls in the engine's crates.** `clippy.toml` lists them
under `disallowed-types` (`Mutex`, `RwLock`, `Condvar`, `Barrier`, `Once`, `OnceLock`, `LazyLock`,
the channels of `mpsc`) and `disallowed-methods` (`thread::sleep`, `thread::park`,
`thread::park_timeout`, `JoinHandle::join`). `scripts/check.sh` runs Clippy with warnings as errors,
so a use fails CI.

**A use off the real-time path is allowed by an `#[expect(clippy::disallowed_types)]` or
`#[expect(clippy::disallowed_methods)]`** on the smallest item that needs it, with a `reason` that
says which thread runs it and why it never runs on the real-time path. Each one is read at review.

**The file sits at the workspace's root** while every crate is the engine's. A crate that never
runs on the real-time path, such as the interface, gets a `clippy.toml` of its own when it comes.

## Alternatives considered

- **A crate of its own for real-time code, with a stricter `clippy.toml`**: no exceptions at all,
  but it splits the engine into crates before anything needs it, and code that runs on both sides,
  such as a node's constructor, would have to be cut apart.
- **A rule checked at review alone**: nothing to set up, but no machine checks it; the gap stays.
- **A lock checker at run time** (an interposed futex, a sanitizer of our own): sees dependencies'
  code too, but costs a tool to build and keep, for what a lint finds before the code runs.

## Consequences

- Clippy reads only Magnolia's own code. A dependency that locks on the real-time path stays
  unseen: the `jack` crate takes a `Mutex` with `try_lock` in its process callback, which never
  waits; when it is busy, as while the client closes or its buffer-size callback runs, the crate
  skips the period and asks the server to stop calling it. Each dependency on the real-time path is
  read for this before it is taken.
- The lint forbids a type everywhere in the crate, not only in real-time functions, so the threads
  that may block, such as the collector, carry exceptions in plain view.
- RealtimeSanitizer still catches the rest at run time: allocations, frees, and system calls that go
  through the C library, a contended lock's wait among them.
