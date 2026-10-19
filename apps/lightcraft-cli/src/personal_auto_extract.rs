//! Build canonical numeric Personal Auto manifests from explicitly supplied photos.
//!
//! This command owns one ephemeral in-memory session per explicitly listed regular file. It
//! imports one file at a time in local mode, resets imported develop settings, computes
//! deterministic Auto and pipeline features, then writes a path-free numeric manifest. It never
//! scans a directory or writes a catalog, original, or sidecar.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use lightcraft_develop::{DevelopSettings, controls};
use lightcraft_engine::{Session, SourceLevel, camera_profiles, catalog::PhotoId, media::RENDER_CACHE_VERSION};
use lightcraft_pipeline::auto::auto_tone;
use lightcraft_pipeline::personal_auto::{BASELINE_AUTO_REVISION, BASELINE_RECEIPT_SCHEMA, FEATURE_NAMES, FEATURE_SCHEMA, FeatureVector};
use serde_json::{Map, Value, json};

const VERSION: u64 = 1;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SHOOTS: usize = 100_000;
const MAX_PHOTOS: usize = 100_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const CONTROLS: [&str; 3] = ["light.contrast", "color.vibrance", "color.saturation"];

fn baseline_receipt(
    source: &lightcraft_raster::Rgb32f,
    info: &lightcraft_pipeline::SourceInfo,
    settings: &DevelopSettings,
    features: &FeatureVector,
    baseline: &BTreeMap<String, f64>,
) -> Result<Value, String> {
    // Digest decoded proxy pixels plus every source fact/settings value used by baseline features.
    // This lets evaluation reject renderer/profile drift after private paths are removed.
    let metadata = json!({
        "sourceLevel": "thumb",
        "width": source.width,
        "height": source.height,
        "sourceInfo": {
            "lens": info.lens,
            "raw": info.raw,
            "asShotTemp": info.as_shot_temp,
            "asShotTint": info.as_shot_tint,
            "relativeWb": info.relative_wb,
            "cameraTone": info.camera_tone,
        },
        "baseline": settings,
    });
    let settings_bytes = serde_json::to_vec(settings).map_err(|_| "could not encode baseline receipt settings")?;
    let mut pixel_bytes = Vec::with_capacity(source.data.len().saturating_mul(12));
    for pixel in &source.data {
        for channel in pixel {
            pixel_bytes.extend_from_slice(&channel.to_bits().to_le_bytes());
        }
    }
    let pixel_digest = lightcraft_photo_ai::digest(&pixel_bytes);
    let settings_digest = lightcraft_photo_ai::digest(&settings_bytes);
    let features_bytes = serde_json::to_vec(&features.0).map_err(|_| "could not encode baseline receipt features")?;
    let baseline_bytes = serde_json::to_vec(baseline).map_err(|_| "could not encode baseline receipt values")?;
    let mut bytes = serde_json::to_vec(&metadata).map_err(|_| "could not encode baseline receipt metadata")?;
    bytes.extend_from_slice(&pixel_bytes);
    Ok(json!({
        "schema": BASELINE_RECEIPT_SCHEMA,
        "autoRevision": BASELINE_AUTO_REVISION,
        "featureSchema": FEATURE_SCHEMA,
        "sourceLevel": "thumb",
        "settingsVersion": settings.version,
        "sourcePixelsDigest": pixel_digest,
        "settingsDigest": settings_digest,
        "featuresDigest": lightcraft_photo_ai::digest(&features_bytes),
        "baselineDigest": lightcraft_photo_ai::digest(&baseline_bytes),
        "sourceDigest": lightcraft_photo_ai::digest(&bytes),
        "renderCacheVersion": RENDER_CACHE_VERSION,
        "cameraProfileCacheKey": camera_profiles::cache_key(),
    }))
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (mode, offset) = if args.first().map(String::as_str) == Some("personal") {
        (args.get(1).map(String::as_str), 2)
    } else {
        (args.first().map(String::as_str), 1)
    };
    if mode != Some("extract") {
        return Err("ai personal expects extract; see --help".into());
    }
    let (manifest, output) = parse_args(&args[offset..])?;
    let input = read_json(Path::new(&manifest))?;
    let plan = parse_input(&input)?;
    let value = extract(plan)?;
    write_new_json(Path::new(&output), &value)?;
    println!("{}", json!({"manifest": output, "schema": FEATURE_SCHEMA, "version": VERSION}));
    Ok(())
}

