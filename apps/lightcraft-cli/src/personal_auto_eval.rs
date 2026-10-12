//! Offline, metadata-free Personal Auto training & evaluation.
//!
//! Inputs are explicit numeric feature/setting manifests. This module never opens a
//! catalog, decodes a photo, renders an image, calls a provider, or applies a setting.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use lightcraft_develop::{DevelopSettings, SCHEMA_VERSION, controls};
use lightcraft_engine::{camera_profiles, media::RENDER_CACHE_VERSION};
use lightcraft_pipeline::personal_auto::{
    BASELINE_AUTO_REVISION, BASELINE_RECEIPT_SCHEMA, FEATURE_COUNT, FEATURE_NAMES, FEATURE_SCHEMA, FeatureVector, FieldLabel, LabelSource,
    MODEL_SCHEMA as CORE_MODEL_SCHEMA, PersonalAutoModel, ShootAssignment, Split, SplitManifest, StyleControl, StyleLabels,
};
use serde_json::{Map, Value, json};

const VERSION: u64 = 1;
const MODEL_SCHEMA: &str = "lightcraft.personal-auto-eval.model.v4";
const REPORT_SCHEMA: &str = "lightcraft.personal-auto.report.v2";
const LEGACY_MODEL_SCHEMA: &str = "lightcraft.personal-auto-eval.model.v3";
const LEGACY_CORE_MODEL_SCHEMA: &str = "lightcraft.personal-auto.v1";
const LEGACY_FEATURE_SCHEMA: &str = "lightcraft.personal-auto-features.v1";
const LEGACY_BASELINE_RECEIPT_SCHEMA: &str = "lightcraft.personal-auto.baseline-receipt.v1";
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SHOOTS: usize = 100_000;
const MAX_PHOTOS: usize = 1_000_000;
const MAX_FEATURES: usize = 64;
const MAX_CONTROLS: usize = 32;
const MAX_LABEL_RECEIPTS: usize = 100_000;
const STYLE_IDS: [&str; 3] = ["light.contrast", "color.vibrance", "color.saturation"];

#[derive(Clone, Debug)]
struct Manifest {
    features: Vec<String>,
    controls: Vec<String>,
    bounds: BTreeMap<String, (f64, f64)>,
    shoots: Vec<Shoot>,
    baseline_receipts: ReceiptMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiptMode {
    ManualNumeric,
    DecodedProxy,
}

#[derive(Clone, Debug)]
struct Shoot {
    id: String,
    split: String,
    camera: String,
    photos: Vec<Photo>,
}

#[derive(Clone, Debug)]
struct Photo {
    id: String,
    features: Vec<f64>,
    baseline: BTreeMap<String, f64>,
    weak: Option<Target>,
    ember: Option<Target>,
    baseline_receipt: Option<BaselineReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BaselineReceipt {
    settings_version: u64,
    source_pixels_digest: String,
    settings_digest: String,
    features_digest: String,
    baseline_digest: String,
    source_digest: String,
    render_cache_version: u64,
    camera_profile_cache_key: u64,
}

#[derive(Clone, Debug)]
struct Target {
    values: BTreeMap<String, f64>,
    confidence: BTreeMap<String, f64>,
    deltas: bool,
    provenance_kind: String,
    provenance_digest: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    Weak,
    Ember,
}

impl Variant {
    fn key(self) -> &'static str {
        match self {
            Self::Weak => "weakLabelColdStart",
            Self::Ember => "emberGroundTruth",
        }
    }

    fn target(self, photo: &Photo) -> Option<&Target> {
        match self {
            Self::Weak => photo.weak.as_ref(),
            Self::Ember => photo.ember.as_ref(),
        }
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (mode, offset) = if args.first().map(String::as_str) == Some("personal") {
        (args.get(1).map(String::as_str), 2)
    } else {
        (args.first().map(String::as_str), 1)
    };
    match mode {
        Some("train") => train_command(&args[offset..]),
        Some("evaluate") => evaluate_command(&args[offset..]),
        Some("--help") | Some("-h") => {
            println!("ai personal train --manifest FILE --out NEW_MODEL");
            println!("ai personal evaluate --manifest FILE --model FILE [--variant weakLabelColdStart|emberGroundTruth|both] [--out NEW_REPORT]");
            Ok(())
        }
        _ => Err("ai personal expects train or evaluate; see --help".into()),
    }
}

fn train_command(args: &[String]) -> Result<(), String> {
    let (manifest_path, output_path) = parse_train_args(args)?;
    let (value, digest) = read_json(Path::new(&manifest_path), "manifest")?;
    let manifest = parse_manifest(&value)?;
    let model = train_model(&manifest, &digest)?;
    write_new_json(Path::new(&output_path), &model, "model")?;
    println!("{}", json!({"model": output_path, "schema": MODEL_SCHEMA, "version": VERSION}));
    Ok(())
}

fn evaluate_command(args: &[String]) -> Result<(), String> {
    let (manifest_path, model_path, variant, output_path) = parse_evaluate_args(args)?;
    let (manifest_value, digest) = read_json(Path::new(&manifest_path), "manifest")?;
    let (model, _) = read_json(Path::new(&model_path), "model")?;
    let manifest = parse_manifest(&manifest_value)?;
    let report = evaluate_model(&manifest, &model, &digest, variant)?;
    if let Some(path) = output_path {
        write_new_json(Path::new(&path), &report, "report")?;
        println!("{}", json!({"report": path, "schema": REPORT_SCHEMA, "version": VERSION}));
    } else {
        let bytes = serde_json::to_vec_pretty(&report).map_err(|_| "could not encode evaluation report")?;
        println!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(())
}

fn parse_train_args(args: &[String]) -> Result<(String, String), String> {
    let mut manifest = None;
    let mut out = None;
    let mut i = 0usize;
    while i < args.len() {
        match args.get(i).map(String::as_str).unwrap_or("") {
            "--manifest" => manifest = Some(next_arg(args, &mut i, "--manifest")?),
            "--out" | "-o" => out = Some(next_arg(args, &mut i, "--out")?),
            "--help" | "-h" => return Err("ai personal train --manifest FILE --out NEW_MODEL".into()),
            value if value.starts_with('-') => return Err(format!("unknown personal option {value}")),
            _ => return Err("personal train accepts named options only".into()),
        }
        i = i.saturating_add(1);
    }
    Ok((manifest.ok_or("personal train requires --manifest")?, out.ok_or("personal train requires --out NEW_MODEL")?))
}

fn parse_evaluate_args(args: &[String]) -> Result<(String, String, Option<Variant>, Option<String>), String> {
    let mut manifest = None;
    let mut model = None;
    let mut variant = None;
    let mut out = None;
    let mut i = 0usize;
    while i < args.len() {
        match args.get(i).map(String::as_str).unwrap_or("") {
            "--manifest" => manifest = Some(next_arg(args, &mut i, "--manifest")?),
            "--model" => model = Some(next_arg(args, &mut i, "--model")?),
            "--variant" => {
                let value = next_arg(args, &mut i, "--variant")?;
                variant = match value.as_str() {
                    "weakLabelColdStart" | "weaklabelcoldstart" | "weak" => Some(Variant::Weak),
                    "emberGroundTruth" | "embergroundtruth" | "ember" => Some(Variant::Ember),
                    "both" => None,
                    _ => return Err("--variant expects weakLabelColdStart, emberGroundTruth or both".into()),
                };
            }
            "--out" | "-o" => out = Some(next_arg(args, &mut i, "--out")?),
            "--help" | "-h" => return Err("ai personal evaluate --manifest FILE --model FILE [--variant VARIANT] [--out NEW_REPORT]".into()),
            value if value.starts_with('-') => return Err(format!("unknown personal option {value}")),
            _ => return Err("personal evaluate accepts named options only".into()),
        }
        i = i.saturating_add(1);
    }
    Ok((manifest.ok_or("personal evaluate requires --manifest")?, model.ok_or("personal evaluate requires --model")?, variant, out))
}

fn next_arg(args: &[String], index: &mut usize, option: &str) -> Result<String, String> {
    *index = index.saturating_add(1);
    args.get(*index).filter(|value| !value.starts_with('-')).cloned().ok_or_else(|| format!("{option}: missing value"))
}

fn read_json(path: &Path, kind: &str) -> Result<(Value, String), String> {
    let file = File::open(path).map_err(|_| format!("could not read {kind} input"))?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| format!("could not read {kind} input"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{kind} input exceeds {MAX_BYTES} byte cap"));
    }
    let digest = lightcraft_photo_ai::digest(&bytes);
    let value = serde_json::from_slice(&bytes).map_err(|_| format!("{kind} input is not valid JSON"))?;
    Ok((value, digest))
}

fn write_new_json(path: &Path, value: &Value, kind: &str) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| format!("could not encode {kind}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{kind} output exceeds {MAX_BYTES} byte cap"));
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path).map_err(|_| format!("could not create new {kind} output"))?;
    file.write_all(&bytes).map_err(|_| format!("could not write {kind} output"))?;
    Ok(())
}

