//! Desktop preview renderer: bounded requests, detached engine jobs, and presentation gating.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::HashMap;
use std::sync::Arc;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::Session;
use lightcraft_engine::media::{QuickJob, RenderJob, RenderResult};
use lightcraft_pipeline::StageCache;
use lightcraft_preview::JobPool;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::preview::{PreviewStore, encode_png};

const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_SLOT_BYTES: usize = 96;
const MAX_PENDING: usize = 96;
const MAX_SLOTS: usize = 256;
const MAX_THUMBS_PER_PHOTO: usize = 4;
const MAX_THUMBS: usize = 512;
const MAX_THUMB_BYTES: usize = 64 * 1024 * 1024;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;
const INTERACTIVE_PRIORITY: u32 = 200;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PreviewQuality {
    Draft,
    Full,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    pub photo_id: u64,
    pub slot: String,
    pub view_generation: u64,
    pub width: u32,
    pub height: u32,
    pub quality: PreviewQuality,
    pub before: bool,
    pub sequence: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDescriptor {
    pub handle: String,
    pub photo_id: u64,
    pub slot: String,
    pub view_generation: u64,
    pub sequence: u64,
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub histogram: Value,
    pub render_ms: f64,
    pub encoding: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SlotId(u64);

struct Pending {
    ticket: u64,
    submission: u64,
    request: PreviewRequest,
    key: u64,
    priority: u32,
    forced_draft: bool,
    quick: bool,
    reply: std::sync::mpsc::Sender<Result<PreviewDescriptor, String>>,
}

struct JobOutput {
    ticket: u64,
    view_generation: u64,
    sequence: u64,
    result: RenderResult,
    payload: Result<EncodedPreview, String>,
}

struct EncodedPreview {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    histogram: Value,
}

struct CachedThumb {
    key: u64,
    bytes: Arc<[u8]>,
    width: u32,
    height: u32,
    histogram: Value,
}

/// Host-side render port. It is owned by the Session owner thread; worker jobs contain no Session.
pub struct Renderer {
    store: PreviewStore,
    pool: JobPool<SlotId, JobOutput>,
    slots: HashMap<String, SlotId>,
    quick_slots: HashMap<String, SlotId>,
    next_slot: u64,
    next_ticket: u64,
    pending: HashMap<SlotId, Pending>,
    failed: HashMap<(SlotId, u64), String>,
    stages: HashMap<SlotId, Arc<StageCache>>,
    thumbs: HashMap<u64, Vec<CachedThumb>>,
    thumb_count: usize,
    thumb_bytes: usize,
}

impl Renderer {
    pub fn new(store: PreviewStore) -> Self {
        let threads = lightcraft_preview::JobPool::<SlotId, JobOutput>::default_threads().min(6);
        Self {
            store,
            pool: JobPool::new(threads),
            slots: HashMap::new(),
            quick_slots: HashMap::new(),
            next_slot: 1,
            next_ticket: 1,
            pending: HashMap::new(),
            failed: HashMap::new(),
            stages: HashMap::new(),
            thumbs: HashMap::new(),
            thumb_count: 0,
            thumb_bytes: 0,
        }
    }

    pub fn store(&self) -> PreviewStore {
        self.store.clone()
    }

    /// Queue a render. A newer sequence replaces a queued request for same slot.
    pub fn request(
        &mut self,
        session: &mut Session,
        request: PreviewRequest,
        reply: std::sync::mpsc::Sender<Result<PreviewDescriptor, String>>,
    ) -> Result<(), String> {
        if let Err(error) = validate_request(&request) {
            let _ = reply.send(Err(error));
            return Ok(());
        }
        let slot = self.slot_id(&request.slot);
        if self
            .pending
            .get(&slot)
            .is_some_and(|pending| pending.request.sequence > request.sequence || (pending.request.sequence == request.sequence && !pending.quick))
        {
            let _ = reply.send(Err("preview request is stale".to_string()));
            return Ok(());
        }
        let photo = PhotoId(request.photo_id);
        let job = session
            .render_job(photo, request.width as usize, request.height as usize, request.before, true)
            .ok_or_else(|| "photo is unavailable".to_string());
        let mut job = match job {
            Ok(job) => job,
            Err(error) => {
                let _ = reply.send(Err(error));
                return Ok(());
            }
        };
        // The React host keeps preview requests at full quality because it does not own
        // engine interaction state.  Session is authoritative: while a slider/brush
        // interaction is open, use the pipeline's draft path so the loupe can keep up.
        let interactive = session.interaction.is_some();
        if uses_draft_quality(request.quality, interactive) {
            job = job.draft();
        }
        if !request.before
            && self.is_loupe_slot(&request.slot)
            && let Some(loupe) = session.loupe_job(photo, request.width as usize, request.height as usize, true)
        {
            job = if uses_draft_quality(request.quality, interactive) { loupe.draft() } else { loupe };
        }
        let key = job.key;
        if let Some(previous) = self.pending.remove(&slot) {
            let _ = previous.reply.send(Err("preview superseded".to_string()));
            self.drop_queued();
        }
        if let Some(error) = self.failed.get(&(slot, key)).cloned() {
            let _ = reply.send(Err(error));
            return Ok(());
        }
        if self.is_thumb_slot(&request.slot)
            && let Some(descriptor) = self.cached_thumb(photo.0, key, &request)
        {
            let _ = reply.send(Ok(descriptor));
            return Ok(());
        }
        let stages = self.stages.entry(slot).or_default().clone();
        let interactive_loupe = interactive && self.is_loupe_slot(&request.slot);
        if !self.submit(slot, request, key, job.with_stages(stages), interactive, reply) {
            return Ok(());
        }
        if interactive_loupe {
            self.drop_background_queued();
        }
        self.apply_pressure();
        Ok(())
    }

    /// Queue engine's existing quick stand-in for a slot. The regular `request` remains the
    /// authoritative path for final output; callers use this for an immediate first paint.
    pub fn request_quick(
        &mut self,
        session: &mut Session,
        request: PreviewRequest,
        reply: std::sync::mpsc::Sender<Result<PreviewDescriptor, String>>,
    ) -> Result<(), String> {
        if let Err(error) = validate_request(&request) {
            let _ = reply.send(Err(error));
            return Ok(());
        }
        if request.before {
            return self.request(session, request, reply);
        }
        let slot = self.quick_slot_id(&request.slot);
        if self.pending.get(&slot).is_some_and(|pending| pending.request.sequence >= request.sequence) {
            let _ = reply.send(Err("preview request is stale".to_string()));
            return Ok(());
        }
        let job = session
            .quick_view_job(PhotoId(request.photo_id), request.width.max(request.height) as usize, true)
            .ok_or_else(|| "photo is unavailable".to_string());
        let job = match job {
            Ok(job) => job,
            Err(error) => {
                let _ = reply.send(Err(error));
                return Ok(());
            }
        };
        let key = job.key;
        if let Some(previous) = self.pending.remove(&slot) {
            let _ = previous.reply.send(Err("preview superseded".to_string()));
            self.drop_queued();
        }
        let _ = self.submit_quick(slot, request, key, job, session.interaction.is_some(), reply);
        self.apply_pressure();
        Ok(())
    }

    /// Drain completed jobs. Session accepts every result, while only current request metadata is
    /// allowed through presentation gate; a draft completing after interaction closes is rerun full.
    pub fn poll(&mut self, session: &mut Session) -> usize {
        let mut completed = 0;
        while let Some(failure) = self.pool.try_failure() {
            completed += 1;
            let matches =
                self.pending.get(&failure.slot).is_some_and(|pending| pending.key == failure.key && pending.submission == failure.submission);
            if matches && let Some(pending) = self.pending.remove(&failure.slot) {
                let _ = pending.reply.send(Err(format!("preview worker failed: {}", failure.error)));
            }
        }
        while let Some(done) = self.pool.try_recv() {
            completed += 1;
            let slot = done.slot;
            let JobOutput { ticket, view_generation, sequence, result, payload } = done.result;
            let current_ticket = self.pending.get(&slot).is_some_and(|pending| pending.ticket == ticket);
            session.accept(&result);
            if !current_ticket {
                continue;
            }
            let Some(pending) = self.pending.get(&slot) else { continue };
            let (visible_generation, _) = session.visible_shared();
            if pending.request.view_generation != visible_generation {
                if let Some(stale) = self.pending.remove(&slot) {
                    let _ = stale.reply.send(Err("preview view is stale".to_string()));
                }
                self.drop_queued();
                continue;
            }
            let interaction_ended = pending.forced_draft && session.interaction.is_none();
            if interaction_ended {
                self.requeue_full_after_interaction(session, slot);
                continue;
            }
            if !publishable(pending, ticket, view_generation, sequence, result.key, result.photo) {
                continue;
            }
            let Some(pending) = self.pending.remove(&slot) else { continue };
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    self.failed.insert((slot, result.key), error.clone());
                    let _ = pending.reply.send(Err(error));
                    continue;
                }
            };
            let bytes = Arc::<[u8]>::from(payload.bytes);
            let handle = match self.store.insert(bytes.clone(), "image/png") {
                Ok(handle) => handle,
                Err(error) => {
                    self.failed.insert((slot, result.key), error.clone());
                    let _ = pending.reply.send(Err(error));
                    continue;
                }
            };
            let width = payload.width;
            let height = payload.height;
            let descriptor = PreviewDescriptor {
                handle: handle.clone(),
                photo_id: pending.request.photo_id,
                slot: pending.request.slot.clone(),
                view_generation: pending.request.view_generation,
                sequence: pending.request.sequence,
                key: format!("{:016x}", result.key),
                width,
                height,
                histogram: payload.histogram.clone(),
                render_ms: done.ms,
                encoding: "png",
                provisional: result.quick.map(|_| true),
                source: result.quick.map(|source| match source {
                    lightcraft_engine::media::QuickSource::Cached => "cached",
                    lightcraft_engine::media::QuickSource::Embedded => "embedded",
                    lightcraft_engine::media::QuickSource::Small => "quick",
                }),
            };
            self.failed.retain(|(candidate, key), _| *candidate != slot || *key != result.key);
            if self.is_thumb_slot(&pending.request.slot) {
                self.cache_thumb(pending.request.photo_id, CachedThumb { key: result.key, bytes, width, height, histogram: payload.histogram });
            }
            let _ = pending.reply.send(Ok(descriptor));
        }
        completed
    }

    fn requeue_full_after_interaction(&mut self, session: &mut Session, slot: SlotId) {
        let Some(previous) = self.pending.remove(&slot) else { return };
        let request = previous.request;
        let reply = previous.reply;
        let photo = PhotoId(request.photo_id);
        let Some(mut job) = session.render_job(photo, request.width as usize, request.height as usize, request.before, true) else {
            let _ = reply.send(Err("photo is unavailable".to_string()));
            return;
        };
        if !request.before
            && self.is_loupe_slot(&request.slot)
            && let Some(loupe) = session.loupe_job(photo, request.width as usize, request.height as usize, true)
        {
            job = loupe;
        }
        let key = job.key;
        let stages = self.stages.entry(slot).or_default().clone();
        let _ = self.submit(slot, request, key, job.with_stages(stages), false, reply);
    }

    pub fn cancel(&mut self, slot: &str) -> bool {
        let slots = [self.slots.get(slot), self.quick_slots.get(slot)];
        let mut cancelled = false;
        for slot_id in slots.into_iter().flatten() {
            if let Some(pending) = self.pending.remove(slot_id) {
                let _ = pending.reply.send(Err("preview cancelled".to_string()));
                cancelled = true;
            }
        }
        if cancelled {
            self.drop_queued();
        }
        cancelled
    }

    /// Finish every outstanding IPC request before owner teardown. Running jobs are bounded by
    /// [`JobPool::shutdown`]; GPU teardown separately waits for driver work.
    pub fn cancel_all(&mut self) -> usize {
        let pending = std::mem::take(&mut self.pending);
        let count = pending.len();
        for (_, request) in pending {
            let _ = request.reply.send(Err("preview cancelled".to_string()));
        }
        self.drop_queued();
        count
    }

    /// Stop preview workers, waiting up to `timeout` for jobs already running.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        let _ = self.cancel_all();
        self.pool.shutdown(timeout)
    }

    pub fn retry(&mut self, slot: &str) {
        let slots = [self.slots.get(slot), self.quick_slots.get(slot)];
        self.failed.retain(|(candidate, _), _| !slots.iter().flatten().any(|slot_id| *slot_id == candidate));
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn submit(
        &mut self,
        slot: SlotId,
        request: PreviewRequest,
        key: u64,
        job: RenderJob,
        interactive: bool,
        reply: std::sync::mpsc::Sender<Result<PreviewDescriptor, String>>,
    ) -> bool {
        let priority = priority_for(&request.slot, interactive);
        let ticket = self.ticket();
        let view_generation = request.view_generation;
        let sequence = request.sequence;
        let submission = match self.pool.submit(
            slot,
            key,
            priority,
            Box::new(move || {
                let result = job.run();
                let payload = encode_result(&result);
                JobOutput { ticket, view_generation, sequence, result, payload }
            }),
        ) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = reply.send(Err(error));
                return false;
            }
        };
        let forced_draft = forced_draft(request.quality, interactive);
        self.pending.insert(slot, Pending { ticket, submission, request, key, priority, forced_draft, quick: false, reply });
        true
    }

    fn submit_quick(
        &mut self,
        slot: SlotId,
        request: PreviewRequest,
        key: u64,
        job: QuickJob,
        interactive: bool,
        reply: std::sync::mpsc::Sender<Result<PreviewDescriptor, String>>,
    ) -> bool {
        let priority = priority_for(&request.slot, interactive).saturating_add(1);
        let ticket = self.ticket();
        let view_generation = request.view_generation;
        let sequence = request.sequence;
        let submission = match self.pool.submit(
            slot,
            key,
            priority,
            Box::new(move || {
                let result = job.run();
                let payload = encode_result(&result);
                JobOutput { ticket, view_generation, sequence, result, payload }
            }),
        ) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = reply.send(Err(error));
                return false;
            }
        };
        self.pending.insert(slot, Pending { ticket, submission, request, key, priority, forced_draft: false, quick: true, reply });
        true
    }

    fn ticket(&mut self) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        ticket
    }

    fn slot_id(&mut self, name: &str) -> SlotId {
        if let Some(slot) = self.slots.get(name) {
            return *slot;
        }
        if self.slots.len() >= MAX_SLOTS
            && let Some(victim) = self.slots.iter().find(|(_, slot)| !self.pending.contains_key(slot)).map(|(name, slot)| (name.clone(), *slot))
        {
            self.slots.remove(&victim.0);
            self.stages.remove(&victim.1);
            self.failed.retain(|(slot, _), _| *slot != victim.1);
            self.drop_queued();
        }
        let slot = SlotId(self.next_slot);
        self.next_slot = self.next_slot.wrapping_add(1).max(1);
        self.slots.insert(name.to_owned(), slot);
        slot
    }

    fn quick_slot_id(&mut self, name: &str) -> SlotId {
        if let Some(slot) = self.quick_slots.get(name) {
            return *slot;
        }
        if self.quick_slots.len() >= MAX_SLOTS
            && let Some(victim) = self.quick_slots.iter().find(|(_, slot)| !self.pending.contains_key(slot)).map(|(name, slot)| (name.clone(), *slot))
        {
            self.quick_slots.remove(&victim.0);
            self.failed.retain(|(slot, _), _| *slot != victim.1);
            self.drop_queued();
        }
        let slot = SlotId(self.next_slot);
        self.next_slot = self.next_slot.wrapping_add(1).max(1);
        self.quick_slots.insert(name.to_owned(), slot);
        slot
    }

    fn is_loupe_slot(&self, slot: &str) -> bool {
        matches!(slot, "main" | "preview" | "before" | "compare-a" | "compare-b")
    }

    fn is_thumb_slot(&self, slot: &str) -> bool {
        slot.starts_with("thumb:") || slot.starts_with("filmstrip:") || slot.starts_with("grid-") || slot.starts_with("filmstrip-")
    }

    fn cached_thumb(&mut self, photo: u64, key: u64, request: &PreviewRequest) -> Option<PreviewDescriptor> {
        let entries = self.thumbs.get_mut(&photo)?;
        let index = entries.iter().position(|entry| entry.key == key && entry.width == request.width && entry.height == request.height)?;
        let (bytes, width, height, histogram) = {
            let entry = entries.get(index)?;
            (entry.bytes.clone(), entry.width, entry.height, entry.histogram.clone())
        };
        // Cache entries own shared bytes, while every response receives its own opaque lease.
        let handle = self.store.insert(bytes, "image/png").ok()?;
        Some(PreviewDescriptor {
            handle,
            photo_id: request.photo_id,
            slot: request.slot.clone(),
            view_generation: request.view_generation,
            sequence: request.sequence,
            key: format!("{:016x}", key),
            width,
            height,
            histogram,
            render_ms: 0.0,
            encoding: "png",
            provisional: None,
            source: None,
        })
    }

    fn cache_thumb(&mut self, photo: u64, entry: CachedThumb) {
        let entry_bytes = entry.bytes.len();
        if entry_bytes > MAX_THUMB_BYTES {
            return;
        }
        let current_thumb_bytes = self.thumb_bytes;
        let entries = self.thumbs.entry(photo).or_default();
        if let Some(old) = entries.iter_mut().find(|old| old.key == entry.key) {
            let old_bytes = old.bytes.len();
            if current_thumb_bytes.saturating_sub(old_bytes).saturating_add(entry_bytes) > MAX_THUMB_BYTES {
                return;
            }
            *old = entry;
            self.thumb_bytes = self.thumb_bytes.saturating_sub(old_bytes).saturating_add(entry_bytes);
            return;
        }
        entries.push(entry);
        self.thumb_count += 1;
        self.thumb_bytes = self.thumb_bytes.saturating_add(entry_bytes);
        while entries.len() > MAX_THUMBS_PER_PHOTO {
            if let Some(removed) = entries.first().map(|item| item.bytes.len()) {
                entries.remove(0);
                self.thumb_bytes = self.thumb_bytes.saturating_sub(removed);
            }
            self.thumb_count = self.thumb_count.saturating_sub(1);
        }
        while self.thumb_count > MAX_THUMBS || self.thumb_bytes > MAX_THUMB_BYTES {
            let Some(photo_id) = self.thumbs.keys().next().copied() else { break };
            let remove_photo = if let Some(items) = self.thumbs.get_mut(&photo_id) {
                let removed = items.pop();
                if let Some(removed) = removed {
                    self.thumb_count = self.thumb_count.saturating_sub(1);
                    self.thumb_bytes = self.thumb_bytes.saturating_sub(removed.bytes.len());
                }
                items.is_empty()
            } else {
                false
            };
            if remove_photo {
                self.thumbs.remove(&photo_id);
            }
        }
    }

    fn apply_pressure(&mut self) {
        while self.pending.len() > MAX_PENDING {
            let Some((slot, _)) = self
                .pending
                .iter()
                .min_by_key(|(_, pending)| (pending.priority, pending.request.sequence))
                .map(|(slot, pending)| (*slot, pending.priority))
            else {
                break;
            };
            if let Some(pending) = self.pending.remove(&slot) {
                let _ = pending.reply.send(Err("preview queue pressure".to_string()));
            }
        }
        self.drop_queued();
    }

    fn drop_queued(&self) {
        let pending = &self.pending;
        let _ = self.pool.reprioritize(|slot, priority| pending.contains_key(slot).then_some(priority));
    }

    /// Interactive loupe work must not wait behind queued thumbnails or prefetches. Running
    /// workers finish normally; only queued background jobs are removed and their callers are
    /// released so they can retry after the interaction ends.
    fn drop_background_queued(&mut self) {
        let pending = &self.pending;
        let dropped = self.pool.reprioritize(
            |slot, priority| {
                if priority < INTERACTIVE_PRIORITY { None } else { pending.contains_key(slot).then_some(priority) }
            },
        );
        for slot in dropped {
            // A queue entry can be stale by the time it is inspected. Release only a matching
            // background pending request; never remove a newer interactive request for same slot.
            let is_background = self.pending.get(&slot).is_some_and(|pending| pending.priority < INTERACTIVE_PRIORITY);
            if is_background && let Some(pending) = self.pending.remove(&slot) {
                // Reuse stale-request handling in web clients: they retain the last decoded
                // pixels and retry briefly after the interaction settles.
                let _ = pending.reply.send(Err("preview superseded".to_string()));
            }
        }
    }
}

