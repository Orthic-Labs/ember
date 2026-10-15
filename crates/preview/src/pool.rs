//! A prioritized job pool with per-slot de-duplication.
//!
//! Each job targets a *slot* (a grid cell, the loupe…). Submitting for a slot replaces a job for
//! that slot that hasn't started yet, so scrolling or dragging never builds a backlog. Workers
//! take the highest priority first (newest first among equals). Frontends re-prioritize or drop
//! queued jobs when the view changes (e.g. thumbnails scrolled out of view).
//!
//! Native: a fixed set of worker threads, started on first use. wasm32 (no threads): the host
//! calls [`JobPool::run_inline`] once per frame.
//!
//! [`JobPool::shutdown`] and dropping the pool discard queued jobs and wait, bounded, for running
//! ones (issue #620: GPU teardown separately gates driver entry when a job exceeds that deadline).

use std::any::Any;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub type Job<R> = Box<dyn FnOnce() -> R + Send>;

struct Entry<S, R> {
    slot: S,
    key: u64,
    priority: u32,
    seq: u64,
    job: Job<R>,
}

struct Shared<S, R> {
    queue: Mutex<Vec<Entry<S, R>>>,
    /// Slots with a job running: their next job waits until it ends, so a slot never has two at
    /// once (a slider drag submits faster than renders finish; overlapping renders of one view
    /// were mostly thrown away, and took the GPU from the one that is shown). Locked inside
    /// `queue`.
    busy: Mutex<Vec<S>>,
    cv: Condvar,
    shutdown: AtomicBool,
    /// The worker threads that have not ended yet, and the signal that one did.
    #[cfg(not(target_arch = "wasm32"))]
    live: Mutex<Vec<std::thread::ThreadId>>,
    #[cfg(not(target_arch = "wasm32"))]
    ended: Condvar,
}

/// How long dropping a pool waits for the jobs still running (see [`JobPool::shutdown`]).
const DROP_WAIT: Duration = Duration::from_secs(2);

/// A finished job.
pub struct Done<S, R> {
    pub slot: S,
    /// The key the job was submitted with (to drop stale results).
    pub key: u64,
    pub result: R,
    /// Run time in milliseconds (0 on wasm).
    pub ms: f64,
}

/// A worker could not produce a [`Done`] result. The owner must surface this to any matching
/// request instead of leaving it pending forever.
pub struct WorkerFailure<S> {
    pub slot: S,
    pub key: u64,
    /// Monotonic submission identity; slot/key can be reused by a newer request.
    pub submission: u64,
    pub error: String,
}

pub struct JobPool<S, R> {
    shared: Arc<Shared<S, R>>,
    tx: Sender<Done<S, R>>,
    rx: Receiver<Done<S, R>>,
    failed_tx: Sender<WorkerFailure<S>>,
    failed_rx: Receiver<WorkerFailure<S>>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    threads: usize,
    started: bool,
    seq: u64,
    #[cfg(not(target_arch = "wasm32"))]
    workers: Vec<std::thread::JoinHandle<()>>,
    /// [`JobPool::shutdown`] ran; submissions are rejected while worker handles remain retryable.
    stopped: bool,
}

impl<S: Copy + Eq + Hash + Send + 'static, R: Send + 'static> JobPool<S, R> {
    /// A pool with `threads` workers (none on wasm; `0` = jobs only run via [`JobPool::run_inline`]).
    pub fn new(threads: usize) -> Self {
        let (tx, rx) = channel();
        let (failed_tx, failed_rx) = channel();
        JobPool {
            shared: Arc::new(Shared {
                queue: Mutex::new(Vec::new()),
                busy: Mutex::new(Vec::new()),
                cv: Condvar::new(),
                shutdown: AtomicBool::new(false),
                #[cfg(not(target_arch = "wasm32"))]
                live: Mutex::new(Vec::new()),
                #[cfg(not(target_arch = "wasm32"))]
                ended: Condvar::new(),
            }),
            tx,
            rx,
            failed_tx,
            failed_rx,
            threads,
            started: false,
            seq: 0,
            #[cfg(not(target_arch = "wasm32"))]
            workers: Vec::new(),
            stopped: false,
        }
    }

