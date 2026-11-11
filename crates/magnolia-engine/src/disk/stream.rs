//! A track's stream: the ring of slots the disk reader fills and the player copies from, and the
//! atomic words through which the two say where they are.
//!
//! A slot's samples are `f32`s stored as the bits of `AtomicU32`s, and its state is one `AtomicU64`.
//! The reader empties the state, writes the samples, then publishes the state (Release); the
//! player reads the state (Acquire), copies the samples, then reads the state again, and takes as
//! missing a block whose state changed in between. Every access is atomic, so no copy is ever
//! undefined, only discarded.

// The player runs this module's code on the real-time thread: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::hint::black_box;
use std::ops::Range;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::BLOCK_FRAMES;
use crate::seqlock::SeqLock;

/// A block's length on the timeline.
pub(crate) const BLOCK: i64 = BLOCK_FRAMES as i64;

/// The block a timeline sample lies in.
pub(crate) fn block_of(sample: i64) -> i64 {
    sample.div_euclid(BLOCK)
}

/// A block's first timeline sample.
pub(crate) fn first_sample(block: i64) -> i64 {
    block.saturating_mul(BLOCK)
}

/// A slot's state word, from the lowest bit: whether the slot holds a block (`READY`), whether that
/// block is all zeros (`SILENT`), the block's number in 30 bits, two's complement, and the low 32
/// bits of the generation it was filled for. Zero is an empty slot.
const READY: u64 = 1;
const SILENT: u64 = 2;
const BLOCK_SHIFT: u32 = 2;
const BLOCK_MASK: u64 = (1 << 30) - 1;
const GENERATION_SHIFT: u32 = 32;
const LOW_32_BITS: u64 = 0xFFFF_FFFF;

/// An empty slot's state.
pub(crate) const EMPTY: u64 = 0;

/// The word of a slot that holds `block` of `generation`. Block numbers lie within ±2^29 (the
/// clips are checked), so 30 bits hold them; generations count modulo 2^32.
pub(crate) fn slot_word(generation: u64, block: i64, silent: bool) -> u64 {
    let generation = (generation & LOW_32_BITS).wrapping_shl(GENERATION_SHIFT);
    let block = (block as u64 & BLOCK_MASK).wrapping_shl(BLOCK_SHIFT);
    let silent = if silent { SILENT } else { 0 };
    generation | block | silent | READY
}

/// What a slot's word says it holds, when it holds a block: the low 32 bits of its generation,
/// and its block.
pub(crate) fn slot_holds(word: u64) -> Option<(u64, i64)> {
    if word & READY == 0 {
        return None;
    }
    let generation = word.wrapping_shr(GENERATION_SHIFT);
    // The block's 30 bits moved to the top, then shifted back with their sign.
    let block = (word.wrapping_shl(GENERATION_SHIFT) as i64)
        .wrapping_shr(GENERATION_SHIFT.wrapping_add(BLOCK_SHIFT));
    Some((generation, block))
}

/// The low 32 bits of a generation, as slot and ready words hold it.
pub(crate) fn generation_tag(generation: u64) -> u64 {
    generation & LOW_32_BITS
}

/// The reader's word for a stream: the generation it fills, and the first block, from those it
/// filled in a row, that it has not filled. The player's supply is what lies before that block.
fn ready_word(generation: u64, until_block: i64) -> u64 {
    let generation = generation_tag(generation).wrapping_shl(GENERATION_SHIFT);
    // Block numbers fit in 30 bits, so in an `i32`.
    let until_block = u64::from(until_block as i32 as u32);
    generation | until_block
}

/// What the player tells the reader after each sub-block: the generation of the play it plays,
/// where it reads next, and the play's stop once known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) generation: u64,
    /// The next timeline sample at the player's input.
    pub(crate) position: i64,
    pub(crate) rolling: bool,
    /// The first sample past the play's audible region, once the play has been asked to stop.
    pub(crate) stop: Option<i64>,
}

