# 0022. A panic on the real-time path gives silence, and is reported

- Status: Accepted
- Date: 2026-11-11

## Context

Magnolia's own code on the real-time path can still panic, from a bug: an index out of range, an
overflow in a debug build. The `jack` crate catches a panic in the process callback and returns
`Quit` for good. Read in the servers' code on 2026-11-11 (not run):

- JACK2 then deactivates the client, which loses its connections;
- PipeWire ignores the return value and keeps calling, so the buffers last written are probably
  played again, as a buzz;
- nothing tells the rest of Magnolia.

Crash recovery, which would restart a dead engine, does not exist before Stage 2.

## Decision

The owner's choice of 2026-11-11.

**Magnolia catches a panic itself, at the edge of its process callback.** From then on, the
processor is poisoned: every period writes silence to every output and returns normally, so the
client stays active and connected on both servers. A flag tells the controller, which reports it.

**The engine is rebuilt in place.** A new processor replaces the poisoned one in the running
backend, without closing the device; the controller sends the graph again.

**Panics are made rare first.** In real-time modules, Clippy forbids indexing that can panic,
`unwrap`, `expect`, `panic!` and arithmetic that can overflow; a port's buffer shorter than the
period is taken as silence.

## Alternatives considered

- **`panic = "abort"`**: simpler, and crash recovery is needed in any case, but it does not exist
  before Stage 2: until then, a bug would stop the sound for good.
- **The `jack` crate's behaviour**: different on each server, silent on one and buzzing on the
  other, and reported to no one.
- **Catching a panic per node and carrying on**: keeps the rest of the graph playing, but the
  processor's own state may be half-changed when the panic unwinds through it.

## Consequences

- A panic is the one moment the real-time path may allocate and lock: unwinding allocates its
  payload, and the panic hook writes to standard error. It happens once, after a bug.
- The poisoned processor goes back to the backend's handle, which drops it on its own thread, never
  on the audio thread.
- Panics in plugins never reach this path: plugins run in other processes
  ([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)).
