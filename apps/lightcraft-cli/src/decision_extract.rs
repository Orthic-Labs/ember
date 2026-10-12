//! Offline, bounded rendered-pair extraction. No catalog, sidecar, provider or model writes.
//! Matching measures geometry on a 512-pixel comparison grid, not at original sensor resolution.

mod geometry;

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use lightcraft_codecs::DecodeOptions;
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Session, SourceLevel, camera_profiles, catalog::PhotoId, media::RENDER_CACHE_VERSION};
use lightcraft_geom::Orientation;
use lightcraft_photo_ai::digest;
use lightcraft_pipeline::auto::{self, SceneKey};
use lightcraft_pipeline::look::{self, LookTarget};
use lightcraft_pipeline::output::{DeepSamples, OutputDepth, OutputSpace};
use lightcraft_pipeline::personal_auto::{FEATURE_NAMES, FEATURE_SCHEMA, FeatureVector};
use lightcraft_pipeline::{RenderRequest, SourceInfo};
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

const SCHEMA: &str = "ember.ai-decision-numeric.v1";
const INPUT_SCHEMA: &str = "ember.ai-decision-extract-input.v1";
const MAX_MEDIA_BYTES: u64 = 512 * 1024 * 1024;
const EDGE: usize = 512;

pub fn run(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut index = 0usize;
    while let Some(option) = args.get(index) {
        let slot = match option.as_str() {
            "--manifest" => &mut input,
            "--out" => &mut output,
            _ => return Err("ai decision extract --manifest PRIVATE_JSON --out NEW_NUMERIC_JSON".into()),
        };
        if slot.is_some() {
            return Err("duplicate extractor option".into());
        }
        index = index.saturating_add(1);
        *slot = Some(args.get(index).ok_or("missing option value")?.clone());
        index = index.saturating_add(1);
    }
    let input = input.ok_or("extract requires --manifest")?;
    let output = output.ok_or("extract requires --out")?;
    let input_bytes = read_bounded(Path::new(&input), 4 * 1024 * 1024)?;
    let manifest: Value = serde_json::from_slice(&input_bytes).map_err(|_| "invalid extractor JSON")?;
    validate_manifest(&manifest)?;
    // Reserve create-new output before decoding; an existing output is never overwritten.
    let mut file = OpenOptions::new().write(true).create_new(true).open(&output).map_err(|_| "cannot create new numeric output")?;
    let records = manifest.get("records").and_then(Value::as_array).ok_or("records missing")?;
    let mut results = Vec::with_capacity(records.len());
    for row in records {
        let id = text(row, "photo_id")?;
        eprintln!("decision extract: {id}");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract_pair(row)));
        let result = match result {
            Ok(Ok(value)) => value,
            Ok(Err(reason)) => json!({"photo_id": id, "status": "quarantined", "reason": reason}),
            Err(_) => json!({"photo_id": id, "status": "quarantined", "reason": "decoder_or_renderer_panicked"}),
        };
        results.push(result);
    }
    let report = json!({
        "schema": SCHEMA, "extractor_revision": "rendered-pair.v1", "input_sha256": digest(&input_bytes),
        "auto_revision": auto::REVISION, "feature_schema": FEATURE_SCHEMA, "feature_names": FEATURE_NAMES,
        "feature_frame": "source resampled into verified export frame; deterministic Auto is recomputed on that frame",
        "statistics": "linear Rec.2020 D65 luminance; gamma 2.2 encoded quantiles p01,p05,p50,p95,p995; floor((n-1)*q); clip Y>=0.985",
        "target_contract": "finished median/spread minus deterministic objective median/spread; top,bottom,clip,weights remain deterministic",
        "alignment_contract": geometry::CONTRACT,
        "runtime": {"os": std::env::consts::OS, "architecture": std::env::consts::ARCH},
        "render_cache_version": RENDER_CACHE_VERSION, "camera_profile_cache_key": camera_profiles::cache_key(),
        "records": results, "training_started": false, "split_assignment": "external complete-corpus connected components required"
    });
    let bytes = serde_json::to_vec(&report).map_err(|_| "numeric serialization failed")?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("numeric output exceeds cap".into());
    }
    file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|_| "numeric output write failed")?;
    println!("{}", json!({"schema": SCHEMA, "records": records.len()}));
    Ok(())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value.get(key).and_then(Value::as_str).filter(|v| !v.is_empty() && v.len() <= 4096).ok_or_else(|| format!("missing_or_invalid_{key}"))
}

