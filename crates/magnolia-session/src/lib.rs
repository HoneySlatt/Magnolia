//! The test session of M1.4: the file that describes a session, its loader into an engine, the
//! offline render, a player for the command line, and the generator of the reference projects
//! ([ADR-0028](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0028-the-test-session-in-json.md)).
//! Only the tests and the command line read it; Stage 2's project, in SQLite, replaces it
//! ([ADR-0009](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0009-the-project-in-sqlite-nothing-lost.md)).
//!
//! - [`file`](mod@file): the session's JSON file, its types, and [`file::parse`], which checks
//!   everything that needs no file on disk;
//! - [`Session::open`] reads a session, opens its audio files, reads its MIDI files into
//!   sequences; [`Session::build`] makes an engine, a disk reader and the graph that plays it;
//! - [`render()`]: the session played offline from its start into a WAV file of floats;
//! - [`SessionPlayer`]: the session played through a backend, for the command line;
//! - [`generate`]: the reference projects without their plugins, and a small session for tests,
//!   the same to the byte on every machine;
//! - [`midi_file`]: Standard MIDI Files, formats 0 and 1, read and written by Magnolia's own code.
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufWriter;
//!
//! use magnolia_session::{Session, render};
//!
//! let session = Session::open("session.json")?;
//! let report = render(&session, 4_096, BufWriter::new(File::create("render.wav")?))?;
//! assert_eq!(report.underruns, 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod build;
pub mod file;
pub mod generate;
pub mod load;
pub mod midi_file;
pub mod play;
pub mod render;

pub use build::{BuildError, BuiltSession, ReaderMode};
pub use load::{LoadError, Session};
pub use play::{PlayReport, SessionPlayer, StartError};
pub use render::{RenderError, RenderReport, render, render_to_file};