    /// Workers for this machine: cores − 1, between 2 and 8.
    pub fn default_threads() -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::available_parallelism().map(|n| n.get().saturating_sub(1)).unwrap_or(4).clamp(2, 8)
        }
        #[cfg(target_arch = "wasm32")]
        {
            1
        }
    }

    fn start(&mut self) -> Result<(), String> {
        if self.started {
            #[cfg(not(target_arch = "wasm32"))]
            if self.threads > 0 && self.workers.is_empty() {
                return Err("preview worker pool has no available workers".to_owned());
            }
            return Ok(());
        }
        self.started = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // held while spawning: a worker that ends at once still finds its id listed
            let mut live = self.shared.live.lock().unwrap_or_else(|e| e.into_inner());
            for i in 0..self.threads {
                let shared = self.shared.clone();
                let tx = self.tx.clone();
                let failed_tx = self.failed_tx.clone();
                let spawned = std::thread::Builder::new().name(format!("lc-job-{i}")).spawn(move || {
                    let _ended = Ended(&shared);
                    while let Some(e) = take(&shared, true) {
                        let t0 = std::time::Instant::now();
                        // the slot is free again when the job ends, also if it panics
                        let _free = Free(&shared, e.slot);
                        let slot = e.slot;
                        let key = e.key;
                        let submission = e.seq;
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(e.job)) {
                            Ok(result) => {
                                let done = Done { slot, key, result, ms: t0.elapsed().as_secs_f64() * 1000.0 };
                                if tx.send(done).is_err() {
                                    break;
                                }
                            }
                            Err(payload) => {
                                let failure = WorkerFailure { slot, key, submission, error: panic_message(payload) };
                                if failed_tx.send(failure).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
                match spawned {
                    Ok(h) => {
                        live.push(h.thread().id());
                        self.workers.push(h);
                    }
                    Err(e) => log::error!("job pool: worker {i} could not start: {e}"),
                }
            }
            if self.threads > 0 && self.workers.is_empty() {
                self.stopped = true;
                close(&self.shared);
                return Err("preview worker pool has no available workers".to_owned());
            }
        }
        Ok(())
    }

    /// Stop the pool: queued jobs are dropped, no new one starts, and the call waits up to
    /// `timeout` for the jobs that are running. Returns whether every worker has ended; one that
    /// hasn't is left to finish on its own. For owners that are about to end the process, where
    /// dropping the pool may come too late or never (`std::process::exit`).
    pub fn shutdown(&mut self, timeout: Duration) -> bool {
        self.stopped = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            stop(&self.shared, &mut self.workers, timeout)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            close(&self.shared);
            true
        }
    }

    /// Worker threads that have not ended (0 before the first job, and on wasm).
    pub fn live_workers(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.shared.live.lock().map(|l| l.len()).unwrap_or(0)
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    /// Queue `job` for `slot`, replacing a queued (not yet running) job for the same slot.
    pub fn submit(&mut self, slot: S, key: u64, priority: u32, job: Job<R>) -> Result<u64, String> {
        if self.stopped {
            return Err("preview worker pool is stopped".to_owned());
        }
        self.start()?;
        self.seq = self.seq.wrapping_add(1).max(1);
        let (lock, cv) = (&self.shared.queue, &self.shared.cv);
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.retain(|e| e.slot != slot);
        let submission = self.seq;
        q.push(Entry { slot, key, priority, seq: submission, job });
        cv.notify_one();
        Ok(submission)
    }

    /// Change queued jobs' priorities: `f(slot, priority)` returns the new priority, or `None` to
    /// drop the job. Returns the dropped slots.
    pub fn reprioritize(&self, mut f: impl FnMut(&S, u32) -> Option<u32>) -> Vec<S> {
        let mut dropped = Vec::new();
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.retain_mut(|e| match f(&e.slot, e.priority) {
            Some(p) => {
                e.priority = p;
                true
            }
            None => {
                dropped.push(e.slot);
                false
            }
        });
        dropped
    }

    /// Queued (not started) jobs.
    pub fn queued(&self) -> usize {
        self.shared.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Is a job for `slot` queued?
    pub fn is_queued(&self, slot: S) -> bool {
        self.shared.queue.lock().map(|q| q.iter().any(|e| e.slot == slot)).unwrap_or(false)
    }

    /// A finished job, if any.
    pub fn try_recv(&self) -> Option<Done<S, R>> {
        self.rx.try_recv().ok()
    }

    /// A worker panic, if one occurred since last poll.
    pub fn try_failure(&self) -> Option<WorkerFailure<S>> {
        self.failed_rx.try_recv().ok()
    }

    /// Run up to `n` jobs on the calling thread (wasm, tests). Returns how many ran.
    pub fn run_inline(&mut self, n: usize) -> usize {
        let mut ran = 0;
        while ran < n {
            let Some(e) = take(&self.shared, false) else { break };
            let _free = Free(&self.shared, e.slot);
            let slot = e.slot;
            let key = e.key;
            let submission = e.seq;
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(e.job)) {
                Ok(result) => {
                    let _ = self.tx.send(Done { slot, key, result, ms: 0.0 });
                }
                Err(payload) => {
                    let _ = self.failed_tx.send(WorkerFailure { slot, key, submission, error: panic_message(payload) });
                }
            }
            ran += 1;
        }
        ran
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "preview worker panicked".to_owned()
}

/// Pop the best job (highest priority, newest first). Blocks when `wait` until one is queued or
/// the pool shuts down.
fn take<S: PartialEq + Copy, R>(shared: &Shared<S, R>, wait: bool) -> Option<Entry<S, R>> {
    let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            return None;
        }
        // (a slot with a job running waits for it: see `Shared::busy`)
        let best = {
            let busy = shared.busy.lock().unwrap_or_else(|e| e.into_inner());
            q.iter().enumerate().filter(|(_, e)| !busy.contains(&e.slot)).max_by_key(|(_, e)| (e.priority, e.seq)).map(|(i, _)| i)
        };
        if let Some(i) = best {
            let e = q.swap_remove(i);
            shared.busy.lock().unwrap_or_else(|e| e.into_inner()).push(e.slot);
            return Some(e);
        }
        if !wait {
            return None;
        }
        q = shared.cv.wait(q).unwrap_or_else(|e| e.into_inner());
    }
}

/// The job for `slot` ended: a job queued for it may start.
fn finished<S: PartialEq, R>(shared: &Shared<S, R>, slot: &S) {
    {
        // under the queue lock: a worker is either before its look at `busy` or waiting
        let _q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        let mut busy = shared.busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = busy.iter().position(|s| s == slot) {
            busy.swap_remove(i);
        }
    }
    shared.cv.notify_all();
}

/// Frees a job's slot when the job ends (see [`finished`]).
struct Free<'a, S: PartialEq, R>(&'a Shared<S, R>, S);

