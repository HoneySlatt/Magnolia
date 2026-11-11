# Plan

This file says what is being worked on now: the milestones of the current stage, their status, how
each one is checked, and the known debt. Direction beyond this stage is in
[`ROADMAP.md`](ROADMAP.md); decisions are in [`adr/`](adr/README.md). A *stage* is a step of the
roadmap; a *milestone* is a small, verifiable step of a stage.

Every milestone ends with the definition of done in [`AGENTS.md`](../AGENTS.md#definition-of-done).
Every milestone also measures the compiling budgets of the [floors](ROADMAP.md#floors), on the high
tier, into [`measurements.md`](measurements.md).

## Current

- **Design session, 2026-11-06 to 07: done.** The owner's interview, the survey of the field
  ([`survey.md`](survey.md)), the recommendations challenged, ten decisions
  ([ADR-0001](adr/0001-what-magnolia-is.md) to [ADR-0010](adr/0010-the-quality-bar.md)), and the
  list of what Magnolia asks of uwui ([`uwui-needs.md`](uwui-needs.md)), handed to the owner for
  SlimeEngine's M6d.1b3b.
- **2026-11-07: Magnolia is no longer a program of uwui's first stage**; Slatty took its place
  ([ADR-0011](adr/0011-magnolia-no-longer-a-program-of-uwuis-first-stage.md)). Stages 1 and 2 are
  unchanged; Stage 3 waits for a stable release of uwui that holds what it needs first.
- **M1.1, the workspace and its checks: done, 2026-11-10.** CI is green; the check's times are in
  [`measurements.md`](measurements.md#m11--the-workspace-and-its-checks).
- **The quality bar measured against the market, 2026-11-10: done.** The owner's interview, its
  facts in the [survey](survey.md#the-market-measured-2026-11-10), its figures challenged; four
  decisions ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md) to
  [ADR-0017](adr/0017-macos-a-candidate-stage-after-stage-4.md)): floors and leads over measured
  rivals, the interface in a process of its own, agents on the shape of uwui's agent core, macOS a
  candidate stage. What follows for uwui is in
  [`uwui-needs.md`](uwui-needs.md#since-uwuis-decisions-of-2026-11-07), for the owner to carry.
- **M1.2, the time model: done, 2026-11-11.** The owner's decisions of 2026-11-11 are
  [ADR-0018](adr/0018-the-time-model.md), which amends ADR-0007: absolute positions are counted in
  time units, not samples. The figures are in
  [`measurements.md`](measurements.md#m12--the-time-model).
- **Next: M1.3**, the JACK backend and the graph, on the owner's go.

## Status

| Milestone | Goal | Status |
|---|---|---|
| [M1.1](#m11--the-workspace-and-its-checks) | The Rust workspace builds and is checked in CI | Done |
| [M1.2](#m12--the-time-model) | Positions, tempo, meter and timecode are exact | Done |
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
  sanitizer aborts, that RealtimeSanitizer catches an allocation in a function marked real-time.
- **How to test.** `nix develop --command scripts/check.sh`.
- **Measured.** The time of a full check and of the CI job; the iteration's budget set from them.

### M1.2 — The time model

- **Goal.** Every position in Magnolia is exact over a film's length.
- **Delivers.** 64-bit sample positions, and absolute positions in time units
  ([ADR-0018](adr/0018-the-time-model.md)); fixed-point musical time; a tempo map with tempo and meter
  changes; timecode at 23.976, 24, 25, 29.97 with and without drop-frame, 30, 50, 59.94 with and
  without drop-frame, and 60; one written rounding rule for every conversion.
- **Done when.** Conversions round-trip over ten hours without drift, by the rounding rule; tested
  and fuzzed.
- **How to test.** `cargo test`; `cargo fuzz` briefly in CI.
- **Measured.** The time of a conversion, as the engine calls it per block.
- **Delivered**, on the owner's decisions of 2026-11-11 ([ADR-0018](adr/0018-the-time-model.md);
  libfuzzer-sys taken for `cargo fuzz`): `magnolia_engine::time`; the rule checked on every grid
  over ten hours, a tick's sample against an independent oracle (`tests/time_over_ten_hours.rs`); two fuzz targets,
  run 20 s each by `scripts/check.sh`; a RealtimeSanitizer case on a block's conversions; a
  benchmark. No program runs yet: the tests, the fuzz targets and the benchmark ran.

### M1.3 — The JACK backend and the graph

- **Goal.** A graph of nodes plays through the JACK API, on PipeWire and on JACK2.
- **Delivers.** The duplex backend; the compiled graph, swapped at a block's boundary, freed by a
  collector; nodes in an arena; sample-accurate events; a backend without a device for tests; the
  engine's histogram of callback times; the reference projects of both tiers, defined and fixed.
- **Done when.** A graph swap makes no click, tested; RealtimeSanitizer finds nothing; the engine
  reports missing real-time rights plainly.
- **How to test.** `cargo test` on the backend without a device; playing by hand through `pw-jack`.
- **Measured.** The callback's histogram at 64 frames on the high tier; rest with the transport
  stopped; the laptop of the lowest tier on mains power, its frequency governor and audio device
  named; the lowest tier's reference project, its plugins named, within 2.5 GB
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).

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
  figures; a test plugin that hangs and one that crashes never cause an xrun, tested; a plugin's
  NaN, infinity or runaway level never reaches an output, tested; a crashed plugin is reloaded with
  its last state within the time set; each feature the engine supports passes its conformance
  plugin in CI; processing power meets its floor, at least 0.9 times REAPER's
  ([floors](ROADMAP.md#floors)).
- **How to test.** `cargo test` with the project's test plugins; free native plugins by hand.
- **Measured.** A round trip per period and per host; memory per host, its budget set; the cost of
  reading a state; the time of a crashed plugin's reload, against Bitwig's, its budget set;
  processing power by DAWbench's method, against REAPER running plugins inside its own process and
  against the best rival that isolates plugins.

### M1.7 — The Wine bridge

- **Goal.** The owner's Windows plugins play under the system's Wine
  ([ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- **Delivers.** yabridge's master branch measured as the baseline; how Wine sets thread priorities,
  checked in its code; the prototype comparing a pure Windows executable with a winelib program,
  its choice an ADR; the Windows host; its audio thread in real-time priority; the test plugins
  built for Windows; the list of tested Wine versions.
- **Done when.** Serum, FabFilter's plugins, a Valhalla reverb, Kontakt with a large library and
  Auto-Tune in low-latency mode each load, play, show their GUI and survive a kill; each plugin of
  the Wine test set that works under yabridge does so under Magnolia, its result published; the
  test plugins pass in CI; the bridge's round trip is at least as good as yabridge's.
- **How to test.** `cargo test` on the test plugins under Wine; the owner's plugins by hand.
- **By hand.** The owner activates the licenses in a test prefix; Magnolia never touches the owner's
  own prefix.
- **Measured.** A round trip per period under Wine; memory per Wine host; on a plugin that does
  nothing, the bridge's cost above a futex ping-pong between two processes, against yabridge's.

### M1.8 — Plugin windows and the command surface

- **Goal.** Plugin GUIs open, and every engine function is driven without an interface.
- **Delivers.** Floating plugin windows, opened, sized and grouped by Magnolia, kept above where the
  compositor allows it; each plugin's parameter list; the command line and the MCP server over the
  engine, the server on the shape of uwui's agent core, its declarations in the core's format
  ([ADR-0016](adr/0016-agents-on-the-shape-of-uwuis-agent-core.md)).
- **Done when.** An agent loads, plays, records and edits a plugin's parameters through MCP alone,
  tested.
- **How to test.** `cargo test` through the MCP server; plugin windows by hand on Wayland and X11.
- **Measured.** The engine's start to playing an empty session, against REAPER's, Bitwig's and
  Ardour's.

### M1.9 — The stage's budgets

- **Goal.** Stage 1's exit, measured.
- **Delivers.** The eight-hour run on each tier; rest and memory measured; REAPER, Bitwig and
  Ardour measured on both tiers, for the leads; `docs/measurements.md`.
- **Done when.** The [exit of Stage 1](ROADMAP.md#stage-1--the-engine-and-the-plugins--01) is met,
  then the stage audit.
- **How to test.** The reference projects, by hand, on each tier.
- **Measured.** Every budget of the sound and of rest in the [quality bar](ROADMAP.md#floors).

## Ideas

- Pro Tools' other timecode rates (47.952, 48, 100, 119.88 and 120, 119.88 and 120 drop, and the
  "30 drop" and "60 drop" of pulled workflows), at the stage that brings video; what Pro Tools means by the last two is
  unverified ([survey](survey.md#time-models-2026-11-11)).
- A cursor that remembers the last tempo segment from block to block, if a session with thousands
  of tempo changes ever shows the binary search in the callback's histogram
  ([measurements](measurements.md#m12--the-time-model)).
- 11- and 13-tuplets exact, by refining the ticks by a whole factor, if a user needs them.

## Known debt

- **The sanitizer's reports name addresses, not functions**, until the shell has a symbolizer or the
  runs set `allow_addr2line=true`. ADR-0007's allowlist, suppressed by name, needs names: settled in
  M1.3, taking care that a suppression of `syscall` does not hide a blocking wait.
- **The time model's conversions were measured under load** (load average 34 to 36): their figures
  are upper bounds until taken again on a quiet machine
  ([measurements](measurements.md#m12--the-time-model)).

Finished stages move to [`history.md`](history.md), in a few lines each, so that this file holds the
current stage only, under about 2,000 words.
