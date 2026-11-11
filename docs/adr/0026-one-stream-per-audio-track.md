# 0026. One stream per audio track, from the disk reader

- Status: Accepted; amends 0007 (where audio clips' snapshots go) and 0023 (the disk reader's wake)
- Date: 2026-11-11

## Context

[ADR-0007](0007-the-audio-engine.md) says that the timeline reaches the real-time thread as
immutable snapshots, one per clip, and that a disk thread reads audio ahead of the playhead. M1.4
plays audio clips from disk. Stage 2 edits clips while playing, at 240 Hz when one is dragged, in
sessions of up to 1,000 tracks and 50,000 clips; M1.9 plays the reference projects in a loop for
eight hours. The lowest tier has two cores, 4 GB and storage not yet known.

The field ([survey](../survey.md#delay-compensation-and-disk-streaming-2026-11-11)): Ardour mixes
each track's clips into one ring on its disk thread, which the process thread wakes, and rewrites
the ring in place on an edit; Tracktion mixes clips on the audio thread from a cache of
memory-mapped file sections.

Measured during the design's challenge, on the high tier: a block of 4,096 frames read and decoded
in 5.4 µs at the median, warm; decoding 0.96 ns per stereo frame; 16 streams read cold at 2.3 to
2.7 GiB/s from NVMe.

## Decision

The owner's choice of 2026-11-11.

**One stream per audio track.** The disk reader, a thread of its own, mixes the track's audio clips
into the stream; the track's player, on the real-time thread, only copies from it. Audio clips'
snapshots go to the disk reader; MIDI clips' snapshots go to the real-time thread, as ADR-0007 says.

**The stream is a ring of block slots indexed by timeline position**, not a queue. Each slot holds a
block of frames and one atomic word: its generation, its first timeline sample, and whether it is
silent. The player publishes the position it reads; the reader fills, or rewrites in place, any slot
far enough ahead of it. An edit, a locate near what is held, or a loop then rewrites only the slots
concerned. A player over a silent block flags its output silent, so that its chain sleeps
([ADR-0021](0021-the-node-contract.md)).

**The reader reads ahead 10 s by default, in reads of at least 256 KB.** The processor wakes it when
a stream holds less than its threshold ahead, through `wake_disk_reader`, a futex wake added to the
real-time allowlist ([ADR-0023](0023-the-real-time-allowlist.md)). Stopped, it sleeps until a
command comes.

**A frame the player lacks is silence**, counted as an underrun, never waited for. The offline render
drives the same reader on its own thread, before each block, so a render never underruns.

## Alternatives considered

- **A cache per clip, mixed on the real-time thread** (Tracktion): ADR-0007 unchanged and a moved
  clip discards nothing, but more work on the real-time thread and a table of blocks shared through
  atomics, for edits that come only at Stage 2.
- **A queue of blocks per track**: the simplest, but nothing queued can be rewritten, so an edit, a
  locate while playing or a loop must drop or play stale audio.
- **The reader woken by a timer**: no new system call on the real-time path, but a refill waits for
  the timer, and the tests cannot play faster than real time.

## Consequences

- At 10 s of read-ahead, the lowest tier's reference project holds about 42 MB of streams.
- How an edit during playback reaches the slots is built at Stage 2, on this structure.
- Overlapping clips on one track are refused until Stage 2 decides how layers play.
- The reader's I/O priority is left at the default until the lowest tier's measures show it needed;
  the best-effort class at its highest level needs no privilege.
