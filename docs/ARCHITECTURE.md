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
| `magnolia-engine` | The audio engine ([ADR-0007](adr/0007-the-audio-engine.md)): the graph of nodes, its swaps and its real-time processor (module `engine`), the node contract (`node`) and the engine's own nodes (`nodes`), what a backend lends each period (`device`), a backend without a device (`offline`), the engine's statistics (`stats`, `callback_times`), the time model (`time`), and WAV files read, decoded and written by its own code (`wav`, [ADR-0029](adr/0029-magnolias-own-wav-and-midi-file-readers.md)). It holds no audio backend: backends are crates of their own. | `rtrb` |
| `magnolia-jack` | The JACK backend: plays the engine's processor through the JACK API, on PipeWire and JACK2, and says whether its audio thread runs in real time ([below](#the-jack-backend)). | `magnolia-engine`, `jack`, `libc`, `rtrb` |
| `magnolia-session` | The test session of M1.4, which only the tests and the command line read and Stage 2's project replaces ([ADR-0028](adr/0028-the-test-session-in-json.md)). For now, Standard MIDI Files of format 0 and 1 read and written by its own code (`midi_file`, [ADR-0029](adr/0029-magnolias-own-wav-and-midi-file-readers.md)). | none |

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
node implements the contract of [ADR-0021](adr/0021-the-node-contract.md): fixed channel counts, a
latency fixed for its life, and a `process` that returns *continue*, *sleep* or *error*. A
transport plays the timeline, pre-rolled by the graph's largest compensation, and nodes may have
timeline sources, MIDI sequences, whose events they receive as the transport rolls.
`engine::new` makes the engine's two halves and starts its collector.

### Threads and what each owns

| Part | Runs on | Owns |
|---|---|---|
| `Controller` | any thread but the audio one | the graph's description being edited, the nodes not yet sent, the slots and their generations |
| `Processor` | the audio thread: a backend calls `process` once a period | the compiled graph being played, the arena of nodes and the notes their sources hold, the buffer pool, the pending events, the transport |
| collector | a thread of its own, `magnolia-collector` | nothing for long: it drops what the processor lets go |

