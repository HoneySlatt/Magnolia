//! What the command line's tests share: the `magnolia` binary run with arguments, and a folder of
//! their own.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// What a run of `magnolia` did.
pub struct Ran {
    /// Its exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `magnolia` with `arguments` and waits for it. A panic, which would say `panicked` on
/// standard error, fails the test whatever the exit code.
pub fn magnolia<I, S>(arguments: I) -> Ran
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(env!("CARGO_BIN_EXE_magnolia"))
        .args(arguments)
        .output()
        .expect("the magnolia binary runs");
    let ran = Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert!(
        !ran.stderr.contains("panicked"),
        "magnolia panicked: {}",
        ran.stderr
    );
    ran
}

/// A folder of its own under the system's temporary folder, removed with everything in it when
/// dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "magnolia-cli-test-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