fn parse_args(args: &[String]) -> Result<(String, String), String> {
    let mut manifest = None;
    let mut output = None;
    let mut index = 0usize;
    while index < args.len() {
        match args.get(index).map(String::as_str).unwrap_or("") {
            "--manifest" => manifest = Some(next_arg(args, &mut index, "--manifest")?),
            "--out" | "-o" => output = Some(next_arg(args, &mut index, "--out")?),
            "--help" | "-h" => return Err("ai personal extract --manifest FILE --out NEW_MANIFEST".into()),
            value if value.starts_with('-') => return Err(format!("unknown personal option {value}")),
            _ => return Err("personal extract accepts named options only".into()),
        }
        index = index.saturating_add(1);
    }
    Ok((manifest.ok_or("personal extract requires --manifest")?, output.ok_or("personal extract requires --out NEW_MANIFEST")?))
}

fn next_arg(args: &[String], index: &mut usize, option: &str) -> Result<String, String> {
    *index = index.saturating_add(1);
    args.get(*index).filter(|value| !value.starts_with('-')).cloned().ok_or_else(|| format!("{option}: missing value"))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let file = File::open(path).map_err(|_| "could not read extract manifest".to_owned())?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| "could not read extract manifest".to_owned())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("extract manifest exceeds {MAX_BYTES} byte cap"));
    }
    serde_json::from_slice(&bytes).map_err(|_| "extract manifest is not valid JSON".into())
}

fn write_new_json(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| "could not encode extracted manifest")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("extracted manifest exceeds {MAX_BYTES} byte cap"));
    }
    crate::output_file::publish_new(path, &bytes).map_err(|e| match e {
        rightkit_fs::FsError::AlreadyExists(_) => "could not create new extracted manifest".to_string(),
        _ => "could not write extracted manifest".to_string(),
    })
}

#[derive(Debug)]
struct InputPlan {
    shoots: Vec<InputShoot>,
}

#[derive(Debug)]
struct InputShoot {
    id: String,
    split: String,
    camera: Option<String>,
    photos: Vec<InputPhoto>,
}

#[derive(Debug)]
struct InputPhoto {
    id: String,
    path: PathBuf,
    weak: Option<Value>,
    ember: Option<Value>,
}

