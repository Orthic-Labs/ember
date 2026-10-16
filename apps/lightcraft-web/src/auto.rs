//! Auto off the browser's main thread (#192).
//!
//! `develop.auto`, `develop.autoTone`, `develop.autoControl`, `develop.autoBatch`,
//! `develop.autoBwMix`, `develop.autoNoise`, White Balance Auto, a preset that applies Auto and a
//! dry run prepare their input on the main thread (cheap; it needs the session), and the measuring
//! runs in a render worker ([`Workers::offer_auto`]): the worker decodes the photo's source itself
//! (thumbnail level, the original for the noise fit; from storage, or from the bytes the page sends
//! with `?store=memory`) and measures with the engine's pure `measure_kind`. The values come back as
//! JSON; the UI's `auto_task` then applies them on this thread as one undo step, refused when the
//! photo changed meanwhile. A first command after a reload needs no original on the main thread, so
//! it does not wait for one.
//!
//! The row in the activity stack belongs to `auto_task`. When every worker is busy (they also
//! render the views) the measure waits for one, polling every [`RETRY_MS`] for up to
//! [`GIVE_UP_MS`]; if no worker is alive the host declines and the command runs in the session.

use lightcraft_engine::auto_measure::{AutoInput, AutoMeasured};
use lightcraft_engine::catalog::Source;
use lightcraft_ui_egui::auto_task::{AutoDone, AutoOffload};
use std::rc::Rc;

use crate::exports::later;
use crate::store::{Originals, hash_of_path};
use crate::wire::WireAuto;
use crate::workers::{AutoDone as WorkerDone, AutoFailure, AutoOffer, Workers};

/// How often a measure looks again for a free worker (ms).
const RETRY_MS: i32 = 50;

/// How long a measure waits for a free worker before it gives up (ms).
const GIVE_UP_MS: f64 = 30_000.0;

/// The message a worker is given.
struct Job {
    text: String,
    /// The original's bytes, when the page has no storage for the worker to read them from.
    original: Option<Vec<u8>>,
}

/// The main thread's Auto measure runner.
#[derive(Clone)]
pub struct AutoHost {
    workers: Workers,
    originals: Originals,
    /// The page has browser storage, where a worker reads an original itself.
    stored: bool,
    ctx: egui::Context,
}

impl AutoHost {
    pub fn new(workers: Workers, originals: Originals, stored: bool, ctx: egui::Context) -> AutoHost {
        AutoHost { workers, originals, stored, ctx }
    }

    /// The hook for `LightcraftApp::auto_offload`.
    pub fn offload(&self) -> AutoOffload {
        let host = self.clone();
        Rc::new(move |input, done| host.start(input, done))
    }

    /// Take a measure, or decline (`false`) when no worker is alive or the photo can't be sent.
    fn start(&self, input: AutoInput, done: AutoDone) -> bool {
        if !self.workers.any_alive() {
            return false;
        }
        let Some(job) = self.job_for(&input) else { return false };
        let ctx = self.ctx.clone();
        let reply: WorkerDone = Box::new(move |result| {
            let values = match result {
                Ok(text) => serde_json::from_str::<AutoMeasured>(&text).map_err(|e| format!("Auto values: {e}")),
                Err(f) if f.worker_lost => Err(format!("Auto: its render worker failed ({}); run it again", f.message)),
                Err(f) => Err(f.message),
            };
            done(values);
            ctx.request_repaint();
        });
        self.offer(job, reply, js_sys::Date::now());
        true
    }

    /// Give the measure to a free worker; while none is free, look again shortly.
    fn offer(&self, job: Job, reply: WorkerDone, since: f64) {
        match self.workers.offer_auto(&job.text, job.original.as_deref(), reply) {
            AutoOffer::Started => {}
            AutoOffer::NoWorkers(reply) => reply(Err(AutoFailure { message: "no render worker is running".into(), worker_lost: true })),
            AutoOffer::Busy(reply) => {
                if js_sys::Date::now() - since > GIVE_UP_MS {
                    reply(Err(AutoFailure { message: "Auto: every render worker stayed busy; try again".into(), worker_lost: false }));
                    return;
                }
                // if the browser won't schedule it, the reply is dropped and the UI ends the row
                let host = self.clone();
                later(RETRY_MS, move || host.offer(job, reply, since));
            }
        }
    }

    /// The job for a worker. `None`: not sendable (an original that is not stored), so the
    /// session measures it.
    fn job_for(&self, input: &AutoInput) -> Option<Job> {
        let original = match &input.origin {
            Source::File { path } if !self.stored => Some(self.originals.get(path)?.to_vec()),
            Source::File { path } if hash_of_path(path).is_none() => return None,
            _ => None,
        };
        let text = serde_json::to_string(&WireAuto::from_input(input)).map_err(|e| log::warn!("auto: {e}")).ok()?;
        Some(Job { text, original })
    }
}
