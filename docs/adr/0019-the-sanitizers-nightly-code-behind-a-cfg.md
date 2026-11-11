# 0019. The sanitizer's nightly code behind a cfg

- Status: Accepted; amends 0013 (a cfg instead of a feature)
- Date: 2026-11-11

## Context

[ADR-0013](0013-a-pinned-stable-toolchain-and-a-nightly-for-checks.md) put the sanitizer's
attribute, `#[sanitize(realtime = "nonblocking")]`, behind the `realtime-sanitizer` feature of
`magnolia-engine`. That attribute and `#![feature(sanitize)]` compile only on the nightly. A Cargo
feature is meant to be turned on freely, so `cargo build --all-features`, and any tool that turns
every feature on, failed on the stable toolchain that builds Magnolia. M1.3 marks real functions of
the engine, not only a test, so the gate now reaches the library's own code.

## Decision

The owner's choice of 2026-11-11.

**The nightly-only code is gated on the cfg `magnolia_rtsan`**, written as
`#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]` and
`#![cfg_attr(magnolia_rtsan, feature(sanitize))]`. Nothing sets it but the sanitizer's flags:
`scripts/check.sh` passes `RUSTFLAGS="-Zsanitizer=realtime --cfg magnolia_rtsan"`, with
`--target`, so that build scripts and procedural macros see neither. The workspace declares the
cfg to the compiler's `unexpected_cfgs` lint, so that a misspelt one fails the lints.

**The `realtime-sanitizer` feature is removed.** Every feature now builds on stable.

**The sanitizer's test is a target with `test = false`.** `cargo test` and `--all-targets` leave it
out. `scripts/check.sh` names it with `--test`, for Clippy and for the run, both with the
sanitizer's flags. Run on stable by hand, it builds, then fails and says that it was built without
the sanitizer.

## Alternatives considered

- **Keep the feature**: one less change, but `--all-features` fails on stable, and every crate that
  marks real-time code would carry a feature that its users must never turn on.
- **rustc's own `cfg(sanitize = "realtime")`**: set by `-Zsanitizer` itself, but reading it needs
  the unstable `cfg_sanitize` feature, so the gate would itself compile only on the nightly.
- **`rtsan-standalone`**, which works on stable: rejected by ADR-0013 for its downloaded runtime;
  nothing has changed that.

## Consequences

- Stable builds every feature, and every target that `cargo test` runs.
- A function marked real-time costs nothing outside the sanitizer's build: the attribute is not
  there.
- A new crate with real-time code needs no feature, only the same `cfg_attr`.
