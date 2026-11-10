# 0010. The quality bar

- Status: Accepted; amended by [0011](0011-magnolia-no-longer-a-program-of-uwuis-first-stage.md)
  (the screen of reference, Magnolia's own) and [0014](0014-the-quality-bar-measured-against-the-market.md)
  (rivals measured, floors and leads, runs of eight hours)
- Date: 2026-11-06

## Context

The owner's rules ask every project to name its references, its hardware tiers and its budgets,
and to measure them. A DAW's bar has three parts: the sound (latency, no xrun), the interface
(240 Hz at 4K, in step with the sound) and rest (nothing done when nothing happens).

The challenge of this design corrected the first proposal in five ways:

- one hour at 64 frames is 2.7 million blocks, so a 99.9th percentile lets 2,700 of them run long:
  the worst block is judged too;
- waking workers and plugin hosts needs system calls, so "no system call" became an allowlist;
- the lowest tier cannot draw 4K at 240 Hz, and its memory is the first limit: it got its own
  interface and memory budgets;
- a MIDI-armed track must sound at once, so it is not rest;
- killing a process does not test a sync to disk: power cuts are simulated.

## Decision

**The budgets, references and tiers are those of the
[ROADMAP's quality bar](../ROADMAP.md#quality-bar)**, which the owner accepted on 2026-11-06. This
ADR records why they are set as they are; the figures live there alone.

**64 frames on the high tier, 128 on the lowest.** At 64 frames a Scarlett's round trip is about
6.5 ms, which a singer does not notice. At 128 it is about 13 ms, which a singer may notice; the
lowest tier's two cores need that margin.

**Rest keeps the audio device open.** The owner's rules ask for no wake-up at rest. A DAW's device
wakes the program each period while it is open; closing it delays the next play and the next
note. Whether Magnolia releases the device after a while at rest is decided in Stage 1, on the
measured cost of each ([ROADMAP](../ROADMAP.md#open-decisions)). Every other wake-up of Magnolia's
own is forbidden at rest, and plugin hosts receive no calls.

**Magnolia's screen of reference for uwui is the arrangement playing**
([uwui-needs.md](../uwui-needs.md#a-screen-of-reference-for-magnolia)), chosen by the owner.

## Alternatives considered

- **Stricter, 32 frames on the high tier**: little gain on common USB interfaces, and more xruns
  from plugins under Wine.
- **Laxer, 128 frames on the high tier**: a delay that a singer may hear.
- **A smaller screen of reference, or the piano roll**: closer to a beat, but not to a film's
  session, where uwui is weighed hardest.

## Consequences

- Each budget has a benchmark in the repository; those that need the hardware run by hand, their
  figures in `docs/measurements.md`.
- A budget missed is debt until it is met.
- The reference projects are defined in Stage 1 and never tuned for afterwards.
