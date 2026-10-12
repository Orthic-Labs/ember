//! Assisted culling: score photos for focus and clipping, group similar shots taken close
//! together (bursts) and mark the sharpest of each group; optionally reject blurry ones and pick
//! each group's best. Scores are kept on the photos (rules / smart albums: `sharpness`,
//! `bestOfGroup`).

use lightcraft_catalog::{Analysis, Flag, Op, PhotoId};
use serde_json::{Value, json};
use web_time::Instant;

use super::{CommandSpec, always, cmd};
use crate::{Result, Session};

/// Seconds between shots of the same burst at most.
const BURST_GAP: i64 = 10;
/// Signature similarity for "the same scene".
const SAME: f32 = 0.93;
const PROPOSAL_VERSION: u64 = 1;
const TIMING_SCHEMA: &str = "lightcraft.cull-timing.v1";

fn secs(iso: &str) -> Option<i64> {
    let iso = iso.trim();
    let bytes = iso.as_bytes();
    // Culling needs precise capture times so partial dates never create burst groups. Keep
    // parser work bounded, then reject extra clock fields and malformed fractions that
    // `DateTime::parse_iso` intentionally tolerates.
    if bytes.len() > 128
        || bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !bytes[..4]
            .iter()
            .chain(&bytes[5..7])
            .chain(&bytes[8..10])
            .chain(&bytes[11..13])
            .chain(&bytes[14..16])
            .chain(&bytes[17..19])
            .all(u8::is_ascii_digit)
    {
        return None;
    }
    let parsed = lightcraft_meta::DateTime::parse_iso(iso)?;
    let time = iso.get(11..)?;
    let zone_at = time.find(['Z', '+', '-']).unwrap_or(time.len());
    let clock = &time[..zone_at];
    let mut fields = clock.split(':');
    fields.next()?;
    fields.next()?;
    let seconds = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    if let Some((_, fraction)) = seconds.split_once('.')
        && (fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let days = match parsed.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if parsed.year % 400 == 0 || (parsed.year % 4 == 0 && parsed.year % 100 != 0) => 29,
        2 => 28,
        _ => return None,
    };
    if parsed.day == 0 || parsed.day > days || parsed.hour >= 24 || parsed.minute >= 60 || parsed.second >= 60 {
        return None;
    }
    Some(parsed.unix_seconds())
}

fn source_identity(photo: &lightcraft_catalog::Photo) -> String {
    let key = crate::media::content_key(photo);
    let path = match &photo.source {
        lightcraft_catalog::Source::File { path } => Some(path.as_str()),
        lightcraft_catalog::Source::Demo { .. } => None,
    };
    source_identity_for(&key, path)
}

fn source_identity_for(key: &str, path: Option<&str>) -> String {
    let Some(path) = path else {
        return key.to_string();
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return format!("{key}|stat:missing");
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| format!("{}:{}", time.as_secs(), time.subsec_nanos()))
        .unwrap_or_else(|| "unknown".into());
    format!("{key}|stat:{modified}:len:{}", meta.len())
}

#[derive(Clone)]
struct CullRow {
    id: PhotoId,
    file_name: String,
    sharpness: f32,
    clipped: f32,
    source: String,
    flag: Flag,
    group: Option<u32>,
    best: bool,
    decision: lightcraft_pipeline::cull::report::Decision,
    uncertainty: lightcraft_pipeline::cull::report::Uncertainty,
    reason_codes: Vec<lightcraft_pipeline::cull::report::ReasonCode>,
}

/// Detached culling input. Source refs own their decoder and can cross into a worker without a
/// `Session` or catalog lock.
#[derive(Clone)]
pub struct CullPhotoSnapshot {
    pub id: PhotoId,
    pub file_name: String,
    pub captured_secs: Option<i64>,
    pub source: String,
    pub source_key: String,
    pub source_path: Option<String>,
    pub flag: Flag,
    pub source_ref: crate::media::SourceRef,
}

/// Owned culling request suitable for bounded background execution.
pub struct CullJob {
    pub catalog_revision: u64,
    pub reject_below: Option<f32>,
    pub pick_best: bool,
    pub photos: Vec<CullPhotoSnapshot>,
}

/// Result produced by [`CullJob::run`]. Caller must validate revision/source state before showing
/// it as current or passing its nested proposal to `photo.cullApply`.
pub struct CullJobResult {
    pub catalog_revision: u64,
    pub value: Value,
}

#[derive(Clone, Copy)]
enum TimingStatus {
    Ok,
    DecodeFailed,
    AnalysisFailed,
    SourceChanged,
}

impl TimingStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::DecodeFailed => "decodeFailed",
            Self::AnalysisFailed => "analysisFailed",
            Self::SourceChanged => "sourceChanged",
        }
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn photo_timing(id: PhotoId, decode_ms: Option<f64>, analysis_ms: Option<f64>, total_ms: f64, status: TimingStatus) -> Value {
    json!({
        "id": id.0,
        "decodeMs": decode_ms,
        "analysisMs": analysis_ms,
        "totalMs": total_ms,
        "status": status.as_str(),
    })
}

fn timing_report(per_photo: Vec<Value>, grouping_ms: f64, job_total_ms: f64) -> Value {
    json!({
        "schema": TIMING_SCHEMA,
        "classicalOnly": true,
        "sourcePolicy": "uncached-origin-thumbnail",
        "scope": "measurement-and-planning",
        "excludes": ["timingReceiptSerialization", "proposalSerialization", "proposalBinding", "dispatch", "import"],
        "perPhoto": per_photo,
        "groupingMs": grouping_ms,
        "jobTotalMs": job_total_ms,
        "stages": {
            "decode": {"status": "measured"},
            "analysis": {"status": "measured"},
            "crops": {"status": "notApplicable"},
            "inference": {"status": "notApplicable"},
        },
    })
}

/// Detached apply plan. Run [`Self::job`] off-thread, then pass whole plan & result to
/// [`Session::finish_cull_apply`] for final validation & one undoable commit.
pub struct CullApplyJob {
    pub job: CullJob,
    proposal: Value,
    accepted: std::collections::BTreeMap<PhotoId, Flag>,
}

pub type CullProgressFn<'a> = dyn Fn(f32, &str) -> bool + Sync + 'a;

fn validate_policy_values(reject_below: Option<f32>, _pick_best: bool) -> std::result::Result<(), String> {
    if reject_below.is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value)) {
        return Err("`rejectBelow` must be a finite number 0..100".into());
    }
    Ok(())
}

impl CullJob {
    pub fn new(
        catalog_revision: u64,
        photos: Vec<CullPhotoSnapshot>,
        reject_below: Option<f32>,
        pick_best: bool,
    ) -> std::result::Result<Self, String> {
        validate_policy_values(reject_below, pick_best)?;
        if photos.len() > lightcraft_pipeline::cull::report::MAX_MEASUREMENTS {
            return Err(format!("too many culling photo ids: {}", photos.len()));
        }
        let mut ids = std::collections::HashSet::with_capacity(photos.len());
        for photo in &photos {
            if !ids.insert(photo.id) {
                return Err(format!("duplicate photo id {}", photo.id.0));
            }
        }
        Ok(Self { catalog_revision, reject_below, pick_best, photos })
    }

