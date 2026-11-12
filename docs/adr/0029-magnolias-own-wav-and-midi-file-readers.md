# 0029. Magnolia's own WAV and MIDI file readers

- Status: Accepted; amended by 0030
- Date: 2026-11-11

## Context

From M1.4 the disk reader streams WAV files: 16-, 24- and 32-bit integers and 32-bit floats, any
channel count. A film's session at 96 kHz or with many channels passes 4 GB, which only RF64 and
BW64 hold. M1.5 records into WAV files that a crash may leave with a data size of 0 or 0xFFFFFFFF.
The test session reads Standard MIDI Files, formats 0 and 1; the generator writes both kinds of
files, which REAPER must open as they are for the bar's efficiency comparison.

On 2026-11-11 ([survey](../survey.md#delay-compensation-and-disk-streaming-2026-11-11)): `hound`
3.5.1 (2023) and `symphonia` 0.6.1 (2026) read 24-bit, float and WAVE_FORMAT_EXTENSIBLE but no RF64;
`midly` 0.5.3 (2023, Unlicense) reads and writes MIDI files fully, its `parallel` feature pulling
rayon.

## Decision

The owner's choice of 2026-11-11.

**Magnolia reads and writes WAV files with its own code**, in the engine's `wav` module: RIFF, RF64
and BW64; PCM, IEEE float and WAVE_FORMAT_EXTENSIBLE; 16, 24 and 32 bits; unknown chunks skipped,
a chunk's pad byte honoured.

**Magnolia reads and writes Standard MIDI Files with its own code**, in `magnolia-session`: formats
0 and 1, running status, notes, pitch bends and controllers, tempo and time-signature meta events;
everything else skipped.

**Both readers are fuzzed**, and the cases fuzzing finds only by luck are tested by hand: a pad byte
after an odd-sized chunk, a data size of 0 or 0xFFFFFFFF, chunks after the data.

## Alternatives considered

- **`hound` and `midly`**: less code, but RF64 would still be Magnolia's to write, and `midly` adds
  the Unlicense to the licenses allowed.
- **`symphonia` and `midly`**: a whole decoding framework, which may serve importing MP3 or FLAC at
  Stage 2, but without RF64, and heavier than streaming needs.

## Consequences

- No dependency for audio or MIDI files; their decoding stays under the disk reader's control.
- Importing compressed formats, at Stage 2, converts them to WAV, and may take a decoding crate then.
