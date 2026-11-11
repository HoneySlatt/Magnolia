//! Renders a test session offline into a WAV file of 32-bit floats.
//!
//! ```sh
//! cargo run --release -p magnolia-session --example render -- /tmp/magnolia-lowest/session.json \
//!     /tmp/magnolia-lowest.wav
//! ```
//!
//! It plays the session from its start, in blocks of 4,096 frames or `--block N`, until its last
//! clip's end, its compensation and one second of tail, and says how many frames it wrote, how
//! long it took, the underruns (always 0) and the session's largest compensation. It refuses to
//! write over a file.

use std::env;
use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;
use std::time::Instant;

use magnolia_session::{Session, render};

const USAGE: &str = "usage: render <session.json> <out.wav> [--block FRAMES]";

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let (session_path, out_path, block) = match arguments.as_slice() {
        [session, out] => (session, out, 4_096),
        [session, out, flag, frames] if flag == "--block" => match frames.parse() {
            Ok(frames) => (session, out, frames),
            Err(_) => {
                eprintln!("--block takes a whole number of frames, not {frames:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let start = Instant::now();
    let session = match Session::open(session_path) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let opened = start.elapsed();
    let out = match File::create_new(out_path) {
        Ok(out) => out,
        Err(error) => {
            eprintln!("{out_path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match render(&session, block, BufWriter::new(out)) {
        Ok(report) => {
            let rate = f64::from(session.sample_rate().hertz());
            let seconds = start.elapsed().as_secs_f64();
            println!(
                "rendered {} frames ({:.1} s of audio) into {out_path} in {seconds:.2} s, the \
                 session opened in {:.3} s: {:.0} times real time; {} underruns; largest \
                 compensation {} samples",
                report.frames,
                report.frames as f64 / rate,
                opened.as_secs_f64(),
                report.frames as f64 / rate / seconds,
                report.underruns,
                report.largest_compensation
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
