# 0009. The project in SQLite; nothing lost

- Status: Accepted
- Date: 2026-11-06

## Context

The owner wants nothing ever lost: after a crash or a power cut, the project reopens at the last
gesture, its undo history included, with named snapshots made at will
([ADR-0001](0001-what-magnolia-is.md)). FL Studio and Pro Tools save every few minutes. Ardour
keeps an XML file, backups and its undo history; REAPER a text file and a backup
([the survey](../survey.md#saving)).

Three facts shape the design:

- Replaying a journal of commands needs each command to keep its meaning across versions. An update
  of Magnolia with an uncompacted journal on disk would break recovery.
- A plugin's internal state (a wavetable loaded in Serum, a Kontakt patch) is not a command. It is
  recoverable only to its last snapshot ([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)).
- Flushing to disk at each gesture costs from about 1 ms to tens of ms on slow storage, and
  competes with recording.

## Decision

**A project is a folder**: a SQLite file, the audio copied in, peak files as a cache that can be
deleted and rebuilt. Video is referenced, never copied: masters run to hundreds of gigabytes.
Missing media can be relinked.

**SQLite in WAL mode holds the project**, with `synchronous=FULL`, without which WAL is not durable
through a power cut. Each gesture is one transaction. The schema is versioned
and migrated. The undo history is a table. Named snapshots are kept in the same file.

**Every edit is a command.** Commands serve undo, tests, the command line, the MCP server and
accessibility alike. What is written to the file is a command's result, the document's change, not
the command, so that recovery does not depend on a command's code staying the same.

**Writes leave the interface's thread free.** One thread writes. Gestures that arrive while a sync
is under way are committed together at the next one. A gesture is kept once its sync returns; a
power cut can lose only the gestures waiting for that sync, a window whose budget Stage 2 sets on the
lowest tier's storage.

**Recordings survive a crash.** Audio is recorded into a format whose length can be unknown, or
whose header is updated as it goes, and repaired on recovery. Broadcast timestamps are kept for
film.

**A plugin's state is never overwritten when the plugin is missing**: the project keeps it as it was.

**What "nothing lost" promises**: every gesture whose sync returned, and at most the gestures of
one sync window; a plugin's internal state to its last snapshot
([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)).

## Alternatives considered

- **A file of Magnolia's own and a journal, compacted on save**: readable by hand, without a
  dependency, but transactions, recovery and compaction all to write and prove.
- **Saving every few minutes, as FL Studio and Pro Tools**: simpler, but minutes lost.
- **DAWproject as the native format**: open, but made for exchange, and the owner needs none.

## Consequences

- A dependency in C, SQLite, public domain, through `rusqlite`; it is put to the owner when Stage 2
  adds it.
- The file cannot be read in a text editor; a command exports it as text for debugging.
- Recovery is tested by power cuts simulated, not only by killing the process: in CI through a
  SQLite file layer that drops unsynced writes, which needs no root.