impl<S: PartialEq, R> Drop for Free<'_, S, R> {
    fn drop(&mut self) {
        finished(self.0, &self.1);
    }
}

/// Takes a worker off the live list when its thread ends (also when a job panicked).
#[cfg(not(target_arch = "wasm32"))]
struct Ended<'a, S, R>(&'a Shared<S, R>);

#[cfg(not(target_arch = "wasm32"))]
impl<S, R> Drop for Ended<'_, S, R> {
    fn drop(&mut self) {
        let me = std::thread::current().id();
        self.0.live.lock().unwrap_or_else(|e| e.into_inner()).retain(|id| *id != me);
        self.0.ended.notify_all();
    }
}

/// Tell the workers to stop and take the queued jobs away from them.
fn close<S, R>(shared: &Shared<S, R>) {
    // the flag changes under the queue lock: a worker is either before its check or waiting
    let queued = {
        let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        shared.shutdown.store(true, Ordering::Relaxed);
        std::mem::take(&mut *q)
    };
    shared.cv.notify_all();
    drop(queued); // outside the lock: a job may own a lot
}

/// [`JobPool::shutdown`]: wait up to `timeout` for the workers to end, without holding the queue
/// lock, and never for the calling thread (a pool dropped by one of its own jobs).
#[cfg(not(target_arch = "wasm32"))]
fn stop<S, R>(shared: &Shared<S, R>, workers: &mut Vec<std::thread::JoinHandle<()>>, timeout: Duration) -> bool {
    close(shared);
    let me = std::thread::current().id();
    let t0 = std::time::Instant::now();
    let mut live = shared.live.lock().unwrap_or_else(|e| e.into_inner());
    while live.iter().any(|id| *id != me) {
        let Some(left) = timeout.checked_sub(t0.elapsed()).filter(|d| !d.is_zero()) else { break };
        live = shared.ended.wait_timeout(live, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    let running: Vec<std::thread::ThreadId> = live.iter().copied().filter(|id| *id != me).collect();
    drop(live);
    let mut remaining = Vec::new();
    for h in workers.drain(..) {
        let id = h.thread().id();
        // a worker off the list is past its last job: joining it only waits for the thread to unwind
        if id != me && !running.contains(&id) {
            let _ = h.join();
        } else {
            remaining.push(h);
        }
    }
    *workers = remaining;
    if !running.is_empty() {
        log::warn!("job pool: {} job(s) still running after {timeout:?}; not waiting for them", running.len());
    }
    running.is_empty()
}

impl<S, R> Drop for JobPool<S, R> {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if !self.workers.is_empty() {
            stop(&self.shared, &mut self.workers, DROP_WAIT);
        } else if !self.stopped {
            close(&self.shared);
        }
        #[cfg(target_arch = "wasm32")]
        close(&self.shared);
    }
}