impl Cursor {
    /// Before any play: generation 0, stopped at sample 0.
    const STOPPED: Cursor = Cursor {
        generation: 0,
        position: 0,
        rolling: false,
        stop: None,
    };

    /// Its words: the generation, the position, the flags (rolling, then whether the stop is
    /// known), and the stop.
    fn words(self) -> [u64; 4] {
        let rolling = u64::from(self.rolling);
        let stop_known = if self.stop.is_some() { 2 } else { 0 };
        [
            self.generation,
            self.position as u64,
            rolling | stop_known,
            self.stop.unwrap_or(0) as u64,
        ]
    }

    fn from_words([generation, position, flags, stop]: [u64; 4]) -> Self {
        Self {
            generation,
            position: position as i64,
            rolling: flags & 1 != 0,
            stop: (flags & 2 != 0).then_some(stop as i64),
        }
    }
}

/// One slot: a block of every channel, channel after channel, and its state.
pub(crate) struct Slot {
    pub(crate) state: AtomicU64,
    samples: Box<[AtomicU32]>,
}

impl Slot {
    /// One channel's samples in the slot, as `f32` bits.
    pub(crate) fn channel(&self, channel: usize) -> &[AtomicU32] {
        channel
            .checked_mul(BLOCK_FRAMES)
            .and_then(|start| self.samples.get(start..start.checked_add(BLOCK_FRAMES)?))
            .unwrap_or(&[])
    }
}

/// One audio track's stream, shared by the reader and the track's player.
pub(crate) struct Stream {
    pub(crate) channels: usize,
    /// The timeline samples its clips cover, from the first's start to the last's end; outside
    /// them it is silent, and nothing is read. Empty for a track without clips.
    pub(crate) extent: Range<i64>,
    slots: Box<[Slot]>,
    /// Written by the reader alone: see [`ready_word`].
    ready: AtomicU64,
    /// Written by the player alone.
    cursor: SeqLock<4>,
    /// The fewest frames the player found ready ahead of it since its play started; `u64::MAX`
    /// before it found any. Written by the player alone.
    low_water: AtomicU64,
}

impl Stream {
    /// A stream of `channels` channels over `extent`, with `slot_count` slots, all empty. Every
    /// page of it is written here through `black_box`, so that neither the player nor the reader
    /// faults on it (`engine/memory.rs`). Built off the real-time thread.
    pub(crate) fn new(channels: usize, extent: Range<i64>, slot_count: usize) -> Self {
        let samples_per_slot = channels.saturating_mul(BLOCK_FRAMES);
        let slots: Box<[Slot]> = (0..slot_count)
            .map(|_| Slot {
                state: AtomicU64::new(black_box(EMPTY)),
                samples: (0..samples_per_slot)
                    .map(|_| AtomicU32::new(black_box(0)))
                    .collect(),
            })
            .collect();
        black_box(&slots);
        Self {
            channels,
            extent,
            slots,
            ready: AtomicU64::new(black_box(ready_word(0, 0))),
            cursor: SeqLock::new(Cursor::STOPPED.words()),
            low_water: AtomicU64::new(black_box(u64::MAX)),
        }
    }

    /// The slot that holds `block`, if any: slot `block mod N`.
    pub(crate) fn slot(&self, block: i64) -> Option<&Slot> {
        let count = i64::try_from(self.slots.len()).ok()?;
        let index = block.checked_rem_euclid(count)?;
        self.slots.get(usize::try_from(index).ok()?)
    }

    /// How many slots the ring holds.
    pub(crate) fn slot_count(&self) -> i64 {
        i64::try_from(self.slots.len()).unwrap_or(i64::MAX)
    }