fn parse_input(value: &Value) -> Result<InputPlan, String> {
    let root = value.as_object().ok_or("extract manifest root must be an object")?;
    reject_unknown(root, &["version", "userConsented", "shoots"], "extract manifest")?;
    if root.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err("extract manifest requires version 1".into());
    }
    if root.get("userConsented").and_then(Value::as_bool) != Some(true) {
        return Err("extract manifest requires userConsented:true".into());
    }
    let raw_shoots = root.get("shoots").and_then(Value::as_array).ok_or("extract manifest requires shoots")?;
    if raw_shoots.is_empty() || raw_shoots.len() > MAX_SHOOTS {
        return Err("extract shoot count is outside bounds".into());
    }
    let mut shoot_ids = BTreeSet::new();
    let mut photo_ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut photo_count = 0usize;
    let mut shoots = Vec::with_capacity(raw_shoots.len());
    for raw_shoot in raw_shoots {
        let shoot = raw_shoot.as_object().ok_or("extract shoot must be an object")?;
        reject_unknown(shoot, &["shoot_id", "split", "camera", "photos"], "extract shoot")?;
        let id = required_id(shoot, "shoot_id")?;
        if !shoot_ids.insert(id.clone()) {
            return Err("duplicate shoot_id".into());
        }
        let split = required_text(shoot, "split")?;
        if !matches!(split.as_str(), "train" | "validation" | "eval" | "test") {
            return Err("extract shoot has invalid split".into());
        }
        let camera = match shoot.get("camera") {
            None => None,
            Some(value) => {
                let camera =
                    value.as_str().map(str::trim).filter(|value| !value.is_empty()).ok_or("extract shoot camera must be an opaque string")?;
                if camera.len() > 256 || camera.chars().any(|character| character.is_control() || matches!(character, '/' | '\\')) {
                    return Err("extract shoot camera is invalid".into());
                }
                Some(camera.to_owned())
            }
        };
        let raw_photos = shoot.get("photos").and_then(Value::as_array).ok_or("extract shoot requires photos")?;
        if raw_photos.is_empty() {
            return Err("extract shoot has no photos".into());
        }
        let mut photos = Vec::with_capacity(raw_photos.len());
        for raw_photo in raw_photos {
            photo_count = photo_count.saturating_add(1);
            if photo_count > MAX_PHOTOS {
                return Err("extract photo count exceeds cap".into());
            }
            let photo = raw_photo.as_object().ok_or("extract photo must be an object")?;
            reject_unknown(photo, &["photo_id", "path", "weakLabelColdStart", "emberGroundTruth"], "extract photo")?;
            let photo_id = required_id(photo, "photo_id")?;
            if !photo_ids.insert(photo_id.clone()) {
                return Err("duplicate photo_id".into());
            }
            let raw_path = required_text(photo, "path")?;
            if raw_path.len() > MAX_PATH_BYTES {
                return Err("extract photo path exceeds cap".into());
            }
            let path = std::fs::canonicalize(&raw_path).map_err(|_| "extract photo path is unreadable")?;
            let metadata = std::fs::metadata(&path).map_err(|_| "extract photo path is unreadable")?;
            if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
                return Err("extract photo must be a regular file within size cap".into());
            }
            let key = path.to_string_lossy().into_owned();
            if !paths.insert(key) {
                return Err("duplicate extract photo path".into());
            }
            let weak = copied_label(photo, "weakLabelColdStart", false)?;
            let ember = copied_label(photo, "emberGroundTruth", true)?;
            if weak.is_none() && ember.is_none() {
                return Err(format!("{photo_id}: explicit user label is required"));
            }
            photos.push(InputPhoto { id: photo_id, path, weak, ember });
        }
        shoots.push(InputShoot { id, split, camera, photos });
    }
    Ok(InputPlan { shoots })
}

fn reject_unknown(object: &Map<String, Value>, allowed: &[&str], scope: &str) -> Result<(), String> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{scope} has unknown field"));
    }
    Ok(())
}

fn required_id(object: &Map<String, Value>, field: &str) -> Result<String, String> {
    let value = required_text(object, field)?;
    if value.len() > 256 || value.chars().any(|character| character.is_control() || matches!(character, '/' | '\\')) {
        return Err(format!("invalid {field}"));
    }
    Ok(value)
}

fn required_text(object: &Map<String, Value>, field: &str) -> Result<String, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("extract object is missing {field}"))
}

fn copied_label(object: &Map<String, Value>, key: &str, ember: bool) -> Result<Option<Value>, String> {
    let Some(value) = object.get(key) else { return Ok(None) };
    if !value.is_object() {
        return Err(format!("{key} must be an object"));
    }
    let canonical = super::personal_auto_eval::canonical_input_label(value, ember).map_err(|error| format!("{key}: {error}"))?;
    Ok(Some(canonical))
}

