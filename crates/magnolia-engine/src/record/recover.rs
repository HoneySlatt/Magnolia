//! Recovering a recording folder after a crash: Magnolia's takes repaired, those without frames
//! removed, every other file left as it was
//! ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).

use std::fs::{self, File, OpenOptions};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use super::files::{FolderLock, TakeName, open_folder};
use super::{ORIGINATOR, RecordError};
use crate::wav::{Repair, WavError, read_info, repair};

/// What [`recover`] did in a folder.
#[derive(Debug, Default)]
pub struct Recovery {
    /// Magnolia's takes the folder holds, each with its frames, repaired or not, in no set order.
    pub takes: Vec<RecoveredTake>,
    /// The files removed: takes without frames, and empty files named as takes, which a recorder
    /// made ahead and never used.
    pub removed: Vec<PathBuf>,
    /// Files named as takes that are not Magnolia's: unreadable as WAV, or of another originator.
    /// They were left untouched.
    pub skipped: Vec<PathBuf>,
    /// Magnolia's takes that could not be repaired, with why; left as the failed repair left them,
    /// which a later recovery takes up again.
    pub failed: Vec<(PathBuf, WavError)>,
}

/// One of Magnolia's takes, as recovery found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredTake {
    /// The take's file.
    pub path: PathBuf,
    /// The track and take its file's name gives, if it is named as a take.
    pub name: Option<TakeName>,
    /// The timeline sample of its first frame: its `bext` chunk's time reference.
    pub first_sample: u64,
    /// Its whole frames.
    pub frames: u64,
    /// Whether recovery repaired it: its writer had died before writing its sizes.
    pub repaired: bool,
}

/// Recovers a recording folder: under its exclusive lock, repairs with [`wav::repair`](repair)
/// every file that is Magnolia's take, its `bext` chunk's originator `Magnolia`; removes those
/// without frames once repaired, and the empty files named as takes; leaves every other file
/// untouched, and lists each take. The folder is synced once its files are removed.
/// [`Recorder::start`](super::Recorder::start) runs it on its folder.
///
/// A file is looked at only if its name ends in `.wav`. A take whose repair fails is listed in
/// [`Recovery::failed`] and the others are still recovered.
///
/// # Errors
///
/// [`RecordError::FolderLocked`] while a recorder or another recovery holds the folder's lock;
/// [`RecordError::Folder`] when the folder cannot be read, locked or synced.
pub fn recover(folder: &Path) -> Result<Recovery, RecordError> {
    let _lock = FolderLock::take(folder)?;
    recover_locked(folder)
}

/// Recovers `folder`, whose lock the caller holds.
pub(crate) fn recover_locked(folder: &Path) -> Result<Recovery, RecordError> {
    let folder_error = |error| RecordError::Folder {
        folder: folder.to_path_buf(),
        error,
    };
    let mut recovery = Recovery::default();
    for entry in fs::read_dir(folder).map_err(folder_error)? {
        let entry = entry.map_err(folder_error)?;
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str().filter(|name| name.ends_with(".wav")) else {
            continue;
        };
        if !entry.file_type().map_err(folder_error)?.is_file() {
            continue;
        }
        recover_file(path, TakeName::parse(file_name), &mut recovery);
    }
    if !recovery.removed.is_empty() {
        open_folder(folder)?.sync_all().map_err(folder_error)?;
    }
    Ok(recovery)
}

/// Recovers one file of the folder, named as a take or not, and notes what became of it. Only a
/// file found to be Magnolia's take is opened for writing.
fn recover_file(path: PathBuf, name: Option<TakeName>, recovery: &mut Recovery) {
    let fail = |recovery: &mut Recovery, path: PathBuf, error: WavError| {
        if name.is_some() {
            recovery.failed.push((path, error));
        }
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => return fail(recovery, path, WavError::Io(error)),
    };
    if file.metadata().is_ok_and(|metadata| metadata.len() == 0) {
        drop(file);
        if name.is_some() {
            remove(path, recovery);
        }
        return;
    }
    let is_take = read_info(&mut BufReader::new(&file))
        .ok()
        .and_then(|info| info.bext)
        .is_some_and(|bext| bext.originator() == ORIGINATOR.as_bytes());
    drop(file);
    if !is_take {
        if name.is_some() {
            recovery.skipped.push(path);
        }
        return;
    }
    let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) => return recovery.failed.push((path, WavError::Io(error))),
    };
    let repaired = match repair(&mut file) {
        Ok(Repair::Untouched) => false,
        Ok(Repair::Repaired { .. }) => true,
        Err(error) => return recovery.failed.push((path, error)),
    };
    let info = read_info(&mut BufReader::new(&file));
    drop(file);
    match info {
        Ok(info) if info.frames == 0 => remove(path, recovery),
        Ok(info) => recovery.takes.push(RecoveredTake {
            path,
            name,
            first_sample: info.bext.map_or(0, |bext| bext.time_reference),
            frames: info.frames,
            repaired,
        }),
        Err(error) => recovery.failed.push((path, error)),
    }
}

/// Removes a file without frames, or notes why it could not be.
fn remove(path: PathBuf, recovery: &mut Recovery) {
    match fs::remove_file(&path) {
        Ok(()) => recovery.removed.push(path),
        Err(error) => recovery.failed.push((path, WavError::Io(error))),
    }
}
