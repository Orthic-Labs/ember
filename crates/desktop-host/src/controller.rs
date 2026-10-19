use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use lightcraft_engine::Session;

use crate::auto_import::AutoImport;
use crate::merge::{MergePreviewCancelRequest, MergePreviews};
use crate::tasks::Tasks;
use crate::{HostOptions, MergePreviewDescriptor, MergePreviewRequest, PreviewDescriptor, PreviewRequest, PreviewStore, Renderer, Request};

const TICK: Duration = Duration::from_millis(250);
const COMMAND_POLL: Duration = Duration::from_millis(20);
const PREVIEW_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

pub struct Controller {
    session: Session,
    renderer: Renderer,
    merge_previews: MergePreviews,
    tasks: Tasks,
    auto_import: AutoImport,
    snapshot_cache: crate::snapshot::CatalogSnapshotCache,
    preferences: Map<String, Value>,
    notices: Vec<String>,
    last_error: Option<String>,
    segmenter_dir: Option<PathBuf>,
    segmenter_mirrors_file: Option<PathBuf>,
    sam_validation: Option<SamValidationTask>,
    sam_validation_status: Option<SamValidationStatus>,
}

struct SamValidationTask {
    path: PathBuf,
    cancel: std::sync::Arc<AtomicBool>,
    progress: std::sync::Arc<AtomicU64>,
    result: Receiver<Result<(), String>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

struct SamValidationStatus {
    path: PathBuf,
    done: u64,
    total: u64,
    error: Option<String>,
    finished: bool,
}

impl Controller {
    pub(crate) fn new(options: HostOptions, store: PreviewStore) -> Result<Self, String> {
        let HostOptions { library_path, demo, demo_count, sam3_dir, sam3_mirrors_file } = options;
        let segmenter_dir = sam3_dir
            .or_else(|| std::env::var_os("LIGHTCRAFT_SAM3_DIR").map(PathBuf::from))
            .or_else(|| lightcraft_engine::camera_profiles::config_dir().map(|dir| dir.join("models").join("sam3")));
        let segmenter_mirrors_file =
            sam3_mirrors_file.or_else(|| lightcraft_engine::camera_profiles::config_dir().map(|dir| dir.join("models").join("sam3-mirrors.txt")));
        let mut session = if let Some(path) = library_path {
            let mut session = Session::new().with_fs().with_system_clock();
            session.open_library(&path, demo).map_err(|error| error.to_string())?;
            session
        } else if demo {
            Session::with_demo().with_fs().with_system_clock()
        } else {
            Session::new().with_fs().with_system_clock()
        };
        configure_segmenter(&mut session, &segmenter_dir, &segmenter_mirrors_file);
        if demo_count > 0 {
            let visible = session.visible_cloned();
            if demo_count < visible.len() {
                for id in visible.into_iter().skip(demo_count) {
                    let _ = session.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id });
                }
            }
        }
        let notices = session.take_library_warnings();
        Ok(Self {
            session,
            renderer: Renderer::new(store.clone()),
            merge_previews: MergePreviews::new(store.clone()),
            tasks: Tasks::with_store(store),
            auto_import: AutoImport::new(),
            snapshot_cache: crate::snapshot::CatalogSnapshotCache::default(),
            preferences: Map::new(),
            notices,
            last_error: None,
            segmenter_dir,
            segmenter_mirrors_file,
            sam_validation: None,
            sam_validation_status: None,
        })
    }

    pub(crate) fn serve(&mut self, rx: Receiver<Request>) {
        let mut next_tick = Instant::now() + TICK;
        loop {
            let timeout = next_tick.saturating_duration_since(Instant::now()).min(COMMAND_POLL);
            match rx.recv_timeout(timeout) {
                Ok(request) => {
                    let stop = self.handle(request);
                    self.poll();
                    if Instant::now() >= next_tick {
                        self.session.persist_if_dirty();
                        next_tick = Instant::now() + TICK;
                    }
                    if stop {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.poll();
                    if Instant::now() >= next_tick {
                        self.session.persist_if_dirty();
                        next_tick = Instant::now() + TICK;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // This host owner is disposable; process-wide GPU shutdown belongs to the
                    // application lifecycle, so later hosts can still create/use the device.
                    let _ = self.tasks.cancel(None);
                    if let Err(error) = self.auto_import.stop() {
                        log::error!("desktop auto import worker did not stop: {error}");
                    }
                    while self.tasks.running() {
                        self.poll();
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if let Err(error) = cancel_preview_build(&self.session, PREVIEW_SHUTDOWN_WAIT) {
                        log::error!("desktop owner disconnected before preview build stopped: {error}");
                    }
                    if let Err(error) = self.merge_previews.shutdown(&mut self.session, PREVIEW_SHUTDOWN_WAIT) {
                        log::error!("desktop owner disconnected before merge previews stopped: {error}");
                    }
                    if !self.renderer.shutdown(PREVIEW_SHUTDOWN_WAIT) {
                        log::error!("desktop owner disconnected while preview renders were still stopping");
                    }
                    let _ = self.session.close_library();
                    break;
                }
            }
        }
    }

    fn handle(&mut self, request: Request) -> bool {
        match request {
            Request::Run { id, params, reply } => {
                let result = self.run(&id, &params);
                let _ = reply.send(result);
                false
            }
            Request::Snapshot { reply } => {
                let result = self.snapshot();
                let _ = reply.send(result);
                false
            }
            Request::ViewSlice { generation, offset, limit, reply } => {
                let result = self.slice(generation, offset, limit);
                let _ = reply.send(result);
                false
            }
            Request::Preview { request, reply } => self.preview(request, reply),
            Request::PreviewQuick { request, reply } => self.preview_quick(request, reply),
            Request::MergePreview { request, reply } => self.merge_preview(request, reply),
            Request::MergePreviewCancel { request, reply } => {
                let result = self.merge_preview_cancel(request);
                let _ = reply.send(result);
                false
            }
            Request::Preferences { patch, reply } => {
                let result = self.preferences(patch);
                let _ = reply.send(result);
                false
            }
            Request::Persist { reply } => {
                let result = self.persist();
                let _ = reply.send(result);
                false
            }
            Request::Shutdown { process_exit, reply } => {
                let result = self.shutdown(process_exit);
                let stop = result.is_ok();
                let _ = reply.send(result);
                stop
            }
        }
    }

    fn run(&mut self, id: &str, params: &Value) -> Result<Value, String> {
        self.poll_sam_validation();
        let result = lightcraft_engine::guard::catch("desktop command", || match id {
            "segment.model.status" => {
                let mut status = self.session.execute(id, params).map_err(|error| error.to_string())?;
                if let Some(object) = status.as_object_mut() {
                    object.insert("validation".into(), self.sam_validation_json());
                }
                Ok(status)
            }
            "segment.model.selectFolder" => self.start_sam_validation(params),
            "segment.model.cancelSelection" => self.cancel_sam_validation(),
            "app.export" => self.tasks.start_export(&mut self.session, params),
            "app.exportPrevious" => {
                let previous = self.session.last_export.clone().ok_or_else(|| "nothing exported yet — use Export…".to_string())?;
                self.tasks.start_export(&mut self.session, &previous)
            }
            "library.import" => self.tasks.start_import(&mut self.session, params),
            "library.importPreview" => self.tasks.start_import_review(&mut self.session, params),
            "library.importLightroom" => self.tasks.start_lightroom_import(&mut self.session, params),
            "library.inspectLightroom" => self.tasks.start_lightroom_inspection(&mut self.session, params),
            "library.buildPreviews" => self.tasks.start_preview_build(&mut self.session, params),
            "photo.cullSuggest" => self.tasks.start_cull_suggest(&mut self.session, params),
            "photo.cullApply" => self.tasks.start_cull_apply(&mut self.session, params),
            "merge.hdr" | "merge.panorama" | "merge.hdrPanorama" => {
                if params.get("preview").and_then(Value::as_bool).unwrap_or(false) {
                    self.session.execute(id, params).map_err(|error| error.to_string())
                } else {
                    self.tasks.start_merge(&mut self.session, id, params)
                }
            }
            "library.backup" => self.backup(params),
            "library.open" => self.open(params),
            "library.restore" => self.restore(params),
            "library.save" => self.persist().map(|()| Value::Null),
            "app.export.cancel" | "task.cancel" => self.tasks.cancel(params.get("taskId").or_else(|| params.get("id")).and_then(Value::as_str)),
            "ui.preferences" => self.preferences(Some(params.clone())),
            _ => self.session.execute(id, params).map_err(|error| error.to_string()),
        })
        .inspect_err(|error| {
            self.last_error = Some(error.clone());
        })?;
        match result {
            Ok(value) => {
                self.last_error = None;
                Ok(value)
            }
            Err(error) => {
                self.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn snapshot(&mut self) -> Result<Value, String> {
        self.poll();
        let error = self.last_error.clone().or_else(|| self.session.unsaved().map(|(_, message)| message.to_string()));
        let value = crate::snapshot::snapshot(&mut self.session, &self.tasks, &self.preferences, &self.notices, error, &mut self.snapshot_cache)?;
        self.last_error = None;
        Ok(value)
    }

    fn start_sam_validation(&mut self, params: &Value) -> Result<Value, String> {
        let path = params.get("path").and_then(Value::as_str).ok_or_else(|| "segment.model.selectFolder requires path".to_string())?;
        if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
            return Err("invalid SAM 3 model folder".into());
        }
        if self.sam_validation.is_some() {
            return Err("SAM 3 model folder validation is already running".into());
        }
        let path = PathBuf::from(path);
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let progress = std::sync::Arc::new(AtomicU64::new(0));
        let (tx, result) = std::sync::mpsc::sync_channel(1);
        let worker_path = path.clone();
        let worker_cancel = cancel.clone();
        let worker_progress = progress.clone();
        let worker = std::thread::Builder::new()
            .name("sam3-validate".into())
            .spawn(move || {
                let result = lightcraft_engine::segment::Segmenter::validate_model_dir_with_progress(&worker_path, |done, _total| {
                    worker_progress.store(done, Ordering::Relaxed);
                    !worker_cancel.load(Ordering::Relaxed)
                });
                let _ = tx.send(result);
            })
            .map_err(|error| format!("could not start SAM 3 folder validation: {error}"))?;
        self.sam_validation_status =
            Some(SamValidationStatus { path: path.clone(), done: 0, total: lightcraft_engine::segment::MODEL_BYTES, error: None, finished: false });
        self.sam_validation = Some(SamValidationTask { path: path.clone(), cancel, progress, result, worker: Some(worker) });
        Ok(json!({"pending": true, "path": path.to_string_lossy()}))
    }

    fn cancel_sam_validation(&mut self) -> Result<Value, String> {
        let cancelled = self.sam_validation.as_ref().is_some_and(|task| {
            task.cancel.store(true, Ordering::Relaxed);
            true
        });
        Ok(json!({"cancelled": cancelled}))
    }

    fn poll_sam_validation(&mut self) {
        let Some(task) = self.sam_validation.as_ref() else { return };
        if let Some(status) = self.sam_validation_status.as_mut() {
            status.done = task.progress.load(Ordering::Relaxed).min(status.total);
        }
        let cancelled = task.cancel.load(Ordering::Relaxed);
        let result = match task.result.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("SAM 3 model validation worker stopped".into()),
        };
        let Some(mut task) = self.sam_validation.take() else { return };
        if let Some(worker) = task.worker.take() {
            let _ = worker.join();
        }
        let path = task.path;
        match result {
            Ok(()) if !cancelled => {
                self.segmenter_dir = Some(path.clone());
                self.session.segmenter.configure_model_dir(Some(path.clone()));
                self.preferences.insert("sam3Dir".into(), json!(path.to_string_lossy()));
                if let Some(status) = self.sam_validation_status.as_mut() {
                    status.done = status.total;
                    status.error = None;
                    status.finished = true;
                }
            }
            Ok(()) => {
                if let Some(status) = self.sam_validation_status.as_mut() {
                    status.error = Some("SAM 3 model validation cancelled".into());
                    status.finished = true;
                }
            }
            Err(error) => {
                if let Some(status) = self.sam_validation_status.as_mut() {
                    status.error = Some(error);
                    status.finished = true;
                }
            }
        }
    }

    fn sam_validation_json(&self) -> Value {
        let Some(status) = self.sam_validation_status.as_ref() else { return Value::Null };
        let running = self.sam_validation.is_some();
        json!({
            "running": running,
            "path": status.path.to_string_lossy(),
            "done": status.done,
            "total": status.total,
            "error": status.error,
            "finished": status.finished && !running,
        })
    }

    fn slice(&mut self, generation: Option<u64>, offset: usize, limit: usize) -> Result<Value, String> {
        crate::snapshot::view_slice(&mut self.session, generation, offset, limit)
    }

    fn preview(&mut self, request: PreviewRequest, reply: Sender<Result<PreviewDescriptor, String>>) -> bool {
        let result = lightcraft_engine::guard::catch("desktop preview request", || self.renderer.request(&mut self.session, request, reply.clone()));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) | Err(error) => {
                log::error!("preview request: {error}");
                let _ = reply.send(Err(error));
            }
        }
        self.renderer.poll(&mut self.session);
        false
    }

    fn preview_quick(&mut self, request: PreviewRequest, reply: Sender<Result<PreviewDescriptor, String>>) -> bool {
        let result = lightcraft_engine::guard::catch("desktop quick preview request", || {
            self.renderer.request_quick(&mut self.session, request, reply.clone())
        });
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) | Err(error) => {
                log::error!("quick preview request: {error}");
                let _ = reply.send(Err(error));
            }
        }
        self.renderer.poll(&mut self.session);
        false
    }

    fn merge_preview(&mut self, request: MergePreviewRequest, reply: Sender<Result<MergePreviewDescriptor, String>>) -> bool {
        let result = lightcraft_engine::guard::catch("desktop merge preview request", || {
            self.merge_previews.request(&mut self.session, request, reply.clone())
        });
        if let Ok(Err(error)) | Err(error) = result {
            let _ = reply.send(Err(error));
        }
        self.merge_previews.poll(&mut self.session);
        false
    }

    fn merge_preview_cancel(&mut self, request: MergePreviewCancelRequest) -> Result<bool, String> {
        lightcraft_engine::guard::catch("desktop merge preview cancellation", || self.merge_previews.cancel(request)).unwrap_or_else(Err)
    }

    fn preferences(&mut self, patch: Option<Value>) -> Result<Value, String> {
        if let Some(Value::Object(values)) = patch {
            let segmenter_dir = values.get("sam3Dir").and_then(persisted_segmenter_dir);
            let memory_patch = values.get("ui").and_then(Value::as_object).and_then(|ui| ui.get("memoryMb")).cloned();
            Self::merge_preferences(&mut self.preferences, values);
            self.apply_memory_preference(memory_patch);
            if let Some(dir) = segmenter_dir {
                self.segmenter_dir = Some(dir.clone());
                self.session.segmenter.configure_model_dir(Some(dir));
            }
        } else if patch.is_some() {
            return Err("preferences patch must be an object".into());
        }
        Ok(Value::Object(self.preferences.clone()))
    }

    fn apply_memory_preference(&mut self, memory_patch: Option<Value>) {
        let Some(value) = memory_patch else { return };
        match Self::memory_budget_bytes(&value) {
            Ok(bytes) => {
                self.session.set_memory_budget(bytes);
                self.notices.retain(|notice| !notice.starts_with("Invalid memory budget preference:"));
            }
            Err(error) => {
                let notice = format!("Invalid memory budget preference: {error}");
                if !self.notices.iter().any(|existing| existing == &notice) {
                    self.notices.push(notice);
                }
            }
        }
    }

    fn merge_preferences(target: &mut Map<String, Value>, patch: Map<String, Value>) {
        for (key, value) in patch {
            match value {
                Value::Object(incoming) => {
                    if let Some(Value::Object(existing)) = target.get_mut(&key) {
                        Self::merge_preferences(existing, incoming);
                    } else {
                        target.insert(key, Value::Object(incoming));
                    }
                }
                other => {
                    target.insert(key, other);
                }
            }
        }
    }

    fn memory_budget_bytes(value: &Value) -> Result<usize, String> {
        let mb = value.as_u64().ok_or_else(|| "ui.memoryMb must be an integer".to_string())?;
        if mb == 0 {
            return Ok(lightcraft_engine::memory::default_budget());
        }
        if !(64..=1_048_576).contains(&mb) {
            return Err("ui.memoryMb must be 0 or between 64 and 1048576 MB".into());
        }
        mb.checked_mul(1_048_576)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(|| "ui.memoryMb exceeds this platform's addressable memory".into())
    }

    fn persist(&mut self) -> Result<(), String> {
        self.session.persist_if_dirty();
        if let Err(error) = self.session.save_prefs() {
            let message = error.to_string();
            self.last_error = Some(message.clone());
            return Err(message);
        }
        if let Some((count, error)) = self.session.unsaved() {
            let message = format!("{count} library change(s) remain unsaved: {error}");
            self.last_error = Some(message.clone());
            return Err(message);
        }
        self.last_error = None;
        Ok(())
    }

    fn poll(&mut self) {
        self.poll_sam_validation();
        if let Err(error) = lightcraft_engine::guard::catch("desktop task poll", || self.tasks.poll(&mut self.session)) {
            self.last_error = Some(error);
        }
        if let Err(error) = lightcraft_engine::guard::catch("desktop auto import poll", || self.auto_import.poll(&mut self.session, &mut self.tasks))
        {
            self.last_error = Some(error);
        }
        self.notices.extend(self.tasks.take_notices());
        if self.notices.len() > 64 {
            let drop_count = self.notices.len() - 64;
            self.notices.drain(..drop_count);
        }
        if let Err(error) = lightcraft_engine::guard::catch("desktop preview poll", || self.renderer.poll(&mut self.session)) {
            self.last_error = Some(error);
        }
        self.merge_previews.poll(&mut self.session);
    }

    pub(crate) fn shutdown(&mut self, process_exit: bool) -> Result<(), String> {
        if let Some(task) = self.sam_validation.as_mut() {
            task.cancel.store(true, Ordering::Relaxed);
            if task.worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
                return Err("segment validation is still stopping; retry shutdown shortly".into());
            }
        }
        if let Some(mut task) = self.sam_validation.take()
            && let Some(worker) = task.worker.take()
        {
            let _ = worker.join();
        }
        self.auto_import.stop()?;
        self.tasks.cancel(None)?;
        self.poll();
        if self.tasks.running() {
            return Err("background tasks are still stopping; retry shutdown after progress completes".into());
        }
        self.merge_previews.shutdown(&mut self.session, PREVIEW_SHUTDOWN_WAIT)?;
        cancel_preview_build(&self.session, PREVIEW_SHUTDOWN_WAIT)?;
        // Delay owner close until all retryable cancellation & persistence checks above succeed.
        // Only process-exit teardown closes the process-wide GPU gate; host tests and library
        // replacement keep it open for later sessions.
        self.session.persist().map_err(|error| {
            let message = error.to_string();
            self.last_error = Some(message.clone());
            message
        })?;
        if process_exit {
            lightcraft_engine::gpu::begin_shutdown();
        }
        if !self.renderer.shutdown(PREVIEW_SHUTDOWN_WAIT) {
            return Err("preview renders are still stopping; retry shutdown shortly".into());
        }
        if process_exit && !lightcraft_engine::gpu::wait_idle(PREVIEW_SHUTDOWN_WAIT) {
            return Err("GPU work is still stopping; retry shutdown shortly".into());
        }
        self.session.close_library().map_err(|error| {
            let message = error.to_string();
            self.last_error = Some(message.clone());
            message
        })
    }

    fn open(&mut self, params: &Value) -> Result<Value, String> {
        let path = library_path(params, "library.open")?;
        let (path, photos) = self.switch_library(path)?;
        Ok(json!({"path": path.to_string_lossy(), "photos": photos}))
    }

    /// Replace the owner session only after the requested library has loaded successfully.
    /// Keeping the old session alive through the load preserves its lock and in-memory edits
    /// when opening a bad path or a library held by another process fails.
    fn switch_library(&mut self, path: PathBuf) -> Result<(PathBuf, usize), String> {
        if self.tasks.running() {
            return Err("cannot change libraries while a background task is running; wait for it to finish".into());
        }
        if self.merge_previews.running() {
            return Err("cannot change libraries while a merge preview is running; wait for it to finish or change options to supersede it".into());
        }
        if preview_build_running(&self.session) {
            return Err("cannot change libraries while a preview build is running; run library.cancelPreviews, wait for library.previewProgress running=false, then retry".into());
        }
        if self.session.library.as_ref().is_some_and(|library| same_library_path(&library.dir, &path)) {
            return Ok((path, self.session.catalog.len()));
        }

        self.persist().map_err(|error| format!("current library must be saved before opening another library: {error}"))?;

        let mut fresh = Session::new().with_fs().with_system_clock();
        fresh.open_library(&path, false).map_err(|error| format!("could not open library: {error}"))?;
        configure_segmenter(&mut fresh, &self.segmenter_dir, &self.segmenter_mirrors_file);
        let notices = fresh.take_library_warnings();
        let photos = fresh.catalog.len();

        self.auto_import.reset()?;
        self.session.close_library().map_err(|error| format!("could not close current library: {error}"))?;
        self.renderer.cancel_all();
        let store = self.renderer.store();
        self.session = fresh;
        self.renderer = Renderer::new(store);
        self.merge_previews.reset();
        self.snapshot_cache = crate::snapshot::CatalogSnapshotCache::default();
        self.notices = notices;
        let memory_preference = self.preferences.get("ui").and_then(Value::as_object).and_then(|ui| ui.get("memoryMb")).cloned();
        self.apply_memory_preference(memory_preference);
        self.last_error = None;
        Ok((path, photos))
    }

    fn backup(&mut self, params: &Value) -> Result<Value, String> {
        let path = params.get("path").and_then(Value::as_str).ok_or_else(|| "library.backup requires path".to_string())?;
        if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
            return Err("invalid library backup path".into());
        }
        if self.tasks.running() {
            return Err("cannot back up while a background task is running; wait for it to finish".into());
        }
        if self.merge_previews.running() {
            return Err("cannot back up while a merge preview is running; wait for it to finish".into());
        }
        let source = self
            .session
            .library
            .as_ref()
            .filter(|library| library.on_disk)
            .map(|library| library.dir.clone())
            .ok_or_else(|| "no on-disk library is open".to_string())?;
        self.session.close_library().map_err(|error| format!("library must be saved before backup: {error}"))?;
        let destination = backup_destination(Path::new(path), &source)?;
        if destination.exists() {
            return Err("backup destination already exists; refusing to overwrite it".into());
        }
        // Claim the destination first; only a folder this call made is removed on failure.
        if let Err(error) = std::fs::create_dir(&destination) {
            return Err(format!("backup create {}: {error}", destination.display()));
        }
        if let Err(error) = copy_library(&source, &destination) {
            let _ = std::fs::remove_dir_all(&destination);
            return Err(error);
        }
        Ok(json!({"path": destination.to_string_lossy(), "source": source.to_string_lossy()}))
    }

    fn restore(&mut self, params: &Value) -> Result<Value, String> {
        let path = library_path(params, "library.restore")?;
        if self.tasks.running() {
            return Err("cannot restore while a background task is running; wait for it to finish".into());
        }
        if self.merge_previews.running() {
            return Err("cannot restore while a merge preview is running; wait for it to finish".into());
        }
        let path_metadata = std::fs::symlink_metadata(&path).map_err(|error| format!("library restore path: {error}"))?;
        if path_metadata.file_type().is_symlink() {
            return Err("library restore refuses symlink backup directory".into());
        }
        if !path_metadata.is_dir() {
            return Err("library restore expects a backup directory".into());
        }
        let backup = std::fs::canonicalize(&path).map_err(|error| format!("library restore path: {error}"))?;
        let destination = restore_destination(&backup)?;
        let metadata = std::fs::symlink_metadata(&backup).map_err(|error| format!("library restore read {}: {error}", backup.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("library restore expects a regular backup directory".into());
        }
        if let Err(error) = std::fs::create_dir(&destination) {
            return Err(format!("could not claim restore destination {}: {error}", destination.display()));
        }
        if let Err(error) = copy_library(&backup, &destination) {
            let _ = std::fs::remove_dir_all(&destination);
            return Err(format!("could not stage restored library: {error}"));
        }
        if let Err(error) = self.switch_library(destination.clone()) {
            let _ = std::fs::remove_dir_all(&destination);
            return Err(error);
        }
        Ok(json!({
            "path": destination.to_string_lossy(),
            "libraryPath": destination.to_string_lossy(),
            "restoredPath": destination.to_string_lossy(),
            "sourceBackup": path.to_string_lossy(),
        }))
    }
}