fn parse_manifest(value: &Value) -> Result<Manifest, String> {
    let object = value.as_object().ok_or("personal manifest root must be an object")?;
    reject_unknown_fields(
        object,
        &[
            "version",
            "feature_schema",
            "featureNames",
            "feature_names",
            "controls",
            "eligible_controls",
            "control_bounds",
            "shoots",
            "baselineReceiptSchema",
            "provenance",
        ],
        "personal manifest",
    )?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("personal manifest requires version 1".into());
    }
    let receipt_schema_declared = object.get("baselineReceiptSchema").is_some();
    if let Some(schema) = object.get("baselineReceiptSchema") {
        if schema.as_str() == Some(LEGACY_BASELINE_RECEIPT_SCHEMA) {
            return Err("personal manifest declares legacy v1 baseline receipts; re-extract manifest".into());
        }
        if schema.as_str() != Some(BASELINE_RECEIPT_SCHEMA) {
            return Err("personal manifest baseline receipt schema is unsupported; re-extract manifest".into());
        }
    }
    let features = string_array(object, &["feature_schema", "featureNames", "feature_names"])?;
    let canonical_features = FEATURE_NAMES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
    if features.len() == 16 {
        return Err(format!(
            "personal manifest uses legacy 16-feature schema ({LEGACY_FEATURE_SCHEMA}); re-extract manifest with Personal Auto feature schema v2"
        ));
    }
    if features != canonical_features || features.is_empty() || features.len() > MAX_FEATURES || !unique_strings(&features) {
        return Err("personal manifest has invalid feature schema".into());
    }
    let controls = string_array(object, &["controls", "eligible_controls"])?;
    let canonical_controls = STYLE_IDS.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
    if controls != canonical_controls || controls.is_empty() || controls.len() > MAX_CONTROLS || !unique_strings(&controls) {
        return Err("personal manifest has invalid control schema".into());
    }
    for control in &controls {
        if !eligible_control(control) {
            return Err("personal manifest includes excluded scene, mask or geometry control".into());
        }
    }
    let bounds = parse_bounds(object.get("control_bounds"), &controls)?;
    let raw_shoots = object.get("shoots").and_then(Value::as_array).ok_or("personal manifest requires shoots")?;
    if raw_shoots.is_empty() || raw_shoots.len() > MAX_SHOOTS {
        return Err("personal manifest shoot count is outside bounds".into());
    }
    let mut shoots = Vec::new();
    let mut shoot_ids = BTreeSet::new();
    let mut photo_ids = BTreeSet::new();
    let mut photo_count = 0usize;
    let mut receipt_count = 0usize;
    for raw in raw_shoots {
        let shoot_object = raw.as_object().ok_or("personal shoot must be an object")?;
        reject_unknown_fields(
            shoot_object,
            &["shoot_id", "shootId", "id", "split", "partition", "camera", "camera_model", "photos", "frames"],
            "personal shoot",
        )?;
        let id = text_alias(shoot_object, &["shoot_id", "shootId", "id"], "shoot_id")?.ok_or("personal shoot is missing shoot_id")?;
        valid_id(&id, "shoot_id")?;
        if !shoot_ids.insert(id.clone()) {
            return Err("duplicate shoot_id".into());
        }
        let split = text_alias(shoot_object, &["split", "partition"], "split")?.ok_or("personal shoot is missing split")?;
        if !matches!(split.as_str(), "train" | "validation" | "eval" | "test") {
            return Err("personal shoot has invalid split".into());
        }
        let camera = camera_key(value_alias(shoot_object, &["camera", "camera_model"], "camera")?)?;
        let raw_photos =
            value_alias(shoot_object, &["photos", "frames"], "photos")?.and_then(Value::as_array).ok_or("personal shoot is missing photos")?;
        if raw_photos.is_empty() {
            return Err("personal shoot has no photos".into());
        }
        let mut photos = Vec::new();
        for raw_photo in raw_photos {
            photo_count = photo_count.saturating_add(1);
            if photo_count > MAX_PHOTOS {
                return Err("personal manifest photo count exceeds cap".into());
            }
            let photo_object = raw_photo.as_object().ok_or("personal photo must be an object")?;
            reject_unknown_fields(
                photo_object,
                &[
                    "photo_id",
                    "photoId",
                    "id",
                    "features",
                    "baseline",
                    "deterministicbaseline",
                    "deterministicBaseline",
                    "weakLabelColdStart",
                    "weaklabelcoldstart",
                    "weak_label_cold_start",
                    "emberGroundTruth",
                    "embergroundtruth",
                    "ember_ground_truth",
                    "baselineReceipt",
                ],
                "personal photo",
            )?;
            let photo_id = text_alias(photo_object, &["photo_id", "photoId", "id"], "photo_id")?.ok_or("personal photo is missing photo_id")?;
            valid_id(&photo_id, "photo_id")?;
            if !photo_ids.insert(photo_id.clone()) {
                return Err("duplicate photo_id across manifest".into());
            }
            let values = photo_object.get("features").ok_or("personal photo is missing numeric features")?;
            let photo_features = parse_features(values, &features)?;
            let baseline_value = value_alias(photo_object, &["baseline", "deterministicbaseline", "deterministicBaseline"], "baseline")?
                .ok_or("personal photo is missing baseline")?;
            let baseline = baseline_map(baseline_value, &controls)?;
            let weak = parse_target(
                value_alias(photo_object, &["weakLabelColdStart", "weaklabelcoldstart", "weak_label_cold_start"], "weakLabelColdStart")?,
                &controls,
                false,
            )
            .map_err(|error| format!("{photo_id}: weakLabelColdStart {error}"))?;
            let ember = parse_target(
                value_alias(photo_object, &["emberGroundTruth", "embergroundtruth", "ember_ground_truth"], "emberGroundTruth")?,
                &controls,
                true,
            )
            .map_err(|error| format!("{photo_id}: emberGroundTruth {error}"))?;
            let baseline_receipt = parse_baseline_receipt(photo_object.get("baselineReceipt")).map_err(|error| format!("{photo_id}: {error}"))?;
            if let Some(receipt) = baseline_receipt.as_ref() {
                let features_digest = serde_json::to_vec(&photo_features)
                    .map(|bytes| lightcraft_photo_ai::digest(&bytes))
                    .map_err(|_| format!("{photo_id}: baselineReceipt featuresDigest cannot encode features"))?;
                if receipt.features_digest != features_digest {
                    return Err(format!("{photo_id}: baselineReceipt featuresDigest does not match features"));
                }
                let baseline_digest = serde_json::to_vec(&baseline)
                    .map(|bytes| lightcraft_photo_ai::digest(&bytes))
                    .map_err(|_| format!("{photo_id}: baselineReceipt baselineDigest cannot encode baseline"))?;
                if receipt.baseline_digest != baseline_digest {
                    return Err(format!("{photo_id}: baselineReceipt baselineDigest does not match baseline"));
                }
            }
            receipt_count = receipt_count.saturating_add(usize::from(baseline_receipt.is_some()));
            photos.push(Photo { id: photo_id, features: photo_features, baseline, weak, ember, baseline_receipt });
        }
        shoots.push(Shoot { id, split, camera, photos });
    }
    let photo_total = shoots.iter().map(|shoot| shoot.photos.len()).sum::<usize>();
    let baseline_receipts = match receipt_count {
        0 if !receipt_schema_declared => ReceiptMode::ManualNumeric,
        0 => return Err("personal manifest declares baseline receipts but contains none".into()),
        count if count == photo_total => ReceiptMode::DecodedProxy,
        _ => return Err("personal manifest must provide baselineReceipt for every photo or none".into()),
    };
    Ok(Manifest { features, controls, bounds, shoots, baseline_receipts })
}

fn reject_unknown_fields(object: &Map<String, Value>, allowed: &[&str], scope: &str) -> Result<(), String> {
    if let Some(field) = object.keys().find(|field| !allowed.contains(&field.as_str())) {
        return Err(format!("{scope} has unknown field {field}"));
    }
    Ok(())
}