    fn measure_rows(&self, progress: &CullProgressFn<'_>) -> std::result::Result<(Vec<CullRow>, Vec<Value>, Value), String> {
        let job_started = Instant::now();
        if !progress(0.0, "Reading photos") {
            return Err("cancelled".into());
        }
        let total = self.photos.len().max(1) as f32;
        let mut rows = Vec::new();
        let mut measurements = Vec::new();
        let mut failed = Vec::new();
        let mut per_photo = Vec::with_capacity(self.photos.len());
        for (index, photo) in self.photos.iter().enumerate() {
            if !progress(index as f32 / total, "Reading photos") {
                return Err("cancelled".into());
            }
            let photo_started = Instant::now();
            let decode_started = Instant::now();
            let source = match photo.source_ref.load() {
                Ok(source) => source,
                Err(error) => {
                    let decode_ms = elapsed_ms(decode_started);
                    let total_ms = elapsed_ms(photo_started);
                    per_photo.push(photo_timing(photo.id, Some(decode_ms), None, total_ms, TimingStatus::DecodeFailed));
                    failed.push(json!([photo.id.0, error]));
                    if !progress((index + 1) as f32 / total, "Measuring photos") {
                        return Err("cancelled".into());
                    }
                    continue;
                }
            };
            let decode_ms = elapsed_ms(decode_started);
            let analysis_started = Instant::now();
            let measured = lightcraft_pipeline::cull::measure_checked(&source);
            let analysis_ms = elapsed_ms(analysis_started);
            match measured {
                Ok(measured) => {
                    if let Some(path) = photo.source_path.as_deref()
                        && source_identity_for(&photo.source_key, Some(path)) != photo.source
                    {
                        let total_ms = elapsed_ms(photo_started);
                        per_photo.push(photo_timing(photo.id, Some(decode_ms), Some(analysis_ms), total_ms, TimingStatus::SourceChanged));
                        failed.push(json!([photo.id.0, "source changed while measuring"]));
                        if !progress((index + 1) as f32 / total, "Measuring photos") {
                            return Err("cancelled".into());
                        }
                        continue;
                    }
                    let total_ms = elapsed_ms(photo_started);
                    per_photo.push(photo_timing(photo.id, Some(decode_ms), Some(analysis_ms), total_ms, TimingStatus::Ok));
                    measurements.push(lightcraft_pipeline::cull::report::Measurement {
                        id: photo.id.0,
                        captured_secs: photo.captured_secs,
                        sharpness: measured.sharpness,
                        clipped: measured.clipped,
                        signature: measured.signature,
                    });
                    rows.push(CullRow {
                        id: photo.id,
                        file_name: photo.file_name.clone(),
                        sharpness: measured.sharpness,
                        clipped: measured.clipped,
                        source: photo.source.clone(),
                        flag: photo.flag,
                        group: None,
                        best: false,
                        decision: lightcraft_pipeline::cull::report::Decision::Abstain,
                        uncertainty: lightcraft_pipeline::cull::report::Uncertainty::Abstain,
                        reason_codes: Vec::new(),
                    });
                }
                Err(error) => {
                    let total_ms = elapsed_ms(photo_started);
                    per_photo.push(photo_timing(photo.id, Some(decode_ms), Some(analysis_ms), total_ms, TimingStatus::AnalysisFailed));
                    failed.push(json!([photo.id.0, error.to_string()]));
                }
            }
            if !progress((index + 1) as f32 / total, "Measuring photos") {
                return Err("cancelled".into());
            }
        }
        let policy = lightcraft_pipeline::cull::report::CullPolicy {
            burst_gap_secs: BURST_GAP,
            similarity_threshold: SAME,
            reject_below: self.reject_below,
            pick_best: self.pick_best,
        };
        let grouping_started = Instant::now();
        let report = lightcraft_pipeline::cull::report::plan(&measurements, &policy).map_err(|error| error.to_string())?;
        let grouping_ms = elapsed_ms(grouping_started);
        let report_by_id: std::collections::HashMap<u64, &lightcraft_pipeline::cull::report::PhotoReport> =
            report.photos.iter().map(|photo| (photo.id, photo)).collect();
        for row in &mut rows {
            if let Some(measured) = report_by_id.get(&row.id.0) {
                row.group = measured.group;
                row.best = measured.best;
                row.decision = measured.decision;
                row.uncertainty = measured.uncertainty;
                row.reason_codes = measured.reason_codes.clone();
            }
        }
        Ok((rows, failed, timing_report(per_photo, grouping_ms, elapsed_ms(job_started))))
    }

    pub fn run(&self, progress: &CullProgressFn<'_>) -> std::result::Result<CullJobResult, String> {
        let (rows, failed, measurement_timing) = self.measure_rows(progress)?;
        Ok(CullJobResult {
            catalog_revision: self.catalog_revision,
            value: proposal_result_revision(self.catalog_revision, &rows, failed, self.reject_below, self.pick_best, measurement_timing),
        })
    }
}

fn exact_object<'a>(value: &'a Value, required: &[&str], what: &str) -> Result<&'a serde_json::Map<String, Value>> {
    let object = value.as_object().ok_or_else(|| super::bad("photo.cullSuggest", format!("{what} must be an object")))?;
    if required.iter().any(|key| !object.contains_key(*key)) || object.keys().any(|key| !required.iter().any(|expected| *expected == key)) {
        return Err(super::bad("photo.cullSuggest", format!("{what} fields are invalid")));
    }
    Ok(object)
}

fn f32_value(value: &Value, field: &str, min: f32, max: f32) -> Result<f32> {
    let value = value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= f64::from(min) && *value <= f64::from(max))
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
        .ok_or_else(|| super::bad("photo.cullSuggest", format!("proposal {field} is invalid")))?;
    Ok(value)
}

fn timing_ms(value: &Value, field: &str, nullable: bool) -> Result<Option<f64>> {
    if value.is_null() {
        return nullable.then_some(None).ok_or_else(|| super::bad("photo.cullSuggest", format!("worker timing {field} is missing")));
    }
    let value = value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| super::bad("photo.cullSuggest", format!("worker timing {field} is invalid")))?;
    Ok(Some(value))
}

fn validate_measurement_timing(job: &CullJob, value: &Value) -> Result<()> {
    const C: &str = "photo.cullSuggest";
    let timing = exact_object(
        value,
        &["schema", "classicalOnly", "sourcePolicy", "scope", "excludes", "perPhoto", "groupingMs", "jobTotalMs", "stages"],
        "worker measurement timing",
    )?;
    if timing["schema"].as_str() != Some(TIMING_SCHEMA)
        || timing["classicalOnly"].as_bool() != Some(true)
        || timing["sourcePolicy"].as_str() != Some("uncached-origin-thumbnail")
        || timing["scope"].as_str() != Some("measurement-and-planning")
    {
        return Err(super::bad(C, "worker measurement timing metadata is invalid"));
    }
    let excludes = timing["excludes"].as_array().ok_or_else(|| super::bad(C, "worker measurement timing exclusions are invalid"))?;
    let expected_excludes = ["timingReceiptSerialization", "proposalSerialization", "proposalBinding", "dispatch", "import"];
    if excludes.len() != expected_excludes.len() || excludes.iter().zip(expected_excludes).any(|(actual, expected)| actual.as_str() != Some(expected))
    {
        return Err(super::bad(C, "worker measurement timing exclusions are invalid"));
    }
    timing_ms(&timing["groupingMs"], "groupingMs", false)?;
    timing_ms(&timing["jobTotalMs"], "jobTotalMs", false)?;
    let stages = exact_object(&timing["stages"], &["decode", "analysis", "crops", "inference"], "worker measurement timing stages")?;
    for stage in ["decode", "analysis"] {
        let stage = exact_object(&stages[stage], &["status"], "worker measurement timing stage")?;
        if stage["status"].as_str() != Some("measured") {
            return Err(super::bad(C, "worker measurement timing stage is invalid"));
        }
    }
    for stage in ["crops", "inference"] {
        let stage = exact_object(&stages[stage], &["status"], "worker measurement timing stage")?;
        if stage["status"].as_str() != Some("notApplicable") {
            return Err(super::bad(C, "worker measurement timing absent stage is invalid"));
        }
    }
    let per_photo = timing["perPhoto"].as_array().ok_or_else(|| super::bad(C, "worker per-photo timing is invalid"))?;
    if per_photo.len() != job.photos.len() {
        return Err(super::bad(C, "worker per-photo timing coverage is incomplete"));
    }
    let expected: std::collections::HashSet<PhotoId> = job.photos.iter().map(|photo| photo.id).collect();
    let mut seen = std::collections::HashSet::with_capacity(per_photo.len());
    for value in per_photo {
        let photo = exact_object(value, &["id", "decodeMs", "analysisMs", "totalMs", "status"], "worker per-photo timing")?;
        let id = photo["id"].as_u64().map(PhotoId).ok_or_else(|| super::bad(C, "worker per-photo timing id is invalid"))?;
        if !expected.contains(&id) || !seen.insert(id) {
            return Err(super::bad(C, "worker per-photo timing id is invalid"));
        }
        let decode = timing_ms(&photo["decodeMs"], "decodeMs", true)?;
        let analysis = timing_ms(&photo["analysisMs"], "analysisMs", true)?;
        let total = timing_ms(&photo["totalMs"], "totalMs", false)?.ok_or_else(|| super::bad(C, "worker per-photo total timing is missing"))?;
        if decode.is_some_and(|value| total < value) || analysis.is_some_and(|value| total < value) {
            return Err(super::bad(C, "worker per-photo total timing is shorter than a measured stage"));
        }
        match photo["status"].as_str() {
            Some("ok") | Some("sourceChanged") if decode.is_some() && analysis.is_some() => {}
            Some("decodeFailed") if decode.is_some() && analysis.is_none() => {}
            Some("analysisFailed") if decode.is_some() && analysis.is_some() => {}
            _ => return Err(super::bad(C, "worker per-photo timing status is invalid")),
        }
    }
    if seen.len() != expected.len() {
        return Err(super::bad(C, "worker per-photo timing coverage is incomplete"));
    }
    Ok(())
}

