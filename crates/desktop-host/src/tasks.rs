use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use serde_json::{Value, json};

use lightcraft_catalog::PhotoId;
use lightcraft_engine::import::{ImportCandidate, ImportJob, ImportOptions, Prepared, RawJpegImportPolicy, ScanInput, ScanOutput, ScanProgress};
use lightcraft_engine::media::QuickSource;
use lightcraft_engine::merge::{MergeJob, MergeOutput};
use lightcraft_engine::{CullApplyJob, CullJob, CullJobResult, Selection, Session};

use crate::snapshot::{JobStatus, TerminalTask};

const EVENT_CAPACITY: usize = 64;
const MAX_TASKS: usize = 128;
const MAX_MERGE_IDS: usize = 256;
const MAX_REVIEW_PATHS: usize = 512;
const MAX_PENDING_PREVIEW_IDS: usize = 8_192;
const MAX_COMPLETED_TASKS: usize = 64;
const MAX_IMPORT_PREVIEWS: usize = 128;
const IMPORT_PREVIEW_EDGE: usize = 256;
const CULL_MAX_IDS: usize = 8_192;

struct Task {
    id: String,
    kind: String,
    label: String,
    total: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// A Lightroom import's step-by-step progress; its status reports this instead of the counts.
    progress: Option<lightcraft_engine::lightroom_job::LightroomProgress>,
}

enum Event {
    Export {
        id: String,
        result: Result<Vec<Value>, String>,
        cancelled: bool,
    },
    Merge {
        id: String,
        job: MergeJob,
        result: Result<MergeOutput, String>,
        cancelled: bool,
    },
    Import {
        id: String,
        prepared: Prepared,
        opts: Box<ImportOptions>,
        now: String,
        album: Option<u64>,
        album_name: Option<String>,
        undo_before: usize,
        cancelled: bool,
        preserve_selection: bool,
    },
    LightroomPrepared {
        id: String,
        result: Result<lightcraft_engine::lightroom_job::PreparedLightroom, String>,
        cancelled: bool,
    },
    LightroomFinalized {
        id: String,
        report: Value,
        result: Result<(), String>,
        index_path: Option<String>,
        commit_error: Option<String>,
    },
    LightroomInspection {
        id: String,
        result: Result<Value, String>,
        cancelled: bool,
    },
    ImportReview {
        id: String,
        result: Result<ScanOutput, String>,
        cancelled: bool,
        restore_probes: std::collections::HashMap<String, lightcraft_engine::media::ProbeInfo>,
    },
    ImportReviewPreviews {
        id: String,
        candidates: Vec<ImportCandidate>,
        previews: Vec<CandidatePreview>,
        cancelled: bool,
        restore_probes: std::collections::HashMap<String, lightcraft_engine::media::ProbeInfo>,
    },
    Preview {
        id: String,
        status: Value,
        failed: usize,
        cancelled: bool,
    },
    Cull {
        id: String,
        job: CullJob,
        result: Result<CullJobResult, String>,
        cancelled: bool,
        library_id: Option<String>,
    },
    CullApply {
        id: String,
        prepared: CullApplyJob,
        params: Value,
        result: Result<CullJobResult, String>,
        cancelled: bool,
        library_id: Option<String>,
    },
}

struct CandidatePreview {
    index: usize,
    revision: String,
    handle: String,
    source: &'static str,
    width: u32,
    height: u32,
}

pub(crate) struct Tasks {
    jobs: BTreeMap<String, Task>,
    tx: SyncSender<Event>,
    rx: Receiver<Event>,
    next: u64,
    notices: Vec<String>,
    completed: VecDeque<TerminalTask>,
    pending_preview_ids: VecDeque<u64>,
    previews: crate::PreviewStore,
}

impl Tasks {
    pub(crate) fn new() -> Self {
        Self::with_store(crate::PreviewStore::default())
    }

    pub(crate) fn with_store(previews: crate::PreviewStore) -> Self {
        let (tx, rx) = sync_channel(EVENT_CAPACITY);
        Self {
            jobs: BTreeMap::new(),
            tx,
            rx,
            next: 1,
            notices: Vec::new(),
            completed: VecDeque::new(),
            pending_preview_ids: VecDeque::new(),
            previews,
        }
    }

