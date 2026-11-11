# 0018. The time model

- Status: Accepted; amends 0007 (positions in time units, not samples)
- Date: 2026-11-11

## Context

[ADR-0007](0007-the-audio-engine.md) decided that time is exact: 64-bit sample positions,
fixed-point musical time, never float beats, a tempo map with tempo and meter changes, and
timecode including 23.976 and 29.97 drop-frame. Milestone M1.2 builds it. The project file of
Stage 2 will store musical positions, tempos and absolute positions, so their units are data
formats, costly to change later.

The field on 2026-11-11 ([the survey](../survey.md#time-models-2026-11-11)):

- Pro Tools counts 960,000 ticks per quarter note, Ardour 1,920; REAPER, Tracktion, CLAP and VST3
  use floating-point beats or tempos for some of their values.
- Ardour stores a tempo as a whole number of "superclocks" (1/282,240,000 s) per note, and every
  absolute position in superclocks, so that its positions do not depend on the sample rate.
- Pro Tools offers 19 timecode rates, six of them drop-frame, among them 59.94 drop-frame
  (SMPTE ST 12-3). FFmpeg skips four of its labels a minute; the standard's text was not read.
- No DAW found stores a tempo as an exact fraction.

Converting between musical time and timecode through samples would make a note's frame depend on
the session's sample rate: at 120 BPM, tick 322,780 lies in frame 0 of 29.97, but plays on a sample
of frame 1 at 44.1 kHz (the test `a_ticks_frame_does_not_depend_on_the_sample_rate`).

## Decision

**Musical positions are 64-bit ticks, 4,838,400 to a quarter note** (2^10 · 3^3 · 5^2 · 7). Binary
divisions to 1/1024 of a quarter, triplets three deep, quintuplets two deep and septuplets are whole
numbers of ticks, and so are MIDI files' common resolutions, from 24 to 15,360 a quarter; others,
such as 1,000 or 2,048, round on import. A tick is no longer than a sample at 768 kHz from 9.53 BPM
up. The range covers 1.9 · 10^12 quarter notes.

**Absolute positions are 64-bit counts of time units, 903,168,000,000 to a second**
(2^17 · 3^2 · 5^6 · 7^2). This amends ADR-0007, which counted them in samples. A sample at every
standard rate from 8 kHz to 768 kHz, a frame at every timecode rate, an eighth of a frame and a
microsecond are each a whole number of time units, so a project keeps its positions exactly when its
sample rate changes. The range is about 118 days either side of zero. The engine still works in
64-bit sample positions, converted from time units for each block.

**A tempo is the length of a quarter note, a whole number of time units**, between 1 and 1,000
quarter notes a minute, as Ardour stores it in superclocks. A typed tempo such as 174 or 93.5 BPM is
kept to the nearest time unit, within 5.6 · 10^-13 s a quarter note: under a hundredth of a sample
over ten hours at 300 BPM and 48 kHz, and shown back as typed. Tempos taken from frames or from
MIDI's microseconds are exact.

**Every position converts through its exact moment.** A moment counts units of
1/(903,168,000,000 · 4,838,400) s, in which a tick at any tempo is a whole number. A tick's moment
is summed exactly from the tempo change before it, so nothing accumulates however many changes a map
holds. All the arithmetic is on integers, with 128-bit intermediates; only the last step of a
conversion rounds.

**One rounding rule.** Placing a position on another grid gives the first point of that grid at or
after its moment: a note plays on the first sample at or after its instant, never early, at most one
sample late. Reading where a position falls gives the last point at or before its moment: the frame
shown for a sample is the frame it lies in. Placing on a finer grid and reading back gives the
position again; no conversion drifts with the distance from zero. Divisions round toward minus
infinity, so negative positions follow the same rule.

**The tempo map** lays ticks on time units from a song start, the time position of tick 0. Tempo
changes lie at ticks, meter changes at bars; the first of each also holds before the start. Tempos
are constant between changes.

**Timecode rates** are 23.976, 24, 25, 29.97, 29.97 drop-frame, 30, 50, 59.94, 59.94 drop-frame and
60. A timecode clock lays frames on time units from a start label at any time position, between two
frames if need be. Labels come round every 24 hours; a label stands for the frame nearest the
clock's start.

## Alternatives considered

- **960 or 1,920 ticks per quarter** (REAPER, Ardour): a tick lasts 25 or 12.5 samples at 120 BPM
  and 48 kHz, so recorded notes would not fall on their sample.
- **960,000 ticks per quarter** (Pro Tools): fine enough, but septuplets and nine-tuplets are not
  whole numbers of ticks.
- **Binary fixed-point beats**: triplets are never exact, so a note on a triplet grid fails to
  compare equal to its grid line.
- **A tempo as an exact fraction of BPM**: every typed tempo exact, but each tempo change's moment
  then has its own denominator, which forces rounding at each change and harder arithmetic, for a
  difference no one can hear.
- **Absolute positions in samples** (ADR-0007 as it was, Pro Tools): simpler, but every change of
  sample rate rounds every position, and each further change rounds again, so positions drift.
- **Rounding to the nearest point everywhere**: a note within half a sample rather than one, but a
  frame's nearest sample can fall before the frame, so a clip snapped to a frame would show the
  previous frame.
- **Converting between grids through samples**: one conversion fewer, but a tick's frame then
  depends on the sample rate.

## Consequences

- A conversion costs tens of nanoseconds, and a block's conversions under a microsecond, with 1 to
  1,000 tempo changes ([measurements](../measurements.md#m12--the-time-model)).
- Sample rates whose sample is not a whole number of time units, such as the pulled-down 44,056 Hz,
  are refused: a session runs at its nominal rate.
- 11- and 13-tuplets are not exact. Refining the ticks or the time unit later by a whole factor is a
  lossless multiplication, migrated like any format change.
- A meter change moves the bars after it, not the tempo changes, which lie at ticks: the command that
  edits a meter must move the later tempo changes too, if they are to stay on their bars.
- Tempo ramps are not decided. A ramp linear in time is not exact within 128-bit integers; whether
  ramps are stored as steps of constant tempo or as a shape with a written rule of its own is an
  open decision of the [ROADMAP](../ROADMAP.md#open-decisions).
- A tempo map frees memory when it is dropped, so a replaced map must be freed off the real-time
  thread, by the collector, like the graph.
