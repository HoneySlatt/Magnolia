# Audit on demand: architecture, stability, evolvability and contribution, 2026-11-17

The owner asked for an adversarial audit of the foundations before the plugin hosts. This file
records its scope, findings and the decision taken. Open items are in [`debt.md`](../debt.md) and
[`PLAN.md`](../PLAN.md), each with the milestone that follows it.

## Scope

- **Commit**: `abf2cfa`, the end of M1.5.
- **Dimensions**: architecture and evolvability, stability and recovery, data safety, documentation,
  contribution.
- **Covered**: every document of `docs/`, `README.md`, `CONTRIBUTING.md`, the manifests, `flake.nix`,
  `scripts/check.sh` and CI; the engine's core read whole (`node.rs`, `engine/processor.rs`,
  `engine/controller.rs`, `engine/compile.rs`, `engine/sequence.rs`, `device.rs`); the public API of
  `disk` and `record`; the coupling of `magnolia-session` and `magnolia-cli` to the engine.
- **Not covered**: the tests were not read one by one; `wav`, `midi_file` and `time` were skimmed,
  not audited line by line.
- **Method**: read only. Nothing was built, run or measured during the audit: no finding below rests
  on a test run then. Two independent readers gathered facts, a challenger attacked the findings,
  and the quotes the findings rest on were checked again.

## Verdict

The foundations are sound; no rewrite is recommended. The engine depends on `rtrb` alone and knows
neither JACK nor JSON; the session uses the engine's public API only; no internal type leaks through
`pub`. The real-time path frees nothing, every queue is bounded and defined when full, panics are
contained at four levels, `unsafe` lives in `magnolia-jack` alone, each block with its `SAFETY:`
comment, and time is exact.

The risk is ahead: M1.6 fixes contracts that no ADR decides yet. These strengths are shown by design
and by tests on JACK2's dummy driver, not yet by the floors on PipeWire and the owner's interface
(F5).

## Findings

Ranked by severity, then urgency. Each names its evidence and the milestone that follows it.

### F1. The execution model cannot wait on several plugin hosts at once

- `Node::process` is synchronous (`crates/magnolia-engine/src/node.rs:33-50`), and the processor
  runs its steps one after another (`engine/processor.rs:494-517`).
- [ADR-0023](../adr/0023-the-real-time-allowlist.md) allows wakes only, no wait with a deadline.
  [ADR-0007](../adr/0007-the-audio-engine.md) decided a graph scheduler on workers; no milestone
  builds it.
- With hosts called in series, plugin work fits in about one core per period, while M1.6's *Done
  when* asks for 0.9 times REAPER's processing power (an inference, not a measure). The comparison
  of isolation units in [ADR-0003](../adr/0003-every-plugin-out-of-the-engines-process.md) changes
  with the call model.
- An offline render must not apply a plugin's deadline, or a bounce holds silences; `DeviceBuffers`
  carries no mode.
- The node contract cannot hold for a third-party plugin: "its output must not depend on how the
  engine cuts blocks" and a latency fixed for its life (`node.rs:28-30`).
