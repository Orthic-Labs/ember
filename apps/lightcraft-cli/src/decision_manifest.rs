//! Read-only inventory and provenance gate for the offline Auto decision experiment.
//!
//! This deliberately creates a private, path-bearing *discovery* manifest. It does not decode
//! images, infer pairs from names, extract numeric features, or assign train/validation/test
//! splits. Explicit pair evidence is required before a row can proceed to a later alignment gate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use lightcraft_engine::lightroom_catalog::{self, CatalogPhoto};
use serde_json::{Map, Value, json};

const SCHEMA: &str = "ember.ai-decision-discovery.v1";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_MEDIA_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DIMENSION: u64 = 100_000;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_ALIGNMENT_RESIDUAL_PX: f64 = 1.0;

#[derive(Clone, Debug)]
struct Pair {
    source_uuid: String,
    virtual_copy_identity: String,
    source_path: String,
    export_path: String,
    edit_version_timestamp: String,
    export_recipe: Value,
}

#[derive(Clone, Debug)]
struct HistoryDecision {
    class: &'static str,
    confidence: Option<&'static str>,
    reason: Option<&'static str>,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let options = parse_args(args)?;
    let catalog_path = fs::canonicalize(&options.catalog).map_err(|e| format!("catalog path: {e}"))?;
    let raw_root = canonical_directory(&options.raw_root, "raw root")?;
    let export_root = canonical_directory(&options.export_root, "export root")?;
    refuse_live_catalog(&catalog_path)?;
    let catalog_before = fs::metadata(&catalog_path).map_err(|e| format!("catalog metadata: {e}"))?;
    if !catalog_before.is_file() || catalog_before.len() > 1024 * 1024 * 1024 {
        return Err("catalog must be a regular file no larger than 1 GiB".into());
    }

    let pair_path = options.pairs.as_deref().map(fs::canonicalize).transpose().map_err(|e| format!("pair manifest path: {e}"))?;
    let pairs = match pair_path.as_deref() {
        Some(path) => read_pairs(path)?,
        None => BTreeMap::new(),
    };
    eprintln!("ai decision manifest: warning: use a closed stable catalog snapshot; concurrent-writer absence cannot be proven");
    let catalog = lightroom_catalog::read(&catalog_path).map_err(|e| format!("catalog read: {e}"))?;
    refuse_live_catalog(&catalog_path)?;
    let catalog_after = fs::metadata(&catalog_path).map_err(|e| format!("catalog metadata: {e}"))?;
    if catalog_before.len() != catalog_after.len() || catalog_before.modified().ok() != catalog_after.modified().ok() {
        return Err("catalog changed during discovery; close Lightroom and retry with a stable local snapshot".into());
    }
    if catalog.photos.len() > MAX_ROWS {
        return Err(format!("catalog has {} photos; discovery cap is {MAX_ROWS}", catalog.photos.len()));
    }

    let mut photos_by_id = BTreeMap::new();
    for photo in &catalog.photos {
        photos_by_id.insert(photo.source_id, photo);
    }
    let mut consumed_pairs = BTreeSet::new();
    let mut drop_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut rows = Vec::with_capacity(catalog.photos.len());
    let mut estimated_output_bytes = 4096usize;

