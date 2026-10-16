//! Exports off the browser's main thread (#192).
//!
//! `app.export` prepares each photo on the main thread (cheap; it needs the session), and this
//! module renders and encodes them in the render workers ([`Workers::offer_export`]): one photo
//! at a time, so a batch never holds more than one decoded original, and the page stays
//! responsive. The files come back as transferred bytes; the main thread then places them
//! (naming, conflicts, never an original: the engine's [`BatchPlacer`]) and hands each to the
//! browser as a download, which only the main thread can do.
//!
//! The row in the activity stack, its cross and the closing toast belong to the UI's export task
//! ([`ExportReport`]). A cancel takes effect at once: the photo in a worker finishes there and its
//! file is dropped.
//!
//! What a worker can't make runs here, one photo per timer tick so the page keeps painting: an
//! original copy or a DNG, a photo with a Denoise edit, and everything when no worker is left. A
//! worker that fails mid-photo hands that photo back the same way, so no photo is lost to it.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use lightcraft_engine::export::{BatchHead, BatchPlacer, Exported, PreparedExport};
use lightcraft_ui_egui::export_task::{ExportBatch, ExportOffload, ExportReport};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::store::Originals;
use crate::wire::WireExport;
use crate::workers::{ExportFailure, ExportFile, ExportOffer, Workers};

/// How often a batch looks again at its cancel flag and at the workers (ms).
const TICK_MS: i32 = 100;

/// Hands a file to the browser (`web.rs`'s download).
type WriteFn = fn(&str, &[u8]) -> Result<(), String>;

/// One running batch.
struct Run {
    queue: VecDeque<PreparedExport>,
    /// The photo a worker is making, and the number of its job: an answer for any other number
    /// (a photo given up on by a cancel) is not wanted.
    current: Option<(PreparedExport, BatchHead)>,
    token: u64,
    placer: Option<BatchPlacer>,
    report: Option<ExportReport>,
    placed: usize,
    finished: bool,
}

impl Run {
    /// End the batch: the files placed so far go to the UI's export task.
    fn finish(&mut self, cancelled: bool) {
        self.finished = true;
        self.queue.clear();
        self.current = None;
        let files = self.placer.take().map(BatchPlacer::finish).unwrap_or_default();
        if let Some(report) = self.report.take() {
            report.finish(files, cancelled);
        }
    }

    fn fail(&mut self, error: String) {
        self.finished = true;
        self.queue.clear();
        self.current = None;
        self.placer = None;
        if let Some(report) = self.report.take() {
            report.fail(error);
        }
    }
}

/// The main thread's export runner.
#[derive(Clone)]
pub struct ExportHost {
    workers: Workers,
    originals: Originals,
    /// The page has browser storage, where a worker reads an original itself; without it the
    /// bytes in memory go with the job.
    stored: bool,
    write: WriteFn,
    ctx: egui::Context,
}

impl ExportHost {
    pub fn new(workers: Workers, originals: Originals, stored: bool, write: WriteFn, ctx: egui::Context) -> ExportHost {
        ExportHost { workers, originals, stored, write, ctx }
    }

    /// The hook for `LightcraftApp::export_offload`.
    pub fn offload(&self) -> ExportOffload {
        let host = self.clone();
        Rc::new(move |batch| host.start(batch))
    }

    /// Take a batch, or give it back when no worker is alive to run any of it.
    fn start(&self, batch: ExportBatch) -> Result<(), ExportBatch> {
        if !self.workers.any_alive() {
            return Err(batch);
        }
        let ExportBatch { items, opts, to, report } = batch;
        let run = Run {
            placer: Some(BatchPlacer::new(&opts, &to, items.len())),
            queue: items.into(),
            current: None,
            token: 0,
            report: Some(report),
            placed: 0,
            finished: false,
        };
        let run = Rc::new(RefCell::new(run));
        self.step(&run);
        self.tick(run);
        Ok(())
    }

    /// Look at the batch every [`TICK_MS`] until it ends: a cancel, or a worker coming free.
    fn tick(&self, run: Rc<RefCell<Run>>) {
        if run.borrow().finished {
            return;
        }
        self.step(&run);
        if run.borrow().finished {
            return;
        }
        let (host, again) = (self.clone(), run.clone());
        if !later(TICK_MS, move || host.tick(again)) {
            run.borrow_mut().fail("the browser would not schedule the export".into());
            self.ctx.request_repaint();
        }
    }

