//! Render jobs across the Web Worker boundary.
//!
//! A worker is a second wasm instance with its own memory, so an engine [`RenderJob`] (which holds
//! `Arc`s into the main instance) can't be shared. The main thread sends a [`WireJob`] instead:
//! where the pixels come from (the catalog [`Source`]), the develop settings and the request. The
//! worker keeps its own decoded sources ([`WorkerCore`]), reads originals and cached thumbnails
//! from browser storage, renders, and transfers the RGBA bytes back.
//!
//! Thumbnail disk cache on the web: workers read/write `thumbs/<key>.jpg` in browser storage; the
//! main thread keeps the index (sizes, recency) and prunes it to the native cache's budget
//! ([`ThumbIndex`]).

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use lightcraft_engine::auto_measure::{AutoInput, AutoKind, AutoMeasured, measure_kind, with_lens_pick};
use lightcraft_engine::catalog::Source;
use lightcraft_engine::develop::{DevelopSettings, EmbeddedLens};
use lightcraft_engine::export::{
    ExportOptions, Exported, FileParts, FileSpec, RenderParts, dng_preview, encode_gain_map_jpeg, encode_rendered, export_file,
};
use lightcraft_engine::media::{DecodedSource, MediaCache, RenderJob, SourceLevel, SourceRef};
use lightcraft_engine::pipeline::auto::FaceBox;
use lightcraft_engine::pipeline::{OutputDepth, OutputSpace, Quality, RenderRequest, Rendered, SourceInfo, StageCache};
use lightcraft_meta::Metadata;
use lightcraft_preview::{Hash128, Lru, PreviewCache};
use serde::{Deserialize, Serialize};

use crate::store::hash_of_path;

/// One render request for a worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireJob {
    #[serde(default)]
    pub cache_generation: u64,
    #[serde(default)]
    pub source_identity: Option<String>,
    pub level: SourceLevel,
    pub origin: Source,
    /// Decode the source at most this long.
    pub max_edge: usize,
    pub raw: bool,
    #[serde(default)]
    pub relative_wb: bool,
    #[serde(default)]
    pub legacy_relative_wb: bool,
    pub as_shot_temp: f64,
    pub as_shot_tint: f64,
    pub lens: Option<EmbeddedLens>,
    pub settings: Arc<DevelopSettings>,
    pub max_w: usize,
    pub max_h: usize,
    pub draft: bool,
    pub apply_crop: bool,
    /// Thumbnail cache key (hex): read the cached file first if `thumb_cached`, store the result.
    pub thumb: Option<String>,
    pub thumb_cached: bool,
    /// Reuse the worker's intermediate results for this view (the loupe), like the desktop's
    /// per-slot stage cache: slider drags then only redo what changed.
    pub stages: Option<String>,
    /// The request's diagnostic overlay (`Overlay::to_parts`).
    #[serde(default)]
    pub overlay: (u8, f64),
    /// Render only this window `[x, y, w, h]` of the output `max_w × max_h` describes (a zoomed view).
    #[serde(default)]
    pub window: Option<[usize; 4]>,
}

impl WireJob {
    /// `view`: the slot name, for jobs that keep a stage cache.
    pub fn from_job(job: &RenderJob, thumb_cached: bool, view: &str) -> WireJob {
        let max_edge = match &job.source {
            SourceRef::Demo { max_edge, .. } | SourceRef::File { max_edge, .. } | SourceRef::Fit { max_edge, .. } => *max_edge,
            SourceRef::Loaded(img) => img.image.width.max(img.image.height),
            SourceRef::Smart { .. } => 2560,
        };
        WireJob {
            cache_generation: job.cache_generation,
            source_identity: job.source_key.map(|key| key.to_string()),
            level: job.level,
            origin: job.origin.clone(),
            max_edge,
            raw: job.info.raw,
            relative_wb: job.info.relative_wb,
            legacy_relative_wb: job.info.legacy_relative_wb,
            as_shot_temp: job.info.as_shot_temp,
            as_shot_tint: job.info.as_shot_tint,
            lens: job.info.lens,
            settings: job.settings.clone(),
            max_w: job.request.max_w,
            max_h: job.request.max_h,
            draft: job.request.quality == Quality::Draft,
            apply_crop: job.request.apply_crop,
            thumb: job.cache.as_ref().map(|(_, k)| k.to_string()),
            thumb_cached,
            stages: job.stages.is_some().then(|| view.to_string()),
            overlay: job.request.overlay.to_parts(),
            window: job.request.window.map(|w| [w.x, w.y, w.w, w.h]),
        }
    }

    pub fn info(&self) -> SourceInfo {
        SourceInfo {
            raw: self.raw,
            as_shot_temp: self.as_shot_temp,
            as_shot_tint: self.as_shot_tint,
            lens: self.lens,
            relative_wb: self.relative_wb,
            legacy_relative_wb: self.legacy_relative_wb,
            ..Default::default()
        }
    }

    pub fn request(&self) -> RenderRequest {
        RenderRequest {
            max_w: self.max_w,
            max_h: self.max_h,
            quality: if self.draft { Quality::Draft } else { Quality::Full },
            apply_crop: self.apply_crop,
            overlay: lightcraft_engine::pipeline::Overlay::from_parts(self.overlay.0, self.overlay.1),
            // sRGB, 8-bit: previews. An export sets its own space and depth ([`WireExport`]).
            space: lightcraft_engine::pipeline::OutputSpace::Srgb,
            depth: lightcraft_engine::pipeline::OutputDepth::U8,
            proof: None,
            window: self.window.map(|[x, y, w, h]| lightcraft_engine::pipeline::PixelWindow { x, y, w, h }),
            display: None,
        }
    }

    /// Identifies the decoded source this job needs.
    fn source_key(&self) -> String {
        match &self.origin {
            Source::File { path } => format!("{}:{:?}:{}:{path}", self.cache_generation, self.source_identity, self.max_edge),
            Source::Demo { scene } => format!("{}:{:?}:{}:demo:{scene}", self.cache_generation, self.source_identity, self.max_edge),
        }
    }

    /// Storage key of the cached thumbnail.
    pub fn thumb_key(&self) -> Option<String> {
        self.thumb.as_ref().map(|h| thumb_storage_key(h))
    }
}

