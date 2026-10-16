//! Native dialogs, external openers, reveal, import drops & library actions.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tauri::{WebviewWindow, Wry};

use rfd::{AsyncFileDialog, FileHandle};

const MAX_PATHS: usize = 512;
const MAX_PREFERENCES_BYTES: usize = 4 * 1024 * 1024;
const PHOTO_EXTENSIONS: &[&str] =
    &["jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "cr3", "nef", "nrw", "arw", "raf", "orf", "rw2", "pef", "psd", "jxl", "avif", "gif", "bmp"];
const PRESET_EXTENSIONS: &[&str] = &["lcpreset", "xmp", "lrtemplate", "zip", "dng", "lmp", "mplumpack", "cube"];

/// Previous desktop IDs & egui settings, in precedence order. Sources stay unchanged.
pub fn legacy_preferences_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME")
            .map(|home| legacy_preferences_under(&PathBuf::from(home).join("Library/Application Support")))
            .unwrap_or_default();
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(|app_data| legacy_preferences_under(&PathBuf::from(app_data))).unwrap_or_default();
    }
    #[allow(unreachable_code)]
    Vec::new()
}

fn legacy_preferences_under(root: &Path) -> Vec<PathBuf> {
    ["ai.storyteller.lightcraft", "ai.storyteller.lightcraft.preview", "LightCraft"].into_iter().map(|name| root.join(name).join("ui.json")).collect()
}

/// Copy valid legacy preferences into a missing destination without touching the source.
/// Unknown preference fields remain byte-for-byte intact. `Ok(false)` means no migration was needed.
pub fn migrate_preferences(source: &Path, destination: &Path) -> Result<bool, String> {
    if destination.exists() {
        return Ok(false);
    }
    let bytes = match fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("legacy preferences could not be read: {error}")),
    };
    if bytes.len() > MAX_PREFERENCES_BYTES {
        return Err(format!("legacy preferences exceed {MAX_PREFERENCES_BYTES} bytes"));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(_)) => {}
        Ok(_) => return Err("legacy preferences must be a JSON object".into()),
        Err(error) => return Err(format!("legacy preferences are invalid: {error}")),
    }
    let parent = destination.parent().ok_or_else(|| "preferences destination has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("creating preferences directory: {error}"))?;
    let name = destination.file_name().and_then(|name| name.to_str()).unwrap_or("ui.json");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    let temporary = parent.join(format!(".{name}.migrate-{}-{stamp}", std::process::id()));
    let result: std::io::Result<bool> = (|| {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if destination.exists() {
            return Ok(false);
        }
        fs::rename(&temporary, destination)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(true)
    })();
    match result {
        Ok(true) => Ok(true),
        Ok(false) => {
            let _ = fs::remove_file(&temporary);
            Ok(false)
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(format!("migrating legacy preferences: {error}"))
        }
    }
}

