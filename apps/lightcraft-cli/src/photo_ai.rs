//! Read-only provider evaluation on a private ephemeral session. Never persist/apply to a library.
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use lightcraft_codecs::{EncodeImage, EncodeMeta, encode_png};
use lightcraft_develop::controls;
use lightcraft_engine::{Session, SourceLevel, catalog::PhotoId};
use lightcraft_photo_ai::{
    CONTROLS, Proxy,
    network::{OpenRouter, preflight},
    scaledown::ScaleDown,
};
use lightcraft_pipeline::auto::auto_tone;
use lightcraft_raster::Rgba8;
use serde_json::{Value, json};

/// `ai search "<text>" [--prepare-only] [--today YYYY-MM-DD]`: natural-language library search to a
/// `library.filter` patch through ScaleDown Extract. Only the typed text (and a date) is sent.
fn search(args: &[String]) -> Result<(), String> {
    let mut text = None;
    let mut prepare = false;
    let mut today = None;
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--prepare-only" => prepare = true,
            "--today" => {
                i += 1;
                let value = args.get(i).ok_or("--today: missing value")?;
                today = Some(lightcraft_photo_ai::search::valid_date(value).filter(|d| d.len() == 10).ok_or("--today expects YYYY-MM-DD")?);
            }
            a if a.starts_with('-') => return Err(format!("unknown ai search option {a}")),
            t if text.is_none() => text = Some(t.to_owned()),
            _ => return Err("ai search takes one quoted search text".into()),
        }
        i += 1;
    }
    let text = text.ok_or("ai search expects a quoted search text")?;
    let today = today.unwrap_or_else(lightcraft_photo_ai::scaledown::today_utc);
    if prepare {
        let body = lightcraft_photo_ai::search::request_body(&text, Some(&today))?;
        println!("{}", serde_json::to_string_pretty(&body).map_err(|_| "could not encode request")?);
        return Ok(());
    }
    let key = std::env::var("SCALEDOWN_API_KEY").map_err(|_| "set SCALEDOWN_API_KEY securely, or use --prepare-only")?;
    let cancel = AtomicBool::new(false);
    let found = ScaleDown::new(key)?.extract_search_on(&text, Some(&today), &cancel)?;
    let filter = lightcraft_photo_ai::search::entities_to_filter_json(&found);
    let out = json!({"entities": found, "command": "library.filter", "filter": filter});
    println!("{}", serde_json::to_string_pretty(&out).map_err(|_| "could not encode result")?);
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("search") {
        return search(args.get(1..).unwrap_or_default());
    }
    if args.first().map(String::as_str) != Some("compare") {
        return Err("ai expects compare or search; see --help".into());
    }
    let mut output = None;
    let mut selected = Vec::new();
    let mut files = Vec::new();
    let mut demo = false;
    let mut prepare = false;
    let mut repeats = 2usize;
    let mut budget = 0.5f64;
    let mut i = 1usize;
    while let Some(arg) = args.get(i) {
        let mut value = || {
            i += 1;
            args.get(i).map(String::as_str).ok_or_else(|| format!("{arg}: missing value"))
        };
        match arg.as_str() {
            "--out" => output = Some(PathBuf::from(value()?)),
            "--model" => selected.push(value()?.to_owned()),
            "--repeat" => repeats = value()?.parse().map_err(|_| "--repeat expects 1..3")?,
            "--budget-usd" => budget = value()?.parse().map_err(|_| "--budget-usd expects a number")?,
            "--demo" => demo = true,
            "--prepare-only" => prepare = true,
            a if a.starts_with('-') => return Err(format!("unknown ai option {a}")),
            f => files.push(f.to_owned()),
        }
        i += 1;
    }
    if !(1..=3).contains(&repeats) || !budget.is_finite() || budget <= 0.0 || budget > 5.0 {
        return Err("repeat must be 1..3; budget must be >0..5 USD".into());
    }
    if demo != files.is_empty() || files.len() > 12 {
        return Err("choose --demo OR 1..12 explicit files; directories/libraries are not supported".into());
    }
    for file in &files {
        if !Path::new(file).is_file() {
            return Err("ai requires explicit photo files, not folders".into());
        }
    }
    if selected.is_empty() {
        selected = lightcraft_photo_ai::models()?
            .get("models")
            .and_then(Value::as_array)
            .ok_or("missing models")?
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
    }
    if selected.len() > 5 {
        return Err("at most five models per run".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for model in &selected {
        lightcraft_photo_ai::validate_model(model)?;
        if !unique.insert(model) {
            return Err("duplicate model".into());
        }
    }
    let output = output.ok_or("missing --out NEW_DIR")?;
    let provider = if prepare {
        None
    } else {
        Some(OpenRouter::new(std::env::var("OPENROUTER_API_KEY").map_err(|_| "set OPENROUTER_API_KEY securely, or use --prepare-only")?)?)
    };
    let cancel = AtomicBool::new(false);
    let catalog = provider.as_ref().map(|p| p.catalog(&cancel)).transpose()?;
    let mut metadata = Vec::new();
    if let Some(catalog) = &catalog {
        for model in &selected {
            metadata.push(preflight(catalog, model)?);
        }
    }
    // Fresh IDs/source caches per run. No saved library, supplied edit recipe or disk-persist path.
    let mut session = if demo { Session::with_demo() } else { Session::new().with_fs() };
    let ids: Vec<PhotoId> = if demo {
        session.catalog.photos().filter(|p| p.in_library()).take(3).map(|p| p.id).collect()
    } else {
        let absolute: Vec<PathBuf> =
            files.iter().map(|p| std::fs::canonicalize(p).map_err(|_| "could not resolve photo input")).collect::<Result<_, _>>()?;
        let result = session.execute("library.import", &json!({"paths": absolute})).map_err(|e| e.to_string())?;
        result.get("imported").and_then(Value::as_array).ok_or("missing imported photos")?.iter().filter_map(|v| v.as_u64().map(PhotoId)).collect()
    };
    if ids.is_empty() || ids.len() > 12 {
        return Err("no readable photos, or input scope exceeds twelve photos".into());
    }
    if !demo && ids.len() != files.len() {
        return Err("some inputs were unreadable, unsupported or duplicate; no photo assessment sent".into());
    }
    std::fs::create_dir(&output).map_err(|_| "--out must name a new directory with an existing parent")?;
    let mut report = json!({
        "version": lightcraft_photo_ai::VERSION, "mode": if prepare {"prepare-only"} else {"openrouter"},
        "catalogSnapshot": metadata, "promptSha256": lightcraft_photo_ai::digest(lightcraft_photo_ai::PROMPT.as_bytes()), "schemaSha256": lightcraft_photo_ai::digest(lightcraft_photo_ai::SCHEMA.as_bytes()), "repeat": repeats,
        "budgetUsd": budget, "budgetKind": "estimated admission reserve; configure provider-key spending limit for hard enforcement",
        "spentReportedUsd": 0.0, "billingUnknown": false, "failedResponses": 0, "libraryMutated": false,
        "cases": [], "errors": [], "winner": null, "qualification": "experiment; blinded human preference required"
    });
    let mut reserved = 0.0f64;
    let mut spent = 0.0f64;
    let mut stopped = false;
    for (case_index, id) in ids.into_iter().enumerate() {
        if stopped {
            break;
        }
        let input_name = format!("case-{case_index}-input.png");
        let auto_name = format!("case-{case_index}-auto.png");
        let mut job = session.render_job(id, 512, 512, false, true).ok_or("photo cannot render")?;
        let mut settings = (*job.settings).clone();
        for (name, _) in CONTROLS {
            if !controls::set(&mut settings, name, 0.0) {
                return Err("unknown tone control".into());
            }
        }
        job.settings = Arc::new(settings.clone());
        job.cache = None;
        job.stages = None;
        job.view_cache = None;
        let input = job.clone().run().rendered?;
        let input_bytes = encode_png(&EncodeImage::rgba8(&input.image), &EncodeMeta::default()).map_err(|e| e.to_string())?;
        let proxy = Proxy::new(input_bytes)?;
        write(&output.join(&input_name), proxy.bytes())?;
        let source = session.source_now(id, SourceLevel::Thumb)?;
        let auto = auto_tone(&source, &session.source_info(id), &settings);
        let baseline_values = json!({
            "light.exposure": auto.exposure, "light.contrast": auto.contrast, "light.highlights": auto.highlights,
            "light.shadows": auto.shadows, "light.whites": auto.whites, "light.blacks": auto.blacks,
            "color.vibrance": auto.vibrance, "color.saturation": auto.saturation
        });
        let mut baseline = settings.clone();
        apply_values(&mut baseline, &baseline_values)?;
        let mut auto_job = job.clone();
        auto_job.settings = Arc::new(baseline);
        let auto_render = auto_job.run().rendered?;
        write_image(&output.join(&auto_name), &auto_render.image)?;
        let mut case = json!({"input": input_name, "localAuto": auto_name, "proxySha256": proxy.digest(), "inputMetrics": metrics(&input.image), "localAutoMetrics": metrics(&auto_render.image), "localAutoRecipe": baseline_values, "runs": []});
        report["cases"].as_array_mut().ok_or("missing cases")?.push(case.clone());
        save_report(&output, &report)?;
        for (model_index, model) in selected.iter().enumerate() {
            if stopped || prepare {
                break;
            }
            // Conservative text/image-token reserve (not a provider-side hard spending limit).
            let reserve = estimated_reserve(model)?;
            for repetition in 0..repeats {
                if reserved + reserve > budget || spent >= budget {
                    stopped = true;
                    report["errors"].as_array_mut().ok_or("missing report errors")?.push(json!("estimated budget exhausted"));
                    break;
                }
                reserved += reserve;
                let Some(provider) = &provider else { break };
                match provider.assess(model, &proxy, "unspecified photographic intent", &cancel) {
                    Ok(receipt) => {
                        if receipt.assessment.is_none() {
                            report["failedResponses"] = json!(report["failedResponses"].as_u64().unwrap_or(0).saturating_add(1));
                        }
                        if let Some(cost) = receipt.usage.get("cost").and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0) {
                            spent += cost;
                        } else {
                            report["billingUnknown"] = json!(true);
                            stopped = true;
                        }
                        let run = json!({"receipt": receipt, "render": null, "metrics": null});
                        case["runs"].as_array_mut().ok_or("missing runs")?.push(run);
                        report["spentReportedUsd"] = json!(spent);
                        checkpoint_case(&output, &mut report, &case)?;
                        if let Some(recipe) = receipt.assessment.as_ref().and_then(|a| a.recipe.as_ref()) {
                            let mut candidate = settings.clone();
                            apply_values(&mut candidate, &json!(recipe.values()))?;
                            let mut candidate_job = job.clone();
                            candidate_job.settings = Arc::new(candidate);
                            let run = case["runs"].as_array_mut().and_then(|runs| runs.last_mut()).ok_or("missing receipt")?;
                            match candidate_job.run().rendered {
                                Ok(rendered) => {
                                    let name = format!("case-{case_index}-model-{model_index}-repeat-{repetition}.png");
                                    write_image(&output.join(&name), &rendered.image)?;
                                    run["render"] = json!(name);
                                    run["metrics"] = metrics(&rendered.image);
                                }
                                Err(_) => {
                                    run["error"] = json!("candidate render failed; receipt retained");
                                    report["failedResponses"] = json!(report["failedResponses"].as_u64().unwrap_or(0).saturating_add(1));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        report["billingUnknown"] = json!(true);
                        stopped = true;
                        case["runs"].as_array_mut().ok_or("missing runs")?.push(json!({"model": model, "error": error, "render": null}));
                    }
                }
                report["spentReportedUsd"] = json!(spent);
                checkpoint_case(&output, &mut report, &case)?;
            }
        }
        checkpoint_case(&output, &mut report, &case)?;
    }
    save_report(&output, &report)?;
    println!(
        "{}",
        json!({"report": output.join("report.json"), "review": output.join("index.html"), "cases": report["cases"].as_array().map(Vec::len), "spentReportedUsd": spent, "billingUnknown": report["billingUnknown"]})
    );
    if report["billingUnknown"] == true
        || report["failedResponses"].as_u64().unwrap_or(0) > 0
        || !report["errors"].as_array().is_some_and(Vec::is_empty)
    {
        return Err("comparison stopped; partial receipts saved".into());
    }
    Ok(())
}

fn apply_values(settings: &mut lightcraft_develop::DevelopSettings, values: &Value) -> Result<(), String> {
    for (name, _) in CONTROLS {
        let value = values.get(name).and_then(Value::as_f64).ok_or("missing candidate value")?;
        if !controls::set(settings, name, value) {
            return Err("unknown candidate control".into());
        }
    }
    Ok(())
}
fn estimated_reserve(model: &str) -> Result<f64, String> {
    let catalog = lightcraft_photo_ai::models()?;
    let item = catalog["models"].as_array().and_then(|m| m.iter().find(|m| m["id"] == model)).ok_or("missing price")?;
    Ok((20_000.0 * item["inputUsdPerMillion"].as_f64().ok_or("missing input rate")?
        + lightcraft_photo_ai::MAX_OUTPUT_TOKENS as f64 * item["outputUsdPerMillion"].as_f64().ok_or("missing output rate")?)
        / 1_000_000.0)
}
fn metrics(image: &Rgba8) -> Value {
    let count = image.data.len().max(1) as f64;
    let high = image.data.iter().filter(|p| p.get(..3).is_some_and(|p| p.iter().any(|v| *v >= 254))).count() as f64 / count;
    let low = image.data.iter().filter(|p| p.get(..3).is_some_and(|p| p.iter().all(|v| *v <= 1))).count() as f64 / count;
    json!({"displayHighlightClipFraction": high, "displayBlackClipFraction": low, "meaning": "proxy observations; intentional clipping is not an automatic failure"})
}
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("could not write experiment output: {e}"))
}
fn write_image(path: &Path, image: &Rgba8) -> Result<(), String> {
    write(path, &encode_png(&EncodeImage::rgba8(image), &EncodeMeta::default()).map_err(|e| e.to_string())?)
}
fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}
fn checkpoint_case(output: &Path, report: &mut Value, case: &Value) -> Result<(), String> {
    let stored = report["cases"].as_array_mut().and_then(|cases| cases.last_mut()).ok_or("missing report case")?;
    *stored = case.clone();
    save_report(output, report)
}
fn save_report(output: &Path, report: &Value) -> Result<(), String> {
    write(&output.join("report.json"), &serde_json::to_vec_pretty(report).map_err(|_| "could not encode experiment report")?)?;
    let mut html = String::from(
        "<!doctype html><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>Photo assessment comparison</title><style>body{background:#101214;color:#ddd;font:15px system-ui;margin:24px}section{margin:28px 0}.row{display:flex;gap:12px;overflow:auto}figure{margin:0;flex:0 0 360px}img{width:100%;object-fit:contain}figcaption{padding:8px}p{max-width:850px;color:#aaa}</style><h1>Photo assessment experiment</h1><p>Reference renders & model suggestions. No library edits. Compare photographic intent, skin/color, clipping & repeat consistency before selecting any default. Models with no valid recipe retain input. JSON contains costs, latency & failures.</p>",
    );
    for (index, case) in report["cases"].as_array().into_iter().flatten().enumerate() {
        html.push_str(&format!("<section><h2>Photo {}</h2><div class='row'>", index + 1));
        for (field, label) in [("input", "Input"), ("localAuto", "Current local Auto")] {
            if let Some(path) = case[field].as_str() {
                html.push_str(&format!("<figure><img src='{}' alt='{}'><figcaption>{}</figcaption></figure>", escape(path), label, label));
            }
        }
        for run in case["runs"].as_array().into_iter().flatten() {
            let name = run.pointer("/receipt/requestedModel").or_else(|| run.get("model")).and_then(Value::as_str).unwrap_or("Unavailable");
            let reason = run
                .get("error")
                .or_else(|| run.pointer("/receipt/assessment/reason"))
                .or_else(|| run.pointer("/receipt/error"))
                .or_else(|| run.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("Review");
            let path = run["render"].as_str().or_else(|| case["input"].as_str()).unwrap_or("");
            let timing = run.pointer("/receipt/latencyMs").and_then(Value::as_u64).map(|n| format!("{n} ms")).unwrap_or_default();
            let cost =
                run.pointer("/receipt/usage/cost").and_then(Value::as_f64).map(|n| format!("${n:.6}")).unwrap_or_else(|| "cost unknown".into());
            html.push_str(&format!(
                "<figure><img src='{}' alt='{}'><figcaption>{}<br>{}<br>{} · {}</figcaption></figure>",
                escape(path),
                escape(name),
                escape(name),
                escape(reason),
                escape(&timing),
                escape(&cost)
            ));
        }
        html.push_str("</div></section>");
    }
    write(&output.join("index.html"), html.as_bytes())
}