    /// Advance the batch as far as it goes without waiting: place finished photos, start the next.
    fn step(&self, run: &Rc<RefCell<Run>>) {
        let mut r = run.borrow_mut();
        if r.finished {
            return;
        }
        let cancelled = r.report.as_ref().is_some_and(ExportReport::is_cancelled);
        if cancelled {
            // a photo still in a worker is given up on: its answer is not wanted
            r.token += 1;
            r.finish(true);
            self.ctx.request_repaint();
            return;
        }
        if r.current.is_some() {
            return;
        }
        let Some(item) = r.queue.pop_front() else {
            r.finish(false);
            self.ctx.request_repaint();
            return;
        };
        if let Some(report) = &r.report {
            report.progress(r.placed, &item.file_name);
        }
        let head = item.head();
        let Some((text, original)) = self.job_for(&item) else {
            // made here; the next photo, after the page has painted
            let result = item.run();
            self.place(&mut r, &head, result);
            let (host, next) = (self.clone(), run.clone());
            drop(r);
            if !later(0, move || host.step(&next)) {
                run.borrow_mut().fail("the browser would not schedule the export".into());
            }
            self.ctx.request_repaint();
            return;
        };
        r.token += 1;
        let (host, next, token) = (self.clone(), run.clone(), r.token);
        let done = Box::new(move |result: Result<ExportFile, ExportFailure>| host.worker_done(&next, token, result));
        match self.workers.offer_export(&text, original.as_deref(), done) {
            ExportOffer::Started => r.current = Some((item, head)),
            ExportOffer::Busy(_) => {
                // every worker is busy: this photo goes first again on the next tick
                r.token += 1;
                r.queue.push_front(item);
            }
            ExportOffer::NoWorkers(_) => {
                r.token += 1;
                r.queue.push_front(item);
                // none left to wait for: the rest run here
                drop(r);
                self.run_here(run);
            }
        }
    }

    /// No worker is alive: make the remaining photos on this thread, one per timer tick.
    fn run_here(&self, run: &Rc<RefCell<Run>>) {
        let mut r = run.borrow_mut();
        if r.finished {
            return;
        }
        if r.report.as_ref().is_some_and(ExportReport::is_cancelled) {
            r.finish(true);
            self.ctx.request_repaint();
            return;
        }
        let Some(item) = r.queue.pop_front() else {
            r.finish(false);
            self.ctx.request_repaint();
            return;
        };
        if let Some(report) = &r.report {
            report.progress(r.placed, &item.file_name);
        }
        let head = item.head();
        let result = item.run();
        self.place(&mut r, &head, result);
        drop(r);
        let (host, next) = (self.clone(), run.clone());
        if !later(0, move || host.run_here(&next)) {
            run.borrow_mut().fail("the browser would not schedule the export".into());
        }
        self.ctx.request_repaint();
    }

    /// The job for a worker, as JSON, and the original's bytes when the page has no storage to
    /// read them from. `None`: this photo is made here.
    fn job_for(&self, item: &PreparedExport) -> Option<(String, Option<Vec<u8>>)> {
        let parts = item.render_parts()?;
        let wire = match WireExport::from_parts(&parts, &item.file_name) {
            Ok(w) => w,
            Err(why) => {
                log::info!("export {}: made on the main thread ({why})", item.file_name);
                return None;
            }
        };
        let original = match (&wire.job.origin, wire.needs_original()) {
            (lightcraft_engine::catalog::Source::File { path }, true) if !self.stored => Some(self.originals.get(path)?.to_vec()),
            (lightcraft_engine::catalog::Source::File { path }, true) if crate::store::hash_of_path(path).is_none() => return None,
            _ => None,
        };
        let text = serde_json::to_string(&wire).map_err(|e| log::warn!("export {}: {e}", item.file_name)).ok()?;
        Some((text, original))
    }

    /// A worker's answer for job `token`.
    fn worker_done(&self, run: &Rc<RefCell<Run>>, token: u64, result: Result<ExportFile, ExportFailure>) {
        {
            let mut r = run.borrow_mut();
            if r.finished || r.token != token {
                return;
            }
            let Some((item, head)) = r.current.take() else { return };
            let exported = match result {
                Ok(file) => Ok(Exported {
                    file_name: head.file_name().to_string(),
                    bytes: file.bytes,
                    width: file.width,
                    height: file.height,
                    sidecars: Vec::new(),
                    gpu_fallback: None,
                }),
                // the worker went away, not this photo's fault: make it here
                Err(f) if f.worker_lost => {
                    log::warn!("export {}: its worker failed ({}); making it on the main thread", head.file_name(), f.message);
                    item.run()
                }
                Err(f) => {
                    log::warn!("export {}: {}", head.file_name(), f.message);
                    Err(f.message)
                }
            };
            self.place(&mut r, &head, exported);
        }
        self.ctx.request_repaint();
        self.step(run);
    }

    /// Write one photo's file (or record its failure), in batch order.
    fn place(&self, r: &mut Run, head: &BatchHead, result: Result<Exported, String>) {
        let write = self.write;
        if let Some(placer) = r.placer.as_mut() {
            placer.place(head, result, &mut |path, bytes| write(path, bytes), &|_| false);
        }
        r.placed += 1;
    }
}

/// Run `f` once after `ms` milliseconds. `false`: the browser would not schedule it.
fn later(ms: i32, f: impl FnOnce() + 'static) -> bool {
    let Some(window) = web_sys::window() else { return false };
    let callback = Closure::once_into_js(f);
    window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), ms).is_ok()
}
