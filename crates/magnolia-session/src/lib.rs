//! The test session of M1.4: the file that describes a session, its loader into an engine, the
//! offline render and the generator of reference projects
//! ([ADR-0028](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0028-the-test-session-in-json.md)).
//! Only the tests and the command line read it; Stage 2's project, in SQLite, replaces it
//! ([ADR-0009](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0009-the-project-in-sqlite-nothing-lost.md)).
//!
//! For now it holds one module:
//!
//! - [`midi_file`]: Standard MIDI Files, formats 0 and 1, read and written by Magnolia's own code.

pub mod midi_file;