The halves share no mutable memory but four queues and the statistics; the timeline's snapshots,
which never change, cross inside compiled graphs ([below](#timeline-sources)). The queues are
rtrb's single-producer, single-consumer rings, allocated once, which never lock or block:

| Queue | From, to | Carries | Capacity |
|---|---|---|---|
| swaps | controller, processor | one message per swap: the compiled graph, its new nodes, new chunks of the arena, a larger buffer pool when needed | 2: one swap is in flight at most |
| events | controller, processor | timed events: a node, a sample position, and a note on or off, a pitch bend, a controller's or a parameter's new value | `EngineConfig::event_capacity`, which also bounds the events queued and pending together ([below](#events)) |
| transport | controller, processor | play from a timeline sample, numbered, and stop ([below](#the-transport)) | 4 |
| garbage | processor, collector | old graphs, old pools, used messages, the nodes a swap dropped, a panic's payload | `EngineConfig::garbage_capacity` |

`EngineStats` goes the other way, atomics only: the heartbeat (periods run), the start of the last
period, the last swap applied and the last one steady, where the last play started, counts of node
errors, events taken and events dropped, non-finite samples, generation mismatches, latencies capped and missed plugin
deadlines, the poisoned flag, the histogram of callback times, and the counters a backend fills
(its own overruns, gaps in the device's frame time, the server's xruns, the audio thread's id).

### A swap

1. The controller's edits (`add_node`, `remove_node`, `connect`, `disconnect`) change a
   description; `set_sequences` and `set_tempo_map` change the timeline sources it holds beside.
   `commit` checks the description whole: a node unknown, a channel out of range, an edge held
   twice and a cycle are each an error of their own, and the description stays as it was. Then it
   compensates the nodes' latencies ([below](#delay-compensation)): the graph it sends holds a
   delay line wherever paths of different latencies meet; the description never does.
2. At most one swap is in flight. A commit made while one is waits; `poll` sends the last commit
   once the processor reports the swap in flight steady. Edits made after a commit wait for the
   next one.
3. The controller compiles the committed graph against the graph the processor plays, delay lines
   included, into **phases**, the last one **steady**. With `R` the edges removed and `A` the
   edges added:
   a commit that adds or removes no edge is one steady phase. Otherwise,
   if the union of both graphs is acyclic and holds at most 1.25 times the nodes of the larger, a
   **crossfade**: one phase runs the union. Otherwise a **dip**: the old graph with every edge of
   `R` fading out, then the new one with every edge of `A` fading in. Each fading phase lasts one
   fade, a raised cosine over 5 ms (240 samples at 48 kHz); on one edge, a fading-in gain and its
   fading-out gain sum to 1 at every sample. A graph the processor installs before its first
   period starts at its steady phase: nothing has sounded to fade from. Why a crossfade or a dip,
   and the limit of 1.25 times: [ADR-0024](adr/0024-how-a-swap-avoids-a-click.md).
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

### Delay compensation

A node declares its latency, `Node::latency`, which `add_node` reads once: a plugin that answers
`L` samples late, such as a lookahead limiter
([ADR-0025](adr/0025-delay-compensation-by-reading-ahead.md)). Above `MAX_LATENCY`, 65,536
samples, it is taken as `MAX_LATENCY` and counted. `nodes::TestLatency` is a pure delay that
declares its delay, to stand for a latent plugin until plugins come (M1.6).

At each commit the controller works out, from the playback channels back, how far ahead of the
output each node runs (`engine/compensation.rs`):

- a playback channel requires its signal 0 samples ahead;
- a node is **audible** when it reaches a playback channel, directly or through other nodes; a
  node that is not demands nothing, so that a latent node whose output goes nowhere delays no
  one;
- a node is **live** when a capture channel feeds it, directly or through other nodes;
- a node's output runs as far ahead as the most demanding audible node it feeds requires, or 0
  when it feeds none; a live node's, as the least demanding, so that nothing live is ever delayed:
  a voice through a latent plugin is late by that plugin's latency, never more;
- its input, `ahead_in`, runs as far ahead as its output plus its own latency.

An edge from a node that is not live into an audible node or a playback channel needs a delay of
what its source runs ahead less what its destination requires; an edge out of a live node or a
capture channel, or into a node that is not audible, needs none. One **delay line** serves each
destination (a node, or the playback channels) and delay: every edge that needs that delay there
goes into the delay line's channel of the same number, whose output feeds the destination's. Three
sources sent both to the output and to a latent bus share one delay line on their direct paths.

Delay lines are nodes of the arena (`engine/delay_line.rs`) that the controller adds and removes;
the user's description never holds them. A commit that keeps a delay line's destination and delay
keeps its node, and so its state: its sound goes on without a fade. A commit that changes the delays
changes edges, so it crossfades or dips like any other; but a path that moves into a new delay line,
which starts empty, is silent for that line's delay ([debt](debt.md)). A delay line's history is
allocated and written by the controller (`engine/memory.rs`); it sleeps once its inputs have been
silent for its delay and a whole sub-block. Each compiled step records its node's `ahead_in`, and
the compiled graph the largest of the audible nodes, the session's largest compensation, by which
the transport pre-rolls ([below](#the-transport)).

### The transport

The processor keeps two clocks ([ADR-0025](adr/0025-delay-compensation-by-reading-ahead.md),
`engine/transport.rs`). The engine's clock counts every sample of every period, rolling or not; the
controller stamps events with it. The transport's clock is the timeline sample the output plays,
and moves only while the transport rolls. A node sees the transport at its own input, the clock
plus its `ahead_in`: `ProcessContext::transport` gives it whether the transport rolls, the timeline
sample of the sub-block's first frame at its input, and the **audible region**, the timeline
samples that play.

`Controller::play(from)` and `Controller::stop()` push commands into a queue of their own; a full
queue is an error, `TransportError::QueueFull`. The processor applies them at a period's start,
after any swap. With `P` the largest compensation of the graph about to play:

- **Play**, stopped: the clock starts at `from - P`, the **pre-roll**. The most latent source reads
  `from` at once, and every path reaches the output on `from` together, `P` samples after the
  period's start. `EngineStats::play_start` publishes that engine sample with the play's sequence
  number, which `play` returned. The audible region is `[from, stop)`, its stop unknown.
- **Stop** at clock `c`: if `c + P <= from`, no node has read anything audible, so the region is
  empty, its stop at `from`, and the transport stops at once, silent; that happens when a play and
  its stop reach one period. Otherwise `stop = c + P + F`: what the most latent node has read, plus
  `F`, the 5 ms of a fade ([ADR-0024](adr/0024-how-a-swap-avoids-a-click.md)). So a stop that
  comes in a later period of the pre-roll still plays what was read, at least one period, plus the
  fade, as ADR-0025 means: every path ends on the same sample, whatever has been read. Each node's
  sources end its notes as its position reaches `stop`, the most latent node first, so every path
  ends on the same timeline sample. The transport rolls until its clock has played `stop` itself,
  where the nodes nearest the output reach it; the processor cuts a sub-block there, so the
  transport never stops inside one. Its clock then holds `stop + 1`, the next sample it would play.
- **Play while rolling**: a stop as above, then the play at the first period after the stop ends;
  at once when the stop is.
- A stop while stopped does nothing; a stop cancels a play that waits for its stop.

A swap that changes `P` while the transport rolls keeps the running clock: a node whose
compensation changes jumps by the difference ([debt](debt.md)).

### Timeline sources

A node may have **timeline sources**: MIDI sequences from M1.4, automation lanes from M1.6
([ADR-0027](adr/0027-timeline-events-from-sources-on-nodes.md)). A `MidiSequence` is built off the
real-time thread from one track's clips (`engine/sequence.rs`) and never changes. Each note becomes
a note on at its start and a note off at its end or its clip's end, whichever comes first; the
events are sorted by tick, and at one tick the note offs come first, then bends and controllers,
then note ons. It also keeps each channel's pitch bends and each controller's values, sorted by
tick, for the chase.

`Controller::set_sequences(node, sequences)` and `Controller::set_tempo_map(map)` change what the
controller holds; the next commit sends them inside the compiled graph, each step naming its node's
sequences. A replaced sequence or map reaches the collector inside the old graph, so the processor
never frees one. Without a tempo map, sequences play nothing.

For each sub-block, for each step with sequences, while the transport rolls, the processor's source
reader (`engine/sources.rs`):

1. finds the ticks placed in the samples that the node's position covers and the audible region
   holds (`TempoMap::ticks_in_block`), each sequence's events at those ticks by a binary search,
   and each event's sample by `place` ([ADR-0018](adr/0018-the-time-model.md));
2. at the region's start, sends first, for each sequence, the last pitch bend per channel and the
   last value per channel and controller before it: the **chase**, a binary search per lane;
3. leaves out a note that began before the region's start, with its note off, and a note shorter
   than a sample, whose note off would come first;
4. sorts what it found by offset, note offs first, then in the order found, and walks it in that
   order, the order the node receives it: a note on holds its note, a note off ends a held note and
   is left out for a note not held, one that never sounded. So a note ended and struck again on one
   sample stays held, whichever sequences, or ticks within a sample, the two came from;
5. once the node's position reaches the region's stop, or has passed it in one jump, as when a
   swap raises the node's compensation, sends a note off for every note the node holds, then, on
   each channel its sequences use, the sustain pedal (controller 64) at 0 and the pitch bend at 0:
   at the stop's offset, or at the sub-block's start after a jump. A node gets these once per play,
   and nothing more from its sources until the next play, even if a swap moves it back before the
   stop;
6. merges what it kept with the controller's events for the node ([below](#events)).

What a node's sources hold, the notes one bit per channel and key and the last play they stopped,
lives in its slot of the arena beside its sleep flag, so it survives swaps: a stop ends the node's
notes even when its sequences changed meanwhile, or its compensation moved its position past the
stop. A sleeping node with a timeline event runs, like one with a controller's event.

A node's sources give it at most `EngineConfig::event_capacity` events in one sub-block; past that,
events are dropped and counted in `events_dropped`. A note on dropped is never held, so its note off
is never sent; a note off dropped leaves its note held until the stop. Which events a sub-block
holds depends on how the period was cut, so once some are dropped, a render no longer nulls with
playback.

`nodes::TestSynth` plays notes until plugins come (M1.6): 16 voices, each a sine and a sawtooth at
half its level, with a linear attack and release of 5 ms whose step is fixed per sample, every
change at its event's sample, so that its output does not depend on how blocks are cut.

### A period

`Processor::process` runs on the real-time thread. It never allocates, frees, locks or blocks;
its one system call wakes the collector ([ADR-0023](adr/0023-the-real-time-allowlist.md)).

1. Poisoned, it writes silence on every playback channel and goes to the last step.
2. Otherwise it runs the period inside `catch_unwind`. A panic poisons it: silence from then on,
   the flag set, every later period returning normally
   ([ADR-0022](adr/0022-a-panic-on-the-real-time-path-gives-silence.md)).
3. It applies at most one swap, then the transport's commands, then pops every queued event into
   its pending buffer, which has room for them all, whether a swap waits or not: the controller
   never sends an event before its node, nor more events than the buffer holds.
4. For each sub-block it copies the capture in, a buffer shorter than the period taken as
   silence. For each step it sums the edges into the node's inputs with their gains; an input
   with nothing sounding holds zeros and is flagged silent. It reads the node's timeline sources
   at the node's position. It skips a node that slept, whose inputs are still silent and which has
   no event, from the controller or its sources, writing silence in its place; otherwise it calls
   the node with the transport at its input, and silences and counts its outputs if it returns an
   error. Last it sums the playback channels and copies them out, a non-finite sample replaced by
   zero and counted, a -0.0 written as +0.0, a buffer shorter than the period left as it is. The
   transport's clock, rolling, moves on by the sub-block. Sub-blocks are cut also where the
   transport stops.
5. It hands what it replaced to the collector and wakes it if it handed anything. Every period,
   poisoned or not, then advances the engine's clock, counts in the heartbeat and records its time.

Every module the period runs, the time model included, denies itself panicking code:
Clippy's `indexing_slicing`, `unwrap_used`, `expect_used`, `panic` and `arithmetic_side_effects`
are warnings there, which `scripts/check.sh` turns into errors.

### Events

A timed event names a node and a sample on the engine's timeline, and says what happens: a note on
or a note off (an id or none, a MIDI channel, a key, a velocity from 0 to 1), a pitch bend from -1
to 1, a controller's value from 0 to 1, or a parameter's value
([ADR-0027](adr/0027-timeline-events-from-sources-on-nodes.md)). A node ignores the kinds it has no
use for. The controller stamps an event from the start of the last period, which `EngineStats`
publishes. The processor keeps popped events in a pending buffer, each with its own sample; each
sub-block takes those due before its end, sorts them in place by node, sample, note offs first,
and arrival, and gives each node its own with offsets from the sub-block's start: at one sample, a
node gets its note offs first, then the rest in the order they were sent. Its timeline sources'
events join them ([above](#timeline-sources)): at one sample, the note offs first, then the
timeline's events before the controller's, each list in its own order. A late event plays at the
first sample of the next period, offset 0, still after the late events of earlier samples: a note
played and released before the period comes on, then off.

The processor never holds more events than `EngineConfig::event_capacity`, queued and pending
together, so that one due now never waits behind events far ahead. `EngineStats` counts the events
it has taken out of its pending buffer, delivered or dropped; the controller counts those it has
pushed into the queue, and refuses a new one with `QueueFull` once the difference reaches the
capacity. The processor publishes its count after the events have left the buffer, so a count read
late only makes the controller refuse early. Every queued event then fits in the pending buffer,
and each period pops the whole queue. The caller sends a refused event again after a period.

An event can wait in four places, each for a reason:

- **in the controller**, when its node comes with a swap the processor has not applied yet, not
  sent or still queued: the controller holds it until `last_swap_applied` reaches that swap, so
  that it never reaches the processor before its node, and so that the processor can pop every
  event even while a swap waits for the collector. `poll`, `wait_until_steady` and the next
  `send_event` send what no longer waits; later events for the same node wait behind it, to keep
  their order. The controller holds `event_capacity` events at most, then refuses with
  `QueueFull`; a held event counts against the processor's capacity only once pushed. Such an
  event plays one period late at least, if its sample falls in the period that applies the swap;
- **in the queue**, until the next period's start;
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
  chunk never reallocates and a node never moves; each slot holds its node, its sleep flag and the
  notes its sources hold;
- **the event buffers**, **the source reader's buffers**, **the vectors reserved for dropped
  nodes** and **each delay line's history**.

The controller writes every page of each before sending it, through `black_box`, since the
compiler may turn a fresh allocation's zero fill into `calloc`, which leaves pages unwritten
(`engine/memory.rs`). The garbage queue, which the processor writes first, is warmed by pushing
and popping through every slot at the start. `tests/engine/page_faults.rs` checks that the
processing thread's minor faults do not change across swaps, those that add or remove a delay line
included.

## The JACK backend

`magnolia_jack::JackBackend` plays an engine's processor through the JACK API, which PipeWire and
JACK2 both serve ([ADR-0007](adr/0007-the-audio-engine.md)). The `jack` crate loads libjack when
the first client opens, so a program that never opens one runs without it. Run through `pw-jack`,
the process loads PipeWire's libjack; otherwise the first `libjack.so.0` on its library path.

`JackBackend::open` connects to a running server and never starts one (`NO_START_SERVER`). It reads
the engine's rate and channels from the processor. It fails with a plain error when libjack is
missing, when no server runs, and when the server's rate differs from the engine's. It registers
one port per engine channel, `capture_1` and on, `playback_1` and
on, and activates the client: the server calls the processor from its next period, capture and
playback in one callback.

### Its threads and callbacks

| Part | Runs on | Does |
|---|---|---|
| the handle, `JackBackend` | the caller's thread | opens and closes the client, hands a replacement processor to the callback and drops the one it replaced, asks whether the audio thread runs in real time |
| the process callback | JACK's audio thread, once a period | swaps in a replacement, counts gaps and its own overruns, counts the server's xruns into the engine's statistics, calls `Processor::process` on the ports, or writes silence while the server's rate differs from the engine's |
| the thread-init callback | each thread JACK starts for the client: with JACK2 the audio thread, then its notification thread; with PipeWire the audio thread, before its first period | reads the thread's kernel id with `gettid` into the thread's own storage |
| the xrun, shutdown and sample-rate callbacks | JACK2's notification thread; PipeWire's audio thread for an xrun | count an xrun, note the shutdown, note the server's rate: atomics only |

Every callback but the process callback touches atomics only: PipeWire skips a process cycle while
a notification handler runs, and the shutdown callback must be async-signal-safe.

### What crosses the boundary

- **Two rtrb queues of one place each**: replacement processors, from the handle to the callback,
  and the processors they replaced, back. The callback swaps only when the queue back has room, so
  it never drops a processor; the handle drops it at its next `replace_processor`, `poll` or
  `close`.
- **Atomics shared by the three parts**: the audio thread's id, the server's xruns, the shutdown,
  the server's rate.
- **The engine's statistics**, `EngineStats`, of the processor being played: the callback fills its
  own overruns, gaps, server xruns and the audio thread's id there, beside the processor's own
  figures.

### What the callback counts

- **A gap**: a period whose first frame (`current_frames` of `jack_get_cycle_times`) does not
  follow on from the last one's, as when the server skipped the client's cycles. The frame time is
  32 bits and wraps, so the comparison wraps; the first period, and the first after a change of
  the period's length, are never gaps.
- **An own overrun**: a callback that started before the next period's estimated start
  (`next_usecs`) and ended after it, by `jack_get_time`, or whose work alone took longer than a
  period. A callback the server started after its deadline is late by the server's doing. On
  JACK2's dummy driver at 64 frames without real time, the server's clock estimate lags the wall
  clock by about 24 ms, measured, so every callback starts after its estimated deadline: there,
  only callbacks longer than a period count.
- **A server xrun**: each xrun callback. JACK2 reports any xrun in its graph, whoever caused it;
  PipeWire reports several between two cycles as one, and never the first.

While the server's rate differs from the engine's, the callback writes silence without calling the
processor, whose heartbeat then stops, until the rate is back.

### Real-time rights

The audio thread's id is read once, by the thread-init callback, into the thread's own storage;
the process callback publishes it at its first period. JACK2 calls the thread-init callback on its
notification thread too, so only the thread that runs the process callback can say which id is the
audio thread's. `JackBackend::realtime_status`, from any other thread, asks the kernel for that
thread's policy and priority (`sched_getscheduler`, the reset-on-fork flag masked, and
`sched_getparam`): the C library's cached copy is stale once rtkit has changed the policy from
outside ([ADR-0023](adr/0023-the-real-time-allowlist.md)). It names the server by the libjack the
process mapped (`/proc/self/maps`): PipeWire when the path names it, JACK2 otherwise. On JACK2 alone
it asks `jack_is_realtime`, which says whether the server runs with `-R`; PipeWire answers yes
whatever its threads' rights. Its report says, in plain words, how that server grants the rights.
rtkit grants them asynchronously, so a caller asks again after about a second.

### A poisoned processor, rebuilt in place

After a panic the processor writes silence and reports itself poisoned
([ADR-0022](adr/0022-a-panic-on-the-real-time-path-gives-silence.md)). The owner of the backend
makes a new engine at the same rate and channels, hands its processor to `replace_processor`, and
commits the graph again on the new controller. The callback swaps it in at a period's start: the
client stays active and its connections stay. A processor whose rate or channels differ from those
the client was opened for is given back unused (`ReplaceError::Differs`).

A panic in the callback's own code, outside the processor, is caught by the callback itself: it
writes silence from then on, returns normally so that the client stays active and connected on both
servers, and `callback_panicked` reports it. A new processor does not help there: close the backend
and open it again.

### Closing, and a server that dies

`close`, or dropping the backend, deactivates the client on the caller's thread and returns the
processor. When the server has stopped or died, JACK2 cannot deactivate the client: the `jack` crate
then keeps the callback's state, the processor with it, for the process's life, since the server
may still point into it. `close` returns an error that says so; the processor's memory and its
collector thread are not freed. The tests show that nothing hangs or crashes.

### The `jack` crate on the real-time path

Read in its code, not measured: around each process callback it takes a `std::sync::Mutex` with
`try_lock`, which never waits. When that lock is busy, the crate skips Magnolia's period and
returns `Quit`, which on JACK2 deactivates the client. Its buffer-size callback takes the same lock,
so a change of the period while a cycle runs could do it too, not only a closing client; on
PipeWire, its locked-process default keeps the two from running at once, as far as was read. Its own wrapper of the port-registration
notification locks a mutex and may allocate, on JACK2's notification thread or, on PipeWire, while
the process cycle is held. Its `log` feature is off, so libjack's messages go to standard error.

### Its tests

`crates/magnolia-jack/tests/jack/` runs each behaviour on a JACK2 server of its own, `jackd -n
<name> -r -d dummy -r 48000 -p <period>`, without real time and without a sound card, one test at
a time; a plain JACK client of the test's plays a signal into Magnolia and records it. Each test
refuses to run when the libjack it would load is PipeWire's (`tests/common/mod.rs`), which would
reach a live PipeWire. A server without real time on a busy machine may skip a client's cycle,
which breaks a recording; the tests that compare a recording sample by sample take it again when
the gap counters show a skipped cycle, five times at most.

## Checking the real-time path

The real-time functions are marked
`#[cfg_attr(magnolia_rtsan, sanitize(realtime = "nonblocking"))]`; so far `Processor::process`, the
period's entry, and the JACK backend's process callback around it. Built on the nightly with
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
its exit status and standard error: `crates/magnolia-engine/tests/realtime_sanitizer.rs`, and
`crates/magnolia-jack/tests/realtime_sanitizer.rs` for the backend. Each sets the options of each
run itself: `halt_on_error=true`, `allow_addr2line=true`, and the suppressions. The engine's cases:

- an allocation in a marked function is stopped at `malloc`;
- work on memory allocated beforehand, a block's conversions of time, and the engine's processor
  through a crossfade, a dip, a larger pool, events, removed nodes handed to the collector, a
  sleeping node, a failing node, swaps that add and remove a delay line, and MIDI sequences played
  through a `TestSynth` with a play, a stop and a play while rolling, all stay silent;
- the collector's wake, without the suppressions, is stopped at `syscall`; with them, it passes;
- a lock's wait right after the wake is still stopped, at the `syscall` of the futex wait.

The backend's cases, on a JACK2 server the test starts on the dummy driver:

- an allocation in a marked function is stopped at `malloc`;
- `gettid` in a marked function is not stopped: the sanitizer does not intercept it, so the
  thread-init callback's one call needs no suppression, though it runs outside any real-time
  context anyway;
- the backend playing the engine through periods, capture, events, a crossfade, a dip and a
  processor replaced in place stays silent: the sanitizer checks every call inside the process
  callback, libjack's included.
