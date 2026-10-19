//! Original procedural RGB denoise qualification baseline.
//!
//! This harness is deliberately a post-demosaic RGB experiment. It does not read photos,
//! catalogs, models or provider assets, and its receipt never claims qualification.

use std::path::Path;
use std::time::Instant;

use lightcraft_develop::DevelopSettings;
use lightcraft_photo_ai::digest;
use lightcraft_pipeline::local::denoise;
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

const SCHEMA: &str = "lightcraft.denoise-baseline.v1";
const ALGORITHM_VERSION: &str = "procedural-rgb-guided-nr.v1";
const INPUT_VERSION: &str = "linear-rec2020-rgb.v1";
const NOISE_VERSION: &str = "signal-dependent-gaussian-read-rowpattern.v1";
const REPORT_CAP: usize = 4 * 1024 * 1024;
const REPEAT_THRESHOLD: f64 = 1.0e-6;

#[derive(Clone, Copy)]
struct CaseSpec {
    name: &'static str,
    seed: u32,
    row_pattern: bool,
}

const CASES: [CaseSpec; 5] = [
    CaseSpec { name: "flat-shadows", seed: 0x1a2b3c01, row_pattern: true },
    CaseSpec { name: "color-patches-gradient", seed: 0x1a2b3c02, row_pattern: false },
    CaseSpec { name: "hard-step-edge", seed: 0x1a2b3c03, row_pattern: true },
    CaseSpec { name: "fine-texture", seed: 0x1a2b3c04, row_pattern: true },
    CaseSpec { name: "intentional-soft-edge", seed: 0x1a2b3c05, row_pattern: false },
];

pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("denoise") || args.get(1).map(String::as_str) != Some("baseline") {
        return Err(
            "usage: ai denoise baseline --hardware LABEL --out NEW_REPORT [--size 64..512] [--repeats 2..10] [--source-revision LABEL]".into()
        );
    }
    let mut hardware = None;
    let mut out = None;
    let mut size = 128usize;
    let mut repeats = 3usize;
    let mut source_revision = "unspecified-unverified".to_owned();
    let mut i = 2usize;
    while i < args.len() {
        let flag = args.get(i).map(String::as_str).unwrap_or("");
        let value = || args.get(i + 1).cloned().ok_or_else(|| format!("{flag} requires a value"));
        match flag {
            "--hardware" => {
                if hardware.is_some() {
                    return Err("--hardware supplied more than once".into());
                }
                hardware = Some(value()?);
                i += 2;
            }
            "--out" => {
                if out.is_some() {
                    return Err("--out supplied more than once".into());
                }
                out = Some(value()?);
                i += 2;
            }
            "--size" => {
                size = parse_range(&value()?, "size", 64, 512)?;
                i += 2;
            }
            "--repeats" => {
                repeats = parse_range(&value()?, "repeats", 2, 10)?;
                i += 2;
            }
            "--source-revision" => {
                source_revision = value()?;
                i += 2;
            }
            other => return Err(format!("unknown denoise baseline option '{other}'")),
        }
    }
    let hardware = hardware.ok_or("--hardware is required")?;
    let out = out.ok_or("--out is required")?;
    valid_label(&hardware, "hardware")?;
    valid_label(&source_revision, "source revision")?;
    let out_path = Path::new(&out);
    if out_path.exists() {
        return Err(format!("refusing to overwrite existing report '{out}'"));
    }

    let report = build_report(&hardware, &source_revision, size, repeats)?;
    let mut bytes = serde_json::to_vec_pretty(&report).map_err(|e| format!("encode report: {e}"))?;
    bytes.push(b'\n');
    if bytes.len() > REPORT_CAP {
        return Err(format!("report exceeds {REPORT_CAP} byte cap"));
    }
    crate::output_file::publish_new(out_path, &bytes).map_err(|e| format!("could not create new report '{out}': {e}"))?;
    Ok(())
}