fn validate_worker_result(job: &CullJob, value: Value) -> Result<()> {
    const C: &str = "photo.cullSuggest";
    let result = exact_object(&value, &["photos", "groups", "rejected", "picked", "failed", "proposal", "measurementTiming"], "worker result")?;
    validate_measurement_timing(job, result.get("measurementTiming").ok_or_else(|| super::bad(C, "worker measurement timing is missing"))?)?;
    let proposal_value = result.get("proposal").ok_or_else(|| super::bad(C, "worker proposal is missing"))?;
    let proposal = exact_object(proposal_value, &["version", "catalogRevision", "policy", "photos", "binding"], "worker proposal")?;
    let version = proposal["version"].as_u64().ok_or_else(|| super::bad(C, "worker proposal version is invalid"))?;
    if version != PROPOSAL_VERSION {
        return Err(super::bad(C, "worker proposal version is unsupported"));
    }
    let revision = proposal["catalogRevision"].as_u64().ok_or_else(|| super::bad(C, "worker proposal revision is invalid"))?;
    if revision != job.catalog_revision {
        return Err(super::bad(C, "worker proposal revision is stale"));
    }
    let policy = exact_object(&proposal["policy"], &["rejectBelow", "pickBest"], "worker proposal policy")?;
    let (reject_below, pick_best) = strict_policy(&Value::Object(policy.clone()), C)?;
    if pick_best != job.pick_best || reject_below.map(f32::to_bits) != job.reject_below.map(f32::to_bits) {
        return Err(super::bad(C, "worker proposal policy does not match job"));
    }
    let binding = proposal["binding"].as_str().ok_or_else(|| super::bad(C, "worker proposal binding is invalid"))?;
    if lightcraft_preview::Hash128::parse(binding).is_none() {
        return Err(super::bad(C, "worker proposal binding is invalid"));
    }
    let proposal_photos = proposal["photos"].as_array().ok_or_else(|| super::bad(C, "worker proposal photos are invalid"))?;
    let result_photos = result["photos"].as_array().ok_or_else(|| super::bad(C, "worker result photos are invalid"))?;
    if proposal_photos != result_photos || proposal_photos.len() > job.photos.len() {
        return Err(super::bad(C, "worker proposal photo coverage is invalid"));
    }
    let expected: std::collections::HashMap<PhotoId, &CullPhotoSnapshot> = job.photos.iter().map(|photo| (photo.id, photo)).collect();
    let mut rows = Vec::with_capacity(proposal_photos.len());
    let mut seen = std::collections::HashSet::with_capacity(proposal_photos.len());
    for value in proposal_photos {
        let photo = exact_object(
            value,
            &["id", "fileName", "source", "flag", "proposedFlag", "sharpness", "clipped", "group", "best", "decision", "uncertainty", "reasonCodes"],
            "worker proposal photo",
        )?;
        let id = photo["id"].as_u64().map(PhotoId).ok_or_else(|| super::bad(C, "worker proposal photo id is invalid"))?;
        if !seen.insert(id) {
            return Err(super::bad(C, format!("duplicate worker proposal photo id {}", id.0)));
        }
        let snapshot = expected.get(&id).ok_or_else(|| super::bad(C, format!("unknown worker proposal photo id {}", id.0)))?;
        let file_name = photo["fileName"].as_str().ok_or_else(|| super::bad(C, "worker proposal fileName is invalid"))?.to_string();
        let source = photo["source"].as_str().ok_or_else(|| super::bad(C, "worker proposal source is invalid"))?.to_string();
        if file_name != snapshot.file_name || source != snapshot.source {
            return Err(super::bad(C, format!("worker proposal photo {} does not match snapshot", id.0)));
        }
        let flag = parse_flag(&photo["flag"], C, "flag")?;
        if flag != snapshot.flag {
            return Err(super::bad(C, format!("worker proposal flag for photo {} does not match snapshot", id.0)));
        }
        let proposed = match &photo["proposedFlag"] {
            Value::Null => None,
            value => Some(parse_flag(value, C, "proposedFlag")?),
        };
        let sharpness = f32_value(&photo["sharpness"], "sharpness", 0.0, 100.0)?;
        let clipped = f32_value(&photo["clipped"], "clipped", 0.0, 1.0)?;
        let group = match &photo["group"] {
            Value::Null => None,
            value => {
                Some(value.as_u64().and_then(|value| u32::try_from(value).ok()).ok_or_else(|| super::bad(C, "worker proposal group is invalid"))?)
            }
        };
        let best = photo["best"].as_bool().ok_or_else(|| super::bad(C, "worker proposal best is invalid"))?;
        let decision = serde_json::from_value(photo["decision"].clone()).map_err(|_| super::bad(C, "worker proposal decision is invalid"))?;
        let uncertainty =
            serde_json::from_value(photo["uncertainty"].clone()).map_err(|_| super::bad(C, "worker proposal uncertainty is invalid"))?;
        let reason_codes =
            serde_json::from_value(photo["reasonCodes"].clone()).map_err(|_| super::bad(C, "worker proposal reasonCodes are invalid"))?;
        let row = CullRow { id, file_name, sharpness, clipped, source, flag, group, best, decision, uncertainty, reason_codes };
        if proposed != proposed_flag(&row, job.reject_below, job.pick_best) {
            return Err(super::bad(C, format!("worker proposal action for photo {} is inconsistent", id.0)));
        }
        rows.push(row);
    }

    let failed = result["failed"].as_array().ok_or_else(|| super::bad(C, "worker result failures are invalid"))?;
    let mut failed_ids = std::collections::HashSet::with_capacity(failed.len());
    for item in failed {
        let pair = item.as_array().filter(|pair| pair.len() == 2).ok_or_else(|| super::bad(C, "worker result failure must be [id, message]"))?;
        let id = pair[0].as_u64().map(PhotoId).ok_or_else(|| super::bad(C, "worker result failure id is invalid"))?;
        if !expected.contains_key(&id) || !failed_ids.insert(id) || seen.contains(&id) || pair[1].as_str().is_none() {
            return Err(super::bad(C, format!("worker result failure for photo {} is invalid", id.0)));
        }
    }
    if seen.len() + failed_ids.len() != job.photos.len()
        || job.photos.iter().any(|photo| !seen.contains(&photo.id) && !failed_ids.contains(&photo.id))
    {
        return Err(super::bad(C, "worker result photo coverage is incomplete"));
    }

    let groups = rows.iter().filter_map(|row| row.group).collect::<std::collections::HashSet<_>>().len() as u64;
    let rejected = rows.iter().filter(|row| proposed_flag(row, job.reject_below, job.pick_best) == Some(Flag::Reject)).count() as u64;
    let picked = rows.iter().filter(|row| proposed_flag(row, job.reject_below, job.pick_best) == Some(Flag::Pick)).count() as u64;
    if result["groups"].as_u64() != Some(groups) || result["rejected"].as_u64() != Some(rejected) || result["picked"].as_u64() != Some(picked) {
        return Err(super::bad(C, "worker result aggregate counts are invalid"));
    }
    if binding_hash(job.catalog_revision, job.reject_below, job.pick_best, &rows) != binding {
        return Err(super::bad(C, "worker proposal binding does not match contents"));
    }
    Ok(())
}

