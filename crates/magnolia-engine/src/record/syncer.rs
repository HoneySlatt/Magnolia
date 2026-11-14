//! The recorder's sync threads: they make what the recorder wrote durable, so that a sync never
//! stalls the thread that drains the rings
//! ([ADR-0030](https://github.com/HoneySlatt/Magnolia/blob/main/docs/adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).
//!
//! The recorder's thread queues jobs; each of [`RecordConfig::sync_threads`](super::RecordConfig)
//! threads, `magnolia-sync`, takes the next one, so that the files of several tracks sync in
//! parallel. A job is a sync of a take's data, which the recorder asks after each second of its
//! audio, one in flight per take; a take's end, which syncs its data, writes its sizes and syncs
//! again; or a sync of the folder, after files were made or removed there, once per batch.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::files::{SyncFile, TakeFile};
use super::recorder::{RecorderStats, TakeList, TakeSummary};
use crate::wav::{WavError, WavWriter};

/// A take whose end the sync threads complete.
pub(crate) struct Ending {
    /// Its writer, unless no frame reached its file; `None` too after its header failed.
    pub(crate) writer: Option<WavWriter<TakeFile>>,
    /// The handle its data is synced through, if one was opened.
    pub(crate) sync: Option<Arc<SyncFile>>,
    /// What the recorder reports of it, completed here.
    pub(crate) summary: TakeSummary,
}

/// What a sync thread does.
pub(crate) enum Job {
    /// Syncs a take's data written so far.
    Sync(Arc<SyncFile>),
    /// Ends a take: syncs its data, writes its sizes, syncs them; for a take whose writes failed,
    /// syncs what it holds and leaves its sizes unknown, as a crash would.
    End(Box<Ending>),
    /// Syncs the folder's entries.
    SyncFolder(Arc<File>),
}

/// The jobs waiting, and what the sync threads do.
struct Jobs {
    queue: VecDeque<Job>,
    /// Jobs taken and not yet done.
    running: usize,
    /// Set when the recorder ends: the threads finish the queue, then exit.
    closed: bool,
}

/// The queue of jobs the recorder's thread fills and the sync threads empty.
#[expect(
    clippy::disallowed_types,
    reason = "the recorder's thread and the sync threads share their queue under a lock; neither runs on the real-time path"
)]
pub(crate) struct SyncQueue {
    jobs: std::sync::Mutex<Jobs>,
    ready: std::sync::Condvar,
    /// Set while a folder's sync is queued, so that a batch of files made or removed syncs it once.
    folder_queued: AtomicBool,
}

impl SyncQueue {
    #[expect(
        clippy::disallowed_types,
        reason = "the recorder's thread and the sync threads share their queue under a lock; neither runs on the real-time path"
    )]
    pub(crate) fn new() -> Self {
        Self {
            jobs: std::sync::Mutex::new(Jobs {
                queue: VecDeque::new(),
                running: 0,
                closed: false,
            }),
            ready: std::sync::Condvar::new(),
            folder_queued: AtomicBool::new(false),
        }
    }

    /// Queues a job for the next free sync thread.
    pub(crate) fn push(&self, job: Job) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.queue.push_back(job);
        drop(jobs);
        self.ready.notify_one();
    }

    /// Queues a sync of `folder`, unless one is queued and not yet started.
    pub(crate) fn sync_folder(&self, folder: &Arc<File>) {
        if !self.folder_queued.swap(true, Ordering::AcqRel) {
            self.push(Job::SyncFolder(Arc::clone(folder)));
        }
    }

    /// Whether no job waits and none is under way.
    pub(crate) fn is_idle(&self) -> bool {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.queue.is_empty() && jobs.running == 0
    }

    /// Tells the threads to exit once the queue is empty.
    pub(crate) fn close(&self) {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.ready.notify_all();
    }

    /// The next job, once one waits; `None` once the queue is closed and empty.
    fn take(&self) -> Option<Job> {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(job) = jobs.queue.pop_front() {
                jobs.running = jobs.running.saturating_add(1);
                return Some(job);
            }
            if jobs.closed {
                return None;
            }
            jobs = self
                .ready
                .wait(jobs)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn done(&self) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.running = jobs.running.saturating_sub(1);
    }
}

