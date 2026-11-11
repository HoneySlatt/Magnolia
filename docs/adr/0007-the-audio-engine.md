# 0007. The audio engine

- Status: Accepted; amended by 0018, 0023, 0024, 0025 and 0026
- Date: 2026-11-06

## Context

The engine must play vocals through Auto-Tune without felt delay, at 64 frames and 48 kHz on the
high tier and 128 on a two-core laptop ([ADR-0001](0001-what-magnolia-is.md),
[ADR-0010](0010-the-quality-bar.md)). It must keep up with film-length sessions and hundreds of
tracks. The field, on 2026-11-06 ([the survey](../survey.md#the-audio-engine)):

- On Linux, REAPER, Bitwig and Ardour use the JACK API or ALSA (REAPER and Ardour also
  PulseAudio). None uses PipeWire's native API; PipeWire serves the JACK API through
  `pipewire-jack`.
- cpal, Rust's common audio library, opens input and output as separate streams. A DAW processes the
  input and the output of a period in one callback.
- REAPER renders tracks that take no live input ahead of time on worker threads (about 200 ms),
  which is why it is light on heavy projects. Ardour and Tracktion schedule a graph of nodes on
  worker threads, delay compensation built into the graph.
- RealtimeSanitizer finds allocations, locks and system calls on a real-time thread; it exists for
  Rust (`rtsan-standalone`).

## Decision

**Magnolia writes its own backends**, without cpal, each one duplex callback: the JACK API first
(PipeWire and JACK2 both serve it), then PipeWire's native API, then ALSA with the device exclusive.
WASAPI and ASIO come with Windows. The engine's core does not depend on a backend.

**The real-time path never allocates, locks or blocks.** The only system calls it makes are on an
allowlist: waking workers and plugin hosts (futex, eventfd). RealtimeSanitizer checks it in CI,
those calls suppressed by name, since it flags any system call.

**The graph is compiled off the real-time thread** and swapped in at a block's boundary. A compiled
graph holds the order of nodes, their buffers and the delays that compensate plugins' latencies. Its
nodes live apart from it, in an arena, so that delay lines, plugin connections and meters survive a
swap. Old graphs are freed by a collector thread. A change of topology crossfades briefly. A plugin
that changes its latency mutes its path briefly and resynchronises it.

**The timeline reaches the real-time thread as immutable snapshots, one per clip**, shared and
published like the graph. A note dragged at 240 Hz replaces one clip's snapshot, not the project's.
A disk thread reads audio ahead of the playhead, as Ardour's does.

**Time is exact.** Positions are 64-bit sample counts. Musical time is fixed-point, never float
beats, which drift over a two-hour reel. A tempo map holds tempo and meter changes. Timecode
includes 23.976 and 29.97 drop-frame. The time model is fixed in Stage 1, though video comes later.

**Events are sample-accurate**: notes, parameters and automation carry their offset in the block.

**Workers sleep, never spin.** A spinning worker on a two-core laptop takes the core a plugin host
needs. Worker threads run with real-time priority, so the audio thread never waits on a thread that
was preempted.

**Render-ahead comes after the graph scheduler.** Nodes are designed so that tracks without live
input can later be rendered ahead, REAPER's way, when measurement shows it needed.

**Without real-time rights, the engine says so** and tells the user how to grant them. It never runs
at normal priority silently.

## Alternatives considered

- **cpal**: one library for every platform, but no duplex callback, and a PipeWire backend still
  young.
- **PipeWire's native API first**: Linux's present, but no pro DAW relies on it yet, and the JACK
  API runs on PipeWire as well.
- **Render-ahead first**: the largest gain on heavy projects, but parameter changes lag, plugin
  analysers run early, and every plugin host must also be driven ahead. The scheduler is needed in
  any case for live input.
- **Floating-point beats**: simpler arithmetic, but drift on long sessions.

## Consequences

- Each backend is code to write and test; the JACK API covers Linux in Stage 1.
- The figures that decide render-ahead, the deadline of plugins and the unit of isolation come from
  Stage 1's measures ([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)).
- Video, film timecode and game loops find their time base ready when their stage comes.
