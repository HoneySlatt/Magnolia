//! The recording folder's files: the takes' names, the files made ahead, the folder's lock, and the
//! take's file as the recorder writes and syncs it.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::RecordError;

/// The file in the recording folder whose exclusive lock a recorder, or a recovery, holds.
pub(crate) const LOCK_FILE: &str = ".magnolia-recording.lock";

/// The longest a sanitized track name may be, in characters.
const NAME_CHARACTERS: usize = 64;

/// How many numbers a new take's file tries past one that exists.
const NAME_TRIES: u32 = 1_000;

/// A take's name, as its file's name gives it: `<track number>-<track name>-take<n>.wav`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeName {
    /// The track's number.
    pub track: u32,
    /// The track's name, sanitized.
    pub name: String,
    /// The take's number on its track, from 1.
    pub take: u32,
}

impl TakeName {
    /// The file's name of this take.
    pub fn file_name(&self) -> String {
        format!("{}-{}-take{}.wav", self.track, self.name, self.take)
    }

    /// The take's name a file's name gives, if it is one: `<track number>-<name>-take<n>.wav`, both
    /// numbers in ASCII digits.
    pub fn parse(file_name: &str) -> Option<Self> {
        let stem = file_name.strip_suffix(".wav")?;
        let (rest, take) = stem.rsplit_once("-take")?;
        let (track, name) = rest.split_once('-')?;
        Some(Self {
            track: digits(track)?,
            name: name.to_owned(),
            take: digits(take)?,
        })
    }
}

/// A number written in ASCII digits alone, as a take's name holds it.
fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A track's name as a take's file holds it: letters and digits of any script, `-` and `_` kept,
/// every other character, a space included, turned into `_`; at most 64 characters; `track` for a
/// name left empty.
pub(crate) fn sanitized(name: &str) -> String {
    let kept: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .take(NAME_CHARACTERS)
        .collect();
    if kept.is_empty() {
        "track".to_owned()
    } else {
        kept
    }
}

/// The number of `track`'s next take in `folder`: one past the highest of its takes' files there,
/// or 1.
pub(crate) fn next_take_number(folder: &Path, track: u32, name: &str) -> io::Result<u32> {
    let mut highest = 0;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let Some(taken) = entry.file_name().to_str().and_then(TakeName::parse) else {
            continue;
        };
        if taken.track == track && taken.name == name {
            highest = highest.max(taken.take);
        }
    }
    Ok(highest.saturating_add(1))
}

/// A take's file, made empty ahead of its take.
#[derive(Debug)]
pub(crate) struct AheadFile {
    pub(crate) file: File,
    pub(crate) path: PathBuf,
    pub(crate) number: u32,
}

/// Makes the next take's file of a track, empty, numbered `next` or past it if that file exists;
/// `next` moves past the number taken. The caller syncs the folder.
pub(crate) fn make_take_file(
    folder: &Path,
    track: u32,
    name: &str,
    next: &mut u32,
) -> io::Result<AheadFile> {
    for _ in 0..NAME_TRIES {
        let number = *next;
        *next = next.saturating_add(1);
        let take = TakeName {
            track,
            name: name.to_owned(),
            take: number,
        };
        let path = folder.join(take.file_name());
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok(AheadFile { file, path, number }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "a thousand take files in a row exist already",
    ))
}

/// The recording folder's exclusive lock, held for as long as this lives. A recorder holds it for
/// its life, a recovery for its run, so that a recovery never cuts a take being written.
#[derive(Debug)]
pub(crate) struct FolderLock {
    _file: File,
}

impl FolderLock {
    /// Takes the lock of `folder`, without waiting.
    ///
    /// # Errors
    ///
    /// [`RecordError::FolderLocked`] while another recorder or recovery holds it;
    /// [`RecordError::Folder`] when the lock file cannot be opened or locked.
    pub(crate) fn take(folder: &Path) -> Result<Self, RecordError> {
        let folder_error = |error| RecordError::Folder {
            folder: folder.to_path_buf(),
            error,
        };
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(folder.join(LOCK_FILE))
            .map_err(folder_error)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(RecordError::FolderLocked(folder.to_path_buf())),
            Err(TryLockError::Error(error)) => Err(folder_error(error)),
        }
    }
}

/// Opens `folder` itself, so that its entries can be synced: a file made or removed there outlasts
/// a power cut only once the folder is synced.
pub(crate) fn open_folder(folder: &Path) -> Result<File, RecordError> {
    let folder_error = |error| RecordError::Folder {
        folder: folder.to_path_buf(),
        error,
    };
    let file = File::open(folder).map_err(folder_error)?;
    if !file.metadata().map_err(folder_error)?.is_dir() {
        return Err(folder_error(io::Error::new(
            io::ErrorKind::NotADirectory,
            "not a folder",
        )));
    }
    Ok(file)
}

/// A take's file, as the recorder writes it: straight to the file, no buffer beyond the WAV
/// writer's one piece. A test's hook makes its writes fail.
pub(crate) struct TakeFile {
    file: File,
    fail: Arc<AtomicBool>,
    #[cfg(test)]
    log: Option<OpLog>,
    #[cfg(test)]
    position: u64,
}

impl TakeFile {
    pub(crate) fn new(file: File, fail: Arc<AtomicBool>) -> Self {
        Self {
            file,
            fail,
            #[cfg(test)]
            log: None,
            #[cfg(test)]
            position: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_log(mut self, log: Option<OpLog>) -> Self {
        self.log = log;
        self
    }
}

impl Write for TakeFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(io::Error::other("a test's hook failed this write"));
        }
        let written = self.file.write(bytes)?;
        #[cfg(test)]
        if let Some(log) = &self.log {
            let bytes = bytes.get(..written).unwrap_or_default().to_vec();
            log_op(
                log,
                FileOp::Write {
                    at: self.position,
                    bytes,
                },
            );
            self.position = self.position.saturating_add(written as u64);
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for TakeFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = self.file.seek(to)?;
        #[cfg(test)]
        {
            self.position = position;
        }
        Ok(position)
    }
}

/// A second handle on a take's file, through which the sync threads sync it while the recorder
/// writes, and whether a sync of it is under way or failed.
pub(crate) struct SyncFile {
    file: File,
    /// Set while a sync is queued or under way, so that one is in flight at most.
    pub(crate) in_flight: AtomicBool,
    /// Set when a sync failed: what was written may not be on the disk.
    pub(crate) failed: AtomicBool,
    #[cfg(test)]
    log: Option<OpLog>,
}

impl SyncFile {
    /// A handle on the file of `file`.
    pub(crate) fn of(file: &File) -> io::Result<Self> {
        Ok(Self {
            file: file.try_clone()?,
            in_flight: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            #[cfg(test)]
            log: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_log(mut self, log: Option<OpLog>) -> Self {
        self.log = log;
        self
    }

    /// Makes every write made before it durable, the file's length included (`fdatasync`).
    pub(crate) fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(log) = &self.log {
            log_op(log, FileOp::Sync);
        }
        self.file.sync_data()
    }
}

/// What a test reads of a take's file: every write, where it landed, and every sync, in order. A
/// sync is logged as it starts, so every write logged before it is one it makes durable.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) enum FileOp {
    Write { at: u64, bytes: Vec<u8> },
    Sync,
}

#[cfg(test)]
#[expect(
    clippy::disallowed_types,
    reason = "a unit test's log of a take's file operations; the recorder's and the sync threads' only"
)]
pub(crate) type OpLog = Arc<std::sync::Mutex<Vec<FileOp>>>;

#[cfg(test)]
fn log_op(log: &OpLog, op: FileOp) {
    log.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(op);
}