/// One photo's export for a worker (#192): the render ([`WireJob`], sized for the output) and
/// what encodes it. The worker decodes the original itself, renders, encodes and sends the file
/// back; where it goes (naming, conflicts, the download) stays with the main thread's batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireExport {
    pub job: WireJob,
    /// Gain map JPEG: also render the HDR rendition ([`OutputDepth::F32Hdr`]).
    #[serde(default)]
    pub hdr: bool,
    pub space: OutputSpace,
    pub depth: OutputDepth,
    /// The options this photo is encoded with ([`RenderParts::opts`]).
    pub options: ExportOptions,
    #[serde(default)]
    pub meta: Option<Metadata>,
    /// The file's name in the batch (the main thread places it).
    pub file_name: String,
}

impl WireExport {
    /// The export of a prepared photo, or why a worker can't do it and the main thread must: a
    /// source that isn't the photo's own file or demo scene.
    ///
    /// A Denoise edit is not a reason: its picture is the AI-denoised one, which exists only where the
    /// engine has a denoise model and a library on disk to keep it in, and the browser has neither (the
    /// web build has no `denoise` feature and its session installs no denoise loader), so there the
    /// edit has nothing to mix in and the photo renders from its plain picture, in a worker as on the
    /// main thread. A job whose source carries a denoised picture (a native session) stays refused
    /// below: a worker could not read it.
    pub fn from_parts(parts: &RenderParts<'_>, file_name: &str) -> Result<WireExport, &'static str> {
        let job = parts.job;
        refuse_unsendable(&job.source)?;
        let mut wire = WireJob::from_job(job, false, "");
        wire.stages = None;
        Ok(WireExport {
            job: wire,
            hdr: parts.hdr_job.is_some(),
            space: job.request.space,
            depth: job.request.depth,
            options: parts.opts.clone(),
            meta: parts.meta.cloned(),
            file_name: file_name.to_string(),
        })
    }

    /// The hash of the original a worker reads from storage, if the photo is a stored file.
    pub fn original_hash(&self) -> Option<String> {
        match &self.job.origin {
            Source::File { path } => hash_of_path(path).map(str::to_string),
            Source::Demo { .. } => None,
        }
    }

    /// Whether the worker needs the original's bytes: a file, not a generated demo scene.
    pub fn needs_original(&self) -> bool {
        matches!(self.job.origin, Source::File { .. })
    }

    fn request(&self, depth: OutputDepth) -> RenderRequest {
        RenderRequest { space: self.space, depth, ..self.job.request() }
    }
}

/// Why a worker can't make a job from `source`: a smart preview (not in a worker's reach) or a
/// denoised picture (made by the engine's model in the instance that owns the library).
fn refuse_unsendable(source: &SourceRef) -> Result<(), &'static str> {
    match source {
        SourceRef::Smart { .. } | SourceRef::File { denoise: Some(_), .. } => Err("the photo is rendered from a smart preview or a denoised picture"),
        _ => Ok(()),
    }
}

/// One photo's Original copy or DNG export for a worker (#192): what the engine does with the file's
/// bytes ([`FileSpec`]) and, for a DNG, the render of its embedded preview. The worker reads the
/// original from storage itself, makes the file and sends it back with its XMP sidecar; where it goes
/// stays with the main thread's batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireFileExport {
    pub spec: FileSpec,
    /// The edited photo for a DNG's embedded preview (sRGB, 8-bit, sized for it).
    #[serde(default)]
    pub preview: Option<WireJob>,
    /// The file's name in the batch (the main thread places it).
    pub file_name: String,
}

impl WireFileExport {
    /// The export of a prepared original copy or DNG, or why the main thread must make it.
    pub fn from_parts(parts: &FileParts<'_>, file_name: &str) -> Result<WireFileExport, &'static str> {
        let preview = match parts.preview {
            Some(job) => {
                refuse_unsendable(&job.source)?;
                let mut wire = WireJob::from_job(job, false, "");
                wire.stages = None;
                Some(wire)
            }
            None => None,
        };
        Ok(WireFileExport { spec: parts.spec.clone(), preview, file_name: file_name.to_string() })
    }

    /// The hash of the original a worker reads from storage, if the photo is a stored file.
    pub fn original_hash(&self) -> Option<String> {
        hash_of_path(&self.spec.path).map(str::to_string)
    }
}

/// One photo's Auto measure for a worker (#192): where the source comes from (thumbnail level; the
/// original for the noise fit) and the settings and facts it is read against ([`WireJob`], no
/// render), and what to measure. The worker answers with the values (`AutoMeasured` as JSON); the
/// main thread applies them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireAuto {
    pub job: WireJob,
    /// What to measure (Auto Settings when absent).
    #[serde(default)]
    pub kind: AutoKind,
    /// The subject, background and sky get their own masks too.
    pub regions: bool,
    /// Cached face boxes (x0, y0, x1, y1 as fractions of the frame) that meter the key.
    #[serde(default)]
    pub faces: Vec<[f32; 4]>,
    /// The user picked a lens (`lens_pick`, which may be none) in place of the file's own.
    #[serde(default)]
    pub lens_picked: bool,
    #[serde(default)]
    pub lens_pick: Option<EmbeddedLens>,
}

impl WireAuto {
    pub fn from_input(input: &AutoInput) -> WireAuto {
        let info = &input.info;
        let job = WireJob {
            cache_generation: 0,
            // the file's identity: a replaced file is not measured from the old decode
            source_identity: Some(input.source_identity.to_string()),
            level: input.kind.level(),
            origin: input.origin.clone(),
            max_edge: input.kind.level().max_edge(),
            raw: info.raw,
            relative_wb: info.relative_wb,
            legacy_relative_wb: info.legacy_relative_wb,
            as_shot_temp: info.as_shot_temp,
            as_shot_tint: info.as_shot_tint,
            lens: info.lens,
            settings: input.settings.clone(),
            max_w: 0,
            max_h: 0,
            draft: false,
            apply_crop: false,
            thumb: None,
            thumb_cached: false,
            stages: None,
            overlay: (0, 0.0),
            window: None,
        };
        WireAuto {
            job,
            kind: input.kind,
            regions: input.regions,
            // a box with a NaN or infinite side can't cross as JSON, and Auto ignores it anyway
            faces: input.faces.iter().map(|f| [f.x0, f.y0, f.x1, f.y1]).filter(|b| b.iter().all(|v| v.is_finite())).collect(),
            lens_picked: input.lens_pick.is_some(),
            lens_pick: input.lens_pick.flatten(),
        }
    }

