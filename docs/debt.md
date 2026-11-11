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
- **An event due now can wait behind events far ahead**: the processor takes events in only while
  its pending buffer has room (1,024 by default), so a sender that keeps that many events ahead holds
  back one due now. Decided at M1.4, when the timeline's own events come.
- **Paths without a test**: a generation mismatch (`processor.rs`), and the silence the JACK callback
  writes while the server's rate differs from the engine's, since a JACK2 server cannot change its
  rate while it runs.
- **`missed_plugin_deadlines` is never written** before M1.6, which brings deadlines; its test checks
  that it stays at zero.
- **The facts read in PipeWire's and JACK2's code** (in ADR-0022 and the JACK backend's section of
  `ARCHITECTURE.md`) name no file or version; their sources are in the M1.3 design challenge's notes,
  not yet in [`survey.md`](survey.md).
- **The test server's command line is written in three places** (`ARCHITECTURE.md`,
  `CONTRIBUTING.md`, the tests' `common/mod.rs`): to keep in the code and link from the documents.