fn library_path(params: &Value, command: &str) -> Result<PathBuf, String> {
    let path = params.get("path").and_then(Value::as_str).ok_or_else(|| format!("{command} requires path"))?;
    if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
        return Err(format!("invalid {command} path"));
    }
    Ok(PathBuf::from(path))
}

fn configure_segmenter(session: &mut Session, dir: &Option<PathBuf>, mirrors_file: &Option<PathBuf>) {
    session.segmenter.configure_model_dir(dir.clone());
    session.segmenter.mirrors_file = mirrors_file.clone();
}

fn persisted_segmenter_dir(value: &Value) -> Option<PathBuf> {
    let path = value.as_str()?.trim();
    if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
        return None;
    }
    Some(PathBuf::from(path))
}

#[cfg(not(target_arch = "wasm32"))]
fn same_library_path(left: &Path, right: &Path) -> bool {
    match (rightkit_fs::canonicalize_plain(left), rightkit_fs::canonicalize_plain(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// A browser build has no folders to compare.
#[cfg(target_arch = "wasm32")]
fn same_library_path(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(not(target_arch = "wasm32"))]
fn backup_destination(path: &Path, source: &Path) -> Result<PathBuf, String> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let parent = rightkit_fs::canonicalize_plain(parent).map_err(|error| format!("backup destination parent: {error}"))?;
    let name = path.file_name().ok_or_else(|| "backup path has no file name".to_string())?;
    let destination = parent.join(name);
    let source = rightkit_fs::canonicalize_plain(source).map_err(|error| format!("library path: {error}"))?;
    // `is_within` is true for the same folder too: the destination may be neither inside the open
    // library nor contain it.
    if rightkit_fs::is_within(&source, &destination) || rightkit_fs::is_within(&destination, &source) {
        return Err("backup destination must be outside open library".into());
    }
    Ok(destination)
}

/// A browser build has no folders to back up to.
#[cfg(target_arch = "wasm32")]
fn backup_destination(_path: &Path, _source: &Path) -> Result<PathBuf, String> {
    Err("this build has no file system".into())
}

fn restore_destination(backup: &Path) -> Result<PathBuf, String> {
    let parent = backup.parent().ok_or_else(|| "backup path has no parent".to_string())?;
    let parent = restore_parent(parent)?;
    let name = backup.file_name().ok_or_else(|| "backup path has no file name".to_string())?.to_string_lossy();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or_default();
    let pid = std::process::id();
    for attempt in 0..32u32 {
        let destination = parent.join(format!("{name}.restored-{pid}-{stamp}-{attempt}"));
        if std::fs::symlink_metadata(&destination).is_err() {
            let destination = backup_destination(&destination, backup)?;
            if std::fs::symlink_metadata(&destination).is_err() {
                return Ok(destination);
            }
        }
    }
    Err("could not allocate a unique restore destination".into())
}

fn preview_build_running(session: &Session) -> bool {
    session.preview_build.as_ref().is_some_and(|build| !build.finished.load(std::sync::atomic::Ordering::Relaxed))
}

fn cancel_preview_build(session: &Session, timeout: Duration) -> Result<(), String> {
    let Some(build) = session.preview_build.as_ref().filter(|build| !build.finished.load(std::sync::atomic::Ordering::Relaxed)) else {
        return Ok(());
    };
    build.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let deadline = Instant::now() + timeout;
    while !build.finished.load(std::sync::atomic::Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if build.finished.load(std::sync::atomic::Ordering::Relaxed) {
        Ok(())
    } else {
        Err("preview build is still stopping; retry shutdown after library.previewProgress reports running=false".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn restore_parent(parent: &Path) -> Result<PathBuf, String> {
    rightkit_fs::canonicalize_plain(parent).map_err(|error| format!("restore destination parent: {error}"))
}

#[cfg(target_arch = "wasm32")]
fn restore_parent(_parent: &Path) -> Result<PathBuf, String> {
    Err("this build has no file system".into())
}

/// Library lock files are never part of a backup or a restore.
#[cfg(not(target_arch = "wasm32"))]
fn is_lock_file(name: &std::ffi::OsStr) -> bool {
    matches!(name.to_str(), Some("catalog.lock" | "catalog.lock.owner"))
}

/// Deepest folder nesting a backup or restore will read.
#[cfg(not(target_arch = "wasm32"))]
const MAX_COPY_DEPTH: usize = 64;

/// Security rule (kept beside `rightkit_fs::copy_tree`, which can only keep a link or copy what it
/// points to): a backup or restore refuses every symlink, so a link planted in the library can never
/// pull in the bytes of a file outside it. A read-only walk that fails on the first link, before
/// anything is copied. The top-level lock files are skipped here exactly as the copy skips them.
#[cfg(not(target_arch = "wasm32"))]
fn refuse_symlinks(path: &Path, depth: usize) -> Result<(), String> {
    if depth > MAX_COPY_DEPTH {
        return Err("library backup directory nesting is too deep".into());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("backup read {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("library backup refuses symlink {}", path.display()));
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|error| format!("backup list {}: {error}", path.display()))? {
            let entry = entry.map_err(|error| format!("backup entry {}: {error}", path.display()))?;
            if depth == 0 && is_lock_file(&entry.file_name()) {
                continue;
            }
            refuse_symlinks(&entry.path(), depth.saturating_add(1))?;
        }
        Ok(())
    } else if metadata.is_file() {
        Ok(())
    } else {
        Err(format!("unsupported library entry {}", path.display()))
    }
}

/// Copy the contents of the library folder `source` into the existing, empty folder `destination`:
/// every symlink refused first (`refuse_symlinks`), then each top-level entry except the two lock
/// files through `rightkit_fs::copy_tree`, which never overwrites. A link that appears after the
/// check is kept as a link (`keep_symlinks`), never followed, so no outside bytes are read either way.
#[cfg(not(target_arch = "wasm32"))]
fn copy_library(source: &Path, destination: &Path) -> Result<(), String> {
    refuse_symlinks(source, 0)?;
    let options = rightkit_fs::CopyOptions::new(true, true);
    for entry in std::fs::read_dir(source).map_err(|error| format!("backup list {}: {error}", source.display()))? {
        let entry = entry.map_err(|error| format!("backup entry {}: {error}", source.display()))?;
        if is_lock_file(&entry.file_name()) {
            continue;
        }
        rightkit_fs::copy_tree(&entry.path(), &destination.join(entry.file_name()), options)
            .map_err(|error| format!("backup copy {}: {error}", entry.path().display()))?;
    }
    Ok(())
}

/// A browser build has no folders to copy.
#[cfg(target_arch = "wasm32")]
fn copy_library(_source: &Path, _destination: &Path) -> Result<(), String> {
    Err("this build has no file system".into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::Ordering;

    use super::*;

    fn temp_library(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or_default();
        std::env::temp_dir().join(format!("lightcraft-host-{label}-{}-{stamp}", std::process::id()))
    }

    fn seed_library(path: &Path) {
        let mut session = Session::new().with_system_clock();
        assert!(session.open_library(path, true).is_ok(), "demo library should open");
        assert!(session.close_library().is_ok(), "demo library should close");
    }

    #[test]
    fn failed_shutdown_keeps_owner_available_until_task_finishes() {
        let controller_result = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok(), "demo controller should start");
        let Ok(mut controller) = controller_result else { return };
        let (started, release) = controller.tasks.hold_for_test();
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(controller.shutdown(false).is_err());
        assert!(controller.snapshot().is_ok());
        release.store(true, Ordering::Release);
        while controller.tasks.running() {
            controller.poll();
            std::thread::yield_now();
        }
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn routine_host_shutdown_keeps_gpu_gate_open_for_next_host() {
        let controller_result = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        assert!(controller.shutdown(false).is_ok());
        assert!(!lightcraft_engine::gpu::shutting_down(), "host shutdown is not process exit");
    }

    #[test]
    fn disconnected_owner_does_not_close_process_gpu_gate() {
        let controller_result = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        controller.serve(receiver);
        assert!(!lightcraft_engine::gpu::shutting_down(), "owner disconnect is not process exit");

        let later = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(later.is_ok(), "later host should remain constructible");
        let Ok(mut later) = later else { return };
        assert!(later.snapshot().is_ok(), "later host should remain usable");
        assert!(later.shutdown(false).is_ok());
    }

    #[test]
    fn preferences_apply_memory_budget_reset_and_preserve_unknown() {
        let controller_result = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        let explicit = controller.preferences(Some(json!({
            "ui": {"memoryMb": 128, "futureSetting": {"keep": true}},
            "futureRoot": {"keep": "yes"}
        })));
        assert!(explicit.is_ok());
        assert_eq!(controller.session.memory_report().budget, 128 * (1 << 20));

        let unrelated = controller.preferences(Some(json!({"ui": {"theme": "dark"}})));
        assert!(unrelated.is_ok());
        assert_eq!(controller.session.memory_report().budget, 128 * (1 << 20));
        assert_eq!(controller.preferences["ui"]["futureSetting"]["keep"], true);
        assert_eq!(controller.preferences["futureRoot"]["keep"], "yes");

        let automatic = controller.preferences(Some(json!({"ui": {"memoryMb": 0}})));
        assert!(automatic.is_ok());
        assert_eq!(controller.session.memory_report().budget, lightcraft_engine::memory::default_budget());
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn malformed_sam_folder_validation_is_async_and_does_not_switch_directory() {
        let current = temp_library("sam3-current");
        let selected = temp_library("sam3-malformed");
        let controller_result =
            Controller::new(HostOptions { demo: true, sam3_dir: Some(current.clone()), ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        assert!(controller.preferences(Some(json!({"sam3Dir": current.to_string_lossy()}))).is_ok());
        let started = Instant::now();
        let pending = controller.run("segment.model.selectFolder", &json!({"path": selected.to_string_lossy()}));
        assert!(pending.is_ok());
        assert!(started.elapsed() < Duration::from_secs(1), "selection must return before validation finishes");
        let status = controller.run("segment.model.status", &json!({}));
        assert!(status.is_ok());
        assert!(status.as_ref().ok().and_then(|value| value.get("validation")).is_some());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            controller.poll();
            let status = controller.sam_validation_json();
            if !status.get("running").and_then(Value::as_bool).unwrap_or(false) {
                assert!(status.get("error").and_then(Value::as_str).is_some(), "malformed folder must report validation error");
                break;
            }
            assert!(Instant::now() < deadline, "malformed folder validation did not finish");
            std::thread::yield_now();
        }
        assert_eq!(controller.session.segmenter.dir.as_ref(), Some(&current));
        assert_eq!(controller.preferences.get("sam3Dir").and_then(Value::as_str), Some(current.to_string_lossy().as_ref()));
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn cancelled_sam_validation_cannot_commit_a_successful_result() {
        let current = temp_library("sam3-cancel-current");
        let selected = temp_library("sam3-cancel-selected");
        let controller_result =
            Controller::new(HostOptions { demo: true, sam3_dir: Some(current.clone()), ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        assert!(controller.preferences(Some(json!({"sam3Dir": current.to_string_lossy()}))).is_ok());
        let cancel = std::sync::Arc::new(AtomicBool::new(true));
        let progress = std::sync::Arc::new(AtomicU64::new(0));
        let (tx, result) = std::sync::mpsc::sync_channel(1);
        assert!(tx.send(Ok(())).is_ok());
        controller.sam_validation_status = Some(SamValidationStatus {
            path: selected.clone(),
            done: 0,
            total: lightcraft_engine::segment::MODEL_BYTES,
            error: None,
            finished: false,
        });
        controller.sam_validation = Some(SamValidationTask { path: selected, cancel, progress, result, worker: None });
        controller.poll_sam_validation();
        let status = controller.sam_validation_json();
        assert!(status.get("error").and_then(Value::as_str).is_some_and(|error| error.contains("cancelled")));
        assert_eq!(controller.session.segmenter.dir.as_ref(), Some(&current));
        assert_eq!(controller.preferences.get("sam3Dir").and_then(Value::as_str), Some(current.to_string_lossy().as_ref()));
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn completed_sam_validation_commits_folder_and_preference() {
        let current = temp_library("sam3-success-current");
        let selected = temp_library("sam3-success-selected");
        let controller_result =
            Controller::new(HostOptions { demo: true, sam3_dir: Some(current.clone()), ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        assert!(controller.preferences(Some(json!({"sam3Dir": current.to_string_lossy()}))).is_ok());
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let progress = std::sync::Arc::new(AtomicU64::new(0));
        let (tx, result) = std::sync::mpsc::sync_channel(1);
        assert!(tx.send(Ok(())).is_ok());
        controller.sam_validation_status = Some(SamValidationStatus {
            path: selected.clone(),
            done: 0,
            total: lightcraft_engine::segment::MODEL_BYTES,
            error: None,
            finished: false,
        });
        controller.sam_validation = Some(SamValidationTask { path: selected.clone(), cancel, progress, result, worker: None });
        controller.poll_sam_validation();
        assert_eq!(controller.session.segmenter.dir.as_ref(), Some(&selected));
        assert_eq!(controller.preferences.get("sam3Dir").and_then(Value::as_str), Some(selected.to_string_lossy().as_ref()));
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn invalid_memory_preference_is_reported_without_changing_budget() {
        let controller_result = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        let before = controller.session.memory_report().budget;
        assert!(controller.preferences(Some(json!({"ui": {"memoryMb": 63}}))).is_ok());
        assert_eq!(controller.session.memory_report().budget, before);
        assert!(controller.notices.iter().any(|notice| notice.contains("ui.memoryMb must be 0 or between 64 and 1048576 MB")));
        assert!(Controller::memory_budget_bytes(&json!(1_048_577)).is_err());
        assert!(Controller::memory_budget_bytes(&json!(-1)).is_err());
        assert!(Controller::memory_budget_bytes(&json!("128")).is_err());
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn library_switch_reapplies_memory_preference() {
        let old = temp_library("memory-old");
        let next = temp_library("memory-next");
        seed_library(&old);
        seed_library(&next);
        let controller_result = Controller::new(HostOptions { library_path: Some(old.clone()), ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        assert!(controller.preferences(Some(json!({"ui": {"memoryMb": 128}}))).is_ok());
        assert!(controller.run("library.open", &json!({"path": next})).is_ok());
        assert_eq!(controller.session.memory_report().budget, 128 * (1 << 20));
        assert!(controller.shutdown(false).is_ok());
        let _ = fs::remove_dir_all(old);
        let _ = fs::remove_dir_all(next);
    }

    #[test]
    fn failed_open_keeps_old_session_and_lock() {
        let old = temp_library("open-old");
        let held = temp_library("open-held");
        let controller_result = Controller::new(HostOptions { library_path: Some(old.clone()), ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        let mut blocker = Session::new().with_system_clock();
        assert!(blocker.open_library(&held, false).is_ok());

        let result = controller.run("library.open", &json!({"path": held}));
        assert!(result.is_err());
        assert_eq!(controller.session.library.as_ref().map(|library| &library.dir), Some(&old));

        let mut second = Session::new().with_system_clock();
        assert!(second.open_library(&old, false).is_err(), "failed open must retain old lock");
        drop(blocker);
        assert!(controller.shutdown(false).is_ok());
        let _ = fs::remove_dir_all(old);
        let _ = fs::remove_dir_all(held);
    }

    #[test]
    fn open_refuses_running_background_task() {
        let controller_result = Controller::new(HostOptions { demo: true, ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        let (started, release) = controller.tasks.hold_for_test();
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let result = controller.run("library.open", &json!({"path": temp_library("running")}));
        assert!(matches!(result, Err(ref error) if error.contains("background task")));
        release.store(true, Ordering::Release);
        while controller.tasks.running() {
            controller.poll();
            std::thread::yield_now();
        }
        assert!(controller.shutdown(false).is_ok());
    }

    #[test]
    fn filesystem_hooks_import_png_and_render_preview() {
        let library = temp_library("png-library");
        let source = temp_library("procedural-rgb-01").with_extension("png");
        let image = lightcraft_raster::Rgba8::from_fn(4, 3, |x, y| [(x * 40) as u8, (y * 60) as u8, 120, 255]);
        let encoded = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default());
        assert!(encoded.is_ok(), "procedural PNG should encode: {:?}", encoded.as_ref().err());
        let Ok(encoded) = encoded else { return };
        assert!(fs::write(&source, encoded).is_ok(), "procedural PNG should be written");

        let controller_result =
            Controller::new(HostOptions { library_path: Some(library.clone()), ..HostOptions::default() }, PreviewStore::default());
        assert!(controller_result.is_ok(), "PNG host controller should start: {:?}", controller_result.as_ref().err());
        let Ok(mut controller) = controller_result else { return };
        let imported = controller.run("library.import", &json!({"paths": [source.to_string_lossy()]}));
        assert!(imported.is_ok(), "PNG import should start: {:?}", imported.err());
        let mut saw_preview_task = false;
        for _ in 0..2_000 {
            controller.poll();
            saw_preview_task |= controller.tasks.statuses().iter().any(|job| job.kind == "preview");
            saw_preview_task |= controller.tasks.completed_jobs().iter().any(|job| job.kind == "preview");
            if !controller.tasks.running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!controller.tasks.running(), "PNG import worker should finish");
        assert!(saw_preview_task, "PNG import should enqueue host preview task");
        assert!(controller.notices.iter().any(|notice| notice == "Import Photos finished"), "PNG import should finish: {:?}", controller.notices);

        let snapshot = controller.snapshot();
        assert!(snapshot.is_ok(), "PNG snapshot should succeed: {:?}", snapshot.as_ref().err());
        let Ok(snapshot) = snapshot else { return };
        let generation = snapshot.get("viewGeneration").and_then(Value::as_u64).expect("PNG snapshot must expose view generation");
        let slice = controller.slice(Some(generation), 0, 1);
        assert!(slice.is_ok(), "PNG slice should succeed: {:?}", slice.as_ref().err());
        let Ok(slice) = slice else { return };
        let Some(photo) = slice.get("photos").and_then(Value::as_array).and_then(|photos| photos.first()) else {
            assert!(slice.get("photos").and_then(Value::as_array).is_some_and(|photos| !photos.is_empty()), "PNG import should create a photo");
            return;
        };
        let photo_id = photo.get("id").and_then(Value::as_u64).expect("PNG import must expose photo identity");
        assert_eq!(photo.get("w").and_then(Value::as_u64), Some(4));
        assert_eq!(photo.get("h").and_then(Value::as_u64), Some(3));

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let request = PreviewRequest {
            photo_id,
            slot: "png-regression".into(),
            view_generation: generation,
            width: 64,
            height: 64,
            quality: crate::PreviewQuality::Draft,
            before: false,
            sequence: 1,
        };
        let _ = controller.preview(request, reply_tx);
        let mut preview = None;
        for _ in 0..2_000 {
            controller.poll();
            match reply_rx.try_recv() {
                Ok(result) => {
                    preview = Some(result);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(1)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        assert!(preview.is_some(), "PNG preview should reply");
        let Some(preview) = preview else { return };
        assert!(preview.is_ok(), "PNG preview should render: {:?}", preview.as_ref().err());
        let Ok(preview) = preview else { return };
        assert_eq!(preview.photo_id, photo_id);
        assert_eq!(preview.encoding, "png");
        assert!(controller.renderer.store().get(&preview.handle).is_some(), "PNG preview bytes should be published");
        assert!(controller.shutdown(false).is_ok());
        let _ = fs::remove_dir_all(library);
        let _ = fs::remove_file(source);
    }

    #[test]
    fn catalog_switch_gets_fresh_generation_and_revision() {
        let old = temp_library("same-old");
        let next = temp_library("same-next");
        let model_dir = temp_library("sam3-model");
        let mirrors_file = temp_library("sam3-mirrors");
        seed_library(&old);
        seed_library(&next);
        let controller_result = Controller::new(
            HostOptions {
                library_path: Some(old.clone()),
                sam3_dir: Some(model_dir.clone()),
                sam3_mirrors_file: Some(mirrors_file.clone()),
                ..HostOptions::default()
            },
            PreviewStore::default(),
        );
        assert!(controller_result.is_ok());
        let Ok(mut controller) = controller_result else { return };
        assert_eq!(controller.session.segmenter.dir.as_ref(), Some(&model_dir));
        assert_eq!(controller.session.segmenter.mirrors_file.as_ref(), Some(&mirrors_file));
        let selected_dir = temp_library("sam3-selected");
        assert!(controller.preferences(Some(json!({"sam3Dir": selected_dir.to_string_lossy()}))).is_ok());
        assert_eq!(controller.session.segmenter.dir.as_ref(), Some(&selected_dir));
        let before_result = controller.snapshot();
        assert!(before_result.is_ok());
        let Ok(before) = before_result else { return };
        let Some(before_generation) = before.get("viewGeneration").and_then(Value::as_u64) else { return };
        let before_revision = before.get("revision").and_then(Value::as_u64);
        let repeated_before_result = controller.snapshot();
        assert!(repeated_before_result.is_ok());
        let Ok(repeated_before) = repeated_before_result else { return };
        assert_eq!(repeated_before.get("revision").and_then(Value::as_u64), before_revision);
        let before_slice_result = controller.slice(Some(before_generation), 0, 1);
        assert!(before_slice_result.is_ok());
        let Ok(before_slice) = before_slice_result else { return };
        let before_id =
            before_slice.get("photos").and_then(Value::as_array).and_then(|photos| photos.first()).and_then(|photo| photo.get("id")).cloned();

        let opened_result = controller.run("library.open", &json!({"path": next}));
        assert!(opened_result.is_ok());
        let Ok(opened) = opened_result else { return };
        assert_eq!(controller.session.segmenter.dir.as_ref(), Some(&selected_dir));
        assert_eq!(controller.session.segmenter.mirrors_file.as_ref(), Some(&mirrors_file));
        assert!(opened.get("photos").and_then(Value::as_u64).is_some_and(|count| count > 0));
        let after_result = controller.snapshot();
        assert!(after_result.is_ok());
        let Ok(after) = after_result else { return };
        // Loaded libraries receive a process-unique revision range. Equal revisions are only
        // stable while the same library remains open; switching libraries must invalidate caches.
        assert_ne!(after.get("revision").and_then(Value::as_u64), before_revision);
        assert_ne!(after.get("viewGeneration").and_then(Value::as_u64), Some(before_generation));
        let Some(after_generation) = after.get("viewGeneration").and_then(Value::as_u64) else { return };
        let after_slice_result = controller.slice(Some(after_generation), 0, 1);
        assert!(after_slice_result.is_ok());
        let Ok(after_slice) = after_slice_result else { return };
        let after_id =
            after_slice.get("photos").and_then(Value::as_array).and_then(|photos| photos.first()).and_then(|photo| photo.get("id")).cloned();
        assert_eq!(after_id, before_id, "equal-revision catalogs should retain same photo ids");

        assert!(controller.shutdown(false).is_ok());
        let _ = fs::remove_dir_all(old);
        let _ = fs::remove_dir_all(next);
        let _ = fs::remove_dir_all(selected_dir);
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn list_files(root: &Path) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn library_copy_takes_every_file_except_the_top_level_lock_files() {
        let base = temp_library("copy-set");
        let (source, destination) = (base.join("library"), base.join("backup"));
        write_file(&source.join("catalog.db"), b"db");
        write_file(&source.join("catalog.lock"), b"lock");
        write_file(&source.join("catalog.lock.owner"), b"owner");
        write_file(&source.join("previews/a/b/p.bin"), b"preview");
        write_file(&source.join("previews/catalog.lock"), b"nested lock names are data");
        fs::create_dir_all(source.join("empty-folder")).unwrap();
        fs::create_dir_all(&destination).unwrap();

        copy_library(&source, &destination).unwrap();

        assert_eq!(list_files(&destination), ["catalog.db", "previews/a/b/p.bin", "previews/catalog.lock"]);
        assert!(destination.join("empty-folder").is_dir());
        assert_eq!(fs::read(destination.join("previews/a/b/p.bin")).unwrap(), b"preview");
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn library_copy_never_overwrites_what_the_destination_already_holds() {
        let base = temp_library("copy-keep");
        let (source, destination) = (base.join("library"), base.join("backup"));
        write_file(&source.join("catalog.db"), b"new");
        write_file(&destination.join("catalog.db"), b"keep");

        assert!(copy_library(&source, &destination).is_err());
        assert_eq!(fs::read(destination.join("catalog.db")).unwrap(), b"keep");
        let _ = fs::remove_dir_all(base);
    }

    /// Security rule B4: a link planted inside the library is refused before anything is copied, so a
    /// backup or restore never carries the bytes of a file outside the library.
    #[cfg(unix)]
    #[test]
    fn library_copy_refuses_a_planted_symlink_before_copying_anything() {
        let base = temp_library("copy-symlink");
        let outside = base.join("outside");
        write_file(&outside.join("secret.txt"), b"secret");
        for (label, link_at) in [("top", "stolen.txt"), ("nested", "previews/deep/stolen.txt")] {
            let (source, destination) = (base.join(format!("library-{label}")), base.join(format!("backup-{label}")));
            write_file(&source.join("catalog.db"), b"db");
            write_file(&source.join("previews/deep/p.bin"), b"preview");
            std::os::unix::fs::symlink(outside.join("secret.txt"), source.join(link_at)).unwrap();
            fs::create_dir_all(&destination).unwrap();

            let error = copy_library(&source, &destination).unwrap_err();
            assert!(error.contains("refuses symlink"), "{label}: {error}");
            assert!(list_files(&destination).is_empty(), "{label}: nothing is copied once a link is found");
        }
        // A link to a folder is refused the same way.
        let (source, destination) = (base.join("library-dir"), base.join("backup-dir"));
        write_file(&source.join("catalog.db"), b"db");
        std::os::unix::fs::symlink(&outside, source.join("outside-folder")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        assert!(copy_library(&source, &destination).unwrap_err().contains("refuses symlink"));
        assert!(list_files(&destination).is_empty());
        let _ = fs::remove_dir_all(base);
    }

    /// A link that appears after the check is kept as a link (never followed): no outside bytes.
    #[cfg(unix)]
    #[test]
    fn library_copy_does_not_follow_a_link_that_appears_after_the_check() {
        let base = temp_library("copy-race");
        let outside = base.join("outside.txt");
        write_file(&outside, b"secret");
        let (source, destination) = (base.join("library"), base.join("backup"));
        write_file(&source.join("catalog.db"), b"db");
        fs::create_dir_all(&destination).unwrap();
        // The copy step alone (the check has already passed in the race this models).
        let link = source.join("late-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let options = rightkit_fs::CopyOptions::new(true, true);
        rightkit_fs::copy_tree(&link, &destination.join("late-link"), options).unwrap();
        assert!(fs::symlink_metadata(destination.join("late-link")).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn backup_destination_must_be_outside_the_open_library() {
        let base = temp_library("backup-where");
        let library = base.join("library");
        fs::create_dir_all(library.join("inner")).unwrap();
        assert!(backup_destination(&library, &library).is_err(), "the library itself");
        assert!(backup_destination(&library.join("inner").join("b"), &library).is_err(), "inside the library");
        assert!(backup_destination(&base, &library).is_err(), "a folder that contains the library");
        let outside = backup_destination(&base.join("backup"), &library).unwrap();
        assert!(outside.ends_with("backup"));
        assert!(same_library_path(&library, &library.join("inner").join("..")));
        let _ = fs::remove_dir_all(base);
    }
}
