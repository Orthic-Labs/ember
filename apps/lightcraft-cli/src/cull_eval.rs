//! Bounded, read-only evaluation of culling decisions.
//!
//! The evaluator deliberately consumes shoot-level labels.  An unpicked frame is not
//! treated as a rejection unless its label says so explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

const REPORT_VERSION: u64 = 1;
const REPORT_SCHEMA: &str = "lightcraft.cull-eval.v1";
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SHOOTS: usize = 100_000;
const MAX_FRAMES: usize = 1_000_000;
const MAX_ID: usize = 256;
const MAX_FILES: usize = 10_000;
const MAX_LABEL_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Label {
    Keep,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Keep,
    Reject,
    Abstain,
}

#[derive(Clone, Debug, Default)]
struct Frame {
    id: String,
    label: Option<Label>,
    decision: Option<Decision>,
    burst: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Shoot {
    id: String,
    split: Option<String>,
    frames: BTreeMap<String, Frame>,
    acceptable: BTreeSet<String>,
    reject: BTreeSet<String>,
    explicit_unknown: BTreeSet<String>,
    winners: BTreeSet<String>,
    bursts: BTreeMap<String, Burst>,
    elapsed_ms: Option<f64>,
}

#[derive(Clone, Debug, Default)]
struct Burst {
    id: String,
    members: BTreeSet<String>,
    acceptable: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct Dataset {
    shoots: BTreeMap<String, Shoot>,
    total_frames: usize,
    flat_decisions: BTreeMap<String, Decision>,
    flat_winners: BTreeSet<String>,
    flat_winner_bursts: BTreeMap<String, String>,
    flat_split: Option<String>,
    flat_elapsed_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputKind {
    Labels,
    Predictions,
}

/// Evaluate bounded predictions against explicit shoot-level labels.
///
/// Invocation:
/// `cull score --predictions PREDICTIONS.json --labels LABELS.json [--out REPORT.json] [--split eval]`
/// Positional `score PREDICTIONS.json LABELS.json` is accepted as well.
/// `cull baseline --manifest MANIFEST.json [--out REPORT.json]` runs the local
/// analyzer in a fresh in-memory session over explicitly listed files.
pub fn run(args: &[String]) -> Result<(), String> {
    let mode = args.first().map(String::as_str).ok_or("cull expects score or baseline")?;
    match mode {
        "score" => run_score(&args[1..]),
        "baseline" => run_baseline(&args[1..]),
        "--help" | "-h" => {
            println!("cull score --predictions P --labels L [--out REPORT] [--split NAME]");
            println!("cull baseline --manifest M [--reject-below 0..100] [--out REPORT]");
            Ok(())
        }
        _ => Err("cull expects score or baseline".into()),
    }
}

fn run_score(args: &[String]) -> Result<(), String> {
    let mut predictions = None;
    let mut labels = None;
    let mut out = None;
    let mut split = None;
    let mut positional = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args.get(i).map(String::as_str).unwrap_or("");
        match arg {
            "--predictions" | "--prediction" => predictions = Some(next_arg(args, &mut i, arg)?),
            "--labels" | "--label" => labels = Some(next_arg(args, &mut i, arg)?),
            "--out" | "-o" => out = Some(PathBuf::from(next_arg(args, &mut i, arg)?)),
            "--split" => split = Some(next_arg(args, &mut i, arg)?),
            "--help" | "-h" => {
                println!("cull score --predictions P --labels L [--out REPORT] [--split NAME]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unknown cull option {value}")),
            value => positional.push(value.to_owned()),
        }
        i += 1;
    }
    if predictions.is_none() && !positional.is_empty() {
        predictions = positional.first().cloned();
    }
    if labels.is_none() && positional.len() > 1 {
        labels = positional.get(1).cloned();
    }
    if positional.len() > 2 {
        return Err("score accepts exactly two positional inputs".into());
    }
    let prediction_path = predictions.ok_or("score needs --predictions and --labels")?;
    let label_path = labels.ok_or("score needs --predictions and --labels")?;
    let prediction_value = read_json(Path::new(&prediction_path), "predictions")?;
    let label_value = read_json(Path::new(&label_path), "labels")?;
    let predictions = parse_dataset(&prediction_value, InputKind::Predictions)?;
    let labels = parse_dataset(&label_value, InputKind::Labels)?;
    let report = score(&predictions, &labels, split.as_deref())?;
    emit_report(&report, out.as_deref())
}

fn run_baseline(args: &[String]) -> Result<(), String> {
    let mut manifest = None;
    let mut out = None;
    let mut reject_below = None;
    let mut i = 0usize;
    while i < args.len() {
        let arg = args.get(i).map(String::as_str).unwrap_or("");
        match arg {
            "--manifest" => manifest = Some(next_arg(args, &mut i, arg)?),
            "--out" | "-o" => out = Some(PathBuf::from(next_arg(args, &mut i, arg)?)),
            "--reject-below" => {
                i = i.saturating_add(1);
                let raw = args.get(i).ok_or("--reject-below: missing value")?;
                let value = raw.parse::<f64>().map_err(|_| "--reject-below expects a finite number 0..100")?;
                if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                    return Err("--reject-below expects a finite number 0..100".into());
                }
                reject_below = Some(value);
            }
            "--help" | "-h" => {
                println!("cull baseline --manifest M [--reject-below 0..100] [--out REPORT]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unknown cull option {value}")),
            _ => return Err("baseline requires --manifest".into()),
        }
        i += 1;
    }
    let manifest_path = manifest.ok_or("baseline requires --manifest")?;
    let value = read_json(Path::new(&manifest_path), "manifest")?;
    let manifest = parse_manifest(&value)?;
    if manifest.files.is_empty() {
        return Err("baseline manifest has no explicit files".into());
    }
    if manifest.files.len() > MAX_FILES {
        return Err("baseline manifest exceeds file count cap".into());
    }

    // `Session::new` is an in-memory catalog. `with_fs` only enables decoding of
    // explicitly supplied files; no catalog path is opened or persisted.
    let started = Instant::now();
    let mut session = lightcraft_engine::Session::new().with_fs();
    let mut id_by_key = BTreeMap::new();
    for (path, key) in manifest.files.iter().zip(&manifest.file_keys) {
        let import_path = path.to_string_lossy().into_owned();
        let imported =
            session.execute("library.import", &json!({"paths": [import_path]})).map_err(|_| "baseline could not import explicit input file")?;
        let ids = imported.get("imported").and_then(Value::as_array).ok_or("baseline import returned no photo id")?;
        if ids.len() != 1 {
            return Err("baseline import did not return exactly one photo id".into());
        }
        let value = ids.first().ok_or("baseline import returned no photo id")?;
        let Some(id) = value.as_u64() else {
            return Err("baseline import returned invalid photo id".into());
        };
        id_by_key.insert(key.clone(), id);
    }