fn validate_manifest(value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("manifest must be object")?;
    if object.keys().any(|key| !["schema", "user_consented", "records"].contains(&key.as_str()))
        || value.get("schema").and_then(Value::as_str) != Some(INPUT_SCHEMA)
        || value.get("user_consented").and_then(Value::as_bool) != Some(true)
    {
        return Err("extract requires strict schema and user_consented:true".into());
    }
    let records = value.get("records").and_then(Value::as_array).ok_or("records must be array")?;
    if records.is_empty() || records.len() > 64 {
        return Err("extract batch must contain 1..64 records".into());
    }
    let mut ids = BTreeSet::new();
    for row in records {
        let fields = row.as_object().ok_or("record must be object")?;
        let allowed = [
            "photo_id",
            "source_path",
            "export_path",
            "source_sha256",
            "export_sha256",
            "provenance_sha256",
            "metadata_identity_bound",
            "target_intent",
            "unsupported_geometry",
            "source_is_raw",
        ];
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err("unknown pair field".into());
        }
        let id = text(row, "photo_id")?;
        if id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) || !ids.insert(id) {
            return Err("invalid or duplicate opaque photo_id".into());
        }
        for key in ["source_sha256", "export_sha256", "provenance_sha256"] {
            let hash = text(row, key)?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return Err(format!("invalid {key}"));
            }
        }
        text(row, "source_path")?;
        text(row, "export_path")?;
        if row.get("metadata_identity_bound").and_then(Value::as_bool) != Some(true)
            || row.get("target_intent").and_then(Value::as_str) != Some("user_selected_finished_export")
            || row.get("unsupported_geometry").and_then(Value::as_bool).is_none()
            || row.get("source_is_raw").and_then(Value::as_bool).is_none()
        {
            return Err("explicit identity, target intent & geometry disposition required".into());
        }
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|_| "input_unreadable")?;
    let info = file.metadata().map_err(|_| "input_metadata_unreadable")?;
    if !info.is_file() || info.len() == 0 || info.len() > limit {
        return Err("input_not_regular_or_outside_size_cap".into());
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| "input_read_failed")?;
    if bytes.len() as u64 != info.len() || bytes.len() as u64 > limit {
        return Err("input_changed_or_exceeds_size_cap".into());
    }
    Ok(bytes)
}

fn checked_media(path: &str, expected: &str) -> Result<Vec<u8>, String> {
    let bytes = read_bounded(Path::new(path), MAX_MEDIA_BYTES)?;
    if digest(&bytes) != expected {
        return Err("media_sha256_mismatch".into());
    }
    Ok(bytes)
}

fn finite_image(image: &Rgb32f) -> Result<(), String> {
    if image.width == 0
        || image.height == 0
        || image.width.checked_mul(image.height) != Some(image.data.len())
        || image.data.len() > 4_000_000
        || image.data.iter().flatten().any(|v| !v.is_finite())
    {
        return Err("invalid_or_nonfinite_proxy".into());
    }
    Ok(())
}

fn render_linear(source: &Rgb32f, info: &SourceInfo, settings: &DevelopSettings) -> Result<Rgb32f, String> {
    let mut request = RenderRequest::fit(EDGE, EDGE);
    request.space = OutputSpace::Rec2020;
    request.depth = OutputDepth::F32Linear;
    let rendered = lightcraft_pipeline::render(source, info, settings, &request);
    let deep = rendered.deep.ok_or("deep_render_missing")?;
    let DeepSamples::F32(samples) = deep.samples else { return Err("deep_render_wrong_type".into()) };
    if deep.width.checked_mul(deep.height).and_then(|n| n.checked_mul(3)) != Some(samples.len()) || samples.len() > EDGE * EDGE * 3 {
        return Err("deep_render_invalid_storage".into());
    }
    let mut image = Rgb32f::new(deep.width, deep.height);
    for (pixel, channels) in image.data.iter_mut().zip(samples.chunks_exact(3)) {
        let [r, g, b] = channels else { return Err("deep_render_invalid_channel_count".into()) };
        *pixel = [*r, *g, *b];
    }
    finite_image(&image)?;
    Ok(image)
}

