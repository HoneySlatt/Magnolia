# 0030. Takes in WAV files of unknown length, repaired on recovery

- Status: Accepted; amends 0029 (what the WAV writer writes and the reader reads)
- Date: 2026-11-12

## Context

M1.5 records a voice. [ADR-0009](0009-the-project-in-sqlite-nothing-lost.md) asks that recordings
survive a crash, in a format "whose length can be unknown, or whose header is updated as it goes,
and repaired on recovery", with broadcast timestamps kept. The floor "Crashes" asks that a kill lose
no sample written before it ([ROADMAP](../ROADMAP.md#floors)). A WAV file states the sizes of its
data in its header, which a take does not know until it ends.

The field, read in code and manuals on 2026-11-12 ([survey](../survey.md#recording-2026-11-12)):

- Ardour records float RF64 that falls back to WAV when small, one mono file per channel, its sizes
  written once at the end; a crashed take holds a RIFF size of 8 and a data size of 0. It syncs
  nothing during a take, keeps a log of takes beside the session, and repairs the headers on the
  next load. A block the disk was too slow for is dropped, so the rest of the take lands early.
- Pro Tools writes Broadcast WAV and stamps a take with the timeline position where its recording
  began; Ardour writes that position in `bext`'s `TimeReference` and reads it back.
- With a data size of 0xFFFFFFFF, FFmpeg, SoX and libsndfile read a file to its end. With 0, SoX
  reads nothing, and libsndfile reads to the end only beside a RIFF size of exactly 8.
- Apple's CAF defines a data size of -1 as unknown. BW64 (ITU-R BS.2088) defines none, and has a
  recorder reserve a `JUNK` chunk where `ds64` goes past 4 GiB.

Measured during the design's challenge, on the high tier's NVMe under load: syncing 8 files one
after the other stalls a writer 13 ms a second, 3 ms in parallel; 32 files, 26 ms and 5 ms.

## Decision

The owner's choice of 2026-11-12.

**A take is a WAV file whose sizes say "unknown" until it ends.** Its RIFF size and its data size
are 0xFFFFFFFF while it records, so that the common readers open an unfinished take whole. At the
end the writer writes the true sizes: as RIFF, or as RF64 past 4 GiB, its `JUNK` chunk becoming
`ds64`. The data size is written last, so that a crash in between leaves a file that still reads as
unfinished.

**Each take carries a `bext` chunk** (EBU Tech 3285, version 2): its `TimeReference` is the
timeline sample of the file's first frame, its Originator `Magnolia`, its OriginatorReference unique
to the take. The marks say which files are Magnolia's takes; the time reference places a take
without any other file.

**Samples are 32-bit floats**: what the device gives, bit for bit. One file per armed track and
take, its channels interleaved, as the disk reader streams one file per clip
([ADR-0026](0026-one-stream-per-audio-track.md)).

**Frames reach the file as they come, and are synced every second.** A kill loses only what had
not yet left the engine's ring, a few tens of milliseconds; a power cut, about the last second. The
syncs run on a thread of their own, files in parallel. The folder is synced when take files are made,
which happens ahead of the takes; a take that recorded nothing leaves no file.

**Frames the disk was too slow for become silence**, counted and reported, so that the rest of the
take stays in place.

**Recovery repairs what a crash left.** Under an exclusive lock on the recording folder, which
recording also holds, it finds Magnolia's takes whose sizes say unknown, cuts each file to its last
whole frame, syncs, writes the sizes as the end of a take does, and syncs again. A crash during a
repair leaves a file that repairs again. A file whose sizes agree with its length, or that is not
Magnolia's take, is never touched.

## Alternatives considered

- **A header rewritten as the take goes** (REAPER): a cut file reads anywhere without repair, but
  many more writes, and after a power cut a header may claim data that never reached the disk; a
  repair is needed all the same.
- **CAF**, whose unknown length is defined: the right tool on paper, but rare outside macOS, a
  second format to write and read, and a conversion before a take leaves for a studio.
- **A journal of raw samples converted at the end**: every take written twice, and one format more.
- **24-bit integers**: a third smaller, but no longer the device's samples bit for bit.
- **One mono file per channel** (Ardour, Pro Tools): handy to edit a channel alone, but a clip of
  several files, which the disk reader does not stream.

## Consequences

- The WAV writer gains the unknown sizes and `bext`; the reader reads `bext`, and a repair joins
  them. Both are fuzzed.
- A take past 4 GiB that crashed reads whole through Magnolia's repair or a reader that reads to the
  file's end; its RF64 header is written only at its end.
- What "nothing lost" promises for a take: every frame the writer wrote before a kill; every frame
  synced before a power cut.
- The sync's stall on the lowest tier's storage is measured by the owner; the ring's length follows
  from it.
