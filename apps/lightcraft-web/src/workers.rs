//! Render workers: dedicated Web Workers, each running a second instance of this wasm module
//! (`worker.js` → [`worker_main`]). The main thread compiles the module once and posts the
//! compiled `WebAssembly.Module` to every worker, so a worker starts in a few milliseconds.
//!
//! This doesn't need wasm threads (shared memory, `+atomics`, a nightly `build-std`), only
//! module workers: the main thread posts a [`WireJob`] as JSON, the worker renders it and
//! transfers the RGBA bytes back (no copy across the boundary).
//!
//! Exports use the same workers (#192): the main thread posts a [`WireExport`] (and the original's
//! bytes when the page has no storage to read them from), the worker decodes, renders and encodes,
//! and transfers the file back. An Original copy or a DNG goes as a [`WireFileExport`]: the worker
//! reads the original from storage (asynchronously, off the main thread), makes the file and sends
//! it back with its XMP sidecar. [`Workers::offer_export`] is the main side; `exports.rs` runs a
//! batch on top of it.
//!
//! Auto Settings use them too (#192): the main thread posts a [`WireAuto`] (the photo's source,
//! its settings and facts), the worker decodes the thumbnail-level source and measures with the
//! engine's pure `measure_auto`, and the values come back as JSON for the main thread to apply as
//! one undo step. [`Workers::offer_auto`] is the main side; `auto.rs` drives it.
//!
//! Main side: [`Workers`] implements the UI's [`RenderOffload`]: one job per worker at a time,
//! with affinity (a photo goes back to the worker that already decoded it), the rendered-
//! thumbnail memory cache checked before dispatch, and the thumbnail storage index
//! ([`ThumbIndex`]) kept and pruned here. If no worker comes up, jobs run inline.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Array, Object, Reflect, Uint8Array, Uint32Array};
use lightcraft_engine::catalog::PhotoId;
use lightcraft_engine::media::{RenderJob, RenderResult, SourceLevel};
use lightcraft_engine::pipeline::Rendered;
use lightcraft_preview::{Hash128, PreviewCache};
use lightcraft_raster::{Histogram, Rgba8};
use lightcraft_ui_egui::render::{RenderOffload, Slot};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::backend::Backend;
use crate::wire::{CacheWatch, ThumbIndex, WireAuto, WireExport, WireFileExport, WireJob, WorkerCore, WorkerSlots, thumb_storage_key};

/// Storage key of the thumbnail index.
pub const THUMB_INDEX: &str = "thumbs/index.json";

fn set(o: &Object, k: &str, v: &JsValue) {
    let _ = Reflect::set(o, &k.into(), v);
}

fn get(o: &JsValue, k: &str) -> JsValue {
    Reflect::get(o, &k.into()).unwrap_or(JsValue::UNDEFINED)
}

fn now() -> f64 {
    let perf = get(&js_sys::global(), "performance");
    let f = get(&perf, "now");
    f.dyn_ref::<js_sys::Function>().and_then(|f| f.call0(&perf).ok()).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

// ---------------------------------------------------------------------------------------------
// worker side

thread_local! {
    static CORE: RefCell<WorkerCore> = RefCell::new(WorkerCore::default());
}

/// Entry point of a render worker (called by `worker.js` after instantiating the module).
/// `store` is the backend kind the page uses ("opfs", "idb" or "memory").
#[wasm_bindgen]
pub fn worker_main(store: String) {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    let scope: web_sys::DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    // The main thread sends jobs only after "ready", i.e. once storage is open, and one at a time.
    wasm_bindgen_futures::spawn_local(async move {
        let backend = match store.as_str() {
            "memory" => None,
            _ => Backend::open(store == "idb").await.map_err(|e| log::warn!("render worker: no storage ({e})")).ok(),
        };
        let s = scope.clone();
        let onmessage = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            let (s, b) = (s.clone(), backend.clone());
            wasm_bindgen_futures::spawn_local(async move { handle(&s, b, e.data()).await });
        });
        scope.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
        let ready = Object::new();
        set(&ready, "ready", &true.into());
        let _ = scope.post_message(&ready);
    });
}

