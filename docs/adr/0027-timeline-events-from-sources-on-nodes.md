# 0027. Timeline events from sources attached to nodes

- Status: Accepted; amends 0021 (the events a node receives)
- Date: 2026-11-11

## Context

M1.4 plays MIDI clips into an instrument. M1.6 hosts plugins whose parameters are automated to the
sample, and several MIDI tracks may feed one multi-timbral instrument
([ADR-0008](0008-the-project-model.md)). [ADR-0021](0021-the-node-contract.md)'s events hold only a
parameter's change, and the processor skips a sleeping node whose inputs are silent and which has no
event: a timeline event it cannot see would never wake the node.

Until now every event came from the controller, through a queue the processor drains only as far as
its pending buffer has room. A sender that keeps that buffer full of later events holds back one due
now ([debt](../debt.md), as it stood at M1.3).

CLAP's note event carries a note's id, port, channel, key and a velocity from 0 to 1; its note
expressions name a note by its id.

## Decision

The lead's choice, in the design that the owner's decisions of 2026-11-11
([ADR-0025](0025-delay-compensation-by-reading-ahead.md), [ADR-0026](0026-one-stream-per-audio-track.md))
settled.

**A node may have timeline sources**: MIDI clips at M1.4, automation lanes at M1.6. The compiled
graph lists them per node; the processor reads them for each sub-block at the node's own clock, its
compensation included, merges their events with the controller's, and counts them when it decides
whether the node may sleep. A source's clips are immutable snapshots, freed by the collector.

**Events are structured**: a note on and a note off, each with an id (or none), a channel, a key and
a velocity from 0 to 1; a pitch bend from -1 to 1; a controller's value from 0 to 1; a parameter's
value. At the same offset, note offs come before note ons, then the rest by arrival.

**The transport's edges are musical**: at a play's start, each source sends the last pitch bend and
controller values before the start; at a stop, a note off for every held note, the sustain pedal
released and the pitch bend reset. A note ends at its clip's end. Notes that began before the start
are not played.

**The controller never sends more events than the processor can hold.** The processor publishes how
many events it has taken from its pending buffer; the controller refuses an event once those sent
less those taken reach the buffer's capacity. Every queued event then fits in the pending buffer,
so none due waits behind later ones.

## Alternatives considered

- **Raw MIDI bytes** (`[u8; 3]`): one kind covers notes, bends and controllers, but no note id for
  M1.6's note expressions, and 7-bit velocities.
- **A wrapper node that plays its instrument's clips**: no change to the processor, but the
  processor's skip rule cannot see its events, so a sleeping instrument never wakes; and each new
  kind of source (automation, a second MIDI track) needs another wrapper.
- **Events on graph edges** (a MIDI output into another node's input): needed only for MIDI effects
  between plugins, which Stage 1 does not require; designed when they come.
- **Refusing events far ahead**: bounds how far, not how many; a burst within the coming second
  still holds back one due now.

## Consequences

- MIDI effects between plugins need events on edges, a design of its own when they come.
- Notes held across a play's start are not chased; a later stage may chase them.
- The controller's `send_event` can refuse for want of room; its caller retries after a period.
