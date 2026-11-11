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
| `magnolia-engine` | The audio engine ([ADR-0007](adr/0007-the-audio-engine.md)): the graph of nodes, its swaps and its real-time processor (module `engine`), the node contract (`node`) and the engine's own nodes (`nodes`), what a backend lends each period (`device`), a backend without a device (`offline`), the engine's statistics (`stats`, `callback_times`), and the time model (`time`). No audio backend yet. | `rtrb` |

Each crate's fuzz targets live in its `fuzz/` folder, a workspace of its own, so that the
workspace's builds never compile libFuzzer. Its benchmarks live in `benches/`, built on the standard
library alone.

## The time model

`magnolia_engine::time` says where every position lies
([ADR-0018](adr/0018-the-time-model.md)). It has four grids, each a type that implements `Grid`:

| Grid | Positions | What it measures |
|---|---|---|
| `SampleRate` | `SamplePosition` | samples, what the audio device plays |
| `TimeUnits` | `TimePosition` | absolute time in time units, whatever the sample rate |
| `TempoMap` | `MusicalPosition` | ticks, laid on time units by tempo changes from the song start |
| `TimecodeClock` | `FramePosition` | timecode frames, laid on time units from a start label |

Every conversion goes through a `Moment`, the exact instant of a position, computed with integers:

```text
position on grid A --moment_of--> Moment --first_at_or_after (place)--> position on grid B
                                         --last_at_or_before (read)---->
```

`place` and `read` are the only two ways to convert, and the only steps that round. No conversion
goes through a third grid, so a position's frame or bar never depends on the sample rate. The
engine calls `TempoMap::ticks_in_block` for each block, then places each event of those ticks on
its sample.

A tempo map is built off the real-time thread and never changes; its lookups are binary searches
that neither allocate nor lock. `tests/time_over_ten_hours.rs` checks the rule on every grid over
ten hours; `tests/common/mod.rs` holds the checks it shares with the fuzz targets.

## The engine

`magnolia_engine::engine` plays a graph of nodes ([ADR-0007](adr/0007-the-audio-engine.md)). A
node implements the contract of [ADR-0021](adr/0021-the-node-contract.md): fixed channel counts,
and a `process` that returns *continue*, *sleep* or *error*. `engine::new` makes the engine's two
halves and starts its collector.

### Threads and what each owns

| Part | Runs on | Owns |
|---|---|---|
| `Controller` | any thread but the audio one | the graph's description being edited, the nodes not yet sent, the slots and their generations |
| `Processor` | the audio thread: a backend calls `process` once a period | the compiled graph being played, the arena of nodes, the buffer pool, the pending events |
| collector | a thread of its own, `magnolia-collector` | nothing for long: it drops what the processor lets go |

The halves share no memory but three queues and the statistics. The queues are rtrb's
single-producer, single-consumer rings, allocated once, which never lock or block:

| Queue | From, to | Carries | Capacity |
|---|---|---|---|
| swaps | controller, processor | one message per swap: the compiled graph, its new nodes, new chunks of the arena, a larger buffer pool when needed | 2: one swap is in flight at most |
| events | controller, processor | timed events: a node, a sample position, a parameter's new value | `EngineConfig::event_capacity` |
| garbage | processor, collector | old graphs, old pools, used messages, the nodes a swap dropped, a panic's payload | `EngineConfig::garbage_capacity` |

