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
| `magnolia-engine` | The audio engine ([ADR-0007](adr/0007-the-audio-engine.md)). Empty so far; its one test proves that RealtimeSanitizer stops a real-time function that allocates. | nothing |

## Checking the real-time path

The engine's real-time functions will be marked `#[sanitize(realtime = "nonblocking")]` under the
crate's `realtime-sanitizer` feature. With that feature, the nightly toolchain and
`-Zsanitizer=realtime`, RealtimeSanitizer ends the process at the first allocation, lock or
blocking call made inside a marked function, on any thread
([ADR-0013](adr/0013-a-pinned-stable-toolchain-and-a-nightly-for-checks.md)).

Because the sanitizer ends the process, a test of it runs each case in a child process and reads
its exit status and standard error: `crates/magnolia-engine/tests/realtime_sanitizer.rs`.
