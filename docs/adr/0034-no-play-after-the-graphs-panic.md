# 0034. After the graph's panic, stops are kept and plays refused

- Status: Accepted; amends 0022 (what a poisoned processor still does) and 0032 (how a take ends
  after the graph's panic)
- Date: 2026-11-14

## Context

[ADR-0022](0022-a-panic-on-the-real-time-path-gives-silence.md) makes a panic on the real-time path
poison the processor: silence from then on, until the owner of the backend replaces it.
[ADR-0032](0032-recording-by-a-capture-tap-of-the-processor.md) keeps the takes recording after the
graph's panic, through a capture tap beside the graph. Neither says what becomes of the transport's
commands then. Frozen, a stop never reaches the takes, which record on, open-ended, until the
processor is replaced. Followed in full, a play after the panic would start a new take while the
user hears nothing.

## Decision

The owner's choice of 2026-11-14.

**After a panic of the graph, the processor still takes the transport's stops**, each period, and
moves the transport's clock: a stop ends the play under way where it would have, and its takes end
whole.

**A play is refused.** A play sent after the panic, or one that was waiting, never starts; one sent
while a play rolls ends that play as a stop would. Each is counted in `EngineStats::plays_refused`.
No take begins on a silent output.

**A panic of the period's start** (a swap, the transport, the events) still freezes everything, as
ADR-0022 says: the transport may be half-changed. Replacing the processor drops its taps, which
ends their takes.

## Alternatives considered

- **Every command followed**, plays included: a voice is captured even with the music silent, but a
  take may start that the user cannot hear, which an interface would have to explain.
- **Every command frozen**, as before M1.5: the simplest rule, but a take under way ends only when
  the processor is replaced, without a clean stop.

## Consequences

- The transport has a second way to apply its commands, used only after the graph's panic.
- Until Stage 3's interface, the command line says the engine panicked, and the counter says which
  plays were refused.