pub fn run(action: &str, params: &Value) -> Result<Value, String> {
    match action {
        "pickPhotos" | "pickFiles" | "import" => match provided_paths(params)? {
            Some(paths) => Ok(paths_value(paths)),
            None => pick_files("Photos", PHOTO_EXTENSIONS),
        },
        "chooseFiles" => match provided_paths(params)? {
            Some(paths) => Ok(paths_value(paths)),
            None => pick_files_with_params(params),
        },
        "pickFolder" | "chooseFolder" | "importFolder" | "openLibrary" => Ok(provided_path(params)?
            .map_or_else(|| path_value(rfd::FileDialog::new().set_title("Open Folder").pick_folder()), |path| path_value(Some(path)))),
        "pickLightroomCatalog" | "importLightroom" => Ok(provided_path(params)?.map_or_else(
            || path_value(rfd::FileDialog::new().set_title("Import LRCAT Catalog").add_filter("LRCAT Catalog", &["lrcat"]).pick_file()),
            |path| path_value(Some(path)),
        )),
        "pickDevice" | "chooseDevice" | "importDevice" => Ok(provided_path(params)?
            .map_or_else(|| path_value(rfd::FileDialog::new().set_title("Import from Device").pick_folder()), |path| path_value(Some(path)))),
        "pickPresetFiles" => match provided_paths(params)? {
            Some(paths) => Ok(paths_value(paths)),
            None => pick_files("Import Presets & Profiles", PRESET_EXTENSIONS),
        },
        "openFile" => match provided_paths(params)? {
            Some(paths) => Ok(paths_value(paths)),
            None => pick_files_with_params(params),
        },
        "saveFile" | "saveExport" | "exportFile" => {
            if params.get("path").is_some() {
                Ok(path_value(provided_path(params)?))
            } else {
                save_file_with_params(params)
            }
        }
        "pickCurvePresetFiles" => match provided_paths(params)? {
            Some(paths) => Ok(paths_value(paths)),
            None => pick_files("Import Point Curve Presets", &["lccurve", "json"]),
        },
        "pickTracklog" => Ok(provided_path(params)?.map_or_else(
            || path_value(rfd::FileDialog::new().set_title("Auto-Tag from Tracklog").add_filter("GPS Track Log", &["gpx"]).pick_file()),
            |path| path_value(Some(path)),
        )),
        "savePresetFile" => save_file(params, "Export Presets", "Ember Preset", "lcpreset"),
        "saveCurvePresetFile" => save_file(params, "Export Point Curve Presets", "Point Curve Preset", "lccurve"),
        "dropImport" => drop_import(params),
        "reveal" => reveal(required_string(params, "path")?.as_str()).map(|_| Value::Null),
        "openExternal" | "openExternalEditor" => open_external(params).map(|_| Value::Null),
        "openUrl" => open_url(required_string(params, "url")?.as_str()).map(|_| Value::Null),
        _ => Err(format!("unknown native action {action}")),
    }
}

/// Import-related pickers must be awaited by the Tauri command so macOS presents them from the
/// caller's window instead of synchronously dispatching from a worker to the main thread.
pub(crate) fn is_import_picker(action: &str) -> bool {
    matches!(
        action,
        "pickPhotos"
            | "pickFiles"
            | "import"
            | "chooseFiles"
            | "pickFolder"
            | "chooseFolder"
            | "importFolder"
            | "pickDevice"
            | "chooseDevice"
            | "importDevice"
            | "pickLightroomCatalog"
            | "importLightroom"
    )
}

/// Return every native action that may open a modal file or folder dialog.
pub(crate) fn is_picker_action(action: &str) -> bool {
    matches!(
        action,
        "pickPhotos"
            | "pickFiles"
            | "import"
            | "chooseFiles"
            | "pickFolder"
            | "chooseFolder"
            | "importFolder"
            | "openLibrary"
            | "pickLightroomCatalog"
            | "importLightroom"
            | "pickDevice"
            | "chooseDevice"
            | "importDevice"
            | "pickPresetFiles"
            | "openFile"
            | "saveFile"
            | "saveExport"
            | "exportFile"
            | "pickCurvePresetFiles"
            | "pickTracklog"
            | "savePresetFile"
            | "saveCurvePresetFile"
    )
}

/// Validate whether automation supplied all picker input needed to bypass a modal dialog.
pub(crate) fn has_explicit_picker_input(action: &str, params: &Value) -> Result<bool, String> {
    if matches!(action, "pickPhotos" | "pickFiles" | "import" | "chooseFiles" | "pickPresetFiles" | "openFile" | "pickCurvePresetFiles") {
        return provided_paths(params).map(|paths| paths.is_some_and(|paths| !paths.is_empty()));
    }
    provided_path(params).map(|path| path.is_some())
}