fn encode_result(result: &RenderResult) -> Result<EncodedPreview, String> {
    let rendered = result.rendered.as_ref().map_err(|error| error.clone())?;
    let bytes = encode_png(&rendered.image)?;
    let histogram = serde_json::to_value(&rendered.histogram).map_err(|error| format!("preview histogram: {error}"))?;
    let width = u32::try_from(rendered.image.width).map_err(|_| "preview width exceeds transport limit".to_string())?;
    let height = u32::try_from(rendered.image.height).map_err(|_| "preview height exceeds transport limit".to_string())?;
    Ok(EncodedPreview { bytes, width, height, histogram })
}

fn validate_request(request: &PreviewRequest) -> Result<(), String> {
    if request.photo_id > MAX_SEQUENCE || request.view_generation > MAX_SEQUENCE || request.sequence > MAX_SEQUENCE {
        return Err("preview identifier exceeds safe integer range".to_string());
    }
    if request.slot.is_empty()
        || request.slot.len() > MAX_SLOT_BYTES
        || !request.slot.is_ascii()
        || request.slot.bytes().any(|byte| byte == b'/' || byte == b'\\')
    {
        return Err("preview slot is invalid".to_string());
    }
    if request.width == 0 || request.height == 0 || request.width > MAX_DIMENSION || request.height > MAX_DIMENSION {
        return Err("preview dimensions are out of bounds".to_string());
    }
    if u64::from(request.width) * u64::from(request.height) > MAX_PIXELS {
        return Err("preview pixel budget exceeded".to_string());
    }
    Ok(())
}

