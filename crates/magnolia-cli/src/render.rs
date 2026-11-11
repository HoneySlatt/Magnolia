//! `magnolia render`: a test session rendered offline into a new WAV file.

use std::error::Error;
use std::fs::{self, File};
use std::io::BufWriter;
use std::time::Instant;

use magnolia_session::Session;

use crate::arguments::RenderOptions;
use crate::say;

/// Renders the session `options` name into a new WAV file of 32-bit floats, through
/// [`magnolia_session::render()`], and says what it wrote and how fast.
///
/// # Errors
///
/// A plain message when the session does not open, when the output file exists or cannot be
/// created, and when the render fails, such as for a block out of range; the incomplete file is
/// then removed.
pub fn render(options: &RenderOptions) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let session = Session::open(&options.session)?;
    let out = &options.out;
    // Never over a file: what it holds may be the user's.
    let file = File::create_new(out)
        .map_err(|error| format!("{} cannot be created: {error}", out.display()))?;
    let report = match magnolia_session::render(&session, options.block, BufWriter::new(file)) {
        Ok(report) => report,
        Err(error) => {
            // The file is the render's own, created above, and holds no whole render.
            return Err(match fs::remove_file(out) {
                Ok(()) => error.into(),
                Err(removal) => format!(
                    "{error}; the incomplete {} could not be removed: {removal}",
                    out.display()
                )
                .into(),
            });
        }
    };
    let audio = report.frames as f64 / f64::from(session.sample_rate().hertz());
    let elapsed = started.elapsed().as_secs_f64();
    say(format_args!(
        "Rendered {} frames ({audio:.1} s of audio) into {} in {elapsed:.2} s, {:.0} times real \
         time; {} underruns; the largest compensation {} samples.",
        report.frames,
        out.display(),
        audio / elapsed,
        report.underruns,
        report.largest_compensation
    ));
    Ok(())
}