    pub(crate) fn start_export(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.running_kind("export") {
            return Err("an export is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let mut expanded = session.export_params(params)?;
        normalize_export_params(&mut expanded);
        let opts = lightcraft_engine::export::ExportOptions::from_json(&expanded);
        let ids = export_targets(session, params);
        if ids.is_empty() {
            return Err("no photos selected for export".into());
        }
        for id in &ids {
            crate::validate_id(id.0)?;
        }
        let destination = lightcraft_engine::export::Destination {
            dir: export_directory(session, params)?,
            exact: params.get("path").and_then(Value::as_str).map(str::to_string),
        };
        if destination.dir.contains('\0') || destination.exact.as_deref().is_some_and(|path| path.contains('\0')) {
            return Err("export path contains NUL".into());
        }
        let items = lightcraft_engine::export::prepare_batch(session, &ids, &opts)?;
        let total = items.len();
        let last_dir = destination.dir.clone();
        let worker_destination = destination.clone();
        let task_id = self.new_id("export");
        let completed = Arc::new(AtomicUsize::new(0));
        let total_counter = Arc::new(AtomicUsize::new(total));
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Task {
            id: task_id.clone(),
            kind: "export".into(),
            label: "Export Photos".into(),
            total: total_counter.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-export".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("export", || {
                    let mut write = |path: &str, bytes: &[u8]| lightcraft_engine::export::write_file(path, bytes);
                    let exists = |path: &str| std::path::Path::new(path).exists();
                    let mut progress = |done: usize, _name: &str| {
                        completed.store(done, Ordering::Relaxed);
                        !cancel.load(Ordering::Relaxed)
                    };
                    lightcraft_engine::export::run_batch(items, &opts, &worker_destination, &mut write, &exists, false, &mut progress)
                })
                .unwrap_or_else(Err);
                let cancelled = cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::Export { id: id_for_worker, result, cancelled });
            })
            .map_err(|error| format!("could not start export: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        let mut last = params.clone();
        if let Some(values) = last.as_object_mut() {
            values.remove("ids");
            values.remove("path");
            values.insert("dir".into(), Value::String(last_dir));
        }
        session.last_export = Some(last);
        if let Err(error) = session.save_prefs() {
            self.notices.push(format!("export preferences were not saved: {error}"));
        }
        Ok(json!({"taskId": task_id, "total": total}))
    }

    /// Plan culling on the owner thread, then measure detached sources in cancellable worker.
    /// Catalog flags and analysis remain untouched until a caller explicitly applies proposal.
    pub(crate) fn start_cull_suggest(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.running_kind("cull") {
            return Err("a cull suggestion is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let reject_below = match params.get("rejectBelow") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                    .ok_or_else(|| "photo.cullSuggest: `rejectBelow` must be a finite number 0..100".to_string())? as f32,
            ),
        };
        let pick_best = params
            .get("pickBest")
            .map_or(Ok(false), |value| value.as_bool().ok_or_else(|| "photo.cullSuggest: `pickBest` must be a boolean".to_string()))?;
        let ids = if let Some(raw_ids) = params.get("ids") {
            let values = raw_ids.as_array().ok_or_else(|| "photo.cullSuggest: `ids` must be an array of photo ids".to_string())?;
            if values.len() > CULL_MAX_IDS {
                return Err(format!("photo.cullSuggest: too many culling photo ids: {}", values.len()));
            }
            let ids = values
                .iter()
                .map(|value| value.as_u64().map(PhotoId).ok_or_else(|| "photo.cullSuggest: `ids` must contain only unsigned photo ids".to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            let mut seen = HashSet::with_capacity(ids.len());
            for id in &ids {
                if !seen.insert(*id) {
                    return Err(format!("photo.cullSuggest: duplicate photo id {}", id.0));
                }
                if session.catalog.photo(*id).is_none() {
                    return Err(format!("photo.cullSuggest: unknown photo id {}", id.0));
                }
            }
            ids
        } else if session.selection.ids.len() > 1 {
            session.targets(params)
        } else {
            session.visible_cloned()
        };
        if ids.len() > CULL_MAX_IDS {
            return Err(format!("photo.cullSuggest: too many culling photo ids: {}", ids.len()));
        }
        for id in &ids {
            crate::validate_id(id.0)?;
        }
        let job = session.plan_cull_job(&ids, reject_below, pick_best).map_err(|error| error.to_string())?;
        let library_id = session.library.as_ref().map(|library| library.dir.to_string_lossy().into_owned());
        // Reserve one terminal unit for owner-thread validation/publication. The worker reports
        // item progress in 0..N while the task advertises N+1 units until it is finished.
        let total = Arc::new(AtomicUsize::new(job.photos.len().saturating_add(1).max(1)));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("cull");
        let task = Task {
            id: task_id.clone(),
            kind: "cull".into(),
            label: "Suggest Culling".into(),
            total: total.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let worker_total = total.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-cull".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("cull suggestion", || {
                    job.run(&|fraction, _stage| {
                        let terminal = worker_total.load(Ordering::Relaxed).saturating_sub(1);
                        completed.store((fraction.clamp(0.0, 1.0) * terminal as f32).round() as usize, Ordering::Relaxed);
                        !worker_cancel.load(Ordering::Relaxed)
                    })
                })
                .unwrap_or_else(Err);
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::Cull { id: id_for_worker, job, result, cancelled, library_id });
            })
            .map_err(|error| format!("could not start cull suggestion: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "kind": "cull", "total": total.load(Ordering::Relaxed)}))
    }

    /// Re-measure a reviewed proposal off-thread, then apply accepted flags on owner thread.
    /// The engine performs final revision/source/flag validation before one undoable commit.
    pub(crate) fn start_cull_apply(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.running_kind("cullApply") {
            return Err("a cull apply is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let prepared = session.plan_cull_apply(params).map_err(|error| error.to_string())?;
        let library_id = session.library.as_ref().map(|library| library.dir.to_string_lossy().into_owned());
        let total = Arc::new(AtomicUsize::new(prepared.job.photos.len().saturating_add(1).max(1)));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("cullApply");
        let task = Task {
            id: task_id.clone(),
            kind: "cullApply".into(),
            label: "Apply Culling".into(),
            total: total.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let worker_total = total.clone();
        let params_for_event = params.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-cull-apply".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("cull apply", || {
                    prepared.job.run(&|fraction, _stage| {
                        let terminal = worker_total.load(Ordering::Relaxed).saturating_sub(1);
                        completed.store((fraction.clamp(0.0, 1.0) * terminal as f32).round() as usize, Ordering::Relaxed);
                        !worker_cancel.load(Ordering::Relaxed)
                    })
                })
                .unwrap_or_else(Err);
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::CullApply { id: id_for_worker, prepared, params: params_for_event, result, cancelled, library_id });
            })
            .map_err(|error| format!("could not start cull apply: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "kind": "cullApply", "total": total.load(Ordering::Relaxed)}))
    }

    pub(crate) fn start_import(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        self.start_import_with_selection(session, params, false)
    }

    /// Start the engine's detached thumbnail/loupe build under the host task registry. The
    /// engine owns render/cache semantics; this monitor mirrors its counters into `status.jobs`
    /// so desktop clients get one progress surface for imports, previews and exports.
    pub(crate) fn start_preview_build(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.running_kind("preview") {
            return Err("a preview build is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let mut request = params.clone();
        if let Some(values) = request.as_object_mut() {
            // Desktop commands are always represented as a task; avoid blocking the owner
            // thread when a caller carried over the CLI-only `wait` parameter.
            values.insert("wait".into(), Value::Bool(false));
        }
        let result = session.execute("library.buildPreviews", &request).map_err(|error| error.to_string())?;
        let state = session.preview_build.clone().ok_or_else(|| "preview build did not create progress state".to_string())?;
        let total = Arc::new(AtomicUsize::new(state.total));
        let completed = Arc::new(AtomicUsize::new(state.done.load(Ordering::Relaxed)));
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("preview");
        let task = Task {
            id: task_id.clone(),
            kind: "preview".into(),
            label: "Build Previews".into(),
            total: total.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let state_for_worker = state.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-preview-progress".into())
            .spawn(move || {
                while !state_for_worker.finished.load(Ordering::Acquire) {
                    if worker_cancel.load(Ordering::Relaxed) {
                        state_for_worker.cancel.store(true, Ordering::Relaxed);
                    }
                    completed.store(state_for_worker.done.load(Ordering::Relaxed), Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                completed.store(state_for_worker.done.load(Ordering::Relaxed), Ordering::Relaxed);
                let cancelled = worker_cancel.load(Ordering::Relaxed) || state_for_worker.cancel.load(Ordering::Relaxed);
                let status = state_for_worker.json();
                let failed = state_for_worker.failed.load(Ordering::Relaxed);
                let _ = tx.send(Event::Preview { id: id_for_worker, status, failed, cancelled });
            })
            .map_err(|error| {
                state.cancel.store(true, Ordering::Relaxed);
                format!("could not start preview progress: {error}")
            })?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "kind": "preview", "total": result["total"]}))
    }

    fn start_preview_build_for_ids(&mut self, session: &mut Session, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        let params = json!({"ids": ids, "size": "standard"});
        if let Err(error) = self.start_preview_build(session, &params) {
            log::warn!("automatic preview build: {error}");
        }
    }

    fn schedule_preview_ids(&mut self, session: &mut Session, ids: &[u64]) {
        let queued: HashSet<u64> = self.pending_preview_ids.iter().copied().collect();
        let mut seen = queued;
        let mut unique = Vec::new();
        for id in ids.iter().copied().filter(|id| *id != 0) {
            if seen.insert(id) {
                unique.push(id);
            }
        }
        if unique.is_empty() {
            return;
        }
        let preview_cancelled = self.jobs.values().find(|job| job.kind == "preview").is_some_and(|job| job.cancel.load(Ordering::Relaxed))
            || session.preview_build.as_ref().is_some_and(|state| state.cancel.load(Ordering::Relaxed));
        if self.running_kind("preview") {
            if preview_cancelled {
                self.pending_preview_ids.clear();
                return;
            }
            let mut dropped = 0;
            for id in unique {
                if self.pending_preview_ids.len() >= MAX_PENDING_PREVIEW_IDS {
                    dropped += 1;
                } else {
                    self.pending_preview_ids.push_back(id);
                }
            }
            if dropped > 0 {
                self.notices.push(format!("preview queue full; dropped {dropped} photo(s)"));
                if self.notices.len() > 32 {
                    let drop_count = self.notices.len() - 32;
                    self.notices.drain(..drop_count);
                }
            }
            return;
        }
        self.start_preview_build_for_ids(session, &unique);
    }

    pub(crate) fn start_auto_import(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        self.start_import_with_selection(session, params, true)
    }

    pub(crate) fn start_import_review(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.running_import() {
            return Err("an import is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let values = params.get("paths").and_then(Value::as_array).ok_or_else(|| "library.importPreview: paths must be an array".to_string())?;
        if values.is_empty() {
            return Err("library.importPreview: no paths".into());
        }
        if values.len() > MAX_REVIEW_PATHS {
            return Err(format!("library.importPreview: at most {MAX_REVIEW_PATHS} paths"));
        }
        let mut paths = Vec::with_capacity(values.len());
        for value in values {
            let path = value.as_str().ok_or_else(|| "library.importPreview: paths must contain strings".to_string())?;
            if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
                return Err("library.importPreview: invalid path".into());
            }
            paths.push(path.to_string());
        }

        let scan_options =
            lightcraft_engine::cmd::library::import_scan_options(params, "library.importPreview").map_err(|error| error.to_string())?;
        let raw_jpeg_policy = match params.get("rawJpegPolicy").and_then(Value::as_str) {
            Some(value) => {
                RawJpegImportPolicy::parse(value).ok_or_else(|| "library.importPreview: unknown rawJpegPolicy (keepBoth|rawOnly)".to_string())?
            }
            None => RawJpegImportPolicy::default(),
        };

        // ScanInput::new drains the review cache into its detached snapshot. Restore the owner
        // cache immediately; a review must leave owner state unchanged until it completes.
        let restore_probes = session.import_probes.clone();
        let (mut input, paths) = ScanInput::new_with_options(session, &paths, scan_options);
        input.raw_jpeg_policy = raw_jpeg_policy;
        session.import_probes = restore_probes.clone();
        let progress = Arc::new(ScanProgress::default());
        let total = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("importReview");
        let task = Task {
            id: task_id.clone(),
            kind: "importReview".into(),
            label: "Review Import".into(),
            total: total.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let worker_progress = progress.clone();
        let worker_total = total.clone();
        let worker_completed = completed.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-import-review".into())
            .spawn(move || {
                let monitor_done = Arc::new(AtomicBool::new(false));
                let monitor_done_for_worker = monitor_done.clone();
                let monitor_cancel = worker_cancel.clone();
                let monitor = match std::thread::Builder::new().name("lightcraft-import-review-progress".into()).spawn(move || {
                    while !monitor_done.load(Ordering::Acquire) {
                        worker_total.store(worker_progress.total.load(Ordering::Relaxed), Ordering::Relaxed);
                        worker_completed.store(worker_progress.done.load(Ordering::Relaxed), Ordering::Relaxed);
                        if monitor_cancel.load(Ordering::Relaxed) {
                            worker_progress.cancel.store(true, Ordering::Relaxed);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    worker_total.store(worker_progress.total.load(Ordering::Relaxed), Ordering::Relaxed);
                    worker_completed.store(worker_progress.done.load(Ordering::Relaxed), Ordering::Relaxed);
                }) {
                    Ok(monitor) => monitor,
                    Err(error) => {
                        let _ = tx.send(Event::ImportReview {
                            id: id_for_worker.clone(),
                            result: Err(format!("could not start import review progress: {error}")),
                            cancelled: false,
                            restore_probes: restore_probes.clone(),
                        });
                        return;
                    }
                };
                let result = lightcraft_engine::guard::catch("import review", || lightcraft_engine::import::scan_with(input, &paths, &progress));
                monitor_done_for_worker.store(true, Ordering::Release);
                let _ = monitor.join();
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::ImportReview { id: id_for_worker, result, cancelled, restore_probes });
            })
            .map_err(|error| format!("could not start import review: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "kind": "importReview", "total": 0}))
    }

    fn start_import_candidate_previews(
        &mut self,
        session: &mut Session,
        id: String,
        candidates: Vec<ImportCandidate>,
        restore_probes: std::collections::HashMap<String, lightcraft_engine::media::ProbeInfo>,
    ) {
        let Some(cancel) = self.jobs.get(&id).map(|task| task.cancel.clone()) else { return };
        if cancel.load(Ordering::Relaxed) {
            session.import_probes = restore_probes;
            self.finish(id, Err("cancelled".into()), "importReview", true);
            return;
        }
        let jobs = candidates
            .iter()
            .take(MAX_IMPORT_PREVIEWS)
            .enumerate()
            .filter_map(|(index, candidate)| session.candidate_thumb_job(candidate, IMPORT_PREVIEW_EDGE, index as u64 + 1).map(|job| (index, job)))
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            self.finish(id, import_review_result(candidates, Vec::new()), "importReview", false);
            return;
        }
        if let Some(worker) = self.jobs.get_mut(&id).and_then(|task| task.worker.take()) {
            let _ = worker.join();
        }
        let Some(completed) = self.jobs.get(&id).map(|task| {
            task.total.store(jobs.len(), Ordering::Relaxed);
            task.completed.store(0, Ordering::Relaxed);
            task.completed.clone()
        }) else {
            return;
        };
        let tx = self.tx.clone();
        let store = self.previews.clone();
        let worker_id = id.clone();
        let restore_for_worker = restore_probes.clone();
        let worker = std::thread::Builder::new().name("lightcraft-import-review-previews".into()).spawn(move || {
            let rendered = lightcraft_engine::guard::catch("import review previews", || {
                let mut previews = Vec::new();
                for (index, job) in jobs {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = job.run();
                    let quick = result.quick;
                    if let Ok(image) = result.rendered
                        && let Ok(bytes) = crate::preview::encode_png(&image.image)
                        && let Ok(handle) = store.insert(bytes, "image/png")
                    {
                        let source = match quick {
                            Some(QuickSource::Embedded) => "embedded",
                            Some(QuickSource::Cached) => "cached",
                            Some(QuickSource::Small) | None => "quick",
                        };
                        if let (Ok(width), Ok(height)) = (u32::try_from(image.image.width), u32::try_from(image.image.height)) {
                            previews.push(CandidatePreview { index, revision: worker_id.clone(), handle, source, width, height });
                        } else {
                            let _ = store.acknowledge(&handle);
                        }
                    }
                    completed.fetch_add(1, Ordering::Relaxed);
                }
                previews
            })
            .unwrap_or_default();
            let cancelled = cancel.load(Ordering::Relaxed);
            let _ =
                tx.send(Event::ImportReviewPreviews { id: worker_id, candidates, previews: rendered, cancelled, restore_probes: restore_for_worker });
        });
        match worker {
            Ok(worker) => {
                if let Some(task) = self.jobs.get_mut(&id) {
                    task.worker = Some(worker);
                } else {
                    let _ = worker.join();
                }
            }
            Err(error) => {
                session.import_probes = restore_probes;
                self.finish(id, Err(format!("could not start import review previews: {error}")), "importReview", false);
            }
        }
    }

    fn start_import_with_selection(&mut self, session: &mut Session, params: &Value, preserve_selection: bool) -> Result<Value, String> {
        if self.running_import() {
            return Err("an import is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let request = lightcraft_engine::cmd::library::import_params(session, params).map_err(|error| error.to_string())?;
        if request.paths.is_empty() {
            return Err("no paths to import".into());
        }
        let mut job = ImportJob::new(session, request.opts.clone()).map_err(|error| error.to_string())?;
        let total = job.total.clone();
        let completed = job.done.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("import");
        let task = Task {
            id: task_id.clone(),
            kind: "import".into(),
            label: "Import Photos".into(),
            total,
            completed,
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let paths = request.paths;
        let opts = request.opts;
        let album = request.album;
        let album_name = request.album_name;
        let undo_before = session.undo.len();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-import".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("import", || job.prepare(&paths, &worker_cancel));
                match result {
                    Ok(prepared) => {
                        let cancelled = worker_cancel.load(Ordering::Relaxed);
                        let _ = tx.send(Event::Import {
                            id: id_for_worker,
                            prepared,
                            opts: Box::new(opts),
                            now: job.now().to_string(),
                            album,
                            album_name,
                            undo_before,
                            cancelled,
                            preserve_selection,
                        });
                    }
                    Err(error) => {
                        let empty = Prepared::default();
                        let _ = tx.send(Event::Import {
                            id: id_for_worker,
                            prepared: empty,
                            opts: Box::new(opts),
                            now: String::new(),
                            album,
                            album_name,
                            undo_before,
                            cancelled: worker_cancel.load(Ordering::Relaxed),
                            preserve_selection,
                        });
                        log::error!("import task failed: {error}");
                    }
                }
            })
            .map_err(|error| format!("could not start import: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "total": 0}))
    }

    pub(crate) fn start_lightroom_import(&mut self, session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.running_import() {
            return Err("an import is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let path = lightroom_path(params, "library.importLightroom")?;
        let update_existing = params.get("updateExisting").and_then(Value::as_bool).unwrap_or(false);
        let mut job = lightcraft_engine::lightroom_job::LightroomJob::new(session, path, update_existing).map_err(|error| error.to_string())?;
        let progress = job.progress();
        let (total, completed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("lightroomImport");
        let task = Task {
            id: task_id.clone(),
            kind: "lightroomImport".into(),
            label: "Import LRCAT Catalog".into(),
            total,
            completed,
            cancel: cancel.clone(),
            worker: None,
            progress: Some(progress),
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-lightroom-import".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("LRCAT import", || job.prepare(&worker_cancel))
                    .and_then(|result| result.map_err(|error| error.to_string()));
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::LightroomPrepared { id: id_for_worker, result, cancelled });
            })
            .map_err(|error| format!("could not start LRCAT import: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "kind": "lightroomImport", "total": 0}))
    }

    pub(crate) fn start_lightroom_inspection(&mut self, _session: &mut Session, params: &Value) -> Result<Value, String> {
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        let path = lightroom_path(params, "library.inspectLightroom")?;
        let total = Arc::new(AtomicUsize::new(16));
        let completed = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.new_id("lightroomInspect");
        let task = Task {
            id: task_id.clone(),
            kind: "lightroomInspect".into(),
            label: "Inspect LRCAT Catalog".into(),
            total: total.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-lightroom-inspect".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("LRCAT inspection", || {
                    lightcraft_engine::lightroom_catalog::read_with_progress(&path, &worker_cancel, &total, &completed)
                        .map(|data| lightroom_inspection_report(&data))
                })
                .unwrap_or_else(Err);
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::LightroomInspection { id: id_for_worker, result, cancelled });
            })
            .map_err(|error| format!("could not start LRCAT inspection: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "kind": "lightroomInspect", "total": 16}))
    }

    pub(crate) fn start_merge(&mut self, session: &mut Session, command: &str, params: &Value) -> Result<Value, String> {
        if self.running_kind("merge") {
            return Err("a merge is already running".into());
        }
        if self.jobs.len() >= MAX_TASKS {
            return Err("too many background tasks".into());
        }
        if let Some(raw_ids) = params.get("ids") {
            let ids = raw_ids.as_array().ok_or_else(|| "merge ids must be an array".to_string())?;
            if ids.len() > MAX_MERGE_IDS {
                return Err(format!("merge accepts at most {MAX_MERGE_IDS} photos"));
            }
        }
        let (kind, finish) = lightcraft_engine::merge::parse(command, params).map_err(|error| error.to_string())?;
        let ids = session.targets(params);
        for id in &ids {
            crate::validate_id(id.0)?;
        }
        let job = session.plan_merge(kind, finish, &ids, false).map_err(|error| error.to_string())?;
        let task_id = self.new_id("merge");
        let completed = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(100));
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Task {
            id: task_id.clone(),
            kind: "merge".into(),
            label: merge_label(command),
            total: total.clone(),
            completed: completed.clone(),
            cancel: cancel.clone(),
            worker: None,
            progress: None,
        };
        let tx = self.tx.clone();
        let id_for_worker = task_id.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("lightcraft-merge".into())
            .spawn(move || {
                let result = lightcraft_engine::guard::catch("merge", || {
                    job.run(&|fraction, _stage| {
                        let progress = (fraction.clamp(0.0, 1.0) * 100.0).round() as usize;
                        completed.store(progress, Ordering::Relaxed);
                        !worker_cancel.load(Ordering::Relaxed)
                    })
                })
                .unwrap_or_else(Err);
                let cancelled = worker_cancel.load(Ordering::Relaxed);
                let _ = tx.send(Event::Merge { id: id_for_worker, job, result, cancelled });
            })
            .map_err(|error| format!("could not start merge: {error}"))?;
        self.insert(Task { worker: Some(worker), ..task })?;
        Ok(json!({"taskId": task_id, "total": 100}))
    }

    pub(crate) fn poll(&mut self, session: &mut Session) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Export { id, result, cancelled } => self.finish(id, result.map(|files| json!({"files": files})), "export", cancelled),
                Event::Merge { id, job, result, cancelled } => {
                    let cancelled = merge_cancelled(cancelled, self.jobs.get(&id));
                    let result = if cancelled {
                        Err("cancelled".into())
                    } else {
                        result.and_then(|output| session.finish_merge(&job, output).map_err(|error| error.to_string()))
                    };
                    self.finish(id, result, "merge", cancelled);
                }
                Event::Import { id, prepared, opts, now, album, album_name, undo_before, cancelled, preserve_selection } => {
                    let cancelled = cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
                    let selection_before = preserve_selection.then(|| session.selection.clone());
                    let result = if cancelled || now.is_empty() {
                        Err("import worker failed".into())
                    } else {
                        lightcraft_engine::import::commit_prepared(session, &opts, &now, prepared)
                            .map_err(|error| error.to_string())
                            .and_then(|report| {
                                lightcraft_engine::cmd::library::import_batch_done(session, report, album, album_name.as_deref())
                                    .map_err(|error| error.to_string())
                            })
                            .inspect(|report| {
                                let n = session.undo.len().saturating_sub(undo_before);
                                session.merge_undo(n, "Import Photos");
                                if !preserve_selection
                                    && let Some(first) =
                                        report.get("imported").and_then(Value::as_array).and_then(|ids| ids.first()).and_then(Value::as_u64)
                                {
                                    session.selection = Selection::single(PhotoId(first));
                                }
                            })
                    };
                    if let Some(selection) = selection_before {
                        session.selection = selection;
                    }
                    let preview_ids = result
                        .as_ref()
                        .ok()
                        .and_then(|report| report.get("imported"))
                        .and_then(Value::as_array)
                        .map(|ids| ids.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
                        .unwrap_or_default();
                    self.finish(id, result, "import", cancelled);
                    self.schedule_preview_ids(session, &preview_ids);
                }
                Event::LightroomPrepared { id, result, cancelled } => self.poll_lightroom_prepared(session, id, result, cancelled),
                Event::LightroomFinalized { id, report, result, index_path, commit_error } => {
                    self.finish_lightroom(session, id, report, result, index_path, commit_error)
                }
                Event::LightroomInspection { id, result, cancelled } => self.finish(id, result, "lightroomInspect", cancelled),
                Event::ImportReview { id, result, cancelled, restore_probes } => {
                    let cancelled = cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
                    if cancelled {
                        session.import_probes = restore_probes;
                        self.finish(id, Err("cancelled".into()), "importReview", true);
                    } else {
                        match result {
                            Ok(output) => {
                                session.import_probes = output.probes;
                                self.start_import_candidate_previews(session, id, output.candidates, restore_probes);
                            }
                            Err(error) => {
                                session.import_probes = restore_probes;
                                self.finish(id, Err(error), "importReview", false);
                            }
                        }
                    }
                }
                Event::ImportReviewPreviews { id, candidates, previews, cancelled, restore_probes } => {
                    let cancelled = cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
                    if cancelled {
                        for preview in previews {
                            let _ = self.previews.acknowledge(&preview.handle);
                        }
                        session.import_probes = restore_probes;
                        self.finish(id, Err("cancelled".into()), "importReview", true);
                    } else {
                        let result = import_review_result(candidates, previews);
                        self.finish(id, result, "importReview", false);
                    }
                }
                Event::Preview { id, status, failed, cancelled } => {
                    let cancelled = cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
                    self.finish_preview(id, status, failed, cancelled);
                    if cancelled {
                        self.pending_preview_ids.clear();
                    } else {
                        let pending = self.pending_preview_ids.drain(..).collect::<Vec<_>>();
                        self.start_preview_build_for_ids(session, &pending);
                    }
                }
                Event::Cull { id, job, result, cancelled, library_id } => {
                    let cancelled = cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
                    let current_library_id = session.library.as_ref().map(|library| library.dir.to_string_lossy().into_owned());
                    let result = if cancelled {
                        Err("cancelled".into())
                    } else if current_library_id != library_id {
                        Err("stale culling result: library changed".into())
                    } else {
                        result.and_then(|result| session.validate_cull_job_result(&job, &result).map_err(|error| error.to_string()))
                    };
                    self.finish(id, result, "cull", cancelled);
                }
                Event::CullApply { id, prepared, params, result, cancelled, library_id } => {
                    let cancelled = cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
                    let current_library_id = session.library.as_ref().map(|library| library.dir.to_string_lossy().into_owned());
                    let result = if cancelled {
                        Err("cancelled".into())
                    } else if current_library_id != library_id {
                        Err("stale culling result: library changed".into())
                    } else {
                        result.and_then(|result| {
                            session
                                .execute_prepared_command("photo.cullApply", &params, |session| session.finish_cull_apply(prepared, result))
                                .map_err(|error| error.to_string())
                        })
                    };
                    self.finish(id, result, "cullApply", cancelled);
                }
            }
        }
    }

    fn poll_lightroom_prepared(
        &mut self,
        session: &mut Session,
        id: String,
        result: Result<lightcraft_engine::lightroom_job::PreparedLightroom, String>,
        worker_cancelled: bool,
    ) {
        if let Some(task) = self.jobs.get_mut(&id)
            && let Some(worker) = task.worker.take()
        {
            let _ = worker.join();
        }
        let cancelled = worker_cancelled || self.jobs.get(&id).is_some_and(|task| task.cancel.load(Ordering::Relaxed));
        let prepared = match result {
            Ok(prepared) if !cancelled => prepared,
            Ok(prepared) => {
                prepared.rollback();
                self.finish(id, Err("cancelled".into()), "lightroomImport", true);
                return;
            }
            Err(error) => {
                self.finish(id, Err(error), "lightroomImport", cancelled);
                return;
            }
        };
        let mut completion_out = None;
        let commit_result = session.execute_fn("library.importLightroom", |owner| {
            let completion = lightcraft_engine::lightroom_job::commit_prepared(owner, prepared)?;
            let report = completion.report.clone();
            completion_out = Some(completion);
            Ok(report)
        });
        let Some(completion) = completion_out else {
            self.finish(id, commit_result.map_err(|error| error.to_string()), "lightroomImport", false);
            return;
        };
        let commit_error = commit_result.err().map(|error| error.to_string());
        let report = completion.report;
        let index_path = completion.finalization.path().map(|path| path.to_string_lossy().to_string());
        let report_for_worker = report.clone();
        let index_path_for_worker = index_path.clone();
        let commit_error_for_worker = commit_error.clone();
        let tx = self.tx.clone();
        let id_for_worker = id.clone();
        let worker = std::thread::Builder::new().name("lightcraft-lightroom-index".into()).spawn(move || {
            let result = lightcraft_engine::guard::catch("LRCAT archive index", || completion.finalization.finish())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = tx.send(Event::LightroomFinalized {
                id: id_for_worker,
                report: report_for_worker,
                result,
                index_path: index_path_for_worker,
                commit_error: commit_error_for_worker,
            });
        });
        match worker {
            Ok(worker) => {
                if let Some(task) = self.jobs.get_mut(&id) {
                    task.completed.store(task.total.load(Ordering::Relaxed), Ordering::Relaxed);
                    task.worker = Some(worker);
                } else {
                    let _ = worker.join();
                }
            }
            Err(error) => {
                self.finish_lightroom(
                    session,
                    id,
                    report,
                    Err(format!("could not start LRCAT archive finalization: {error}")),
                    index_path,
                    commit_error,
                );
            }
        }
    }

    fn finish_lightroom(
        &mut self,
        session: &mut Session,
        id: String,
        mut report: Value,
        result: Result<(), String>,
        index_path: Option<String>,
        commit_error: Option<String>,
    ) {
        let preview_ids = lightroom_preview_ids(&report);
        let Some(mut job) = self.jobs.remove(&id) else {
            self.notices.push("unknown lightroomImport task completed".into());
            return;
        };
        if let Some(worker) = job.worker.take() {
            let _ = worker.join();
        }
        job.completed.store(job.total.load(Ordering::Relaxed), Ordering::Relaxed);
        let archive_error = match result {
            Ok(()) => None,
            Err(error) => {
                let warning = match &index_path {
                    Some(path) => format!("LRCAT import committed, but archive index could not be written at {path}: {error}"),
                    None => format!("LRCAT import committed, but archive index could not be finalized: {error}"),
                };
                append_warning(&mut report, warning.clone());
                append_index_warning(&mut report);
                Some(warning)
            }
        };
        let error =
            commit_error.map(|error| format!("LRCAT import was applied in memory, but library save failed: {error}; retry saving the library"));
        if let Some(commit_error) = &error {
            append_warning(&mut report, commit_error.clone());
        }
        let state = if error.is_some() { "failed" } else { "done" };
        let notice = error.clone().or(archive_error).unwrap_or_else(|| format!("{} finished", job.label));
        self.completed.push_back(TerminalTask { id: job.id, kind: job.kind, label: job.label, state: state.into(), result: Some(report), error });
        while self.completed.len() > MAX_COMPLETED_TASKS {
            self.completed.pop_front();
        }
        self.notices.push(notice);
        if self.notices.len() > 32 {
            let drop_count = self.notices.len() - 32;
            self.notices.drain(..drop_count);
        }
        self.schedule_preview_ids(session, &preview_ids);
    }

    pub(crate) fn cancel(&mut self, id: Option<&str>) -> Result<Value, String> {
        let mut changed = 0usize;
        for job in self.jobs.values() {
            if id.is_none_or(|wanted| wanted == job.id) {
                job.cancel.store(true, Ordering::Relaxed);
                changed += 1;
            }
        }
        Ok(json!({"cancelled": changed}))
    }

    pub(crate) fn statuses(&self) -> Vec<JobStatus> {
        self.jobs
            .values()
            .map(|job| {
                let (label, completed, total) = match &job.progress {
                    Some(progress) => lightroom_status(&job.label, progress),
                    None => (job.label.clone(), job.completed.load(Ordering::Relaxed), job.total.load(Ordering::Relaxed)),
                };
                JobStatus { id: job.id.clone(), kind: job.kind.clone(), label, completed, total, cancellable: true, error: None }
            })
            .collect()
    }

    pub(crate) fn completed_jobs(&self) -> Vec<TerminalTask> {
        self.completed.iter().cloned().collect()
    }

    pub(crate) fn running_kind(&self, kind: &str) -> bool {
        self.jobs.values().any(|job| {
            if kind == "import" { job.kind == "import" || job.kind == "importReview" || job.kind == "lightroomImport" } else { job.kind == kind }
        })
    }

    fn running_import(&self) -> bool {
        self.running_kind("import")
    }
    pub(crate) fn running(&self) -> bool {
        !self.jobs.is_empty()
    }
    pub(crate) fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    fn new_id(&mut self, kind: &str) -> String {
        let id = format!("{kind}-{}", self.next);
        self.next = self.next.saturating_add(1);
        id
    }

    fn insert(&mut self, task: Task) -> Result<(), String> {
        if self.jobs.len() >= MAX_TASKS {
            task.cancel.store(true, Ordering::Relaxed);
            if let Some(worker) = task.worker {
                let _ = worker.join();
            }
            return Err("too many background tasks".into());
        }
        self.jobs.insert(task.id.clone(), task);
        Ok(())
    }

    fn finish(&mut self, id: String, result: Result<Value, String>, kind: &str, cancelled: bool) {
        if let Some(mut job) = self.jobs.remove(&id) {
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
            job.completed.store(job.total.load(Ordering::Relaxed), Ordering::Relaxed);
            let (state, result, error, notice) = if cancelled {
                ("cancelled", None, None, format!("{} cancelled", job.label))
            } else {
                match result {
                    Ok(result) => ("done", Some(result), None, format!("{} finished", job.label)),
                    Err(error) => ("failed", None, Some(error.clone()), format!("{} failed: {error}", job.label)),
                }
            };
            self.completed.push_back(TerminalTask { id: job.id, kind: job.kind, label: job.label, state: state.into(), result, error });
            while self.completed.len() > MAX_COMPLETED_TASKS {
                self.completed.pop_front();
            }
            self.notices.push(notice);
        } else {
            self.notices.push(format!("unknown {kind} task completed"));
        }
        if self.notices.len() > 32 {
            let drop_count = self.notices.len() - 32;
            self.notices.drain(..drop_count);
        }
    }

    fn finish_preview(&mut self, id: String, status: Value, failed: usize, cancelled: bool) {
        let Some(mut job) = self.jobs.remove(&id) else {
            self.notices.push("unknown preview task completed".into());
            return;
        };
        if let Some(worker) = job.worker.take() {
            let _ = worker.join();
        }
        job.completed.store(job.total.load(Ordering::Relaxed), Ordering::Relaxed);
        let (state, result, error, notice) = if cancelled {
            ("cancelled", None, None, format!("{} cancelled", job.label))
        } else if failed > 0 {
            let error = format!("{failed} preview job(s) failed");
            ("failed", Some(status), Some(error.clone()), format!("{} failed: {error}", job.label))
        } else {
            ("done", Some(status), None, format!("{} finished", job.label))
        };
        self.completed.push_back(TerminalTask { id: job.id, kind: job.kind, label: job.label, state: state.into(), result, error });
        while self.completed.len() > MAX_COMPLETED_TASKS {
            self.completed.pop_front();
        }
        self.notices.push(notice);
        if self.notices.len() > 32 {
            let drop_count = self.notices.len() - 32;
            self.notices.drain(..drop_count);
        }
    }
}

fn import_review_result(candidates: Vec<ImportCandidate>, previews: Vec<CandidatePreview>) -> Result<Value, String> {
    let mut values = serde_json::to_value(candidates).map_err(|error| format!("could not serialize import candidates: {error}"))?;
    let (duplicates, scanned) = {
        let Some(items) = values.as_array_mut() else { return Err("import candidates did not serialize as an array".into()) };
        for preview in previews {
            let Some(candidate) = items.get_mut(preview.index).and_then(Value::as_object_mut) else { continue };
            candidate.insert(
                "preview".into(),
                json!({
                    "handle": preview.handle,
                    "revision": preview.revision,
                    "state": "provisional",
                    "source": preview.source,
                    "width": preview.width,
                    "height": preview.height,
                    "encoding": "png",
                }),
            );
        }
        let duplicates = items.iter().filter(|candidate| candidate.get("duplicate").is_some_and(|value| !value.is_null())).count();
        (duplicates, items.len())
    };
    Ok(json!({"candidates": values, "duplicates": duplicates, "scanned": scanned}))
}

fn export_targets(session: &mut Session, params: &Value) -> Vec<PhotoId> {
    if let Some(ids) = params.get("ids").and_then(Value::as_array) {
        return ids.iter().filter_map(Value::as_u64).map(PhotoId).collect();
    }
    if let Some(id) = params.get("id").and_then(Value::as_u64) {
        return vec![PhotoId(id)];
    }
    if session.selection.ids.is_empty() {
        return session.active().into_iter().collect();
    }
    let selected: HashSet<PhotoId> = session.selection.ids.iter().copied().collect();
    let visible = session.visible_cloned();
    let mut ids: Vec<PhotoId> = visible.iter().copied().filter(|id| selected.contains(id)).collect();
    let shown: HashSet<PhotoId> = ids.iter().copied().collect();
    ids.extend(session.selection.ids.iter().copied().filter(|id| !shown.contains(id)));
    ids
}

fn export_directory(session: &Session, params: &Value) -> Result<String, String> {
    if let Some(value) = params.get("dir") {
        let dir = value.as_str().ok_or_else(|| "Choose an export folder first.".to_string())?;
        if dir.trim().is_empty() {
            return Err("Choose an export folder first.".into());
        }
        return Ok(dir.to_string());
    }
    session
        .last_export
        .as_ref()
        .and_then(|last| last.get("dir"))
        .and_then(Value::as_str)
        .filter(|dir| !dir.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Choose an export folder first.".into())
}

fn merge_label(command: &str) -> String {
    match command {
        "merge.hdr" => "HDR Merge".into(),
        "merge.panorama" => "Panorama Merge".into(),
        "merge.hdrPanorama" => "HDR Panorama Merge".into(),
        _ => "Photo Merge".into(),
    }
}

fn merge_cancelled(worker_cancelled: bool, task: Option<&Task>) -> bool {
    worker_cancelled || task.is_some_and(|task| task.cancel.load(Ordering::Relaxed))
}

/// A Lightroom import's status: the step under way with its own count in the label, and the whole
/// preparation as thousandths, so a long read of the originals moves the bar instead of looking hung.
fn lightroom_status(label: &str, progress: &lightcraft_engine::lightroom_job::LightroomProgress) -> (String, usize, usize) {
    use lightcraft_engine::lightroom_job::LightroomStep;
    let (step, done, total) = progress.step();
    let name = match step {
        LightroomStep::Catalog => "Reading the catalog",
        LightroomStep::ReadPhotos => "Reading photos",
        LightroomStep::AddPhotos => "Adding photos",
    };
    let label = if total > 0 { format!("{label}: {name} ({done} of {total})") } else { format!("{label}: {name}") };
    let fraction = progress.fraction();
    let completed = if fraction.is_finite() { (fraction.clamp(0.0, 1.0) * 1000.0).round() as usize } else { 0 };
    (label, completed, 1000)
}

fn lightroom_path(params: &Value, command: &str) -> Result<std::path::PathBuf, String> {
    let path = params.get("path").and_then(Value::as_str).ok_or_else(|| format!("{command} requires path"))?;
    if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
        return Err(format!("invalid {command} path"));
    }
    Ok(std::path::PathBuf::from(path))
}

fn lightroom_inspection_report(data: &lightcraft_engine::lightroom_catalog::CatalogImport) -> Value {
    let collections = data.collections.iter().filter(|row| row.get("systemOnly").and_then(Value::as_f64).unwrap_or(0.0) == 0.0).count();
    let virtual_copies = data.photos.iter().filter(|photo| photo.image.get("masterImage").and_then(Value::as_i64).unwrap_or(0) != 0).count();
    let missing = data
        .photos
        .iter()
        .filter(|photo| !std::path::Path::new(&photo.path).is_file())
        .map(|photo| Value::String(photo.path.clone()))
        .collect::<Vec<_>>();
    json!({
        "source": data.source,
        "photos": data.photos.len(),
        "collections": collections,
        "virtualCopies": virtual_copies,
        "missing": missing,
        "warnings": data.warnings,
    })
}

fn lightroom_preview_ids(report: &Value) -> Vec<u64> {
    let Some(mapping) = report.get("mapping").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut ids = mapping.values().filter_map(Value::as_u64).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn append_warning(report: &mut Value, warning: String) {
    let Some(object) = report.as_object_mut() else { return };
    match object.get_mut("warnings") {
        Some(Value::Array(warnings)) => warnings.push(Value::String(warning)),
        _ => {
            object.insert("warnings".into(), json!([warning]));
        }
    }
}

fn append_index_warning(report: &mut Value) {
    let Some(object) = report.as_object_mut() else { return };
    object.insert(
        "indexWarning".into(),
        Value::String("archive index finalization failed; retry import after checking file permissions and free space".into()),
    );
}

fn normalize_export_params(params: &mut Value) {
    let Some(values) = params.as_object_mut() else { return };
    if values.get("naming").is_none()
        && let Some(rename) = values.remove("rename")
    {
        values.insert("naming".into(), rename);
    }
    let full_size = values.get("fullSize").and_then(Value::as_bool).unwrap_or(false);
    let mode = values.get("resizeMode").and_then(Value::as_str).map(str::to_string);
    let size = values.get("resizeValue").cloned();
    for key in ["fullSize", "resizeMode", "resizeValue"] {
        values.remove(key);
    }
    if full_size {
        for key in ["resize", "longEdge", "shortEdge", "width", "height", "megapixels", "percent"] {
            values.remove(key);
        }
        values.insert("longEdge".into(), Value::from(0));
    } else if let (Some(mode), Some(size)) = (mode, size) {
        for key in ["resize", "longEdge", "shortEdge", "width", "height", "megapixels", "percent"] {
            values.remove(key);
        }
        let key = match mode.as_str() {
            "longEdge" => Some("longEdge"),
            "shortEdge" => Some("shortEdge"),
            "width" => Some("width"),
            "height" => Some("height"),
            "megapixels" => Some("megapixels"),
            "percent" => Some("percent"),
            _ => None,
        };
        if let Some(key) = key {
            values.insert(key.into(), size);
        }
    } else if !["resize", "longEdge", "shortEdge", "width", "height", "megapixels", "percent"].iter().any(|key| values.contains_key(*key)) {
        values.insert("longEdge".into(), Value::from(2048));
    }
    if values.get("format").is_none() {
        let extension = values.get("path").and_then(Value::as_str).and_then(|path| path.rsplit_once('.').map(|(_, ext)| ext.to_string()));
        if let Some(extension) = extension {
            values.insert("format".into(), Value::String(extension));
        }
    }
}

impl Default for Tasks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Tasks {
    pub(crate) fn hold_for_test(&mut self) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        let id = self.new_id("test");
        let cancel = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = release.clone();
        let worker_started = started.clone();
        let tx = self.tx.clone();
        let worker_id = id.clone();
        let worker = std::thread::spawn(move || {
            worker_started.store(true, Ordering::Release);
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let _ = tx.send(Event::Export { id: worker_id, result: Ok(Vec::new()), cancelled: true });
        });
        let task = Task {
            id: id.clone(),
            kind: "test".into(),
            label: "Test task".into(),
            total: Arc::new(AtomicUsize::new(1)),
            completed: Arc::new(AtomicUsize::new(0)),
            cancel,
            worker: Some(worker),
            progress: None,
        };
        self.jobs.insert(id, task);
        (started, release)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct ReleaseProbe(Arc<AtomicBool>);

    impl Drop for ReleaseProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn finished_task(tasks: &mut Tasks, id: &str, result: Result<Value, String>) {
        tasks.jobs.insert(
            id.into(),
            Task {
                id: id.into(),
                kind: "merge".into(),
                label: "Merge Photos".into(),
                total: Arc::new(AtomicUsize::new(1)),
                completed: Arc::new(AtomicUsize::new(0)),
                cancel: Arc::new(AtomicBool::new(false)),
                worker: None,
                progress: None,
            },
        );
        tasks.finish(id.into(), result, "merge", false);
    }

    #[test]
    fn lightroom_status_names_the_step_and_moves_forward() {
        let progress = lightcraft_engine::lightroom_job::LightroomProgress::default();
        let (label, completed, total) = lightroom_status("Import LRCAT Catalog", &progress);
        assert_eq!((label.as_str(), completed, total), ("Import LRCAT Catalog: Reading the catalog", 0, 1000));
        progress.catalog_total.store(14, Ordering::Relaxed);
        progress.catalog_done.store(7, Ordering::Relaxed);
        let (label, completed, _) = lightroom_status("Import LRCAT Catalog", &progress);
        assert_eq!(label, "Import LRCAT Catalog: Reading the catalog (7 of 14)");
        assert!(completed > 0 && completed < 1000);
    }

    fn task_with_cancel(cancelled: bool) -> Task {
        Task {
            id: "merge-test-1".into(),
            kind: "merge".into(),
            label: "Merge test".into(),
            total: Arc::new(AtomicUsize::new(100)),
            completed: Arc::new(AtomicUsize::new(100)),
            cancel: Arc::new(AtomicBool::new(cancelled)),
            worker: None,
            progress: None,
        }
    }

    fn preview_task(id: &str, cancelled: bool) -> Task {
        Task {
            id: id.into(),
            kind: "preview".into(),
            label: "Build Previews".into(),
            total: Arc::new(AtomicUsize::new(3)),
            completed: Arc::new(AtomicUsize::new(0)),
            cancel: Arc::new(AtomicBool::new(cancelled)),
            worker: None,
            progress: None,
        }
    }

    fn wait_for_flag(flag: &AtomicBool, timeout: Duration, label: &str) {
        let started = Instant::now();
        while !flag.load(Ordering::Acquire) {
            if started.elapsed() >= timeout {
                panic!("{label}: flag was not set within {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_terminal(tasks: &mut Tasks, session: &mut Session, id: &str, timeout: Duration, label: &str) -> TerminalTask {
        let started = Instant::now();
        loop {
            tasks.poll(session);
            if let Some(task) = tasks.completed_jobs().into_iter().find(|task| task.id == id) {
                return task;
            }
            if started.elapsed() >= timeout {
                let status = tasks
                    .statuses()
                    .into_iter()
                    .find(|job| job.id == id)
                    .map(|job| format!("progress={}/{}", job.completed, job.total))
                    .unwrap_or_else(|| "task absent from running & completed records".into());
                panic!("{label}: task {id} did not reach terminal state within {timeout:?}; {status}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn merge_completion_cancel_wins_worker_success_race() {
        let task = task_with_cancel(true);
        assert!(merge_cancelled(false, Some(&task)));
        assert!(merge_cancelled(true, Some(&task_with_cancel(false))));
        assert!(!merge_cancelled(false, Some(&task_with_cancel(false))));
    }

    #[test]
    fn terminal_record_survives_completion_before_first_snapshot() {
        let mut tasks = Tasks::new();
        finished_task(&mut tasks, "merge-early", Ok(json!({"id": 41, "path": "merged.dng"})));
        let completed = tasks.completed_jobs();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, "merge-early");
        assert_eq!(completed[0].state, "done");
        assert_eq!(completed[0].result.as_ref().and_then(|value| value.get("id")).and_then(Value::as_u64), Some(41));
    }

    #[test]
    fn cull_suggest_returns_task_status_and_read_only_terminal_proposal() {
        let mut tasks = Tasks::new();
        let mut session = Session::with_demo();
        let before = session.catalog.to_snapshot();
        let ids: Vec<_> = session.visible_cloned().into_iter().take(1).collect();
        assert_eq!(ids.len(), 1, "demo fixture provides one real culling source");
        let expected_total = ids.len() + 1;
        let started = tasks
            .start_cull_suggest(&mut session, &json!({"ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(), "rejectBelow": 100.0, "pickBest": true}));
        assert!(started.is_ok(), "cull suggestion should start: {started:?}");
        let Ok(started) = started else { return };
        let task_id = started.get("taskId").and_then(Value::as_str).expect("cull task id");
        assert_eq!(started.get("total").and_then(Value::as_u64), Some(expected_total as u64));
        assert!(tasks.statuses().iter().any(|job| job.id == task_id && job.kind == "cull"));
        let terminal = wait_for_terminal(&mut tasks, &mut session, task_id, Duration::from_secs(10), "cull suggestion");
        assert!(!tasks.running(), "cull worker must reach terminal state");
        assert_eq!(terminal.state, "done");
        let proposal = terminal.result.as_ref().and_then(|value| value.get("proposal")).expect("terminal result carries proposal");
        assert_eq!(proposal["photos"].as_array().map(Vec::len), Some(ids.len()), "proposal covers bounded input ids");
        assert_eq!(session.catalog.to_snapshot(), before, "suggestion does not mutate catalog");
    }

    #[test]
    fn cull_suggest_rejects_malformed_explicit_ids() {
        let mut tasks = Tasks::new();
        let mut session = Session::with_demo();
        let first = session.visible_cloned().first().copied().expect("demo photo").0;
        let cases = [
            (json!({"ids": null}), "must be an array"),
            (json!({"ids": "1"}), "must be an array"),
            (json!({"ids": ["1"]}), "must contain only unsigned"),
            (json!({"ids": [first, first]}), "duplicate photo id"),
            (json!({"ids": [9_999_999_u64]}), "unknown photo id"),
        ];
        for (params, expected) in cases {
            let error = tasks.start_cull_suggest(&mut session, &params).expect_err("malformed ids must fail before starting a task");
            assert!(error.contains(expected), "{error:?} should mention {expected:?}");
            assert!(tasks.statuses().is_empty());
        }
    }

    #[test]
    fn cull_cancel_wins_before_terminal_publication() {
        let mut tasks = Tasks::new();
        let mut session = Session::with_demo();
        let ids = session.visible_cloned();
        let job = session.plan_cull_job(&ids, None, false).expect("cull job plan");
        let id = "cull-cancel-1".to_string();
        tasks.jobs.insert(
            id.clone(),
            Task {
                id: id.clone(),
                kind: "cull".into(),
                label: "Suggest Culling".into(),
                total: Arc::new(AtomicUsize::new(1)),
                completed: Arc::new(AtomicUsize::new(0)),
                cancel: Arc::new(AtomicBool::new(false)),
                worker: None,
                progress: None,
            },
        );
        assert_eq!(tasks.cancel(Some(&id)).expect("cull cancel")["cancelled"], 1);
        let _ = tasks.tx.send(Event::Cull { id: id.clone(), job, result: Err("cancelled".into()), cancelled: true, library_id: None });
        tasks.poll(&mut session);
        let terminal = tasks.completed_jobs().into_iter().find(|task| task.id == id).expect("cancelled cull terminal record");
        assert_eq!(terminal.state, "cancelled");
        assert!(terminal.result.is_none());
    }

    #[test]
    fn cull_apply_remeasures_then_commits_one_undoable_batch() {
        let mut tasks = Tasks::new();
        let mut session = Session::with_demo();
        let ids: Vec<_> = session.visible_cloned().into_iter().take(1).collect();
        assert_eq!(ids.len(), 1, "demo fixture provides one real culling source");
        let suggestion = session
            .execute("photo.cullSuggest", &json!({"ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(), "rejectBelow": 100.0, "pickBest": true}))
            .expect("cull proposal");
        let proposal = suggestion.get("proposal").cloned().expect("nested cull proposal");
        assert_eq!(proposal["photos"].as_array().map(Vec::len), Some(ids.len()));
        let accepted = proposal
            .get("photos")
            .and_then(Value::as_array)
            .and_then(|photos| {
                photos.iter().find_map(|photo| {
                    let flag = photo.get("proposedFlag").and_then(Value::as_str)?;
                    if flag != "pick" && flag != "reject" {
                        return None;
                    }
                    let id = photo.get("id")?;
                    Some(json!({"id": id, "flag": flag}))
                })
            })
            .expect("proposal should include an explicit flag");
        let undo_before = session.undo.len();
        let started = tasks.start_cull_apply(&mut session, &json!({"proposal": proposal, "accept": [accepted]}));
        assert!(started.is_ok(), "cull apply should start: {started:?}");
        let Ok(started) = started else { return };
        let task_id = started.get("taskId").and_then(Value::as_str).expect("cull apply task id");
        assert_eq!(started.get("kind").and_then(Value::as_str), Some("cullApply"));
        assert_eq!(started.get("total").and_then(Value::as_u64), Some(2), "one photo plus terminal publication");
        let terminal = wait_for_terminal(&mut tasks, &mut session, task_id, Duration::from_secs(10), "cull apply");
        assert_eq!(terminal.state, "done");
        assert_eq!(terminal.result.as_ref().and_then(|value| value.get("accepted")).and_then(Value::as_u64), Some(1));
        assert_eq!(session.undo.len(), undo_before + 1, "accepted flags are one undoable batch");
    }

    #[test]
    fn terminal_records_keep_bounded_recent_history() {
        let mut tasks = Tasks::new();
        for index in 0..(MAX_COMPLETED_TASKS + 3) {
            finished_task(&mut tasks, &format!("merge-{index}"), Ok(Value::Null));
        }
        let completed = tasks.completed_jobs();
        assert_eq!(completed.len(), MAX_COMPLETED_TASKS);
        assert_eq!(completed.first().map(|task| task.id.as_str()), Some("merge-3"));
        assert_eq!(completed.last().map(|task| task.id.as_str()), Some("merge-66"));
    }

    #[test]
    fn preview_ids_queue_behind_existing_job_with_bound_and_deduplication() {
        let mut tasks = Tasks::new();
        tasks.jobs.insert("preview-1".into(), preview_task("preview-1", false));
        let mut session = Session::new();
        tasks.schedule_preview_ids(&mut session, &[7, 8, 7]);
        assert_eq!(tasks.pending_preview_ids.iter().copied().collect::<Vec<_>>(), vec![7, 8]);
        tasks.schedule_preview_ids(&mut session, &[8, 9]);
        assert_eq!(tasks.pending_preview_ids.iter().copied().collect::<Vec<_>>(), vec![7, 8, 9]);
        tasks.pending_preview_ids.clear();
        let many = (1..=(MAX_PENDING_PREVIEW_IDS as u64 + 4)).collect::<Vec<_>>();
        tasks.schedule_preview_ids(&mut session, &many);
        assert_eq!(tasks.pending_preview_ids.len(), MAX_PENDING_PREVIEW_IDS);
    }

    #[test]
    fn preview_terminal_failure_keeps_failure_state_and_cancel_drops_queue() {
        let mut tasks = Tasks::new();
        tasks.jobs.insert("preview-1".into(), preview_task("preview-1", false));
        let _ = tasks.tx.send(Event::Preview {
            id: "preview-1".into(),
            status: json!({"total": 3, "done": 3, "failed": 2}),
            failed: 2,
            cancelled: false,
        });
        let mut session = Session::new();
        tasks.poll(&mut session);
        assert_eq!(tasks.completed_jobs()[0].state, "failed");
        assert_eq!(tasks.completed_jobs()[0].error.as_deref(), Some("2 preview job(s) failed"));

        tasks.jobs.insert("preview-2".into(), preview_task("preview-2", false));
        tasks.pending_preview_ids.push_back(9);
        assert_eq!(tasks.cancel(Some("preview-2")).expect("cancel preview")["cancelled"], 1);
        let _ = tasks.tx.send(Event::Preview { id: "preview-2".into(), status: json!({"done": 3}), failed: 0, cancelled: true });
        tasks.poll(&mut session);
        assert_eq!(tasks.completed_jobs().last().map(|task| task.state.as_str()), Some("cancelled"));
        assert!(tasks.pending_preview_ids.is_empty());
    }

    #[test]
    fn import_review_result_marks_quick_handles_as_provisional() {
        let candidate = ImportCandidate { path: "card/IMG_0001.CR3".into(), name: "IMG_0001.CR3".into(), format: "CR3".into(), ..Default::default() };
        let value = import_review_result(
            vec![candidate],
            vec![CandidatePreview {
                index: 0,
                revision: "importReview-1".into(),
                handle: "lc-preview-0000000000000001".into(),
                source: "embedded",
                width: 256,
                height: 192,
            }],
        )
        .expect("candidate preview result should serialize");
        assert_eq!(value["candidates"][0]["preview"]["state"], "provisional");
        assert_eq!(value["candidates"][0]["preview"]["source"], "embedded");
        assert_eq!(value["candidates"][0]["preview"]["revision"], "importReview-1");
        assert_eq!(value["candidates"][0]["preview"]["handle"], "lc-preview-0000000000000001");
    }

    #[test]
    fn preview_queue_starts_next_host_job_after_terminal_event() {
        let mut tasks = Tasks::new();
        tasks.jobs.insert("preview-1".into(), preview_task("preview-1", false));
        tasks.pending_preview_ids.push_back(1);
        let mut session = Session::with_demo();
        let _ = tasks.tx.send(Event::Preview { id: "preview-1".into(), status: json!({"done": 3}), failed: 0, cancelled: false });
        tasks.poll(&mut session);
        assert!(
            tasks.statuses().iter().any(|job| job.kind == "preview")
                || tasks.completed_jobs().iter().filter(|job| job.kind == "preview").count() >= 2
        );
    }

    #[test]
    fn blank_export_directory_requires_user_choice() {
        let mut session = Session::new();
        let expected = "Choose an export folder first.";
        assert_eq!(export_directory(&session, &json!({})).expect_err("first export must require a folder"), expected);
        assert_eq!(export_directory(&session, &json!({"dir": "/chosen"})).expect("explicit export folder"), "/chosen");
        session.last_export = Some(json!({"dir": "/previous"}));
        assert_eq!(export_directory(&session, &json!({})).expect("previous export folder"), "/previous");
        assert_eq!(export_directory(&session, &json!({"dir": "  "})).expect_err("blank export destination must be rejected"), expected);
    }

    #[test]
    fn lightroom_inspection_report_counts_virtual_copies_and_preserves_schema() {
        let data = lightcraft_engine::lightroom_catalog::CatalogImport {
            source: "catalog.lrcat".into(),
            photos: vec![
                lightcraft_engine::lightroom_catalog::CatalogPhoto {
                    source_id: 1,
                    uuid: "master".into(),
                    path: "/missing/master.raw".into(),
                    image: serde_json::from_value(json!({"id_local": 1})).unwrap_or_default(),
                    settings: String::new(),
                    xmp: String::new(),
                    keywords: Vec::new(),
                    history: Vec::new(),
                    snapshots: Vec::new(),
                },
                lightcraft_engine::lightroom_catalog::CatalogPhoto {
                    source_id: 2,
                    uuid: "copy".into(),
                    path: "/missing/master.raw".into(),
                    image: serde_json::from_value(json!({"id_local": 2, "masterImage": 1})).unwrap_or_default(),
                    settings: String::new(),
                    xmp: String::new(),
                    keywords: Vec::new(),
                    history: Vec::new(),
                    snapshots: Vec::new(),
                },
            ],
            collections: vec![
                serde_json::from_value(json!({"name": "Visible"})).unwrap_or_default(),
                serde_json::from_value(json!({"systemOnly": 1})).unwrap_or_default(),
            ],
            members: Vec::new(),
            collection_content: Vec::new(),
            warnings: vec!["metadata warning".into()],
        };
        let report = lightroom_inspection_report(&data);
        assert_eq!(report["source"], "catalog.lrcat");
        assert_eq!(report["photos"], 2);
        assert_eq!(report["collections"], 1);
        assert_eq!(report["virtualCopies"], 1);
        assert_eq!(report["missing"].as_array().map(Vec::len), Some(2));
        assert_eq!(report["warnings"][0], "metadata warning");
    }

    #[test]
    fn lightroom_mapping_ids_are_deduplicated_for_preview_queue() {
        assert_eq!(lightroom_preview_ids(&json!({"mapping": {"1": 9, "2": 7, "3": 9}})), vec![7, 9]);
        assert!(lightroom_preview_ids(&json!({"mapping": []})).is_empty());
    }

    #[test]
    fn lightroom_archive_failure_keeps_committed_report_and_actionable_warning() {
        let mut tasks = Tasks::new();
        tasks.jobs.insert(
            "lightroomImport-1".into(),
            Task {
                id: "lightroomImport-1".into(),
                kind: "lightroomImport".into(),
                label: "Import LRCAT Catalog".into(),
                total: Arc::new(AtomicUsize::new(1)),
                completed: Arc::new(AtomicUsize::new(0)),
                cancel: Arc::new(AtomicBool::new(false)),
                worker: None,
                progress: None,
            },
        );
        let mut session = Session::new();
        tasks.finish_lightroom(
            &mut session,
            "lightroomImport-1".into(),
            json!({"source": "catalog.lrcat", "photos": 2, "warnings": []}),
            Err("permission denied".into()),
            Some("/library/Interop/lightroom-index.json".into()),
            None,
        );
        let completed = tasks.completed_jobs();
        assert_eq!(completed[0].state, "done");
        assert_eq!(completed[0].result.as_ref().and_then(|value| value.get("photos")).and_then(Value::as_u64), Some(2));
        assert!(completed[0].result.as_ref().is_some_and(|value| value.get("indexWarning").is_some()));
        assert!(completed[0].error.is_none(), "archive warnings must not mark a committed import as failed");
        let warnings =
            completed[0].result.as_ref().and_then(|value| value.get("warnings")).and_then(Value::as_array).expect("retained import warnings");
        assert!(warnings.iter().filter_map(Value::as_str).any(|warning| {
            warning.contains("permission denied") && warning.contains("/library/Interop/lightroom-index.json") && warning.contains("import committed")
        }));
        assert!(tasks.take_notices().iter().any(|notice| notice.contains("permission denied")));
    }

    #[test]
    fn import_review_is_cancellable_without_owner_mutation_and_reports_candidates() {
        let root = std::env::temp_dir().join(format!(
            "lightcraft-import-review-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or_default()
        ));
        assert!(fs::create_dir_all(&root).is_ok());
        let path = root.join("review.png");
        assert!(fs::write(&path, [1_u8, 2, 3, 4]).is_ok());

        let mut session = Session::new().with_fs();
        let selection_before = session.selection.clone();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let _release_probe = ReleaseProbe(release.clone());
        let probe_entered = entered.clone();
        let probe_release = release.clone();
        session.media.file_probe = Some(Arc::new(move |_| {
            probe_entered.store(true, Ordering::Release);
            while !probe_release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(lightcraft_engine::media::ProbeInfo {
                format: "PNG".into(),
                kind: lightcraft_catalog::MediaKind::Image,
                width: 1,
                height: 1,
                file_size: 4,
                ..Default::default()
            })
        }));
        let mut tasks = Tasks::new();
        let response = tasks.start_import_review(&mut session, &json!({"paths": [path.to_string_lossy()]})).expect("review task starts");
        let task_id = response["taskId"].as_str().expect("task id").to_string();
        wait_for_flag(&entered, Duration::from_secs(5), "import review probe");
        assert_eq!(tasks.cancel(Some(&task_id)).expect("cancel review")["cancelled"], 1);
        assert_eq!(session.selection, selection_before, "cancellation must not change selection");
        release.store(true, Ordering::Release);
        let cancelled = wait_for_terminal(&mut tasks, &mut session, &task_id, Duration::from_secs(10), "cancelled import review");
        assert_eq!(cancelled.state, "cancelled");

        session.media.file_probe = Some(Arc::new(|_| {
            Ok(lightcraft_engine::media::ProbeInfo {
                format: "PNG".into(),
                kind: lightcraft_catalog::MediaKind::Image,
                width: 1,
                height: 1,
                file_size: 4,
                ..Default::default()
            })
        }));
        let response = tasks.start_import_review(&mut session, &json!({"paths": [path.to_string_lossy()]})).expect("second review task starts");
        let task_id = response["taskId"].as_str().expect("task id").to_string();
        let completed = wait_for_terminal(&mut tasks, &mut session, &task_id, Duration::from_secs(10), "import review");
        assert_eq!(completed.state, "done");
        assert_eq!(completed.result.as_ref().and_then(|value| value.get("scanned")).and_then(Value::as_u64), Some(1));
        assert_eq!(completed.result.as_ref().and_then(|value| value.get("duplicates")).and_then(Value::as_u64), Some(0));
        assert_eq!(completed.result.as_ref().and_then(|value| value.get("candidates")).and_then(Value::as_array).map(Vec::len), Some(1));
        assert_eq!(session.selection, selection_before, "review completion must not change selection");
        let _ = fs::remove_dir_all(root);
    }
}