    pub fn faces(&self) -> Vec<FaceBox> {
        self.faces.iter().map(|[x0, y0, x1, y1]| FaceBox { x0: *x0, y0: *y0, x1: *x1, y1: *y1 }).collect()
    }
}

pub fn thumb_storage_key(hex: &str) -> String {
    format!("thumbs/{hex}.jpg")
}

/// Decoded sources per worker.
const WORKER_SOURCE_BYTES: usize = 192 << 20;

/// The synchronous part of a worker: decoded-source cache + rendering.
pub struct WorkerCore {
    sources: Lru<String, DecodedSource>,
    media: MediaCache,
    stages: HashMap<String, Arc<StageCache>>,
}

impl Default for WorkerCore {
    fn default() -> Self {
        WorkerCore { sources: Lru::new(WORKER_SOURCE_BYTES), media: MediaCache::default(), stages: HashMap::new() }
    }
}

impl WorkerCore {
    /// The content hash of the original to read from storage before [`WorkerCore::render`], if
    /// the job needs one that isn't decoded yet.
    pub fn needs_original(&mut self, job: &WireJob) -> Option<String> {
        if self.sources.get(&job.source_key()).is_some() {
            return None;
        }
        match &job.origin {
            Source::File { path } => hash_of_path(path).map(str::to_string),
            Source::Demo { .. } => None,
        }
    }

    /// The decoded source `job` needs: the one this worker holds, or decoded now from `original`
    /// (the file's bytes, when [`WorkerCore::needs_original`] asked) or the demo scene.
    fn source_for(&mut self, job: &WireJob, original: Option<&[u8]>) -> Result<DecodedSource, String> {
        let key = job.source_key();
        if let Some(s) = self.sources.get(&key).cloned() {
            return Ok(s);
        }
        let src = match (&job.origin, original) {
            (Source::File { .. }, Some(bytes)) => {
                let (image, info) = lightcraft_engine::files::load_bytes(bytes, job.max_edge)?;
                DecodedSource::new(Arc::new(image), Some(info))
            }
            (Source::File { path }, None) => return Err(format!("{path}: original not found in browser storage")),
            (origin @ Source::Demo { .. }, _) => self.media.origin_ref(origin, job.max_edge).load_source()?,
        };
        let cost = src.image.width * src.image.height * 12 + 64;
        self.sources.insert(key, src.clone(), cost);
        Ok(src)
    }

    /// Measure Auto for `a` (#192): the same decode as a thumbnail's (the original's for the noise
    /// fit), then the engine's pure measure. `original` holds the file's bytes when
    /// [`WorkerCore::needs_original`] asked.
    pub fn measure(&mut self, a: &WireAuto, original: Option<&[u8]>) -> Result<AutoMeasured, String> {
        let src = self.source_for(&a.job, original)?;
        let info = with_lens_pick(src.info_or(a.job.info()), a.lens_picked.then_some(a.lens_pick));
        Ok(measure_kind(a.kind, &src.image, &info, &a.job.settings, &a.faces(), a.regions))
    }

    /// Render `job`; `original` holds the file's bytes when [`WorkerCore::needs_original`] asked.
    pub fn render(&mut self, job: &WireJob, original: Option<&[u8]>) -> Result<Rendered, String> {
        let src = self.source_for(job, original)?;
        let info = src.info_or(job.info());
        let mut rendered = match &job.stages {
            Some(view) => {
                let st = self.stages.entry(view.clone()).or_default().clone();
                lightcraft_engine::pipeline::render_cached(&src.image, &info, &job.settings, &job.request(), &st)
            }
            None => lightcraft_engine::pipeline::render(&src.image, &info, &job.settings, &job.request()),
        };
        // an HDR edit's histogram shows its HDR range (as on native)
        if let Some(h) = lightcraft_engine::pipeline::hdr_histogram(&src.image, &info, &job.settings, &job.request()) {
            rendered.histogram = h;
        }
        Ok(rendered)
    }
}

impl WorkerCore {
    /// The source an export decodes: from `original` (the file's bytes, for a stored file) or the
    /// demo scene, for this export only: a full-size one would push the previews' decoded sources
    /// out of the cache.
    fn decode_for_export(&mut self, job: &WireJob, original: Option<&[u8]>) -> Result<DecodedSource, String> {
        match (&job.origin, original) {
            (Source::File { .. }, Some(bytes)) => {
                let (image, info) = lightcraft_engine::files::load_bytes(bytes, job.max_edge)?;
                Ok(DecodedSource::new(Arc::new(image), Some(info)))
            }
            (Source::File { path }, None) => Err(format!("{path}: original not found in browser storage")),
            (origin @ Source::Demo { .. }, _) => self.media.origin_ref(origin, job.max_edge).load_source(),
        }
    }

    /// What an export develops with: the decoder's facts, and the lens the user picked in place of the
    /// file's own (as the engine's render job does).
    fn export_info(src: &DecodedSource, job: &WireJob) -> SourceInfo {
        let mut info = src.info_or(job.info());
        if job.settings.lens_override.is_some() {
            info.lens = job.lens;
        }
        info
    }

    /// Run one export: decode the original (`original` holds the file's bytes for a stored file),
    /// render at the output size and encode.
    pub fn export(&mut self, x: &WireExport, original: Option<&[u8]>) -> Result<Exported, String> {
        let job = &x.job;
        let src = self.decode_for_export(job, original)?;
        let info = Self::export_info(&src, job);
        let rendered = lightcraft_engine::pipeline::render(&src.image, &info, &job.settings, &x.request(x.depth));
        let bytes = if x.hdr {
            let hdr = lightcraft_engine::pipeline::render(&src.image, &info, &job.settings, &x.request(OutputDepth::F32Hdr));
            let deep = hdr.deep.ok_or("the HDR render has no float samples")?;
            encode_gain_map_jpeg(&rendered.image, &deep, &x.options, x.meta.as_ref())?
        } else {
            encode_rendered(&rendered, &x.options, x.meta.as_ref())?
        };
        Ok(Exported {
            file_name: x.file_name.clone(),
            bytes,
            width: rendered.image.width,
            height: rendered.image.height,
            sidecars: Vec::new(),
            gpu_fallback: None,
            staged: None,
        })
    }