- **Recommended**: remote nodes split in two on the audio thread (wake every ready host, run the
  engine's own work, collect each result when needed), workers kept as an option for the engine's own
  DSP; one named bounded wait on the allowlist; a deadline per hop of a chain; a real-time or offline
  mode from the backend; cuts sent as events; a contract for third-party nodes of its own. A
  blocking proxy is enough for the host, shared memory and the crash and reload tests, never for the
  figures that decide.
- **Follows it**: M1.6's design session.

### F2. Plugin hosts must outlive the processor; the engine's process has no owner

- A poisoned processor is rebuilt and "the controller sends the graph again"
  ([ADR-0022](../adr/0022-a-panic-on-the-real-time-path-gives-silence.md)). If proxies own their
  hosts, a rebuild or a dead JACK server reloads every plugin from its last snapshot: data lost for
  plugins that never report a change.
- `Controller` is `&mut self`, and its owner must `poll` while a swap is pending. The command line
  holds orchestration that MCP will need (`crates/magnolia-cli/src/play.rs:225-245` and `334-364`,
  `record.rs:153-203`), including a third copy of an engine rule: "A stop plays as much again, then
  fades" (`play.rs:237-238`).
- **Recommended**: a supervisor owns the hosts and proxies are handles that reattach; one owner in
  the library, one thread with a bounded inbox, holds the controller, the supervisor, the recorder
  and the disk reader; the engine publishes the end of its stop.
- **Follows it**: M1.6's design session.

### F3. The transport a node sees is too poor for plugins

- `Transport` holds `rolling`, `position`, `audible` and `generation` (`node.rs:131-149`), while
  [ADR-0025](../adr/0025-delay-compensation-by-reading-ahead.md) promises plugins the transport "as
  CLAP's transport event expects". CLAP's transport carries tempo and its ramp, meter, bar, loop,
  and updates inside a block by events (`clap/events.h`).
- **Recommended**: a musical transport for remote nodes, and every change inside a sub-block (tempo,
  meter, later a loop's wrap at a node's own position) as an event at an offset. Loop playback is
  not designed now.
- **Follows it**: M1.6's design session.

### F4. The host protocol has no version, layout or trust model

- No document covers them. A panic in a proxy caused by a length a corrupted host wrote poisons the
  graph ([ADR-0034](../adr/0034-no-play-after-the-graphs-panic.md)) and, through F2, reloads every
  plugin. Reading memory another process writes through `&[f32]` is a data race.
- M1.6 says "waking by futex" while M1.7 still chooses between a pure Windows executable and a
  winelib program ([ADR-0004](../adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)); a
  pure Windows program likely cannot reach a Linux futex (unverified).
- **Recommended**: a magic number, a protocol version and a build id at the handshake; fixed offsets
  and no pointers; every field the host writes read through atomics, checked, unable to panic, and
  fuzzed; the wake behind one function on each side; the mapping in a crate of its own, `unsafe`
  forbidden in the engine; Miri in `scripts/check.sh` from that first `unsafe`. The documents say
  plainly that isolating faults is not confinement: a plugin runs with the user's rights.
- **Follows it**: M1.6's design session.

### F5. M1.6 rests on floors never measured on the owner's hardware

- M1.3 to M1.5 wait for the owner's measures; PipeWire was read, not run (`debt.md`); every
  callback figure comes from JACK2's dummy driver without real time. A plugin's deadline, the round
  trip's budget and the unit of isolation depend on PipeWire and the Focusrite at 64 frames.
- **Recommended**: the owner's measures of M1.3 to M1.5 come before the ADRs that need figures; the
  design goes on meanwhile.

### F6. M1.6 and M1.8 are each several milestones

- M1.6 holds the host, shared memory, a prototype, deadlines, crash, hang, reload, state, five
  conformance features and two comparisons of processing power, and inherits automation lanes
  ([ADR-0027](../adr/0027-timeline-events-from-sources-on-nodes.md)), latency changes and a resync
  debt. M1.8 pairs plugin windows with the engine's server and protocol.
- **Recommended**: the design session proposes a split to the owner.

### F7. MCP edits the engine before commands and identities exist

- M1.8 edits plugin parameters through MCP; tracks are numbered by position
  ([ARCHITECTURE](../ARCHITECTURE.md#recording-a-session)), plugin instances are arena slots, and no
  ADR defines identities. Parameter ids are stable in CLAP and VST3, yet the event names the field
  `index` (`node.rs`, `EventKind::Parameter`).
- **Recommended**: M1.8 gives tracks and plugin instances ids the engine assigns, never positions;
  its declarations say that Stage 1's edits are live, unsaved and without undo; Stage 2 decides the
  persistent ids and takes them over.
- **Follows it**: M1.8, before its server.

### F8. Rest against plugin hosts

- Rest means "plugin hosts receive no calls" ([glossary](../glossary.md)), yet ADR-0003 reads plugin
  state at intervals on the host's main thread.
- **Recommended**: M1.6 stops the interval at rest and maps a plugin's silence and tail to *sleep*.
- **Follows it**: M1.6.

### F9. The `jack` crate on the real-time path

- Known debt: its `try_lock` around each period returns `Quit` when busy, and
  `jack.locked-process=false` waits for the backend to bypass its `AsyncClient`. The eight-hour run
  happens on a desktop where other programs create ports all the time. No milestone paid it.
- **Recommended**: a milestone before M1.9, decided on the owner's PipeWire runs, counting the frame
  time's gaps.

### F10. The method changed on 2026-11-16, after M1.5

- The owner's method now wants a changed decision to be a new ADR that supersedes the old one and
  states the rule whole; here ADR-0007 is amended by six. `ARCHITECTURE.md`, about 15,800 words,
  has no page that comes first. The method's definition of done now checks architecture contracts
  and change and failure scenarios: stability scenarios exist in the quality bar, evolution and
  compatibility ones do not. The ADR index is written by hand. Most items of `debt.md` name no
  milestone, and its header states the old rule.
- The local rule "No abstraction without a second concrete use" may be stricter than the method's,
  but it contradicts this project's own "platform code behind the engine's own interfaces" and the
  `jack` crate's fix: a boundary around an unstable dependency.
- The quality bar says Miri runs in CI; no step runs it. Today all `unsafe` wraps libjack or libc,
  which Miri cannot run.
- **Recommended**: superseding ADRs with M1.6, for the allowlist, the node contract and the engine;
  the rest consolidated at Stage 1's audit; a page first in `ARCHITECTURE.md`; the change and
  compatibility scenarios of [below](#change-and-failure-scenarios) in the quality bar; a generated
  ADR index; a milestone for each debt item; the local abstraction rule aligned with the method's,
  on the owner's go.

### F11. Contribution is closed, and nothing says so

- [ADR-0002](../adr/0002-gpl-3-0-or-later-copyright-kept.md): "No outside contribution is merged
  before a contribution policy exists." Neither `README.md` nor `CONTRIBUTING.md` says so.
- A license, the ownership of copyright, a DCO, a CLA and an assignment differ. A DCO certifies a
  right to submit under the project's license and grants the owner nothing more. An exception under
  the GPL's section 7 holds only for parts whose holders grant it. A transfer of author's rights in
  France must name each right and its extent, purpose, place and duration (article L131-3 of the
  Code de la propriété intellectuelle). A policy needs legal advice first.
- **Recommended**: one sentence in each file now; the policy before contributions open; pull request
  templates, a security policy and a code of conduct at the release audit.

### F12. Guards that apply only by choice

- The five no-panic lints are inner attributes in 39 files: a new real-time module gets none.
  `clippy.toml` bans locks and sleeps workspace-wide, "Every crate of the workspace is the engine's so
  far", which will hit the plugin host and the server. Nothing forbids `unsafe` in the engine.
- **Recommended**: the lints at the engine crate's root, allowed with a reason in its modules off the
  real-time path; a `clippy.toml` per crate off the real-time path; `#![forbid(unsafe_code)]` in
  `magnolia-engine`.
- **Follows it**: M1.6.

### F13. Live MIDI input is in no milestone

- [ADR-0001](../adr/0001-what-magnolia-is.md) names a MIDI keyboard and pads; the quality bar sets a
  floor of 0.5 ms of jitter; `DeviceBuffers` carries audio only. If PipeWire's JACK MIDI stamps
  events at the cycle's start, jitter reaches 1.33 ms at 64 frames (unverified).
- **Open**: the owner places it in the roadmap.

### F14. SQLite's WAL and network filesystems

- WAL "does not work over a network filesystem"; EXCLUSIVE locking, set before the first access,
  avoids the shared memory (sqlite.org, *Write-Ahead Logging*). ADR-0009 says nothing of it. The
  recording folder's lock, `File::try_lock`, is unverified on NFS and SMB.
- **Recommended**: WAL with `locking_mode=EXCLUSIVE`, since only the engine opens the project
  ([ADR-0015](../adr/0015-the-interface-in-a-process-of-its-own.md)).
- **Follows it**: Stage 2's design.

### F15. One snapshot per track, not per clip

- ADR-0007 says "one per clip"; `engine/sequence.rs` flattens a track's clips into one sequence, and
  every timeline edit is a full commit and compile (`engine/controller.rs:450-478`). No ADR records
  the change. At 10 s of read-ahead, 1,000 stereo tracks at 48 kHz hold about 3.8 GB of streams
  (an estimate); [ADR-0026](../adr/0026-one-stream-per-audio-track.md) does not scale the read-ahead
  with the tracks.
- **Follows it**: Stage 2's design; nothing to optimise now.

### F16. A torn WAV size can hide frames

- Known debt. The method pays at once what puts data at risk, unless the owner decides to keep it.
- **Open**: the owner's choice.

### Minor findings

- The collector's thread is never joined (`engine/collector.rs`); an overflow of the garbage queue
  leaks without a counter; the sync queue is an unbounded `VecDeque`, bounded in practice by the
  takes.
- The full check takes 153 s for a budget of 3 minutes, 194 s under load
  ([measurements](../measurements.md#m15--recording-and-monitoring)); the plan does not say how the
  CLAP, VST3 and Windows test plugins are built.
- No memory is locked (`mlock`); the major faults of the audio thread on the 4 GB laptop running
  Wine hosts are not covered by any test (unverified).
- A second backend has no written contract beyond `DeviceBuffers`: replacing a poisoned processor,
  the counters, the real-time report and freewheel are described in the JACK section alone.

## Change and failure scenarios

Traced in the code, none run. Each is a candidate for the quality bar.

| Scenario | Spread today | Acceptance criterion |
|---|---|---|
| The test session replaced by the project | The engine does not change; the session's copies of engine rules, the generator and the tests built on it do | The reference projects render, through the project, to the bit of today's renders |
| A Windows backend | `DeviceBuffers` holds; `read_at`, priorities and the backend contract change | The engine's tests pass on the device-less backend unchanged |
| A plugin changes its latency | CLAP changes latency only while activating: the node is silent, then a new shape is swapped in; needs F2's handles | Aligned to the sample after the resync; the silence bounded and counted |
| A host lost mid-take | The capture tap records before the graph: the take goes on | The take whole; no other host restarted |
| The interface in its own process | Needs F2's owner, F7's server and the meters' shared memory | Killing the interface stops neither the sound nor the take |
| An edit through the interface, the command line, MCP and undo | No command layer yet | One declaration serves the four |
| A migration interrupted | No project format yet; the test session already refuses a newer version as such | A newer project refused plainly; a power cut mid-migration leaves the old file or the new one, whole |
| A session ten times larger, and a long one | Budgets, 64-bit counters and the ten-hour test hold; the read-ahead and full commits do not scale | 1,000 tracks within the memory budget; an edit's time measured |
| Two contributions at once | Both take the next ADR number; the ADR index is written by hand | A generated index; numbers given at the merge |

## Decision

The owner chose, on 2026-11-17, that M1.6 opens with a design session that builds nothing: F1 to F4
and F8 decided by ADRs, and a split of M1.6 proposed. A plugin's deadline and the unit of isolation
stay open until the owner's measures of M1.3 to M1.5 and the prototype. The owner's measures go on
meanwhile ([PLAN](../PLAN.md#m16--plugin-hosts)).

## Fixed

Nothing yet: the audit changed no code.