    for photo in &catalog.photos {
        let history = classify_history(&photo.history);
        let pair = if photo.uuid.trim().is_empty() { None } else { pairs.get(&photo.uuid) };
        let (pair_status, pair_drop_reason, pair_value) = match pair {
            None => ("missing", Some("missing_explicit_pair"), Value::Null),
            Some(pair) => {
                consumed_pairs.insert(pair.source_uuid.clone());
                match validate_pair(photo, pair, &photos_by_id, &raw_root, &export_root) {
                    Ok(recipe) => ("validated_metadata", None, recipe),
                    Err(reason) => ("invalid", Some(reason), Value::Null),
                }
            }
        };
        let source_path = canonical_source_path(&photo.path).filter(|path| path.as_os_str().to_string_lossy().len() <= MAX_PATH_BYTES);
        let mut reasons = Vec::new();
        if photo.uuid.trim().is_empty() {
            reasons.push("catalog_source_uuid_missing");
        }
        if let Some(reason) = pair_drop_reason {
            reasons.push(reason);
        }
        if let Some(reason) = history.reason {
            reasons.push(reason);
        }
        if source_path.is_none() {
            reasons.push("catalog_source_path_unavailable_or_too_long");
        }
        for reason in &reasons {
            *drop_counts.entry((*reason).to_owned()).or_default() += 1;
        }
        let folder_session = source_path.as_deref().and_then(Path::parent).map(|parent| format!("folder-session:{}", parent.to_string_lossy()));
        let virtual_copy_identity = virtual_identity(photo, &photos_by_id);
        let row = json!({
            "source_uuid": photo.uuid,
            "source_id": photo.source_id,
            "virtual_copy_identity": virtual_copy_identity,
            "source_family_id": virtual_identity(photo, &photos_by_id),
            "source_path": source_path.map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|| photo.path.clone()),
            "export_path": pair.map(|p| p.export_path.clone()),
            "edit_version_timestamp": pair.map(|p| p.edit_version_timestamp.clone()),
            "export_recipe": pair_value,
            "history": { "class": history.class, "confidence": history.confidence },
            "pair_status": pair_status,
            "decision": if reasons.is_empty() { "paired_candidate_pending_alignment" } else { "dropped" },
            "drop_reasons": reasons,
            "folder_session_id": folder_session,
            "split": "unassigned"
        });
        estimated_output_bytes = estimated_output_bytes.saturating_add(serde_json::to_vec(&row).map_err(|_| "could not size discovery row")?.len());
        if estimated_output_bytes > MAX_OUTPUT_BYTES {
            return Err(format!("discovery output is predicted to exceed {MAX_OUTPUT_BYTES} byte cap"));
        }
        rows.push(row);
    }

    let orphan_pairs = pairs.keys().filter(|uuid| !consumed_pairs.contains(*uuid)).count();
    if orphan_pairs > 0 {
        *drop_counts.entry("pair_source_uuid_not_in_catalog".to_owned()).or_default() += orphan_pairs;
    }
    let pending = rows.iter().filter(|row| row.get("decision").and_then(Value::as_str) == Some("paired_candidate_pending_alignment")).count();
    let dropped = rows.len().saturating_sub(pending);
    let report = json!({
        "schema": SCHEMA,
        "version": 1,
        "kind": "private_path_bearing_discovery_only",
        "catalog_snapshot": "reader_checked_before_and_after; concurrent-writer absence cannot be proven",
        "snapshot_warning": "Use a closed, stable catalog snapshot. Discovery does not mutate the catalog, WAL, media, or exports.",
        "limits": {
            "pair_manifest_bytes": MAX_MANIFEST_BYTES,
            "output_bytes": MAX_OUTPUT_BYTES,
            "photos_and_pairs": MAX_ROWS,
            "media_bytes": MAX_MEDIA_BYTES,
            "max_export_dimension": MAX_DIMENSION,
            "max_export_pixels": MAX_PIXELS,
            "max_geometry_residual_px": MAX_ALIGNMENT_RESIDUAL_PX,
            "history_policy": "known English history identifiers only; unknown/localized history is dropped",
            "split_policy": "folder_session_id and source_family_id are grouping evidence; any later split must keep both intact; every split remains unassigned"
        },
        "counts": {
            "catalog_photos": rows.len(),
            "paired_candidates_pending_alignment": pending,
            "dropped_catalog_photos": dropped,
            "orphan_pair_entries": orphan_pairs,
            "drop_reasons": drop_counts,
            "catalog_warnings": catalog.warnings
        },
        "rows": rows
    });
    let bytes = serde_json::to_vec_pretty(&report).map_err(|_| "could not encode discovery manifest")?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(format!("discovery output exceeds {MAX_OUTPUT_BYTES} byte cap"));
    }
    let output = resolve_new_output(&options.output, &[&catalog_path, pair_path.as_deref().unwrap_or(Path::new(""))])?;
    let mut file =
        OpenOptions::new().write(true).create_new(true).open(&output).map_err(|e| format!("cannot create new output {}: {e}", output.display()))?;
    file.write_all(&bytes).map_err(|e| format!("cannot write output: {e}"))?;
    file.flush().map_err(|e| format!("cannot flush output: {e}"))?;
    println!(
        "{}: {} catalog photos; {} paired candidates pending alignment; {} dropped; {} orphan pair entries; output {}",
        SCHEMA,
        rows.len(),
        pending,
        dropped,
        orphan_pairs,
        output.display()
    );
    Ok(())
}

