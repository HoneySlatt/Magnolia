//! What the command line's tests share: the `magnolia` binary run with arguments, a folder of
//! their own, and the turn of the tests that start a JACK server, from `tests/common/mod.rs`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub use crate::common::one_at_a_time;
use crate::common::step;

/// The path of the `magnolia` binary the tests run.
pub fn magnolia_binary() -> &'static str {
    env!("CARGO_BIN_EXE_magnolia")
}

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
    let arguments: Vec<OsString> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    let line: Vec<_> = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect();
    step(format!("running magnolia {}", line.join(" ")));
    let output = Command::new(magnolia_binary())
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
