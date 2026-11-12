# 0031. The round trip reported, and takes placed by it

- Status: Accepted
- Date: 2026-11-12

## Context

A singer hears the output late by the device's playback latency, and the voice reaches Magnolia
late by its capture latency. A take lies on the timeline in step with what was heard only when it is
moved earlier by the sum, the round trip. M1.5 must report it, and its floor is that Magnolia's
monitoring path adds nothing to a direct loopback ([ROADMAP](../ROADMAP.md#floors)).

The field ([survey](../survey.md#recording-2026-11-12)):

- JACK gives each port a capture and a playback latency range; a client without a latency callback
  gets its ports' ranges copied from what they connect to. A client with one sets its own.
- JACK2's drivers report the period and the buffering, plus the extra input and output latency
  given to `jackd -I/-O`; no hardware value is read. `jack_iodelay` measures a loopback cable and
  prints the "extra loopback latency" beyond what JACK reports.
- Ardour places a take by its input latency plus its output latency, and calibrates the extra in the
  server, half on each side, from its own measure. REAPER uses the driver's figures plus manual input
  and output offsets.

Measured during the design's challenge on JACK2's dummy driver: a range read right after a connect
was stale in up to 8 reads of 30 at 64 frames, settled within 12 ms; read in a latency callback, it
was right 30 times of 30.

## Decision

The owner's choice of 2026-11-12.

**The round trip is the server's capture latency plus its playback latency, plus an extra of
Magnolia's own**: the "extra loopback latency" the user measured with a loopback cable and
`jack_iodelay`, given to the command line, 0 by default. A take's first frame is the frame captured
that round trip after the output played the take's first timeline sample.

**The backend reads the latencies in a latency callback**, which only stores them, and sets its own
ports' ranges, adding the latency of Magnolia's monitoring path, so that `jack_iodelay` and other
clients see it. It reaches libjack through the `jack` crate's own `jack_sys`: no new dependency.

**Magnolia reports the round trip** when it opens the device with input tracks: the capture and
playback latencies, their sum, the extra, the latency its monitoring path adds, in frames and
milliseconds.

**A change of the device's latency during a take ends the take**, reported, rather than placing the
rest of it wrong.

## Alternatives considered

- **The server's figures alone, the extra set in the server** (Ardour): one setting for every JACK
  program, but on PipeWire, the owner's server, it lives in a node's configuration, and a wrong
  value shifts every program's recordings.
- **Magnolia measuring the extra itself** with a loopback, as Ardour's calibration does: no other
  tool to run, but measuring code to write and prove now, while `jack_iodelay` does the job. An idea
  for the stage that brings the interface.
- **Reading the ranges right after connecting**, without a callback: simpler, but measured stale.

## Consequences

- The extra is given on the command line until Magnolia has settings, per device, at Stage 2 or 3.
- The owner measures the round trip with a loopback cable on the Focusrite, directly and through
  Magnolia's monitoring path.
