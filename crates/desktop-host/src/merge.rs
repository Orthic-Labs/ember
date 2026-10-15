//! Asynchronous merge-preview transport for desktop clients.
//!
//! Merge jobs are planned on Session's owner thread, then run from detached, cancellable workers.
//! Workers return only encoded pixels through PreviewStore; no preview path is accepted or written.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lightcraft_engine::Session;
use lightcraft_engine::merge::MergeOutput;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::preview::{PreviewStore, encode_png};

const MAX_SLOT_BYTES: usize = 96;
const MAX_IDS: usize = 256;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;
const MAX_WORKERS: usize = 16;
const MAX_SLOTS: usize = 32;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePreviewRequest {
    pub command: String,
    pub params: Value,
    pub slot: String,
    pub view_generation: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePreviewCancelRequest {
    pub slot: String,
    pub sequence: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePreviewDescriptor {
    pub handle: String,
    pub slot: String,
    pub view_generation: u64,
    pub sequence: u64,
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub info: Value,
    pub render_ms: f64,
}

struct Task {
    sequence: u64,
    cancel: Arc<AtomicBool>,
    reply: Sender<Result<MergePreviewDescriptor, String>>,
    worker: JoinHandle<()>,
}

struct Completion {
    ticket: u64,
    slot: String,
    view_generation: u64,
    sequence: u64,
    result: Result<MergeOutput, String>,
    cancelled: bool,
    render_ms: f64,
}

/// Owner-thread state for bounded merge preview workers.
pub(crate) struct MergePreviews {
    store: PreviewStore,
    jobs: HashMap<u64, Task>,
    current: HashMap<String, u64>,
    latest: HashMap<String, u64>,
    tx: Sender<Completion>,
    rx: Receiver<Completion>,
    next_ticket: u64,
}

impl MergePreviews {
    pub(crate) fn new(store: PreviewStore) -> Self {
        let (tx, rx) = channel();
        Self { store, jobs: HashMap::new(), current: HashMap::new(), latest: HashMap::new(), tx, rx, next_ticket: 1 }
    }

    pub(crate) fn request(
        &mut self,
        session: &mut Session,
        request: MergePreviewRequest,
        reply: Sender<Result<MergePreviewDescriptor, String>>,
    ) -> Result<(), String> {
        validate_request(&request)?;
        let (current_generation, _) = session.visible_shared();
        validate_generation(request.view_generation, current_generation)?;
        if !sequence_is_fresh(self.latest.get(&request.slot), request.sequence) {
            return Err("merge preview request is stale".into());
        }
        if !self.latest.contains_key(&request.slot) && self.latest.len() >= MAX_SLOTS {
            return Err("too many merge preview slots; reuse an existing slot or change catalog".into());
        }
        let mut params = request.params.clone();
        let values = params.as_object_mut().ok_or_else(|| "merge preview params must be an object".to_string())?;
        if values.contains_key("previewPath") {
            return Err("merge preview does not accept previewPath".into());
        }
        if let Some(ids) = values.get("ids") {
            let ids = ids.as_array().ok_or_else(|| "merge preview ids must be an array".to_string())?;
            if ids.len() > MAX_IDS {
                return Err(format!("merge preview accepts at most {MAX_IDS} photos"));
            }
        }
        values.insert("preview".into(), Value::Bool(true));
        let (kind, finish) = lightcraft_engine::merge::parse(&request.command, &params).map_err(|error| error.to_string())?;
        let ids = session.targets(&params).map_err(|error| error.to_string())?;
        if ids.len() > MAX_IDS {
            return Err(format!("merge preview accepts at most {MAX_IDS} photos"));
        }
        for id in &ids {
            crate::validate_id(id.0)?;
        }
        let job = session.plan_merge(kind, finish, &ids, true).map_err(|error| error.to_string())?;
        if let Some(ticket) = self.current.get(&request.slot).copied()
            && let Some(previous) = self.jobs.get(&ticket)
        {
            if previous.sequence >= request.sequence {
                return Err("merge preview request is stale".into());
            }
            previous.cancel.store(true, Ordering::Relaxed);
        }
        if self.jobs.len() >= MAX_WORKERS {
            return Err("too many merge previews are stopping; retry shortly".into());
        }
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_tx = self.tx.clone();
        let slot = request.slot.clone();
        let view_generation = request.view_generation;
        let sequence = request.sequence;
        let worker = std::thread::Builder::new()
            .name("lightcraft-merge-preview".into())
            .spawn(move || {
                let started = Instant::now();
                let result =
                    lightcraft_engine::guard::catch("merge preview", || job.run(&|_, _| !worker_cancel.load(Ordering::Relaxed))).unwrap_or_else(Err);
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = worker_tx.send(Completion {
                    ticket,
                    slot,
                    view_generation,
                    sequence,
                    result,
                    cancelled,
                    render_ms: started.elapsed().as_secs_f64() * 1000.0,
                });
            })
            .map_err(|error| format!("could not start merge preview: {error}"))?;
        self.current.insert(request.slot.clone(), ticket);
        self.latest.insert(request.slot.clone(), request.sequence);
        self.jobs.insert(ticket, Task { sequence: request.sequence, cancel, reply, worker });
        Ok(())
    }

    pub(crate) fn cancel(&mut self, request: MergePreviewCancelRequest) -> Result<bool, String> {
        validate_cancel(&request)?;
        let latest = self.latest.get(&request.slot).copied().unwrap_or(0);
        if !self.latest.contains_key(&request.slot) && self.latest.len() >= MAX_SLOTS {
            return Err("too many merge preview slots; reuse an existing slot or change catalog".into());
        }
        self.latest.insert(request.slot.clone(), latest.max(request.sequence));
        let Some(ticket) = self.current.get(&request.slot).copied() else { return Ok(false) };
        let Some(task) = self.jobs.get(&ticket) else { return Ok(false) };
        if task.sequence <= request.sequence {
            task.cancel.store(true, Ordering::Relaxed);
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn poll(&mut self, session: &mut Session) {
        let (current_generation, _) = session.visible_shared();
        while let Ok(done) = self.rx.try_recv() {
            let Some(task) = self.jobs.remove(&done.ticket) else { continue };
            let _ = task.worker.join();
            let current = self.current.get(&done.slot).copied() == Some(done.ticket);
            if !current {
                let _ = task.reply.send(Err("merge preview superseded".into()));
                continue;
            }
            self.current.remove(&done.slot);
            if done.cancelled || task.cancel.load(Ordering::Relaxed) {
                let _ = task.reply.send(Err("cancelled".into()));
                continue;
            }
            if done.view_generation != current_generation {
                let _ = task.reply.send(Err("merge preview view is stale".into()));
                continue;
            }
            let result = done.result.and_then(|output| {
                let image = output.preview.ok_or_else(|| "merge preview produced no image".to_string())?;
                let width = u32::try_from(image.width).map_err(|_| "merge preview width exceeds transport limit".to_string())?;
                let height = u32::try_from(image.height).map_err(|_| "merge preview height exceeds transport limit".to_string())?;
                let bytes = encode_png(&image)?;
                let handle = self.store.insert(Arc::<[u8]>::from(bytes), "image/png")?;
                Ok(MergePreviewDescriptor {
                    handle,
                    slot: done.slot.clone(),
                    view_generation: done.view_generation,
                    sequence: done.sequence,
                    key: format!("{:016x}", done.ticket),
                    width,
                    height,
                    info: output.info,
                    render_ms: done.render_ms,
                })
            });
            match result {
                Ok(descriptor) => {
                    let handle = descriptor.handle.clone();
                    if task.reply.send(Ok(descriptor)).is_err() {
                        let _ = self.store.acknowledge(&handle);
                    }
                }
                Err(error) => {
                    let _ = task.reply.send(Err(error));
                }
            }
        }
    }

    pub(crate) fn running(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub(crate) fn reset(&mut self) {
        debug_assert!(self.jobs.is_empty());
        self.current.clear();
        self.latest.clear();
    }

    pub(crate) fn cancel_all(&self) {
        for task in self.jobs.values() {
            task.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn shutdown(&mut self, session: &mut Session, timeout: Duration) -> Result<(), String> {
        self.cancel_all();
        let deadline = Instant::now() + timeout;
        while self.running() && Instant::now() < deadline {
            self.poll(session);
            std::thread::sleep(Duration::from_millis(5));
        }
        self.poll(session);
        if self.running() { Err("merge previews are still stopping; retry shutdown shortly".into()) } else { Ok(()) }
    }
}

fn validate_request(request: &MergePreviewRequest) -> Result<(), String> {
    if !matches!(request.command.as_str(), "merge.hdr" | "merge.panorama" | "merge.hdrPanorama") {
        return Err("unknown merge preview command".into());
    }
    if request.slot.is_empty()
        || request.slot.len() > MAX_SLOT_BYTES
        || !request.slot.is_ascii()
        || request.slot.bytes().any(|byte| byte == b'/' || byte == b'\\')
    {
        return Err("merge preview slot is invalid".into());
    }
    if request.view_generation > MAX_SEQUENCE || request.sequence > MAX_SEQUENCE {
        return Err("merge preview identifier exceeds safe integer range".into());
    }
    Ok(())
}

fn validate_generation(requested: u64, current: u64) -> Result<(), String> {
    if requested == current { Ok(()) } else { Err("merge preview view generation is stale; refresh the catalog view".into()) }
}

fn sequence_is_fresh(latest: Option<&u64>, sequence: u64) -> bool {
    latest.is_none_or(|latest| *latest < sequence)
}

fn validate_cancel(request: &MergePreviewCancelRequest) -> Result<(), String> {
    if request.slot.is_empty()
        || request.slot.len() > MAX_SLOT_BYTES
        || !request.slot.is_ascii()
        || request.slot.bytes().any(|byte| byte == b'/' || byte == b'\\')
    {
        return Err("merge preview slot is invalid".into());
    }
    if request.sequence > MAX_SEQUENCE {
        return Err("merge preview sequence exceeds safe integer range".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancel(slot: &str, sequence: u64) -> MergePreviewCancelRequest {
        MergePreviewCancelRequest { slot: slot.into(), sequence }
    }

    #[test]
    fn cancellation_before_request_leaves_sequence_tombstone() {
        let mut previews = MergePreviews::new(PreviewStore::default());
        assert_eq!(previews.cancel(cancel("merge__main", 7)), Ok(false));
        assert!(!sequence_is_fresh(previews.latest.get("merge__main"), 7));
        assert!(!sequence_is_fresh(previews.latest.get("merge__main"), 6));
    }

    #[test]
    fn older_cancel_cannot_lower_completed_sequence() {
        let mut previews = MergePreviews::new(PreviewStore::default());
        assert_eq!(previews.cancel(cancel("merge__main", 9)), Ok(false));
        assert_eq!(previews.cancel(cancel("merge__main", 4)), Ok(false));
        assert_eq!(previews.latest.get("merge__main"), Some(&9));
        assert!(!sequence_is_fresh(previews.latest.get("merge__main"), 9));
        assert!(sequence_is_fresh(previews.latest.get("merge__main"), 10));
    }

    #[test]
    fn generation_mismatch_is_rejected_before_worker_planning() {
        let error = validate_generation(12, 13).expect_err("stale generation must be rejected");
        assert!(error.contains("stale"));
    }

    #[test]
    fn slot_tombstones_are_bounded() {
        let mut previews = MergePreviews::new(PreviewStore::default());
        for index in 0..MAX_SLOTS {
            assert_eq!(previews.cancel(cancel(&format!("merge-{index}"), 1)), Ok(false));
        }
        assert!(previews.cancel(cancel("merge-overflow", 1)).is_err());
    }
}
