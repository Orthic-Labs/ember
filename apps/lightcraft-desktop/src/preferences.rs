//! Crash-safe app preferences for the Tauri desktop host, persisted by `rightkit-app-settings`.
//!
//! Preferences are JSON objects rather than a typed projection so fields introduced by another
//! Ember host survive a round trip through this app. The file stays the flat object `ui.json`
//! has always been (`FileFormat::Flat`): the egui host and the startup readers in `native.rs`
//! (`persisted_library_path`, `model_paths`) read it directly, undeclared keys are kept, and an
//! existing file is adopted byte for byte until the first real change.
//!
//! RightKit owns the atomic write, the cross-process lock, `.bak` retention, and quarantine of a
//! damaged file. Ember keeps the deep-merge patch semantics and the 4 MiB bound.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_app_settings::{Error as StoreError, FileFormat, Schema, Store, StoreOptions};
use serde_json::{Map, Value};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const SCHEMA_VERSION: u32 = 1;
const OPEN_ATTEMPTS: u32 = 3;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(40);

type PrefMap = Map<String, Value>;

#[derive(Clone)]
pub struct Preferences {
    inner: Arc<Mutex<State>>,
}

struct State {
    /// `None` while the file could not be opened at all (unreadable, oversized or from a newer schema, not
    /// merely missing or corrupt): the app runs on defaults and every save is refused.
    store: Option<Store<PrefMap>>,
    path: PathBuf,
    warning: Option<String>,
}

/// The empty-object defaults provider keeps a missing file missing: without it the store treats a missing
/// document as dirty and creates it on open. With it, the first patch that changes a value writes the file.
fn store_options() -> StoreOptions {
    StoreOptions::new(Schema::new("ember", SCHEMA_VERSION)).format(FileFormat::Flat).trailing_newline(true).max_bytes(MAX_BYTES as u64).defaults_provider(|| Value::Object(Map::new()))
}

fn open_store(path: &Path) -> Result<Store<PrefMap>, StoreError> {
    let mut attempt = 1;
    loop {
        match Store::<PrefMap>::open_path(path, store_options()) {
            Err(StoreError::Io(_)) if attempt < OPEN_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(OPEN_RETRY_DELAY);
            }
            other => return other,
        }
    }
}

/// What is wrong with the file before the store looks at it, so the user can be told. The store
/// itself quarantines an unparseable file and refuses an oversized one.
fn diagnose(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() > MAX_BYTES => Some(format!("preferences exceed {MAX_BYTES} bytes")),
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(_)) => None,
            Ok(_) => Some("preferences must be a JSON object; the damaged file was quarantined next to it".into()),
            Err(error) => Some(format!("preferences are invalid: {error}; the damaged file was quarantined next to it")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("preferences could not be read: {error}")),
    }
}

impl Preferences {
    pub fn load(path: PathBuf) -> Self {
        let mut warning = diagnose(&path);
        let store = match open_store(&path) {
            Ok(store) => Some(store),
            Err(error) => {
                // An oversized or newer-schema file makes `open_path` fail and leaves the file untouched; keep
                // the more specific reason `diagnose` found, if any.
                warning.get_or_insert_with(|| format!("preferences could not be opened: {error}"));
                None
            }
        };
        Self { inner: Arc::new(Mutex::new(State { store, path, warning })) }
    }

    pub fn get(&self) -> Result<Value, String> {
        let state = self.inner.lock().map_err(|_| "preferences lock poisoned".to_string())?;
        let value = state.store.as_ref().map(Store::get).unwrap_or_default();
        Ok(Value::Object(value))
    }

    pub fn warning(&self) -> Option<String> {
        self.inner.lock().ok().and_then(|state| state.warning.clone())
    }

