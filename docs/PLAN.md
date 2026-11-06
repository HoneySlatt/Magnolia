# Plan

This file says what is being worked on now: the milestones of the current stage, their status, how
each one is checked, and the known debt. Direction beyond this stage is in
[`ROADMAP.md`](ROADMAP.md); decisions are in [`adr/`](adr/README.md). A *stage* is a step of the
roadmap; a *milestone* is a small, verifiable step of a stage.

Every milestone ends with the definition of done in [`AGENTS.md`](../AGENTS.md#definition-of-done).

## Current

- **Design session, 2026-11-06 to 07: done.** The owner's interview, the survey of the field
  ([`survey.md`](survey.md)), the recommendations challenged, ten decisions
  ([ADR-0001](adr/0001-what-magnolia-is.md) to [ADR-0010](adr/0010-the-quality-bar.md)), and the
  list of what Magnolia asks of uwui ([`uwui-needs.md`](uwui-needs.md)), handed to the owner for
  SlimeEngine's M6d.1b3b.
- **Next: M1.1**, the workspace and its checks, on the owner's go.

## Status

| Milestone | Goal | Status |
|---|---|---|
| [M1.1](#m11--the-workspace-and-its-checks) | The Rust workspace builds and is checked in CI | Planned |
| [M1.2](#m12--the-time-model) | Positions, tempo, meter and timecode are exact | Planned |
| [M1.3](#m13--the-jack-backend-and-the-graph) | A graph of nodes plays through the JACK API, swapped without a click | Planned |
| [M1.4](#m14--a-session-plays) | A test session of clips and MIDI plays from disk, delays compensated | Planned |
| [M1.5](#m15--recording-and-monitoring) | A voice is recorded and monitored at low latency | Planned |
| [M1.6](#m16--plugin-hosts) | Native CLAP and VST3 plugins play out of the engine's process and survive a crash | Planned |
| [M1.7](#m17--the-wine-bridge) | The owner's Windows plugins play under the system's Wine | Planned |
| [M1.8](#m18--plugin-windows-and-the-command-surface) | Plugin GUIs open; the engine is driven by the command line and MCP | Planned |
| [M1.9](#m19--the-stages-budgets) | The stage's budgets are measured and met | Planned |

## Stage 1 — The engine and the plugins

Magnolia plays, records and monitors at low latency on Linux, hosting the owner's plugins out of
the engine's process, without an interface
([ADR-0006](adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md),
[ADR-0007](adr/0007-the-audio-engine.md)). Each new dependency is put to the owner when its
milestone first needs it.

### M1.1 — The workspace and its checks

- **Goal.** Code can be written and checked from the first line.
- **Delivers.** A Cargo workspace; the Rust toolchain in the development shell, with a nightly for
  RealtimeSanitizer, `cargo fuzz` and Miri; `scripts/check.sh` with formatting, lints as errors,
  tests, documentation and dependency policy (`cargo deny`); CI running the script.
- **Done when.** CI is green on a first crate whose test proves, in a child process since the
  sanitizer aborts, that RealtimeSanitizer catches an allocation on a thread marked real-time.
- **How to test.** `nix develop --command scripts/check.sh`.
- **Measured.** The time of a full check and of the CI job; the iteration's budget set from them.

### M1.2 — The time model

- **Goal.** Every position in Magnolia is exact over a film's length.
- **Delivers.** 64-bit sample positions; fixed-point musical time; a tempo map with tempo and meter
  changes; timecode at 23.976, 24, 25, 29.97 with and without drop-frame, 30, 50, 59.94 and 60; one
  written rounding rule for every conversion.
- **Done when.** Conversions round-trip over ten hours without drift, by the rounding rule; tested
  and fuzzed.
- **How to test.** `cargo test`; `cargo fuzz` briefly in CI.
- **Measured.** The time of a conversion, as the engine calls it per block.

### M1.3 — The JACK backend and the graph

- **Goal.** A graph of nodes plays through the JACK API, on PipeWire and on JACK2.
- **Delivers.** The duplex backend; the compiled graph, swapped at a block's boundary, freed by a
  collector; nodes in an arena; sample-accurate events; a backend without a device for tests; the
  engine's histogram of callback times; the reference projects of both tiers, defined and fixed.
- **Done when.** A graph swap makes no click, tested; RealtimeSanitizer finds nothing; the engine
  reports missing real-time rights plainly.
- **How to test.** `cargo test` on the backend without a device; playing by hand through `pw-jack`.
- **Measured.** The callback's histogram at 64 frames on the high tier; rest with the transport
  stopped.

### M1.4 — A session plays

- **Goal.** A test session of audio clips and MIDI plays from disk.
- **Delivers.** Clips as immutable snapshots; the disk thread reading ahead; delay compensation;
  the test session, a file that only tests and the command line read, replaced by Stage 2's
  project.
- **Done when.** Playback nulls against an offline render of the same session, to the last bit;
  delay compensation aligns a test plugin's latency to the sample.
- **How to test.** `cargo test`; the command line plays the session by hand.
- **Measured.** The callback's histogram on the reference project without plugins; the disk
  thread's read-ahead.

### M1.5 — Recording and monitoring

- **Goal.** The owner records a voice and hears it without delay.
- **Delivers.** Input monitoring; recording to a format that survives a crash, repaired on
  recovery; the latency of the round trip reported.
- **Done when.** A recording killed mid-take is recovered whole, tested; the round trip through
  Magnolia equals a direct loopback.
- **How to test.** `cargo test` for recovery; the loopback by hand.
- **Measured.** The round trip with a loopback cable and `jack_iodelay`, by the owner on the
  Focusrite.

### M1.6 — Plugin hosts

- **Goal.** Native CLAP and VST3 plugins play out of the engine's process.
- **Delivers.** The plugin-host process; audio by shared memory, waking by futex; the prototype that
  measures one process per plugin against one per track's chain; the deadline of a late plugin;
  silence on a crash or a hang; reload; state snapshots, and the interval for plugins that never
  report a change.
- **Done when.** The unit of isolation, the deadline and the interval are decided by an ADR on the
  figures; a test plugin that hangs and one that crashes never cause an xrun, tested.
- **How to test.** `cargo test` with the project's test plugins; free native plugins by hand.
- **Measured.** A round trip per period and per host; memory per host; the cost of reading a state.

### M1.7 — The Wine bridge

- **Goal.** The owner's Windows plugins play under the system's Wine
  ([ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- **Delivers.** yabridge's master branch measured as the baseline; how Wine sets thread priorities,
  checked in its code; the prototype comparing a pure Windows executable with a winelib program,
  its choice an ADR; the Windows host; its audio thread in real-time priority; the test plugins
  built for Windows; the list of tested Wine versions.
- **Done when.** Serum, FabFilter's plugins, a Valhalla reverb, Kontakt with a large library and
  Auto-Tune in low-latency mode each load, play, show their GUI and survive a kill; the test
  plugins pass in CI; the bridge's round trip is at least as good as yabridge's.
- **How to test.** `cargo test` on the test plugins under Wine; the owner's plugins by hand.
- **By hand.** The owner activates the licenses in a test prefix; Magnolia never touches the owner's
  own prefix.
- **Measured.** A round trip per period under Wine; memory per Wine host.

### M1.8 — Plugin windows and the command surface

- **Goal.** Plugin GUIs open, and every engine function is driven without an interface.
- **Delivers.** Floating plugin windows, opened, sized and grouped by Magnolia, kept above where the
  compositor allows it; each plugin's parameter list; the command line and the MCP server over the
  engine.
- **Done when.** An agent loads, plays, records and edits a plugin's parameters through MCP alone,
  tested.
- **How to test.** `cargo test` through the MCP server; plugin windows by hand on Wayland and X11.
- **Measured.** The engine's start to playing an empty session.

### M1.9 — The stage's budgets

- **Goal.** Stage 1's exit, measured.
- **Delivers.** The hour-long run on each tier; rest, memory and the comparison with REAPER
  measured; `docs/measurements.md`.
- **Done when.** The [exit of Stage 1](ROADMAP.md#stage-1--the-engine-and-the-plugins--01) is met,
  then the stage audit.
- **How to test.** The reference projects, by hand, on each tier.
- **Measured.** Every budget of the sound and of rest in the [quality bar](ROADMAP.md#budgets).

## Ideas

None yet.

## Known debt

None yet: nothing is built.

Finished stages move to [`history.md`](history.md), in a few lines each, so that this file holds the
current stage only, under about 2,000 words.
