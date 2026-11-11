//! Generates a test session into an empty folder: the lowest or the high tier's reference project
//! without its plugins, or the small session of the tests.
//!
//! ```sh
//! cargo run --release -p magnolia-session --example generate -- lowest /tmp/magnolia-lowest
//! ```
//!
//! It writes `session.json`, `audio/*.wav` and `midi/*.mid`, the same to the byte on every machine,
//! and says how many files and bytes it wrote and how long it took. It refuses a folder that holds
//! anything.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use magnolia_session::generate::{Kind, generate};

const USAGE: &str = "usage: generate <lowest|high|small> <folder>";

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let [kind, folder] = arguments.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let kind: Kind = match kind.parse() {
        Ok(kind) => kind,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let folder = PathBuf::from(folder);
    let start = Instant::now();
    match generate(kind, &folder) {
        Ok(generated) => {
            println!(
                "wrote {} audio files, {} MIDI files and session.json into {}: {} bytes ({:.2} GB) \
                 in {:.1} s",
                generated.audio_files,
                generated.midi_files,
                folder.display(),
                generated.bytes,
                generated.bytes as f64 / 1e9,
                start.elapsed().as_secs_f64()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