fn strict_policy(p: &Value, cmd: &str) -> Result<(Option<f32>, bool)> {
    let reject_below = match p.get("rejectBelow") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                .ok_or_else(|| super::bad(cmd, "`rejectBelow` must be a finite number 0..100"))?;
            Some(n as f32)
        }
    };
    let pick_best = match p.get("pickBest") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| super::bad(cmd, "`pickBest` must be a boolean"))?,
    };
    validate_policy_values(reject_below, pick_best).map_err(|message| super::bad(cmd, message))?;
    Ok((reject_below, pick_best))
}

fn strict_ids(s: &mut Session, p: &Value, cmd: &str) -> Result<Vec<PhotoId>> {
    let ids = if let Some(raw) = p.get("ids") {
        let a = raw.as_array().ok_or_else(|| super::bad(cmd, "`ids` must be an array of photo ids"))?;
        if a.len() > lightcraft_pipeline::cull::report::MAX_MEASUREMENTS {
            return Err(super::bad(cmd, format!("too many culling photo ids: {}", a.len())));
        }
        a.iter()
            .map(|v| v.as_u64().map(PhotoId).ok_or_else(|| super::bad(cmd, "`ids` must contain only unsigned photo ids")))
            .collect::<Result<Vec<_>>>()?
    } else if s.selection.ids.len() > 1 {
        s.targets(p)
    } else {
        s.visible_cloned()
    };
    if ids.len() > lightcraft_pipeline::cull::report::MAX_MEASUREMENTS {
        return Err(super::bad(cmd, format!("too many culling photo ids: {}", ids.len())));
    }
    let mut seen = std::collections::HashSet::new();
    for id in &ids {
        if !seen.insert(*id) {
            return Err(super::bad(cmd, format!("duplicate photo id {}", id.0)));
        }
        if s.catalog.photo(*id).is_none() {
            return Err(super::bad(cmd, format!("unknown photo id {}", id.0)));
        }
    }
    Ok(ids)
}

fn prepare_cull_job(s: &mut Session, ids: &[PhotoId], reject_below: Option<f32>, pick_best: bool, cmd: &str) -> Result<CullJob> {
    validate_policy_values(reject_below, pick_best).map_err(|message| super::bad(cmd, message))?;
    if ids.len() > lightcraft_pipeline::cull::report::MAX_MEASUREMENTS {
        return Err(super::bad(cmd, format!("too many culling photo ids: {}", ids.len())));
    }
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    let mut photos = Vec::with_capacity(ids.len());
    for id in ids {
        if !seen.insert(*id) {
            return Err(super::bad(cmd, format!("duplicate photo id {}", id.0)));
        }
        let photo = s.catalog.photo(*id).ok_or_else(|| super::bad(cmd, format!("unknown photo id {}", id.0)))?.clone();
        let source_key = crate::media::content_key(&photo);
        let source_path = match &photo.source {
            lightcraft_catalog::Source::File { path } => Some(path.clone()),
            lightcraft_catalog::Source::Demo { .. } => None,
        };
        let source_ref = s.media.origin_ref(&photo.source, crate::media::SourceLevel::Thumb.max_edge());
        photos.push(CullPhotoSnapshot {
            id: *id,
            file_name: photo.file_name.clone(),
            captured_secs: photo.captured.as_deref().and_then(secs),
            source: source_identity(&photo),
            source_key,
            source_path,
            flag: photo.flag,
            source_ref,
        });
    }
    CullJob::new(s.catalog.revision, photos, reject_below, pick_best).map_err(|message| super::bad(cmd, message))
}

impl Session {
    /// Detach culling sources so decode can run on a worker without borrowing this session.
    pub fn plan_cull_job(&mut self, ids: &[PhotoId], reject_below: Option<f32>, pick_best: bool) -> Result<CullJob> {
        prepare_cull_job(self, ids, reject_below, pick_best, "photo.cullSuggest")
    }

    /// Revalidate a worker result against current catalog revision, source identities & flags.
    pub fn validate_cull_job_result(&self, job: &CullJob, result: &CullJobResult) -> Result<Value> {
        if job.catalog_revision != self.catalog.revision
            || result.catalog_revision != self.catalog.revision
            || result.catalog_revision != job.catalog_revision
        {
            return Err(super::bad("photo.cullSuggest", "stale culling result: catalog changed"));
        }
        for photo in &job.photos {
            let current = self
                .catalog
                .photo(photo.id)
                .ok_or_else(|| super::bad("photo.cullSuggest", format!("stale culling result: photo {} was removed", photo.id.0)))?;
            if current.flag != photo.flag || current.file_name != photo.file_name || source_identity(current) != photo.source {
                return Err(super::bad("photo.cullSuggest", "stale culling result: source or flags changed"));
            }
        }
        validate_worker_result(job, result.value.clone())?;
        Ok(result.value.clone())
    }
}