fn parse_range(value: &str, name: &str, min: usize, max: usize) -> Result<usize, String> {
    let n = value.parse::<usize>().map_err(|_| format!("invalid {name} '{value}'"))?;
    if !(min..=max).contains(&n) {
        return Err(format!("{name} must be {min}..{max}"));
    }
    Ok(n)
}

fn valid_label(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 512 || value.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        return Err(format!("{name} must be a nonempty path-safe label of at most 512 bytes"));
    }
    Ok(())
}

fn build_report(hardware: &str, source_revision: &str, size: usize, repeats: usize) -> Result<Value, String> {
    let mut cases = Vec::with_capacity(CASES.len());
    for spec in CASES {
        let clean = make_clean(size, spec.name);
        let noisy = make_noisy(&clean, spec.seed, spec.row_pattern);
        let clean_digest = pixel_digest(&clean)?;
        let noisy_digest = pixel_digest(&noisy)?;
        let identity_metrics = metrics(&clean, &noisy)?;
        let mut amounts = Vec::with_capacity(5);
        for amount in [0usize, 25, 50, 75, 100] {
            amounts.push(run_amount(&clean, &noisy, amount, repeats, clean_digest.clone(), noisy_digest.clone())?);
        }
        cases.push(json!({
            "name": spec.name,
            "noiseSeed": format!("0x{:08x}", spec.seed),
            "rowPatternNoise": spec.row_pattern,
            "cleanSha256": clean_digest,
            "noisySha256": noisy_digest,
            "identity": {
                "source": "identity",
                "cleanSha256": pixel_digest(&clean)?,
                "noisySha256": pixel_digest(&noisy)?,
                "outputSha256": pixel_digest(&noisy)?,
                "metrics": identity_metrics,
            },
            "amounts": amounts,
        }));
    }
    Ok(json!({
        "schema": SCHEMA,
        "status": "alwaysUNQUALIFIED",
        "qualification": "alwaysUNQUALIFIED",
        "mode": "procedural-only",
        "hardware": hardware,
        "hardwareVerified": false,
        "sourceRevision": source_revision,
        "sourceRevisionVerified": false,
        "size": size,
        "repeats": repeats,
        "firstCalls": 1,
        "warmRepeats": repeats,
        "sourceLong": size,
        "outputLong": size,
        "defaultProductionSettings": {"detail": 50, "colorDetail": 50, "colorSmoothness": 50, "contrast": 0},
        "colorSpace": "linear-Rec2020-RGB",
        "algorithmVersion": ALGORITHM_VERSION,
        "inputVersion": INPUT_VERSION,
        "noiseVersion": NOISE_VERSION,
        "noiseDescription": "Original deterministic signal-dependent Gaussian shot/read noise with optional deterministic row-pattern noise; synthetic only, not calibrated camera noise.",
        "timedScope": "production lightcraft_pipeline::local::denoise only; fresh noisy clone outside timed scope",
        "firstRunColdCacheAsserted": false,
        "metrics": ["fullRGB-MSE", "fullRGB-RMSE", "PSNR-peak1", "signed-channel-bias", "gradient-RMSE"],
        "cases": cases,
        "errors": [],
    }))
}

