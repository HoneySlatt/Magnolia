# 0028. The test session in JSON

- Status: Accepted
- Date: 2026-11-11

## Context

M1.4 plays a test session from disk: a file that only the tests and the command line read, replaced
by Stage 2's project in SQLite ([ADR-0009](0009-the-project-in-sqlite-nothing-lost.md)). It holds
the sample rate, tempo and meter changes, audio and MIDI tracks with their clips (WAV and MIDI files
beside it), test inserts, buses and sends. A generator writes it; a person may correct it by hand.
M1.8's MCP server speaks JSON-RPC.

## Decision

The owner's choice of 2026-11-11.

**The test session is a JSON file, read with `serde` and `serde_json`.** Its schema is strict: a
field unknown, a version newer than the reader's, or a value out of range refuses the file with a
plain error. Its first fields name the format and its version.

**Positions are integers**: time units for absolute positions, ticks for musical ones
([ADR-0018](0018-the-time-model.md)), samples for offsets into a file. Paths are relative to the
session file.

**JSON that leaves Rust writes time units as strings**: they pass 2^53 after 2 h 46 min, which a
JavaScript client rounds. The test session is read by Rust alone; M1.8's schema follows this rule.

The two crates and what they pull (`serde_core`, `serde_derive`, `proc-macro2`, `quote`, `syn`,
`unicode-ident`, `itoa`, `memchr`, `zmij`) are under MIT, Apache-2.0 or Unicode-3.0, maintained, and
live only in `magnolia-session`: the engine never sees them.

## Alternatives considered

- **TOML with `serde`**: comments and easier by hand, but a larger tree, and a second format once
  JSON comes with MCP.
- **A format of Magnolia's own**: no dependency, but a parser to write, document and fuzz, thrown
  away at Stage 2.

## Consequences

- No comments in the file; the generator and `docs/reference-projects.md` say what a session holds.
- `serde_json` is in the workspace before M1.8 needs it.
- The loader is fuzzed with arbitrary text, so that a malformed session never crashes Magnolia.