fn extract(plan: InputPlan) -> Result<Value, String> {
    let mut output_shoots = Vec::with_capacity(plan.shoots.len());
    for shoot in plan.shoots {
        let mut output_photos = Vec::with_capacity(shoot.photos.len());
        for photo in shoot.photos {
            // Keep catalog/raw decode state bounded to one explicitly supplied file. A
            // fresh disposable session also prevents 100k-file runs from accumulating
            // imported catalog entries or source caches.
            let mut session = Session::new().with_fs();
            session.xmp.auto_write = false;
            let imported = session
                .execute("library.import", &json!({"paths": [photo.path.to_string_lossy()], "local": true, "rawJpegPolicy": "keepBoth"}))
                .map_err(|_| "extract could not import explicit photo")?;
            let ids = imported.get("imported").and_then(Value::as_array).ok_or("extract import returned no photo id")?;
            if ids.len() != 1 || imported.get("duplicates").and_then(Value::as_array).is_some_and(|duplicates| !duplicates.is_empty()) {
                return Err("extract import did not return exactly one photo".into());
            }
            let id = ids.first().and_then(Value::as_u64).map(PhotoId).ok_or("extract import returned invalid photo id")?;
            session
                .set_develop(id, DevelopSettings::default(), "Personal Auto extract baseline")
                .map_err(|_| "extract could not reset imported develop settings")?;
            let source = session.source_now(id, SourceLevel::Thumb).map_err(|_| "extract could not decode explicit photo")?;
            let info = session.source_info(id);
            let mut settings = DevelopSettings::default();
            let auto = auto_tone(&source, &info, &settings);
            if !auto.exposure.is_finite() || !auto.contrast.is_finite() || !auto.vibrance.is_finite() || !auto.saturation.is_finite() {
                return Err("extract deterministic Auto returned non-finite settings".into());
            }
            settings.light.exposure = auto.exposure;
            for (name, value) in [(CONTROLS[0], auto.contrast), (CONTROLS[1], auto.vibrance), (CONTROLS[2], auto.saturation)] {
                if !controls::set(&mut settings, name, value) {
                    return Err("extract deterministic Auto control is unavailable".into());
                }
            }
            let features =
                FeatureVector::from_pipeline(&source, &info, &settings).map_err(|_| "extract could not compute canonical pipeline features")?;
            let baseline = BTreeMap::from([
                (CONTROLS[0].to_owned(), auto.contrast),
                (CONTROLS[1].to_owned(), auto.vibrance),
                (CONTROLS[2].to_owned(), auto.saturation),
            ]);
            let receipt = baseline_receipt(&source, &info, &settings, &features, &baseline)?;
            let mut output = Map::new();
            output.insert("photo_id".into(), Value::String(photo.id));
            output.insert("features".into(), serde_json::to_value(features.0).map_err(|_| "extract could not encode features")?);
            output.insert("baseline".into(), serde_json::to_value(baseline).map_err(|_| "extract could not encode baseline")?);
            output.insert("baselineReceipt".into(), receipt);
            if let Some(label) = photo.weak {
                output.insert("weakLabelColdStart".into(), label);
            }
            if let Some(label) = photo.ember {
                output.insert("emberGroundTruth".into(), label);
            }
            output_photos.push(Value::Object(output));
        }
        let mut output_shoot = json!({"shoot_id": shoot.id, "split": shoot.split, "photos": output_photos});
        if let Some(camera) = shoot.camera {
            output_shoot["camera"] = Value::String(camera);
        }
        output_shoots.push(output_shoot);
    }
    Ok(json!({
        "version": VERSION,
        "feature_schema": FEATURE_NAMES,
        "controls": CONTROLS,
        "baselineReceiptSchema": BASELINE_RECEIPT_SCHEMA,
        "shoots": output_shoots,
        "provenance": {"mode": "explicit-files", "numericOnly": true, "pathsOmitted": true, "exifOmitted": true, "libraryMutated": false}
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_requires_explicit_consent() {
        let value = json!({"version": 1, "shoots": []});
        let error = parse_input(&value).expect_err("consent marker must be required");
        assert!(error.contains("userConsented:true"));
    }

    #[test]
    fn input_rejects_unknown_contract_fields() {
        let value = json!({"version": 1, "userConsented": true, "extra": true, "shoots": []});
        let error = parse_input(&value).expect_err("strict extraction schema must reject unknown root fields");
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn canonical_label_strips_private_provenance_fields() {
        let input = json!({
            "values": {"light.contrast": 1.0},
            "confidence": {"light.contrast": 1.0},
            "provenance": {"source": "lightroom", "path": "/private/original.xmp", "exif": {"artist": "private"}}
        });
        let parent = json!({"weakLabelColdStart": input});
        let output = copied_label(parent.as_object().unwrap(), "weakLabelColdStart", false).unwrap().unwrap();
        assert!(output["provenance"].get("path").is_none());
        assert!(output["provenance"].get("exif").is_none());
        assert_eq!(output["provenance"]["kind"], "lightroom");
        assert!(output["provenance"]["provenanceSha256"].as_str().is_some());
    }

    #[test]
    fn opaque_ids_reject_path_separators() {
        let value = json!({
            "version": 1,
            "userConsented": true,
            "shoots": [{"shoot_id": "shoot/id", "split": "train", "photos": [{"photo_id": "p", "path": "/missing"}]}]
        });
        let error = parse_input(&value).expect_err("opaque shoot IDs must not carry path separators");
        assert!(error.contains("shoot_id"));
    }
}