pub(crate) async fn run_import_picker(action: &str, params: &Value, parent: &WebviewWindow<Wry>) -> Result<Value, String> {
    match action {
        "pickPhotos" | "pickFiles" | "import" => {
            if let Some(paths) = provided_paths(params)?.filter(|paths| !paths.is_empty()) {
                return Ok(paths_value(paths));
            }
            pick_files_async(parent, "Photos", PHOTO_EXTENSIONS).await
        }
        "chooseFiles" => {
            if let Some(paths) = provided_paths(params)?.filter(|paths| !paths.is_empty()) {
                return Ok(paths_value(paths));
            }
            pick_files_with_params_async(parent, params).await
        }
        "pickFolder" | "chooseFolder" | "importFolder" => {
            if let Some(path) = provided_path(params)? {
                return Ok(path_value(Some(path)));
            }
            pick_folder_async(parent, "Open Folder").await
        }
        "pickDevice" | "chooseDevice" | "importDevice" => {
            if let Some(path) = provided_path(params)? {
                return Ok(path_value(Some(path)));
            }
            pick_folder_async(parent, "Import from Device").await
        }
        "pickLightroomCatalog" | "importLightroom" => {
            if let Some(path) = provided_path(params)? {
                return Ok(path_value(Some(path)));
            }
            let dialog = AsyncFileDialog::new().set_parent(parent).set_title("Import LRCAT Catalog").add_filter("LRCAT Catalog", &["lrcat"]);
            Ok(path_value(dialog.pick_file().await.map(|file| file.path().to_path_buf())))
        }
        _ => Err(format!("unknown import picker {action}")),
    }
}

async fn pick_files_async(parent: &WebviewWindow<Wry>, title: &str, extensions: &[&str]) -> Result<Value, String> {
    let files = AsyncFileDialog::new().set_parent(parent).set_title(title).add_filter(title, extensions).pick_files().await.unwrap_or_default();
    selected_paths(files)
}

async fn pick_files_with_params_async(parent: &WebviewWindow<Wry>, params: &Value) -> Result<Value, String> {
    let extensions = params
        .get("extensions")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).filter(|value| value.len() < 32).collect::<Vec<_>>())
        .unwrap_or_default();
    let dialog = AsyncFileDialog::new().set_parent(parent).set_title("Open File");
    let dialog = if extensions.is_empty() { dialog } else { dialog.add_filter("Selected files", &extensions) };
    if params.get("multiple").and_then(Value::as_bool).unwrap_or(true) {
        selected_paths(dialog.pick_files().await.unwrap_or_default())
    } else {
        Ok(path_value(dialog.pick_file().await.map(|file| file.path().to_path_buf())))
    }
}

async fn pick_folder_async(parent: &WebviewWindow<Wry>, title: &str) -> Result<Value, String> {
    let path = AsyncFileDialog::new().set_parent(parent).set_title(title).pick_folder().await;
    Ok(path_value(path.map(|folder| folder.path().to_path_buf())))
}

fn selected_paths(files: Vec<FileHandle>) -> Result<Value, String> {
    if files.len() > MAX_PATHS {
        return Err("too many files selected".into());
    }
    Ok(paths_value(files.into_iter().map(|file| file.path().to_string_lossy().to_string()).collect()))
}

fn pick_files(title: &str, extensions: &[&str]) -> Result<Value, String> {
    let files = rfd::FileDialog::new().set_title(title).add_filter(title, extensions).pick_files().unwrap_or_default();
    if files.len() > MAX_PATHS {
        return Err("too many files selected".into());
    }
    Ok(json!({"paths": files.into_iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>() }))
}

fn pick_files_with_params(params: &Value) -> Result<Value, String> {
    let extensions = params
        .get("extensions")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).filter(|value| value.len() < 32).collect::<Vec<_>>())
        .unwrap_or_default();
    let dialog = rfd::FileDialog::new().set_title("Open File");
    let dialog = if extensions.is_empty() { dialog } else { dialog.add_filter("Selected files", &extensions) };
    let paths = if params.get("multiple").and_then(Value::as_bool).unwrap_or(true) {
        dialog.pick_files().unwrap_or_default()
    } else {
        dialog.pick_file().into_iter().collect()
    };
    if paths.len() > MAX_PATHS {
        return Err("too many files selected".into());
    }
    Ok(json!({"paths": paths.into_iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>() }))
}

