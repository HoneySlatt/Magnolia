# 0025. Delay compensation by reading ahead

- Status: Accepted; amends 0007 (how delays compensate latencies) and 0021 (a node's latency, the
  transport it sees); amended by 0032
- Date: 2026-11-11

## Context

A plugin that answers `L` samples late, such as a linear-phase equaliser or a compressor with
lookahead, puts its path `L` samples behind the others. [ADR-0007](0007-the-audio-engine.md) says
that the compiled graph holds the delays that compensate plugins' latencies; M1.4 builds them and
must align a test plugin's latency to the sample. The high tier's reference project monitors a voice
live through Auto-Tune ([reference projects](../reference-projects.md)), and the floor "latency
added" is none.

The field on 2026-11-11 ([survey](../survey.md#delay-compensation-and-disk-streaming-2026-11-11)):

- Ardour reads each track's disk ahead by its path's latency, puts a delay line on each track and on
  each send, and pre-rolls by the session's worst latency at every start and locate.
- REAPER reads media ahead and delays input monitoring.
- Pro Tools delays every track to the longest path, at most 16,383 samples at 48 kHz per channel,
  and suspends compensation on a track armed for recording.
- Tracktion puts a delay on each shorter input of every sum, does not read ahead, starts at once,
  and loses the first samples of latent paths.

Measured during the design's challenge, on the high tier: 1,000 stereo delay lines of 480 to 16,384
samples cost 46 to 118 µs of a 64-frame period at the median, 143 to 194 µs with the cache cold, and
up to 125 MiB; one delay line shared by the same 1,000 sources costs under 1.5 µs.

## Decision

The owner's choice of 2026-11-11.

**A node declares its latency**, in samples, when it is added; it is fixed for the node's life
until M1.6, which brings latency changes. A latency above 65,536 samples is taken as 65,536 and
counted.

**Sources read ahead.** The compiler computes, backwards from the playback channels, how far ahead
of the output each node runs: a node's output runs as far ahead as the most demanding node it feeds,
and its input that far plus its own latency. A source of the timeline, an audio track's player or a
MIDI clip, reads the timeline that far ahead. A chain of latent plugins on a track then needs no
delay line.

**Delay lines only where paths of different latency meet**, one per destination and delay length,
shared by every source that needs that delay there. They are nodes of the arena that the compiler
inserts, so they survive swaps.

**Nothing live is ever delayed.** A node fed, directly or not, by a capture channel is live: no edge
out of it gets a delay, and it runs as far ahead as the least demanding node it feeds. A live voice
through a latent plugin is late by that plugin's latency, never more; what a live node also plays
from the timeline is then out of line, as on an armed track in Pro Tools.

**Play pre-rolls by the largest compensation.** Played from a timeline position, the transport's
clock starts that many samples earlier; each node sees the clock at its own input, ahead by its
compensation; sources produce nothing before the position asked. The sound starts on the position
asked, every latent path already primed, a few milliseconds after the command. A stop ends every
path on the same timeline sample.

## Alternatives considered

- **Delay every track to the longest path** (Pro Tools): simple to explain, but a delay line per
  track, measured above, and the whole output late by the largest latency.
- **Delay lines at every sum, no read-ahead** (Tracktion): play starts at the instant of the
  command, but the first samples of latent paths are lost at every start, an export's opening
  included, and the delay lines are as many as Pro Tools'.
- **One delay line per source**: simpler to compile, but the cost measured above at 1,000 tracks.
- **Delaying input monitoring** (REAPER): keeps a live track in line with its timeline, against the
  floor "latency added: none".

## Consequences

- A delay line exists only at a meeting of paths of different latency, such as a track sent both to
  the master and to a latent bus.
- Pressing play waits for the largest compensation of the session, typically a few milliseconds.
- A swap that changes a playing source's compensation jumps its read position; M1.6, which brings
  latency changes and the mute and resync of ADR-0007, designs how it resynchronises
  ([debt](../debt.md)).
- Plugins (M1.6) see the transport at their own input, as CLAP's transport event expects.
