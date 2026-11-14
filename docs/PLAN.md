# Plan

This file says what is being worked on now: the milestones of the current stage, their status, how
each one is checked, and the known debt. Direction beyond this stage is in
[`ROADMAP.md`](ROADMAP.md); decisions are in [`adr/`](adr/README.md). A *stage* is a step of the
roadmap; a *milestone* is a small, verifiable step of a stage.

Every milestone ends with the definition of done in [`AGENTS.md`](../AGENTS.md#definition-of-done).
Every milestone also measures the compiling budgets of the [floors](ROADMAP.md#floors), on the high
tier, into [`measurements.md`](measurements.md).

## Current

- **The design session, 2026-11-06 to 07, and the quality bar measured against the market,
  2026-11-10: done** ([ADR-0001](adr/0001-what-magnolia-is.md) to
  [ADR-0017](adr/0017-macos-a-candidate-stage-after-stage-4.md); the [survey](survey.md); what
  Magnolia asks of uwui in [`uwui-needs.md`](uwui-needs.md), for the owner to carry). Stage 3 waits
  for a stable uwui release that holds what it needs
  ([ADR-0011](adr/0011-magnolia-no-longer-a-program-of-uwuis-first-stage.md)).
- **M1.1, the workspace and its checks: done, 2026-11-10.**
- **M1.2, the time model: done, 2026-11-11** ([ADR-0018](adr/0018-the-time-model.md)).
- **M1.3 to M1.5: built, 2026-11-11 to 14; waiting for the owner's measures by hand**, on the
  owner's decisions ([ADR-0019](adr/0019-the-sanitizers-nightly-code-behind-a-cfg.md) to
  [ADR-0032](adr/0032-recording-by-a-capture-tap-of-the-processor.md); `jack`, `rtrb`, `libc`,
  `serde` and `serde_json` taken; the [reference projects](reference-projects.md) adopted). What
  only the owner's hardware can measure is listed in each milestone below.

## Status

| Milestone | Goal | Status |
|---|---|---|
| [M1.1](#m11--the-workspace-and-its-checks) | The Rust workspace builds and is checked in CI | Done |
| [M1.2](#m12--the-time-model) | Positions, tempo, meter and timecode are exact | Done |
| [M1.3](#m13--the-jack-backend-and-the-graph) | A graph of nodes plays through the JACK API, swapped without a click | Built; waiting for the owner's measures by hand |
| [M1.4](#m14--a-session-plays) | A test session of clips and MIDI plays from disk, delays compensated | Built; waiting for the owner's measures by hand |
| [M1.5](#m15--recording-and-monitoring) | A voice is recorded and monitored at low latency | Built; waiting for the owner's measures by hand |
| [M1.6](#m16--plugin-hosts) | Native CLAP and VST3 plugins play out of the engine's process and survive a crash | Planned |
| [M1.7](#m17--the-wine-bridge) | The owner's Windows plugins play under the system's Wine | Planned |
| [M1.8](#m18--plugin-windows-and-the-command-surface) | Plugin GUIs open; the engine is driven by the command line and MCP | Planned |
| [M1.9](#m19--the-stages-budgets) | The stage's budgets are measured and met | Planned |

## Stage 1 — The engine and the plugins

Magnolia plays, records and monitors at low latency on Linux, hosting the owner's plugins out of
process, without an interface
([ADR-0006](adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md),
[ADR-0007](adr/0007-the-audio-engine.md)).

### M1.1 — The workspace and its checks

Done, 2026-11-10: the workspace, its checks and CI
([measurements](measurements.md#m11--the-workspace-and-its-checks)).

### M1.2 — The time model

Done, 2026-11-11: exact positions over a film's length ([ADR-0018](adr/0018-the-time-model.md),
[measurements](measurements.md#m12--the-time-model)).

### M1.3 — The JACK backend and the graph

- **Goal.** A graph of nodes plays through the JACK API, on PipeWire and on JACK2.
- **Delivers.** The duplex backend; the compiled graph, swapped at a block's boundary, freed by a
  collector; nodes in an arena; sample-accurate events; a backend without a device for tests; the
  engine's histogram of callback times; the reference projects of both tiers, defined and fixed.
- **Done when.** A graph swap makes no click, tested; RealtimeSanitizer finds nothing; the engine
  reports missing real-time rights plainly.
- **How to test.** `cargo test` without a device; playing by hand through `pw-jack`.
- **Measured.** The callback's histogram at 64 frames on the high tier; rest with the transport
  stopped; the laptop of the lowest tier on mains power, its frequency governor and audio device
  named; the lowest tier's reference project defined, its plugins named; its memory, within
  2.5 GB, is measured at M1.9, once it can play
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Delivered**, on ADR-0019 to ADR-0024: the graph, its swaps, events and collector in
  `magnolia-engine`; the duplex backend in `magnolia-jack`; tested against an oracle, on JACK2's
  dummy driver and under RealtimeSanitizer. Figures in
  [`measurements.md`](measurements.md#m13--the-jack-backend-and-the-graph).
- **By hand, by the owner** (agents have no sound card and no display):
  - listen through PipeWire: `pw-jack -p 64 cargo run --release -p magnolia-jack --example play --
    --connect --seconds 60` ([CONTRIBUTING](../CONTRIBUTING.md#playing-through-jack)); no click at
    each crossfade, every two seconds; the real-time report says the audio thread runs in real time;
  - the callback's histogram at 64 frames on the high tier, through the Focusrite (its model
    recorded), with `--load 64` and more: the worst callback under 85 % of the period, the 99.9th
    percentile under 50 %;
  - the same through JACK2 if it is installed, `jackd -R`;
  - the laptop of the lowest tier on mains power, its frequency governor and audio device named, at
    128 and 64 frames.

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
- **Delivered**, on ADR-0025 to ADR-0029: compensation, the transport, MIDI sequences, the disk
  reader, WAV and MIDI files, the test session, the `magnolia` command. Tested: a render nulls
  against playback, device-less and through JACK2's dummy driver; compensation aligns
  `TestLatency` to the sample. Figures in [`measurements.md`](measurements.md#m14--a-session-plays).
- **By hand, by the owner**, with the reference projects generated as
  [CONTRIBUTING](../CONTRIBUTING.md#test-sessions) says:
  - listen through PipeWire: `pw-jack -p 64 cargo run --release -p magnolia-cli -- play
    <high>/session.json --connect`; no click at the start, at the stop, nor at a swap; the real-time
    report says the audio thread runs in real time;
  - the callback's histogram on the high tier's project at 64 frames through the Focusrite, and on
    the lowest tier's at 128 frames on the laptop on mains power: the worst callback under 85 % of
    the period, the 99.9th percentile under 50 %, no disk underrun;
  - on the laptop, the reader's low-water marks and refill times, which its storage sets;
  - REAPER opens the generated WAV and MIDI files as they are (ADR-0029).

### M1.5 — Recording and monitoring

- **Goal.** The owner records a voice and hears it without delay.
- **Delivers.** Input monitoring; recording to a format that survives a crash, repaired on
  recovery; the latency of the round trip reported.
- **Done when.** A recording killed mid-take is recovered whole, tested; the round trip through
  Magnolia equals a direct loopback.
- **How to test.** `cargo test` for recovery; the loopback by hand.
- **Measured.** The round trip with a loopback cable and `jack_iodelay`, by the owner on the
  Focusrite.
- **Delivered**, on ADR-0030 to ADR-0032: in `magnolia-engine`, WAV takes of unknown length with
  `bext` and their repair, the capture tap, the recorder and recovery (`record`); in
  `magnolia-jack`, the device's latencies from JACK's latency callback, physical capture connected,
  its tests in freewheel; version 2 of the test session with input tracks; `magnolia record` and
  `magnolia recover`. Tested: a take killed mid-way recovered whole; a simulated power cut keeps
  every synced frame; takes placed to the sample by the round trip; on JACK2's dummy driver, an
  impulse through Magnolia's monitoring path arrives with a direct one. Figures in
  [`measurements.md`](measurements.md#m15--recording-and-monitoring).
- **By hand, by the owner**, as [CONTRIBUTING](../CONTRIBUTING.md#measuring-the-round-trip) says:
  - on the Focusrite at 64 frames, a loopback cable and `jack_iodelay`, direct and through
    Magnolia's monitoring path, each reopened several times: the two round trips equal, to the
    sample;
  - a take of the small session's playback through the cable, its `mic` at gain 0 so that nothing
    feeds back, placed by `--extra-latency` from `jack_iodelay`: in line with its render, to the
    sample;
  - through `pw-jack`: the latency report gives PipeWire's figures, `record --connect` records,
    and a quantum forced during a take ends it, reported;
  - a voice recorded and heard live, at 64 frames: no xrun nor own overrun during the take, the
    callback within its floor;
  - on the laptop, `cargo bench --bench recorder`: the syncs' stall on its storage.

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

The known debt is in [`debt.md`](debt.md).

Finished stages move to [`history.md`](history.md).