#[derive(Debug)]
struct Options {
    catalog: PathBuf,
    raw_root: PathBuf,
    export_root: PathBuf,
    pairs: Option<PathBuf>,
    output: PathBuf,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut catalog = None;
    let mut raw_root = None;
    let mut export_root = None;
    let mut pairs = None;
    let mut output = None;
    let mut index = 0usize;
    while index < args.len() {
        let option = args.get(index).map(String::as_str).unwrap_or("");
        if matches!(option, "--help" | "-h") {
            return Err("ai decision manifest --lrcat FILE --raw-root DIR --export-root DIR --out NEW_FILE [--pairs PRIVATE_JSON]".into());
        }
        let value = match option {
            "--lrcat" => Some(&mut catalog),
            "--raw-root" => Some(&mut raw_root),
            "--export-root" => Some(&mut export_root),
            "--pairs" => Some(&mut pairs),
            "--out" => Some(&mut output),
            value if value.starts_with('-') => return Err(format!("unknown decision manifest option `{value}`")),
            _ => return Err("decision manifest accepts named options only".into()),
        };
        let slot = value.ok_or("invalid decision manifest option")?;
        index = index.saturating_add(1);
        let text = args.get(index).filter(|value| !value.starts_with('-')).ok_or_else(|| format!("{option}: missing value"))?;
        if slot.is_some() {
            return Err(format!("{option}: specified more than once"));
        }
        *slot = Some(PathBuf::from(text));
        index = index.saturating_add(1);
    }
    Ok(Options {
        catalog: catalog.ok_or("decision manifest requires --lrcat")?,
        raw_root: raw_root.ok_or("decision manifest requires --raw-root")?,
        export_root: export_root.ok_or("decision manifest requires --export-root")?,
        pairs,
        output: output.ok_or("decision manifest requires --out NEW_FILE")?,
    })
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path).map_err(|e| format!("{label}: {e}"))?;
    if !fs::metadata(&canonical).map_err(|e| format!("{label}: {e}"))?.is_dir() {
        return Err(format!("{label} must be a directory"));
    }
    Ok(canonical)
}

fn refuse_live_catalog(path: &Path) -> Result<(), String> {
    let appended = PathBuf::from(format!("{}.lock", path.display()));
    let stem_lock = path.with_extension("lock");
    if appended.exists() || stem_lock.exists() {
        return Err("catalog has a Lightroom lock file; close Lightroom and use a stable catalog snapshot".into());
    }
    Ok(())
}

fn read_pairs(path: &Path) -> Result<BTreeMap<String, Pair>, String> {
    let file = File::open(path).map_err(|e| format!("pair manifest: {e}"))?;
    if file.metadata().map_err(|e| format!("pair manifest metadata: {e}"))?.len() > MAX_MANIFEST_BYTES {
        return Err(format!("pair manifest exceeds {MAX_MANIFEST_BYTES} byte cap"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| format!("pair manifest read: {e}"))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!("pair manifest exceeds {MAX_MANIFEST_BYTES} byte cap"));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| format!("invalid pair manifest JSON: {e}"))?;
    let root = value.as_object().ok_or("pair manifest root must be an object")?;
    reject_unknown(root, &["schema", "version", "pairs"], "pair manifest")?;
    if root.get("schema").and_then(Value::as_str) != Some("ember.ai-decision-pairs.v1") || root.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("pair manifest requires schema ember.ai-decision-pairs.v1 and version 1".into());
    }
    let entries = root.get("pairs").and_then(Value::as_array).ok_or("pair manifest requires pairs array")?;
    if entries.len() > MAX_ROWS {
        return Err(format!("pair count exceeds {MAX_ROWS} cap"));
    }
    let mut pairs = BTreeMap::new();
    for entry in entries {
        let object = entry.as_object().ok_or("pair entry must be an object")?;
        reject_unknown(
            object,
            &["source_uuid", "virtual_copy_identity", "source_path", "export_path", "edit_version_timestamp", "export_recipe"],
            "pair entry",
        )?;
        let source_uuid = required_text(object, "source_uuid")?;
        validate_id(&source_uuid, "source_uuid")?;
        let pair = Pair {
            source_uuid: source_uuid.clone(),
            virtual_copy_identity: required_text(object, "virtual_copy_identity")?,
            source_path: required_path_text(object, "source_path")?,
            export_path: required_path_text(object, "export_path")?,
            edit_version_timestamp: required_text(object, "edit_version_timestamp")?,
            export_recipe: object.get("export_recipe").cloned().ok_or("pair entry requires export_recipe")?,
        };
        validate_id(&pair.virtual_copy_identity, "virtual_copy_identity")?;
        if !is_rfc3339(&pair.edit_version_timestamp) {
            return Err("edit_version_timestamp must be RFC 3339 with an explicit zone".into());
        }
        validate_recipe(&pair.export_recipe)?;
        if pairs.insert(source_uuid, pair).is_some() {
            return Err("duplicate source_uuid in pair manifest".into());
        }
    }
    Ok(pairs)
}

