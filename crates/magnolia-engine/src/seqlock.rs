//! A sequence lock over a few 64-bit words: one thread writes them together without ever waiting,
//! and any other reads them whole, trying again when it meets a write.
//!
//! The real-time thread may write one, since writing never waits; it never reads one another
//! thread writes, since a reader that meets a write spins. Every access is atomic, so a read that
//! straddles a write is retried, never undefined.

// The processor writes these in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::hint::{self, black_box};
use std::sync::atomic::{self, AtomicU64, Ordering};

/// `N` words written together by one thread at a time, read whole by any.
#[derive(Debug)]
pub(crate) struct SeqLock<const N: usize> {
    /// Odd while the writer writes.
    version: AtomicU64,
    words: [AtomicU64; N],
}

impl<const N: usize> SeqLock<N> {
    /// The words given, every one of them written through `black_box`, so that the real-time
    /// thread never faults on their page (`engine/memory.rs`).
    pub(crate) fn new(words: [u64; N]) -> Self {
        Self {
            version: AtomicU64::new(black_box(0)),
            words: words.map(|word| AtomicU64::new(black_box(word))),
        }
    }

    /// Writes every word. Never waits. Only one thread may write a given lock, as each user of it
    /// says.
    pub(crate) fn write(&self, words: [u64; N]) {
        let version = self.version.load(Ordering::Relaxed);
        self.version
            .store(version.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd version before the words' writes, for a reader that sees a new word.
        atomic::fence(Ordering::Release);
        for (word, value) in self.words.iter().zip(words) {
            word.store(value, Ordering::Relaxed);
        }
        self.version
            .store(version.wrapping_add(2), Ordering::Release);
    }

    /// Reads every word, as one write left them. Spins while a write is under way: never call it
    /// on the real-time thread.
    pub(crate) fn read(&self) -> [u64; N] {
        loop {
            let before = self.version.load(Ordering::Acquire);
            let words = self
                .words
                .each_ref()
                .map(|word| word.load(Ordering::Relaxed));
            // Orders the reads of the words before the second read of the version.
            atomic::fence(Ordering::Acquire);
            let after = self.version.load(Ordering::Relaxed);
            if before == after && before.is_multiple_of(2) {
                return words;
            }
            hint::spin_loop();
        }
    }
}

impl<const N: usize> Default for SeqLock<N> {
    /// Every word at zero.
    fn default() -> Self {
        Self::new([0; N])
    }
}
