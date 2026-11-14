# Known debt

What the project knows it owes: each item short, with why it stays and where it is tracked. The
[PLAN](PLAN.md) links here. An item leaves when a milestone that touches its code pays it; the
stage's audit triages the whole list.

- **A processor is lost when the JACK server dies**: JACK2 then refuses to deactivate the client,
  and the `jack` crate leaks its callback's context, the processor and its collector with it;
  `close` says so. Fixing it means bypassing the crate's `AsyncClient`
  ([ARCHITECTURE](ARCHITECTURE.md#the-jack-backend)).
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
- **The session's memory estimate repeats the engine's compensation rule** (`magnolia-session`'s
  `load/budget.rs` against `engine/compensation.rs`) and its rounding of streams and buffers: a
  change to either must change both until the engine exposes its own estimate; Stage 2's project
  replaces the test session.
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
  can leave a file that reads as finished but short, which no repair then takes up.
- **The session's memory budget copies the recorder's marks ring** (`load/budget.rs`, 256 marks
  of 64 bytes, against the engine's private `MARKS_CAPACITY`): an overestimate today, which will
  not follow a change of the engine's until it exports a tap's bytes.
- **The test server's command line is written in three places** (`ARCHITECTURE.md`,
  `CONTRIBUTING.md`, the tests' `common/mod.rs`): to keep in the code and link from the documents.