    /// Merge a bounded object patch, preserving unknown baseline keys, then persist atomically.
    pub fn patch(&self, patch: Option<Value>) -> Result<Value, String> {
        let Some(patch) = patch else { return self.get() };
        let Value::Object(values) = patch else { return Err("preferences patch must be an object".into()) };
        let state = self.inner.lock().map_err(|_| "preferences lock poisoned".to_string())?;
        let blocked = || state.warning.clone().unwrap_or_else(|| "preferences file is blocked for this session".into());
        let Some(store) = state.store.as_ref() else { return Err(blocked()) };
        let mut too_large = false;
        let result = store.update(|current| {
            merge_object(current, values);
            let size = serde_json::to_vec(&*current)?.len();
            if size > MAX_BYTES {
                too_large = true;
                return Err(StoreError::Invalid("preferences patch exceeds size limit".into()));
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(_) if too_large => return Err("preferences patch exceeds size limit".into()),
            Err(error) => return Err(format!("saving preferences to {}: {error}", state.path.display())),
        }
        Ok(Value::Object(store.get()))
    }
}

fn merge_object(target: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (key, value) in patch {
        match value {
            Value::Object(incoming) => {
                if let Some(Value::Object(existing)) = target.get_mut(&key) {
                    merge_object(existing, incoming);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_dir(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!("ember-prefs-{name}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// A `ui.json` written by a build before RightKit settings: flat, unversioned, with keys this
    /// build does not know and a nested object.
    const LEGACY: &str = "{\n  \"libraryPath\": \"/photos/catalog\",\n  \"sam3Dir\": \"models\",\n  \"theme\": \"light\",\n  \"panels\": {\"left\": true, \"right\": false},\n  \"futureSetting\": {\"keep\": [1, 2, 3]}\n}";

    #[test]
    fn existing_flat_preferences_migrate_on_first_run_without_loss() -> TestResult {
        let dir = test_dir("migrate")?;
        let path = dir.join("ui.json");
        fs::write(&path, LEGACY)?;

        let preferences = Preferences::load(path.clone());
        assert_eq!(preferences.warning(), None);
        let expected: Value = serde_json::from_str(LEGACY)?;
        assert_eq!(preferences.get()?, expected, "every legacy key reads back");
        assert_eq!(fs::read_to_string(&path)?, LEGACY, "load alone does not rewrite the file");

        let merged = preferences.patch(Some(json!({"theme": "dark", "panels": {"right": true}, "newKey": 7})))?;
        assert_eq!(merged["theme"], json!("dark"));
        assert_eq!(merged["panels"], json!({"left": true, "right": true}), "nested objects deep-merge");
        assert_eq!(merged["newKey"], json!(7));

        // A fresh load sees the saved file; the startup readers in native.rs parse it as a flat object.
        let saved: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert!(saved.get("schemaVersion").is_none() && saved.get("values").is_none(), "the file stays flat");
        assert_eq!(saved["libraryPath"], json!("/photos/catalog"));
        assert_eq!(saved["sam3Dir"], json!("models"));
        assert_eq!(saved["futureSetting"], json!({"keep": [1, 2, 3]}), "unknown keys survive a save");
        assert_eq!(Preferences::load(path).get()?, saved);

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn missing_file_starts_empty_and_creates_on_first_patch() -> TestResult {
        let dir = test_dir("missing")?;
        let path = dir.join("nested").join("ui.json");
        let preferences = Preferences::load(path.clone());
        assert_eq!(preferences.get()?, json!({}));
        assert!(!path.exists(), "nothing is written until the first change");
        preferences.patch(Some(json!({"libraryPath": "/a"})))?;
        assert_eq!(Preferences::load(path).get()?, json!({"libraryPath": "/a"}));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn damaged_file_is_quarantined_and_preferences_recover() -> TestResult {
        let dir = test_dir("corrupt")?;
        let path = dir.join("ui.json");
        fs::write(&path, b"{ not json")?;
        let preferences = Preferences::load(path.clone());
        assert!(preferences.warning().is_some_and(|warning| warning.contains("invalid")));
        assert_eq!(preferences.get()?, json!({}));
        let quarantined = fs::read_dir(&dir)?.filter_map(Result::ok).any(|entry| entry.file_name().to_string_lossy().contains(".corrupt"));
        assert!(quarantined, "the damaged bytes are kept beside the file");
        preferences.patch(Some(json!({"theme": "dark"})))?;
        assert_eq!(Preferences::load(path).get()?, json!({"theme": "dark"}));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn oversized_file_is_left_untouched_and_blocks_saves() -> TestResult {
        let dir = test_dir("oversized")?;
        let path = dir.join("ui.json");
        let filler = "x".repeat(MAX_BYTES);
        let body = format!("{{\"blob\":\"{filler}\"}}");
        fs::write(&path, &body)?;
        let preferences = Preferences::load(path.clone());
        assert!(preferences.warning().is_some_and(|warning| warning.contains("exceed")));
        assert!(preferences.patch(Some(json!({"theme": "dark"}))).is_err());
        assert_eq!(fs::read_to_string(&path)?, body, "a refused file is never rewritten");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn patch_must_be_an_object_and_stays_within_the_size_bound() -> TestResult {
        let dir = test_dir("bounds")?;
        let path = dir.join("ui.json");
        let preferences = Preferences::load(path.clone());
        assert!(preferences.patch(Some(json!([1]))).is_err());
        let huge = json!({"blob": "x".repeat(MAX_BYTES)});
        assert_eq!(preferences.patch(Some(huge)), Err("preferences patch exceeds size limit".to_string()));
        assert!(!path.exists());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