fn priority(slot: &str) -> u32 {
    if matches!(slot, "main" | "preview" | "before" | "compare-a" | "compare-b") {
        return 100;
    }
    if slot.starts_with("thumb:") || slot.starts_with("filmstrip:") || slot.starts_with("grid-") || slot.starts_with("filmstrip-") {
        return 50;
    }
    if slot.starts_with("prefetch:") {
        return 10;
    }
    25
}

fn priority_for(slot: &str, interactive: bool) -> u32 {
    if interactive && matches!(slot, "main" | "preview" | "before" | "compare-a" | "compare-b") { INTERACTIVE_PRIORITY } else { priority(slot) }
}

fn uses_draft_quality(quality: PreviewQuality, interactive: bool) -> bool {
    interactive || quality == PreviewQuality::Draft
}

fn forced_draft(quality: PreviewQuality, interactive: bool) -> bool {
    interactive && quality == PreviewQuality::Full
}

fn publishable(pending: &Pending, result_ticket: u64, result_generation: u64, result_sequence: u64, result_key: u64, result_photo: PhotoId) -> bool {
    pending.ticket == result_ticket
        && pending.request.view_generation == result_generation
        && pending.request.sequence == result_sequence
        && pending.key == result_key
        && PhotoId(pending.request.photo_id) == result_photo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bounds_reject_untrusted_dimensions_and_slots() {
        let mut request = PreviewRequest {
            photo_id: 1,
            slot: "main".into(),
            view_generation: 1,
            width: 1,
            height: 1,
            quality: PreviewQuality::Full,
            before: false,
            sequence: 1,
        };
        assert!(validate_request(&request).is_ok());
        request.width = MAX_DIMENSION + 1;
        assert!(validate_request(&request).is_err());
        request.width = 1;
        request.slot = "/tmp/photo.png".into();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn priorities_keep_interactive_work_ahead_of_prefetch() {
        assert!(priority("main") > priority("thumb:1"));
        assert!(priority("thumb:1") > priority("prefetch:1"));
        assert!(priority_for("main", true) > priority("main"));
        assert_eq!(priority_for("thumb:1", true), priority("thumb:1"));
    }

    #[test]
    fn active_interactions_force_draft_quality_even_for_full_requests() {
        assert!(uses_draft_quality(PreviewQuality::Full, true));
        assert!(uses_draft_quality(PreviewQuality::Draft, true));
        assert!(!uses_draft_quality(PreviewQuality::Full, false));
        assert!(uses_draft_quality(PreviewQuality::Draft, false));
        assert!(forced_draft(PreviewQuality::Full, true));
        assert!(!forced_draft(PreviewQuality::Draft, true));
        assert!(!forced_draft(PreviewQuality::Full, false));
    }

    #[test]
    fn draft_quality_keeps_requested_dimensions_but_changes_cache_key() {
        let mut session = Session::with_demo();
        let photo = session.active().unwrap();
        let full = session.loupe_job(photo, 640, 480, true).unwrap();
        let draft = full.clone().draft();
        assert_eq!(draft.request.max_w, full.request.max_w);
        assert_eq!(draft.request.max_h, full.request.max_h);
        assert_ne!(draft.key, full.key);
        assert_ne!(draft.request.quality, full.request.quality);
    }

    #[test]
    fn full_request_after_unchanged_interaction_does_not_stay_draft() {
        let mut session = Session::with_demo();
        let photo = session.active().unwrap();
        let (generation, _) = session.visible_shared();
        let full_key = session.loupe_job(photo, 320, 240, true).unwrap().key;
        let draft_key = session.loupe_job(photo, 320, 240, true).unwrap().draft().key;
        assert_ne!(draft_key, full_key);
        session.begin_interaction("Exposure").unwrap();

        let mut renderer = Renderer::new(PreviewStore::new(8, 1024, std::time::Duration::from_secs(30)));
        let (reply, _) = std::sync::mpsc::channel();
        renderer
            .request(
                &mut session,
                PreviewRequest {
                    photo_id: photo.0,
                    slot: "main".into(),
                    view_generation: generation,
                    width: 320,
                    height: 240,
                    quality: PreviewQuality::Full,
                    before: false,
                    sequence: 1,
                },
                reply,
            )
            .unwrap();
        let slot = renderer.slots.get("main").copied().unwrap();
        assert_eq!(renderer.pending.get(&slot).unwrap().key, draft_key);

        session.end_interaction().unwrap();
        let (after_generation, _) = session.visible_shared();
        assert_eq!(after_generation, generation);
        let (reply, _) = std::sync::mpsc::channel();
        renderer
            .request(
                &mut session,
                PreviewRequest {
                    photo_id: photo.0,
                    slot: "main".into(),
                    view_generation: after_generation,
                    width: 320,
                    height: 240,
                    quality: PreviewQuality::Full,
                    before: false,
                    sequence: 2,
                },
                reply,
            )
            .unwrap();
        assert_eq!(renderer.pending.get(&slot).unwrap().key, full_key);
    }

    #[test]
    fn draft_completion_after_interaction_end_is_requeued_as_full() {
        let mut session = Session::with_demo();
        let photo = session.active().unwrap();
        let (generation, _) = session.visible_shared();
        let full_key = session.loupe_job(photo, 320, 240, true).unwrap().key;
        session.begin_interaction("Exposure").unwrap();

        // Hold one worker so interaction can end while draft work is still in flight.
        let mut renderer = Renderer::new(PreviewStore::new(8, 4 * 1024 * 1024, std::time::Duration::from_secs(30)));
        renderer.pool = JobPool::new(1);
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let gate_worker = gate.clone();
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let _ = renderer.pool.submit(
            SlotId(99),
            0,
            1000,
            Box::new(move || {
                let _ = started_sender.send(());
                let (lock, cv) = &*gate_worker;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
                JobOutput {
                    ticket: 0,
                    view_generation: 0,
                    sequence: 0,
                    result: RenderResult {
                        request_id: 0,
                        source_key: None,
                        photo: PhotoId(0),
                        level: lightcraft_engine::media::SourceLevel::Thumb,
                        key: 0,
                        rendered: Err("gate".into()),
                        loaded: None,
                        quick: None,
                        gpu_fallback: None,
                        display: None,
                    },
                    payload: Err("gate".into()),
                }
            }),
        );
        assert!(started_receiver.recv_timeout(std::time::Duration::from_secs(1)).is_ok());
        let (gate_reply, _) = std::sync::mpsc::channel();
        renderer.pending.insert(
            SlotId(99),
            Pending {
                ticket: 0,
                submission: 0,
                request: PreviewRequest {
                    photo_id: 0,
                    slot: "gate".into(),
                    view_generation: generation,
                    width: 1,
                    height: 1,
                    quality: PreviewQuality::Full,
                    before: false,
                    sequence: 0,
                },
                key: 0,
                priority: 1000,
                forced_draft: false,
                quick: false,
                reply: gate_reply,
            },
        );

        let (reply, receiver) = std::sync::mpsc::channel();
        renderer
            .request(
                &mut session,
                PreviewRequest {
                    photo_id: photo.0,
                    slot: "main".into(),
                    view_generation: generation,
                    width: 320,
                    height: 240,
                    quality: PreviewQuality::Full,
                    before: false,
                    sequence: 1,
                },
                reply,
            )
            .unwrap();
        session.end_interaction().unwrap();
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_one();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut response = None;
        while response.is_none() && std::time::Instant::now() < deadline {
            renderer.poll(&mut session);
            if let Ok(value) = receiver.try_recv() {
                response = Some(value);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let response = response.unwrap().unwrap();
        assert_eq!(response.key, format!("{full_key:016x}"));
    }

    #[test]
    fn cached_thumb_mints_independent_handles_for_each_response() {
        let store = PreviewStore::new(8, 1024, std::time::Duration::from_secs(30));
        let mut renderer = Renderer::new(store.clone());
        renderer.cache_thumb(7, CachedThumb { key: 11, bytes: Arc::<[u8]>::from(vec![1, 2, 3]), width: 16, height: 16, histogram: Value::Null });
        let request = PreviewRequest {
            photo_id: 7,
            slot: "thumb:one".into(),
            view_generation: 1,
            width: 16,
            height: 16,
            quality: PreviewQuality::Draft,
            before: false,
            sequence: 1,
        };
        let first = renderer.cached_thumb(7, 11, &request).expect("cached thumb should mint first lease");
        let second = renderer.cached_thumb(7, 11, &PreviewRequest { sequence: 2, ..request }).expect("cached thumb should mint second lease");
        assert_ne!(first.handle, second.handle);
        assert!(store.acknowledge(&first.handle));
        assert!(store.get(&second.handle).is_some());
    }

    #[test]
    fn quick_and_full_same_generation_publish_and_release_independent_handles() {
        let store = PreviewStore::new(8, 8 * 1024 * 1024, std::time::Duration::from_secs(30));
        let mut renderer = Renderer::new(store.clone());
        let mut session = Session::with_demo();
        let photo = session.active().expect("demo session has an active photo");
        let (generation, _) = session.visible_shared();
        let request = PreviewRequest {
            photo_id: photo.0,
            slot: "main".into(),
            view_generation: generation,
            width: 320,
            height: 240,
            quality: PreviewQuality::Full,
            before: false,
            sequence: 7,
        };
        let (quick_reply, quick_results) = std::sync::mpsc::channel();
        renderer.request_quick(&mut session, request.clone(), quick_reply).unwrap();
        let (full_reply, full_results) = std::sync::mpsc::channel();
        renderer.request(&mut session, request, full_reply).unwrap();
        assert_ne!(renderer.slots.get("main"), renderer.quick_slots.get("main"));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut quick = None;
        let mut full = None;
        while (quick.is_none() || full.is_none()) && std::time::Instant::now() < deadline {
            renderer.poll(&mut session);
            quick = quick.or_else(|| quick_results.try_recv().ok().and_then(Result::ok));
            full = full.or_else(|| full_results.try_recv().ok().and_then(Result::ok));
            if quick.is_none() || full.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let quick = quick.expect("quick preview should publish");
        let full = full.expect("full preview should publish");
        assert_eq!(quick.provisional, Some(true));
        assert_eq!(full.provisional, None);
        assert_ne!(quick.handle, full.handle);
        assert!(store.get(&quick.handle).is_some());
        assert!(store.get(&full.handle).is_some());
        assert!(store.acknowledge(&quick.handle));
        assert!(store.acknowledge(&full.handle));
        assert!(store.is_empty());
    }

    #[test]
    fn stale_result_never_passes_presentation_gate() {
        let (reply, _) = std::sync::mpsc::channel();
        let pending = Pending {
            ticket: 8,
            submission: 8,
            request: PreviewRequest {
                photo_id: 42,
                slot: "main".into(),
                view_generation: 3,
                width: 1,
                height: 1,
                quality: PreviewQuality::Full,
                before: false,
                sequence: 9,
            },
            key: 17,
            priority: 100,
            forced_draft: false,
            quick: false,
            reply,
        };
        assert!(publishable(&pending, 8, 3, 9, 17, PhotoId(42)));
        assert!(!publishable(&pending, 9, 3, 9, 17, PhotoId(42)));
        assert!(!publishable(&pending, 8, 4, 9, 17, PhotoId(42)));
        assert!(!publishable(&pending, 8, 3, 10, 17, PhotoId(42)));
        assert!(!publishable(&pending, 8, 3, 9, 17, PhotoId(43)));
    }
}