fn save_file(params: &Value, title: &str, filter: &str, extension: &str) -> Result<Value, String> {
    if params.get("path").is_some() {
        return Ok(path_value(provided_path(params)?));
    }
    let name = params.get("name").and_then(Value::as_str).unwrap_or("Ember Preset");
    if name.is_empty() || name.contains('\0') {
        return Err("invalid file name".into());
    }
    if name.len() > 240 {
        return Err("file name is too long".into());
    }
    Ok(path_value(rfd::FileDialog::new().set_title(title).add_filter(filter, &[extension]).set_file_name(name).save_file()))
}

fn save_file_with_params(params: &Value) -> Result<Value, String> {
    let suggested = params.get("suggestedName").and_then(Value::as_str).unwrap_or("Ember Export");
    let extension = params.get("extensions").and_then(Value::as_array).and_then(|items| items.first()).and_then(Value::as_str).unwrap_or("lcpreset");
    save_file(&json!({"name": suggested}), "Save File", "Ember file", extension)
}

fn drop_import(params: &Value) -> Result<Value, String> {
    let paths = params.get("paths").and_then(Value::as_array).ok_or_else(|| "dropImport expects paths".to_string())?;
    if paths.len() > MAX_PATHS {
        return Err("too many dropped paths".into());
    }
    let mut output = Vec::with_capacity(paths.len());
    for value in paths {
        let path = value.as_str().ok_or_else(|| "drop path must be a string".to_string())?;
        if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
            return Err("invalid dropped path".into());
        }
        output.push(path.to_string());
    }
    Ok(json!({"paths": output}))
}

fn path_value(path: Option<PathBuf>) -> Value {
    match path {
        Some(path) => json!({"path": path.to_string_lossy().to_string()}),
        None => json!({"path": null}),
    }
}

fn paths_value(paths: Vec<String>) -> Value {
    json!({"paths": paths})
}

fn provided_paths(params: &Value) -> Result<Option<Vec<String>>, String> {
    let Some(value) = params.get("paths") else { return Ok(None) };
    let paths = value.as_array().ok_or_else(|| "paths must be an array".to_string())?;
    if paths.len() > MAX_PATHS {
        return Err("too many paths supplied".into());
    }
    let mut output = Vec::with_capacity(paths.len());
    for value in paths {
        let path = value.as_str().ok_or_else(|| "path must be a string".to_string())?;
        if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
            return Err("invalid path".into());
        }
        output.push(path.to_string());
    }
    Ok(Some(output))
}

fn provided_path(params: &Value) -> Result<Option<PathBuf>, String> {
    let Some(value) = params.get("path") else { return Ok(None) };
    let path = value.as_str().ok_or_else(|| "path must be a string".to_string())?;
    if path.is_empty() || path.len() > 8_192 || path.contains('\0') {
        return Err("invalid path".into());
    }
    Ok(Some(PathBuf::from(path)))
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| format!("native action requires {key}"))?;
    if value.is_empty() || value.len() > 8_192 || value.contains('\0') {
        return Err(format!("invalid {key}"));
    }
    Ok(value.to_string())
}

fn open_external(params: &Value) -> Result<(), String> {
    let path = required_string(params, "path")?;
    let app = params.get("app").and_then(Value::as_str).unwrap_or("").trim();
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        if !app.is_empty() {
            command.args(["-a", app]);
        }
        command
    } else if cfg!(target_os = "windows") && app.is_empty() {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else if cfg!(target_os = "windows") {
        Command::new(app)
    } else if app.is_empty() {
        Command::new("xdg-open")
    } else {
        Command::new(app)
    };
    command.arg(path).spawn().map(|_| ()).map_err(|error| format!("opening external editor: {error}"))
}

fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("only https URLs may be opened".into());
    }
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(url).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    };
    status.map_err(|error| format!("opening URL: {error}"))?.success().then_some(()).ok_or_else(|| "external URL opener failed".into())
}

