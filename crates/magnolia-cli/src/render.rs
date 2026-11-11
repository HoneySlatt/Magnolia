//! `magnolia render`: a test session rendered offline into a new WAV file.

use std::error::Error;
use std::time::Instant;

use magnolia_session::{Session, render_to_file};

use crate::arguments::RenderOptions;
use crate::say;

/// Renders the session `options` name into a new WAV file of 32-bit floats, through
/// [`render_to_file`], which never writes over a file and makes it appear whole or not at all.
/// Says what it wrote, how fast, and the memory the session asks for, estimated.
///
/// # Errors
///
/// A plain message when the session does not open, when the output file exists or cannot be
/// made, and when the render fails, such as for a block out of range.
pub fn render(options: &RenderOptions) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let session = Session::open(&options.session)?;
    let report = render_to_file(&session, options.block, &options.out)?;
    let audio = report.frames as f64 / f64::from(session.sample_rate().hertz());
    let elapsed = started.elapsed().as_secs_f64();
    say(format_args!(
        "Rendered {} frames ({audio:.1} s of audio) into {} in {elapsed:.2} s, {:.0} times real \
         time; {} underruns; the largest compensation {} samples; the memory the session asks \
         for, estimated: {}.",
        report.frames,
        options.out.display(),
        audio / elapsed,
        report.underruns,
        report.largest_compensation,
        session.memory_estimate()
    ));
    Ok(())
}
