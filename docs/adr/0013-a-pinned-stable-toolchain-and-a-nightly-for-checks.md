# 0013. A pinned stable toolchain, and a nightly for checks only

- Status: Accepted; amended by 0019
- Date: 2026-11-07

## Context

Magnolia is written in stable Rust ([ROADMAP](../ROADMAP.md#fixed)). Three of its checks need a
nightly compiler: RealtimeSanitizer, which proves that the real-time path never allocates, locks or
blocks ([ADR-0007](0007-the-audio-engine.md)); Miri, which checks `unsafe` code; and `cargo fuzz`,
which fuzzes the parsers of outside files. Nixpkgs packages stable Rust only.

On 2026-11-07, RealtimeSanitizer reaches Rust in two ways:

- **rustc's own**, since 2025-11-08 (rust-lang/rust#147935): the flag `-Zsanitizer=realtime` and the
  attribute `#[sanitize(realtime = "nonblocking")]`, documented in the Unstable Book. The nightly's
  standard library ships its runtime (`librustc-nightly_rt.rtsan.a`, checked in the 2026-11-06
  nightly).
- **The crate `rtsan-standalone`** (0.3.0, Apache-2.0), which works on stable. By default its build
  script downloads a prebuilt runtime with `curl`; without that, it clones and builds LLVM's.

## Decision

**Stable Rust builds Magnolia**, at a version pinned in `rust-toolchain.toml` (1.99.1), which the
development shell and rustup both read. `rust-version` in `Cargo.toml` follows it. Edition 2024.

**A nightly pinned by date runs the checks that need one, and nothing else**: RealtimeSanitizer,
Miri and `cargo fuzz`. Its date lives in `flake.nix`; the shell's `nightly` command puts it first on
the path. Nothing that users run is built by it.

**rust-overlay supplies both toolchains** to the development shell: Rust's own builds, by version
or by date, pinned by `flake.lock`.

**RealtimeSanitizer is rustc's own.** Code marks its real-time functions with
`#[sanitize(realtime = "nonblocking")]` under the `realtime-sanitizer` feature, which only the
sanitizer's build turns on. The standard library is not rebuilt with the sanitizer: it instruments
only the functions marked and intercepts calls into the C library, so an uninstrumented standard
library is still checked where it calls `malloc` or `pthread_mutex_lock`.

**Both toolchains are bumped on purpose**, at each stage's end with the dependencies, or when a
check needs a newer one, each in its own commit.

## Alternatives considered

- **`rtsan-standalone`**: works on stable, but its build downloads a binary or builds LLVM, and it
  adds a crate to every crate with real-time code, for what rustc already does.
- **fenix**: supplies the same toolchains; it would make a second way beside SlimeEngine's, which
  uses rust-overlay, for no gain.
- **rustup outside Nix**: no dependency, but the shell would no longer hold every tool, and CI would
  install rustup apart.
- **Nightly for everything**: one toolchain, but every user's build would ride on unstable features
  and a moving compiler.

## Consequences

- The sanitizer's attribute is unstable: a nightly bump may break it. The check fails plainly, and
  the date is pinned until then.
- The shell downloads two toolchains; CI pays for both on each run.
- `rtsan-standalone` remains the way back if rustc's sanitizer is removed.