    /// Writes a block's samples, interleaved as `frames` holds them, into its slot, and publishes
    /// it for `generation`. A silent block's samples are not written: the player never reads them.
    /// A slot that holds that block of that generation already holds those samples, and is left
    /// as it is, so that a slot's word changes at every write: a copy that straddles a write always
    /// finds the word changed. The reader alone calls it.
    pub(crate) fn write_block(&self, generation: u64, block: i64, frames: &[f32]) {
        let Some(slot) = self.slot(block) else {
            return;
        };
        let silent = frames.iter().all(|&sample| sample == 0.0);
        let word = slot_word(generation, block, silent);
        if slot.state.load(Ordering::Relaxed) == word {
            return;
        }
        slot.state.store(EMPTY, Ordering::Relaxed);
        // The empty state is ordered before every sample written below: a player that copies one
        // of them then sees the state changed when it reads it again.
        std::sync::atomic::fence(Ordering::Release);
        if !silent {
            for channel in 0..self.channels {
                let samples = frames.iter().skip(channel).step_by(self.channels.max(1));
                for (target, &sample) in slot.channel(channel).iter().zip(samples) {
                    target.store(sample.to_bits(), Ordering::Relaxed);
                }
            }
        }
        slot.state.store(word, Ordering::Release);
    }

    /// Publishes that the blocks before `until_block`, from those filled in a row, hold
    /// `generation`. The reader alone calls it.
    pub(crate) fn publish_ready(&self, generation: u64, until_block: i64) {
        self.ready
            .store(ready_word(generation, until_block), Ordering::Release);
    }

    /// The reader's word: the low 32 bits of the generation it fills, and the first sample past
    /// what it filled in a row.
    pub(crate) fn ready_until(&self) -> (u64, i64) {
        let word = self.ready.load(Ordering::Acquire);
        let until_block = i64::from(word as u32 as i32);
        (
            word.wrapping_shr(GENERATION_SHIFT),
            first_sample(until_block),
        )
    }

    /// Publishes the player's cursor. The player alone calls it; it never waits.
    pub(crate) fn publish_cursor(&self, cursor: Cursor) {
        self.cursor.write(cursor.words());
    }

    /// The player's last cursor. Spins while the player writes it: never on the real-time thread.
    pub(crate) fn cursor(&self) -> Cursor {
        Cursor::from_words(self.cursor.read())
    }

    /// Records how many frames the player found ready ahead of it, keeping the fewest.
    pub(crate) fn record_supply(&self, frames: u64) {
        if frames < self.low_water.load(Ordering::Relaxed) {
            self.low_water.store(frames, Ordering::Relaxed);
        }
    }

    /// Forgets the low-water mark, at a new play.
    pub(crate) fn reset_low_water(&self) {
        self.low_water.store(u64::MAX, Ordering::Relaxed);
    }

    /// The low-water mark since the player's play started, if it has found any supply.
    pub(crate) fn low_water(&self) -> Option<u64> {
        let frames = self.low_water.load(Ordering::Relaxed);
        (frames != u64::MAX).then_some(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_word_holds_its_generation_and_block_with_their_signs() {
        for block in [0, 1, -1, (1 << 29) - 1, -(1 << 29), 123_456, -98_765] {
            for generation in [1, 2, u64::from(u32::MAX), u64::from(u32::MAX) + 5] {
                for silent in [false, true] {
                    let word = slot_word(generation, block, silent);
                    assert_eq!(
                        slot_holds(word),
                        Some((generation_tag(generation), block)),
                        "generation {generation}, block {block}"
                    );
                    assert_eq!(word & SILENT != 0, silent);
                }
            }
        }
        assert_eq!(slot_holds(EMPTY), None);
    }

    #[test]
    fn the_ready_word_holds_negative_blocks() {
        let stream = Stream::new(1, 0..0, 2);
        for block in [0, 5, -7, (1 << 29) - 1, -(1 << 29)] {
            stream.publish_ready(9, block);
            assert_eq!(stream.ready_until(), (9, block * BLOCK));
        }
    }

    #[test]
    fn a_cursor_survives_its_words() {
        for cursor in [
            Cursor::STOPPED,
            Cursor {
                generation: 7,
                position: -1_234,
                rolling: true,
                stop: Some(-1),
            },
            Cursor {
                generation: u64::MAX,
                position: i64::MIN,
                rolling: true,
                stop: None,
            },
        ] {
            assert_eq!(Cursor::from_words(cursor.words()), cursor);
        }
    }
}