fn parse_baseline_receipt(value: Option<&Value>) -> Result<Option<BaselineReceipt>, String> {
    let Some(value) = value else { return Ok(None) };
    let object = value.as_object().ok_or("baselineReceipt must be an object")?;
    if object.get("schema").and_then(Value::as_str) == Some(LEGACY_BASELINE_RECEIPT_SCHEMA) {
        return Err("baselineReceipt uses legacy v1 schema; re-extract manifest".into());
    }
    if object.get("schema").and_then(Value::as_str) != Some(BASELINE_RECEIPT_SCHEMA)
        || object.get("autoRevision").and_then(Value::as_str) != Some(BASELINE_AUTO_REVISION)
        || object.get("featureSchema").and_then(Value::as_str) != Some(FEATURE_SCHEMA)
        || object.get("sourceLevel").and_then(Value::as_str) != Some("thumb")
    {
        return Err("baselineReceipt renderer identity is unsupported; re-extract manifest".into());
    }
    let digest = |name: &str| -> Result<String, String> {
        let value = object.get(name).and_then(Value::as_str).ok_or_else(|| format!("baselineReceipt {name} is missing"))?;
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("baselineReceipt {name} is invalid"));
        }
        Ok(value.to_owned())
    };
    let source_pixels_digest = digest("sourcePixelsDigest")?;
    let settings_digest = digest("settingsDigest")?;
    let features_digest = digest("featuresDigest")?;
    let baseline_digest = digest("baselineDigest")?;
    let source_digest = digest("sourceDigest")?;
    let settings_version = object.get("settingsVersion").and_then(Value::as_u64).ok_or("baselineReceipt settingsVersion is missing")?;
    if settings_version != u64::from(SCHEMA_VERSION) {
        return Err("baselineReceipt settingsVersion is stale; re-extract manifest".into());
    }
    let render_cache_version = object.get("renderCacheVersion").and_then(Value::as_u64).ok_or("baselineReceipt renderCacheVersion is missing")?;
    let camera_profile_cache_key =
        object.get("cameraProfileCacheKey").and_then(Value::as_u64).ok_or("baselineReceipt cameraProfileCacheKey is missing")?;
    Ok(Some(BaselineReceipt {
        settings_version,
        source_pixels_digest,
        settings_digest,
        features_digest,
        baseline_digest,
        source_digest,
        render_cache_version,
        camera_profile_cache_key,
    }))
}

fn string_array(object: &Map<String, Value>, keys: &[&str]) -> Result<Vec<String>, String> {
    let mut found = None;
    for key in keys {
        let Some(value) = object.get(*key) else { continue };
        let values = value.as_array().ok_or("personal schema must be an array")?;
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            let text = value.as_str().ok_or("personal schema names must be strings")?.trim();
            if text.is_empty() || text.len() > 128 || text.chars().any(char::is_control) {
                return Err("personal schema name is invalid".into());
            }
            result.push(text.to_owned());
        }
        if found.as_ref().is_some_and(|previous: &Vec<String>| previous != &result) {
            return Err("personal schema aliases conflict".into());
        }
        found = Some(result);
    }
    found.ok_or_else(|| "personal manifest is missing schema array".into())
}