fn pixels_digest(image: &Rgb32f) -> String {
    let mut bytes = Vec::with_capacity(image.data.len().saturating_mul(12).saturating_add(16));
    bytes.extend_from_slice(&(image.width as u64).to_le_bytes());
    bytes.extend_from_slice(&(image.height as u64).to_le_bytes());
    for pixel in &image.data {
        for value in pixel {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    digest(&bytes)
}

fn measured(image: &Rgb32f) -> Result<LookTarget, String> {
    finite_image(image)?;
    let stats = look::extract_linear(std::slice::from_ref(image)).ok_or("render_statistics_empty")?;
    stats.validate().map_err(|_| "render_statistics_invalid")?;
    Ok(stats)
}

fn extract_pair(row: &Value) -> Result<Value, String> {
    if row.get("unsupported_geometry").and_then(Value::as_bool) == Some(true) {
        return Err("unsupported_perspective_or_content_aware_edit".into());
    }
    let source_path = text(row, "source_path")?;
    let export_path = text(row, "export_path")?;
    let source_sha = text(row, "source_sha256")?;
    let export_sha = text(row, "export_sha256")?;
    if source_sha == export_sha {
        return Err("source_and_export_identical_content".into());
    }
    let source_before = std::fs::metadata(source_path).map_err(|_| "source_metadata_unreadable")?;
    drop(checked_media(source_path, source_sha)?);
    let export_bytes = checked_media(export_path, export_sha)?;
    let decoded = lightcraft_codecs::decode(&export_bytes, DecodeOptions { max_size: Some((EDGE as u32, EDGE as u32)), max_pixels: 64_000_000 })
        .map_err(|_| "finished_export_decode_failed")?;
    if decoded.has_alpha || decoded.cmyk {
        return Err("unsupported_alpha_or_cmyk_export".into());
    }
    let finished = decoded.to_working().into_oriented(Orientation::from_exif(decoded.orientation));
    finite_image(&finished)?;
    let finished_dimensions = [decoded.source_width, decoded.source_height];
    let finished_icc_sha = decoded.icc.as_ref().map(|bytes| digest(bytes));
    drop(decoded);
    drop(export_bytes);

    let mut session = Session::new().with_fs();
    session.xmp.auto_write = false;
    let imported = session
        .execute("library.import", &json!({"paths": [source_path], "local": true, "rawJpegPolicy": "keepBoth"}))
        .map_err(|_| "source_import_failed")?;
    let ids = imported.get("imported").and_then(Value::as_array).ok_or("source_import_missing_id")?;
    if ids.len() != 1 {
        return Err("source_import_not_single".into());
    }
    let id = PhotoId(ids.first().and_then(Value::as_u64).ok_or("source_import_invalid_id")?);
    let base = DevelopSettings::default();
    session.set_develop(id, base.clone(), "Offline rendered-pair source").map_err(|_| "source_reset_failed")?;
    let source = session.source_now(id, SourceLevel::Thumb).map_err(|_| "source_decode_failed")?;
    let info = session.source_info(id);
    if row.get("source_is_raw").and_then(Value::as_bool) != Some(info.raw) {
        return Err("source_decode_kind_mismatch_or_embedded_preview_fallback".into());
    }
    finite_image(&source)?;
    let before = render_linear(&source, &info, &base)?;
    let (aligned_source, geometry) = geometry::align(&source, &before, &finished)?;
    let source_after = std::fs::metadata(source_path).map_err(|_| "source_metadata_unreadable")?;
    if source_before.len() != source_after.len() || source_before.modified().ok() != source_after.modified().ok() {
        return Err("source_changed_during_extraction".into());
    }
    drop(checked_media(source_path, source_sha)?);
    let population = auto::scene_population(&aligned_source, &info, &base).ok_or("source_population_empty")?;
    let auto = auto::auto_tone_of(&population, &info);
    let settings = look::settings_with(&base, auto);
    let features = FeatureVector::from_pipeline(&aligned_source, &info, &settings).map_err(|_| "features_invalid")?;
    let key = SceneKey::of(population.median, population.p05, population.p95, population.p995);
    let objectives = auto::fit::auto_targets(&population.ev, auto.exposure as f32, key, &info);
    let before_aligned = render_linear(&aligned_source, &info, &base)?;
    let baseline = render_linear(&aligned_source, &info, &settings)?;
    let before_stats = measured(&before_aligned)?;
    let baseline_stats = measured(&baseline)?;
    let final_stats = measured(&finished)?;
    let [_, p05, median, p95, _] = final_stats.luminance;
    let spread = p95 - p05;
    let feature_bytes = serde_json::to_vec(&features.0).map_err(|_| "feature_serialization_failed")?;
    let settings_bytes = serde_json::to_vec(&settings).map_err(|_| "settings_serialization_failed")?;
    Ok(json!({
        "photo_id": text(row, "photo_id")?, "status": "extracted_pending_grouped_split",
        "source_sha256": source_sha, "export_sha256": export_sha, "provenance_sha256": text(row, "provenance_sha256")?,
        "features": features.0, "features_sha256": digest(&feature_bytes),
        "target_fields": ["median", "spread"], "finished_targets": [median, spread],
        "deterministic_targets": {"median": objectives.median, "spread": objectives.spread, "top": objectives.top,
            "bottom": objectives.bottom, "clip": objectives.clip, "weights": objectives.w},
        "target_residuals": [median - objectives.median, spread - objectives.spread],
        "before_statistics": before_stats, "baseline_statistics": baseline_stats, "finished_statistics": final_stats,
        "baseline_auto": auto, "baseline_settings_sha256": digest(&settings_bytes),
        "source_pixels_sha256": pixels_digest(&source), "aligned_source_pixels_sha256": pixels_digest(&aligned_source),
        "before_pixels_sha256": pixels_digest(&before_aligned), "baseline_pixels_sha256": pixels_digest(&baseline),
        "finished_pixels_sha256": pixels_digest(&finished), "geometry": geometry,
        "source_fingerprint": geometry::fingerprint(&before), "finished_fingerprint": geometry::fingerprint(&finished),
        "finished_source_dimensions": finished_dimensions, "finished_icc_sha256": finished_icc_sha,
        "split": "unassigned", "training_eligible": false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rejects_unbound_or_unapproved_pairs() {
        let row = json!({"photo_id":"p1","source_path":"source","export_path":"export",
            "source_sha256":"a".repeat(64),"export_sha256":"b".repeat(64),"provenance_sha256":"c".repeat(64),
            "metadata_identity_bound":false,"target_intent":"user_selected_finished_export","unsupported_geometry":false,"source_is_raw":false});
        let mut value = json!({"schema":INPUT_SCHEMA,"user_consented":true,"records":[row]});
        assert!(validate_manifest(&value).is_err());
        value["records"][0]["metadata_identity_bound"] = json!(true);
        assert!(validate_manifest(&value).is_ok());
        value["records"][0]["source_sha256"] = json!("A".repeat(64));
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn statistics_measure_finished_pixels_and_reject_nonfinite() {
        let image = Rgb32f::from_fn(64, 64, |x, _| {
            let v = x as f32 / 63.0;
            [v, v, v]
        });
        let result = measured(&image).expect("finite gradient");
        let [_, lo, median, hi, _] = result.luminance;
        assert!(lo < median && median < hi && hi - lo > 0.5);
        let mut malformed = image;
        malformed.data[0][0] = f32::NAN;
        assert!(measured(&malformed).is_err());
    }

    #[test]
    fn digest_binds_dimensions_as_well_as_pixels() {
        let mut a = Rgb32f::new(4, 2);
        let mut b = Rgb32f::new(2, 4);
        a.data.fill([0.2, 0.3, 0.4]);
        b.data.fill([0.2, 0.3, 0.4]);
        assert_ne!(pixels_digest(&a), pixels_digest(&b));
    }

    #[test]
    fn paired_pixels_extract_repeatably_without_mutating_inputs() {
        use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples};
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("ember-decision-{}-{unique}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        // Band-limited procedural texture: many unique, stable corners across every quadrant.
        let mut pixels = Vec::new();
        for y in 0..384u32 {
            for x in 0..512u32 {
                let cell = (x / 12).wrapping_mul(0x9e3779b9) ^ (y / 12).wrapping_mul(0x85ebca6b);
                let value = 35 + ((cell ^ (cell >> 13)).wrapping_mul(0xc2b2ae35) % 180) as u8;
                pixels.extend_from_slice(&[value, value, value]);
            }
        }
        let input = EncodeImage::new(512, 384, 3, Samples::U8(&pixels));
        let source_bytes = lightcraft_codecs::encode_png(&input, &EncodeMeta::default()).unwrap();
        let final_pixels: Vec<u8> = pixels.iter().map(|v| v.saturating_add(8)).collect();
        let final_image = EncodeImage::new(512, 384, 3, Samples::U8(&final_pixels));
        let export_bytes = lightcraft_codecs::encode_png(&final_image, &EncodeMeta::default()).unwrap();
        let source = dir.join("source.png");
        let export = dir.join("finished.png");
        std::fs::write(&source, &source_bytes).unwrap();
        std::fs::write(&export, &export_bytes).unwrap();
        let mut row = json!({"photo_id":"synthetic","source_path":source,"export_path":export,
            "source_sha256":digest(&source_bytes),"export_sha256":digest(&export_bytes),"provenance_sha256":"c".repeat(64),
            "metadata_identity_bound":true,"target_intent":"user_selected_finished_export","unsupported_geometry":false,"source_is_raw":false});
        let first = extract_pair(&row).expect("procedural pair must align and extract");
        let second = extract_pair(&row).expect("repeat must extract");
        assert_eq!(first, second);
        assert_eq!(first["features"].as_array().unwrap().len(), 28);
        assert_eq!(first["training_eligible"], false);
        assert!(!serde_json::to_string(&first).unwrap().contains("source.png"));
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
        assert_eq!(std::fs::read(&export).unwrap(), export_bytes);
        row["source_sha256"] = json!("0".repeat(64));
        assert_eq!(extract_pair(&row).unwrap_err(), "media_sha256_mismatch");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