fn reveal(path: &str) -> Result<(), String> {
    let candidate = Path::new(path);
    let status = if cfg!(target_os = "macos") {
        Command::new("open").args(["-R", path]).status()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(format!("/select,{path}")).status()
    } else {
        let directory = candidate.parent().unwrap_or_else(|| Path::new("."));
        Command::new("xdg-open").arg(directory).status()
    };
    status.map_err(|error| format!("revealing path: {error}"))?.success().then_some(()).ok_or_else(|| "reveal failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_dir() -> PathBuf {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
        std::env::temp_dir().join(format!("lightcraft-native-preferences-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn legacy_preferences_candidates_keep_primary_before_preview() {
        let root = Path::new("profiles");
        assert_eq!(
            legacy_preferences_under(root),
            vec![
                root.join("ai.storyteller.lightcraft/ui.json"),
                root.join("ai.storyteller.lightcraft.preview/ui.json"),
                root.join("LightCraft/ui.json"),
            ]
        );
    }

    #[test]
    fn migration_copies_unknown_fields_once() -> Result<(), Box<dyn std::error::Error>> {
        let root = fixture_dir();
        fs::create_dir_all(&root)?;
        let source = root.join("legacy-ui.json");
        let destination = root.join("new/ui.json");
        fs::write(&source, br#"{"libraryPath":"/catalog","futureField":{"keep":true}}"#)?;
        assert!(migrate_preferences(&source, &destination)?);
        assert_eq!(fs::read(&source)?, fs::read(&destination)?);
        assert!(!migrate_preferences(&source, &destination)?);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn migration_preserves_corrupt_source() -> Result<(), Box<dyn std::error::Error>> {
        let root = fixture_dir();
        fs::create_dir_all(&root)?;
        let source = root.join("legacy-ui.json");
        let destination = root.join("new/ui.json");
        let corrupt = br#"{"libraryPath":"unterminated""#;
        fs::write(&source, corrupt)?;
        assert!(migrate_preferences(&source, &destination).is_err());
        assert_eq!(fs::read(&source)?, corrupt);
        assert!(!destination.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn picker_contract_requires_input_for_every_dialog_alias() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join("lightcraft-picker-contract.txt");
        let path_text = path.to_string_lossy().to_string();
        for action in ["pickPhotos", "pickPresetFiles", "pickCurvePresetFiles", "openFile"] {
            assert!(is_picker_action(action));
            assert!(!has_explicit_picker_input(action, &json!({}))?);
            assert!(has_explicit_picker_input(action, &json!({"paths": [path_text.clone()]}))?);
        }
        for action in ["pickFolder", "openLibrary", "pickTracklog", "saveFile", "savePresetFile", "saveCurvePresetFile"] {
            assert!(is_picker_action(action));
            assert!(!has_explicit_picker_input(action, &json!({}))?);
            assert!(has_explicit_picker_input(action, &json!({"path": path_text.clone()}))?);
        }
        assert!(!is_picker_action("openExternal"));
        assert!(!has_explicit_picker_input("openFile", &json!({"path": path_text.clone()}))?);
        Ok(())
    }

    #[test]
    fn explicit_paths_bypass_new_picker_aliases() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join("lightcraft-picker-contract.txt");
        let path_text = path.to_string_lossy().to_string();
        for (action, params, key) in [
            ("pickPresetFiles", json!({"paths": [path_text.clone()]}), "paths"),
            ("pickCurvePresetFiles", json!({"paths": [path_text.clone()]}), "paths"),
            ("pickTracklog", json!({"path": path_text.clone()}), "path"),
            ("savePresetFile", json!({"path": path_text.clone()}), "path"),
            ("saveCurvePresetFile", json!({"path": path_text.clone()}), "path"),
        ] {
            let value = run(action, &params)?;
            assert!(value.get(key).is_some(), "{action} must return supplied picker input");
        }
        Ok(())
    }
}