fn unique_strings(values: &[String]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn parse_bounds(value: Option<&Value>, controls: &[String]) -> Result<BTreeMap<String, (f64, f64)>, String> {
    let mut bounds = BTreeMap::new();
    let object = value.and_then(Value::as_object);
    for control in controls {
        let pair = object
            .and_then(|map| map.get(control))
            .map(|value| {
                if let Some(array) = value.as_array() {
                    let low = array.first().and_then(Value::as_f64).ok_or("control bound is invalid")?;
                    let high = array.get(1).and_then(Value::as_f64).ok_or("control bound is invalid")?;
                    Ok((low, high))
                } else if let Some(map) = value.as_object() {
                    let low = map.get("min").and_then(Value::as_f64).ok_or("control bound is invalid")?;
                    let high = map.get("max").and_then(Value::as_f64).ok_or("control bound is invalid")?;
                    Ok((low, high))
                } else {
                    Err("control bound is invalid")
                }
            })
            .transpose()?
            .unwrap_or((-100.0, 100.0));
        if !pair.0.is_finite() || !pair.1.is_finite() || pair.0 >= pair.1 {
            return Err("control bound is invalid".into());
        }
        bounds.insert(control.clone(), pair);
    }
    Ok(bounds)
}

fn parse_features(value: &Value, names: &[String]) -> Result<Vec<f64>, String> {
    let mut result = Vec::with_capacity(names.len());
    if let Some(array) = value.as_array() {
        if array.len() != names.len() {
            return Err("personal feature vector has wrong length".into());
        }
        for value in array {
            result.push(finite_number(value, "feature")?);
        }
    } else {
        let object = value.as_object().ok_or("personal features must be numeric object or array")?;
        for name in names {
            result.push(finite_number(object.get(name).ok_or("personal feature is missing")?, "feature")?);
        }
        if object.keys().any(|name| !names.contains(name)) {
            return Err("personal features contain unknown name".into());
        }
    }
    Ok(result)
}

fn numeric_map(value: &Value, controls: &[String], kind: &str) -> Result<BTreeMap<String, f64>, String> {
    let object = value.as_object().ok_or_else(|| format!("{kind} must be a numeric object"))?;
    let mut result = BTreeMap::new();
    for control in controls {
        let number = finite_number(object.get(control).ok_or_else(|| format!("{kind} is missing control"))?, kind)?;
        result.insert(control.clone(), number);
    }
    if object.keys().any(|name| !controls.contains(name)) {
        return Err(format!("{kind} contains unknown control"));
    }
    Ok(result)
}

fn baseline_map(value: &Value, controls: &[String]) -> Result<BTreeMap<String, f64>, String> {
    let object = value.as_object().ok_or("baseline must be a numeric object")?;
    if object.contains_key("values") && object.contains_key("settings") {
        return Err("baseline values/settings aliases conflict".into());
    }
    if let Some(values) = object.get("values").or_else(|| object.get("settings")) {
        return numeric_map(values, controls, "baseline");
    }
    if object.contains_key("provenance") {
        let mut values = object.clone();
        values.remove("provenance");
        return numeric_map(&Value::Object(values), controls, "baseline");
    }
    numeric_map(value, controls, "baseline")
}

fn parse_target(value: Option<&Value>, controls: &[String], ember: bool) -> Result<Option<Target>, String> {
    let Some(value) = value else { return Ok(None) };
    let object = value.as_object().ok_or("personal label must be an object")?;
    let provenance = object.get("provenance").ok_or("personal label requires provenance")?;
    let (provenance_kind, provenance_digest) = parse_provenance(provenance, ember)?;
    let fields = ["deltas", "values", "settings"];
    let supplied = fields.iter().filter(|field| object.contains_key(**field)).copied().collect::<Vec<_>>();
    if supplied.len() > 1 {
        return Err("personal label values/settings/deltas aliases conflict".into());
    }
    let field = supplied.first().copied().ok_or("personal label requires values or deltas")?;
    let deltas = field == "deltas";
    let values_value = object.get(field).ok_or("personal label requires values or deltas")?;
    let values = partial_numeric_map(values_value, controls)?;
    let confidence = partial_confidence(object.get("confidence").ok_or("personal label requires confidence for every target field")?, controls)?;
    if values.keys().any(|control| !confidence.contains_key(control)) {
        return Err("personal label requires confidence for every target field".into());
    }
    Ok(Some(Target { values, confidence, deltas, provenance_kind, provenance_digest }))
}

/// Canonicalizes labels while retaining provenance as an opaque source receipt. The emitted
/// provenanceSha256 is not an authenticity proof because canonical form strips original metadata.
pub(crate) fn canonical_input_label(value: &Value, ember: bool) -> Result<Value, String> {
    let controls = STYLE_IDS.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
    let target = parse_target(Some(value), &controls, ember)?.ok_or("personal label is missing")?;
    let deltas = target.deltas;
    let provenance_kind = target.provenance_kind;
    let provenance_digest = target.provenance_digest;
    let values = target.values.into_iter().map(|(key, value)| (key, json!(value))).collect::<Map<_, _>>();
    let confidence = target.confidence.into_iter().map(|(key, value)| (key, json!(value))).collect::<Map<_, _>>();
    let mut provenance = Map::new();
    provenance.insert("kind".into(), Value::String(provenance_kind));
    provenance.insert("provenanceSha256".into(), Value::String(provenance_digest));
    if ember {
        provenance.insert("accepted".into(), Value::Bool(true));
    }
    let mut canonical = Map::new();
    canonical.insert(if deltas { "deltas" } else { "values" }.into(), Value::Object(values));
    canonical.insert("confidence".into(), Value::Object(confidence));
    canonical.insert("provenance".into(), Value::Object(provenance));
    Ok(Value::Object(canonical))
}

fn partial_numeric_map(value: &Value, controls: &[String]) -> Result<BTreeMap<String, f64>, String> {
    let object = value.as_object().ok_or("personal label values must be a numeric object")?;
    let mut result = BTreeMap::new();
    for (key, value) in object {
        if !controls.contains(key) {
            return Err("personal label contains unknown control".into());
        }
        let number = finite_number(value, "target")?;
        result.insert(key.clone(), number);
    }
    if result.is_empty() { Err("personal label values are empty".into()) } else { Ok(result) }
}

fn partial_confidence(value: &Value, controls: &[String]) -> Result<BTreeMap<String, f64>, String> {
    let object = value.as_object().ok_or("personal confidence must be a numeric object")?;
    let mut result = BTreeMap::new();
    for (key, value) in object {
        if !controls.contains(key) {
            return Err("personal confidence contains unknown control".into());
        }
        let number = finite_number(value, "confidence")?;
        if !(0.0..=1.0).contains(&number) || number == 0.0 {
            return Err("personal confidence must be between 0 and 1".into());
        }
        result.insert(key.clone(), number);
    }
    Ok(result)
}

fn parse_provenance(value: &Value, ember: bool) -> Result<(String, String), String> {
    let object = value.as_object().ok_or("personal label provenance must be an object")?;
    let kind = canonical_provenance_kind(value, ember).ok_or_else(|| {
        if ember {
            "emberGroundTruth requires explicit accepted human-edit provenance".to_owned()
        } else {
            "weakLabelColdStart requires explicit mapped-source provenance".to_owned()
        }
    })?;
    let supplied_digest = match object.get("provenanceSha256") {
        None => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(_) => return Err("personal label provenance digest must be a string".into()),
    };
    let digest = provenance_digest(value)?;
    if let Some(supplied) = supplied_digest {
        if !valid_digest(supplied) {
            return Err("personal label provenance digest is invalid".into());
        }
        let canonical_only = object.keys().all(|key| matches!(key.as_str(), "kind" | "accepted" | "provenanceSha256"));
        // Canonical output intentionally retains only an opaque source receipt digest; original
        // provenance metadata is unavailable there, so canonical-only digests are not proofs.
        if !canonical_only && supplied != digest {
            return Err("personal label provenance digest does not match source".into());
        }
    }
    let digest = supplied_digest.map(str::to_owned).unwrap_or(digest);
    if object.len() > 16 || digest.is_empty() {
        return Err("personal label provenance exceeds bounds".into());
    }
    Ok((kind, digest))
}

fn canonical_provenance_kind(value: &Value, ember: bool) -> Option<String> {
    let object = value.as_object()?;
    if ember && object.get("accepted").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let keys: &[&str] = if ember { &["kind", "origin", "source"] } else { &["kind", "source", "origin", "labelSource", "label_source"] };
    let mut found = None;
    for key in keys {
        let Some(raw) = object.get(*key) else { continue };
        let kind = raw.as_str()?;
        let allowed = if ember {
            matches!(kind, "human-edit" | "synthetic-human-edit")
        } else {
            matches!(kind, "lightroom" | "lightroom-mapped-settings" | "mapped-lightroom-settings" | "synthetic-weak")
        };
        if !allowed || found.as_deref().is_some_and(|previous| previous != kind) {
            return None;
        }
        found = Some(kind.to_owned());
    }
    found
}

fn provenance_digest(value: &Value) -> Result<String, String> {
    let mut object = value.as_object().cloned().ok_or("personal label provenance must be an object")?;
    object.remove("provenanceSha256");
    let bytes = serde_json::to_vec(&Value::Object(object)).map_err(|_| "personal label provenance is not encodable".to_owned())?;
    Ok(lightcraft_photo_ai::digest(&bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn finite_number(value: &Value, field: &str) -> Result<f64, String> {
    let number = value.as_f64().ok_or_else(|| format!("{field} must be numeric"))?;
    if !number.is_finite() {
        return Err(format!("{field} must be finite"));
    }
    Ok(number)
}

fn eligible_control(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let excluded = [
        "mask",
        "spot",
        "crop",
        "geometry",
        "lens",
        "calibration",
        "profile",
        "process",
        "exposure",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "temperature",
        "tint",
        "whitebalance",
        "wb",
    ];
    if excluded.iter().any(|word| lower.contains(word)) {
        return false;
    }
    ["contrast", "vibrance", "saturation", "texture", "clarity", "dehaze"].iter().any(|word| lower.ends_with(word))
}

fn valid_id(id: &str, field: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 || id.chars().any(|character| character.is_control() || matches!(character, '/' | '\\')) {
        return Err(format!("invalid {field}"));
    }
    Ok(())
}

fn text_alias(object: &Map<String, Value>, keys: &[&str], field: &str) -> Result<Option<String>, String> {
    let mut found = None;
    for key in keys {
        let Some(value) = object.get(*key) else { continue };
        let value = value.as_str().ok_or_else(|| format!("{field} must be a string"))?.trim();
        if value.is_empty() {
            return Err(format!("{field} must not be empty"));
        }
        if found.as_deref().is_some_and(|previous| previous != value) {
            return Err(format!("{field} aliases conflict"));
        }
        found = Some(value.to_owned());
    }
    Ok(found)
}

fn value_alias<'a>(object: &'a Map<String, Value>, keys: &[&str], field: &str) -> Result<Option<&'a Value>, String> {
    let mut found = None;
    for key in keys {
        let Some(value) = object.get(*key) else { continue };
        if found.is_some_and(|previous| previous != value) {
            return Err(format!("{field} aliases conflict"));
        }
        found = Some(value);
    }
    Ok(found)
}

fn camera_key(value: Option<&Value>) -> Result<String, String> {
    match value {
        Some(Value::String(value)) => {
            let value = value.trim();
            valid_id(value, "camera")?;
            Ok(value.to_owned())
        }
        Some(Value::Object(object)) => {
            let make = text_alias(object, &["make", "manufacturer"], "camera make")?.unwrap_or_else(|| "unknown".into());
            let model = text_alias(object, &["model", "camera_model"], "camera model")?.unwrap_or_else(|| "unknown".into());
            valid_id(&make, "camera make")?;
            valid_id(&model, "camera model")?;
            Ok(format!("{make}/{model}"))
        }
        None | Some(Value::Null) => Ok("unknown".into()),
        Some(_) => Err("personal shoot camera must be a string or object".into()),
    }
}

fn core_split_manifest(manifest: &Manifest, identity: &str) -> SplitManifest {
    let shoots = manifest
        .shoots
        .iter()
        .map(|shoot| ShootAssignment {
            shoot_id: shoot.id.clone(),
            split: match shoot.split.as_str() {
                "train" => Split::Train,
                "validation" => Split::Development,
                "eval" | "test" => Split::HeldOut,
                _ => Split::HeldOut,
            },
        })
        .collect();
    SplitManifest::new(identity, shoots)
}

fn core_sample(shoot: &Shoot, photo: &Photo, variant: Variant) -> Result<Option<lightcraft_pipeline::personal_auto::TrainingSample>, String> {
    let Some(target) = variant.target(photo) else { return Ok(None) };
    let values: [f64; FEATURE_COUNT] = photo.features.clone().try_into().map_err(|_| format!("{}: feature vector has wrong length", photo.id))?;
    let features = FeatureVector::new(values).map_err(|_| format!("{}: feature vector is non-finite", photo.id))?;
    let mut labels = StyleLabels::default();
    for (index, control) in STYLE_IDS.iter().enumerate() {
        let Some(value) = target.values.get(*control) else { continue };
        let confidence = target.confidence.get(*control).copied().unwrap_or(1.0);
        if confidence <= 0.0 {
            continue;
        }
        let delta = if target.deltas { *value } else { *value - photo.baseline[*control] };
        let source = match variant {
            Variant::Weak => LabelSource::WeakLightroom,
            Variant::Ember => LabelSource::EmberValidated,
        };
        let label =
            FieldLabel::new(delta, source, confidence).map_err(|_| format!("{}: label residual or confidence is outside bounds", photo.id))?;
        labels.set(StyleControl::ALL[index], Some(label));
    }
    Ok(Some(lightcraft_pipeline::personal_auto::TrainingSample {
        features,
        labels,
        provenance: lightcraft_pipeline::personal_auto::SampleProvenance {
            sample_id: photo.id.clone(),
            shoot_id: shoot.id.clone(),
            split: Split::Train,
            source_ref: format!("label:{}:{}", target.provenance_kind, target.provenance_digest),
        },
    }))
}

fn train_variant(manifest: &Manifest, variant: Variant, manifest_digest: &str) -> Result<(Option<Value>, Option<Value>), String> {
    let split_manifest = core_split_manifest(manifest, manifest_digest);
    let mut samples = Vec::new();
    for shoot in manifest.shoots.iter().filter(|shoot| shoot.split == "train") {
        for photo in &shoot.photos {
            if let Some(sample) = core_sample(shoot, photo, variant)? {
                samples.push(sample);
            }
        }
    }
    if samples.is_empty() {
        return Ok((None, None));
    }
    let model = PersonalAutoModel::fit_train_only(&samples, &split_manifest).map_err(|error| format!("{}: {error}", variant.key()))?;
    let eligibility =
        serde_json::to_value(model.training_data_eligibility()).map_err(|_| format!("{}: could not encode eligibility", variant.key()))?;
    let model = serde_json::to_value(model).map_err(|_| format!("{}: could not encode pipeline model", variant.key()))?;
    Ok((Some(model), Some(eligibility)))
}

fn train_model(manifest: &Manifest, manifest_digest: &str) -> Result<Value, String> {
    validate_baseline_receipts(manifest)?;
    let train_photos: Vec<&Photo> = manifest.shoots.iter().filter(|shoot| shoot.split == "train").flat_map(|shoot| shoot.photos.iter()).collect();
    let mut errors = Map::new();
    let (weak, weak_eligibility) = match train_variant(manifest, Variant::Weak, manifest_digest) {
        Ok(result) => result,
        Err(error) => {
            errors.insert(Variant::Weak.key().to_owned(), Value::String(error));
            (None, None)
        }
    };
    let (ember, ember_eligibility) = match train_variant(manifest, Variant::Ember, manifest_digest) {
        Ok(result) => result,
        Err(error) => {
            errors.insert(Variant::Ember.key().to_owned(), Value::String(error));
            (None, None)
        }
    };
    let status = if weak.is_some() || ember.is_some() { "experimentalTrainedUnqualified" } else { "harness-only" };
    let mut variants = Map::new();
    let no_train_variants = weak.is_none() && ember.is_none();
    variants.insert(Variant::Weak.key().to_owned(), weak.unwrap_or(Value::Null));
    variants.insert(Variant::Ember.key().to_owned(), ember.unwrap_or(Value::Null));
    let mut eligibility = Map::new();
    if let Some(value) = weak_eligibility {
        eligibility.insert(Variant::Weak.key().to_owned(), value);
    }
    if let Some(value) = ember_eligibility {
        eligibility.insert(Variant::Ember.key().to_owned(), value);
    }
    let mut labelled = Map::new();
    let mut control_eligible = Map::new();
    let mut excluded = Map::new();
    for variant in [Variant::Weak, Variant::Ember] {
        let sample_count = eligibility.get(variant.key()).and_then(|value| value.get("sampleCount")).and_then(Value::as_u64).unwrap_or(0) as usize;
        let field_counts = eligibility.get(variant.key()).and_then(|value| value.get("fieldCounts")).cloned().unwrap_or_else(|| json!([]));
        labelled.insert(variant.key().to_owned(), json!(sample_count));
        control_eligible.insert(variant.key().to_owned(), field_counts);
        excluded.insert(variant.key().to_owned(), json!(train_photos.len().saturating_sub(sample_count)));
    }
    let mut receipts = Map::new();
    for variant in [Variant::Weak, Variant::Ember] {
        receipts.insert(variant.key().to_owned(), Value::Array(training_receipts(manifest, variant)?));
    }
    Ok(json!({
        "version": VERSION,
        "schema": MODEL_SCHEMA,
        "coreModelSchema": CORE_MODEL_SCHEMA,
        "status": status,
        "trainingSplit": "train",
        "featureSchema": manifest.features,
        "controls": manifest.controls,
        "controlBounds": manifest.bounds,
        "variants": variants,
        "trainingErrors": errors,
        "trainingEligibility": eligibility,
        "trainingLabelReceipts": receipts,
        "baselineContract": baseline_contract(manifest),
        "harnessOnlyReason": if no_train_variants { json!("no variant had eligible train labels") } else { Value::Null },
        "qualificationStatus": "unqualified_no_promotion",
        "training": {"shootCount": manifest.shoots.iter().filter(|shoot| shoot.split == "train").count(), "inputPhotoCount": train_photos.len(), "labelledPhotoCount": labelled, "controlEligible": control_eligible, "excludedPhotoCount": excluded, "manifestSha256": manifest_digest},
        "provenance": {"baseline": "deterministicbaseline supplied in explicit manifest", "weakLabelColdStart": "per-photo provenance supplied in manifest", "emberGroundTruth": "accepted-human-edit provenance supplied in manifest", "provenanceCompleteness": "numericHarnessOnly"},
        "policy": {"automaticPromotion": false, "automaticApply": false, "catalogRead": false, "network": false, "render": false, "featuresMetadataFree": true, "excluded": ["masks", "sceneControls", "geometry", "cameraProfile", "calibration", "lensCorrection"]}
    }))
}

fn baseline_contract(manifest: &Manifest) -> Value {
    json!({
        "receiptSchema": BASELINE_RECEIPT_SCHEMA,
        "autoRevision": BASELINE_AUTO_REVISION,
        "featureSchema": FEATURE_SCHEMA,
        "settingsSchemaVersion": SCHEMA_VERSION,
        "renderCacheVersion": RENDER_CACHE_VERSION,
        "cameraProfileCacheKey": camera_profiles::cache_key(),
        "mode": match manifest.baseline_receipts {
            ReceiptMode::DecodedProxy => "decodedProxy",
            ReceiptMode::ManualNumeric => "manualNumeric",
        },
    })
}

fn validate_baseline_receipts(manifest: &Manifest) -> Result<(), String> {
    if manifest.baseline_receipts == ReceiptMode::ManualNumeric {
        return Ok(());
    }
    let current_cache_key = camera_profiles::cache_key();
    for shoot in &manifest.shoots {
        for photo in &shoot.photos {
            let receipt = photo.baseline_receipt.as_ref().ok_or("decoded-proxy manifest is missing baseline receipt")?;
            if receipt.settings_version != u64::from(SCHEMA_VERSION)
                || receipt.source_pixels_digest.is_empty()
                || receipt.settings_digest.is_empty()
                || receipt.features_digest.is_empty()
                || receipt.baseline_digest.is_empty()
                || receipt.source_digest.is_empty()
            {
                return Err("baseline receipt settings or source digest is stale; re-extract manifest".into());
            }
            if receipt.render_cache_version != RENDER_CACHE_VERSION {
                return Err("baseline receipt render cache version is stale; re-extract manifest".into());
            }
            if receipt.camera_profile_cache_key != current_cache_key {
                return Err("baseline receipt camera profile cache key is stale; re-extract manifest".into());
            }
        }
    }
    Ok(())
}

fn training_receipts(manifest: &Manifest, variant: Variant) -> Result<Vec<Value>, String> {
    let mut receipts = Vec::new();
    for shoot in manifest.shoots.iter().filter(|shoot| shoot.split == "train") {
        for photo in &shoot.photos {
            let Some(target) = variant.target(photo) else { continue };
            receipts.push(json!({
                "photoId": photo.id,
                "shootId": shoot.id,
                "kind": target.provenance_kind,
                "provenanceSha256": target.provenance_digest
            }));
            if receipts.len() > MAX_LABEL_RECEIPTS {
                return Err(format!("{} training label receipts exceed cap", variant.key()));
            }
        }
    }
    Ok(receipts)
}

fn evaluate_model(manifest: &Manifest, model: &Value, manifest_digest: &str, requested: Option<Variant>) -> Result<Value, String> {
    validate_model(model, manifest, manifest_digest)?;
    let variants = requested.into_iter().collect::<Vec<_>>();
    let variants = if variants.is_empty() { vec![Variant::Weak, Variant::Ember] } else { variants };
    let mut reports = Map::new();
    for variant in variants {
        reports.insert(variant.key().to_owned(), evaluate_variant(manifest, model, variant));
    }
    let numeric = reports.values().any(|report| report["status"] == "numericEvaluatedUnqualified");
    Ok(json!({
        "version": VERSION,
        "schema": REPORT_SCHEMA,
        "status": if numeric { "numericEvaluatedUnqualified" } else { "harness-only" },
        "manifestSha256": manifest_digest,
        "excludedSplits": ["train"],
        "variants": reports,
        "modelStatus": model.get("status").cloned().unwrap_or(Value::String("unknown".into())),
        "trainingErrors": model.get("trainingErrors").cloned().unwrap_or_else(|| json!({})),
        "trainingEligibility": model.get("trainingEligibility").cloned().unwrap_or_else(|| json!({})),
        "dataCoverage": model.get("training").cloned().unwrap_or_else(|| json!({})),
        "harnessOnlyReason": model.get("harnessOnlyReason").cloned().unwrap_or(Value::Null),
        "qualificationStatus": "unqualified_no_promotion",
        "provenance": {"baseline": "deterministicbaseline supplied in explicit manifest", "weakLabelColdStart": "mapped settings are style-transfer evidence only", "emberGroundTruth": "accepted-human-edit targets require per-photo provenance", "provenanceCompleteness": "numericHarnessOnly"},
        "claims": {"visualPreference": "unavailable_without_supplied_blinded_votes_and_render_receipts", "personalPreference": "unclaimed", "weakLabelStatus": "style-transfer-only"},
        "policy": {"automaticPromotion": false, "automaticApply": false, "catalogRead": false, "network": false, "render": false, "masksExcluded": true, "sceneControlsExcluded": true}
    }))
}

fn validate_model(model: &Value, manifest: &Manifest, manifest_digest: &str) -> Result<(), String> {
    let object = model.as_object().ok_or("personal model root must be an object")?;
    if object.get("schema").and_then(Value::as_str) == Some(LEGACY_MODEL_SCHEMA)
        || object.get("coreModelSchema").and_then(Value::as_str) == Some(LEGACY_CORE_MODEL_SCHEMA)
        || object.get("featureSchema").and_then(Value::as_array).is_some_and(|features| features.len() == 16)
    {
        return Err("personal model uses legacy v1/16-feature schema; retrain model".into());
    }
    if object.get("version").and_then(Value::as_u64) != Some(VERSION) || object.get("schema").and_then(Value::as_str) != Some(MODEL_SCHEMA) {
        return Err("personal model schema is unsupported; retrain model".into());
    }
    if object.get("coreModelSchema").and_then(Value::as_str) != Some(CORE_MODEL_SCHEMA)
        || object.get("featureSchema") != Some(&json!(manifest.features))
        || object.get("controls") != Some(&json!(manifest.controls))
    {
        return Err("personal model schema does not match manifest or pipeline core; retrain model".into());
    }
    if object.get("training").and_then(Value::as_object).and_then(|training| training.get("manifestSha256")).and_then(Value::as_str)
        != Some(manifest_digest)
    {
        return Err("personal model split manifest does not match evaluation manifest".into());
    }
    validate_baseline_receipts(manifest)?;
    if object.get("baselineContract") != Some(&baseline_contract(manifest)) {
        return Err("personal model baseline renderer contract does not match manifest; retrain model".into());
    }
    let variants = object.get("variants").and_then(Value::as_object).ok_or("personal model is missing variants")?;
    let receipts = object.get("trainingLabelReceipts").and_then(Value::as_object).ok_or("personal model is missing training label receipts")?;
    for variant in [Variant::Weak, Variant::Ember] {
        let expected = Value::Array(training_receipts(manifest, variant)?);
        if receipts.get(variant.key()) != Some(&expected) {
            return Err(format!("{} training label receipts do not match manifest", variant.key()));
        }
    }
    let split_manifest = core_split_manifest(manifest, manifest_digest);
    for variant in [Variant::Weak, Variant::Ember] {
        let Some(value) = variants.get(variant.key()) else { return Err("personal model is missing variant".into()) };
        if !value.is_null() {
            let core: PersonalAutoModel = serde_json::from_value(value.clone()).map_err(|_| "personal model variant is malformed".to_owned())?;
            core.validate_against(&split_manifest).map_err(|_| "personal model variant split manifest is incompatible".to_owned())?;
        }
    }
    Ok(())
}

fn evaluate_variant(manifest: &Manifest, model: &Value, variant: Variant) -> Value {
    let core_model = model
        .get("variants")
        .and_then(|value| value.get(variant.key()))
        .filter(|value| value.is_object())
        .and_then(|value| serde_json::from_value::<PersonalAutoModel>(value.clone()).ok())
        .filter(|value| value.validate().is_ok());
    let mut split_reports = Map::new();
    for split in ["validation", "eval", "test"] {
        split_reports.insert(split.to_owned(), evaluate_split(manifest, core_model.as_ref(), variant, split));
    }
    let status = if core_model.is_some() && split_reports.values().any(|report| report["coverage"]["modelScored"].as_u64().unwrap_or(0) > 0) {
        "numericEvaluatedUnqualified"
    } else {
        "harness-only"
    };
    json!({"status": status, "coreModelSchema": CORE_MODEL_SCHEMA, "qualificationStatus": "unqualified_no_promotion", "splits": split_reports})
}

fn evaluate_split(manifest: &Manifest, model: Option<&PersonalAutoModel>, variant: Variant, split: &str) -> Value {
    let mut pairs = Vec::new();
    let mut per_photo: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    let mut per_shoot: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    let mut per_camera: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    let mut input = 0usize;
    let mut eligible = 0usize;
    let mut skipped = 0usize;
    let mut fallback = 0usize;
    let mut model_scored = 0usize;
    let mut fallback_scored = 0usize;
    let mut skipped_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut fallback_reasons: BTreeMap<String, usize> = BTreeMap::new();
    for shoot in manifest.shoots.iter().filter(|shoot| shoot.split == split) {
        for photo in &shoot.photos {
            input = input.saturating_add(1);
            let Some(target) = variant.target(photo) else {
                skipped = skipped.saturating_add(1);
                *skipped_reasons.entry("missingLabel".into()).or_default() += 1;
                continue;
            };
            eligible = eligible.saturating_add(1);
            let Some(baseline_distance) = distance(&photo.baseline, &photo.baseline, target, &manifest.controls, &manifest.bounds) else {
                skipped = skipped.saturating_add(1);
                *skipped_reasons.entry("invalidTarget".into()).or_default() += 1;
                continue;
            };
            let Some(model) = model else {
                skipped = skipped.saturating_add(1);
                *skipped_reasons.entry("missingModel".into()).or_default() += 1;
                continue;
            };
            let (predicted, used_model) = match predict(model, photo, manifest) {
                PredictionValues::Model(values) => (values, true),
                PredictionValues::Fallback { values, reason } => {
                    fallback = fallback.saturating_add(1);
                    *fallback_reasons.entry(reason.into()).or_default() += 1;
                    (values, false)
                }
            };
            let Some(regressor_distance) = distance(&predicted, &photo.baseline, target, &manifest.controls, &manifest.bounds) else {
                skipped = skipped.saturating_add(1);
                *skipped_reasons.entry("invalidPrediction".into()).or_default() += 1;
                continue;
            };
            pairs.push((baseline_distance, regressor_distance));
            if used_model {
                model_scored = model_scored.saturating_add(1);
            } else {
                fallback_scored = fallback_scored.saturating_add(1);
            }
            per_photo.insert(photo.id.clone(), vec![(baseline_distance, regressor_distance)]);
            per_shoot.entry(shoot.id.clone()).or_default().push((baseline_distance, regressor_distance));
            per_camera.entry(shoot.camera.clone()).or_default().push((baseline_distance, regressor_distance));
        }
    }
    let shoots = per_shoot.iter().map(|(id, pairs)| (id.clone(), metric(pairs))).collect::<Map<String, Value>>();
    let cameras = per_camera.iter().map(|(id, pairs)| (id.clone(), metric(pairs))).collect::<Map<String, Value>>();
    let photos = per_photo.iter().map(|(id, pairs)| (id.clone(), metric(pairs))).collect::<Map<String, Value>>();
    json!({
        "photos": metric(&pairs),
        "perPhoto": photos,
        "shoots": shoots,
        "byCamera": cameras,
        "coverage": {
            "input": input,
            "eligible": eligible,
            "scored": pairs.len(),
            "modelScored": model_scored,
            "fallbackScored": fallback_scored,
            "skipped": skipped,
            "fallback": fallback,
            "skippedReasons": skipped_reasons,
            "fallbackReasons": fallback_reasons
        }
    })
}

enum PredictionValues {
    Model(BTreeMap<String, f64>),
    Fallback { values: BTreeMap<String, f64>, reason: &'static str },
}

fn predict(model: &PersonalAutoModel, photo: &Photo, manifest: &Manifest) -> PredictionValues {
    let fallback = |reason| PredictionValues::Fallback { values: photo.baseline.clone(), reason };
    let values: [f64; FEATURE_COUNT] = match photo.features.clone().try_into() {
        Ok(values) => values,
        Err(_) => return fallback("invalidFeatures"),
    };
    let features = match FeatureVector::new(values) {
        Ok(features) => features,
        Err(_) => return fallback("invalidFeatures"),
    };
    let mut baseline = DevelopSettings::default();
    for control in STYLE_IDS {
        if !controls::set(&mut baseline, control, photo.baseline[control]) {
            return fallback("invalidBaseline");
        }
    }
    let prediction = model.predict(&baseline, &features);
    if !prediction.used_model {
        return fallback("modelFallback");
    }
    let mut result = photo.baseline.clone();
    for control in &manifest.controls {
        let Some(value) = controls::get(&prediction.settings, control) else {
            return fallback("invalidPrediction");
        };
        result.insert(control.clone(), value);
    }
    PredictionValues::Model(result)
}

fn distance(
    values: &BTreeMap<String, f64>,
    baseline: &BTreeMap<String, f64>,
    target: &Target,
    controls: &[String],
    bounds: &BTreeMap<String, (f64, f64)>,
) -> Option<f64> {
    let mut total = 0.0;
    let mut count = 0usize;
    for control in controls {
        let Some(target_value) = target.values.get(control) else { continue };
        let value = *values.get(control)?;
        let (low, high) = bounds[control];
        let expected = if target.deltas { baseline[control] + target_value } else { *target_value };
        total += (value - expected).abs() / (high - low);
        count += 1;
    }
    if count == 0 { None } else { Some(total / count as f64) }
}

fn metric(pairs: &[(f64, f64)]) -> Value {
    if pairs.is_empty() {
        return json!({"distance": "normalizedL1StyleEditDistance", "count": 0, "baselineMean": null, "baselineMedian": null, "regressorMean": null, "regressorMedian": null, "gain": null, "pairedImprovementRate": null});
    }
    let baseline: f64 = pairs.iter().map(|pair| pair.0).sum::<f64>() / pairs.len() as f64;
    let regressor: f64 = pairs.iter().map(|pair| pair.1).sum::<f64>() / pairs.len() as f64;
    let mut baseline_values = pairs.iter().map(|pair| pair.0).collect::<Vec<_>>();
    let mut regressor_values = pairs.iter().map(|pair| pair.1).collect::<Vec<_>>();
    baseline_values.sort_by(f64::total_cmp);
    regressor_values.sort_by(f64::total_cmp);
    let middle = pairs.len() / 2;
    let baseline_median =
        if pairs.len().is_multiple_of(2) { (baseline_values[middle - 1] + baseline_values[middle]) / 2.0 } else { baseline_values[middle] };
    let regressor_median =
        if pairs.len().is_multiple_of(2) { (regressor_values[middle - 1] + regressor_values[middle]) / 2.0 } else { regressor_values[middle] };
    let improvements = pairs.iter().filter(|pair| pair.1 < pair.0).count();
    let gain = if baseline > 0.0 { Some((baseline - regressor) / baseline) } else { None };
    json!({"distance": "normalizedL1StyleEditDistance", "count": pairs.len(), "baselineMean": baseline, "baselineMedian": baseline_median, "regressorMean": regressor, "regressorMedian": regressor_median, "gain": gain, "pairedImprovementRate": improvements as f64 / pairs.len() as f64})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature_values(base: [f64; 16], spatial: [f64; 12]) -> Value {
        let mut values = base.to_vec();
        values.extend(spatial);
        json!(values)
    }

    fn manifest() -> Value {
        json!({"version":1,"feature_schema":FEATURE_NAMES,"controls":["light.contrast","color.vibrance","color.saturation"],"shoots":[
            {"shoot_id":"train-1","split":"train","camera":{"make":"Ember","model":"Synthetic"},"photos":[
                {"photo_id":"train-a","features":feature_values([0.0,0.0,0.0,0.0,0.0,0.0,0.2,0.2,0.1,0.1,0.1,1.5,1.0,0.0,6500.0,0.0],[0.11,0.021,0.18,0.24,0.031,0.23,0.37,0.041,0.28,0.49,0.051,0.34]),"baseline":{"light.contrast":0.0,"color.vibrance":0.0,"color.saturation":0.0},"emberGroundTruth":{"values":{"light.contrast":10.0,"color.vibrance":2.0,"color.saturation":1.0},"confidence":{"light.contrast":1.0,"color.vibrance":1.0,"color.saturation":1.0},"provenance":{"accepted":true,"origin":"human-edit"}}},
                {"photo_id":"train-b","features":feature_values([1.0,1.0,1.0,1.0,1.0,0.0,0.4,0.4,0.1,0.1,0.1,1.5,1.0,0.0,6500.0,0.0],[0.61,0.071,0.42,0.74,0.081,0.47,0.87,0.091,0.52,0.93,0.101,0.58]),"baseline":{"light.contrast":0.0,"color.vibrance":0.0,"color.saturation":0.0},"weakLabelColdStart":{"values":{"light.contrast":8.0,"color.vibrance":2.0,"color.saturation":1.0},"confidence":{"light.contrast":1.0,"color.vibrance":1.0,"color.saturation":1.0},"provenance":{"source":"lightroom"}}}
            ]},
            {"shoot_id":"validation-1","split":"validation","camera":{"make":"Ember","model":"Synthetic"},"photos":[
                {"photo_id":"validation-a","features":feature_values([0.5,0.5,0.5,0.5,0.5,0.0,0.3,0.3,0.1,0.1,0.1,1.5,1.0,0.0,6500.0,0.0],[0.36,0.046,0.31,0.44,0.056,0.35,0.55,0.066,0.39,0.63,0.076,0.43]),"baseline":{"light.contrast":0.0,"color.vibrance":0.0,"color.saturation":0.0},"emberGroundTruth":{"values":{"light.contrast":5.0,"color.vibrance":1.0,"color.saturation":0.5},"confidence":{"light.contrast":1.0,"color.vibrance":1.0,"color.saturation":1.0},"provenance":{"accepted":true,"origin":"human-edit"}}}
            ]},
            {"shoot_id":"test-1","split":"test","camera":{"make":"Other","model":"Synthetic"},"photos":[
                {"photo_id":"test-a","features":feature_values([0.5,0.5,0.5,0.5,0.5,0.0,0.3,0.3,0.1,0.1,0.1,1.5,1.0,0.0,6500.0,0.0],[0.27,0.037,0.26,0.33,0.047,0.29,0.41,0.057,0.32,0.52,0.067,0.36]),"baseline":{"light.contrast":0.0,"color.vibrance":0.0,"color.saturation":0.0},"weakLabelColdStart":{"values":{"light.contrast":4.0,"color.vibrance":1.0,"color.saturation":0.5},"confidence":{"light.contrast":1.0,"color.vibrance":1.0,"color.saturation":1.0},"provenance":{"source":"lightroom"}}}
            ]}
        ]})
    }

    fn decoded_manifest() -> Value {
        let mut value = manifest();
        value["baselineReceiptSchema"] = json!(BASELINE_RECEIPT_SCHEMA);
        let digest = "11".repeat(32);
        for shoot in value["shoots"].as_array_mut().unwrap() {
            for photo in shoot["photos"].as_array_mut().unwrap() {
                let features_digest = lightcraft_photo_ai::digest(&serde_json::to_vec(&photo["features"]).unwrap());
                let baseline = BTreeMap::from([
                    ("light.contrast".to_owned(), photo["baseline"]["light.contrast"].as_f64().unwrap()),
                    ("color.vibrance".to_owned(), photo["baseline"]["color.vibrance"].as_f64().unwrap()),
                    ("color.saturation".to_owned(), photo["baseline"]["color.saturation"].as_f64().unwrap()),
                ]);
                let baseline_digest = lightcraft_photo_ai::digest(&serde_json::to_vec(&baseline).unwrap());
                photo["baselineReceipt"] = json!({
                    "schema": BASELINE_RECEIPT_SCHEMA,
                    "autoRevision": BASELINE_AUTO_REVISION,
                    "featureSchema": FEATURE_SCHEMA,
                    "sourceLevel": "thumb",
                    "settingsVersion": SCHEMA_VERSION,
                    "sourcePixelsDigest": digest.clone(),
                    "settingsDigest": digest.clone(),
                    "featuresDigest": features_digest,
                    "baselineDigest": baseline_digest,
                    "sourceDigest": digest.clone(),
                    "renderCacheVersion": RENDER_CACHE_VERSION,
                    "cameraProfileCacheKey": camera_profiles::cache_key()
                });
            }
        }
        value
    }

    #[test]
    fn trains_only_train_shoots_and_evaluates_holdouts() {
        let parsed = parse_manifest(&manifest()).unwrap();
        let model = train_model(&parsed, "fixture").unwrap();
        assert_eq!(model["schema"], MODEL_SCHEMA);
        assert_eq!(model["trainingSplit"], "train");
        assert_eq!(model["training"]["shootCount"], 1);
        let report = evaluate_model(&parsed, &model, "fixture", None).unwrap();
        assert_eq!(report["excludedSplits"][0], "train");
        assert_eq!(report["variants"]["emberGroundTruth"]["splits"]["validation"]["photos"]["count"], 1);
        assert_eq!(report["claims"]["visualPreference"], "unavailable_without_supplied_blinded_votes_and_render_receipts");
    }

    #[test]
    fn excluded_controls_are_rejected() {
        let mut value = manifest();
        value["controls"] = json!(["light.contrast", "color.vibrance", "light.exposure"]);
        assert!(parse_manifest(&value).is_err());
    }

    #[test]
    fn canonical_v2_manifest_accepts_28_features() {
        let parsed = parse_manifest(&manifest()).expect("canonical v2 manifest must parse");
        assert_eq!(parsed.features.len(), 28);
        assert_eq!(parsed.features, FEATURE_NAMES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>());
    }

    #[test]
    fn legacy_feature_schema_requires_reextraction() {
        let mut value = manifest();
        value["feature_schema"] = json!([
            "ev.p01",
            "ev.p05",
            "ev.median",
            "ev.p95",
            "ev.p995",
            "ev.spread",
            "chroma.p90",
            "luminance.mean",
            "luminance.stddev",
            "scene.shadow_share",
            "scene.highlight_share",
            "source.aspect",
            "source.raw",
            "baseline.exposure",
            "source.wb_temp",
            "source.wb_tint"
        ]);
        let error = parse_manifest(&value).expect_err("legacy features must be rejected");
        assert!(error.contains("re-extract"));
    }

    #[test]
    fn legacy_model_schema_requires_retraining() {
        let parsed = parse_manifest(&manifest()).unwrap();
        let mut model = train_model(&parsed, "fixture").unwrap();
        model["schema"] = json!(LEGACY_MODEL_SCHEMA);
        let error = evaluate_model(&parsed, &model, "fixture", Some(Variant::Ember)).expect_err("legacy models must be rejected");
        assert!(error.contains("retrain"));
    }

    #[test]
    fn legacy_baseline_receipt_requires_reextraction() {
        let mut value = decoded_manifest();
        value["baselineReceiptSchema"] = json!(LEGACY_BASELINE_RECEIPT_SCHEMA);
        let error = parse_manifest(&value).expect_err("legacy receipts must be rejected");
        assert!(error.contains("re-extract"));
    }

    #[test]
    fn frozen_numeric_fixture_keeps_whole_shoot_splits() {
        let value: Value = serde_json::from_str(include_str!("../../../tests/fixtures/personal-auto/minimal.json")).unwrap();
        let parsed = parse_manifest(&value).unwrap();
        assert_eq!(parsed.shoots.iter().filter(|shoot| shoot.split == "train").count(), 1);
        assert_eq!(parsed.shoots.iter().filter(|shoot| shoot.split == "validation").count(), 1);
        assert_eq!(parsed.shoots.iter().filter(|shoot| shoot.split == "test").count(), 1);
        assert_eq!(parsed.shoots.iter().map(|shoot| shoot.photos.len()).sum::<usize>(), 4);
    }

    #[test]
    fn malformed_label_and_nonhuman_provenance_are_rejected() {
        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"]["values"]["light.contrast"] = json!("bad");
        let error = parse_manifest(&value).expect_err("malformed numeric label must not be dropped");
        assert!(error.contains("emberGroundTruth"));

        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"].as_object_mut().unwrap().remove("confidence");
        let error = parse_manifest(&value).expect_err("missing confidence must not default silently");
        assert!(error.contains("confidence"));

        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"]["provenance"] = json!({"accepted": true, "origin": "automatic-edit"});
        let error = parse_manifest(&value).expect_err("automatic edit is not Ember ground truth");
        assert!(error.contains("accepted human-edit provenance"));

        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"]["provenance"] = json!({"accepted": true, "origin": "invented-human-edit"});
        assert!(parse_manifest(&value).is_err(), "arbitrary provenance text must not qualify");

        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"]["provenance"] =
            json!({"accepted": true, "origin": "human-edit", "source": "synthetic-human-edit"});
        assert!(parse_manifest(&value).is_err(), "conflicting provenance aliases must not qualify");
    }

    #[test]
    fn canonical_labels_reject_wrong_digest_type_and_conflicting_value_aliases() {
        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"]["provenance"]["provenanceSha256"] = json!(true);
        assert!(parse_manifest(&value).is_err(), "wrong provenance digest type must fail");

        let mut value = manifest();
        value["shoots"][0]["photos"][0]["emberGroundTruth"]["settings"] = json!({"light.contrast": 10.0});
        assert!(parse_manifest(&value).is_err(), "values/settings aliases must not coexist");

        let mut value = manifest();
        let baseline = value["shoots"][0]["photos"][0]["baseline"].clone();
        value["shoots"][0]["photos"][0]["baseline"] = json!({"values": baseline.clone(), "settings": baseline});
        assert!(parse_manifest(&value).is_err(), "baseline values/settings aliases must not coexist");

        let mut value = manifest();
        value["featureNames"] = value["feature_schema"].clone();
        value["featureNames"][0] = json!("different.feature");
        assert!(parse_manifest(&value).is_err(), "schema aliases must not conflict");

        let mut value = manifest();
        value["shoots"][0]["shootId"] = json!("different-shoot");
        assert!(parse_manifest(&value).is_err(), "shoot ID aliases must not conflict");
    }

    #[test]
    fn camera_strata_validate_components_before_joining() {
        assert_eq!(camera_key(Some(&json!({"make": "Ember", "model": "Synthetic"}))).unwrap(), "Ember/Synthetic");
        assert!(camera_key(Some(&json!("/private/path"))).is_err());
        assert!(camera_key(Some(&json!({"make": "Ember", "model": "private/path"}))).is_err());
        assert!(camera_key(Some(&json!({"make": "Ember", "manufacturer": "Other"}))).is_err());
    }

    #[test]
    fn sample_provenance_is_digest_only_and_bound_to_identity() {
        let parsed = parse_manifest(&manifest()).unwrap();
        let shoot = parsed.shoots.first().unwrap();
        let photo = shoot.photos.first().unwrap();
        let sample = core_sample(shoot, photo, Variant::Ember).unwrap().unwrap();
        assert_eq!(sample.provenance.sample_id, photo.id);
        assert_eq!(sample.provenance.shoot_id, shoot.id);
        assert!(sample.provenance.source_ref.starts_with("label:human-edit:"));
        assert!(!sample.provenance.source_ref.contains("accepted"));
    }

    #[test]
    fn missing_model_has_no_comparison_or_numeric_status() {
        let parsed = parse_manifest(&manifest()).unwrap();
        let mut model = train_model(&parsed, "fixture").unwrap();
        model["variants"]["emberGroundTruth"] = Value::Null;
        let report = evaluate_model(&parsed, &model, "fixture", Some(Variant::Ember)).unwrap();
        assert_eq!(report["status"], "harness-only");
        let coverage = &report["variants"]["emberGroundTruth"]["splits"]["validation"]["coverage"];
        assert_eq!(coverage["input"], 1);
        assert_eq!(coverage["eligible"], 1);
        assert_eq!(coverage["scored"], 0);
        assert_eq!(coverage["skipped"], 1);
        assert_eq!(coverage["skippedReasons"]["missingModel"], 1);
    }

    #[test]
    fn altered_training_receipt_is_rejected_during_evaluation() {
        let parsed = parse_manifest(&manifest()).unwrap();
        let mut model = train_model(&parsed, "fixture").unwrap();
        model["trainingLabelReceipts"]["emberGroundTruth"][0]["provenanceSha256"] = json!("00");
        let error = evaluate_model(&parsed, &model, "fixture", Some(Variant::Ember)).expect_err("receipt must bind model to manifest");
        assert!(error.contains("training label receipts"));
    }

    #[test]
    fn baseline_renderer_contract_requires_retraining() {
        let parsed = parse_manifest(&manifest()).unwrap();
        let mut model = train_model(&parsed, "fixture").unwrap();
        model["baselineContract"]["autoRevision"] = json!("lightcraft.deterministic-auto.v3");
        let error = evaluate_model(&parsed, &model, "fixture", Some(Variant::Ember)).expect_err("renderer drift must reject model");
        assert!(error.contains("baseline renderer contract"));
    }

    #[test]
    fn partial_baseline_receipts_are_rejected() {
        let mut value = manifest();
        value["shoots"][0]["photos"][0]["baselineReceipt"] = json!({"schema": BASELINE_RECEIPT_SCHEMA});
        let error = parse_manifest(&value).expect_err("partial baseline receipt must fail");
        assert!(error.contains("renderer identity") || error.contains("sourceDigest"));
    }

    #[test]
    fn decoded_receipts_bind_features_baseline_and_settings_schema() {
        let value = decoded_manifest();
        parse_manifest(&value).expect("fixture receipt digests should match numeric payload");

        let mut value = decoded_manifest();
        value["shoots"][0]["photos"][0]["features"][0] = json!(99.0);
        let error = parse_manifest(&value).expect_err("feature mutation must invalidate receipt");
        assert!(error.contains("featuresDigest"));

        let mut value = decoded_manifest();
        value["shoots"][0]["photos"][0]["features"][27] = json!(99.0);
        let error = parse_manifest(&value).expect_err("spatial feature mutation must invalidate receipt");
        assert!(error.contains("featuresDigest"));

        let mut value = decoded_manifest();
        value["shoots"][0]["photos"][0]["baseline"]["light.contrast"] = json!(99.0);
        let error = parse_manifest(&value).expect_err("baseline mutation must invalidate receipt");
        assert!(error.contains("baselineDigest"));

        let mut value = decoded_manifest();
        value["shoots"][0]["photos"][0]["baselineReceipt"]["settingsVersion"] = json!(0);
        let error = parse_manifest(&value).expect_err("stale settings schema must invalidate receipt");
        assert!(error.contains("settingsVersion"));
    }

    #[test]
    fn numeric_manifests_reject_path_bearing_root_shoot_and_photo_fields() {
        let mut value = manifest();
        value["path"] = json!("/private/catalog");
        let error = parse_manifest(&value).expect_err("root path must not enter numeric manifest");
        assert!(error.contains("path"));

        let mut value = manifest();
        value["shoots"][0]["path"] = json!("/private/shoot");
        let error = parse_manifest(&value).expect_err("shoot path must not enter numeric manifest");
        assert!(error.contains("path"));

        let mut value = manifest();
        value["shoots"][0]["photos"][0]["path"] = json!("/private/photo");
        let error = parse_manifest(&value).expect_err("photo path must not enter numeric manifest");
        assert!(error.contains("path"));
    }

    #[test]
    fn rejected_prediction_scores_explicit_baseline_fallback() {
        let mut parsed = parse_manifest(&manifest()).unwrap();
        parsed.shoots[1].photos[0].features[0] = f64::NAN;
        let model_value = train_model(&parsed, "fixture").unwrap();
        let core_value = model_value["variants"]["emberGroundTruth"].clone();
        let core: PersonalAutoModel = serde_json::from_value(core_value).unwrap();
        let report = evaluate_split(&parsed, Some(&core), Variant::Ember, "validation");
        assert_eq!(report["coverage"]["eligible"], 1);
        assert_eq!(report["coverage"]["scored"], 1);
        assert_eq!(report["coverage"]["modelScored"], 0);
        assert_eq!(report["coverage"]["fallbackScored"], 1);
        assert_eq!(report["coverage"]["fallback"], 1);
        assert_eq!(report["coverage"]["fallbackReasons"]["invalidFeatures"], 1);
        assert_eq!(report["photos"]["baselineMean"], report["photos"]["regressorMean"]);
        let variant_report = evaluate_variant(&parsed, &model_value, Variant::Ember);
        assert_eq!(variant_report["status"], "harness-only", "fallback-only split cannot claim numeric model evaluation");
    }
}