fn validate_pair(
    photo: &CatalogPhoto,
    pair: &Pair,
    photos_by_id: &BTreeMap<i64, &CatalogPhoto>,
    raw_root: &Path,
    export_root: &Path,
) -> Result<Value, &'static str> {
    if pair.source_uuid != photo.uuid {
        return Err("pair_source_uuid_mismatch");
    }
    if pair.virtual_copy_identity != virtual_identity(photo, photos_by_id) {
        return Err("pair_virtual_identity_mismatch");
    }
    let expected_source = fs::canonicalize(&photo.path).map_err(|_| "catalog_source_unavailable")?;
    if !expected_source.starts_with(raw_root) {
        return Err("catalog_source_outside_raw_root");
    }
    let supplied_source = canonical_input_path(&pair.source_path).map_err(|_| "pair_source_unavailable")?;
    if !supplied_source.starts_with(raw_root) {
        return Err("pair_source_outside_raw_root");
    }
    if supplied_source != expected_source {
        return Err("pair_source_path_mismatch");
    }
    let source_meta = fs::metadata(&supplied_source).map_err(|_| "pair_source_unavailable")?;
    if !source_meta.is_file() || source_meta.len() > MAX_MEDIA_BYTES {
        return Err("pair_source_not_regular_file_or_over_size_cap");
    }
    let export = canonical_input_path(&pair.export_path).map_err(|_| "pair_export_unavailable")?;
    if !export.starts_with(export_root) {
        return Err("pair_export_outside_export_root");
    }
    let export_meta = fs::metadata(&export).map_err(|_| "pair_export_unavailable")?;
    if !export_meta.is_file() || export_meta.len() > MAX_MEDIA_BYTES {
        return Err("pair_export_not_regular_file_or_over_size_cap");
    }
    let recipe = validate_recipe(&pair.export_recipe).map_err(|_| "pair_export_recipe_invalid")?;
    Ok(json!({
        "edit_version_timestamp": pair.edit_version_timestamp,
        "recipe": recipe,
        "geometry_status": "evidence_supplied_not_independently_render_verified"
    }))
}

fn validate_recipe(value: &Value) -> Result<Value, &'static str> {
    let recipe = value.as_object().ok_or("export_recipe must be an object")?;
    reject_unknown(recipe, &["color_space", "width", "height", "geometry"], "export_recipe").map_err(|_| "unknown export_recipe field")?;
    if recipe.get("color_space").and_then(Value::as_str) != Some("sRGB") {
        return Err("export_recipe color_space must be sRGB");
    }
    let width = recipe.get("width").and_then(Value::as_u64).ok_or("export width must be a positive integer")?;
    let height = recipe.get("height").and_then(Value::as_u64).ok_or("export height must be a positive integer")?;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION || width.saturating_mul(height) > MAX_PIXELS {
        return Err("export dimensions exceed limits");
    }
    let geometry = recipe.get("geometry").and_then(Value::as_object).ok_or("export_recipe requires geometry object")?;
    reject_unknown(geometry, &["kind", "evidence", "residual_px"], "geometry").map_err(|_| "unknown geometry field")?;
    let kind = geometry.get("kind").and_then(Value::as_str).ok_or("geometry kind is required")?;
    if !matches!(kind, "identity" | "crop_orientation") {
        return Err("unsupported geometry; only identity or crop_orientation is accepted");
    }
    let evidence =
        geometry.get("evidence").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("geometry evidence is required")?;
    if evidence.len() > 512 || evidence.chars().any(char::is_control) {
        return Err("geometry evidence is invalid");
    }
    let residual = geometry.get("residual_px").and_then(Value::as_f64).ok_or("geometry residual_px must be finite")?;
    if !residual.is_finite() || residual < 0.0 || residual > MAX_ALIGNMENT_RESIDUAL_PX {
        return Err("geometry residual exceeds 1.0 px");
    }
    Ok(
        json!({ "color_space": "sRGB", "width": width, "height": height, "geometry": { "kind": kind, "evidence": evidence, "residual_px": residual } }),
    )
}