`EngineStats` goes the other way, atomics only: the heartbeat (periods run), the start of the last
period, the last swap applied and the last one steady, counts of node errors, events dropped,
non-finite samples, generation mismatches and missed plugin deadlines, the poisoned flag, the
histogram of callback times, and the counters a backend fills (its own overruns, gaps in the
device's frame time, the server's xruns, the audio thread's id).

### A swap

1. The controller's edits (`add_node`, `remove_node`, `connect`, `disconnect`) change a
   description. `commit` checks it whole: a node unknown, a channel out of range, an edge held
   twice and a cycle are each an error of their own, and the description stays as it was.
2. At most one swap is in flight. A commit made while one is waits; `poll` sends the last commit
   once the processor reports the swap in flight steady. Edits made after a commit wait for the
   next one.
3. The controller compiles the committed description against the graph the processor plays,
   into **phases**, the last one **steady**. With `R` the edges removed and `A` the edges added:
   the first commit, or a commit that adds or removes no edge, is one steady phase. Otherwise,
   if the union of both graphs is acyclic and holds at most 1.25 times the nodes of the larger, a
   **crossfade**: one phase runs the union. Otherwise a **dip**: the old graph with every edge of
   `R` fading out, then the new one with every edge of `A` fading in. Each fading phase lasts one
   fade, a raised cosine over 5 ms (240 samples at 48 kHz); on one edge, a fading-in gain and its
   fading-out gain sum to 1 at every sample.
4. In a crossfade, each path that changes carries one fade, so that the level holds when a node is
   inserted or removed: gains multiply along a path, and a fade on each of its edges would dip it
   by 2.5 dB at mid-fade. An edge of `R` fades out, unless it goes into a node the swap drops,
   whose own outgoing edges fade out; an edge of `A` fades in, unless it comes from a node the swap
   adds that has an input edge, whose input edges fade in. A new node without an input edge, a
   source, fades its own edges in, so that its sound never steps in. Two fades can still meet on
   one path: an existing node put in series fades in on both of its new edges (out on both of its
   old ones when taken out of series), and a removed edge followed by an added edge through a node
   both graphs hold makes a path neither graph has, faded out and in at once. A dip keeps a fade on
   every edge: a node reordered among three edges fades as the cube of one fade, to 0.125 (-18 dB)
   at mid-fade, then meets silence at the junction like any dip.
5. The processor applies the message whole at a period's start, and only when the collector's
   queue has room for everything it replaces: the old pool, the used message, the old graph.
   Otherwise it leaves the message queued for a later period, while events go on.
6. It cuts each period into sub-blocks no longer than the largest block, and at each phase's last
   sample, so a phase ends at its exact sample at every period, and nothing separates a dip's two
   fades. A node is never given more frames than the largest block.
7. At the steady phase it moves the nodes the swap dropped out of the arena, into a vector the
   compiled graph reserved for them, sends that to the collector, and publishes the swap's
   sequence number. Only then does the controller reuse their slots, each under its next
   generation; a step whose slot holds another generation writes silence and is counted.

### A period

`Processor::process` runs on the real-time thread. It never allocates, frees, locks or blocks;
its one system call wakes the collector ([ADR-0023](adr/0023-the-real-time-allowlist.md)).

1. Poisoned, it writes silence on every playback channel and goes to the last step.
2. Otherwise it runs the period inside `catch_unwind`. A panic poisons it: silence from then on,
   the flag set, every later period returning normally
   ([ADR-0022](adr/0022-a-panic-on-the-real-time-path-gives-silence.md)).
3. It applies at most one swap, then pops the queued events, as many as its pending buffer holds,
   whether a swap waits or not: the controller never sends an event before its node.
4. For each sub-block it copies the capture in, a buffer shorter than the period taken as
   silence. For each step it sums the edges into the node's inputs with their gains; an input
   with nothing sounding holds zeros and is flagged silent. It skips a node that slept, whose
   inputs are still silent and which has no event, writing silence in its place; otherwise it
   calls the node, and silences and counts its outputs if it returns an error. Last it sums the
   playback channels and copies them out, a non-finite sample replaced by zero and counted, a
   buffer shorter than the period left as it is.
5. It hands what it replaced to the collector and wakes it if it handed anything. Every period,
   poisoned or not, then advances the position, counts in the heartbeat and records its time.

Every module the period runs denies itself panicking code: Clippy's `indexing_slicing`,
`unwrap_used`, `expect_used`, `panic` and `arithmetic_side_effects` are warnings there, which
`scripts/check.sh` turns into errors.

### Events

A timed event names a node and a sample on the engine's timeline; the controller stamps it from
the start of the last period, which `EngineStats` publishes. The processor keeps popped events
in a pending buffer; each sub-block takes those due before its end, sorts them in place by node,
sample and arrival, and gives each node its own with offsets from the sub-block's start. A late
event plays at the first sample of the next period.

An event can wait in four places, each for a reason:

- **in the controller**, when its node comes with a swap the processor has not applied yet, not
  sent or still queued: the controller holds it until `last_swap_applied` reaches that swap, so
  that it never reaches the processor before its node, and so that the processor can pop every
  event even while a swap waits for the collector. `poll`, `wait_until_steady` and the next
  `send_event` send what no longer waits; later events for the same node wait behind it, to keep
  their order. The controller holds as many events as the queue at most, then refuses with
  `QueueFull`. Such an event plays one period late at least, if its sample falls in the period
  that applies the swap;
- **in the queue**, while the processor's pending buffer is full;
- **in the pending buffer**, until the sub-block it is due in;
- **in the pending buffer through a dip's first phase**, when its node is one that the dip
  brings: that node runs only from the second phase.

An event for a node the current phase does not run, and no later phase of its swap, is dropped
and counted.

### Memory the real-time thread touches

A page of fresh memory is mapped when it is first written, and a page fault in a period costs
time that the sanitizer cannot see. So every page the processor will touch is written before it
gets it:

- **one buffer pool** for the engine: an input region (the playback channels, then each node's
  inputs) and an output region (the capture channels, then each node's outputs), so a node reads
  its inputs and writes its outputs from two disjoint slices, without `unsafe`. Each channel is
  `stride` samples long: the largest block, rounded up to 16 floats. Compiled graphs hold indices
  into it; a larger pool comes in a swap's message only when a graph needs more channels;
- **the arena**: a table of chunks of 64 slots, allocated whole for 4,096 chunks, so adding a
  chunk never reallocates and a node never moves;
- **the event buffers** and **the vectors reserved for dropped nodes**.

The controller writes every page of each before sending it, through `black_box`, since the
compiler may turn a fresh allocation's zero fill into `calloc`, which leaves pages unwritten
(`engine/memory.rs`). The garbage queue, which the processor writes first, is warmed by pushing
and popping through every slot at the start. `tests/engine/page_faults.rs` checks that the
processing thread's minor faults do not change across swaps.

## Checking the real-time path

The engine's real-time functions are marked
`#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]`; so far `Processor::process`, the
period's entry. Built on the nightly with
`-Zsanitizer=realtime --cfg magnolia_rtsan`, RealtimeSanitizer ends the process at the first
allocation, free or system call made inside a marked function, on any thread
([ADR-0013](adr/0013-a-pinned-stable-toolchain-and-a-nightly-for-checks.md),
[ADR-0019](adr/0019-the-sanitizers-nightly-code-behind-a-cfg.md)). Elsewhere the attribute is
not there.

Its limit, measured on the pinned nightly: an uncontended `std::sync::Mutex` or `RwLock` and
`thread::sleep` pass unreported. std's locks on Linux use a futex directly, which the sanitizer sees
only when a lock is contended and waits; `thread::sleep` calls `clock_nanosleep`, which it does not
intercept. A clean run therefore does not prove that the real-time path takes no lock. Clippy
covers the gap: `clippy.toml` forbids std's locks, sleeps and blocking waits in the engine's crates,
and each use off the real-time path carries an `#[expect]` that names its thread
([ADR-0020](adr/0020-clippy-forbids-the-locks-the-sanitizer-cannot-see.md)).

The allowlist's calls are suppressed by name ([ADR-0023](adr/0023-the-real-time-allowlist.md)):
`rtsan-suppressions.txt`, at the workspace's root, holds one `call-stack-contains:` line per
function of Magnolia's that makes an allowed call, so far `wake_collector`; never an intercepted
function such as `syscall`, which a lock's wait goes through too. The names match only in
symbolized reports, so every run sets `allow_addr2line=true`, with binutils' `addr2line` from the
development shell.

Because the sanitizer ends the process, a test of it runs each case in a child process and reads
its exit status and standard error: `crates/magnolia-engine/tests/realtime_sanitizer.rs`. It sets
the options of each run itself: `halt_on_error=true`, `allow_addr2line=true`, and the suppressions.
Its cases:

- an allocation in a marked function is stopped at `malloc`;
- work on memory allocated beforehand, a block's conversions of time, and the engine's processor
  through a crossfade, a dip, a larger pool, events, removed nodes handed to the collector, a
  sleeping node and a failing node, all stay silent;
- the collector's wake, without the suppressions, is stopped at `syscall`; with them, it passes;
- a lock's wait right after the wake is still stopped, at the `syscall` of the futex wait.
