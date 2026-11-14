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
| `magnolia-engine` | The audio engine ([ADR-0007](adr/0007-the-audio-engine.md)): the graph of nodes, its swaps and its real-time processor (module `engine`), the node contract (`node`) and the engine's own nodes (`nodes`), audio clips streamed from disk by the disk reader and played by the audio players (`disk`, [ADR-0026](adr/0026-one-stream-per-audio-track.md)), what a backend lends each period (`device`), a backend without a device (`offline`), the engine's statistics (`stats`, `callback_times`), the time model (`time`), and WAV files read, decoded, written and repaired by its own code, a take's sizes unknown until it ends and its `bext` chunk placing it on the timeline (`wav`, [ADR-0029](adr/0029-magnolias-own-wav-and-midi-file-readers.md), [ADR-0030](adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)). It holds no audio backend: backends are crates of their own. | `rtrb` |
| `magnolia-jack` | The JACK backend: plays the engine's processor through the JACK API, on PipeWire and JACK2, and says whether its audio thread runs in real time ([below](#the-jack-backend)). | `magnolia-engine`, `jack`, `libc`, `rtrb` |
| `magnolia-cli` | The command line, the binary `magnolia`, which drives Magnolia until the interface comes ([ADR-0006](adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md)): it plays a test session through the JACK backend and renders one offline ([below](#the-command-line)). | `magnolia-engine`, `magnolia-jack`, `magnolia-session` |
| `magnolia-session` | The test session of M1.4, which only the tests and the command line read and Stage 2's project replaces ([ADR-0028](adr/0028-the-test-session-in-json.md), [below](#the-test-session)): its JSON file and the checks that need no file on disk (`file`), its loader (`load`), the graph it builds into an engine (`build`), the offline render (`render`), a player for the command line (`play`), the generator of the reference projects (`generate`), and Standard MIDI Files of format 0 and 1 read and written by its own code (`midi_file`, [ADR-0029](adr/0029-magnolias-own-wav-and-midi-file-readers.md)). | `magnolia-engine`, `serde`, `serde_json` |

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
timeline sources, MIDI sequences, whose events they receive as the transport rolls. Audio clips
stream from disk: the disk reader fills one stream per audio track, which the track's audio player,
a node, plays. Armed tracks record: the processor's capture tap copies their capture channels into
rings, which the recorder writes into takes that survive a crash. `engine::new` makes the engine's
two halves and starts its collector; `disk::DiskReader::start` makes the disk reader and the
players; `record::Recorder::start` makes the recorder.

### Threads and what each owns

| Part | Runs on | Owns |
|---|---|---|
| `Controller` | any thread but the audio one | the graph's description being edited, the nodes not yet sent, the slots and their generations |
| `Processor` | the audio thread: a backend calls `process` once a period | the compiled graph being played, the arena of nodes and the notes their sources hold, the buffer pool, the pending events, the transport |
| collector | a thread of its own, `magnolia-collector` | nothing for long: it drops what the processor lets go |
| disk reader | a thread of its own, `magnolia-disk`; or the owner's, for an offline render | the audio tracks' clips and their files; it fills the streams the audio players play ([below](#audio-clips-from-disk)) |
| recorder | a thread of its own, `magnolia-recorder`, and its sync threads, `magnolia-sync` | the takes being written and their files; it drains the armed tracks' rings ([below](#recording)) |

The halves share no mutable memory but four queues and the statistics; the timeline's snapshots,
which never change, cross inside compiled graphs ([below](#timeline-sources)). The audio players,
nodes of the processor's arena, share their streams with the disk reader, atomics only. The
processor's capture tap shares two rings per armed track with the recorder's thread. The queues and
the rings are rtrb's single-producer, single-consumer rings, allocated once, which never lock or
block:

| Queue | From, to | Carries | Capacity |
|---|---|---|---|
| swaps | controller, processor | one message per swap: the compiled graph, its new nodes, new chunks of the arena, a larger buffer pool when needed | 2: one swap is in flight at most |
| events | controller, processor | timed events: a node, a sample position, and a note on or off, a pitch bend, a controller's or a parameter's new value | `EngineConfig::event_capacity`, which also bounds the events queued and pending together ([below](#events)) |
| transport | controller, processor | play from a cue, numbered, and stop ([below](#the-transport)) | 4 |
| garbage | processor, collector | old graphs, old pools, used messages, the nodes a swap dropped, the taps a swap disarmed, a panic's payload | `EngineConfig::garbage_capacity` |
| frames, one per armed track | capture tap, recorder | the track's takes' frames, interleaved ([below](#recording)) | `RecordConfig::ring_frames` frames, 4 s by default |
| marks, one per armed track | capture tap, recorder | a take's start, a gap, a take's end, each at its place among the frames | 256 |

`EngineStats` goes the other way, atomics only: the heartbeat (periods run), the start of the last
period, the last swap applied and the last one steady, where the last play started, counts of node
errors, events taken and events dropped, non-finite samples, generation mismatches, latencies capped,
missed plugin deadlines and the frames the audio players lacked (`disk_underruns`), the frames the
capture tap dropped (`capture_overruns`) and the plays it had no room to record (`missed_takes`),
the generations of disk streams handed out and the latest primed, the poisoned flag and the capture
tap's stopped flag, the histogram of callback times, and the counters a backend fills
(its own overruns, gaps in the device's frame time, the server's xruns, the audio thread's id, the
device's capture and playback latencies, in one word).

### A swap

1. The controller's edits (`add_node`, `remove_node`, `connect`, `disconnect`) change a
   description; `set_sequences` and `set_tempo_map` change the timeline sources it holds beside,
   `arm` and `disarm` the armed tracks' taps ([below](#recording)).
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
   Otherwise it leaves the message queued for a later period, while events go on. A message whose
   commit armed or disarmed a track also carries the change of the armed taps: the taps it arms, in
   a list with room for every tap it keeps, and those it keeps; the capture tap applies it in the
   same period, after the transport's commands, and the taps it retires go to the collector
   ([below](#recording)).
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
the transport pre-rolls ([below](#the-transport)). A live node counts in it only when it has
timeline sources: a live path reads nothing else ahead, so monitoring through a latent plugin
lengthens no play's pre-roll ([ADR-0032](adr/0032-recording-by-a-capture-tap-of-the-processor.md)).

### The transport

The processor keeps two clocks ([ADR-0025](adr/0025-delay-compensation-by-reading-ahead.md),
`engine/transport.rs`). The engine's clock counts every sample of every period, rolling or not; the
controller stamps events with it. The transport's clock is the timeline sample the output plays,
and moves only while the transport rolls. A node sees the transport at its own input, the clock
plus its `ahead_in`: `ProcessContext::transport` gives it whether the transport rolls, the timeline
sample of the sub-block's first frame at its input, the **audible region**, the timeline samples
that play, and the **generation** of the disk streams the play reads.

A play starts from a **cue**, `engine::Cue`: a timeline sample, `from`, and the generation of the
disk streams the play reads, which the disk reader's `locate` gave ([below](#audio-clips-from-disk)).
A sample alone is a cue of generation 0, which no stream holds: enough for MIDI. The engine numbers
generations, across every disk reader made on it (`EngineStats`), and a cue plays once:
`Controller::play` refuses one whose generation is not above every generation it has sent,
`TransportError::CueAlreadyPlayed`.

`Controller::play(cue)` and `Controller::stop()` push commands into a queue of their own; a full
queue is an error, `TransportError::QueueFull`. The processor applies them at a period's start,
after any swap. With `P` the largest compensation of the graph about to play:

- **Play**, stopped: the clock starts at `from - P`, the **pre-roll**. The most latent source reads
  `from` at once, and every path reaches the output on `from` together, `P` samples after the
  period's start. `EngineStats::play_start` publishes that engine sample with the play's sequence
  number, which `play` returned. The audible region is `[from, stop)`, its stop unknown. Every node
  sees the cue's generation from the play's start on.
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
- **A play that reads disk streams**, of a generation above 0, starts only at a period whose start
  finds that generation primed: `EngineStats::primed_generation`, which the disk reader publishes
  once every stream holds what the play reads first. Until then it waits, as a play while rolling
  waits for its stop, and the transport looks again at each period's start, never waiting itself.
  So `locate` then `play(cue)` is always right, rolling or not; the cost is the silence between a
  stop's end, or the command, and a late priming. `play_start` gives the period it did start.
- A stop while stopped does nothing; a stop cancels a play that waits, for its stop or its cue.

A swap that changes `P` while the transport rolls keeps the running clock: a node whose
compensation changes jumps by the difference ([debt](debt.md)).

### Timeline sources

A node may have **timeline sources**: MIDI sequences from M1.4, automation lanes from M1.6
([ADR-0027](adr/0027-timeline-events-from-sources-on-nodes.md)). A `MidiSequence` is built off the
real-time thread from one track's clips (`engine/sequence.rs`) and never changes. Each note becomes
a note on at its start and a note off at its end, its clip's end, or where a later note of its
channel and key starts, whichever comes first: within one sequence a key is never held twice, as in
the piano rolls of FL Studio and Ableton Live, and of notes of one key struck on one tick only the
last listed plays. The events are sorted by tick, and at one tick the note offs come first, then
bends and controllers, then note ons. It also keeps each channel's pitch bends and each controller's
values, sorted by tick, for the chase.

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
   is left out for a note not held: one that never sounded, or the second note off of a key that
   two of the node's sequences held at once, since the node holds one bit per channel and key
   ([debt](debt.md)). So a note ended and struck again on one sample stays held, whichever
   sequences, or ticks within a sample, the two came from;
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

### Audio clips from disk

Audio clips play from WAV files through one **stream** per audio track
([ADR-0026](adr/0026-one-stream-per-audio-track.md), module `disk`). The **disk reader** owns the
clips and fills the streams; each track's **audio player**, a node of the graph, plays its stream.

| Part | Runs on | Does |
|---|---|---|
| `DiskReader` | its owner's thread | made from the tracks with a thread (`start`) or without (`offline`), with one player per track; locates, waits until primed, fills (without a thread), reports |
| the reader's thread | `magnolia-disk` | sleeps until a locate or a player asks, then fills every stream that needs it |
| `AudioPlayer` | the audio thread, as a node | copies the frames of the play under way from its stream |

A track's clips must be at the engine's rate, have the track's channels, and not overlap; the
reader sorts them and refuses any other with a plain `DiskError`. Each file is opened once, its
header read by `wav::read_info`; its frames are read with `FileExt::read_at`, which moves no file
position.

**The stream** is a ring of `N` slots: the read-ahead in blocks of 4,096 frames, plus two. Block
`b`, the frames from `b × 4,096`, lives in slot `b mod N`. A slot holds the block's frames, channel after
channel, as the bits of `AtomicU32`s, and one atomic word: the low 32 bits of the generation it was
filled for, the block's number, whether it holds a block, and whether that block is silent. The
reader empties the word, writes the samples, then publishes the word (Release); the player reads the
word (Acquire), copies, reads the word again, and takes the block as missing if it changed. A slot
is not rewritten while it holds the block of the generation about to be written, so a copy that
straddles a write always sees the word change. Beside the ring, three things: the reader's word, the generation
it fills and the first block after those it filled in a row; the player's **cursor**, the generation
of its play, where it reads next, whether it rolls, and the play's stop once known, a sequence lock
the player writes without waiting; and the player's **low-water mark**.

**Locate, prime, play.** `DiskReader::locate(from)` takes the engine's next **generation** and
returns its cue; the reader fills every stream from `from`. Once every stream holds half a second
from `from` (half the read-ahead if shorter), or up to its last clip's end, the reader publishes the
generation primed, in `EngineStats`: after the run that primes it, without waiting to fill the whole
read-ahead. `Controller::play(cue)`, sent at once, starts at the first
period that finds it primed ([above](#the-transport)); `wait_primed(cue)` waits for the same, for a
caller that wants a timeout and an error. A player reads only slots of its play's generation, so
what an older locate filled never plays; a play from an older cue lacks every frame, counted.

**The reader** fills, round after round, the stream that holds the fewest frames ahead of its player
first, one **run** of consecutive blocks per stream and round, until each holds its window: the
read-ahead from where its player reads next, or from the cue's sample for a player that has not
started its play, up to its last clip's end. A run reads each clip it covers in one read, of at
least 256 KB when the clip covers the whole run: 11 blocks for 24-bit stereo. It renders the run from
zeros, then adds each clip's frames, decoded by `wav::decode`, times its gain, and, for a clip that
starts before the cue's sample, times the fade-in over the 5 ms from that sample (the swap's raised
cosine, `engine/fade.rs`); frames before the cue's sample stay zero. A block of zeros is marked
silent, and its samples are not written. Frames a file does not hold, past its end or after a failed
read, are zeros, counted in `DiskStats::unread_frames`. A stream is left until it lacks a whole run,
unless it holds less than half its read-ahead or its window reaches its last clip's end, so that
reads stay large. Each pass that fills something is a **refill**, timed in `DiskStats`.

**A player**, for each sub-block, at its own position, its compensation included, so that a player
on a latent path reads ahead by that latency ([ADR-0025](adr/0025-delay-compensation-by-reading-ahead.md)):

1. notes a new play, which starts a new low-water mark;
2. reads how many frames its stream holds ahead of what it needs, and records the fewest;
3. copies the frames of the audible region within its clips from slots of its play's generation, and
   writes zeros elsewhere. A frame it lacks is silence, counted in `EngineStats::disk_underruns`,
   never waited for;
4. fades out over the 5 ms before the play's stop, as a swap fades an edge out;
5. flags its outputs silent when it copied no block that sounds: over silent blocks, outside its
   clips, stopped;
6. publishes its cursor;
7. asks for a refill if it holds less than half the read-ahead, lacked a frame, or if its play's
   stop has just become known or it has just passed it: after its cursor, so that the pass the
   request starts sees where it reads next.

It never sleeps: it has no input that could wake it. A refill is asked through one flag shared by
every player, set once, and one wake, `disk::wake_disk_reader`, which calls `Thread::unpark`: a
futex wake when the reader sleeps, on the real-time allowlist
([ADR-0023](adr/0023-the-real-time-allowlist.md), [below](#checking-the-real-time-path)). The reader
clears the flag as each pass begins. Every set of the flag, a player's, a locate's or the end of a
pause, and its clearing, is a read-modify-write: a chain of them carries every cursor published
before a request to the pass that clears it, where a plain store would cut the chain.
`DiskStats::refill_latency` times each request that wakes the reader, to the end of the first run
that follows it.

**A play while rolling.** Its locate makes the reader fill the new generation at once, while the old
play still rolls. The reader leaves alone every slot that holds a block of the rolling play's
generation that the play may still read: everything ahead of its position until its stop is asked,
since it may roll on; once the player has published the stop, up to that stop, which holds every
path's compensation and the fade, whatever the graph. What is kept only ever shrinks, so a cursor
the reader read a moment ago never lets it overwrite what the play still reads. So the old play
fades out over its own audio. A block of the new generation whose slot is kept waits until the
player has passed it. A player asks for a refill when its play's stop becomes known, so that the
reader may write past it, and when it passes the stop, which frees all it kept, each time after the
cursor that says so, whatever it believes its stream holds: the reader may start the new
generation during the player's sub-block, and the new play waits for it. The new play waits for its
cue to be primed, then starts exactly at its sample.

**An offline render** uses a reader made by `DiskReader::offline`, without a thread: `fill` does the
reader's work on the caller's thread, and the render calls it before each block, no longer than half
the read-ahead, so that it never underruns.

Its limits:

- a play waits for its cue's priming without a timeout: if the reader's thread has stopped after a
  panic, the play never starts; `wait_primed` reports it (`PrimeError::ReaderStopped`);
- generations count modulo 2^32 in the slots; clips lie within 2^41 frames of the timeline's zero;
- `read_at` is Unix's: Windows' `seek_read` comes with the port; the reader's I/O priority is left
  as it is ([ADR-0026](adr/0026-one-stream-per-audio-track.md)).

### Recording

Armed tracks record into WAV takes that survive a crash
([ADR-0030](adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md),
[ADR-0031](adr/0031-the-round-trip-reported-and-takes-placed-by-it.md),
[ADR-0032](adr/0032-recording-by-a-capture-tap-of-the-processor.md), module `record`). The
processor's **capture tap** copies each armed track's capture channels into the track's rings; the
**recorder**'s thread writes them into the takes' files, and its sync threads make them durable.

| Part | Runs on | Does |
|---|---|---|
| `Recorder` | its owner's thread | takes the folder's lock and recovers the folder (`start`), makes each armed track's `Tap` (`tap`), reports the takes that ended |
| the capture tap | the audio thread, in the processor | copies each take's window of the capture into its track's rings, and wakes the recorder |
| the recorder's thread | `magnolia-recorder` | drains the rings, writes the takes, makes the next takes' files |
| the sync threads | `magnolia-sync`, 4 by default | sync the takes' data, complete their ends, sync the folder |

**Arming.** `Recorder::tap(track)` makes a track's tap from its number, its name and its capture
channels, one per channel of its takes: its two rings, every page written, and its first take's
file, made ahead. `Controller::arm(tap)` and `Controller::disarm(track)` are edits like the graph's:
the next commit carries the change of the armed taps in its swap's message, with room reserved for
every tap the swap keeps, and the capture tap applies it in the period that applies the swap, after
the transport's commands. A tap's state, its windows and its rings, survives every swap that keeps
it. A tap armed during a play records from the next play; one disarmed during a take ends it at the
frames it has reached, and goes to the collector, which drops it; a tap dropped sets a flag of its
own, then asks its recorder for a pass, which finds the flag set, ends the take the tap left under
way and lets the track go. The flag comes before the ask: the rings' own abandonment comes only
after the drop's body, too late for that pass. `Controller::arm` refuses a capture channel the
engine lacks, a recorder at another rate than the engine's, and a track armed already, and gives
the tap back each time.

**The window.** A take's window is counted in engine samples. A play that starts publishes
`audible_at`, the engine sample at which the output plays its first timeline sample `from`
([above](#the-transport)): within a play the transport's clock moves with the engine's, so the
output plays timeline sample `t` at `audible_at + (t - from)`. With `R` the round trip, the device's
capture and playback latencies read at the play's start plus the recorder's extra latency
(`RecordConfig::extra_latency`), the take's first frame is the frame captured at `audible_at + R`.
Its length, `stop - from`, is set once the stop is known; until then the window is open-ended. A
take starts at the timeline's zero at the earliest: a play from before it skips the frames before
it. An empty audible region, a play and its stop in one period, opens no window. A play while
rolling can stop the last play at once, nothing audible read, in the very period that starts the new
one: after a swap that lowered the pre-roll, or a period of no frame. The tap never sees that stop,
so when a newer play opens, it settles every older play's window whose length is still unknown:
dropped, as it reached no frame. Windows never overlap, since a play starts after the last one's
stop has played, but a take's tail, `R` frames past the transport's stop, can still be captured
while the next play starts: a tap holds up to 16 windows, and a play past them makes no take,
counted in `EngineStats::missed_takes`.

**A period of the tap.** It runs at each period's start, after the transport's commands and before
the graph, in a `catch_unwind` of its own ([below](#a-period)). For each tap it settles the older
windows and opens a window for a play it has not seen, and sets the length of the play's window once
the stop is known; it ends every take placed by a latency other than the device's now, at the frames
it has reached, counted by the recorder; then, window after window, it pushes the period's frames
each window needs, interleaved by the take's channels, and ends the windows complete. A buffer
shorter than the period is silence, as the graph takes it.

**The rings, and the order of reading.** Each tap pushes frames into one ring, `ring_frames` frames
long, and marks into another of 256: a take's start, with its play's number, its first timeline
sample and the round trip that placed it; a gap, with the take's frame where it starts and its
length; a take's end, with its length and why it ended. Each mark carries its position among the
frames: how many the tap had pushed before it. Frames the ring has no room for are dropped whole,
counted in `EngineStats::capture_overruns`, and become a gap. A mark keeps room for what must follow
it, so that a take's end always fits once its start is in: a start goes in with room for three
marks, a gap with room for two. A window's start and its waiting gap go in only right before frames
that fit, so a gap grows over every frame dropped until frames fit again; an end folds a gap still
waiting into its length. The recorder reads how many frames the frames ring holds *first*, then the
next mark, and writes frames only up to that mark's position: every frame it counted was pushed
before the count was read, and every mark before such a frame was pushed before the frame, so it is
seen before the frames that follow it are written. Read the other way, marks first, it could count
frames that the tap pushed after an end and a start it did not see, and write them into the old
take. A mark beyond the frames counted means the count is older than the mark: it counts again.

**The wake.** A tap asks its recorder to drain once its ring holds 2,048 frames it has not
announced, or it has pushed a mark: one flag shared by every tap of the recorder, set by a
read-modify-write, and one wake, `record::wake_recorder`, which calls `Thread::unpark` when the flag
was clear: a futex wake when the recorder sleeps, on the real-time allowlist
([ADR-0023](adr/0023-the-real-time-allowlist.md), [below](#checking-the-real-time-path)). The
capture tap asks once per period at most, after every push of the period. The recorder clears the
flag as each pass begins.

**The recorder's pass.** It takes the tracks of the taps made since, then drains each track's rings
in the order above: frames go straight to the take's file through `WavWriter`, no buffer beyond the
writer's one piece; a gap and the zeros an end pads are written as zeros. A take's first frames
write its header into the file made ahead: sizes unknown, and a `bext` chunk, originator
`Magnolia`, a reference unique to the take (the recorder's start in nanoseconds, the track's number
and the take's), the time reference of its first timeline sample. A take's end hands the take to the
sync threads; a take that ends without frames removes its file. The pass then ends the takes of a
tap dropped without ending them (the processor dropped) or of a stopped tap, removes the files made
ahead of the tracks that left, makes the next take's file of each track that has none, and, when it
made or removed a file, has the folder synced once for the batch. Last it asks a sync of each take
that has written a second of audio since its last, unless one is in flight.

**Takes' files.** A take is the file `<track number>-<track name>-take<n>.wav` in the recording
folder: the name sanitized, its letters and digits of any script, `-` and `_` kept, every other
character `_`, at most 64 characters; `n` continues past the files of that track and name already
there. A track armed again numbers its takes past its old tap's file made ahead, which the old track
removes as it leaves: a gap in the numbers. 32-bit floats by default, the device's samples bit for
bit, or 24-bit integers (`RecordConfig::format`). The next take's file of each armed track exists,
empty, so that a take never waits for its file; the recorder removes the files made ahead when it
ends. `Recorder::takes` lists every take that ended: its file, frames, padding, how it ended, and
the error that ended it, if one did.

**Syncs run off the writer's thread.** On btrfs they still slow its writes: about 1 ms with four
sync threads, 5 to 57 ms with eight ([measurements](measurements.md#the-recorder)). The sync threads take jobs from one queue, so that several files
sync in parallel: a sync of a take's data (`fdatasync`), asked after each second of its audio, one
in flight per take; a take's end, which syncs its data, writes its sizes (`WavWriter::finish`) and
syncs them, so that the sizes never claim data that did not reach the disk; and a sync of the
folder.

**A write error** ends its take: the take writes nothing more, its later frames are drained and
dropped, its file is left as a crash leaves it, its sizes unknown, the error counted in
`RecorderStats::write_errors` and given in the take's summary. A sync of a take's data that fails
ends its take so too, at the recorder's next pass: what it wrote may not be on the disk.

**A panic.** The graph's panic silences the output ([below](#a-period)), but the capture tap goes
on, and so does the transport: a stop still sets the takes' lengths, and they end whole. A panic in
a period's start, a swap's, the transport's or the events', freezes the transport: a take whose stop
was unknown records on until the processor is replaced, as ADR-0022's rebuild does. Dropping the
processor drops its taps, which ends their takes at what they hold.

**Recovery.** `record::recover(folder)` takes the folder's exclusive lock (`File::lock` on
`.magnolia-recording.lock`, which a recorder holds for its life, so a recovery never cuts a take
being written), repairs with `wav::repair` every file whose `bext` originator is `Magnolia`, removes
those without frames and the empty files named as takes, leaves every other file untouched, syncs
the folder if it removed one, and lists each take: its file, the track and take its name gives, its
first timeline sample, its frames, whether it was repaired. `Recorder::start` runs it on its folder.

**What a crash loses.** A kill loses what had not yet left the tap's ring: at most a period plus
2,047 frames, the tap's wake coming once 2,048 frames wait, and a pass: about 50 ms at 48 kHz with
periods of 256 frames. A power cut loses about the last second of each take and the time of a sync, its data synced
every second. A stall of the disk longer than the ring, 4 s by default, drops frames, which become zeros,
so the rest of the take stays in place.

Its limits:

- a tap whose panic stopped it records nothing more until the engine is rebuilt; its takes end at
  what they hold;
- after a panic in a period's start, a take whose stop was unknown records until the processor is
  replaced;
- a recorder closed (`Recorder::close`) or dropped while its tracks are armed ends their takes at
  what they hold; its taps then fill their rings and drop what follows, counted as overruns;
- the recorder reads the engine's statistics it was started with: a processor rebuilt on a new
  engine needs a new recorder;
- the latency is compared at each period's start: a change published mid-period ends the take at
  the next period's start;
- a take's reference is unique while no two recorders start in the same nanosecond;
- a crashed take past 4 GiB reads whole only through Magnolia's repair: its `ds64` chunk is written
  at its end ([ADR-0030](adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).

### A period

`Processor::process` runs on the real-time thread. It never allocates, frees, locks or blocks;
its system calls wake the collector, from an audio player the disk reader, and from the capture tap
the recorder ([ADR-0023](adr/0023-the-real-time-allowlist.md),
[ADR-0026](adr/0026-one-stream-per-audio-track.md),
[ADR-0032](adr/0032-recording-by-a-capture-tap-of-the-processor.md)).

1. Poisoned, it writes silence on every playback channel, the flag set, every period returning
   normally ([ADR-0022](adr/0022-a-panic-on-the-real-time-path-gives-silence.md)). After a panic
   of the graph, it skips the graph, swaps and events, but still applies the transport's commands
   at step 2 and moves the transport's clock over the period, cut at its stop, in place of step 4:
   a stop still ends the takes under way. After a panic of step 2, it skips steps 2 and 4 whole.
2. Otherwise it starts the period inside `catch_unwind`: it applies at most one swap, then the
   transport's commands, then pops every queued event into its pending buffer, which has room for
   them all, whether a swap waits or not: the controller never sends an event before its node, nor
   more events than the buffer holds. A panic poisons it whole.
3. The capture tap runs inside a `catch_unwind` of its own, poisoned or not, so that the takes under
   way go on recording after the graph's panic: it applies the change of the armed taps that the
   swap brought, then copies the capture of each take's window into the rings and wakes the
   recorders ([above](#recording)). It places takes by the last play as the last period's start
   that completed left it, never by a transport a panic may have left half-changed. A panic of the
   tap stops it for good, flags it in
   `EngineStats::tap_stopped` and wakes the recorders, which end its takes; the graph plays on.
4. The graph runs inside `catch_unwind`, whose panic poisons the processor, the graph alone: the
   transport's clock moves over what the graph did not play of the period. For each sub-block it
   copies the capture in, a buffer shorter than the period taken as
   silence. For each step it sums the edges into the node's inputs with their gains; an input
   with nothing sounding holds zeros and is flagged silent. It reads the node's timeline sources
   at the node's position. It skips a node that slept, whose inputs are still silent and which has
   no event, from the controller or its sources, writing silence in its place; otherwise it calls
   the node with the transport at its input, and silences and counts its outputs if it returns an
   error. Last it sums the playback channels and copies them out, a non-finite sample replaced by
   zero and counted, a -0.0 written as +0.0, a buffer shorter than the period left as it is. The
   transport's clock, rolling, moves on by the sub-block. Sub-blocks are cut also where the
   transport stops.
5. It hands what it replaced to the collector, the taps a swap disarmed included, and wakes it if it
   handed anything. Every period, poisoned or not, then advances the engine's clock, counts in the
   heartbeat and records its time.

A poisoned processor goes back to the backend, which drops it off the real-time thread and puts a
new one in its place (ADR-0022): its taps go with it, which ends the takes under way at what they
hold ([above](#recording)); the new processor records once tracks are armed on it again.

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
  nodes** and **each delay line's history**;
- **each disk stream**: its slots, words and cursor, and the flag the players set, written when the
  disk reader is made, off the real-time thread;
- **each armed track's tap**: its two rings, every slot of which is pushed and popped when the
  recorder makes the tap, its windows and its counters, and its recorder's flag; and the list of
  taps a swap brings, reserved by the controller for every tap the swap keeps.

The controller writes every page of each before sending it, through `black_box`, since the
compiler may turn a fresh allocation's zero fill into `calloc`, which leaves pages unwritten
(`engine/memory.rs`). The garbage queue, which the processor writes first, is warmed by pushing
and popping through every slot at the start. `tests/engine/page_faults.rs` checks that the
processing thread's minor faults do not change across swaps, those that add or remove a delay line
included, nor while audio players play as the reader rewrites their slots, nor while capture taps
record into rings the recorder drains.

## The JACK backend

`magnolia_jack::JackBackend` plays an engine's processor through the JACK API, which PipeWire and
JACK2 both serve ([ADR-0007](adr/0007-the-audio-engine.md)). The `jack` crate loads libjack when
the first client opens, so a program that never opens one runs without it. Run through `pw-jack`,
the process loads PipeWire's libjack; otherwise the first `libjack.so.0` on its library path.

`JackBackend::open` connects to a running server and never starts one (`NO_START_SERVER`). It reads
the engine's rate and channels from the processor. It fails with a plain error when libjack is
missing, when no server runs, and when the server's rate differs from the engine's. It registers
one port per engine channel, `capture_1` and on, `playback_1` and
on, sets its latency callback, and activates the client: the server calls the processor from its
next period, capture and playback in one callback.

### Its threads and callbacks

| Part | Runs on | Does |
|---|---|---|
| the handle, `JackBackend` | the caller's thread | opens and closes the client, hands a replacement processor to the callback and drops the one it replaced, asks whether the audio thread runs in real time, connects ports and waits for their latency, sets the monitoring latency |
| the process callback | JACK's audio thread, once a period | swaps in a replacement, publishes the device's latencies, counts gaps and its own overruns, counts the server's xruns into the engine's statistics, calls `Processor::process` on the ports, or writes silence while the server's rate differs from the engine's |
| the thread-init callback | each thread JACK starts for the client: with JACK2 the audio thread, then its notification thread; with PipeWire the audio thread, before its first period | reads the thread's kernel id with `gettid` into the thread's own storage |
| the latency callback | JACK2's notification thread, once in each mode after every change of the graph and when the handle asks | reads the device's latencies from Magnolia's ports' ranges and sets the other side's ranges ([below](#the-devices-latencies)) |
| the xrun, shutdown, sample-rate and freewheel callbacks | JACK2's notification thread; PipeWire's audio thread for an xrun | count an xrun, note the shutdown, note the server's rate, note whether the server freewheels: atomics only |

Every callback but the process callback touches atomics only, in its own code: PipeWire skips a
process cycle while a notification handler runs, and the shutdown callback must be
async-signal-safe. The latency callback also calls libjack's range functions, which on JACK2 sleep
for 1.1 periods when called off the audio thread while a change of the graph is pending
(`WaitGraphChange` in JACK2's `JackAPI.cpp`, seen by RealtimeSanitizer): on JACK2's notification
thread, which is not real-time.

### What crosses the boundary

- **Two rtrb queues of one place each**: replacement processors, from the handle to the callback,
  and the processors they replaced, back. The callback swaps only when the queue back has room, so
  it never drops a processor; the handle drops it at its next `replace_processor`, `poll` or
  `close`.
- **Atomics shared by the parts**: the audio thread's id, the server's xruns, the shutdown, the
  server's rate, whether it freewheels; the device's latencies, the monitoring latency, and the
  handle's requests for the latency callback with the last each mode served.
- **The latency callback's context**: Magnolia's ports, by libjack's handles, and those atomics,
  lent to libjack as a raw pointer ([below](#closing-and-a-server-that-dies)).
- **The engine's statistics**, `EngineStats`, of the processor being played: the callback fills its
  own overruns, gaps, server xruns, the audio thread's id and the device's latencies there, beside
  the processor's own figures.

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
  only callbacks longer than a period count. None counts while the server freewheels: it has no
  deadline, and its cycles' times made one period in 8 to 20 look overrun, measured on JACK2's
  dummy driver. JACK2 tells its clients before its first cycle in freewheel and after its last
  (read in its `JackServer.cpp`), so the flag the freewheel callback sets is exact there.
- **A server xrun**: each xrun callback. JACK2 reports any xrun in its graph, whoever caused it;
  PipeWire reports several between two cycles as one, and never the first.

In freewheel, measured on JACK2's dummy driver, the frame time follows on, so no gap is counted,
and the server reports no xrun: it waits for every client, up to 10 s, rather than skip one.

While the server's rate differs from the engine's, the callback writes silence without calling the
processor, whose heartbeat then stops, until the rate is back.

### The device's latencies

The backend reads the device's latencies in a latency callback, as JACK means a client to
([ADR-0031](adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)). JACK gives each port a
capture range, how long ago what it carries entered the system, and a playback range, how long
until it leaves it, in frames. Before it calls the callback, JACK recomputes Magnolia's input ports'
capture ranges, in capture mode, or its output ports' playback ranges, in playback mode, from what
they connect to. The callback (`src/latency.rs`):

- in capture mode, takes the largest maximum of its input ports' capture ranges as the device's
  capture latency, and sets every output port's capture range to the inputs' widest plus the
  **monitoring latency**, the delay of Magnolia's path from a capture port to a playback port;
- in playback mode, takes the largest maximum of its output ports' playback ranges as the device's
  playback latency, and sets every input port's playback range to the outputs' widest plus the
  monitoring latency.

So `jack_iodelay` and other clients see Magnolia's monitoring path. A client that sets no latency
callback gets JACK's default instead, which copies the ranges across with nothing added. The
callback stores the device's latencies in atomics; `JackBackend::device_latency` reads them, and
the process callback publishes them each period to the processor it plays
(`EngineStats::publish_device_latency`), so that a processor swapped in has them from its first.

A range read right after `jack_connect` returns may be stale: the server notes a connection for its
next graph, switches to that graph at a later period's start, and runs the callback only then
(stale in up to 8 reads of 30 on JACK2's dummy driver at 64 frames, measured while designing
ADR-0031). `JackBackend::wait_for_latency` takes the connections the caller made, waits until each
is in the graph the server runs (`jack_port_connected_to`), then asks the server to recompute the
latencies (`jack_recompute_total_latencies`) and waits until the callback has run in both modes
since: a request counter the handle raises, and, for each mode, the request the callback saw when it
began. `connect_to_physical_playback` and `connect_to_physical_capture` wait so before they return, and `set_monitoring_latency`
waits for the recompute it asks; each gives up after 2 s with a plain error. JACK2 runs the callback
of a client connected to nothing too, since it walks every active client.

On JACK2's dummy driver, in its default, asynchronous mode, Magnolia's ports connected to the
`system:` ports report one period of capture latency and two of playback, the second for the
asynchronous mode's buffering. A hardware driver reports its buffering too, plus the extra given to
`jackd -I/-O`, never the converters': the user measures those with a loopback cable.

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

The latency callback is Magnolia's own, set through `jack_sys`, which the `jack` crate knows
nothing of: its context lives in a box the backend lends to libjack (`latency::lend`). Once the
client is deactivated, the backend clears the callback, drops the client's last handle, which
closes the client and stops JACK2's notification thread, and only then frees the context. When the
deactivation fails, as when the server died, the context is left allocated with the crate's own,
for the same reason. When the activation fails, the crate normally closes the client, and the
context is simply leaked: a few bytes, on a path only a refused activation takes. Waiting for the
latency rests on JACK2 recomputing latencies synchronously; PipeWire's behaviour is not yet
checked. `magnolia play --connect` and the example warn, and play on, when the latency is not
reported.

### The `jack` crate on the real-time path

Read in its code, not measured: around each process callback it takes a `std::sync::Mutex` with
`try_lock`, which never waits. When that lock is busy, the crate skips Magnolia's period and
returns `Quit`, which on JACK2 deactivates the client. Its buffer-size callback takes the same lock,
so a change of the period while a cycle runs could do it too, not only a closing client; on
PipeWire, its locked-process default keeps the two from running at once, as far as was read. Its own wrapper of the port-registration
notification locks a mutex and may allocate, on JACK2's notification thread or, on PipeWire, while
the process cycle is held. Its `log` feature is off, so libjack's messages go to standard error.

On deactivation, as in `JackBackend::close`, the crate marks its callbacks invalid before it calls
`jack_deactivate`, so a cycle that the server runs in between returns `Quit` (read in
`client/async_client.rs` and `client/callbacks.rs` of `jack` 0.14). A JACK2 server in its default,
asynchronous mode goes on. One in synchronous mode, `jackd -S`, waits for that client until ten
times its client timeout: measured with `-t 5000`, a stall of 50 s at closing. With the default
timeout, the deactivation's own wait gives up first, and `close` returns
`CloseError::NotDeactivated`.

### Its tests

`crates/magnolia-jack/tests/jack/` runs each behaviour on a JACK2 server of its own, `jackd -n
<name> -r -d dummy -r 48000 -p <period>`, without real time and without a sound card, one test at
a time; a plain JACK client of the test's plays a signal into Magnolia and records it. Each test
refuses to run when the libjack it would load is PipeWire's (`tests/common/mod.rs`), which would
reach a live PipeWire. A server without real time on a busy machine may skip a client's cycle,
which breaks a recording; the tests that compare a recording sample by sample take it again when
the gap counters show a skipped cycle, five times at most. A JACK call never times out, so a
watchdog bounds each test: one that marks no step for two minutes is reported with where each of
its threads waits, and its servers are stopped, which ends its JACK calls; one still stuck after
that is aborted (`tests/common/mod.rs`, [CONTRIBUTING](../CONTRIBUTING.md#the-jack-backends-tests)).

The tests that need JACK's processing path and not its timing run their server in **freewheel**
(`support::Freewheel`, `jack_set_freewheel`): it runs its cycles one after another, as fast as its
clients go, never skipping one; 80 to 240 times real time on JACK2's dummy driver, measured. They
are the null below, the duplex, the swap, the panic, the round trip, and the counters' freewheel
case. A test's thread cannot act at a point of a recording by waiting for it there, so the probe
that records **holds** where the test asks: it waits in its own period, and the freewheeling server
with it, until the test moves the hold on, 5 s at most. JACK2 1.9.22 stalls for its freewheel
driver's timeout, 10 s, when a client closes while it freewheels, measured, so a test opens its
clients first, turns freewheel on through a client it keeps open, and closes nothing until that
client has turned it off. Freewheel disconnects the server's `system:` ports, and connections made
during it are undone when it ends (read in `JackServer.cpp`). The others run in real time: the
counters, which need deadlines, and whose overrunning node is the suite's short real-time play; the
latencies, which need the `system:` ports; the rights, the shutdowns and the errors on open. The
null's start, before its freewheel, is real time too.

`tests/jack/round_trip.rs` shows that Magnolia's monitoring path adds nothing to a direct
connection: a source client's impulse reaches a sink client directly and through Magnolia's
capture port, the engine and its playback port, on the same sample with capture straight to
playback or through a `TestLatency` of 0, exactly 100 samples later through one of 100. It proves
that nothing is added within a cycle, not the hardware's round trip, which only a loopback cable
measures. `tests/jack/latency.rs` checks the latencies above on the dummy driver at 64 and 256
frames, a processor swapped in included, and that other clients see the monitoring latency.

`tests/jack/session.rs` is the null through JACK: the small test session, played by a
`SessionPlayer` through the backend at 64 and at 256 frames, equals its offline render to the bit
from the play's first audible sample, and is silent before it. Its recorder keeps, with each
period, the engine sample at which Magnolia's period started (`EngineStats::block_start`): JACK
runs it after Magnolia, whose outputs it reads, so each period is compared with the render where
the engine says it lies, and the first audible sample is where `EngineStats::play_start` says. A
period Magnolia was playing while the recorder copied it, seen by the engine's count of periods
and its block start, is set aside. A cycle the server skips leaves a hole or a repeat, never a
period out of place. So takes are played until each sample of the render has been heard in at least
one, five takes at most. The play starts in real time; once the disk reader is idle, holding every
frame of the session, which its 10 s read-ahead covers, the server freewheels for the rest of the
take: the processor cannot outrun the reader, so no frame is missing, and no cycle is skipped.

## The test session

`magnolia-session` reads, plays, records and generates the test session of M1.4 and M1.5
([ADR-0028](adr/0028-the-test-session-in-json.md)): a JSON file beside the WAV and MIDI files it
names, which the tests and the command line read until Stage 2's project replaces it. The `file`
module's documentation gives the format, an example and every rule; this section gives the flow.

| Step | Code | Does |
|---|---|---|
| parse | `file::parse` | reads the format and the version first, so that a version it does not read is refused as such; then the whole file into `SessionFile` with serde, `deny_unknown_fields`; then every check that needs no file on disk ([below](#what-parse-checks)) |
| open | `Session::open` | reads the file, at most 64 MiB, and parses it; holds the memory the session asks for to a budget of 1 GiB, before allocating it ([below](#the-memory-budget)); reads each MIDI file once, at most 64 MiB; opens each audio file once and checks its rate, its channels and that it holds every frame of its clips; makes each MIDI track's clips one `MidiSequence`; resolves every output and send |
| build | `Session::build` | makes the engine, the disk reader and the graph, and commits it with the tempo map ([below](#the-graph-a-session-builds)) |
| render | `render`, `render_to_file` | plays the session offline from sample 0 into a WAV file of 32-bit floats ([below](#the-offline-render)) |
| play | `SessionPlayer` | builds the session with the disk reader's thread and hands its processor to a backend: the JACK backend, or the device-less backend in tests |
| record | `SessionRecorder` | builds it as `SessionPlayer` does, with a recorder that records its armed input tracks ([below](#recording-a-session)) |
| generate | `generate::generate` | writes the reference projects without their plugins, or a small session of every feature, the same to the byte on every machine ([below](#the-generator)) |

### What parse checks

The format is at version 2, which brought input tracks; this reader reads versions 1 and 2, and the
generator writes 2. A version it does not read is refused as such, before any other field.

The ranges of every value: the rate is one the time model takes, a tempo lies from 1 to 1,000
quarter notes a minute, channels from 1 to 1,024 (the master's from 2), a latency up to 65,536
samples, every gain finite, every clip at least a frame or a tick long, at or after the timeline's
zero and ending within 24 hours of it at the session's rate, so that a test session lasts at most a
day and its render with it; over it, the clip's `length` is refused. The tempo map builds. Each
track has the fields of its kind and no other: an audio track its channels and clips, a MIDI track
its instrument and clips, an input track its channels, `inputs` and `armed`; an audio clip its file
offset and gain, a MIDI clip neither. An input track's `inputs` name capture channels from 1 to
1,024, one per channel of the track, and each capture channel feeds one channel of one input track
at most; an input track in a file of version 1 is refused. Bus names are unique and none is
`master`; every output and send names `master` or a bus; what feeds a bus or the master has its
channels, or is mono into stereo; no bus feeds itself back, through any chain of buses, checked
once per bus. A track's audio clips, placed on the sample grid, do not overlap
([ADR-0026](adr/0026-one-stream-per-audio-track.md)). Paths are relative and stay inside the
session file's folder: an absolute path or a `..` is refused, a `.` ignored, so that one file has
one spelling, and is read once; a symbolic link inside the folder is followed wherever it leads. At
most 512 distinct audio files, each kept open while the session plays.

An error names the field by its path, such as `tracks[1].clips[0].start`, and says what is wrong.
serde_json gives an error's line and column, and names an unknown or missing field, but not the
path to it, so `file/position.rs` walks the text up to the error's position to name the field. The
checks the loader needs again, such as resolving the routing or placing a clip, are the same
functions, so that one rule has one copy. The fuzz target `session_file` feeds the parser text, raw
or built like a session with arbitrary values, input tracks among its tracks, and checks that a
session that parses names no file outside its folder, names each capture channel once at most, in
input tracks of version 2 alone, and writes back to text that parses. The fuzz target
`session_open` feeds `Session::open` text, raw or a session that opens changed in a few places, its
input tracks' capture channels among them, beside small WAV and MIDI files it writes at its start,
so that most inputs reach the memory estimate, the MIDI files' clips and the sequences; it checks
that a session that opens fits its budget, lasts at most a day, and gives each armed track capture
channels the engine has.

### The memory budget

A session file of a few hundred bytes can ask for gigabytes: an audio track of 1,024 channels holds
2 GiB of stream, a latency of 65,536 samples on a bus of 1,024 channels 256 MiB, and 400 clips of one
dense MIDI file expand it 400 times. So `Session::open` estimates what the session asks for before
it allocates anything large, and refuses it over 1 GiB (`MEMORY_BUDGET`), with an error that says
what takes the memory (`load/budget.rs`):

- each audio track's stream: its channels times the read-ahead in frames, rounded up to whole
  blocks of 4,096 and two blocks more, as the disk reader rounds, times 4 bytes;
- each test latency's history, and each delay line's that the engine will add: channels times
  samples of delay, times 4 bytes. The delay lines are worked out by the engine's rule
  ([above](#delay-compensation)) on the graph a session builds (`load/latency.rs`): a track's gain
  runs as far ahead as the most demanding of its output and its sends, and every other one gets a
  delay line; an input track is live, so nothing out of it gets one;
- the engine's buffers: a largest block of 1,024 frames for each input and output of every node and
  each capture and playback channel, times 4 bytes;
- each armed input track's recording rings: a recorder's default ring, 4 seconds, times its
  channels, times 4 bytes, and its ring of marks;
- each MIDI file, read and parsed once, at 16 bytes an event, and the events each clip expands it to,
  at 128 bytes an event, above the 78 measured at the peak of 400 clips of a file of 40,000 events.

The first four come from the file alone, and are checked first. Then the MIDI files are read, each
once, the budget checked after each; then the clips' events are counted and checked, before any
clip expands. `Session::memory_estimate` gives the estimate of a session that opened. The high
tier's reference project asks for 143 MiB, the lowest tier's 44 MiB.

### The graph a session builds

```text
audio track:  player    ─┐
MIDI track:   test synth ┼─> inserts, in series ─> gain ─┬─> its output's input
input track:  capture   ─┘                               └─> one gain per send ─> the send's input
bus:          its input sums ─> inserts, in series ─> gain ─> its output's input
master:       its input sums ─> inserts, in series ─> the playback channels
```

The engine has the session's rate, the master's channels as playback channels, as many capture
channels as the highest that an input track names (`Session::capture_channels`, none without input
tracks), and a largest block of 1,024. A strip's input is its first insert, else its gain; the
master without inserts sums straight into the playback channels. Sends leave after the track's
gain. A mono source feeds both channels of a stereo input. The disk reader reads 10 seconds ahead,
one stream per audio track; its players come in the audio tracks' order, and are paired with them
by that order. Inserts are the engine's test nodes, `TestLatency` and `Gain`, until plugins come
(M1.6); the engine compensates their latencies ([above](#delay-compensation)). `tests/session.rs`
renders impulses through each kind of path: a send after its track's gain, compensated onto its
output's sample; a bus into a bus; mono into stereo; a clip that starts between two samples; a note
of a MIDI file of 1,000 ticks a quarter; and a compensation longer than the read-ahead.

An input track plays its capture channels, numbered from 1 in the file and from 0 in the engine,
one per channel of the track: monitoring is always on, whether the transport plays or not. Its path
is live, so the engine never delays it to line it up with others: a capture frame reaches the
playback channels late by the test latencies along its path alone, its inserts', each bus's it
goes through and the master's. `Session::monitoring_latency` gives the slowest such path over every
input track's output and sends, which the command line hands the JACK backend as its monitoring
latency ([ADR-0031](adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)).
`tests/session.rs` feeds an input through the device-less backend and hears it late by its insert's
latency and no more, while a latent audio track pre-rolls every play, and through its send into a
latent bus later by that bus's latency, without a delay line.

A MIDI clip plays its whole file from its start: every track of the file merged, at one tick in the
order of the tracks then of the file; each tick scaled from the file's division to the time model's
by the first tick at or after (exact for every common division, rounded up for others, such as
1,000); each note off ends the earliest note of its channel and key still held, first in, first
out, and one that ends none is ignored; a note without its note off ends at the clip's end, and a
note that lasts no tick after scaling does not play; velocities and controllers' values divided by
127, pitch bends by 8,192. The track's sequence then ends each note where a later note of its
channel and key starts ([above](#timeline-sources)). The file's tempo and time signatures are
ignored: the session's tempo map rules.

### The offline render

`render(session, block_frames, writer)` builds the session with a disk reader without a thread,
locates it at sample 0, primes it and plays the cue. It processes blocks of `block_frames`, from 1
to half the read-ahead (240,000 frames at 48 kHz), the reader filled before each, so it never
underruns. It gives the engine no capture: input tracks play silence. It writes from the engine
sample where the output reaches timeline sample 0, which the largest compensation delays
(`PlayStart::audible_at`), up to the session's last clip's end and one second of tail, every
playback channel, in 32-bit floats. Its report gives the frames written, the underruns and the
largest compensation. `render_to_file` renders into a new file so that it appears whole or not at
all: into `.<name>.partial` beside it, synced to disk, then renamed; a render that fails removes its
partial file, and a file in the way is never written over.

The render is the same to the bit whatever its blocks, and a play from sample 0 through a
`SessionPlayer`, in periods of any length, plays the same bits from where its output reaches that
sample: `tests/session.rs` renders the small session in blocks of 4,096, 64 and 1,000 and plays it
in random periods of 1 to 5,000 frames, its capture silent, waiting for the disk reader between
periods. Its sends into a latent bus put two delay lines in its graph, which the test counts, so
that the null passes them; its input track sends there too, and adds none.

`SessionPlayer::start(from, timeout)` stops any play under way, locates the reader, waits until it
has primed the cue, then plays it, the order the disk reader documents
([above](#audio-clips-from-disk)). The stop comes first because a rolling play keeps the slots of
all it may still read, which the new cue may need, until its stop reaches the players; so while a
play rolls, the backend must be playing for the wait to end. `stop` stops;
`report` gives the engine's and the reader's statistics and each audio track's low-water mark, which
stays empty while its stream holds the track to its end, as for a track shorter than the read-ahead.

### Recording a session

`SessionRecorder::new(session, settings)` builds the session as `SessionPlayer::new` does, with a
recorder ([above](#recording)) in `RecordSettings::folder`, which must exist, its extra latency and
its take format given. Once the engine is made and before any node is, it starts the recorder,
which takes the folder's lock, holds it until it is dropped, and recovers what a crash left there,
then arms each of `Session::armed_tracks` with a tap: the track's number, from 1 in the session's
order of tracks, every kind together; its name; its capture channels from its `inputs`. The arming
goes out in the graph's own commit, so the first play records. The recorder's ring has its default
length, which the memory budget counts.

It holds the player and the recorder side by side, as public fields: the player starts and stops
plays as `SessionPlayer` does, and the recorder lists each take once its file is complete
(`Recorder::takes`). A take of a play from timeline sample `from` starts with the frame captured a
round trip after the output played `from`, and lasts to the play's stop. It is dropped once the
backend has let its processor go and the takes have ended: the recorder then removes the file it
made ahead of each track's next take, and releases the folder's lock.

`tests/session.rs` records the small session's input through the device-less backend, its capture
a ramp of distinct values, from one second in with an extra latency of 37 frames: the take's
frames are the frames captured from 37 after the output played the cue, bit for bit, its `bext`
time reference the cue, its length the span to the stop.

### The generator

`cargo run --release -p magnolia-session --example generate -- <kind> <folder>` writes
`session.json`, `audio/*.wav` in 24 bits and `midi/*.mid` into an empty or new folder, and refuses
any other. The kinds: `lowest` and `high`, the [reference projects](reference-projects.md) without
their plugins, the high tier's with its live voice, an armed mono input track from capture channel
1 into the vocals' bus through a gain where Auto-Tune goes; `small`, about 8 seconds that hold every
feature of the file, an armed mono input track `mic` among them, through a test latency of 32
samples into the vocals' bus and sent to the latent room bus, for tests and CI.

Every file is the same to the byte on every machine. Each draws its numbers from a SplitMix64
generator of its own, seeded from one fixed seed mixed with the file's name; a part's musical line
comes from a seed of its own, so a lead and its double sing the same line. The audio is computed
with +, -, *, / and `sqrt` on `f64`, and the conversions IEEE 754 makes exact, in a fixed order:
the sine from a table computed by its Taylor series, the equal temperament from a twelfth root of
two found by bisection, decays by one multiplication a sample; never `sin`, `exp` or `powf`, whose
precision differs between C libraries. The JSON is serde_json's, the MIDI files the module's own
writer's: format 1, a first track of the song's tempo and meters, which Magnolia ignores and REAPER
reads, then the part. An input track has no file. `tests/session.rs` generates the small session
twice and compares the bytes, and pins a digest of its files, which a machine that computes
otherwise fails; the test says what to do when the generator changes on purpose.

The test session's limits: the format holds no comments; buses have no sends; the only instrument
is the test synth; only mono feeds a wider strip, and only a stereo one; a MIDI clip has no offset
into its file; an input track always monitors, and an armed one records whenever a recorder plays
the session.

## The command line

`magnolia`, the binary of `magnolia-cli`, plays a test session through the JACK backend, records
its armed input tracks as it plays, recovers a recording folder, and renders a session offline;
`magnolia --help` gives its usage, and
[CONTRIBUTING](../CONTRIBUTING.md#playing-and-rendering-a-session) its commands. It reads its
arguments by hand (`arguments.rs`). A misuse exits with 2 and the usage, any other failure with 1;
each is one plain sentence on standard error, and bad arguments or a bad session never panic.
Seconds, as `--from 1.5`, are read exactly into time units, digits and a fraction, then placed on
the session's samples by `place`.

`magnolia render <session.json> <out.wav> [--block FRAMES]` runs
[`render_to_file`](#the-offline-render): the file appears whole or not at all, never over another.
It prints the frames, the time taken and the memory the session asks for, estimated.

`magnolia play <session.json> [--server NAME] [--connect] [--from SECONDS] [--seconds N]`
(`play.rs`):

1. opens the session, and the span to play: from `--from` for `--seconds`, or up to the session's
   last sample and one second of tail, as the render;
2. builds a `SessionPlayer` and opens the JACK backend on its processor, on the named server or the
   default one; a server at another rate than the session's is an error;
3. with `--connect`, connects the playback ports to the server's first physical playback ports,
   which waits for their latency; `play` never connects the capture ports, so that a session's
   input tracks never route a microphone to the speakers unasked: `record` does;
4. for a session with input tracks, sets the backend's monitoring latency to
   [`Session::monitoring_latency`](#the-graph-a-session-builds), which waits until the server has
   recomputed the latencies, and prints the **latency report** (`latency.rs`,
   [ADR-0031](adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)): the device's capture and
   playback latencies, their sum, and the monitoring path's latency, each in frames and
   milliseconds;
5. starts the play ([`SessionPlayer::start`](#the-offline-render), the disk reader given 30 s to
   prime), then waits until the output has played the span, counted from the play's first audible
   sample on the engine's own clock (`EngineStats::play_start` and `block_start`), not the wall
   clock;
6. stops, and waits for the stop to play out: the pre-roll's length, the fade and the test synth's
   release, 5 ms each, and 100 ms of margin;
7. asks the kernel whether the audio thread runs in real time, while it still runs, closes the
   client, and prints that report, the player's `PlayReport` and the gaps in the server's frame
   time.

A latency the server did not report in time is a warning, not a failure: the ports play all the
same. Between two looks at the engine, the main thread sleeps for what is left to play, at least a
period and at most a second. It gives up when the server shuts the client down, when the backend's
callback panics, or when no period has run for 2 s. A play during which the engine or the
backend's callback panicked prints its figures, then exits with 1.

`magnolia record <session.json> <folder> [--server NAME] [--connect] [--from SECONDS] [--seconds N]
[--extra-latency FRAMES] [--format float32|int24]` (`record.rs`) plays as `play` does, through a
[`SessionRecorder`](#recording-a-session):

1. opens the session, and refuses one without an armed input track before it touches the folder;
2. starts the recorder on the folder, which must exist: it recovers the folder and holds its lock,
   so a folder another recorder or a recovery holds is a plain error, and prints what the recovery
   did, before the server is opened, so that it is seen even when that fails; `--extra-latency` is the
   recorder's extra latency, 0 by default, `--format` its takes' samples, 32-bit floats by default;
3. opens the backend, connects it with `--connect`, sets the monitoring latency and prints the
   latency report as `play` does, with the extra latency, and what places the takes: the capture
   and playback latencies and the extra;
4. plays, stops and waits for the stop as `play` does, then, on the engine's clock, for the round
   trip after it, which the takes' last frames need, then for the recorder to end each armed
   track's take of the play, 10 s at most;
5. closes the client, and prints each take (its file, track, first timeline sample, frames, padded
   frames, the round trip that placed it and how it ended), the recorder's figures, its rings at their fullest, the capture's overruns and missed takes, and the
   engine's figures as `play` prints them.

It exits with 1, after printing, when a take's writing failed, when a change of the device's
latency ended a take, or when the capture tap panicked; a take padded with silence exits 0, its
padding printed. A play that fails mid-take, as when the server stops, first drops the processor the
backend gave back, which ends the takes; when the backend could not give it back, as when the
server died, it closes the recorder, which ends them; then it prints them as above, and exits
with 1. Dropping the recorder at the end removes the
file it made ahead of each track's next take, so the folder holds the finished takes alone.

`magnolia recover <folder>` (`recover.rs`) runs [`record::recover`](#recording) on the folder, and
lists, each list in the order of the files' names, the takes it found, whether each was repaired or
whole already, the files it removed, those it left untouched and those it failed to repair; a
folder that a recorder holds is a plain error, and a take it failed to repair makes it exit with 1.

`tests/cli/` runs the commands on a JACK2 server of the test's own, as the backend's tests do
([above](#its-tests)): `record` leaves one finished take placed at `--from`, as long as the span
played, and prints the dummy driver's latencies; a second `record`, and a `recover`, on a folder a
first `record` holds fail plainly, and once that first one is killed mid-take, `recover` repairs
its take; `recover` repairs a take cut by hand, its sizes unknown and half a frame at its end, and
leaves another program's WAV file untouched.

Its limits: the disk reader reads the session's fixed 10 seconds ahead, as `SessionPlayer` takes no
read-ahead; Ctrl-C ends the process at once, without the stop's fade or the figures, and leaves a
take under way as a crash leaves it, which the next `record` or `recover` on the folder repairs; the
play starts once, with no locate or loop while it rolls; `--connect` connects the playback ports,
then the capture ports, before the play starts: a connection made during a take changes the capture
latency, which ends the take.

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
function of Magnolia's that makes an allowed call, so far `wake_collector`, `wake_disk_reader`
([ADR-0026](adr/0026-one-stream-per-audio-track.md)) and `wake_recorder`
([ADR-0032](adr/0032-recording-by-a-capture-tap-of-the-processor.md)); never an intercepted
function such as
`syscall`, which a lock's wait goes through too. The names match only in
symbolized reports, so every run sets `allow_addr2line=true`, with binutils' `addr2line` from the
development shell.

Because the sanitizer ends the process, a test of it runs each case in a child process and reads
its exit status and standard error: `crates/magnolia-engine/tests/realtime_sanitizer.rs`, and
`crates/magnolia-jack/tests/realtime_sanitizer.rs` for the backend. Each sets the options of each
run itself: `halt_on_error=true`, `allow_addr2line=true`, and the suppressions. The engine's cases:

- an allocation in a marked function is stopped at `malloc`;
- work on memory allocated beforehand, a block's conversions of time, and the engine's processor
  through a crossfade, a dip, a larger pool, events, removed nodes handed to the collector, a
  sleeping node, a failing node, swaps that add and remove a delay line, MIDI sequences played
  through a `TestSynth` with a play, a stop and a play while rolling, two audio tracks streamed
  by the disk reader on its thread, whose players wake it, with a play, a stop and a play while
  rolling, and two armed tracks recorded through a play, a stop, a play and a play while rolling,
  the recorder on its thread woken by the capture tap, all stay silent;
- the collector's wake, without the suppressions, is stopped at `syscall`; with them, it passes;
- the disk reader's wake, without the suppressions or with the collector's line alone, is stopped at
  `syscall`; with its own line, it passes; and the audio tracks' case, without that line, is stopped
  at a player's wake, so its silence rests on the players' real wakes;
- the recording case, without the recorder's line, is stopped at the capture tap's wake, so its
  silence rests on the tap's real wakes;
- a lock's wait right after the wake is still stopped, at the `syscall` of the futex wait.

The backend's cases, on a JACK2 server the test starts on the dummy driver:

- an allocation in a marked function is stopped at `malloc`;
- `gettid` in a marked function is not stopped: the sanitizer does not intercept it, so the
  thread-init callback's one call needs no suppression, though it runs outside any real-time
  context anyway;
- the backend playing the engine through periods, capture, events, a crossfade, a dip and a
  processor replaced in place stays silent: the sanitizer checks every call inside the process
  callback, libjack's included.