    /// Run one Original copy or DNG export from the file's bytes (`original`; none: the worker could
    /// not read it). The engine's own [`export_file`] does the work, so the file is the one
    /// `PreparedExport::run` makes; a DNG's embedded preview is rendered here from the same bytes.
    pub fn export_file(&mut self, x: &WireFileExport, original: Option<Vec<u8>>) -> Result<Exported, String> {
        let bytes = original.ok_or_else(|| format!("{}: original not found in browser storage", x.spec.path))?;
        // Rendered before the raw data is decoded, but only reported where the engine would have
        // rendered it, so a file that is no raw photo fails the way it does everywhere else.
        let preview = match (&x.preview, x.spec.dng.is_some()) {
            (Some(job), true) => Some(self.render_preview(job, &bytes).map_err(|e| format!("{}: preview: {e}", x.spec.label))),
            _ => None,
        };
        export_file(x.file_name.clone(), bytes, x.spec.clone(), move || preview.map(|r| r.and_then(dng_preview)).transpose())
    }

    fn render_preview(&mut self, job: &WireJob, bytes: &[u8]) -> Result<Rendered, String> {
        let src = self.decode_for_export(job, Some(bytes))?;
        let info = Self::export_info(&src, job);
        Ok(lightcraft_engine::pipeline::render(&src.image, &info, &job.settings, &job.request()))
    }
}

/// The web thumbnail cache's index (main thread): size and last use of every stored thumbnail,
/// pruned like the native disk cache (least recently used first, down to 80 % of the budget).
#[derive(Default, Serialize, Deserialize)]
pub struct ThumbIndex {
    #[serde(default)]
    namespace: u64,
    /// hex key → (bytes, last use).
    entries: HashMap<String, (u64, u64)>,
    clock: u64,
    #[serde(skip)]
    total: u64,
    #[serde(skip)]
    pub dirty: bool,
}

impl ThumbIndex {
    pub fn namespace(&self) -> u64 {
        self.namespace
    }

    /// A persistent new namespace prevents late old workers restoring an invalidated disk hit.
    pub fn cache_key(&self, key: Hash128) -> String {
        if self.namespace == 0 {
            return key.to_string();
        }
        lightcraft_preview::Hasher128::new().str(&key.to_string()).str("cleared").u64(self.namespace).finish().to_string()
    }

