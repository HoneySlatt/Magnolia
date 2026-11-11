//! The collector: the thread that frees what the processor lets go, so that the real-time thread
//! never frees memory itself.
//!
//! The processor pushes what it replaces (an old graph, an old pool, a used message, the nodes a
//! swap dropped) into a queue, then wakes the collector with [`wake_collector`], the one system
//! call it makes for it
//! ([ADR-0023](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0023-the-real-time-allowlist.md)).
//! The collector sleeps until woken, frees everything queued, and sleeps again. At rest nothing is
//! swapped, so nothing wakes it.

// The processor runs this module's code in every period: nothing here may panic (ADR-0022).
#![warn(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::any::Any;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use super::compiled::{CompiledGraph, SwapMessage};
use super::pool::BufferPool;
use crate::node::Node;

/// The most items one applied swap returns at once: the old graph, the used message and the old
/// pool. The processor applies a swap only when the queue has room for them all.
pub(crate) const RELEASED_PER_SWAP: usize = 3;

/// Items the processor keeps when the queue is full, until it has room. Between two swaps it
/// releases at most one item beyond those it made room for: the dropped nodes, at the steady
/// phase. A panic's payload may add one.
const OVERFLOW_SLOTS: usize = 4;

/// Something the processor lets go, for the collector to drop.
pub(crate) enum Garbage {
    Graph(Box<CompiledGraph>),
    Pool(BufferPool),
    Message(Box<SwapMessage>),
    Nodes(Vec<Box<dyn Node>>),
    PanicPayload(Box<dyn Any + Send>),
}

/// Wakes the collector thread: a futex wake when it sleeps, nothing when it is awake already
/// (`Thread::unpark`). It is the one call on the real-time allowlist that the collector needs
/// ([ADR-0023](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0023-the-real-time-allowlist.md)):
/// `rtsan-suppressions.txt` names this function, so RealtimeSanitizer lets its system call pass
/// and still reports any other. It is public so that the sanitizer's test can call it beside a
/// lock's wait, and never inlined, so that it shows on the call stack.
#[inline(never)]
pub fn wake_collector(collector: &Thread) {
    collector.unpark();
}

/// The processor's end of the collector's queue.
pub(crate) struct GarbageOut {
    queue: Producer<Garbage>,
    overflow: [Option<Garbage>; OVERFLOW_SLOTS],
    collector: Thread,
    closed: Arc<AtomicBool>,
    /// Whether something was pushed since the collector was last woken.
    pushed: bool,
}

impl GarbageOut {
    /// The processor's end of `queue`, whose consumer `collector` drains until `closed`.
    pub(super) fn new(
        queue: Producer<Garbage>,
        collector: Thread,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            queue,
            overflow: Default::default(),
            collector,
            closed,
            pushed: false,
        }
    }

    /// Hands an item to the collector: into the queue, or into an overflow slot while it is full.
    pub(crate) fn release(&mut self, item: Garbage) {
        match self.queue.push(item) {
            Ok(()) => self.pushed = true,
            Err(PushError::Full(item)) => {
                if let Some(slot) = self.overflow.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(item);
                } else {
                    // Unreachable: a swap is applied only with the overflow empty and room in the
                    // queue for what it returns (`can_take_swap`), which leaves at most two items
                    // for these slots. Leaking is still better than freeing on this thread.
                    std::mem::forget(item);
                }
            }
        }
    }

    /// Moves what waits in the overflow slots into the queue, as far as it has room.
    pub(crate) fn flush_overflow(&mut self) {
        for slot in &mut self.overflow {
            let Some(item) = slot.take() else { continue };
            match self.queue.push(item) {
                Ok(()) => self.pushed = true,
                Err(PushError::Full(item)) => {
                    *slot = Some(item);
                    return;
                }
            }
        }
    }

    /// Whether the processor can apply a swap now: nothing waits in the overflow, and the queue
    /// has room for everything the swap returns at once.
    pub(crate) fn can_take_swap(&self) -> bool {
        self.overflow.iter().all(Option::is_none) && self.queue.slots() >= RELEASED_PER_SWAP
    }

    /// Wakes the collector if something was pushed since it was last woken.
    pub(crate) fn wake_if_pushed(&mut self) {
        if self.pushed {
            wake_collector(&self.collector);
            self.pushed = false;
        }
    }
}

impl Drop for GarbageOut {
    /// Tells the collector to finish: it drops what is queued, then exits. Runs where the
    /// processor is dropped, off the real-time thread.
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.collector.unpark();
    }
}

/// Starts the collector thread, with a queue of `capacity` items. Every slot of the queue is
/// written here first, since the processor, which writes them next, must not fault on them.
pub(crate) fn start_collector(capacity: usize) -> io::Result<GarbageOut> {
    let (mut producer, mut consumer) = RingBuffer::new(capacity);
    for _ in 0..capacity {
        // An empty vector allocates nothing, so these items cost nothing to drop.
        if producer.push(Garbage::Nodes(Vec::new())).is_err() {
            break;
        }
    }
    while consumer.pop().is_ok() {}
    let closed = Arc::new(AtomicBool::new(false));
    let collector_closed = Arc::clone(&closed);
    let handle = thread::Builder::new()
        .name("magnolia-collector".into())
        .spawn(move || collect(consumer, &collector_closed))?;
    Ok(GarbageOut::new(producer, handle.thread().clone(), closed))
}

/// The collector thread's loop: drops what is queued, sleeps until woken, and exits once the
/// processor is gone.
#[expect(
    clippy::disallowed_methods,
    reason = "the collector thread sleeps until the processor wakes it; it never runs on the real-time path"
)]
fn collect(mut queue: Consumer<Garbage>, closed: &AtomicBool) {
    loop {
        drop_queued(&mut queue);
        // Acquire: everything the processor pushed before closing is visible to the last drain.
        if closed.load(Ordering::Acquire) {
            drop_queued(&mut queue);
            return;
        }
        thread::park();
    }
}

/// Drops every queued item. An item whose drop panics loses only itself: the collector carries
/// on.
fn drop_queued(queue: &mut Consumer<Garbage>) {
    while let Ok(item) = queue.pop() {
        // The payload of a panic is dropped here too, apart, in case its own drop panics.
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop_item(item))) {
            let _ = catch_unwind(AssertUnwindSafe(|| drop(payload)));
        }
    }
}

fn drop_item(item: Garbage) {
    match item {
        Garbage::Graph(graph) => drop(graph),
        Garbage::Pool(pool) => drop(pool),
        Garbage::Message(message) => drop(message),
        Garbage::Nodes(nodes) => drop(nodes),
        Garbage::PanicPayload(payload) => drop(payload),
    }
}
