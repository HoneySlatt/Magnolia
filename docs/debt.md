# Known debt

What the project knows it owes: each item short, with why it stays and where it is tracked. The
[PLAN](PLAN.md) links here. An item leaves when a milestone that touches its code pays it; the
stage's audit triages the whole list.

- **A processor is lost when the JACK server dies**: JACK2 then refuses to deactivate the client,
  and the `jack` crate leaks its callback's context, the processor and its collector with it;
  `close` says so. Fixing it means bypassing the crate's `AsyncClient`
  ([ARCHITECTURE](ARCHITECTURE.md#the-jack-backend)). This item, the third and the `jackd -S` one
  are paid by a milestone of their own before M1.9's eight-hour runs, decided on the owner's
  PipeWire runs ([audit](audits/2026-11-17-architecture.md#f9-the-jack-crate-on-the-real-time-path)).
- **PipeWire was read in its code, not run** by an agent: its thread-init callback, its server
  names, its behaviour after a panic. The owner's listening, listed under
  [M1.3](PLAN.md#m13--the-jack-backend-and-the-graph), runs it.
- **From the design's challenge, not done**: asking PipeWire for its quantum; and
  `jack.locked-process=false`, so that other clients' notifications never skip a cycle, which must
  wait until the backend bypasses the `jack` crate's `AsyncClient`, since that default is what keeps
  the crate's buffer-size callback from contending its lock with the process callback; a ring of the
  slowest blocks with their start delay and margin; a logger for libjack's messages that never takes
  stderr's lock. The `jack` crate itself takes a `Mutex::try_lock` around each period
  ([ADR-0020](adr/0020-clippy-forbids-the-locks-the-sanitizer-cannot-see.md)).
- **An event for a node that a swap not yet applied brings** waits in the controller, and may play a
  period late; the controller's owner must `poll` while a swap is pending.
- **Three rare edits fade one path twice** (an existing node put into or taken out of series; a path
  through a kept node that neither graph holds): down to a quarter of the level for the 5 ms of the
  fade, never a step; no oracle test covers them.
- **The choice between a crossfade and a dip counts nodes, not their cost**: once plugins come, one
  node may cost a hundred gains.
- **JACK2 keeps `/dev/shm/jack-shm-registry`** after the backend's tests, as it does after any
  server.
- **A change of delay leaves the moved path silent for the new delay**: a commit that moves a
  playing path into a new delay line, such as a latent node added on a bus, crossfades into that
  line's empty history, so the path is silent for that line's delay
  ([ARCHITECTURE](ARCHITECTURE.md#delay-compensation)).
- **A swap that changes a playing source's compensation jumps its read position**
  ([ADR-0025](adr/0025-delay-compensation-by-reading-ahead.md)); M1.6 designs the resync with
  latency changes.
- **Two sequences of one node that overlap on one key**: one sequence never holds a key twice
  (`engine/sequence.rs`), but two may, as two MIDI tracks into one multi-timbral instrument will.
  The node's notes are one bit per channel and key, so the first note off releases the key, and
  the second note off, or the stop's, is never sent. The test synth releases every voice of a key
  at once, so the second note is only cut short; from M1.6, a plugin that releases one voice per
  note off keeps that voice sounding after the stop
  ([ARCHITECTURE](ARCHITECTURE.md#timeline-sources)).
- **The total compensation of a path is not capped**: only each node's latency is, at
  `MAX_LATENCY`; k latent nodes in series at the cap need delay lines of k × 65,536 samples per
  channel, allocated on the controller's thread.
- **A play waits for its cue's priming without a timeout**: if the disk reader's thread has died,
  the play never starts; `wait_primed` reports it (`ReaderStopped`), the transport does not. Tracked
  for the stage that brings the interface's transport.
- **The disk reader's limits**: clips within ±2^41 frames of the timeline's zero; generations wrap
  after 2^32 locates; `read_at` is Unix-only (the Windows stage replaces it); its I/O priority is the
  default, and the lowest tier's storage is not measured yet (the owner's list, M1.4).
- **The player copies with one relaxed atomic load per sample**, which the compiler does not
  vectorise: cheap at 16 tracks; to measure at large track counts in M1.9.
- **The session repeats the engine's compensation rule twice**: its memory estimate
  (`load/budget.rs`, with the rounding of streams and buffers) and the monitoring latency the
  command line reports to JACK (`load/latency.rs`), against `engine/compensation.rs`. A change to
  either must change both until the engine exposes its own figures; at M1.6, plugins' latencies are
  known only at run time, in the engine, and the second copy would then go wrong without a sound.
- **`JackBackend::close` stalls on a JACK2 server started with `jackd -S`**: the `jack` crate
  invalidates its callbacks before it deactivates the client, so a cycle in between ends the
  client's thread and a synchronous server waits for it, then may refuse to deactivate it
  (`NotDeactivated`, the processor lost). Fixing it means bypassing the crate's `AsyncClient`, as
  the first item does ([ARCHITECTURE](ARCHITECTURE.md#the-jack-backend)).
- **The null through JACK tells a period copied mid-period by x86's ordering of stores and loads**
  (`crates/magnolia-jack/tests/jack/session.rs`): to revisit when CI runs on another architecture.
- **`magnolia play` ends at once on Ctrl-C**, with no fade and no figures: handling the signal needs
  `unsafe` or a dependency; M1.8's command surface decides.
- **Paths without a test**: a generation mismatch (`processor.rs`), and the silence the JACK callback
  writes while the server's rate differs from the engine's, since a JACK2 server cannot change its
  rate while it runs.
- **`missed_plugin_deadlines` is never written** before M1.6, which brings deadlines; its test checks
  that it stays at zero.
- **The facts read in PipeWire's and JACK2's code** (in ADR-0022 and the JACK backend's section of
  `ARCHITECTURE.md`) name no file or version; their sources are in the M1.3 design challenge's notes,
  not yet in [`survey.md`](survey.md).
- **A WAV size torn by a power cut can hide frames**: `wav::repair` assumes storage writes a
  sector whole. A 4-byte size torn mid-write, as when a repair marks a file of sizes 0 unfinished,
  can leave a file that reads as finished but short, which no repair then takes up. Data is at
  risk, so it is paid at once or kept by the owner's explicit choice, still open
  ([audit](audits/2026-11-17-architecture.md#f16-a-torn-wav-size-can-hide-frames)).
- **The session's memory budget copies the recorder's marks ring** (`load/budget.rs`, 256 marks
  of 64 bytes, against the engine's private `MARKS_CAPACITY`): an overestimate today, which will
  not follow a change of the engine's until it exports a tap's bytes.
- **Two `magnolia` processes collide in JACK2**, even on different servers: JACK2 names a client's
  notification socket after the client's name alone and deletes any socket in its way, and the
  command line's client is always `magnolia`. The tests name their clients by process id.
- **libjack's close may wait for ever**: JACK2 1.9.22 cancels its client threads asynchronously and
  joins them without a bound, one of them inside a process-wide mutex; read in its code, not seen.
  A hang of the JACK tests during `scripts/check.sh` on 2026-11-14 was never reproduced in 248
  runs; a watchdog now ends any JACK test stuck 120 s, with a diagnosis
  (`crates/magnolia-jack/tests/common/mod.rs`).
- **The test server's command line is written in three places** (`ARCHITECTURE.md`,
  `CONTRIBUTING.md`, the tests' `common/mod.rs`): to keep in the code and link from the documents.

From the [audit of 2026-11-17](audits/2026-11-17-architecture.md), each with the milestone that
follows it:

- **Ids before MCP edits** (F7): tracks are numbered by position and plugin instances are arena
  slots; `EventKind::Parameter` names a parameter's id `index`. M1.8 gives tracks and instances
  ids the engine assigns, before its server; Stage 2 takes them over as persistent ids.
- **The method's change of 2026-11-16** (F10): amendment chains instead of superseding ADRs, no page
  first in `ARCHITECTURE.md`, no change or compatibility scenarios in the quality bar, an ADR index
  written by hand, most items here without a milestone, and this file's header stating the old
  rule; the local rule on abstractions, to align on the owner's go. M1.6 supersedes the ADRs it
  changes; Stage 1's audit pays the rest.
- **The quality bar says Miri runs in CI; no step runs it** (F10): all `unsafe` today wraps libjack
  or libc, which Miri cannot run. Added with M1.6's first `unsafe` of its own.
- **Nothing says contribution is closed** (F11): ADR-0002 merges no outside contribution before a
  policy, and neither `README.md` nor `CONTRIBUTING.md` says so. One sentence each, with M1.6's
  first documents; the policy, after legal advice, before contributions open.
- **Guards that apply only by choice** (F12): the no-panic lints are inner attributes of 39 files,
  `clippy.toml`'s bans hold for every crate, and nothing forbids `unsafe` in the engine. M1.6, with
  its first crates off the real-time path.
- **SQLite's WAL and network filesystems** (F14): ADR-0009 does not say; the recording folder's lock
  is unverified on NFS and SMB. Stage 2's design.
- **One snapshot per track, not per clip** (F15): ADR-0007 says one per clip; a timeline edit is a
  full commit; the read-ahead does not scale with the tracks. Stage 2's design.
- **The collector's thread is never joined, and an overflow of its queue leaks without a counter**;
  the sync queue is unbounded, bounded in practice by the takes. The next milestone that touches
  `engine/collector.rs` or `record/syncer.rs`.
- **A second backend has no written contract** beyond `DeviceBuffers`: replacing a poisoned
  processor, the counters, the real-time report and freewheel are told in the JACK section alone.
  The stage of the next backend.
- **The test plugins' build is unplanned**, while the full check takes 153 s of its 3 minutes. M1.6's
  design session.