fn classify_history(history: &[BTreeMap<String, Value>]) -> HistoryDecision {
    if history.is_empty() {
        return HistoryDecision { class: "unknown", confidence: None, reason: Some("history_empty") };
    }
    let mut entries = Vec::with_capacity(history.len());
    for row in history {
        let named_operation = row.get("name").and_then(Value::as_str).filter(|name| !name.trim().is_empty());
        let Some(text) = named_operation.or_else(|| row.get("text").and_then(Value::as_str)) else {
            return HistoryDecision { class: "unknown", confidence: None, reason: Some("history_label_missing_or_unknown") };
        };
        match classify_history_label(text) {
            Some("human") if named_operation.is_some() => {
                let delta = row.get("relValueString").and_then(Value::as_str).and_then(|value| value.trim().parse::<f64>().ok());
                if !delta.is_some_and(|delta| delta.is_finite() && delta != 0.0) {
                    return HistoryDecision { class: "unknown", confidence: None, reason: Some("manual_change_evidence_missing") };
                }
                entries.push((row.get("dateCreated").and_then(Value::as_f64).filter(|time| time.is_finite()), "human"));
            }
            Some("human") => return HistoryDecision { class: "unknown", confidence: None, reason: Some("manual_change_evidence_missing") },
            Some(kind) => entries.push((row.get("dateCreated").and_then(Value::as_f64).filter(|time| time.is_finite()), kind)),
            None => return HistoryDecision { class: "unknown", confidence: None, reason: Some("history_label_missing_or_unknown") },
        }
    }
    let contains_human_and_barrier =
        entries.iter().any(|(_, kind)| *kind == "human") && entries.iter().any(|(_, kind)| *kind == "auto" || *kind == "sync");
    if contains_human_and_barrier {
        if entries.iter().any(|(id, _)| id.is_none()) {
            return HistoryDecision { class: "unknown", confidence: None, reason: Some("history_order_unavailable") };
        }
        entries.sort_by(|(left, _), (right, _)| left.unwrap_or(f64::INFINITY).total_cmp(&right.unwrap_or(f64::INFINITY)));
        if entries.windows(2).any(|window| window.first().and_then(|entry| entry.0) == window.get(1).and_then(|entry| entry.0)) {
            return HistoryDecision { class: "unknown", confidence: None, reason: Some("history_order_ambiguous") };
        }
    }
    let kinds: Vec<&str> = entries.iter().map(|(_, kind)| *kind).collect();
    let last_human = kinds.iter().rposition(|kind| *kind == "human");
    let last_barrier = kinds.iter().rposition(|kind| *kind == "auto" || *kind == "sync");
    match (last_barrier, last_human) {
        (None, Some(_)) => HistoryDecision { class: "manual", confidence: Some("high"), reason: None },
        (Some(barrier), Some(human)) if human > barrier => {
            let class = if kinds.get(barrier) == Some(&"auto") { "manual_after_auto" } else { "manual_after_sync" };
            HistoryDecision { class, confidence: Some("weak"), reason: None }
        }
        (Some(barrier), Some(_)) if kinds.get(barrier) == Some(&"sync") => {
            HistoryDecision { class: "manual_before_sync", confidence: None, reason: Some("manual_before_last_sync") }
        }
        (Some(_), Some(_)) => HistoryDecision { class: "auto_overrode_manual", confidence: None, reason: Some("manual_before_last_auto") },
        (Some(_), None) if kinds.iter().all(|kind| *kind == "auto") => {
            HistoryDecision { class: "auto_only", confidence: None, reason: Some("auto_only_history") }
        }
        (Some(_), None) if kinds.iter().all(|kind| *kind == "sync") => {
            HistoryDecision { class: "sync_only", confidence: None, reason: Some("sync_or_paste_only_history") }
        }
        (Some(_), None) => HistoryDecision { class: "auto_sync_only", confidence: None, reason: Some("auto_or_sync_only_history") },
        (None, None) => HistoryDecision { class: "unknown", confidence: None, reason: Some("history_label_missing_or_unknown") },
    }
}