    let mut shoot_values = Vec::new();
    let mut engine_reports = Vec::new();
    for shoot in &manifest.shoots {
        let mut photo_ids = Vec::new();
        for frame in &shoot.frames {
            let Some(id) = id_by_key.get(&frame.file_key) else {
                return Err("baseline shoot references file outside manifest".into());
            };
            photo_ids.push(*id);
        }
        let measured = session
            .execute("photo.cullSuggest", &json!({"ids": photo_ids, "pickBest": true, "rejectBelow": reject_below}))
            .map_err(|_| "baseline cull analysis failed")?;
        let mut frames = Vec::new();
        let mut winners = Vec::new();
        let rows = measured.get("photos").and_then(Value::as_array).ok_or("baseline cull report has no photos")?;
        if rows.len() != shoot.frames.len() || measured.get("failed").and_then(Value::as_array).is_some_and(|failed| !failed.is_empty()) {
            return Err("baseline cull report does not cover every manifest photo".into());
        }
        let mut by_engine_id = BTreeMap::new();
        for frame in &shoot.frames {
            if let Some(id) = id_by_key.get(&frame.file_key) {
                by_engine_id.insert(*id, frame.id.clone());
            }
        }
        for row in rows {
            let id = row.get("id").and_then(Value::as_u64).ok_or("baseline cull report has invalid photo id")?;
            let frame_id = by_engine_id.get(&id).ok_or("baseline cull report photo is outside manifest")?;
            let decision = row.get("decision").cloned().unwrap_or_else(|| Value::String("abstain".into()));
            if decision.as_str() == Some("pick") {
                winners.push(frame_id.clone());
            }
            frames.push(json!({"id": frame_id, "decision": decision, "burstId": row.get("group").cloned().unwrap_or(Value::Null)}));
        }
        shoot_values.push(json!({"shootId": &shoot.id, "split": &shoot.split, "frames": frames, "winnerIds": winners, "groups": measured.get("groups").cloned().unwrap_or(Value::Null)}));
        engine_reports.push(json!({"shootId": &shoot.id, "report": measured}));
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stage_summary = crate::cull_timing::summarize(&engine_reports, manifest.files.len())?;
    let report = json!({
        "version": REPORT_VERSION,
        "schema": REPORT_SCHEMA,
        "mode": "baseline",
        "source": "ephemeral-session",
        "libraryMutated": false,
        "policy": {"rejectBelow": reject_below, "pickBest": true},
        "inputFileCount": manifest.files.len(),
        "timing": {"elapsedMs": elapsed_ms, "includesImportAndDecode": true, "status": "stage-only", "stageSummary": stage_summary},
        "shoots": shoot_values,
        "engineReports": engine_reports,
        "elapsedMs": elapsed_ms,
        "limits": {"maxInputBytes": MAX_BYTES, "maxShoots": MAX_SHOOTS, "maxFrames": MAX_FRAMES}
    });
    emit_report(&report, out.as_deref())
}

fn next_arg(args: &[String], index: &mut usize, option: &str) -> Result<String, String> {
    *index = index.saturating_add(1);
    args.get(*index).filter(|v| !v.starts_with('-')).cloned().ok_or_else(|| format!("{option}: missing value"))
}

fn read_json(path: &Path, kind: &str) -> Result<Value, String> {
    let file = std::fs::File::open(path).map_err(|_| format!("could not read {kind} input"))?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| format!("could not read {kind} input"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{kind} input exceeds {MAX_BYTES} byte cap"));
    }
    serde_json::from_slice(&bytes).map_err(|_| format!("{kind} input is not valid JSON"))
}

fn emit_report(report: &Value, out: Option<&Path>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|_| "could not encode cull evaluation report")?;
    if let Some(path) = out {
        crate::output_file::replace(path, &bytes).map_err(|_| "could not write cull evaluation report")?;
        println!("{}", json!({"report": path.to_string_lossy(), "version": REPORT_VERSION, "schema": REPORT_SCHEMA}));
    } else {
        println!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(())
}

fn parse_dataset(value: &Value, kind: InputKind) -> Result<Dataset, String> {
    if kind == InputKind::Labels {
        validate_label_manifest_header(value)?;
    }
    if kind == InputKind::Predictions && value.get("predictions").is_some() {
        return parse_flat_predictions(value);
    }
    let shoots = shoot_values(value)?;
    if shoots.len() > MAX_SHOOTS {
        return Err("input exceeds shoot count cap".into());
    }
    let mut result = Dataset::default();
    for (index, value) in shoots.iter().enumerate() {
        let shoot = parse_shoot(value, kind, index)?;
        if result.shoots.insert(shoot.id.clone(), shoot).is_some() {
            return Err("duplicate shootId".into());
        }
    }
    result.total_frames = result.shoots.values().map(|s| s.frames.len()).sum();
    if result.total_frames > MAX_FRAMES {
        return Err("input exceeds frame count cap".into());
    }
    let mut frame_owners = BTreeSet::new();
    for shoot in result.shoots.values() {
        for frame_id in shoot.frames.keys() {
            if !frame_owners.insert(frame_id.clone()) {
                return Err("duplicate frame ID across shoots".into());
            }
        }
    }
    let mut burst_owners = BTreeMap::new();
    let mut burst_members = BTreeMap::new();
    for shoot in result.shoots.values() {
        for burst in shoot.bursts.values() {
            if burst_owners.insert(burst.id.clone(), shoot.id.clone()).is_some() {
                return Err("duplicate burst ID across shoots".into());
            }
            for frame_id in &burst.members {
                if burst_members.insert(frame_id.clone(), burst.id.clone()).is_some() {
                    return Err("photo belongs to multiple bursts".into());
                }
            }
        }
        for frame in shoot.frames.values() {
            if let Some(burst_id) = &frame.burst {
                let Some(burst) = shoot.bursts.get(burst_id) else {
                    return Err("photo burst_id is not listed in shoot bursts".into());
                };
                if !burst.members.contains(&frame.id) {
                    return Err("photo is missing from declared burst membership".into());
                }
            }
        }
    }
    validate_splits(value, &result)?;
    validate_bursts(value, &result)?;
    Ok(result)
}

fn validate_label_manifest_header(value: &Value) -> Result<(), String> {
    let Some(object) = value.as_object() else { return Ok(()) };
    if let Some(version) = object.get("version")
        && version.as_u64() != Some(1)
    {
        return Err("labels version is unsupported".into());
    }
    if let Some(fixture_kind) = object.get("fixture_kind").and_then(Value::as_str)
        && !matches!(fixture_kind, "SYNTHETIC" | "CONSENTED")
    {
        return Err("labels fixture_kind is unsupported".into());
    }
    Ok(())
}

fn parse_flat_predictions(value: &Value) -> Result<Dataset, String> {
    let object = value.as_object().ok_or("predictions root must be an object")?;
    if object.get("bursts").is_some() {
        return Err("root-level bursts are unsupported; nest bursts under their shoot".into());
    }
    let split = text(object, &["split", "partition"]);
    validate_split(split.as_deref())?;
    let rows = object.get("predictions").and_then(Value::as_array).ok_or("predictions must be an array")?;
    if rows.len() > MAX_FRAMES {
        return Err("input exceeds frame count cap".into());
    }
    let mut dataset = Dataset::default();
    for row in rows {
        let item = row.as_object().ok_or("prediction must be an object")?;
        let id = text(item, &["photo_id", "photoId", "id", "frameId"]).ok_or("prediction is missing photo_id")?;
        valid_id(&id, "photo ID")?;
        if dataset.flat_decisions.contains_key(&id) {
            return Err("duplicate prediction photo ID".into());
        }
        let decision =
            decision_from_value(item.get("decision").or_else(|| item.get("status")).or_else(|| item.get("label"))).unwrap_or(Decision::Abstain);
        dataset.flat_decisions.insert(id, decision);
    }
    if let Some(winners) = object.get("winner_predictions") {
        let winners = winners.as_array().ok_or("winner_predictions must be an array")?;
        if winners.len() > MAX_FRAMES {
            return Err("winner_predictions exceeds frame count cap".into());
        }
        for row in winners {
            let item = row.as_object().ok_or("winner prediction must be an object")?;
            let id = text(item, &["selected_winner_id", "selectedWinnerId", "winnerId", "photo_id"])
                .ok_or("winner prediction is missing selected_winner_id")?;
            valid_id(&id, "winner ID")?;
            if !dataset.flat_winners.insert(id.clone()) {
                return Err("duplicate selected winner ID".into());
            }
            if let Some(burst_id) = text(item, &["burst_id", "burstId"]) {
                valid_id(&burst_id, "burst ID")?;
                if dataset.flat_winner_bursts.values().any(|selected| selected == &burst_id) {
                    return Err("multiple selected winners in burst".into());
                }
                if dataset.flat_winner_bursts.insert(id, burst_id).is_some() {
                    return Err("duplicate selected winner ID".into());
                }
            }
        }
    }
    dataset.flat_split = split;
    dataset.flat_elapsed_ms = number(object, &["elapsedMs", "elapsed_ms", "latencyMs", "durationMs"])?;
    Ok(dataset)
}

fn shoot_values(value: &Value) -> Result<Vec<Value>, String> {
    if let Some(rows) = value.as_array() {
        return Ok(rows.clone());
    }
    let Some(object) = value.as_object() else {
        return Err("input root must be an object or shoot array".into());
    };
    if let Some(rows) = object.get("shoots").and_then(Value::as_array) {
        return Ok(rows.clone());
    }
    // A split-keyed fixture is accepted, while preserving explicit split metadata.
    let mut rows = Vec::new();
    for split in ["train", "validation", "dev", "eval", "test"] {
        if let Some(items) = object.get(split).and_then(Value::as_array) {
            for item in items {
                let mut item = item.clone();
                if let Some(item_object) = item.as_object_mut() {
                    item_object.entry("split").or_insert_with(|| Value::String(split.to_owned()));
                }
                rows.push(item);
            }
        }
    }
    if rows.is_empty() {
        return Err("input has no shoots".into());
    }
    Ok(rows)
}

fn parse_shoot(value: &Value, kind: InputKind, index: usize) -> Result<Shoot, String> {
    let object = value.as_object().ok_or_else(|| format!("shoot {index} must be an object"))?;
    let id = text(object, &["shootId", "shoot_id", "id"]).ok_or_else(|| format!("shoot {index} is missing shootId"))?;
    valid_id(&id, "shootId")?;
    let split = text(object, &["split", "partition"]);
    validate_split(split.as_deref())?;
    let raw_frames = object
        .get("frames")
        .or_else(|| object.get("photos"))
        .or_else(|| object.get("frameIds"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("shoot {index} is missing frames"))?;
    if raw_frames.len() > MAX_FRAMES {
        return Err("shoot frame count exceeds cap".into());
    }
    let mut shoot = Shoot { id, split, ..Shoot::default() };
    for (frame_index, raw) in raw_frames.iter().enumerate() {
        let (frame_id, frame_label, frame_unknown, frame_decision, burst, winner_marker) = parse_frame(raw, kind, frame_index)?;
        if shoot.frames.contains_key(&frame_id) {
            return Err("duplicate frame ID within shoot".into());
        }
        if kind == InputKind::Predictions && winner_marker {
            shoot.winners.insert(frame_id.clone());
        }
        if frame_unknown {
            shoot.explicit_unknown.insert(frame_id.clone());
        }
        shoot.frames.insert(frame_id.clone(), Frame { id: frame_id, label: frame_label, decision: frame_decision, burst });
    }
    let keep_keys = if kind == InputKind::Labels {
        &["acceptableWinnerIds", "acceptableWinners", "winnerIds", "winners", "picks"][..]
    } else {
        &["winnerIds", "winners", "pickedIds", "selectedIds"][..]
    };
    let reject_keys = &["rejectIds", "rejectedIds", "rejects"];
    let abstain_keys = &["abstainIds", "abstentions"];
    shoot.acceptable = ids_from_keys(object, keep_keys)?;
    shoot.reject = ids_from_keys(object, reject_keys)?;
    if kind == InputKind::Predictions {
        shoot.winners.extend(shoot.acceptable.clone());
        for id in shoot.acceptable.clone() {
            set_decision(&mut shoot, &id, Decision::Keep)?;
        }
        for id in shoot.reject.clone() {
            set_decision(&mut shoot, &id, Decision::Reject)?;
        }
        for id in ids_from_keys(object, abstain_keys)? {
            set_decision(&mut shoot, &id, Decision::Abstain)?;
        }
    }
    let frame_annotations: Vec<(String, Option<Label>, Option<Decision>)> =
        shoot.frames.values().map(|frame| (frame.id.clone(), frame.label, frame.decision)).collect();
    for (frame_id, label, decision) in frame_annotations {
        if let Some(label) = label
            && kind == InputKind::Labels
        {
            match label {
                Label::Keep => {
                    shoot.acceptable.insert(frame_id.clone());
                }
                Label::Reject => {
                    shoot.reject.insert(frame_id.clone());
                }
            }
        }
        if let Some(decision) = decision
            && kind == InputKind::Predictions
        {
            set_decision(&mut shoot, &frame_id, decision)?;
        }
    }
    parse_shoot_bursts(object, &mut shoot)?;
    validate_membership(&shoot)?;
    if kind == InputKind::Predictions {
        validate_prediction_winners(&shoot)?;
    }
    shoot.elapsed_ms = number(object, &["elapsedMs", "elapsed_ms", "latencyMs", "durationMs"])?;
    Ok(shoot)
}

fn parse_frame(
    value: &Value,
    kind: InputKind,
    index: usize,
) -> Result<(String, Option<Label>, bool, Option<Decision>, Option<String>, bool), String> {
    if let Some(id) = value.as_str() {
        valid_id(id, "frame ID")?;
        return Ok((id.to_owned(), None, false, None, None, false));
    }
    let object = value.as_object().ok_or_else(|| format!("frame {index} must be a string or object"))?;
    let id = text(object, &["id", "frameId", "frame_id", "photoId", "photo_id"]).ok_or("frame is missing id")?;
    valid_id(&id, "frame ID")?;
    let (label, explicit_unknown) = if kind == InputKind::Labels { explicit_label(object)? } else { (None, false) };
    let decision = if kind == InputKind::Predictions {
        decision_from_value(object.get("decision").or_else(|| object.get("status")).or_else(|| object.get("label")))
            .or_else(|| bool_decision(object, &["accepted", "acceptable", "keep", "picked"], Decision::Keep))
            .or_else(|| bool_decision(object, &["reject", "rejected"], Decision::Reject))
    } else {
        None
    };
    let burst = value_text(object, &["burstId", "burst_id", "group"]);
    let winner_marker = kind == InputKind::Predictions
        && winner_marker_value(object.get("decision").or_else(|| object.get("status")).or_else(|| object.get("label")));
    Ok((id, label, explicit_unknown, decision, burst, winner_marker))
}

fn parse_shoot_bursts(object: &serde_json::Map<String, Value>, shoot: &mut Shoot) -> Result<(), String> {
    let Some(raw_bursts) = object.get("bursts") else { return Ok(()) };
    let Some(raw_bursts) = raw_bursts.as_array() else { return Err("shoot bursts must be an array".into()) };
    for raw in raw_bursts {
        let burst_object = raw.as_object().ok_or("burst must be an object")?;
        let id = text(burst_object, &["burstId", "burst_id", "id"]).ok_or("burst is missing burst_id")?;
        valid_id(&id, "burst ID")?;
        if shoot.bursts.contains_key(&id) {
            return Err("duplicate burst ID within shoot".into());
        }
        let raw_members = burst_object
            .get("photo_ids")
            .or_else(|| burst_object.get("photoIds"))
            .or_else(|| burst_object.get("frameIds"))
            .and_then(Value::as_array)
            .ok_or("burst is missing photo_ids")?;
        let mut members = BTreeSet::new();
        for value in raw_members {
            let member = value.as_str().ok_or("burst contains invalid photo ID")?;
            valid_id(member, "photo ID")?;
            if !members.insert(member.to_owned()) {
                return Err("duplicate photo ID within burst".into());
            }
            if !shoot.frames.contains_key(member) {
                return Err("burst references photo outside shoot".into());
            }
        }
        let acceptable_values =
            burst_object.get("acceptable_winners").or_else(|| burst_object.get("acceptableWinners")).or_else(|| burst_object.get("winnerIds"));
        let acceptable = if let Some(values) = acceptable_values {
            let values = values.as_array().ok_or("acceptable_winners must be an array")?;
            let mut ids = BTreeSet::new();
            for value in values {
                let winner = value.as_str().ok_or("acceptable_winners contains invalid photo ID")?;
                if !members.contains(winner) {
                    return Err("acceptable winner is outside burst".into());
                }
                if !ids.insert(winner.to_owned()) {
                    return Err("duplicate acceptable winner".into());
                }
            }
            ids
        } else {
            BTreeSet::new()
        };
        if acceptable.iter().any(|winner| shoot.reject.contains(winner)) {
            return Err("acceptable winner has reject label".into());
        }
        shoot.bursts.insert(id.clone(), Burst { id, members, acceptable });
    }
    Ok(())
}

fn text(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| object.get(*key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned))
}

fn value_text(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| match value {
            Value::String(value) if !value.trim().is_empty() => Some(value.trim().to_owned()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
    })
}

fn ids_from_keys(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for key in keys {
        let Some(values) = object.get(*key) else { continue };
        let Some(values) = values.as_array() else { return Err(format!("{key} must be an array")) };
        for value in values {
            let id = value.as_str().ok_or_else(|| format!("{key} contains invalid ID"))?.trim();
            valid_id(id, "frame ID")?;
            if !ids.insert(id.to_owned()) {
                return Err("duplicate frame ID in decision list".into());
            }
        }
    }
    Ok(ids)
}

fn explicit_label(object: &serde_json::Map<String, Value>) -> Result<(Option<Label>, bool), String> {
    let mut seen = false;
    let mut label = None;
    for key in ["ground_truth", "groundTruth", "label", "decision", "status"] {
        let Some(value) = object.get(key) else { continue };
        let parsed = label_value(value).map_err(|_| format!("{key} has invalid label"))?;
        if seen && label != parsed {
            return Err("conflicting labels for frame".into());
        }
        seen = true;
        label = parsed;
    }
    for (key, bool_label_value, bool_label_kind) in [
        ("acceptable", Label::Keep, "keep"),
        ("accepted", Label::Keep, "keep"),
        ("keep", Label::Keep, "keep"),
        ("picked", Label::Keep, "keep"),
        ("reject", Label::Reject, "reject"),
        ("rejected", Label::Reject, "reject"),
    ] {
        let Some(value) = object.get(key) else { continue };
        let Some(value) = value.as_bool() else { return Err(format!("{key} must be boolean")) };
        if !value {
            continue;
        }
        if seen && label != Some(bool_label_value) {
            return Err(format!("{key} conflicts with {bool_label_kind} label"));
        }
        seen = true;
        label = Some(bool_label_value);
    }
    Ok((label, seen && label.is_none()))
}

fn label_value(value: &Value) -> Result<Option<Label>, ()> {
    enum Task<'a> {
        Visit(&'a Value, usize),
        Combine { values: Vec<&'a Value>, next: usize, labels: Vec<Option<Label>>, depth: usize },
    }

    let mut tasks = vec![Task::Visit(value, 0)];
    let mut result = None;
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(value, depth) => {
                if depth > MAX_LABEL_DEPTH {
                    result = Some(Err(()));
                    continue;
                }
                match value {
                    Value::String(value) => {
                        result = Some(match value.trim().to_ascii_lowercase().as_str() {
                            "keep" | "accept" | "accepted" | "pick" | "picked" | "winner" | "selected" => Ok(Some(Label::Keep)),
                            "reject" | "rejected" | "discard" => Ok(Some(Label::Reject)),
                            "unknown" | "abstain" | "review" | "unlabeled" => Ok(None),
                            _ => Err(()),
                        });
                    }
                    Value::Object(value) => {
                        let values: Vec<&Value> =
                            ["ground_truth", "groundTruth", "label", "decision", "status"].iter().filter_map(|key| value.get(*key)).collect();
                        let Some(first) = values.first().copied() else {
                            result = Some(Err(()));
                            continue;
                        };
                        tasks.push(Task::Combine { values, next: 0, labels: Vec::new(), depth });
                        tasks.push(Task::Visit(first, depth.saturating_add(1)));
                    }
                    _ => result = Some(Err(())),
                }
            }
            Task::Combine { values, next, mut labels, depth } => {
                let Some(Ok(label)) = result.take() else {
                    result = Some(Err(()));
                    continue;
                };
                labels.push(label);
                let next_index = next.saturating_add(1);
                let Some(next_value) = values.get(next_index).copied() else {
                    result = Some(if labels.iter().all(|candidate| *candidate == label) { Ok(label) } else { Err(()) });
                    continue;
                };
                tasks.push(Task::Combine { values, next: next_index, labels, depth });
                tasks.push(Task::Visit(next_value, depth.saturating_add(1)));
            }
        }
    }
    match result {
        Some(value) => value,
        None => Err(()),
    }
}

fn decision_from_value(value: Option<&Value>) -> Option<Decision> {
    match value {
        Some(Value::String(v))
            if matches!(v.to_ascii_lowercase().as_str(), "keep" | "accept" | "accepted" | "pick" | "picked" | "winner" | "selected" | "best") =>
        {
            Some(Decision::Keep)
        }
        Some(Value::String(v)) if matches!(v.to_ascii_lowercase().as_str(), "reject" | "rejected" | "discard") => Some(Decision::Reject),
        Some(Value::String(v)) if matches!(v.to_ascii_lowercase().as_str(), "abstain" | "unknown" | "review" | "unlabeled") => {
            Some(Decision::Abstain)
        }
        Some(Value::Object(v)) => decision_from_value(v.get("decision").or_else(|| v.get("label")).or_else(|| v.get("status"))),
        _ => None,
    }
}

fn winner_marker_value(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(v)) => matches!(v.to_ascii_lowercase().as_str(), "pick" | "picked" | "winner" | "selected" | "best"),
        Some(Value::Object(v)) => winner_marker_value(v.get("decision").or_else(|| v.get("label")).or_else(|| v.get("status"))),
        _ => false,
    }
}

