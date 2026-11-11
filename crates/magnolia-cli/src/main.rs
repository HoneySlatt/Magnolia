//! `magnolia`, Magnolia's command line until the interface comes at Stage 3: it plays a test
//! session through JACK, or renders one offline into a WAV file. `magnolia --help` gives its usage;
//! `docs/ARCHITECTURE.md`, under *The command line*, says how it works.
//!
//! It exits with 0 when the command did what was asked, 2 for a misuse of the command line, with
//! the usage, and 1 for any other failure, said plainly on standard error.

mod arguments;
mod play;
mod render;

use std::env;
use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;

use arguments::{Command, USAGE};

/// The exit code of a misuse of the command line.
const MISUSE: u8 = 2;

fn main() -> ExitCode {
    let command = match arguments::parse(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            complain(format_args!("magnolia: {error}\n\n{USAGE}"));
            return ExitCode::from(MISUSE);
        }
    };
    let done = match command {
        Command::Help => {
            say(USAGE.trim_end());
            Ok(())
        }
        Command::Play(options) => play::play(&options),
        Command::Render(options) => render::render(&options),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            complain(format_args!("magnolia: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Prints a line on standard output. A closed output, such as a pager quit early, is no reason
/// to stop: the play must still close its client, so the failure is let go.
fn say(line: impl fmt::Display) {
    let _ = writeln!(io::stdout().lock(), "{line}");
}

/// Prints a line on standard error; a closed one is let go, as in [`say`].
fn complain(line: impl fmt::Display) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}