fn classify_history_label(text: &str) -> Option<&'static str> {
    let normalized = text.trim().to_ascii_lowercase().replace(['_', '-'], " ");
    let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    if ["auto", "auto settings", "auto tone", "automatic settings"].contains(&normalized.as_str()) {
        return Some("auto");
    }
    if ["sync", "sync settings", "paste settings", "paste from previous", "paste settings from previous"].contains(&normalized.as_str()) {
        return Some("sync");
    }
    if [
        "exposure",
        "contrast",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "white balance",
        "temperature",
        "tint",
        "texture",
        "clarity",
        "dehaze",
        "vibrance",
        "saturation",
        "crop",
        "rotate",
        "straighten",
        "tone curve",
        "color grading",
        "sharpening",
        "noise reduction",
    ]
    .contains(&normalized.as_str())
    {
        return Some("human");
    }
    None
}

fn virtual_identity(photo: &CatalogPhoto, photos_by_id: &BTreeMap<i64, &CatalogPhoto>) -> String {
    let master = photo.image.get("masterImage").and_then(Value::as_i64).unwrap_or(0);
    if master == 0 {
        format!("original:{}", if photo.uuid.is_empty() { photo.source_id.to_string() } else { photo.uuid.clone() })
    } else {
        let master_identity = photos_by_id
            .get(&master)
            .map(|photo| if photo.uuid.is_empty() { photo.source_id.to_string() } else { photo.uuid.clone() })
            .unwrap_or_else(|| master.to_string());
        format!("master:{master_identity}")
    }
}

fn canonical_source_path(path: &str) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}

fn canonical_input_path(path: &str) -> Result<PathBuf, String> {
    if path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control) {
        return Err("path is invalid or too long".into());
    }
    fs::canonicalize(path).map_err(|e| e.to_string())
}

fn resolve_new_output(output: &Path, protected: &[&Path]) -> Result<PathBuf, String> {
    let file_name = output.file_name().ok_or("--out must name a file")?;
    if output.components().any(|component| matches!(component, Component::ParentDir)) {
        return Err("--out may not contain parent traversal".into());
    }
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|e| format!("output parent: {e}"))?;
    let resolved = parent.join(file_name);
    if resolved.exists() {
        return Err("--out already exists; discovery never overwrites files".into());
    }
    for input in protected {
        if input.as_os_str().is_empty() {
            continue;
        }
        if resolved == *input {
            return Err("--out must differ from catalog and pair manifest".into());
        }
    }
    Ok(resolved)
}

fn required_text(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    let value =
        object.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| format!("{key} is required"))?;
    if value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(format!("{key} is invalid"));
    }
    Ok(value.to_owned())
}

fn required_path_text(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    let value = required_text(object, key)?;
    if value.len() > MAX_PATH_BYTES {
        return Err(format!("{key} exceeds path length cap"));
    }
    Ok(value)
}

fn validate_id(value: &str, label: &str) -> Result<(), String> {
    if value.len() > 256 || value.chars().any(|character| character.is_control() || matches!(character, '/' | '\\')) {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

fn reject_unknown(object: &Map<String, Value>, allowed: &[&str], scope: &str) -> Result<(), String> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{scope} has unknown field"));
    }
    Ok(())
}

