//! `magnolia recover`: a recording folder recovered after a crash, its takes listed.

use std::error::Error;
use std::path::Path;

use magnolia_engine::record::{Recovery, recover as recover_folder};

use crate::arguments::RecoverOptions;
use crate::say;

/// Recovers the folder `options` name, through `record::recover`: under the folder's lock, it
/// repairs Magnolia's takes whose sizes a crash left unknown, removes those without frames, and
/// leaves every other file untouched. Prints what it found and did.
///
/// # Errors
///
/// A plain message when a recorder or another recovery holds the folder, when it cannot be read,
/// and, after printing, when a take could not be repaired.
pub fn recover(options: &RecoverOptions) -> Result<(), Box<dyn Error>> {
    let recovery = recover_folder(&options.folder)?;
    say(format_args!("Recovered {}:", options.folder.display()));
    print_recovery(&recovery);
    match recovery.failed.len() {
        0 => Ok(()),
        failed => Err(format!(
            "{failed} of the folder's takes could not be repaired; a later recovery tries again"
        )
        .into()),
    }
}

/// Prints what a recovery found and did, each list in the order of its files' names.
pub fn print_recovery(recovery: &Recovery) {
    let repaired = recovery.takes.iter().filter(|take| take.repaired).count();
    say(format_args!(
        "  {} takes, {repaired} of them repaired; {} files removed; {} left untouched; {} failed.",
        recovery.takes.len(),
        recovery.removed.len(),
        recovery.skipped.len(),
        recovery.failed.len()
    ));
    let mut takes: Vec<_> = recovery.takes.iter().collect();
    takes.sort_by(|one, other| one.path.cmp(&other.path));
    for take in takes {
        let named = take.name.as_ref().map_or_else(String::new, |name| {
            format!("track {}, take {}, ", name.track, name.take)
        });
        say(format_args!(
            "  {}: {named}from timeline sample {}, {} frames, {}",
            take.path.display(),
            take.first_sample,
            take.frames,
            if take.repaired {
                "repaired"
            } else {
                "whole already"
            }
        ));
    }
    for path in sorted(&recovery.removed) {
        say(format_args!(
            "  {}: removed, as it held no frame",
            path.display()
        ));
    }
    for path in sorted(&recovery.skipped) {
        say(format_args!(
            "  {}: left untouched, as it is not Magnolia's take",
            path.display()
        ));
    }
    let mut failed: Vec<_> = recovery.failed.iter().collect();
    failed.sort_by(|one, other| one.0.cmp(&other.0));
    for (path, error) in failed {
        say(format_args!("  {}: not repaired: {error}", path.display()));
    }
}

/// `paths`, in order.
fn sorted<P: AsRef<Path>>(paths: &[P]) -> Vec<&Path> {
    let mut sorted: Vec<&Path> = paths.iter().map(AsRef::as_ref).collect();
    sorted.sort();
    sorted
}