fn parse_cull_apply(s: &mut Session, p: &Value) -> Result<CullApplyJob> {
    const C: &str = "photo.cullApply";
    let proposal_arg = p.get("proposal").ok_or_else(|| super::bad(C, "missing `proposal`"))?;
    let proposal = if proposal_arg.get("version").is_some() {
        proposal_arg
    } else {
        proposal_arg.get("proposal").ok_or_else(|| super::bad(C, "`proposal` must be a cull proposal or cullSuggest result"))?
    };
    let proposal = exact_object(proposal, &["version", "catalogRevision", "policy", "photos", "binding"], "cull proposal")?;
    let proposal = Value::Object(proposal.clone());
    let version = proposal["version"].as_u64().ok_or_else(|| super::bad(C, "proposal version is missing or invalid"))?;
    if version != PROPOSAL_VERSION {
        return Err(super::bad(C, "unsupported proposal version"));
    }
    let revision = proposal["catalogRevision"].as_u64().ok_or_else(|| super::bad(C, "proposal catalog revision is missing or invalid"))?;
    if revision != s.catalog.revision {
        return Err(super::bad(C, "stale culling proposal: catalog changed"));
    }
    let binding = proposal["binding"].as_str().ok_or_else(|| super::bad(C, "proposal binding is missing or invalid"))?;
    if lightcraft_preview::Hash128::parse(binding).is_none() {
        return Err(super::bad(C, "proposal binding is invalid"));
    }
    let policy = exact_object(&proposal["policy"], &["rejectBelow", "pickBest"], "cull proposal policy")?;
    let (reject_below, pick_best) = strict_policy(&Value::Object(policy.clone()), C)?;
    let photo_values = proposal["photos"].as_array().ok_or_else(|| super::bad(C, "proposal photos are missing or invalid"))?;
    if photo_values.len() > lightcraft_pipeline::cull::report::MAX_MEASUREMENTS {
        return Err(super::bad(C, "too many culling proposal photos"));
    }
    let mut ids = Vec::with_capacity(photo_values.len());
    let mut rows = Vec::with_capacity(photo_values.len());
    let mut seen_ids = std::collections::HashSet::with_capacity(photo_values.len());
    let mut offered = std::collections::HashMap::with_capacity(photo_values.len());
    for photo in photo_values {
        let object = exact_object(
            photo,
            &["id", "fileName", "source", "flag", "proposedFlag", "sharpness", "clipped", "group", "best", "decision", "uncertainty", "reasonCodes"],
            "cull proposal photo",
        )?;
        let id = object["id"].as_u64().map(PhotoId).ok_or_else(|| super::bad(C, "proposal photo id is invalid"))?;
        if !seen_ids.insert(id) {
            return Err(super::bad(C, format!("duplicate proposal photo id {}", id.0)));
        }
        let file_name = object["fileName"].as_str().ok_or_else(|| super::bad(C, "proposal photo fileName is invalid"))?.to_owned();
        let source = object["source"].as_str().ok_or_else(|| super::bad(C, "proposal photo source is invalid"))?.to_owned();
        let prior_flag = parse_flag(&object["flag"], C, "flag")?;
        let proposed = if object["proposedFlag"].is_null() { None } else { Some(parse_flag(&object["proposedFlag"], C, "proposedFlag")?) };
        offered.insert(id, (prior_flag, proposed));
        let sharpness = f32_value(&object["sharpness"], "sharpness", 0.0, 100.0)?;
        let clipped = f32_value(&object["clipped"], "clipped", 0.0, 1.0)?;
        let group = if object["group"].is_null() {
            None
        } else {
            Some(
                object["group"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| super::bad(C, "proposal photo group is invalid"))?,
            )
        };
        let best = object["best"].as_bool().ok_or_else(|| super::bad(C, "proposal photo best is invalid"))?;
        let decision: lightcraft_pipeline::cull::report::Decision =
            serde_json::from_value(object["decision"].clone()).map_err(|_| super::bad(C, "proposal photo decision is invalid"))?;
        let uncertainty: lightcraft_pipeline::cull::report::Uncertainty =
            serde_json::from_value(object["uncertainty"].clone()).map_err(|_| super::bad(C, "proposal photo uncertainty is invalid"))?;
        let reason_codes: Vec<lightcraft_pipeline::cull::report::ReasonCode> =
            serde_json::from_value(object["reasonCodes"].clone()).map_err(|_| super::bad(C, "proposal photo reasonCodes are invalid"))?;
        let row = CullRow { id, file_name, sharpness, clipped, source, flag: prior_flag, group, best, decision, uncertainty, reason_codes };
        if proposed != proposed_flag(&row, reject_below, pick_best) {
            return Err(super::bad(C, format!("proposal action for photo {} is inconsistent", id.0)));
        }
        rows.push(row);
        ids.push(id);
    }
    if binding_hash(revision, reject_below, pick_best, &rows) != binding {
        return Err(super::bad(C, "proposal binding does not match contents"));
    }
    // Re-emit validated rows so wire-level f64 formatting cannot make an unchanged f32
    // measurement compare unequal to the worker's canonical JSON. Binding still covers exact
    // f32 bits, source identity, flags, grouping, decisions & proposed actions.
    let canonical_proposal = proposal_value_revision(revision, &rows, reject_below, pick_best);
    let accept = p.get("accept").and_then(Value::as_array).ok_or_else(|| super::bad(C, "`accept` must be an array of {id, flag}"))?;
    if accept.len() > lightcraft_pipeline::cull::report::MAX_MEASUREMENTS {
        return Err(super::bad(C, "too many accepted culling photos"));
    }
    let mut accepted = std::collections::BTreeMap::new();
    for item in accept {
        let object = item.as_object().ok_or_else(|| super::bad(C, "each accepted item must be an object"))?;
        if object.keys().any(|key| key != "id" && key != "flag") || !object.contains_key("id") || !object.contains_key("flag") {
            return Err(super::bad(C, "accepted item fields are invalid"));
        }
        let id = object["id"].as_u64().map(PhotoId).ok_or_else(|| super::bad(C, "accepted photo id is invalid"))?;
        let flag = parse_flag(&object["flag"], C, "flag")?;
        if offered.get(&id) != Some(&(Flag::None, Some(flag))) || flag == Flag::None {
            return Err(super::bad(C, format!("accepted flag for photo {} does not match an available proposal", id.0)));
        }
        if accepted.insert(id, flag).is_some() {
            return Err(super::bad(C, format!("duplicate accepted photo id {}", id.0)));
        }
    }
    let job = prepare_cull_job(s, &ids, reject_below, pick_best, C)?;
    let snapshots: std::collections::HashMap<PhotoId, &CullPhotoSnapshot> = job.photos.iter().map(|photo| (photo.id, photo)).collect();
    for row in &rows {
        let snapshot = snapshots.get(&row.id).ok_or_else(|| super::bad(C, format!("proposal photo {} is not in current catalog", row.id.0)))?;
        if row.file_name != snapshot.file_name || row.source != snapshot.source || row.flag != snapshot.flag {
            return Err(super::bad(C, format!("proposal photo {} does not match current source or flag", row.id.0)));
        }
    }
    Ok(CullApplyJob { job, proposal: canonical_proposal, accepted })
}

impl Session {
    /// Validate apply input & detach sources before any worker decode.
    pub fn plan_cull_apply(&mut self, params: &Value) -> Result<CullApplyJob> {
        parse_cull_apply(self, params)
    }

    /// Validate detached measurements against current catalog, then commit accepted flags once.
    pub fn finish_cull_apply(&mut self, prepared: CullApplyJob, result: CullJobResult) -> Result<Value> {
        const C: &str = "photo.cullApply";
        let fresh = self.validate_cull_job_result(&prepared.job, &result)?;
        let result_object = fresh.as_object().ok_or_else(|| super::bad(C, "worker result is invalid"))?;
        if result_object["failed"].as_array().is_none_or(|failed| !failed.is_empty()) {
            return Err(super::bad(C, "stale culling proposal: a proposed photo can no longer be measured"));
        }
        let fresh_proposal = result_object.get("proposal").ok_or_else(|| super::bad(C, "worker proposal is missing"))?;
        if fresh_proposal.get("binding") != prepared.proposal.get("binding") || fresh_proposal.get("photos") != prepared.proposal.get("photos") {
            return Err(super::bad(C, "stale culling proposal: source, flags or measurements changed"));
        }
        let photos = fresh_proposal["photos"].as_array().ok_or_else(|| super::bad(C, "worker proposal photos are invalid"))?;
        let rows_by_id: std::collections::HashMap<PhotoId, &Value> =
            photos.iter().filter_map(|photo| photo.get("id").and_then(Value::as_u64).map(|id| (PhotoId(id), photo))).collect();
        let mut ops = Vec::new();
        for (id, flag) in prepared.accepted {
            let row = rows_by_id.get(&id).ok_or_else(|| super::bad(C, format!("accepted photo id {} is not in proposal", id.0)))?;
            let row_flag = parse_flag(row.get("flag").ok_or_else(|| super::bad(C, "proposal photo flag is missing"))?, C, "flag")?;
            let proposed = row
                .get("proposedFlag")
                .and_then(|value| (!value.is_null()).then_some(value))
                .map(|value| parse_flag(value, C, "proposedFlag"))
                .transpose()?;
            if row_flag != Flag::None {
                return Err(super::bad(C, format!("photo {} already has a flag", id.0)));
            }
            if proposed != Some(flag) {
                return Err(super::bad(C, format!("accepted flag for photo {} does not match proposal", id.0)));
            }
            ops.push(Op::SetFlag { id, flag });
        }
        if ops.is_empty() {
            return Ok(json!({"accepted": 0}));
        }
        let count = ops.len();
        self.commit("Accept Assisted Culling", Op::Batch { ops })?;
        Ok(json!({"accepted": count}))
    }
}

fn measure(s: &mut Session, ids: &[PhotoId], cmd: &str, reject_below: Option<f32>, pick_best: bool) -> Result<(Vec<CullRow>, Vec<Value>, Value)> {
    let job = prepare_cull_job(s, ids, reject_below, pick_best, cmd)?;
    job.measure_rows(&|_, _| true).map_err(|message| super::bad(cmd, message))
}

fn flag_name(flag: Flag) -> &'static str {
    match flag {
        Flag::None => "none",
        Flag::Pick => "pick",
        Flag::Reject => "reject",
    }
}

fn parse_flag(v: &Value, cmd: &str, key: &str) -> Result<Flag> {
    let name = v.as_str().ok_or_else(|| super::bad(cmd, format!("`{key}` must be a flag string")))?;
    Flag::parse(name).ok_or_else(|| super::bad(cmd, format!("unknown flag `{name}` in `{key}`")))
}

fn proposed_flag(row: &CullRow, reject_below: Option<f32>, pick_best: bool) -> Option<Flag> {
    if row.flag != Flag::None {
        return None;
    }
    if reject_below.is_some_and(|threshold| row.sharpness < threshold) {
        Some(Flag::Reject)
    } else if pick_best && row.best {
        Some(Flag::Pick)
    } else {
        None
    }
}

fn row_json(row: &CullRow, reject_below: Option<f32>, pick_best: bool) -> Value {
    let proposed = proposed_flag(row, reject_below, pick_best).map(flag_name);
    json!({
        "id": row.id.0,
        "fileName": row.file_name,
        "source": row.source,
        "flag": flag_name(row.flag),
        "proposedFlag": proposed,
        // Keep exact f32 for threshold decisions; clients can round for display.
        "sharpness": row.sharpness,
        "clipped": row.clipped,
        "group": row.group,
        "best": row.best,
        "decision": row.decision,
        "uncertainty": row.uncertainty,
        "reasonCodes": row.reason_codes,
    })
}