fn run_amount(
    clean: &Rgb32f,
    noisy: &Rgb32f,
    amount: usize,
    warm_repeats: usize,
    clean_digest: String,
    noisy_digest: String,
) -> Result<Value, String> {
    let mut first_ms = 0.0f64;
    let mut timings = Vec::with_capacity(warm_repeats);
    let mut first_output = None;
    let mut repeat_max = 0.0f64;
    let mut errors = 0usize;
    let mut settings = DevelopSettings::default();
    settings.detail.nr_luminance = amount as f64;
    settings.detail.nr_color = amount as f64;
    for run in 0..=warm_repeats {
        let mut output = noisy.clone();
        let long = output.width.max(output.height);
        let started = Instant::now();
        denoise(&mut output, &settings, long, long);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if run == 0 {
            first_ms = elapsed;
        } else {
            timings.push(elapsed);
        }
        errors += validate_output(&output)?;
        if let Some(first) = &first_output {
            repeat_max = repeat_max.max(max_abs(first, &output)?);
        } else {
            first_output = Some(output);
        }
    }
    let output = first_output.ok_or("denoise baseline produced no output")?;
    let metrics = metrics(clean, &output)?;
    Ok(json!({
        "amount": amount,
        "source": "outputlong-maxdim",
        "cleanSha256": clean_digest,
        "noisySha256": noisy_digest,
        "outputSha256": pixel_digest(&output)?,
        "metrics": metrics,
        "timingMs": {"first": first_ms, "warmP50": percentile(&timings, 0.50), "warmP95": percentile(&timings, 0.95), "warmCount": timings.len()},
        "repeatMaxAbs": repeat_max,
        "repeatThreshold": REPEAT_THRESHOLD,
        "validation": {"outOfRangeChannelsAcrossRuns": errors, "outputRangeWithinUnit": errors == 0},
        "repeatability": {"maxAbs": repeat_max, "threshold": REPEAT_THRESHOLD, "withinThreshold": repeat_max <= REPEAT_THRESHOLD, "errors": []},
    }))
}

fn metrics(clean: &Rgb32f, output: &Rgb32f) -> Result<Value, String> {
    validate_dimensions(clean, "clean")?;
    validate_dimensions(output, "output")?;
    validate_finite(clean, "clean")?;
    validate_finite(output, "output")?;
    if (clean.width, clean.height) != (output.width, output.height) {
        return Err("clean/output dimensions differ".into());
    }
    let mut squared = 0.0f64;
    let mut bias = [0.0f64; 3];
    for i in 0..clean.data.len() {
        let a = clean.data.get(i).ok_or("clean pixel offset out of bounds")?;
        let b = output.data.get(i).ok_or("output pixel offset out of bounds")?;
        for c in 0..3 {
            let d = b[c] as f64 - a[c] as f64;
            squared += d * d;
            bias[c] += d;
        }
    }
    let count = (clean.data.len() * 3).max(1) as f64;
    let mse = squared / count;
    let gradient = gradient_rmse(clean, output)?;
    Ok(json!({
        "mse": mse,
        "rmse": mse.sqrt(),
        "psnrDbPeak1": (mse > 0.0).then(|| 10.0 * (1.0 / mse).log10()),
        "signedChannelBias": bias.map(|v| v / clean.data.len().max(1) as f64),
        "gradientRmse": gradient,
    }))
}

fn gradient_rmse(clean: &Rgb32f, output: &Rgb32f) -> Result<f64, String> {
    validate_dimensions(clean, "clean")?;
    validate_dimensions(output, "output")?;
    if (clean.width, clean.height) != (output.width, output.height) {
        return Err("clean/output dimensions differ".into());
    }
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for y in 0..clean.height {
        for x in 0..clean.width {
            let i = y.checked_mul(clean.width).and_then(|n| n.checked_add(x)).ok_or("gradient offset overflow")?;
            for (dx, dy) in [(1usize, 0usize), (0, 1)] {
                if x + dx >= clean.width || y + dy >= clean.height {
                    continue;
                }
                let j = (y + dy).checked_mul(clean.width).and_then(|n| n.checked_add(x + dx)).ok_or("gradient offset overflow")?;
                let a = clean.data.get(i).ok_or("clean gradient offset out of bounds")?;
                let b = clean.data.get(j).ok_or("clean gradient offset out of bounds")?;
                let ao = output.data.get(i).ok_or("output gradient offset out of bounds")?;
                let bo = output.data.get(j).ok_or("output gradient offset out of bounds")?;
                for c in 0..3 {
                    let want = b[c] as f64 - a[c] as f64;
                    let got = bo[c] as f64 - ao[c] as f64;
                    let d = got - want;
                    sum += d * d;
                    count += 1;
                }
            }
        }
    }
    Ok((sum / count.max(1) as f64).sqrt())
}

fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((sorted.len() as f64 * q).ceil() as usize).max(1);
    sorted.get(rank.saturating_sub(1)).copied().or_else(|| sorted.last().copied()).unwrap_or(0.0)
}

fn validate_output(image: &Rgb32f) -> Result<usize, String> {
    validate_dimensions(image, "output")?;
    validate_finite(image, "output")?;
    let mut errors = 0;
    for p in &image.data {
        for c in p {
            if !c.is_finite() {
                return Err("production denoise returned a non-finite pixel".into());
            } else if !(0.0..=1.0).contains(c) {
                errors += 1;
            }
        }
    }
    Ok(errors)
}

fn validate_finite(image: &Rgb32f, role: &str) -> Result<(), String> {
    for (i, p) in image.data.iter().enumerate() {
        if p.iter().any(|v| !v.is_finite()) {
            return Err(format!("{role} pixel {i} is non-finite"));
        }
    }
    Ok(())
}

fn validate_dimensions(image: &Rgb32f, role: &str) -> Result<(), String> {
    if image.width == 0 || image.height == 0 {
        return Err(format!("{role} dimensions must be nonzero"));
    }
    if image.width > 512 || image.height > 512 {
        return Err(format!("{role} dimensions exceed 512 px cap"));
    }
    let expected = image.width.checked_mul(image.height).ok_or_else(|| format!("{role} dimensions overflow"))?;
    if expected != image.data.len() {
        return Err(format!("{role} storage length does not match dimensions"));
    }
    Ok(())
}