/// The original's bytes for a job: the ones the page sent, else the stored file (read here, asynchronously).
async fn original_bytes(msg: &JsValue, hash: Option<String>, backend: Option<&Backend>) -> Result<Option<Vec<u8>>, String> {
    let sent = get(msg, "original");
    if sent.is_undefined() || sent.is_null() {
        return match (hash, backend) {
            (Some(hash), Some(b)) => b.read(&crate::store::storage_key(&hash)).await,
            _ => Ok(None),
        };
    }
    Ok(Some(Uint8Array::new(&sent).to_vec()))
}

/// Run one export message (a render, or an Original copy or DNG) and post the file (or the error) back.
async fn handle_export(scope: &web_sys::DedicatedWorkerGlobalScope, backend: Option<Backend>, msg: JsValue) {
    let t0 = now();
    let out = Object::new();
    set(&out, "id", &get(&msg, "id"));
    set(&out, "kind", &"export".into());
    let result: Result<lightcraft_engine::export::Exported, String> = async {
        if let Some(text) = get(&msg, "file").as_string() {
            let job: WireFileExport = serde_json::from_str(&text).map_err(|e| format!("bad export job: {e}"))?;
            let original = original_bytes(&msg, job.original_hash(), backend.as_ref()).await?;
            return CORE.with(|c| c.borrow_mut().export_file(&job, original));
        }
        let text = get(&msg, "export").as_string().ok_or("no export job")?;
        let job: WireExport = serde_json::from_str(&text).map_err(|e| format!("bad export job: {e}"))?;
        let original = original_bytes(&msg, job.original_hash(), backend.as_ref()).await?;
        CORE.with(|c| c.borrow_mut().export(&job, original.as_deref()))
    }
    .await;
    let transfer = Array::new();
    match result {
        Ok(file) => {
            let bytes = Uint8Array::from(file.bytes.as_slice());
            set(&out, "ok", &true.into());
            set(&out, "w", &(file.width as u32).into());
            set(&out, "h", &(file.height as u32).into());
            set(&out, "bytes", &bytes);
            transfer.push(&bytes.buffer());
            // files written beside it: `{ext, bytes}` each
            let sidecars = Array::new();
            for (ext, data) in &file.sidecars {
                let (item, data) = (Object::new(), Uint8Array::from(data.as_slice()));
                set(&item, "ext", &(*ext).into());
                set(&item, "bytes", &data);
                transfer.push(&data.buffer());
                sidecars.push(&item);
            }
            set(&out, "sidecars", &sidecars);
        }
        Err(e) => {
            set(&out, "ok", &false.into());
            set(&out, "err", &e.into());
        }
    }
    set(&out, "ms", &(now() - t0).into());
    if let Err(e) = scope.post_message_with_transfer(&out, &transfer) {
        log::error!("render worker: post: {e:?}");
    }
}

/// Run one Auto measure message and post the values (or the error) back.
async fn handle_auto(scope: &web_sys::DedicatedWorkerGlobalScope, backend: Option<Backend>, msg: JsValue) {
    let t0 = now();
    let out = Object::new();
    set(&out, "id", &get(&msg, "id"));
    set(&out, "kind", &"auto".into());
    let result: Result<String, String> = async {
        let text = get(&msg, "auto").as_string().ok_or("no auto job")?;
        let job: WireAuto = serde_json::from_str(&text).map_err(|e| format!("bad auto job: {e}"))?;
        let sent = get(&msg, "original");
        let original = if sent.is_undefined() || sent.is_null() {
            match (CORE.with(|c| c.borrow_mut().needs_original(&job.job)), &backend) {
                (Some(hash), Some(b)) => b.read(&crate::store::storage_key(&hash)).await?,
                _ => None,
            }
        } else {
            Some(Uint8Array::new(&sent).to_vec())
        };
        let values = CORE.with(|c| c.borrow_mut().measure(&job, original.as_deref()))?;
        serde_json::to_string(&values).map_err(|e| format!("Auto values: {e}"))
    }
    .await;
    match result {
        Ok(values) => {
            set(&out, "ok", &true.into());
            set(&out, "values", &values.into());
        }
        Err(e) => {
            set(&out, "ok", &false.into());
            set(&out, "err", &e.into());
        }
    }
    set(&out, "ms", &(now() - t0).into());
    if let Err(e) = scope.post_message(&out) {
        log::error!("render worker: post: {e:?}");
    }
}