fn binding_hash(revision: u64, reject_below: Option<f32>, pick_best: bool, rows: &[CullRow]) -> String {
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(|r| r.id);
    let mut h = lightcraft_preview::Hasher128::new();
    h.str("photo.cull.v1").u64(revision);
    match reject_below {
        Some(v) => h.u64(v.to_bits() as u64),
        None => h.u64(u64::MAX),
    };
    h.u64(pick_best as u64);
    for row in sorted {
        h.u64(row.id.0).str(&row.file_name).str(&row.source).str(flag_name(row.flag));
        h.u64(row.sharpness.to_bits() as u64).u64(row.clipped.to_bits() as u64);
        h.u64(row.group.is_some() as u64).u64(row.group.unwrap_or(0) as u64).u64(row.best as u64);
        h.str(proposed_flag(&row, reject_below, pick_best).map(flag_name).unwrap_or(""));
        h.str(&serde_json::to_string(&row.decision).unwrap_or_default());
        h.str(&serde_json::to_string(&row.uncertainty).unwrap_or_default());
        h.str(&serde_json::to_string(&row.reason_codes).unwrap_or_default());
    }
    h.finish().to_string()
}

fn proposal_value_revision(revision: u64, rows: &[CullRow], reject_below: Option<f32>, pick_best: bool) -> Value {
    let photos: Vec<Value> = rows.iter().map(|row| row_json(row, reject_below, pick_best)).collect();
    json!({
        "version": PROPOSAL_VERSION,
        "catalogRevision": revision,
        "policy": {"rejectBelow": reject_below, "pickBest": pick_best},
        "photos": photos,
        "binding": binding_hash(revision, reject_below, pick_best, rows),
    })
}

fn proposal_result(
    s: &Session,
    rows: &[CullRow],
    failed: Vec<Value>,
    reject_below: Option<f32>,
    pick_best: bool,
    measurement_timing: Value,
) -> Value {
    proposal_result_revision(s.catalog.revision, rows, failed, reject_below, pick_best, measurement_timing)
}

fn proposal_result_revision(
    revision: u64,
    rows: &[CullRow],
    failed: Vec<Value>,
    reject_below: Option<f32>,
    pick_best: bool,
    measurement_timing: Value,
) -> Value {
    let photos: Vec<Value> = rows.iter().map(|row| row_json(row, reject_below, pick_best)).collect();
    let rejected = rows.iter().filter(|row| proposed_flag(row, reject_below, pick_best) == Some(Flag::Reject)).count();
    let picked = rows.iter().filter(|row| proposed_flag(row, reject_below, pick_best) == Some(Flag::Pick)).count();
    let groups = rows.iter().filter_map(|row| row.group).collect::<std::collections::HashSet<_>>().len();
    json!({
        "photos": photos,
        "groups": groups,
        "rejected": rejected,
        "picked": picked,
        "failed": failed,
        "proposal": proposal_value_revision(revision, rows, reject_below, pick_best),
        "measurementTiming": measurement_timing,
    })
}

fn cull_suggest(s: &mut Session, p: &Value) -> Result<Value> {
    let (reject_below, pick_best) = strict_policy(p, "photo.cullSuggest")?;
    let ids = strict_ids(s, p, "photo.cullSuggest")?;
    let (rows, failed, measurement_timing) = measure(s, &ids, "photo.cullSuggest", reject_below, pick_best)?;
    Ok(proposal_result(s, &rows, failed, reject_below, pick_best, measurement_timing))
}

fn cull_apply(s: &mut Session, p: &Value) -> Result<Value> {
    let prepared = s.plan_cull_apply(p)?;
    let result = prepared.job.run(&|_, _| true).map_err(|message| super::bad("photo.cullApply", message))?;
    s.finish_cull_apply(prepared, result)
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let dry_run = match p.get("dryRun") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| super::bad("photo.analyze", "`dryRun` must be a boolean"))?,
    };
    if dry_run {
        let (reject_below, pick_best) = strict_policy(p, "photo.analyze")?;
        let ids = strict_ids(s, p, "photo.analyze")?;
        let (rows, failed, measurement_timing) = measure(s, &ids, "photo.analyze", reject_below, pick_best)?;
        return Ok(proposal_result(s, &rows, failed, reject_below, pick_best, measurement_timing));
    }
    let ids = strict_ids(s, p, "photo.analyze")?;
    let (reject_below, pick_best) = strict_policy(p, "photo.analyze")?;
    // measure (thumbnail-level sources: fast, and enough for focus at 512 px)
    let mut rows: Vec<(PhotoId, Option<i64>, f32, f32, [f32; 64])> = Vec::new();
    let mut failed = Vec::new();
    for id in ids {
        match s.source_now(id, crate::media::SourceLevel::Thumb) {
            Ok(src) => {
                let t = s.catalog.photo(id).and_then(|p| p.captured.as_deref().and_then(secs));
                use lightcraft_pipeline::cull;
                match cull::measure_checked(&src) {
                    Ok(measured) => rows.push((id, t, measured.sharpness, measured.clipped, measured.signature)),
                    Err(e) => failed.push(json!([id.0, e.to_string()])),
                }
            }
            Err(e) => failed.push(json!([id.0, e])),
        }
    }
    // bursts: consecutive (by capture time) look-alike shots
    rows.sort_by_key(|r| (r.1.unwrap_or(i64::MAX), r.0));
    let mut group_of: Vec<Option<u32>> = vec![None; rows.len()];
    let mut next = 1u32;
    for i in 1..rows.len() {
        let (a, b) = (&rows[i - 1], &rows[i]);
        let close = matches!((a.1, b.1), (Some(x), Some(y)) if x.abs_diff(y) <= BURST_GAP as u64);
        let similar = close
            && match lightcraft_pipeline::cull::similarity_checked(&a.4, &b.4) {
                Ok(similarity) => similarity >= SAME,
                Err(e) => {
                    failed.push(json!([b.0.0, e.to_string()]));
                    false
                }
            };
        if similar {
            let g = *group_of[i - 1].get_or_insert_with(|| {
                next += 1;
                next - 1
            });
            group_of[i] = Some(g);
        }
    }
    let mut best: std::collections::HashMap<u32, (usize, f32)> = Default::default();
    for (i, g) in group_of.iter().enumerate() {
        if let Some(g) = g {
            let e = best.entry(*g).or_insert((i, -1.0));
            if rows[i].2 > e.1 {
                *e = (i, rows[i].2);
            }
        }
    }
    let mut ops = Vec::new();
    let (mut rejected, mut picked) = (0, 0);
    let mut out = Vec::new();
    for (i, (id, _, sharp, clip, _)) in rows.iter().enumerate() {
        let g = group_of[i];
        let is_best = g.is_some_and(|g| best.get(&g).is_some_and(|b| b.0 == i));
        ops.push(Op::SetAnalysis { id: *id, analysis: Some(Analysis { sharpness: *sharp, clipped: *clip, group: g, best: is_best }) });
        let flag = s.catalog.photo(*id).map(|p| p.flag).unwrap_or_default();
        if reject_below.is_some_and(|t| *sharp < t) && flag != Flag::Reject {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Reject });
            rejected += 1;
        } else if pick_best && is_best && flag == Flag::None {
            ops.push(Op::SetFlag { id: *id, flag: Flag::Pick });
            picked += 1;
        }
        out.push(json!({"id": id.0, "sharpness": (sharp * 10.0).round() / 10.0, "clipped": clip, "group": g, "best": is_best}));
    }
    if !ops.is_empty() {
        s.commit("Assisted Culling", Op::Batch { ops })?;
    }
    Ok(json!({"photos": out, "groups": best.len(), "rejected": rejected, "picked": picked, "failed": failed}))
}