fn pixel_digest(image: &Rgb32f) -> Result<String, String> {
    validate_dimensions(image, "digest image")?;
    validate_finite(image, "digest image")?;
    let pixel_bytes = image.data.len().checked_mul(12).ok_or("digest pixel byte count overflow")?;
    let capacity = 16usize.checked_add(pixel_bytes).ok_or("digest byte count overflow")?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(&(image.width as u64).to_le_bytes());
    bytes.extend_from_slice(&(image.height as u64).to_le_bytes());
    for p in &image.data {
        for c in p {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
    }
    Ok(digest(&bytes))
}

fn max_abs(a: &Rgb32f, b: &Rgb32f) -> Result<f64, String> {
    validate_dimensions(a, "repeat first")?;
    validate_dimensions(b, "repeat next")?;
    if (a.width, a.height) != (b.width, b.height) {
        return Err("repeat dimensions differ".into());
    }
    let mut max: f64 = 0.0;
    for i in 0..a.data.len() {
        let x = a.data.get(i).ok_or("repeat first offset out of bounds")?;
        let y = b.data.get(i).ok_or("repeat next offset out of bounds")?;
        for c in 0..3 {
            max = max.max((x[c] as f64 - y[c] as f64).abs());
        }
    }
    Ok(max)
}

fn make_clean(size: usize, name: &str) -> Rgb32f {
    Rgb32f::from_fn(size, size, |x, y| {
        let u = x as f32 / size.max(1) as f32;
        let v = y as f32 / size.max(1) as f32;
        let p = match name {
            "flat-shadows" => [0.012 + 0.018 * u, 0.009 + 0.015 * v, 0.016 + 0.021 * (u + v) * 0.5],
            "color-patches-gradient" => {
                let patch = if u < 0.5 && v < 0.5 {
                    [0.82, 0.12, 0.08]
                } else if u >= 0.5 && v < 0.5 {
                    [0.08, 0.72, 0.16]
                } else if u < 0.5 {
                    [0.08, 0.18, 0.78]
                } else {
                    [0.72, 0.58, 0.08]
                };
                [patch[0] * (0.65 + 0.35 * u), patch[1] * (0.65 + 0.35 * v), patch[2] * (0.7 + 0.3 * u)]
            }
            "hard-step-edge" => {
                if u < 0.5 {
                    [0.05, 0.06, 0.07]
                } else {
                    [0.8, 0.72, 0.62]
                }
            }
            "fine-texture" => {
                let t = 0.5 + 0.5 * hash_unit(x as u32 * 31 + y as u32 * 17 + 7);
                [0.18 + 0.35 * t, 0.2 + 0.28 * (1.0 - t), 0.16 + 0.32 * t]
            }
            _ => {
                let z = ((u - 0.5) * 18.0).tanh() * 0.5 + 0.5;
                [0.06 + 0.7 * z, 0.08 + 0.58 * z, 0.1 + 0.48 * z]
            }
        };
        p.map(|c| c.clamp(0.0, 1.0))
    })
}

fn make_noisy(clean: &Rgb32f, seed: u32, row_pattern: bool) -> Rgb32f {
    let mut out = clean.clone();
    for y in 0..clean.height {
        let row = if row_pattern { (hash_unit(seed ^ (y as u32 * 0x9e37)) - 0.5) * 0.005 } else { 0.0 };
        for x in 0..clean.width {
            let i = y * clean.width + x;
            for c in 0..3 {
                let signal = clean.data[i][c].max(0.0);
                let sigma = (signal * 0.012 + 0.00008).sqrt();
                let read = gaussian(seed ^ (x as u32).wrapping_mul(0x45d9f3b) ^ (y as u32).wrapping_mul(0x119de1f3), c as u32);
                out.data[i][c] = (signal + sigma * read * 0.18 + 0.002 * gaussian(seed, x as u32 ^ y as u32 ^ c as u32) + row).clamp(0.0, 1.0);
            }
        }
    }
    out
}

fn gaussian(seed: u32, salt: u32) -> f32 {
    let a = hash_unit(seed ^ salt.wrapping_mul(0x9e3779b9)).max(1.0e-6);
    let b = hash_unit(seed.rotate_left(13) ^ salt.wrapping_mul(0x85ebca6b));
    (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
}

fn hash_unit(mut x: u32) -> f32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    (x as f32) / (u32::MAX as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_metrics_have_zero_error_and_zero_psnr_option() {
        let image = Rgb32f::from_fn(2, 2, |x, y| [x as f32 * 0.2, y as f32 * 0.3, 0.4]);
        let got = metrics(&image, &image);
        assert_eq!(got.as_ref().unwrap()["mse"], 0.0);
        assert!(got.as_ref().unwrap()["psnrDbPeak1"].is_null());
        assert_eq!(got.as_ref().unwrap()["gradientRmse"], 0.0);
        let changed = Rgb32f::from_fn(2, 2, |x, y| [x as f32 * 0.2 + 0.1, y as f32 * 0.3, 0.4]);
        assert!(metrics(&image, &changed).unwrap()["mse"].as_f64().is_some_and(|v| v > 0.0));
    }

    #[test]
    fn constant_bias_has_analytic_mse_and_unchanged_gradients() {
        let clean = Rgb32f::filled(3, 3, [0.2, 0.3, 0.4]);
        let shifted = Rgb32f::filled(3, 3, [0.3, 0.3, 0.4]);
        let got = metrics(&clean, &shifted).unwrap();
        assert!((got["mse"].as_f64().unwrap() - (0.1f64 * 0.1 / 3.0)).abs() < 1e-8);
        assert_eq!(got["gradientRmse"], 0.0);
    }

    #[test]
    fn metrics_reject_malformed_storage_and_nonfinite_values() {
        let malformed = Rgb32f { width: 2, height: 2, data: vec![[0.0; 3]] };
        assert!(metrics(&malformed, &malformed).is_err());
        let nonfinite = Rgb32f::filled(1, 1, [f32::NAN; 3]);
        assert!(metrics(&nonfinite, &nonfinite).is_err());
    }

    #[test]
    fn procedural_cases_are_repeatable_finite_and_bounded() {
        for case in CASES {
            let clean = make_clean(32, case.name);
            let noisy = make_noisy(&clean, case.seed, case.row_pattern);
            assert!(clean.data.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            assert_eq!(noisy, make_noisy(&clean, case.seed, case.row_pattern));
            assert!(noisy.data.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        }
    }
}