    pub fn invalidate(&mut self) -> Vec<String> {
        self.namespace = self.namespace.wrapping_add(1);
        self.total = 0;
        self.dirty = true;
        self.entries.drain().map(|(key, _)| key).collect()
    }
    pub fn from_json(bytes: &[u8]) -> ThumbIndex {
        let mut i: ThumbIndex = serde_json::from_slice(bytes).unwrap_or_default();
        i.total = i.entries.values().map(|e| e.0).sum();
        i
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Is `hex` stored? Counts as a use.
    pub fn touch(&mut self, hex: &str) -> bool {
        self.clock += 1;
        let clock = self.clock;
        match self.entries.get_mut(hex) {
            Some(e) => {
                e.1 = clock;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    pub fn contains(&self, hex: &str) -> bool {
        self.entries.contains_key(hex)
    }

    pub fn insert(&mut self, hex: &str, bytes: u64) {
        self.clock += 1;
        if let Some(old) = self.entries.insert(hex.to_string(), (bytes, self.clock)) {
            self.total -= old.0;
        }
        self.total += bytes;
        self.dirty = true;
    }

    pub fn remove(&mut self, hex: &str) {
        if let Some(old) = self.entries.remove(hex) {
            self.total -= old.0;
            self.dirty = true;
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Over `budget`: drop the least recently used entries down to 80 % of it. Returns the keys
    /// whose files should be deleted.
    pub fn prune(&mut self, budget: u64) -> Vec<String> {
        if self.total <= budget {
            return Vec::new();
        }
        let target = budget / 5 * 4;
        let mut by_age: Vec<(String, u64, u64)> = self.entries.iter().map(|(k, v)| (k.clone(), v.0, v.1)).collect();
        by_age.sort_by_key(|e| e.2);
        let mut out = Vec::new();
        for (k, _, _) in by_age {
            if self.total <= target {
                break;
            }
            self.remove(&k);
            out.push(k);
        }
        out
    }
}

/// Which rendered-thumbnail cache, at which generation, the stored thumbnails ([`ThumbIndex`])
/// belong to (main thread). Clearing the previews bumps the cache's generation, and opening
/// another library replaces the cache: either makes every stored thumbnail obsolete.
pub struct CacheWatch(Option<(Weak<PreviewCache>, u64)>);

impl CacheWatch {
    /// Watch `active`, the session's cache when the workers start: the persisted index belongs
    /// to it at its current generation. (Without this baseline, the first observation after a
    /// clear would only record the cleared generation and keep the obsolete thumbnails.)
    pub fn new(active: &Arc<PreviewCache>) -> CacheWatch {
        CacheWatch(Some((Arc::downgrade(active), active.generation())))
    }

    /// The watched cache, while it's alive.
    pub fn cache(&self) -> Option<Arc<PreviewCache>> {
        self.0.as_ref().and_then(|(cache, _)| cache.upgrade())
    }

    /// `cache` at `generation` is the active one now. If it was cleared or replaced since the
    /// last observation, invalidate `index` and return the keys whose files should be deleted.
    pub fn observe(&mut self, cache: &Arc<PreviewCache>, generation: u64, index: &mut ThumbIndex) -> Vec<String> {
        let changed = self.0.as_ref().is_some_and(|(old, g)| old.as_ptr() != Arc::as_ptr(cache) || *g != generation);
        self.0 = Some((Arc::downgrade(cache), generation));
        if changed { index.invalidate() } else { Vec::new() }
    }
}

/// Hands out the positions that render workers will have in the main thread's worker list.
/// A worker that fails to start gets no position, so the position a callback captures stays the
/// index of its own worker in that list (#229).
#[derive(Debug, Default)]
pub struct WorkerSlots {
    next: usize,
}

impl WorkerSlots {
    /// The position the worker being started will have, if it comes up.
    pub fn peek(&self) -> usize {
        self.next
    }

    /// The worker at [`Self::peek`] came up and was added to the list.
    pub fn commit(&mut self) {
        self.next += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::Session;

    // Issue #229: a failed worker attempt must not shift the positions of the workers after it
    #[test]
    fn a_failed_worker_attempt_leaves_no_gap_in_the_slots() {
        let mut slots = WorkerSlots::default();
        let mut taken = Vec::new();
        for attempt_ok in [false, true, true, true] {
            let i = slots.peek();
            if attempt_ok {
                taken.push(i);
                slots.commit();
            }
        }
        assert_eq!(taken, vec![0, 1, 2]);
    }

    #[test]
    fn clearing_index_changes_disk_namespace_and_survives_restart() {
        let key = lightcraft_preview::hash_bytes(b"thumbnail");
        let mut index = ThumbIndex::default();
        let old = index.cache_key(key);
        assert_eq!(old, key.to_string());
        index.insert(&old, 100);
        assert_eq!(index.invalidate(), vec![old.clone()]);
        let new = index.cache_key(key);
        assert_ne!(old, new);
        index.insert(&new, 100);
        let reopened = ThumbIndex::from_json(&index.to_json());
        assert_eq!(reopened.cache_key(key), new);
        assert!(!reopened.contains(&old));
        assert!(reopened.contains(&new));
    }

    // Issue #323: a worker given a zoomed view's window renders that window, not the whole frame
    #[test]
    fn a_window_job_crosses_the_wire_and_renders_the_same_window() {
        use lightcraft_engine::pipeline::PixelWindow;
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        let win = PixelWindow { x: 300, y: 200, w: 160, h: 120 };
        let job = s.region_job(id, 1920, 1280, win, true).unwrap();
        let wire = WireJob::from_job(&job, false, "Region");
        let wire: WireJob = serde_json::from_str(&serde_json::to_string(&wire).unwrap()).unwrap();
        assert_eq!(wire.request().window, Some(win));
        let mut core = WorkerCore::default();
        let a = core.render(&wire, None).unwrap();
        let b = job.run().rendered.unwrap();
        assert_eq!((a.image.width, a.image.height), (160, 120));
        assert_eq!((b.image.width, b.image.height), (160, 120));
        // the worker renders on the CPU; the engine job uses a GPU when the machine has one, which
        // stays within docs/gpu-pipeline.md's bound (≤ 3 LSB per channel) rather than bit-exact
        let max = a.image.data.iter().zip(&b.image.data).flat_map(|(p, q)| (0..3).map(move |c| p[c].abs_diff(q[c]))).max();
        assert!(max.is_some_and(|m| m <= 3), "max channel difference {max:?}");
    }

    // a job from before windows existed has none
    #[test]
    fn a_job_without_a_window_field_has_no_window() {
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        let wire = WireJob::from_job(&s.thumb_job(id, 128).unwrap(), false, "Thumb");
        let mut v = serde_json::to_value(&wire).unwrap();
        v.as_object_mut().unwrap().remove("window");
        let old: WireJob = serde_json::from_value(v).unwrap();
        assert_eq!(old.request().window, None);
    }

    #[test]
    fn wire_job_renders_like_the_engine() {
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 0.7})).unwrap();
        let job = s.thumb_job(id, 128).unwrap();
        let wire = WireJob::from_job(&job, false, "Thumb");
        // through JSON, as it crosses the worker boundary
        let wire: WireJob = serde_json::from_str(&serde_json::to_string(&wire).unwrap()).unwrap();
        assert!(wire.thumb.is_some());
        let mut core = WorkerCore::default();
        assert_eq!(core.needs_original(&wire), None, "demo scenes are generated");
        let a = core.render(&wire, None).unwrap();
        let b = job.run().rendered.unwrap();
        assert_eq!((a.image.width, a.image.height), (b.image.width, b.image.height));
        assert_eq!(a.image.data, b.image.data);
        assert_eq!(a.histogram, b.histogram);
    }

    // Issue #192: a worker measures Auto from the message alone, and the values apply on the session
    #[test]
    fn a_worker_measures_auto_like_the_session_and_its_values_apply() {
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
        let shown = s.execute("develop.auto", &serde_json::json!({"dryRun": true})).unwrap();
        let input = lightcraft_engine::auto_measure::auto_input(&s, id, true, None).unwrap();
        // through JSON, as it crosses the worker boundary
        let wire: WireAuto = serde_json::from_str(&serde_json::to_string(&WireAuto::from_input(&input)).unwrap()).unwrap();
        let mut core = WorkerCore::default();
        assert_eq!(core.needs_original(&wire.job), None, "demo scenes are generated");
        let values = core.measure(&wire, None).unwrap();
        let AutoMeasured::Tone(values) = serde_json::from_str(&serde_json::to_string(&values).unwrap()).unwrap() else {
            panic!("Auto Settings measures the tone");
        };
        assert_eq!(serde_json::to_value(&values).unwrap(), shown, "the worker finds what the session finds");
        s.execute("develop.applyAutoValues", &serde_json::json!({"values": values, "basis": input.basis})).unwrap();
        s.execute("edit.undo", &serde_json::json!({})).unwrap();
        assert!(core.measure(&wire, None).is_ok(), "the decoded source is kept for the next measure");
    }

    // Issue #192: the other Auto commands are measured by a worker too, and the command the values are
    // handed to writes what it always wrote
    #[test]
    fn a_worker_measures_every_auto_kind_like_the_session() {
        use lightcraft_engine::auto_measure::auto_input_for;
        for (command, params, kind) in [
            ("develop.autoBwMix", serde_json::json!({}), AutoKind::BwMix),
            ("develop.autoNoise", serde_json::json!({}), AutoKind::Noise),
            ("develop.autoWhiteBalance", serde_json::json!({}), AutoKind::WhiteBalance),
            ("develop.autoBatch", serde_json::json!({"regions": false}), AutoKind::Tone),
        ] {
            let mut native = Session::with_demo();
            let mut s = Session::with_demo();
            let id = s.visible_cloned()[0];
            for session in [&mut native, &mut s] {
                session.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
            }
            let want = native.execute(command, &params).unwrap();
            let input = auto_input_for(&s, id, kind, false, None).unwrap();
            let wire: WireAuto = serde_json::from_str(&serde_json::to_string(&WireAuto::from_input(&input)).unwrap()).unwrap();
            assert_eq!(wire.kind, kind, "{command}");
            assert_eq!(wire.job.max_edge, kind.level().max_edge(), "{command}: the noise fit reads the original, the rest a thumbnail");
            let mut core = WorkerCore::default();
            let values: AutoMeasured = serde_json::from_str(&serde_json::to_string(&core.measure(&wire, None).unwrap()).unwrap()).unwrap();
            assert_eq!(values.kind(), kind, "{command}");
            s.prepare_auto(id, false, input.basis, values);
            let got = s.execute(command, &params).unwrap();
            s.clear_prepared_auto();
            assert_eq!(got, want, "{command}");
            assert_eq!((*s.develop_of(id).unwrap()).clone(), (*native.develop_of(id).unwrap()).clone(), "{command}");
        }
    }

    /// A prepared export of the first demo photo, as the main thread builds it for a worker.
    fn prepared(options: &ExportOptions) -> (Session, lightcraft_engine::export::PreparedExport) {
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        let item = lightcraft_engine::export::prepare_export(&mut s, id, options, 1).unwrap();
        (s, item)
    }

    // Issue #192: an export job crosses the worker boundary as JSON and comes back unchanged
    #[test]
    fn an_export_job_survives_the_message_round_trip() {
        let options = ExportOptions {
            format: lightcraft_engine::export::ExportFormat::Png,
            quality: 77,
            naming: "{name}-web".into(),
            resize: Some(lightcraft_engine::export::Resize::long_edge(96)),
            ..Default::default()
        };
        let (_s, item) = prepared(&options);
        let wire = WireExport::from_parts(&item.render_parts().unwrap(), &item.file_name).unwrap();
        let text = serde_json::to_string(&wire).unwrap();
        let back: WireExport = serde_json::from_str(&text).unwrap();
        assert_eq!(back.options, options);
        assert_eq!(back.file_name, item.file_name);
        assert_eq!((back.space, back.depth, back.hdr), (wire.space, wire.depth, false));
        assert_eq!(serde_json::to_string(&back).unwrap(), text, "nothing is lost or reordered in transit");
        assert!(!back.needs_original(), "a demo scene is generated in the worker");
        assert_eq!(back.original_hash(), None);
    }

    // a message from a build that didn't send the optional parts still reads
    #[test]
    fn an_export_job_without_optional_parts_reads() {
        let (_s, item) = prepared(&ExportOptions::default());
        let wire = WireExport::from_parts(&item.render_parts().unwrap(), &item.file_name).unwrap();
        let mut v = serde_json::to_value(&wire).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("hdr");
        o.remove("meta");
        let old: WireExport = serde_json::from_value(v).unwrap();
        assert!(!old.hdr && old.meta.is_none());
        // a message that is not an export job is an error the worker reports, not a panic
        assert!(serde_json::from_str::<WireExport>("{\"job\": 3}").is_err());
        assert!(serde_json::from_str::<WireExport>("").is_err());
    }

    // Issue #192: the worker's export is the file the main thread's export makes
    #[test]
    fn a_worker_export_makes_the_same_file_as_the_main_thread() {
        let options = ExportOptions {
            format: lightcraft_engine::export::ExportFormat::Png,
            resize: Some(lightcraft_engine::export::Resize::long_edge(96)),
            ..Default::default()
        };
        let (_s, item) = prepared(&options);
        let wire = WireExport::from_parts(&item.render_parts().unwrap(), &item.file_name).unwrap();
        let wire: WireExport = serde_json::from_str(&serde_json::to_string(&wire).unwrap()).unwrap();
        let mut core = WorkerCore::default();
        let remote = core.export(&wire, None).unwrap();
        assert_eq!(remote.file_name, item.file_name);
        assert!(remote.bytes.starts_with(b"\x89PNG"));
        assert_eq!(remote.width.max(remote.height), 96);
        assert!(remote.sidecars.is_empty());
        let local = item.run().unwrap();
        assert_eq!((remote.width, remote.height), (local.width, local.height));
        assert_eq!(remote.file_name, local.file_name);
    }

    // a stored original that the worker could not read is an error for that photo
    #[test]
    fn a_worker_export_without_its_original_fails_with_a_message() {
        let bytes = crate::store::png_bytes(64, 48);
        let hash = crate::store::content_hash(&bytes);
        let (_s, item) = prepared(&ExportOptions::default());
        let mut wire = WireExport::from_parts(&item.render_parts().unwrap(), "a.jpg").unwrap();
        wire.job.origin = Source::File { path: crate::store::original_path(&hash, "a.png") };
        assert!(wire.needs_original());
        assert_eq!(wire.original_hash().as_deref(), Some(hash.as_str()));
        let mut core = WorkerCore::default();
        let e = core.export(&wire, None).err().unwrap();
        assert!(e.contains("not found in browser storage"), "{e}");
        let ok = core.export(&wire, Some(&bytes)).unwrap();
        assert!(ok.bytes.starts_with(b"\xFF\xD8"));
        // garbage bytes are a decode error, not a panic
        assert!(core.export(&wire, Some(b"not an image")).is_err());
    }

    // what a worker can't do stays on the main thread
    #[test]
    fn exports_a_worker_cannot_run_stay_on_the_main_thread() {
        use lightcraft_engine::media::{DenoiseSpec, PairLoader};
        // an original copy or a DNG needs a file: a demo photo has none, an error before any job exists
        let copy = ExportOptions { format: lightcraft_engine::export::ExportFormat::Original, ..Default::default() };
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        assert!(lightcraft_engine::export::prepare_export(&mut s, id, &copy, 1).is_err());
        // a photo whose source carries the engine's denoised picture (a native session) is made where the
        // library and the model are
        let (_s, item) = prepared(&ExportOptions::default());
        let mut job = item.render_parts().unwrap().job.clone();
        let pair: PairLoader = Arc::new(|_, _, _| Err("no model in a worker".into()));
        let spec = DenoiseSpec { product: Default::default(), key: "k".into(), make: None };
        job.source = SourceRef::File { path: "a.dng".into(), max_edge: 64, loader: None, fallback: None, denoise: Some((spec, pair)) };
        let parts = RenderParts { job: &job, hdr_job: None, meta: None, opts: &ExportOptions::default() };
        assert!(WireExport::from_parts(&parts, "a.jpg").is_err());
        // so is a smart preview standing in for a missing original
        job.source = SourceRef::Smart { path: "a.smart".into() };
        let parts = RenderParts { job: &job, hdr_job: None, meta: None, opts: &ExportOptions::default() };
        assert!(WireExport::from_parts(&parts, "a.jpg").is_err());
    }

    /// A session set up as the web's is, with one stored original: its bytes come from memory by path,
    /// for the decoder and for the original copy and DNG exports.
    fn stored_photo(name: &str, bytes: &[u8]) -> (Session, lightcraft_engine::catalog::PhotoId) {
        // the worker renders on the CPU; so must the session, or a GPU's output differs in the last bits
        lightcraft_engine::gpu::set_enabled(false);
        let store = crate::store::Originals::default();
        let mut s = Session::new();
        store.install(&mut s);
        let reader = store.clone();
        s.media.file_bytes = Some(Arc::new(move |path: &str| reader.get(path).map(|b| b.to_vec()).ok_or_else(|| format!("{path}: not loaded"))));
        let path = store.added(name, &crate::store::content_hash(bytes), bytes.to_vec().into());
        let r = s.execute("library.import", &serde_json::json!({"paths": [path]})).unwrap();
        let id = lightcraft_engine::catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
        s.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": 0.7})).unwrap();
        (s, id)
    }

    /// A small Bayer DNG from the engine's own writer, standing in for a camera file.
    fn dng_bytes() -> Vec<u8> {
        use lightcraft_raw::*;
        let (w, h) = (32usize, 24usize);
        let data: Vec<u16> = (0..w * h).map(|i| 256 + ((i % w) * 300 + (i / w) * 200) as u16).collect();
        let raw = RawImage {
            format: RawFormat::Dng,
            width: w,
            height: h,
            cpp: 1,
            data: RawData::U16(data),
            cfa: Cfa::bayer("RGGB"),
            bits: 14,
            black: BlackLevel::uniform(256.0),
            white: vec![16000.0],
            active_area: Rect::new(0, 0, w, h),
            crop: Rect::new(0, 0, w, h),
            orientation: Orientation::Normal,
            color: ColorData {
                illuminant: [17, 21],
                color_matrix: [
                    Some(Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])),
                    Some(Mat3([[0.7, 0.3, -0.1], [-0.35, 1.3, 0.1], [0.05, -0.2, 1.0]])),
                ],
                as_shot_neutral: Some([0.5, 1.0, 0.7]),
                ..Default::default()
            },
            wb_multipliers: None,
            linearized: false,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
        };
        write_dng(&raw, &DngWriteOptions::default()).unwrap()
    }

    /// A file-export job as the worker gets it: through JSON.
    fn file_job(item: &lightcraft_engine::export::PreparedExport) -> WireFileExport {
        let wire = WireFileExport::from_parts(&item.file_parts().unwrap(), &item.file_name).unwrap();
        serde_json::from_str(&serde_json::to_string(&wire).unwrap()).unwrap()
    }

    fn assert_same_file(remote: &Exported, local: &Exported) {
        assert_eq!(remote.file_name, local.file_name);
        assert_eq!((remote.width, remote.height), (local.width, local.height));
        assert_eq!(remote.sidecars, local.sidecars, "the sidecars");
        assert!(remote.bytes == local.bytes, "the bytes: {} against {}", remote.bytes.len(), local.bytes.len());
    }

    // Issue #192: an Original copy made in a worker is the file the session makes, sidecar and all
    #[test]
    fn a_worker_original_export_makes_the_same_file_as_the_session() {
        use lightcraft_engine::export::{ExportFormat, prepare_export};
        let bytes = crate::store::png_bytes(64, 48);
        let (mut s, id) = stored_photo("shot.png", &bytes);
        let options = ExportOptions { format: ExportFormat::Original, naming: "{name}".into(), ..Default::default() };
        let item = prepare_export(&mut s, id, &options, 1).unwrap();
        assert!(item.render_parts().is_none(), "a copy is not a render");
        let wire = file_job(&item);
        assert!(wire.spec.dng.is_none() && wire.preview.is_none());
        assert_eq!(wire.original_hash(), Some(crate::store::content_hash(&bytes)), "the worker reads the original from storage");
        let remote = WorkerCore::default().export_file(&wire, Some(bytes.clone())).unwrap();
        assert_eq!(remote.bytes, bytes, "the original, byte for byte");
        assert_eq!(remote.sidecars.len(), 1);
        assert_same_file(&remote, &item.run().unwrap());
        // an original the worker could not read is an error for that photo
        let e = WorkerCore::default().export_file(&wire, None).err().unwrap();
        assert!(e.contains("not found in browser storage"), "{e}");
    }

    // Issue #192: a DNG written in a worker is the file the session writes, preview and all
    #[test]
    fn a_worker_dng_export_makes_the_same_file_as_the_session() {
        use lightcraft_engine::export::{DngPreviewSize, ExportFormat, prepare_export};
        let dng = dng_bytes();
        let (mut s, id) = stored_photo("raw.dng", &dng);
        for preview in [DngPreviewSize::Medium, DngPreviewSize::None] {
            let options = ExportOptions { format: ExportFormat::Dng, dng_preview: preview, naming: "{name}".into(), ..Default::default() };
            let item = prepare_export(&mut s, id, &options, 1).unwrap();
            let wire = file_job(&item);
            assert_eq!(wire.preview.is_some(), preview != DngPreviewSize::None, "{preview:?}");
            let remote = WorkerCore::default().export_file(&wire, Some(dng.clone())).unwrap();
            assert!(lightcraft_raw::decode(&remote.bytes).is_ok(), "{preview:?}: a DNG");
            assert!(remote.sidecars.is_empty());
            assert_same_file(&remote, &item.run().unwrap());
        }
        // a file that is no raw photo fails with the engine's own message
        let png = crate::store::png_bytes(64, 48);
        let (mut s, id) = stored_photo("shot.png", &png);
        let options = ExportOptions { format: ExportFormat::Dng, ..Default::default() };
        let item = prepare_export(&mut s, id, &options, 1).unwrap();
        let wire = file_job(&item);
        let remote = WorkerCore::default().export_file(&wire, Some(png)).err().unwrap();
        assert!(remote.contains("needs a raw photo"), "{remote}");
        assert_eq!(remote, item.run().err().unwrap());
    }

    // Issue #192: a photo with a Denoise edit is exported by a worker, as the session exports it. Where the
    // engine has no denoise model (the browser) the edit has no picture to mix in and the photo renders plain.
    #[test]
    fn a_worker_exports_a_photo_with_a_denoise_edit_like_the_session() {
        use lightcraft_engine::export::{ExportFormat, Resize, prepare_export};
        let bytes = crate::store::png_bytes(64, 48);
        let (mut s, id) = stored_photo("shot.png", &bytes);
        s.execute("develop.set", &serde_json::json!({"control": "enhance.denoise", "value": 50.0})).unwrap();
        assert!(s.catalog.photo(id).unwrap().develop.denoise_amount() > 0.0);
        for format in [ExportFormat::Jpeg, ExportFormat::Png] {
            let options = ExportOptions { format, resize: Some(Resize::long_edge(40)), ..Default::default() };
            let item = prepare_export(&mut s, id, &options, 1).unwrap();
            let wire = WireExport::from_parts(&item.render_parts().unwrap(), &item.file_name).unwrap();
            let wire: WireExport = serde_json::from_str(&serde_json::to_string(&wire).unwrap()).unwrap();
            let remote = WorkerCore::default().export(&wire, Some(&bytes)).unwrap();
            assert_same_file(&remote, &item.run().unwrap());
        }
    }

    #[test]
    fn worker_reads_originals_once() {
        let bytes = crate::store::png_bytes(64, 48);
        let hash = crate::store::content_hash(&bytes);
        let origin = Source::File { path: crate::store::original_path(&hash, "a.png") };
        let job = WireJob {
            cache_generation: 0,
            source_identity: None,
            level: SourceLevel::Thumb,
            origin,
            max_edge: 512,
            raw: false,
            relative_wb: false,
            legacy_relative_wb: false,
            as_shot_temp: 6500.0,
            as_shot_tint: 0.0,
            lens: None,
            settings: Arc::new(DevelopSettings::default()),
            max_w: 32,
            max_h: 32,
            draft: false,
            apply_crop: true,
            thumb: None,
            thumb_cached: false,
            stages: Some("Main".into()),
            overlay: (0, 0.0),
            window: None,
        };
        let mut core = WorkerCore::default();
        assert_eq!(core.needs_original(&job).as_deref(), Some(hash.as_str()));
        assert!(core.render(&job, None).is_err());
        let r = core.render(&job, Some(&bytes)).unwrap();
        assert_eq!(r.image.width, 32);
        assert_eq!(core.needs_original(&job), None, "decoded source is kept");
        let mut reloaded = job.clone();
        reloaded.source_identity = Some("changed content".into());
        assert_eq!(core.needs_original(&reloaded).as_deref(), Some(hash.as_str()), "content reload must not reuse an old decoded source");
        let mut refreshed = job.clone();
        refreshed.cache_generation = 1;
        assert_eq!(core.needs_original(&refreshed).as_deref(), Some(hash.as_str()), "explicit refresh must not reuse the old decoded source");
    }

    /// The persisted index as a restarted page loads it: one stored thumbnail.
    fn persisted_index(key: Hash128) -> (ThumbIndex, String) {
        let mut index = ThumbIndex::default();
        let hex = index.cache_key(key);
        index.insert(&hex, 100);
        (ThumbIndex::from_json(&index.to_json()), hex)
    }

    #[test]
    fn clearing_previews_before_the_first_request_invalidates_stored_thumbnails() {
        let key = lightcraft_preview::hash_bytes(b"thumbnail");
        let (mut index, old) = persisted_index(key);
        let cache = Arc::new(PreviewCache::memory(1 << 20));
        let mut watch = CacheWatch::new(&cache);
        let mut gone = Vec::new();
        // each frame (`Workers::finished`) observes the watched cache, even with no requests
        let mut frame = |watch: &mut CacheWatch, index: &mut ThumbIndex| {
            if let Some(c) = watch.cache() {
                gone.extend(watch.observe(&c, c.generation(), index));
            }
        };
        frame(&mut watch, &mut index); // an empty filtered view: no render request yet
        cache.clear(); // File ▸ Clear Preview Cache
        frame(&mut watch, &mut index);
        // the filter is removed: the first thumbnail request (`Workers::try_start`)
        gone.extend(watch.observe(&cache, cache.generation(), &mut index));
        let disk_key = index.cache_key(key);
        assert!(!index.touch(&disk_key), "the pre-clear thumbnail must not be read back");
        assert_eq!(gone, vec![old.clone()], "the stored file is deleted");
        assert_ne!(disk_key, old, "a fresh namespace: a late old write can't come back");
        assert!(index.dirty, "the invalidated index is saved");
    }

    #[test]
    fn replacing_the_cache_before_the_first_request_invalidates_stored_thumbnails() {
        let key = lightcraft_preview::hash_bytes(b"thumbnail");
        let (mut index, old) = persisted_index(key);
        let first = Arc::new(PreviewCache::memory(1 << 20));
        let mut watch = CacheWatch::new(&first);
        let other = Arc::new(PreviewCache::memory(1 << 20)); // another library, generation 0 again
        assert_eq!(watch.observe(&other, other.generation(), &mut index), vec![old]);
    }

    #[test]
    fn a_normal_start_keeps_stored_thumbnails() {
        let key = lightcraft_preview::hash_bytes(b"thumbnail");
        let (mut index, old) = persisted_index(key);
        let cache = Arc::new(PreviewCache::memory(1 << 20));
        let mut watch = CacheWatch::new(&cache);
        for _ in 0..3 {
            assert!(watch.observe(&cache, cache.generation(), &mut index).is_empty());
        }
        assert_eq!(index.cache_key(key), old);
        assert!(index.touch(&old), "persisted thumbnails survive a restart");
    }

    #[test]
    fn thumb_index_prunes_lru() {
        let mut i = ThumbIndex::default();
        for k in ["a", "b", "c", "d"] {
            i.insert(k, 100);
        }
        assert!(i.touch("a"));
        assert!(!i.touch("zz"));
        assert!(i.prune(1000).is_empty());
        let gone = i.prune(300); // 400 > 300 → down to 240
        assert_eq!(gone, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(i.total(), 200);
        let j = ThumbIndex::from_json(&i.to_json());
        assert_eq!((j.len(), j.total()), (2, 200));
        assert!(j.contains("a") && j.contains("d"));
    }
}
