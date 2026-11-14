# 0032. Recording by a capture tap of the processor

- Status: Accepted; amends 0007 (where recording is taken), 0022 (a take goes on after the graph's
  panic), 0023 (the recorder's wake) and 0025 (live nodes in the pre-roll); amended by 0034
- Date: 2026-11-12

## Context

M1.5 records the input of armed tracks and monitors it live
([ADR-0025](0025-delay-compensation-by-reading-ahead.md): nothing live is delayed). A take holds the
timeline samples of a play, `[from, stop)`, each placed by the round trip `R`
([ADR-0031](0031-the-round-trip-reported-and-takes-placed-by-it.md)): the capture of the last ones
arrives `R` samples after the output played them, after the transport has stopped.

The design's challenge found, with a probe on the engine, that a node of the graph sees time only
through the transport at its input. A play while rolling can start in the period right after the
stop, with the same start, so a node cannot always tell the new play, and the old take's tail of `R`
frames then falls into the new play. A node added during a play gets its input faded in over 5 ms,
measured. Ardour and Pro Tools record a track's input before its plugins.

Monitoring through a latent plugin made every play pre-roll by that plugin's latency, since the
pre-roll counted live audible nodes, which have nothing to read ahead. Leaving every live node out
would lose the first events and the chase of a latent live node that has timeline sources, such as
MIDI notes into a pitch corrector, measured with the same probe.

## Decision

The owner's choice of 2026-11-12.

**The processor holds a capture tap beside the graph.** Each armed track names its capture
channels; the tap copies them, before any node, into a ring per track that the recorder's thread
writes to disk ([ADR-0030](0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)). Arming is a
command, committed like any edit: the armed tracks travel in the compiled graph.

**A take's window is counted in engine samples**: from the engine sample at which the output plays
the take's first timeline sample, plus `R`, for the take's length once its stop is known. Two
windows never overlap, since a play starts after the last one's stop has played. An empty window
makes no take. No fade of the graph touches what is recorded.

**The tap runs in a `catch_unwind` of its own**, before the graph: a panic of the graph poisons the
processor and silences the output ([ADR-0022](0022-a-panic-on-the-real-time-path-gives-silence.md)),
but the takes go on recording.

**The tap wakes the recorder's thread** through `wake_recorder`, a futex wake, a new entry of the
real-time allowlist ([ADR-0023](0023-the-real-time-allowlist.md)), once enough frames wait or a take
starts or ends.

**The pre-roll counts the audible nodes that are not live, and the live ones that have timeline
sources**: live paths read nothing ahead, unless they play the timeline.

## Alternatives considered

- **A record node of the graph**, fed by the capture, without outputs: no second mechanism, but the
  node contract would need the engine's sample and the play's number to tell plays apart, an arming
  during a play records a fade, and a take ends when the graph panics.
- **Keeping the pre-roll's capture as handles**: a few milliseconds of silence before `from`, and a
  file starting before the timeline's zero; left for Stage 4's takes.
- **Every live node left out of the pre-roll**: simpler, but loses the first events of a latent live
  node that plays the timeline.

## Consequences

- Stage 4's punch-in and loop recording add breakpoints between engine and timeline samples to the
  same tap.
- A take records the raw input; printing a track's plugins is not done.
- What a live node plays from the timeline stays out of line by its latency, as ADR-0025 says.
