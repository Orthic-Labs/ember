//! Local structured logging for the desktop app, on the RightKit owner `rightkit-logs` (M15). Records from the
//! `log` macros go to `<app data>/logs/events.jsonl`; the web layer's `@rightkit/logs` events arrive through
//! `rightkit_logs_write`. Nothing uploads: the user collects the diagnostics bundle and decides what to share.
//!
//! Records below `warn` are kept only for Ember's own targets (`lightcraft*`), so debug output stays out of the
//! file. The owner's built-in redaction always applies to the diagnostics bundle and to web events.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level as LogLevel, LevelFilter, Log, Metadata, Record};
use rightkit_logs::{BundleMetadata, CollectionPolicy, DiagnosticsBundle, EventRecord, JsonlStore, Level, LogSource, RedactionPolicy};
use serde_json::{Value, json};

const LOG_FOLDER: &str = "logs";
const EVENTS_FILE: &str = "events.jsonl";
/// The event file rolls to `events.jsonl.1` past this size (one predecessor).
const MAX_EVENT_BYTES: u64 = 4 * 1024 * 1024;
/// Web events accepted per `rightkit_logs_write` call; the oldest beyond this are dropped and counted.
const MAX_WRITE_BATCH: usize = 500;

static STORE: OnceLock<JsonlStore> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static RK_LOG: RkLog = RkLog;

struct RkLog;

impl Log for RkLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= LogLevel::Warn || (metadata.level() <= LogLevel::Info && metadata.target().starts_with("lightcraft"))
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let Some(store) = STORE.get() else { return };
        let level = match record.level() {
            LogLevel::Error => Level::Error,
            LogLevel::Warn => Level::Warn,
            LogLevel::Info => Level::Info,
            LogLevel::Debug => Level::Debug,
            LogLevel::Trace => Level::Trace,
        };
        let event = EventRecord::new(now_ms(), level, "log")
            .with_field("target", json!(record.target()))
            .with_field("message", json!(record.args().to_string()));
        store.append_best_effort(&event);
    }

    fn flush(&self) {}
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_millis()).ok()).unwrap_or(0)
}

/// The owner's redaction policy for this app: its built-in keys (message, content, secrets) and nothing added.
fn redaction_policy() -> RedactionPolicy {
    RedactionPolicy::default()
}

/// Open the event log under `app_data/logs` and route `log::` records into it. Call once from setup. A second
/// call, or a `log` logger installed earlier, changes nothing.
pub fn install(app_data: &Path) {
    let folder = app_data.join(LOG_FOLDER);
    let _ = std::fs::create_dir_all(&folder);
    let path = folder.join(EVENTS_FILE);
    if STORE.set(JsonlStore::new(path.clone(), MAX_EVENT_BYTES)).is_err() {
        return;
    }
    let _ = LOG_PATH.set(path);
    if log::set_logger(&RK_LOG).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

/// Web events from `@rightkit/logs`. Redacted by the owner's policy, then appended in order.
#[tauri::command]
pub(crate) fn rightkit_logs_write(events: Vec<Value>) -> Result<(), String> {
    let Some(store) = STORE.get() else { return Err("the event log is not open".to_string()) };
    let dropped = events.len().saturating_sub(MAX_WRITE_BATCH);
    if dropped > 0 {
        let marker = EventRecord::new(now_ms(), Level::Warn, "logs_dropped").with_field("dropped_count", json!(dropped));
        store.append_best_effort(&marker);
    }
    let policy = redaction_policy();
    for mut event in events.into_iter().skip(dropped) {
        policy.redact_value(&mut event);
        store.append_value(&event).map_err(|e| format!("write the event log: {e}"))?;
    }
    Ok(())
}

fn diagnostics_bundle() -> Result<DiagnosticsBundle, String> {
    let path = LOG_PATH.get().ok_or_else(|| "the event log is not open".to_string())?;
    let policy = CollectionPolicy { redaction: redaction_policy(), ..CollectionPolicy::default() };
    let metadata = BundleMetadata {
        app_name: "Ember".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        build: (if cfg!(debug_assertions) { "debug" } else { "release" }).to_string(),
        os: std::env::consts::OS.to_string(),
        settings: json!({ "arch": std::env::consts::ARCH }),
    };
    Ok(rightkit_logs::collect_diagnostics(&[LogSource::new("app", path.clone())], metadata, &policy))
}

/// The diagnostics bundle the user may copy or save. Local only; never uploaded.
#[tauri::command]
pub(crate) async fn rightkit_logs_collect() -> Result<DiagnosticsBundle, String> {
    let bundle = tauri::async_runtime::spawn_blocking(diagnostics_bundle)
        .await
        .map_err(|e| format!("join rightkit_logs_collect: {e}"))??;
    Ok(bundle)
}

fn clear_store() -> Result<(), String> {
    let store = STORE.get().ok_or_else(|| "the event log is not open".to_string())?;
    store.clear().map_err(|e| format!("clear the event log: {e}"))
}

/// Remove the local event log (the user's own choice, from settings or the diagnostics panel).
#[tauri::command]
pub(crate) async fn rightkit_logs_clear() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(clear_store)
        .await
        .map_err(|e| format!("join rightkit_logs_clear: {e}"))?
}
