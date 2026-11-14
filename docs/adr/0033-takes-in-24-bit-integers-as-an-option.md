# 0033. Takes in 24-bit integers, as an option

- Status: Accepted; amends 0030 (the takes' samples)
- Date: 2026-11-14

## Context

[ADR-0030](0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md) records takes in 32-bit
floats, the device's samples bit for bit, and lists 24-bit integers among the alternatives it
rejected. M1.5 built the recorder with both formats: 32-bit floats by default, 24-bit integers when
asked (`TakeFormat`, `magnolia record --format int24`). Studios commonly ask for 24-bit files, and
a take in 24 bits is a quarter smaller: 8.6 MB a minute of mono at 48 kHz against 11.5.

## Decision

The owner's choice of 2026-11-14.

**Takes are 32-bit floats by default, and 24-bit integers when the user asks.** A 24-bit take
rounds each sample to the nearest of 2^24 steps, ties away from zero, and clamps what lies outside
full scale, as the WAV writer documents. Everything else ADR-0030 decides holds for both formats.

## Alternatives considered

- **32-bit floats only**, as ADR-0030 wrote it: one format, but tested code removed, and the need
  comes back with the delivery of takes to a studio at Stage 4.

## Consequences

- A 24-bit take is no longer the device's samples bit for bit; the converters of the interfaces the
  bar names are 24-bit, so nothing audible is lost, but a sample above full scale is clipped.
- The tests record in both formats.