/// Find Similar: photos that look like `id` (signatures from thumbnail-level sources, cached by
/// content), most similar first; the view is filtered to them.
fn find_similar(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findSimilar";
    let id = p.get("id").and_then(Value::as_u64).map(PhotoId).or(s.active()).ok_or_else(|| super::bad(C, "no photo"))?;
    let min = p.get("similarity").map_or(Ok(0.8), |v| {
        v.as_f64()
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            .map(|v| v as f32)
            .ok_or_else(|| super::bad(C, "`similarity` must be a finite number 0..1"))
    })?;
    let all: Vec<PhotoId> = s.catalog.photos().filter(|q| q.in_library()).map(|q| q.id).collect();
    let sig = |s: &mut Session, id: PhotoId| -> std::result::Result<[f32; 64], String> {
        let key = s.catalog.photo(id).map(|p| crate::media::content_key(p)).ok_or_else(|| "photo not found".to_string())?;
        if let Some(v) = s.signatures.get(&key) {
            return Ok(*v);
        }
        let src = s.source_now(id, crate::media::SourceLevel::Thumb).map_err(|e| e.to_string())?;
        let v = lightcraft_pipeline::cull::measure_checked(&src).map_err(|e| e.to_string())?.signature;
        s.signatures.insert(key, v);
        Ok(v)
    };
    let me = sig(s, id).map_err(|_| super::bad(C, "can't read the photo"))?;
    lightcraft_pipeline::cull::similarity_checked(&me, &me).map_err(|_| super::bad(C, "can't read the photo"))?;
    let mut hits: Vec<(PhotoId, f32)> = Vec::new();
    let mut failed = Vec::new();
    for q in all {
        match sig(s, q) {
            Ok(v) => match lightcraft_pipeline::cull::similarity_checked(&me, &v) {
                Ok(sim) if sim >= min => hits.push((q, sim)),
                Ok(_) => {}
                Err(e) => failed.push(json!([q.0, e.to_string()])),
            },
            Err(e) => failed.push(json!([q.0, e])),
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    if p.get("filter").and_then(Value::as_bool).unwrap_or(true) {
        s.filter.only = hits.iter().map(|h| h.0).collect();
    }
    let photos = hits.iter().map(|(i, v)| json!({"id": i.0, "similarity": (v * 1000.0).round() / 1000.0})).collect::<Vec<_>>();
    if failed.is_empty() { Ok(json!({"photos": photos})) } else { Ok(json!({"photos": photos, "failed": failed})) }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "library.findSimilar",
            "Find Similar Photos",
            ["Photo"],
            None,
            "{id?, similarity?: 0..1 (0.8), filter?: bool (true)} — photos that look like the active one (composition and tones), most similar first; filters the view to them (Clear Filters to go back) → {photos: [{id, similarity}]}",
            always,
            find_similar
        ),
        cmd!(
            "photo.analyze",
            "Assisted Culling",
            [],
            None,
            "{ids?, dryRun?: bool, rejectBelow?: sharpness 0..100, pickBest?: bool} — score photos for focus and clipping, group look-alikes and optionally set analysis/flags in one undo step; dryRun returns read-only proposals",
            always,
            analyze
        ),
        cmd!(
            "photo.cullApply",
            "Accept Culling Suggestions",
            [],
            None,
            "{proposal: cullSuggest result, accept: [{id, flag}]} — validate an unchanged proposal, accept explicit flags in one undo step → {accepted}",
            always,
            cull_apply
        ),
        cmd!(query "photo.cullSuggest",
            "Suggest Culling Flags",
            [],
            None,
            "{ids?, rejectBelow?: sharpness 0..100, pickBest?: bool} — read-only measurements and safe flag proposals; returns proposal for explicit cullApply → {photos, groups, rejected, picked, failed, proposal}",
            always,
            cull_suggest
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_result_keeps_raw_sharpness_at_reject_cutoff() {
        let mut session = Session::with_demo();
        let id = session.visible_cloned()[0];
        let job = session.plan_cull_job(&[id], Some(50.0), false).unwrap();
        let snapshot = &job.photos[0];
        let sharpness = 49.96_f32;
        let row = CullRow {
            id,
            file_name: snapshot.file_name.clone(),
            sharpness,
            clipped: 0.0,
            source: snapshot.source.clone(),
            flag: snapshot.flag,
            group: None,
            best: false,
            decision: lightcraft_pipeline::cull::report::Decision::Reject,
            uncertainty: lightcraft_pipeline::cull::report::Uncertainty::Low,
            reason_codes: vec![lightcraft_pipeline::cull::report::ReasonCode::BelowRejectThreshold],
        };
        let timing = timing_report(vec![photo_timing(id, Some(1.0), Some(2.0), 3.0, TimingStatus::Ok)], 0.5, 4.0);
        let value = proposal_result_revision(job.catalog_revision, &[row], Vec::new(), Some(50.0), false, timing);
        assert_eq!(value["proposal"]["photos"][0]["sharpness"].as_f64().map(|v| v as f32), Some(sharpness));
        assert_eq!(value["proposal"]["photos"][0]["proposedFlag"], "reject");
        let result = CullJobResult { catalog_revision: job.catalog_revision, value };
        validate_worker_result(&job, result.value).unwrap();
    }

    #[test]
    fn binding_distinguishes_absent_group_from_zero_and_rejects_tampering() {
        let mut session = Session::with_demo();
        let id = session.visible_cloned()[0];
        let job = session.plan_cull_job(&[id], None, false).unwrap();
        let snapshot = &job.photos[0];
        let row = |group| CullRow {
            id,
            file_name: snapshot.file_name.clone(),
            sharpness: 49.96,
            clipped: 0.0,
            source: snapshot.source.clone(),
            flag: snapshot.flag,
            group,
            best: false,
            decision: lightcraft_pipeline::cull::report::Decision::Abstain,
            uncertainty: lightcraft_pipeline::cull::report::Uncertainty::Abstain,
            reason_codes: vec![lightcraft_pipeline::cull::report::ReasonCode::Abstained],
        };
        let absent = row(None);
        let zero = row(Some(0));
        assert_ne!(
            binding_hash(job.catalog_revision, None, false, std::slice::from_ref(&absent)),
            binding_hash(job.catalog_revision, None, false, std::slice::from_ref(&zero))
        );

        let timing = timing_report(vec![photo_timing(id, Some(1.0), Some(2.0), 3.0, TimingStatus::Ok)], 0.5, 4.0);
        let result = proposal_result_revision(job.catalog_revision, std::slice::from_ref(&zero), Vec::new(), None, false, timing);
        let mut tampered = result.clone();
        tampered["photos"][0]["group"] = Value::Null;
        tampered["proposal"]["photos"][0]["group"] = Value::Null;
        tampered["groups"] = json!(0);
        let error = validate_worker_result(&job, tampered).unwrap_err().to_string();
        assert!(error.contains("binding"), "group presence tampering is rejected: {error}");
    }

    #[test]
    fn measurement_timing_covers_each_photo_with_finite_values_and_absent_stages() {
        let mut session = Session::with_demo();
        let ids = session.visible_cloned()[..2].to_vec();
        let job = session.plan_cull_job(&ids, None, true).unwrap();
        let result = job.run(&|_, _| true).unwrap();
        let timing = &result.value["measurementTiming"];
        assert_eq!(timing["schema"], TIMING_SCHEMA);
        assert_eq!(timing["classicalOnly"], true);
        assert_eq!(timing["sourcePolicy"], "uncached-origin-thumbnail");
        assert_eq!(timing["scope"], "measurement-and-planning");
        assert_eq!(
            timing["excludes"],
            serde_json::json!(["timingReceiptSerialization", "proposalSerialization", "proposalBinding", "dispatch", "import"])
        );
        assert_eq!(timing["perPhoto"].as_array().map(Vec::len), Some(ids.len()));
        assert!(timing["groupingMs"].as_f64().is_some_and(f64::is_finite));
        assert!(timing["jobTotalMs"].as_f64().is_some_and(f64::is_finite));
        for photo in timing["perPhoto"].as_array().unwrap() {
            let total = photo["totalMs"].as_f64().filter(|value| value.is_finite()).unwrap();
            let decode = photo["decodeMs"].as_f64().filter(|value| value.is_finite()).unwrap();
            let analysis = photo["analysisMs"].as_f64().filter(|value| value.is_finite()).unwrap();
            assert!(total >= decode);
            assert!(total >= analysis);
        }
        assert_eq!(timing["stages"]["crops"]["status"], "notApplicable");
        assert_eq!(timing["stages"]["inference"]["status"], "notApplicable");
    }

    #[test]
    fn measurement_timing_does_not_change_proposal_binding() {
        let mut session = Session::with_demo();
        let id = session.visible_cloned()[0];
        let job = session.plan_cull_job(&[id], Some(50.0), false).unwrap();
        let snapshot = &job.photos[0];
        let row = CullRow {
            id,
            file_name: snapshot.file_name.clone(),
            sharpness: 49.96,
            clipped: 0.0,
            source: snapshot.source.clone(),
            flag: snapshot.flag,
            group: None,
            best: false,
            decision: lightcraft_pipeline::cull::report::Decision::Reject,
            uncertainty: lightcraft_pipeline::cull::report::Uncertainty::Low,
            reason_codes: vec![lightcraft_pipeline::cull::report::ReasonCode::BelowRejectThreshold],
        };
        let first = proposal_result_revision(
            job.catalog_revision,
            std::slice::from_ref(&row),
            Vec::new(),
            Some(50.0),
            false,
            timing_report(vec![photo_timing(id, Some(1.0), Some(2.0), 3.0, TimingStatus::Ok)], 0.5, 4.0),
        );
        let second = proposal_result_revision(
            job.catalog_revision,
            std::slice::from_ref(&row),
            Vec::new(),
            Some(50.0),
            false,
            timing_report(vec![photo_timing(id, Some(9.0), Some(8.0), 17.0, TimingStatus::Ok)], 7.0, 18.0),
        );
        assert_eq!(first["proposal"]["binding"], second["proposal"]["binding"]);
    }

    #[test]
    fn decode_failure_retains_timing_without_an_analysis_sample() {
        let mut session = Session::with_demo();
        let id = session.visible_cloned()[0];
        let mut job = session.plan_cull_job(&[id], None, false).unwrap();
        job.photos[0].source_ref =
            crate::media::SourceRef::File { path: "synthetic-missing-source".into(), max_edge: 512, loader: None, fallback: None, denoise: None };
        let result = job.run(&|_, _| true).unwrap();
        let photo = &result.value["measurementTiming"]["perPhoto"][0];
        assert_eq!(photo["id"], id.0);
        assert_eq!(photo["status"], "decodeFailed");
        assert!(photo["analysisMs"].is_null());
        assert!(photo["totalMs"].as_f64().unwrap() >= photo["decodeMs"].as_f64().unwrap());
        assert_eq!(result.value["failed"].as_array().unwrap().len(), 1);
        assert!(result.value["proposal"]["photos"].as_array().unwrap().is_empty());
        validate_measurement_timing(&job, &result.value["measurementTiming"]).unwrap();
    }

    #[test]
    fn native_json_roundtrip_preserves_file_identity_f32_binding_and_apply_plan() {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("lightcraft-cull-transport-{}-{suffix}.raw", std::process::id()));
        std::fs::write(&path, b"owned cull transport fixture").unwrap();
        let path_string = path.to_string_lossy().into_owned();

        let mut session = Session::new();
        let id = PhotoId(9_001);
        let mut photo = lightcraft_catalog::Photo::new(
            id,
            lightcraft_catalog::Source::File { path: path_string.clone() },
            "native-transport.raw",
            "RAW",
            64,
            64,
            "2026-10-10T00:00:00",
        );
        photo.file_size = std::fs::metadata(&path).unwrap().len();
        session.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        session.catalog.revision = 1_u64 << 32;
        let loader_path = path_string.clone();
        session.media.file_loader = Some(std::sync::Arc::new(move |source, _| {
            if source != loader_path {
                return Err(format!("unexpected cull fixture source: {source}"));
            }
            Ok((lightcraft_raster::Rgb32f::filled(64, 64, [0.5, 0.5, 0.5]), lightcraft_pipeline::SourceInfo::default()))
        }));

        let source = session.catalog.photo(id).unwrap().clone();
        let source_identity_before = source_identity(&source);
        let source_again = session.catalog.photo(id).unwrap().clone();
        assert_eq!(source_identity(&source), source_identity(&source_again));
        let job = session.plan_cull_job(&[id], Some(100.0), true).unwrap();
        let snapshot = job.photos[0].clone();
        assert_eq!(snapshot.source, source_identity_before);

        let sharpness = 49.96_f32;
        let clipped = 0.125_f32;
        let row = CullRow {
            id,
            file_name: snapshot.file_name.clone(),
            sharpness,
            clipped,
            source: snapshot.source.clone(),
            flag: snapshot.flag,
            group: None,
            best: false,
            decision: lightcraft_pipeline::cull::report::Decision::Reject,
            uncertainty: lightcraft_pipeline::cull::report::Uncertainty::Low,
            reason_codes: vec![lightcraft_pipeline::cull::report::ReasonCode::BelowRejectThreshold],
        };
        let result = proposal_result_revision(
            job.catalog_revision,
            std::slice::from_ref(&row),
            Vec::new(),
            Some(100.0),
            true,
            timing_report(vec![photo_timing(id, Some(1.0), Some(2.0), 3.0, TimingStatus::Ok)], 0.5, 4.0),
        );
        let proposal = result["proposal"].clone();
        let binding = proposal["binding"].clone();
        let transport = serde_json::from_str::<Value>(
            &serde_json::to_string(&json!({
                "proposal": proposal,
                "accept": [{"id": id.0, "flag": "reject"}],
            }))
            .unwrap(),
        )
        .unwrap();
        let transported_photo = &transport["proposal"]["photos"][0];
        assert_eq!(transport["proposal"]["binding"], binding);
        assert_eq!(transport["proposal"]["catalogRevision"], 1_u64 << 32);
        assert_eq!(transported_photo["source"], source_identity_before);
        assert_eq!(transported_photo["group"], Value::Null);
        assert_eq!(transported_photo["proposedFlag"], "reject");
        assert_eq!((transported_photo["sharpness"].as_f64().unwrap() as f32).to_bits(), sharpness.to_bits());
        assert_eq!((transported_photo["clipped"].as_f64().unwrap() as f32).to_bits(), clipped.to_bits());

        let prepared = session.plan_cull_apply(&transport).expect("unchanged native-shaped proposal should plan");
        assert_eq!(prepared.job.catalog_revision, 1_u64 << 32);
        assert_eq!(prepared.job.photos[0].source, source_identity_before);
        let nested = json!({"proposal": {"proposal": transport["proposal"].clone(), "photos": []}, "accept": [{"id": id.0, "flag": "reject"}]});
        let nested_prepared = session.plan_cull_apply(&nested).expect("cullSuggest-shaped proposal should plan");
        assert_eq!(nested_prepared.job.catalog_revision, 1_u64 << 32);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn apply_canonicalizes_equivalent_wire_float_before_fresh_measurement_compare() {
        let mut session = Session::with_demo();
        let id = session.visible_cloned()[0];
        let suggestion = cull_suggest(&mut session, &json!({"ids": [id.0], "rejectBelow": 100.0})).unwrap();
        let mut proposal = suggestion["proposal"].clone();
        let sharpness = proposal["photos"][0]["sharpness"].as_f64().unwrap() as f32;
        let adjacent = if sharpness < 100.0 { f32::from_bits(sharpness.to_bits() + 1) } else { f32::from_bits(sharpness.to_bits() - 1) };
        let wire_value = f64::from(sharpness) + (f64::from(adjacent) - f64::from(sharpness)) * 0.25;
        assert_eq!((wire_value as f32).to_bits(), sharpness.to_bits());
        assert_ne!(wire_value, f64::from(sharpness));
        proposal["photos"][0]["sharpness"] = json!(wire_value);
        let prepared = session
            .plan_cull_apply(&json!({"proposal": proposal, "accept": [{"id": id.0, "flag": "reject"}]}))
            .expect("equivalent wire float remains applyable");
        let result = prepared.job.run(&|_, _| true).unwrap();
        assert_eq!(session.finish_cull_apply(prepared, result).unwrap()["accepted"], 1);
    }
}