fn is_rfc3339(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't'))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let Some(year) = timestamp_number(bytes, 0..4) else { return false };
    let Some(month) = timestamp_number(bytes, 5..7) else { return false };
    let Some(day) = timestamp_number(bytes, 8..10) else { return false };
    let Some(hour) = timestamp_number(bytes, 11..13) else { return false };
    let Some(minute) = timestamp_number(bytes, 14..16) else { return false };
    let Some(second) = timestamp_number(bytes, 17..19) else { return false };
    if year == 0 || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > days_in_month {
        return false;
    }
    let tail = bytes.get(19..).unwrap_or_default();
    if tail.last() == Some(&b'Z') {
        let fractional = tail.get(..tail.len().saturating_sub(1)).unwrap_or_default();
        return fractional.is_empty()
            || (fractional.first() == Some(&b'.')
                && fractional.get(1..).is_some_and(|digits| !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)));
    }
    let zone_start = tail.iter().position(|byte| *byte == b'+' || *byte == b'-');
    let Some(zone_start) = zone_start else { return false };
    let zone = tail.get(zone_start..).unwrap_or_default();
    let fractional = tail.get(..zone_start).unwrap_or_default();
    let fraction_ok = fractional.is_empty()
        || (fractional.first() == Some(&b'.')
            && fractional.get(1..).is_some_and(|digits| !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)));
    let Some(zone_hour) = (zone.len() == 6).then(|| timestamp_number(zone, 1..3)).flatten() else { return false };
    let Some(zone_minute) = (zone.len() == 6).then(|| timestamp_number(zone, 4..6)).flatten() else { return false };
    fraction_ok && zone.get(3) == Some(&b':') && zone_hour <= 23 && zone_minute <= 59
}

