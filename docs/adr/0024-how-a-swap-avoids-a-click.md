# 0024. How a swap avoids a click

- Status: Accepted; amends 0007 (a crossfade, or a dip when a crossfade cannot be made)
- Date: 2026-11-11

## Context

[ADR-0007](0007-the-audio-engine.md) says that a change of topology crossfades briefly. The engine
swaps a compiled graph at a block's boundary; a crossfade means running the old and the new graphs
together for the fade, each node once.

That is not always possible or wise:

- **An edit that reverses an order**, such as two inserts swapped, A→X→Y→out to A→Y→X→out: the union
  of the two graphs has a cycle, so no single order runs it, and each node can run only once.
- **A large edit**, such as a new template or a new mixer layout: running the union doubles the work
  of the fade's blocks, which can overrun the period.
- **A fade on every changed edge** multiplies along a path: inserting a node through two fading
  edges dips the level by 2.5 dB halfway through.

## Decision

The owner's choice of 2026-11-11.

**A crossfade when it can be made.** If the union of the old and new graphs is acyclic and holds at
most 1.25 times the nodes of the larger, one phase runs the union for one fade, then the new graph
plays alone.

**Otherwise a dip.** The old graph fades its removed edges out over one fade, then the new graph
fades its added edges in over one fade. The processor cuts its sub-blocks at each phase's last
sample, so no silence separates the two fades, at any period.

**A fade is a raised cosine of 5 ms** at the engine's rate (240 samples at 48 kHz): its gain and
the matching fade-out's sum to 1 at every sample.

**Each changed path carries one fade.** In a crossfade, an edge into a node the swap drops, or out
of a node the swap adds that has an input, stays constant; the edges where the path meets what both
graphs keep fade. Inserting or removing a node then keeps the level. A new source fades in.

**A graph installed before the first period starts at once**: nothing has sounded yet to fade
from. Once periods have played, every swap fades, the first commit's included.

## Alternatives considered

- **Always a dip**: one rule, never two graphs at once, but every edit, even an inserted equaliser,
  leaves a 10 ms hole in a held voice.
- **Always a crossfade, and an edit that cannot crossfade waits for the transport to stop**: no dip
  ever, but reordering two inserts while playing becomes impossible.
- **A fade on every changed edge**: simpler to compile, but a level dip on the most common edits.

## Consequences

- A reordering, or a large edit, dips through silence for about 10 ms, without a step.
- Three rare edits still fade one path twice, down to a quarter of the level for the 5 ms of the
  fade ([debt](../debt.md)).
- The limit of 1.25 times counts nodes, not their cost; once plugins come (M1.6), one node may cost
  a hundred gains, and the limit is to be measured again.