fn bool_decision(object: &serde_json::Map<String, Value>, keys: &[&str], decision: Decision) -> Option<Decision> {
    keys.iter().find_map(|key| object.get(*key).and_then(Value::as_bool).filter(|value| *value).map(|_| decision))
}

fn number(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Result<Option<f64>, String> {
    let Some(value) = keys.iter().find_map(|key| object.get(*key)) else { return Ok(None) };
    let Some(value) = value.as_f64() else { return Err("elapsed time must be a number".into()) };
    if !value.is_finite() || value < 0.0 || value > 86_400_000.0 {
        return Err("elapsed time is outside bounds".into());
    }
    Ok(Some(value))
}

fn valid_id(id: &str, field: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_ID || id.chars().any(char::is_control) {
        return Err(format!("invalid {field}"));
    }
    Ok(())
}

fn validate_split(split: Option<&str>) -> Result<(), String> {
    let Some(split) = split else { return Err("shoot is missing explicit split".into()) };
    if !matches!(split, "train" | "validation" | "dev" | "eval" | "test") {
        return Err("shoot has invalid split".into());
    }
    Ok(())
}

fn set_decision(shoot: &mut Shoot, id: &str, decision: Decision) -> Result<(), String> {
    let Some(frame) = shoot.frames.get_mut(id) else { return Err("decision references frame outside shoot".into()) };
    if frame.decision.is_some_and(|old| old != decision) {
        return Err("conflicting decisions for frame".into());
    }
    frame.decision = Some(decision);
    Ok(())
}

fn validate_membership(shoot: &Shoot) -> Result<(), String> {
    for id in shoot.acceptable.iter().chain(shoot.reject.iter()).chain(shoot.winners.iter()) {
        if !shoot.frames.contains_key(id) {
            return Err("decision references frame outside shoot".into());
        }
    }
    if shoot.acceptable.iter().any(|id| shoot.reject.contains(id)) {
        return Err("frame has conflicting explicit labels".into());
    }
    if shoot.explicit_unknown.iter().any(|id| shoot.acceptable.contains(id) || shoot.reject.contains(id)) {
        return Err("explicit unknown conflicts with another label".into());
    }
    Ok(())
}

fn validate_prediction_winners(shoot: &Shoot) -> Result<(), String> {
    let mut selected_by_burst = BTreeSet::new();
    for winner in &shoot.winners {
        for burst in shoot.bursts.values().filter(|burst| burst.members.contains(winner)) {
            if !selected_by_burst.insert(burst.id.clone()) {
                return Err("multiple selected winners in burst".into());
            }
        }
    }
    Ok(())
}

fn validate_bursts(root: &Value, dataset: &Dataset) -> Result<(), String> {
    let mut burst_frames: BTreeMap<String, String> = BTreeMap::new();
    for shoot in dataset.shoots.values() {
        for frame in shoot.frames.values() {
            if let Some(burst) = &frame.burst {
                valid_id(burst, "burst ID")?;
                if let Some(previous) = burst_frames.insert(burst.clone(), shoot.id.clone())
                    && previous != shoot.id
                {
                    return Err("burst membership crosses shoots".into());
                }
            }
        }
    }
    if root.get("bursts").is_some() {
        return Err("root-level bursts are unsupported; nest bursts under their shoot".into());
    }
    Ok(())
}

fn validate_splits(root: &Value, dataset: &Dataset) -> Result<(), String> {
    let Some(object) = root.as_object() else { return Ok(()) };
    let mut owners = BTreeMap::new();
    for (key, expected_split) in
        [("trainShootIds", "train"), ("validationShootIds", "validation"), ("devShootIds", "dev"), ("evalShootIds", "eval"), ("testShootIds", "test")]
    {
        for shoot_id in id_set(object.get(key))? {
            if owners.insert(shoot_id.clone(), expected_split).is_some() {
                return Err("split metadata leakage".into());
            }
            let Some(shoot) = dataset.shoots.get(&shoot_id) else {
                return Err("split references unknown shootId".into());
            };
            if shoot.split.as_deref() != Some(expected_split) {
                return Err("split metadata disagrees with shoot split".into());
            }
        }
    }
    let mut frame_splits: BTreeMap<String, String> = BTreeMap::new();
    for shoot in dataset.shoots.values() {
        if let Some(split) = &shoot.split
            && !split.is_empty()
        {
            for id in shoot.frames.keys() {
                if let Some(previous) = frame_splits.insert(id.clone(), split.clone())
                    && previous != *split
                {
                    return Err("frame appears in multiple split shoots".into());
                }
            }
        }
    }
    Ok(())
}

fn id_set(value: Option<&Value>) -> Result<BTreeSet<String>, String> {
    let Some(value) = value else { return Ok(BTreeSet::new()) };
    let Some(values) = value.as_array() else { return Err("split shoot IDs must be arrays".into()) };
    let mut result = BTreeSet::new();
    for item in values {
        let id = item.as_str().ok_or("split shoot IDs must be strings")?;
        valid_id(id, "shootId")?;
        if !result.insert(id.to_owned()) {
            return Err("duplicate split shoot ID".into());
        }
    }
    Ok(result)
}

fn validate_prediction_splits(predictions: &Dataset, labels: &Dataset, split: Option<&str>) -> Result<(), String> {
    if let Some(prediction_split) = predictions.flat_split.as_deref() {
        if split.is_some_and(|wanted| wanted != prediction_split) {
            return Err("flat prediction split does not match requested split".into());
        }
        for id in predictions.flat_decisions.keys().chain(predictions.flat_winners.iter()) {
            let Some(label_shoot) = labels.shoots.values().find(|shoot| shoot.frames.contains_key(id)) else {
                return Err("prediction references photo outside labeled shoots".into());
            };
            if label_shoot.split.as_deref() != Some(prediction_split) {
                return Err("flat prediction split disagrees with labeled shoot split".into());
            }
        }
    }
    for prediction in predictions.shoots.values() {
        let Some(label) = labels.shoots.get(&prediction.id) else {
            return Err("prediction references unknown shootId".into());
        };
        if prediction.split != label.split {
            return Err("prediction split disagrees with labeled shoot split".into());
        }
    }
    Ok(())
}

fn validate_winner_cardinality(predictions: &Dataset, labels: &Dataset, split: Option<&str>) -> Result<(), String> {
    for label in labels.shoots.values() {
        if split.is_some_and(|wanted| label.split.as_deref() != Some(wanted)) {
            continue;
        }
        let prediction = predictions.shoots.get(&label.id);
        for burst in label.bursts.values() {
            let selected = prediction
                .into_iter()
                .flat_map(|shoot| shoot.winners.iter())
                .chain(predictions.flat_winners.iter())
                .filter(|id| burst.members.contains(*id))
                .count();
            if selected > 1 {
                return Err("multiple selected winners in label burst".into());
            }
        }
    }
    Ok(())
}

fn score(predictions: &Dataset, labels: &Dataset, split: Option<&str>) -> Result<Value, String> {
    for prediction in predictions.shoots.values() {
        let Some(label) = labels.shoots.get(&prediction.id) else { return Err("prediction references unknown shootId".into()) };
        for id in prediction.frames.keys() {
            if !label.frames.contains_key(id) {
                return Err("prediction references frame outside labeled shoot".into());
            }
        }
    }
    let label_frame_ids: BTreeSet<String> = labels.shoots.values().flat_map(|shoot| shoot.frames.keys().cloned()).collect();
    if predictions.flat_decisions.keys().any(|id| !label_frame_ids.contains(id))
        || predictions.flat_winners.iter().any(|id| !label_frame_ids.contains(id))
    {
        return Err("prediction references photo outside labeled shoots".into());
    }
    validate_prediction_splits(predictions, labels, split)?;
    validate_winner_cardinality(predictions, labels, split)?;
    for (winner, burst_id) in &predictions.flat_winner_bursts {
        let Some(burst) = labels.shoots.values().find_map(|shoot| shoot.bursts.get(burst_id)) else {
            return Err("winner prediction references unknown burst".into());
        };
        if !burst.members.contains(winner) {
            return Err("winner prediction references photo outside burst".into());
        }
    }
    let mut selected = 0usize;
    let mut labeled_shoots = 0usize;
    let mut labeled_frames = 0usize;
    let mut evaluated_frames = 0usize;
    let mut tp_keep = 0usize;
    let mut fp_keep = 0usize;
    let mut fn_keep = 0usize;
    let mut tp_reject = 0usize;
    let mut fp_reject = 0usize;
    let mut fn_reject = 0usize;
    let mut false_rejects = 0usize;
    let mut abstentions = 0usize;
    let mut all_photos = 0usize;
    let mut all_abstentions = 0usize;
    let mut unknown_rejects = 0usize;
    let mut decision_covered = 0usize;
    let mut total_prediction_frames = 0usize;
    let mut burst_denominator = 0usize;
    let mut burst_numerator = 0usize;
    let mut burst_coverage = 0usize;
    let mut elapsed = Vec::new();
    for label in labels.shoots.values() {
        if split.is_some_and(|wanted| label.split.as_deref() != Some(wanted)) {
            continue;
        }
        selected += 1;
        let prediction = predictions.shoots.get(&label.id);
        total_prediction_frames = total_prediction_frames.saturating_add(prediction.map(|p| p.frames.len()).unwrap_or(0));
        all_photos = all_photos.saturating_add(label.frames.len());
        let explicit_labels: Vec<(&String, Label)> = label
            .frames
            .iter()
            .filter_map(|(id, frame)| frame.label.map(|value| (id, value)))
            .chain(label.acceptable.iter().map(|id| (id, Label::Keep)))
            .chain(label.reject.iter().map(|id| (id, Label::Reject)))
            .collect();
        let mut unique_labeled = BTreeMap::new();
        for (id, value) in explicit_labels {
            unique_labeled.insert(id, value);
        }
        labeled_frames = labeled_frames.saturating_add(unique_labeled.len());
        if !unique_labeled.is_empty() {
            labeled_shoots += 1;
        }
        for (id, expected) in unique_labeled {
            evaluated_frames += 1;
            let decision = prediction_decision(predictions, prediction, id);
            if matches!(decision, Decision::Keep | Decision::Reject) {
                decision_covered += 1;
            }
            match (expected, decision) {
                (Label::Keep, Decision::Keep) => tp_keep += 1,
                (Label::Keep, Decision::Reject) => {
                    fp_reject += 1;
                    false_rejects += 1;
                }
                (Label::Keep, Decision::Abstain) => fn_keep += 1,
                (Label::Reject, Decision::Reject) => tp_reject += 1,
                (Label::Reject, Decision::Keep) => fp_keep += 1,
                (Label::Reject, Decision::Abstain) => fn_reject += 1,
            }
            if decision == Decision::Abstain {
                abstentions += 1;
            }
        }
        for burst in label.bursts.values() {
            if !burst.acceptable.is_empty() {
                burst_denominator += 1;
                let agrees = prediction_winner(predictions, prediction, &burst.acceptable);
                let selected = prediction_winner_any(predictions, prediction, &burst.members);
                if agrees {
                    burst_numerator += 1;
                }
                if selected {
                    burst_coverage += 1;
                }
            }
        }
        if label.bursts.is_empty() && !label.acceptable.is_empty() {
            burst_denominator += 1;
            if prediction_winner(predictions, prediction, &label.acceptable) {
                burst_numerator += 1;
            }
            let known_members: BTreeSet<String> = label.frames.keys().cloned().collect();
            if prediction_winner_any(predictions, prediction, &known_members) {
                burst_coverage += 1;
            }
        }
        let known_label_ids: BTreeSet<&String> =
            label.frames.iter().filter_map(|(id, frame)| frame.label.map(|_| id)).chain(label.acceptable.iter()).chain(label.reject.iter()).collect();
        for id in label.frames.keys() {
            if !known_label_ids.contains(id) {
                let decision = prediction_decision(predictions, prediction, id);
                if decision == Decision::Reject {
                    unknown_rejects += 1;
                }
            }
            if prediction_decision(predictions, prediction, id) == Decision::Abstain {
                all_abstentions += 1;
            }
        }
        if let Some(value) = prediction.and_then(|p| p.elapsed_ms) {
            elapsed.push(value);
        }
    }
    if predictions.shoots.is_empty() {
        total_prediction_frames = predictions
            .flat_decisions
            .keys()
            .filter(|id| {
                labels
                    .shoots
                    .values()
                    .any(|shoot| shoot.frames.contains_key(*id) && split.is_none_or(|wanted| shoot.split.as_deref() == Some(wanted)))
            })
            .count();
        if let Some(value) = predictions.flat_elapsed_ms {
            elapsed.push(value);
        }
    }
    if selected == 0 {
        return Err("no shoots match requested split".into());
    }
    let elapsed_value = if elapsed.is_empty() {
        json!({"count": 0, "totalMs": null, "meanMs": null})
    } else {
        let total: f64 = elapsed.iter().sum();
        json!({"count": elapsed.len(), "totalMs": total, "meanMs": total / elapsed.len() as f64})
    };
    let reject_precision = ratio(tp_reject, tp_reject + fp_reject);
    // A keep decision for an explicitly rejected photo is a missed rejection,
    // so include fp_keep in reject recall's denominator.
    let reject_recall = ratio(tp_reject, tp_reject + fp_keep + fn_reject);
    let winner_agreement = ratio(burst_numerator, burst_denominator);
    let winner_coverage = ratio(burst_coverage, burst_denominator);
    let decision_coverage = ratio(decision_covered, evaluated_frames);
    // A reject decision for an explicitly kept photo is a missed keep, so
    // include fp_reject in keep recall's denominator.
    let keep_recall = ratio(tp_keep, tp_keep + fp_reject + fn_keep);
    let report = json!({
        "version": REPORT_VERSION,
        "schema": REPORT_SCHEMA,
        "mode": "score",
        "split": split,
        "metrics": {
            "precision": ratio(tp_keep, tp_keep + fp_keep),
            "recall": keep_recall,
            "rejectPrecision": reject_precision.clone(),
            "rejectRecall": reject_recall.clone(),
            "reject_precision": reject_precision,
            "reject_recall": reject_recall,
            "falseRejects": false_rejects,
            "falseRejectCount": false_rejects,
            "false_reject_count": false_rejects,
            "falseRejectRate": ratio(false_rejects, unique_keep_count(labels, split)),
            "unknownRejects": unknown_rejects,
            "unknownRejectCount": unknown_rejects,
            "rejection": {"precision": ratio(tp_reject, tp_reject + fp_reject), "recall": reject_recall.clone()},
            "burstWinnerAgreement": {"value": winner_agreement.clone(), "numerator": burst_numerator, "denominator": burst_denominator},
            "winnerAgreement": winner_agreement.clone(),
            "winnerCoverage": {"value": winner_coverage.clone(), "numerator": burst_coverage, "denominator": burst_denominator},
            "decisionCoverage": decision_coverage.clone(),
            "decision_coverage": decision_coverage,
            "abstentions": {"count": all_abstentions, "rate": ratio(all_abstentions, all_photos), "knownLabelCount": abstentions},
            "elapsedMs": elapsed_value
        },
        "coverage": {
            "shoots": {"selected": selected, "labeled": labeled_shoots, "ratio": ratio(labeled_shoots, selected)},
            "frames": {"total": all_photos, "labeled": labeled_frames, "evaluated": evaluated_frames, "predictedRows": total_prediction_frames, "ratio": ratio(total_prediction_frames, all_photos)}
        },
        "limits": {"maxInputBytes": MAX_BYTES, "maxShoots": MAX_SHOOTS, "maxFrames": MAX_FRAMES},
        "labelPolicy": "explicit labels only; unpicked frames are excluded unless explicitly rejected"
    });
    Ok(report)
}

fn ratio(numerator: usize, denominator: usize) -> Value {
    if denominator == 0 { Value::Null } else { json!(numerator as f64 / denominator as f64) }
}

fn prediction_decision(predictions: &Dataset, shoot: Option<&Shoot>, id: &str) -> Decision {
    shoot
        .and_then(|value| value.frames.get(id))
        .and_then(|frame| frame.decision)
        .or_else(|| predictions.flat_decisions.get(id).copied())
        .unwrap_or(Decision::Abstain)
}

fn prediction_winner(predictions: &Dataset, shoot: Option<&Shoot>, acceptable: &BTreeSet<String>) -> bool {
    shoot.is_some_and(|value| value.winners.iter().any(|id| acceptable.contains(id)))
        || predictions.flat_winners.iter().any(|id| acceptable.contains(id))
}

fn prediction_winner_any(predictions: &Dataset, shoot: Option<&Shoot>, members: &BTreeSet<String>) -> bool {
    shoot.is_some_and(|value| value.winners.iter().any(|id| members.contains(id))) || predictions.flat_winners.iter().any(|id| members.contains(id))
}

fn unique_keep_count(labels: &Dataset, split: Option<&str>) -> usize {
    labels
        .shoots
        .values()
        .filter(|shoot| split.is_none_or(|wanted| shoot.split.as_deref() == Some(wanted)))
        .map(|shoot| {
            let mut keep_ids = shoot.acceptable.clone();
            keep_ids.extend(shoot.frames.iter().filter_map(|(id, frame)| (frame.label == Some(Label::Keep)).then_some(id.clone())));
            keep_ids.len()
        })
        .sum()
}

struct Manifest {
    files: Vec<PathBuf>,
    file_keys: Vec<String>,
    shoots: Vec<ManifestShoot>,
}

struct ManifestShoot {
    id: String,
    split: Option<String>,
    frames: Vec<ManifestFrame>,
}

struct ManifestFrame {
    id: String,
    file_key: String,
}

fn parse_manifest(value: &Value) -> Result<Manifest, String> {
    let shoots = shoot_values(value)?;
    let mut files = Vec::new();
    let mut file_keys = Vec::new();
    let mut result = Vec::new();
    let mut shoot_ids = BTreeSet::new();
    let mut photo_ids = BTreeSet::new();
    for (index, raw) in shoots.iter().enumerate() {
        let object = raw.as_object().ok_or("manifest shoot must be an object")?;
        let id = text(object, &["shootId", "shoot_id", "id"]).ok_or("manifest shoot is missing shootId")?;
        valid_id(&id, "shootId")?;
        if !shoot_ids.insert(id.clone()) {
            return Err("duplicate shootId".into());
        }
        let raw_frames = object.get("frames").and_then(Value::as_array).ok_or("manifest shoot is missing frames")?;
        let mut frame_ids = BTreeSet::new();
        let mut frames = Vec::new();
        for raw_frame in raw_frames {
            let frame = raw_frame.as_object().ok_or("manifest frame must be an object")?;
            let frame_id = text(frame, &["id", "frameId", "frame_id"]).ok_or("manifest frame is missing id")?;
            let path = text(frame, &["path", "file", "filePath"]).ok_or("manifest frame is missing explicit path")?;
            valid_id(&frame_id, "frame ID")?;
            if !frame_ids.insert(frame_id.clone()) {
                return Err("duplicate frame ID within shoot".into());
            }
            if !photo_ids.insert(frame_id.clone()) {
                return Err("duplicate frame ID across shoots".into());
            }
            let canonical = std::fs::canonicalize(Path::new(&path)).map_err(|_| "baseline input file is not readable")?;
            if !canonical.is_file() {
                return Err("baseline input path is not a file".into());
            }
            let key = canonical.to_string_lossy().into_owned();
            if file_keys.contains(&key) {
                return Err("duplicate baseline input file".into());
            }
            if files.len() >= MAX_FILES {
                return Err("baseline manifest exceeds file count cap".into());
            }
            file_keys.push(key.clone());
            files.push(canonical);
            frames.push(ManifestFrame { id: frame_id, file_key: key });
        }
        let split = text(object, &["split", "partition"]);
        validate_split(split.as_deref())?;
        result.push(ManifestShoot { id, split, frames });
        if index >= MAX_SHOOTS {
            return Err("baseline manifest exceeds shoot count cap".into());
        }
    }
    Ok(Manifest { files, file_keys, shoots: result })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Value {
        json!({"version":1,"shoots":[
            {"shootId":"s1","split":"eval","frames":[{"id":"a","label":"keep"},{"id":"b"},{"id":"c","label":"reject"}],"acceptableWinnerIds":["a"]},
            {"shootId":"s2","split":"eval","frames":["d","e"],"acceptableWinnerIds":["d","e"]}
        ]})
    }

    #[test]
    fn score_counts_explicit_labels_multiple_winners_and_abstention() {
        let predicted = json!({"version":"pred.v1","shoots":[
            {"shootId":"s1","split":"eval","frames":[{"id":"a","decision":"pick"},{"id":"c","decision":"reject"}],"winnerIds":["a"],"elapsedMs":12.0},
            {"shootId":"s2","split":"eval","frames":[{"id":"d","decision":"keep"}],"winnerIds":["d"]}
        ]});
        let report =
            score(&parse_dataset(&predicted, InputKind::Predictions).unwrap(), &parse_dataset(&labels(), InputKind::Labels).unwrap(), Some("eval"))
                .unwrap();
        assert_eq!(report["metrics"]["falseRejects"], 0);
        assert_eq!(report["metrics"]["abstentions"]["count"], 2);
        assert_eq!(report["metrics"]["burstWinnerAgreement"]["numerator"], 2);
        assert_eq!(report["metrics"]["burstWinnerAgreement"]["denominator"], 2);
    }

    #[test]
    fn conflicting_reject_and_winner_are_rejected() {
        let predictions = json!({"shoots":[{"shootId":"s","split":"eval","frames":[{"id":"a","decision":"reject"}],"winnerIds":["a"]}]});
        assert!(parse_dataset(&predictions, InputKind::Predictions).is_err());
    }

    #[test]
    fn keep_decisions_do_not_fabricate_burst_winners() {
        let labels = json!({"shoots":[{"shootId":"s","split":"test","frames":[
            {"id":"a","label":"keep","burst_id":"burst"},{"id":"b","label":"keep","burst_id":"burst"}
        ],"bursts":[{"burst_id":"burst","photo_ids":["a","b"],"acceptable_winners":["a"]}]}]});
        let picked = json!({"shoots":[{"shootId":"s","split":"test","frames":[
            {"id":"a","decision":"pick"},{"id":"b","decision":"keep"}
        ],"bursts":[{"burst_id":"burst","photo_ids":["a","b"]}]}]});
        let picked_report =
            score(&parse_dataset(&picked, InputKind::Predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test"))
                .unwrap();
        assert_eq!(picked_report["metrics"]["winnerAgreement"], json!(1.0));
        assert_eq!(picked_report["metrics"]["winnerCoverage"]["value"], json!(1.0));

        let keeps = json!({"shoots":[{"shootId":"s","split":"test","frames":[
            {"id":"a","decision":"keep"},{"id":"b","decision":"keep"}
        ],"bursts":[{"burst_id":"burst","photo_ids":["a","b"]}]}]});
        let keep_report =
            score(&parse_dataset(&keeps, InputKind::Predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test"))
                .unwrap();
        assert_eq!(keep_report["metrics"]["winnerAgreement"], json!(0.0));
        assert_eq!(keep_report["metrics"]["winnerCoverage"]["value"], json!(0.0));
    }

    #[test]
    fn unpicked_frame_is_not_inferred_as_reject() {
        let labels = json!({"shoots":[{"shootId":"s","split":"test","frames":["a","b"],"acceptableWinnerIds":["a"]}]});
        let predictions = json!({"shoots":[{"shootId":"s","split":"test","frames":[{"id":"a","decision":"pick"}]}]});
        let report =
            score(&parse_dataset(&predictions, InputKind::Predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), None).unwrap();
        assert_eq!(report["coverage"]["frames"]["labeled"], 1);
        assert_eq!(report["metrics"]["rejection"]["recall"], Value::Null);
    }

    #[test]
    fn duplicate_and_leaking_inputs_are_rejected() {
        let duplicate = json!({"shoots":[{"shootId":"s","frames":["a","a"]}]});
        assert!(parse_dataset(&duplicate, InputKind::Labels).is_err());
        let leaking = json!({"trainShootIds":["s"],"evalShootIds":["s"],"shoots":[{"shootId":"s","frames":["a"]}]});
        assert!(parse_dataset(&leaking, InputKind::Labels).is_err());
        let unknown_split = json!({"trainShootIds":["missing"],"shoots":[{"shootId":"s","split":"train","frames":["a"]}]});
        assert!(parse_dataset(&unknown_split, InputKind::Labels).is_err());
    }

    #[test]
    fn frozen_manifest_photos_ground_truth_and_flat_winners_are_scored() {
        let labels = json!({"version":1,"fixture_kind":"SYNTHETIC","shoots":[{"shoot_id":"s","split":"test","photos":[
            {"photo_id":"a","asset_ref":"synthetic://a","burst_id":"b","ground_truth":"reject"},
            {"photo_id":"c","asset_ref":"synthetic://c","burst_id":"b","ground_truth":"keep"},
            {"photo_id":"u","asset_ref":"synthetic://u","burst_id":null,"ground_truth":"unknown"}
        ],"bursts":[{"burst_id":"b","photo_ids":["a","c"],"acceptable_winners":["c"]}]}]});
        let predictions = json!({"split":"test","predictions":[
            {"photo_id":"a","decision":"reject"},{"photo_id":"c","decision":"keep"},{"photo_id":"u","decision":"reject"}
        ],"winner_predictions":[{"burst_id":"b","selected_winner_id":"c"}]});
        let report =
            score(&parse_flat_predictions(&predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test")).unwrap();
        assert_eq!(report["metrics"]["rejectPrecision"], json!(1.0));
        assert_eq!(report["metrics"]["unknownRejectCount"], json!(1));
        assert_eq!(report["metrics"]["winnerAgreement"], json!(1.0));
        assert_eq!(report["coverage"]["frames"]["predictedRows"], json!(3));
    }

    #[test]
    fn malformed_present_winner_predictions_are_rejected() {
        for malformed in [Value::Null, json!({}), json!("winner"), json!(true)] {
            let predictions = json!({"split":"test","predictions":[],"winner_predictions":malformed});
            assert!(parse_flat_predictions(&predictions).is_err());
        }
        assert!(parse_flat_predictions(&json!({"split":"test","predictions":[],"winner_predictions":[]})).is_ok());
        assert!(parse_flat_predictions(&json!({"split":"test","predictions":[]})).is_ok());
    }

    #[test]
    fn rejected_keep_is_missed_rejection_and_burst_coverage_accepts_any_member() {
        let labels = json!({"shoots":[{"shootId":"s","split":"eval","frames":[
            {"id":"r1","label":"reject"},{"id":"r2","label":"reject"},
            {"id":"a","burst_id":"b","label":"keep"},{"id":"b","burst_id":"b","label":"reject"}
        ],"bursts":[{"burst_id":"b","photo_ids":["a","b"],"acceptable_winners":["a"]}]}]});
        let predictions = json!({"shoots":[{"shootId":"s","split":"eval","frames":[
            {"id":"r1","decision":"reject"},{"id":"r2","decision":"keep"},{"id":"b","decision":"pick"}
        ],"winnerIds":["b"]}]});
        let report =
            score(&parse_dataset(&predictions, InputKind::Predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("eval"))
                .unwrap();
        assert_eq!(report["metrics"]["rejectRecall"], json!(1.0 / 3.0));
        assert_eq!(report["metrics"]["burstWinnerAgreement"]["value"], json!(0.0));
        assert_eq!(report["metrics"]["winnerCoverage"]["value"], json!(1.0));
    }

    #[test]
    fn duplicate_root_burst_ids_are_rejected_even_with_disjoint_members() {
        let labels = json!({"shoots":[
            {"shootId":"s1","split":"eval","frames":["a"]},
            {"shootId":"s2","split":"eval","frames":["b"]}
        ],"bursts":[
            {"burst_id":"duplicate","photo_ids":["a"]},
            {"burst_id":"duplicate","photo_ids":["b"]}
        ]});
        assert!(parse_dataset(&labels, InputKind::Labels).is_err());
    }

    #[test]
    fn predictions_allow_one_selected_winner_per_declared_burst() {
        let predictions = json!({"shoots":[{"shootId":"s","split":"eval","frames":["a","b"],
            "winnerIds":["a","b"],
            "bursts":[{"burst_id":"burst","photo_ids":["a","b"]}]}]});
        assert!(parse_dataset(&predictions, InputKind::Predictions).is_err());

        let flat = json!({"split":"eval","predictions":[],"winner_predictions":[
            {"burst_id":"burst","selected_winner_id":"a"},
            {"burst_id":"burst","selected_winner_id":"b"}
        ]});
        assert!(parse_flat_predictions(&flat).is_err());
    }

    #[test]
    fn unscoped_multiple_winners_are_rejected_after_matching_label_burst() {
        let labels = json!({"shoots":[{"shoot_id":"s","split":"test","photos":[
            {"photo_id":"a","ground_truth":"keep"},{"photo_id":"b","ground_truth":"keep"}
        ],"bursts":[{"burst_id":"burst","photo_ids":["a","b"],"acceptable_winners":["a","b"]}]}]});
        let predictions = json!({"split":"test","predictions":[],"winner_predictions":[
            {"selected_winner_id":"a"},{"selected_winner_id":"b"}
        ]});
        assert!(score(&parse_flat_predictions(&predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test")).is_err());
    }

    #[test]
    fn acceptable_winner_does_not_promote_unknown_frame_to_keep() {
        let labels = json!({"shoots":[{"shoot_id":"s","split":"test","photos":[
            {"photo_id":"u","ground_truth":"unknown","burst_id":"burst"}
        ],"bursts":[{"burst_id":"burst","photo_ids":["u"],"acceptable_winners":["u"]}]}]});
        let predictions = json!({"split":"test","predictions":[
            {"photo_id":"u","decision":"reject"}
        ],"winner_predictions":[{"burst_id":"burst","selected_winner_id":"u"}]});
        let report =
            score(&parse_flat_predictions(&predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test")).unwrap();
        assert_eq!(report["metrics"]["unknownRejectCount"], json!(1));
        assert_eq!(report["metrics"]["falseRejectCount"], json!(0));
        assert_eq!(report["coverage"]["frames"]["labeled"], json!(0));
    }

    #[test]
    fn invalid_ground_truth_is_rejected_but_explicit_unknown_is_valid() {
        let invalid = json!({"shoots":[{"shoot_id":"s","split":"test","photos":[
            {"photo_id":"a","ground_truth":"maybe"}
        ]}]});
        assert!(parse_dataset(&invalid, InputKind::Labels).is_err());

        let unknown = json!({"shoots":[{"shoot_id":"s","split":"test","photos":[
            {"photo_id":"a","ground_truth":"unknown"}
        ]}]});
        assert!(parse_dataset(&unknown, InputKind::Labels).is_ok());
    }

    #[test]
    fn prediction_split_must_match_labels_and_requested_split() {
        let labels = json!({"shoots":[{"shoot_id":"s","split":"test","photos":["a"]}]});
        let flat = json!({"split":"train","predictions":[{"photo_id":"a","decision":"keep"}]});
        assert!(score(&parse_flat_predictions(&flat).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test")).is_err());

        let shoot = json!({"shoots":[{"shoot_id":"s","split":"train","frames":[{"id":"a","decision":"keep"}]}]});
        assert!(
            score(&parse_dataset(&shoot, InputKind::Predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test"))
                .is_err()
        );
    }

    #[test]
    fn split_metadata_checks_all_partitions() {
        let labels = json!({"trainShootIds":["s"],"validationShootIds":["s"],"shoots":[
            {"shoot_id":"s","split":"train","frames":["a"]}
        ]});
        assert!(parse_dataset(&labels, InputKind::Labels).is_err());
    }

    #[test]
    fn frame_coverage_ratio_counts_selected_split_predictions() {
        let labels = json!({"shoots":[
            {"shoot_id":"train","split":"train","frames":[{"id":"a","ground_truth":"keep"}]},
            {"shoot_id":"test","split":"test","frames":[{"id":"b","ground_truth":"keep"}]}
        ]});
        let no_predictions = json!({"split":"test","predictions":[]});
        let report =
            score(&parse_flat_predictions(&no_predictions).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test")).unwrap();
        assert_eq!(report["coverage"]["frames"]["predictedRows"], json!(0));
        assert_eq!(report["coverage"]["frames"]["ratio"], json!(0.0));

        let test_prediction = json!({"split":"test","predictions":[{"photo_id":"b","decision":"keep"}]});
        let report =
            score(&parse_flat_predictions(&test_prediction).unwrap(), &parse_dataset(&labels, InputKind::Labels).unwrap(), Some("test")).unwrap();
        assert_eq!(report["coverage"]["frames"]["predictedRows"], json!(1));

        let mixed_prediction = json!({"shoots":[
            {"shoot_id":"train","split":"train","frames":[{"id":"a","decision":"keep"}]},
            {"shoot_id":"test","split":"test","frames":[{"id":"b","decision":"keep"}]}
        ]});
        let report = score(
            &parse_dataset(&mixed_prediction, InputKind::Predictions).unwrap(),
            &parse_dataset(&labels, InputKind::Labels).unwrap(),
            Some("test"),
        )
        .unwrap();
        assert_eq!(report["coverage"]["frames"]["predictedRows"], json!(1));
    }

    #[test]
    fn malformed_or_conflicting_boolean_labels_are_rejected() {
        let malformed = json!({"shoots":[{"shoot_id":"s","split":"test","frames":[
            {"id":"a","keep":"yes"}
        ]}]});
        assert!(parse_dataset(&malformed, InputKind::Labels).is_err());

        let conflicting = json!({"shoots":[{"shoot_id":"s","split":"test","frames":[
            {"id":"a","ground_truth":"unknown","keep":true}
        ]}]});
        assert!(parse_dataset(&conflicting, InputKind::Labels).is_err());

        let opposing = json!({"shoots":[{"shoot_id":"s","split":"test","frames":[
            {"id":"a","keep":true,"reject":true}
        ]}]});
        assert!(parse_dataset(&opposing, InputKind::Labels).is_err());

        let unknown_winner = json!({"shoots":[{"shoot_id":"s","split":"test","frames":[
            {"id":"a","ground_truth":"unknown"}
        ],"acceptableWinnerIds":["a"]}]});
        assert!(parse_dataset(&unknown_winner, InputKind::Labels).is_err());
    }

    #[test]
    fn nested_label_values_have_a_depth_limit() {
        let mut nested = json!("unknown");
        for _ in 0..=MAX_LABEL_DEPTH {
            nested = json!({"label": nested});
        }
        let labels = json!({"shoots":[{"shoot_id":"s","split":"test","frames":[
            {"id":"a","ground_truth":nested}
        ]}]});
        assert!(parse_dataset(&labels, InputKind::Labels).is_err());

        let conflicting = json!({"shoots":[{"shoot_id":"s","split":"test","frames":[
            {"id":"a","ground_truth":{"label":"keep","decision":"reject"}}
        ]}]});
        assert!(parse_dataset(&conflicting, InputKind::Labels).is_err());
    }

    #[test]
    fn any_root_bursts_value_is_rejected() {
        let labels = json!({"bursts":null,"shoots":[
            {"shoot_id":"s","split":"test","frames":["a"]}
        ]});
        assert!(parse_dataset(&labels, InputKind::Labels).is_err());

        let predictions = json!({"split":"test","bursts":null,"predictions":[]});
        assert!(parse_flat_predictions(&predictions).is_err());
    }

    #[test]
    fn synthetic_cases_fixture_matches_expected_test_and_validation_metrics() {
        let cases: Value = serde_json::from_str(include_str!("../../../tests/fixtures/culling/synthetic-cases.json")).unwrap();
        let labels: Value = serde_json::from_str(include_str!("../../../tests/fixtures/culling/synthetic-v1.json")).unwrap();
        let labels = parse_dataset(&labels, InputKind::Labels).unwrap();
        for case in cases["cases"].as_array().unwrap() {
            let split = case["split"].as_str().unwrap();
            let predictions = json!({
                "version": 1,
                "fixture_kind": "SYNTHETIC",
                "split": split,
                "predictions": case.get("predictions").cloned().unwrap_or_else(|| json!([])),
                "winner_predictions": case.get("winner_predictions").cloned().unwrap_or_else(|| json!([]))
            });
            let report = score(&parse_flat_predictions(&predictions).unwrap(), &labels, Some(split)).unwrap();
            let expected = case["expected"].as_object().unwrap();
            for key in ["rejectPrecision", "rejectRecall", "winnerAgreement", "winnerCoverage", "decisionCoverage"] {
                if let Some(value) = expected.get(key) {
                    let expected = value.as_f64().unwrap();
                    let actual = report["metrics"][key].get("value").and_then(Value::as_f64).or_else(|| report["metrics"][key].as_f64()).unwrap();
                    assert!((actual - expected).abs() < 1e-8, "{key}: expected {expected}, got {actual}");
                }
            }
            if let Some(value) = expected.get("falseRejectCount") {
                assert_eq!(report["metrics"]["falseRejectCount"], *value);
            }
            if let Some(value) = expected.get("unknownRejectCount") {
                assert_eq!(report["metrics"]["unknownRejectCount"], *value);
            }
            if let Some(value) = expected.get("abstentions").and_then(|value| value.get("count")) {
                assert_eq!(report["metrics"]["abstentions"]["count"], *value);
            }
        }
    }
}