/// What the sync threads report into.
pub(crate) struct SyncContext {
    pub(crate) queue: Arc<SyncQueue>,
    pub(crate) stats: Arc<RecorderStats>,
    /// Where an ended take's summary goes.
    pub(crate) takes: Arc<TakeList>,
}

/// Starts `count` sync threads, at least one.
pub(crate) fn start(context: &Arc<SyncContext>, count: usize) -> io::Result<Vec<JoinHandle<()>>> {
    let mut threads = Vec::with_capacity(count.max(1));
    for _ in 0..count.max(1) {
        let thread_context = Arc::clone(context);
        let thread = thread::Builder::new()
            .name("magnolia-sync".into())
            .spawn(move || serve(&thread_context));
        match thread {
            Ok(thread) => threads.push(thread),
            Err(error) => {
                context.queue.close();
                join(threads);
                return Err(error);
            }
        }
    }
    Ok(threads)
}

/// Waits for the sync threads to end, once their queue is closed.
#[expect(
    clippy::disallowed_methods,
    reason = "the recorder's thread waits for its sync threads as it ends; never on the real-time path"
)]
pub(crate) fn join(threads: Vec<JoinHandle<()>>) {
    for thread in threads {
        // A job's panic was caught inside it; a thread that still panicked has nothing to report.
        let _ = thread.join();
    }
}

/// A sync thread: takes jobs until the queue closes and empties. A job that panics loses only
/// itself, counted as a write error.
fn serve(context: &SyncContext) {
    while let Some(job) = context.queue.take() {
        if catch_unwind(AssertUnwindSafe(|| run(job, context))).is_err() {
            context.stats.count_write_error();
        }
        context.queue.done();
    }
}

fn run(job: Job, context: &SyncContext) {
    match job {
        Job::Sync(file) => {
            if timed_sync(&context.stats, || file.sync()).is_err() {
                file.failed.store(true, Ordering::Release);
                context.stats.count_write_error();
            }
            file.in_flight.store(false, Ordering::Release);
        }
        Job::End(ending) => end(*ending, context),
        Job::SyncFolder(folder) => {
            // Cleared first: a file made after this point asks for a sync of its own.
            context.queue.folder_queued.store(false, Ordering::Release);
            if timed_sync(&context.stats, || folder.sync_all()).is_err() {
                context.stats.count_write_error();
            }
        }
    }
}

/// Completes a take's end, then reports it. Each failure is counted once, where it happens: the
/// recorder's thread counted the take's failed write, this thread its failed syncs.
fn end(ending: Ending, context: &SyncContext) {
    let Ending {
        writer,
        sync,
        mut summary,
    } = ending;
    if summary.error.is_none()
        && sync
            .as_ref()
            .is_some_and(|sync| sync.failed.load(Ordering::Acquire))
    {
        summary.error = Some(Arc::new(WavError::Io(io::Error::other(
            "a sync of the take's data failed while it recorded",
        ))));
    }
    let failed = summary.error.is_some();
    if let Err(error) = complete(writer, sync.as_deref(), failed, &context.stats) {
        context.stats.count_write_error();
        summary.error.get_or_insert(Arc::new(error));
    }
    if summary.error.is_none() && summary.frames > 0 {
        context.stats.count_take_finished();
    }
    context.takes.push(summary);
}

/// Syncs a take's data, then, unless the take failed, writes its sizes and syncs them: the sizes
/// never claim data that did not reach the disk. A take that failed keeps its sizes unknown, as a
/// crash leaves them.
fn complete(
    writer: Option<WavWriter<TakeFile>>,
    sync: Option<&SyncFile>,
    failed: bool,
    stats: &RecorderStats,
) -> Result<(), WavError> {
    if let Some(sync) = sync {
        timed_sync(stats, || sync.sync())?;
    }
    let Some(writer) = writer.filter(|_| !failed) else {
        return Ok(());
    };
    writer.finish()?;
    if let Some(sync) = sync {
        timed_sync(stats, || sync.sync())?;
    }
    Ok(())
}

/// Runs a sync, timing it into the stats.
fn timed_sync(stats: &RecorderStats, sync: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    let start = Instant::now();
    let result = sync();
    stats.record_sync(start.elapsed());
    result
}