/// Run one job message and post the result.
async fn handle(scope: &web_sys::DedicatedWorkerGlobalScope, backend: Option<Backend>, msg: JsValue) {
    if get(&msg, "export").is_string() || get(&msg, "file").is_string() {
        return handle_export(scope, backend, msg).await;
    }
    if get(&msg, "auto").is_string() {
        return handle_auto(scope, backend, msg).await;
    }
    let id = get(&msg, "id");
    let t0 = now();
    let out = Object::new();
    set(&out, "id", &id);
    let job: Result<WireJob, String> =
        get(&msg, "job").as_string().ok_or("no job".to_string()).and_then(|j| serde_json::from_str(&j).map_err(|e| e.to_string()));
    let mut transfer = Array::new();
    let result: Result<(Rendered, u32), String> = async {
        let job = job?;
        let mut cache_miss = false;
        // cached thumbnail?
        if job.thumb_cached
            && let (Some(b), Some(key)) = (&backend, job.thumb_key())
        {
            match b.read(&key).await.ok().flatten().and_then(|bytes| lightcraft_preview::decode_jpeg(&bytes)) {
                Some(image) => {
                    let histogram = Histogram::of_srgb8(&image);
                    return Ok((Rendered { image, histogram, deep: None }, 0));
                }
                None => cache_miss = true,
            }
        }
        set(&out, "cacheMiss", &cache_miss.into());
        let original = match CORE.with(|c| c.borrow_mut().needs_original(&job)) {
            Some(hash) => match &backend {
                Some(b) => b.read(&crate::store::storage_key(&hash)).await?,
                None => None,
            },
            None => None,
        };
        let rendered = CORE.with(|c| c.borrow_mut().render(&job, original.as_deref()))?;
        let mut stored = 0;
        if let (Some(b), Some(key)) = (&backend, job.thumb_key())
            && let Some(jpeg) = lightcraft_preview::encode_jpeg(&rendered.image)
        {
            match b.write(&key, &jpeg).await {
                Ok(()) => stored = jpeg.len() as u32,
                Err(e) => log::warn!("thumbnail cache write: {e}"),
            }
        }
        Ok((rendered, stored))
    }
    .await;
    match result {
        Ok((r, stored)) => {
            let rgba = Uint8Array::from(r.image.as_bytes().as_ref());
            let h = &r.histogram;
            let hist = Uint32Array::from(h.to_words().as_slice());
            set(&out, "ok", &true.into());
            set(&out, "w", &(r.image.width as u32).into());
            set(&out, "h", &(r.image.height as u32).into());
            set(&out, "rgba", &rgba);
            set(&out, "hist", &hist);
            set(&out, "stored", &stored.into());
            transfer = Array::of2(&rgba.buffer(), &hist.buffer());
        }
        Err(e) => {
            set(&out, "ok", &false.into());
            set(&out, "err", &e.into());
        }
    }
    set(&out, "ms", &(now() - t0).into());
    if let Err(e) = scope.post_message_with_transfer(&out, &transfer) {
        log::error!("render worker: post: {e:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// main side

struct Busy {
    disk_key: Option<String>,
    namespace: u64,
    request_id: u64,
    cache_generation: u64,
    id: u32,
    slot: Slot,
    photo: PhotoId,
    level: SourceLevel,
    key: u64,
    cache: Option<(Arc<PreviewCache>, Hash128)>,
}

struct W {
    worker: web_sys::Worker,
    ready: bool,
    dead: bool,
    busy: Option<Busy>,
    /// The export this worker is making: its message id and who to tell.
    export: Option<(u32, ExportDone)>,
    /// The Auto measure this worker is making: its message id and who to tell.
    auto: Option<(u32, AutoDone)>,
}

impl W {
    fn free(&self) -> bool {
        self.ready && !self.dead && self.busy.is_none() && self.export.is_none() && self.auto.is_none()
    }
}

/// An export a worker finished: the encoded file.
pub struct ExportFile {
    pub bytes: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Files written beside it, named like it with another extension (the XMP of an Original copy).
    pub sidecars: Vec<(&'static str, Vec<u8>)>,
}

/// What a worker is asked to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    /// A rendered format: a [`WireExport`].
    Render,
    /// An Original copy or a DNG: a [`WireFileExport`].
    File,
}

/// Why an export did not come back.
pub struct ExportFailure {
    pub message: String,
    /// The worker itself failed (not this photo): the batch makes the photo itself instead.
    pub worker_lost: bool,
}

/// Told once, on the main thread, when the worker answers (or fails).
pub type ExportDone = Box<dyn FnOnce(Result<ExportFile, ExportFailure>)>;

/// The answer to [`Workers::offer_export`]; `Busy` and `NoWorkers` hand `done` back.
pub enum ExportOffer {
    Started,
    /// Every worker is busy for now: offer it again soon.
    Busy(ExportDone),
    /// No worker is left (none started, or all failed): make it on this thread.
    NoWorkers(ExportDone),
}

/// Why an Auto measure did not come back.
pub struct AutoFailure {
    pub message: String,
    /// The worker itself failed (not this photo).
    pub worker_lost: bool,
}

/// Told once, on the main thread, with the values (`RegionAuto` as JSON) or why there are none.
pub type AutoDone = Box<dyn FnOnce(Result<String, AutoFailure>)>;

/// The answer to [`Workers::offer_auto`]; `Busy` and `NoWorkers` hand `done` back.
pub enum AutoOffer {
    Started,
    /// Every worker is busy for now: offer it again soon.
    Busy(AutoDone),
    /// No worker is left (none started, or all failed).
    NoWorkers(AutoDone),
}

struct Inner {
    /// The rendered-thumbnail cache the stored thumbnails belong to.
    preview_generation: CacheWatch,
    workers: Vec<W>,
    done: Vec<(Slot, RenderResult, f64)>,
    next_id: u32,
    /// (photo, level) → worker that last decoded it.
    affinity: HashMap<(PhotoId, SourceLevel), usize>,
    index: ThumbIndex,
    /// The session's thumbnail cache budget in bytes. Kept here because the prune happens on a
    /// worker message, where the session is out of reach; the app refreshes it every frame.
    budget: u64,
    backend: Option<Backend>,
    ctx: egui::Context,
    /// Jobs finished by workers / inline (for diagnostics).
    remote_done: u64,
    inline_done: u64,
    inline_this_frame: bool,
}

impl Inner {
    fn observe_cache(&mut self, cache: &Arc<PreviewCache>, generation: u64) {
        let gone = self.preview_generation.observe(cache, generation, &mut self.index);
        if !gone.is_empty()
            && let Some(backend) = self.backend.clone()
        {
            wasm_bindgen_futures::spawn_local(async move {
                for key in gone {
                    let _ = backend.remove(&thumb_storage_key(&key)).await;
                }
            });
        }
    }
}

/// The main thread's handle on the render workers.
#[derive(Clone)]
pub struct Workers(Rc<RefCell<Inner>>);

impl Workers {
    /// Start `n` workers (none: every job runs inline). `store` is the backend kind for workers.
    /// `index` lists the stored thumbnails of `cache`, the session's rendered-thumbnail cache
    /// now: watching it from the start, a clear before the first render request still counts.
    pub fn start(
        n: usize,
        store: &str,
        backend: Option<Backend>,
        index: ThumbIndex,
        cache: &Arc<PreviewCache>,
        budget: u64,
        ctx: egui::Context,
    ) -> Workers {
        let w = Workers(Rc::new(RefCell::new(Inner {
            preview_generation: CacheWatch::new(cache),
            workers: Vec::new(),
            done: Vec::new(),
            next_id: 0,
            affinity: HashMap::new(),
            index,
            budget,
            backend,
            ctx,
            remote_done: 0,
            inline_done: 0,
            inline_this_frame: false,
        })));
        let module = get(&js_sys::global(), "lightcraftModule");
        if module.is_undefined() {
            log::warn!("render workers: no compiled module on the page (window.lightcraftModule); rendering on the main thread");
            return w;
        }
        let mut slots = WorkerSlots::default();
        for attempt in 0..n {
            // The callbacks name their worker by its place in `workers`; a failed attempt takes none.
            let i = slots.peek();
            let opts = web_sys::WorkerOptions::new();
            opts.set_type(web_sys::WorkerType::Module);
            opts.set_name(&format!("lightcraft-render-{attempt}"));
            let worker = match web_sys::Worker::new_with_options("./worker.js", &opts) {
                Ok(x) => x,
                Err(e) => {
                    log::warn!("render worker {attempt}: {e:?}");
                    continue;
                }
            };
            let me = w.clone();
            let onmessage = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| me.on_message(i, e.data()));
            worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget();
            let me = w.clone();
            let onerror = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| me.on_error(i, e));
            worker.set_onerror(Some(onerror.as_ref().unchecked_ref()));
            onerror.forget();
            let init = Object::new();
            set(&init, "module", &module);
            set(&init, "store", &store.into());
            if let Err(e) = worker.post_message(&init) {
                log::warn!("render worker {attempt}: {e:?}");
                // It never joins the list, so it must not answer to the index of the next worker.
                worker.set_onmessage(None);
                worker.set_onerror(None);
                worker.terminate();
                continue;
            }
            w.0.borrow_mut().workers.push(W { worker, ready: false, dead: false, busy: None, export: None, auto: None });
            slots.commit();
        }
        w
    }

    fn on_error(&self, i: usize, e: JsValue) {
        let msg = get(&e, "message").as_string().unwrap_or_else(|| format!("{e:?}"));
        log::error!("render worker {i} failed: {msg}");
        let mut lost = None;
        let mut lost_auto = None;
        let mut g = self.0.borrow_mut();
        if let Some(w) = g.workers.get_mut(i) {
            w.dead = true;
            w.worker.terminate();
            lost = w.export.take();
            lost_auto = w.auto.take();
            if let Some(b) = w.busy.take() {
                let r = RenderResult {
                    request_id: b.request_id,
                    source_key: None,
                    photo: b.photo,
                    level: b.level,
                    key: b.key,
                    rendered: Err(msg.clone()),
                    gpu_fallback: None,
                    loaded: None,
                    quick: None,
                    display: None,
                };
                g.done.push((b.slot, r, 0.0));
            }
        }
        g.ctx.request_repaint();
        drop(g);
        // outside the borrow: the batch answers by offering its next photo
        if let Some((_, done)) = lost {
            done(Err(ExportFailure { message: msg.clone(), worker_lost: true }));
        }
        if let Some((_, done)) = lost_auto {
            done(Err(AutoFailure { message: msg, worker_lost: true }));
        }
    }

    /// A worker's answer to an Auto measure.
    fn on_auto_message(&self, i: usize, data: JsValue) {
        let id = get(&data, "id").as_f64().unwrap_or(-1.0) as u32;
        let pending = {
            let mut g = self.0.borrow_mut();
            g.ctx.request_repaint();
            g.workers.get_mut(i).and_then(|w| w.auto.take_if(|(job, _)| *job == id))
        };
        let Some((_, done)) = pending else { return };
        let result = if get(&data, "ok").is_truthy() {
            Ok(get(&data, "values").as_string().unwrap_or_default())
        } else {
            let message = get(&data, "err").as_string().unwrap_or_else(|| "Auto failed in its worker".into());
            Err(AutoFailure { message, worker_lost: false })
        };
        self.0.borrow_mut().remote_done += 1;
        done(result);
    }

    /// Give a free worker one Auto measure: `text` is a [`WireAuto`] as JSON, `original` the file's
    /// bytes when the worker has no storage to read them from. Any free worker will do: it is short
    /// work on a thumbnail-sized source, and the user is waiting for it.
    pub fn offer_auto(&self, text: &str, original: Option<&[u8]>, done: AutoDone) -> AutoOffer {
        let mut g = self.0.borrow_mut();
        let g = &mut *g;
        if g.workers.iter().all(|w| w.dead) {
            return AutoOffer::NoWorkers(done);
        }
        let Some(i) = g.workers.iter().position(W::free) else { return AutoOffer::Busy(done) };
        g.next_id = g.next_id.wrapping_add(1);
        let id = g.next_id;
        let msg = Object::new();
        set(&msg, "id", &id.into());
        set(&msg, "auto", &text.into());
        let transfer = Array::new();
        if let Some(bytes) = original {
            let arr = Uint8Array::from(bytes);
            set(&msg, "original", &arr);
            transfer.push(&arr.buffer());
        }
        let Some(w) = g.workers.get_mut(i) else { return AutoOffer::Busy(done) };
        if let Err(e) = w.worker.post_message_with_transfer(&msg, &transfer) {
            log::warn!("render worker {i}: {e:?}");
            return AutoOffer::Busy(done);
        }
        w.auto = Some((id, done));
        AutoOffer::Started
    }

    /// A worker's answer to an export job.
    fn on_export_message(&self, i: usize, data: JsValue) {
        let id = get(&data, "id").as_f64().unwrap_or(-1.0) as u32;
        let pending = {
            let mut g = self.0.borrow_mut();
            g.ctx.request_repaint();
            g.workers.get_mut(i).and_then(|w| w.export.take_if(|(job, _)| *job == id))
        };
        let Some((_, done)) = pending else { return };
        let result = if get(&data, "ok").is_truthy() {
            let bytes = Uint8Array::new(&get(&data, "bytes")).to_vec();
            let (width, height) = (get(&data, "w").as_f64().unwrap_or(0.0) as usize, get(&data, "h").as_f64().unwrap_or(0.0) as usize);
            let listed = get(&data, "sidecars");
            let sidecars = if Array::is_array(&listed) {
                Array::from(&listed)
                    .iter()
                    .filter_map(|item| {
                        // (only the XMP exists; a name this build doesn't know is dropped, not guessed)
                        let ext = match get(&item, "ext").as_string().as_deref() {
                            Some("xmp") => "xmp",
                            other => {
                                log::warn!("export: a worker sent a sidecar of unknown kind {other:?}");
                                return None;
                            }
                        };
                        Some((ext, Uint8Array::new(&get(&item, "bytes")).to_vec()))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Ok(ExportFile { bytes, width, height, sidecars })
        } else {
            let message = get(&data, "err").as_string().unwrap_or_else(|| "the export failed in its worker".into());
            Err(ExportFailure { message, worker_lost: false })
        };
        self.0.borrow_mut().remote_done += 1;
        done(result);
    }

    /// Whether any worker is alive (or still starting).
    pub fn any_alive(&self) -> bool {
        self.0.borrow().workers.iter().any(|w| !w.dead)
    }

    /// Give a free worker one export: `text` is a [`WireExport`] or (`kind` [`ExportKind::File`])
    /// a [`WireFileExport`] as JSON, `original` the file's bytes when the worker has no storage to
    /// read them from (copied to the worker). With more than one worker the first is left to the
    /// views (their decoded sources live there), so a long export doesn't hold up the loupe.
    pub fn offer_export(&self, kind: ExportKind, text: &str, original: Option<&[u8]>, done: ExportDone) -> ExportOffer {
        let mut g = self.0.borrow_mut();
        let g = &mut *g;
        let alive: Vec<usize> = g.workers.iter().enumerate().filter(|(_, w)| !w.dead).map(|(i, _)| i).collect();
        if alive.is_empty() {
            return ExportOffer::NoWorkers(done);
        }
        let skip = if alive.len() > 1 { alive.first().copied() } else { None };
        let Some(i) = alive.into_iter().filter(|i| Some(*i) != skip).find(|i| g.workers.get(*i).is_some_and(W::free)) else {
            return ExportOffer::Busy(done);
        };
        g.next_id = g.next_id.wrapping_add(1);
        let id = g.next_id;
        let msg = Object::new();
        set(&msg, "id", &id.into());
        set(&msg, if kind == ExportKind::File { "file" } else { "export" }, &text.into());
        let transfer = Array::new();
        if let Some(bytes) = original {
            let arr = Uint8Array::from(bytes);
            set(&msg, "original", &arr);
            transfer.push(&arr.buffer());
        }
        let w = &mut g.workers[i];
        if let Err(e) = w.worker.post_message_with_transfer(&msg, &transfer) {
            log::warn!("render worker {i}: {e:?}");
            return ExportOffer::Busy(done);
        }
        w.export = Some((id, done));
        ExportOffer::Started
    }

    fn on_message(&self, i: usize, data: JsValue) {
        match get(&data, "kind").as_string().as_deref() {
            Some("export") => return self.on_export_message(i, data),
            Some("auto") => return self.on_auto_message(i, data),
            _ => {}
        }
        let mut g = self.0.borrow_mut();
        let g = &mut *g;
        g.ctx.request_repaint();
        let Some(w) = g.workers.get_mut(i) else { return };
        if get(&data, "ready").is_truthy() {
            w.ready = true;
            return;
        }
        let id = get(&data, "id").as_f64().unwrap_or(-1.0) as u32;
        let Some(b) = w.busy.take_if(|b| b.id == id) else { return };
        let ms = get(&data, "ms").as_f64().unwrap_or(0.0);
        let rendered = if get(&data, "ok").is_truthy() {
            let (wd, ht) = (get(&data, "w").as_f64().unwrap_or(0.0) as usize, get(&data, "h").as_f64().unwrap_or(0.0) as usize);
            let rgba = Uint8Array::new(&get(&data, "rgba")).to_vec();
            let hist = Uint32Array::new(&get(&data, "hist")).to_vec();
            match (Rgba8::from_bytes(wd, ht, &rgba), Histogram::from_words(&hist)) {
                (Some(image), Some(histogram)) => Ok(Rendered { image, histogram, deep: None }),
                _ => Err("render worker: bad image".to_string()),
            }
        } else {
            Err(get(&data, "err").as_string().unwrap_or_else(|| "render worker failed".into()))
        };
        if let Ok(r) = &rendered
            && let Some((cache, key)) = &b.cache
            && cache.generation() == b.cache_generation
            && b.namespace == g.index.namespace()
        {
            cache.put_at(b.cache_generation, *key, Arc::new(r.image.clone()));
            let hex = b.disk_key.clone().unwrap_or_else(|| key.to_string());
            let stored = get(&data, "stored").as_f64().unwrap_or(0.0) as u64;
            if get(&data, "cacheMiss").is_truthy() {
                g.index.remove(&hex);
            }
            if stored > 0 {
                g.index.insert(&hex, stored);
                // The session's own budget (`library.preferences ▸ cacheMb`), not the native
                // default: the browser accepted the setting, so it has to be the one enforced
                // (#245).
                let gone = g.index.prune(g.budget);
                if !gone.is_empty()
                    && let Some(be) = g.backend.clone()
                {
                    log::info!("thumbnail cache: pruning {} files", gone.len());
                    wasm_bindgen_futures::spawn_local(async move {
                        for k in gone {
                            let _ = be.remove(&thumb_storage_key(&k)).await;
                        }
                    });
                }
            }
        }
        g.affinity.insert((b.photo, b.level), i);
        // A cleared worker may finish writing after the initial deletion. Its old namespace
        // is never reused, so removing this file cannot delete a replacement's cache entry.
        if let Some((cache, _)) = &b.cache
            && (cache.generation() != b.cache_generation || b.namespace != g.index.namespace())
            && let (Some(backend), Some(key)) = (g.backend.clone(), b.disk_key.clone())
        {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = backend.remove(&thumb_storage_key(&key)).await;
            });
        }
        g.remote_done += 1;
        g.done.push((
            b.slot,
            RenderResult {
                request_id: b.request_id,
                source_key: None,
                photo: b.photo,
                level: b.level,
                key: b.key,
                rendered,
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
            },
            ms,
        ));
    }

    /// The thumbnail index as JSON if it changed since the last call (the host saves it).
    pub fn take_index_if_dirty(&self) -> Option<Vec<u8>> {
        let mut g = self.0.borrow_mut();
        if !g.index.dirty {
            return None;
        }
        g.index.dirty = false;
        Some(g.index.to_json())
    }

    /// (workers alive, workers ready, jobs done remotely, jobs done inline).
    pub fn stats(&self) -> (usize, usize, u64, u64) {
        let g = self.0.borrow();
        let alive = g.workers.iter().filter(|w| !w.dead).count();
        let ready = g.workers.iter().filter(|w| w.ready && !w.dead).count();
        (alive, ready, g.remote_done, g.inline_done)
    }

    /// The budget the thumbnail index is pruned to. The app refreshes it from the session every
    /// frame, so `library.preferences ▸ cacheMb` applies to the next stored thumbnail.
    pub fn set_budget(&self, bytes: u64) {
        self.0.borrow_mut().budget = bytes;
    }
}

impl RenderOffload for Workers {
    fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
        let mut g = self.0.borrow_mut();
        let g = &mut *g;
        if let Some((cache, _)) = job.cache.as_ref().or(job.view_cache.as_ref()) {
            g.observe_cache(cache, job.cache_generation);
        }
        // rendered thumbnail in memory: no worker needed
        if let Some((cache, key)) = &job.cache
            && let Some(img) = cache.get_at(job.cache_generation, *key)
        {
            let image = Arc::unwrap_or_clone(img);
            let histogram = Histogram::of_srgb8(&image);
            let r = RenderResult {
                request_id: job.request_id,
                source_key: job.source_key,
                photo: job.photo,
                level: job.level,
                key: job.key,
                rendered: Ok(Rendered { image, histogram, deep: None }),
                gpu_fallback: None,
                loaded: None,
                quick: None,
                display: None,
            };
            g.done.push((slot, r, 0.0));
            return None;
        }
        if g.workers.iter().all(|w| w.dead) {
            // no workers: render here, one job per frame (as without workers)
            if g.inline_this_frame {
                return Some(job);
            }
            g.inline_this_frame = true;
            let t0 = now();
            let r = job.run();
            g.inline_done += 1;
            g.done.push((slot, r, now() - t0));
            return None;
        }
        let free = W::free;
        let pick = match g.affinity.get(&(job.photo, job.level)) {
            // a decoded preview-level source, or the loupe's stage cache, is worth waiting for; a
            // thumbnail isn't
            Some(&i) if g.workers.get(i).is_some_and(free) => Some(i),
            Some(&i) if (job.level == SourceLevel::Preview || job.stages.is_some()) && g.workers.get(i).is_some_and(|w| !w.dead) => None,
            _ => g.workers.iter().position(free),
        };
        let Some(i) = pick else { return Some(job) };
        // decided here, so the next job for this photo follows even before this one finishes
        g.affinity.insert((job.photo, job.level), i);
        let disk_key = job.cache.as_ref().map(|(_, key)| g.index.cache_key(*key));
        let thumb_cached = disk_key.as_ref().is_some_and(|key| g.index.touch(key));
        let mut wire = WireJob::from_job(&job, thumb_cached, &format!("{slot:?}"));
        // The namespace also changes when a new library's cache starts again at generation zero.
        wire.cache_generation = g.index.namespace();
        wire.thumb = disk_key.clone();
        let Ok(text) = serde_json::to_string(&wire) else { return Some(job) };
        g.next_id = g.next_id.wrapping_add(1);
        let id = g.next_id;
        let msg = Object::new();
        set(&msg, "id", &id.into());
        set(&msg, "job", &text.into());
        let namespace = g.index.namespace();
        let w = &mut g.workers[i];
        if let Err(e) = w.worker.post_message(&msg) {
            log::warn!("render worker {i}: {e:?}");
            return Some(job);
        }
        w.busy = Some(Busy {
            disk_key,
            namespace,
            request_id: job.request_id,
            cache_generation: job.cache_generation,
            id,
            slot,
            photo: job.photo,
            level: job.level,
            key: job.key,
            cache: job.cache,
        });
        None
    }

    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
        let mut g = self.0.borrow_mut();
        // Persist invalidation even when the cleared library has no visible render requests.
        if let Some(cache) = g.preview_generation.cache() {
            g.observe_cache(&cache, cache.generation());
        }
        g.inline_this_frame = false;
        std::mem::take(&mut g.done)
    }
}