fn timestamp_number(bytes: &[u8], range: std::ops::Range<usize>) -> Option<u32> {
    let digits = bytes.get(range)?;
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(label: &str) -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([
            ("name".to_owned(), Value::String(label.to_owned())),
            ("text".to_owned(), json!("s = { Exposure2012 = 0.25 }")),
            ("relValueString".to_owned(), json!("+0.25")),
        ])]
    }

    fn history_step(label: &str, id: i64) -> BTreeMap<String, Value> {
        let mut row = history(label).remove(0);
        row.insert("id_local".into(), json!(id));
        row.insert("dateCreated".into(), json!(id));
        row
    }

    #[test]
    fn history_drops_unknown_auto_only_sync_only_and_untouched() {
        assert_eq!(classify_history(&[]).reason, Some("history_empty"));
        assert_eq!(classify_history(&history("Auto Settings")).reason, Some("auto_only_history"));
        assert_eq!(classify_history(&history("Sync Settings")).reason, Some("sync_or_paste_only_history"));
        assert_eq!(classify_history(&history("Exposición")).reason, Some("history_label_missing_or_unknown"));
        assert_eq!(classify_history(&history("s = { Exposure2012 = 0 }")).reason, Some("history_label_missing_or_unknown"));
        assert_eq!(classify_history(&history("s = { Exposure2012 = 0.5 }")).reason, Some("history_label_missing_or_unknown"));
        assert_eq!(classify_history(&history("Exposure")).confidence, Some("high"));
        let mut no_change = history("Exposure");
        no_change[0].insert("relValueString".into(), json!("0"));
        assert_eq!(classify_history(&no_change).reason, Some("manual_change_evidence_missing"));
        let text_only = vec![BTreeMap::from([("text".to_owned(), json!("Exposure"))])];
        assert_eq!(classify_history(&text_only).reason, Some("manual_change_evidence_missing"));
    }

    #[test]
    fn manual_after_auto_is_weak_and_manual_before_auto_is_dropped() {
        let after = vec![history_step("Auto Settings", 1), history_step("Exposure", 2)];
        assert_eq!(classify_history(&after).confidence, Some("weak"));
        let before = vec![history_step("Exposure", 1), history_step("Auto Settings", 2)];
        assert_eq!(classify_history(&before).reason, Some("manual_before_last_auto"));
        let final_paste = vec![history_step("Exposure", 1), history_step("Paste Settings", 2)];
        assert_eq!(classify_history(&final_paste).reason, Some("manual_before_last_sync"));
        let after_paste = vec![history_step("Exposure", 1), history_step("Paste Settings", 2), history_step("Contrast", 3)];
        assert_eq!(classify_history(&after_paste).class, "manual_after_sync");
        let unordered = vec![history("Auto Settings").remove(0), history("Exposure").remove(0)];
        assert_eq!(classify_history(&unordered).reason, Some("history_order_unavailable"));
    }

    #[test]
    fn recipe_enforces_srgb_bounded_dimensions_and_geometry() {
        let good = json!({"color_space":"sRGB","width":4000,"height":3000,"geometry":{"kind":"crop_orientation","evidence":"local export receipt","residual_px":0.5}});
        assert!(validate_recipe(&good).is_ok());
        let bad = json!({"color_space":"Display P3","width":4000,"height":3000,"geometry":{"kind":"identity","evidence":"x","residual_px":0.0}});
        assert!(validate_recipe(&bad).is_err());
        let oversized = json!({"color_space":"sRGB","width":100000,"height":100000,"geometry":{"kind":"identity","evidence":"x","residual_px":0.0}});
        assert!(validate_recipe(&oversized).is_err());
        let perspective = json!({"color_space":"sRGB","width":4000,"height":3000,"geometry":{"kind":"perspective","evidence":"x","residual_px":0.0}});
        assert!(validate_recipe(&perspective).is_err());
    }

    #[test]
    fn timestamp_requires_explicit_timezone() {
        assert!(is_rfc3339("2026-10-11T12:30:00Z"));
        assert!(is_rfc3339("2026-10-11T12:30:00+05:30"));
        assert!(is_rfc3339("2026-10-11T12:30:00.125Z"));
        assert!(is_rfc3339("2024-02-29T12:30:00-04:00"));
        assert!(!is_rfc3339("2026-10-11T12:30:00"));
        assert!(!is_rfc3339("2025-02-29T12:30:00Z"));
        assert!(!is_rfc3339("2026-13-11T12:30:00Z"));
        assert!(!is_rfc3339("2026-10-11T25:30:00Z"));
    }

    #[test]
    fn pair_validation_checks_identity_paths_and_export_bounds() {
        let unique = format!("ember-decision-manifest-{}", std::process::id());
        let base = std::env::temp_dir().join(unique);
        let raw_root = base.join("raw");
        let export_root = base.join("exports");
        let outside = base.join("outside");
        fs::create_dir_all(&raw_root).unwrap();
        fs::create_dir_all(&export_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let source = raw_root.join("source.raw");
        let export = export_root.join("export.jpg");
        fs::write(&source, b"raw").unwrap();
        fs::write(&export, b"jpeg").unwrap();
        let raw_root = fs::canonicalize(&raw_root).unwrap();
        let export_root = fs::canonicalize(&export_root).unwrap();
        let photo = CatalogPhoto {
            source_id: 1,
            uuid: "photo-1".into(),
            path: source.to_string_lossy().into_owned(),
            image: BTreeMap::new(),
            settings: String::new(),
            xmp: String::new(),
            keywords: Vec::new(),
            history: Vec::new(),
            snapshots: Vec::new(),
        };
        let photos = BTreeMap::from([(1, &photo)]);
        let pair = Pair {
            source_uuid: "photo-1".into(),
            virtual_copy_identity: "original:photo-1".into(),
            source_path: source.to_string_lossy().into_owned(),
            export_path: export.to_string_lossy().into_owned(),
            edit_version_timestamp: "2026-10-11T12:30:00Z".into(),
            export_recipe: json!({"color_space":"sRGB","width":4000,"height":3000,"geometry":{"kind":"identity","evidence":"measured local export","residual_px":0.0}}),
        };
        assert!(validate_pair(&photo, &pair, &photos, &raw_root, &export_root).is_ok());
        let mut outside_pair = pair.clone();
        outside_pair.export_path = outside.join("export.jpg").to_string_lossy().into_owned();
        fs::write(outside.join("export.jpg"), b"jpeg").unwrap();
        assert_eq!(validate_pair(&photo, &outside_pair, &photos, &raw_root, &export_root), Err("pair_export_outside_export_root"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn output_creation_refuses_existing_files() {
        let base = std::env::temp_dir().join(format!("ember-decision-output-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let output = base.join("manifest.json");
        fs::write(&output, b"keep").unwrap();
        assert!(resolve_new_output(&output, &[]).is_err());
        let _ = fs::remove_dir_all(base);
    }
}
