//! The qualification eval: reports the metrics of the learned-RGB gate in `docs/denoise-qualification.md` for a
//! trained network, on the held-out split only.
//!
//! What it measures (the gate's numbered items):
//! * colour / transfer (item 2): signed per-channel bias and the shift of the R/G, B/G ratios, per ISO stratum;
//! * whole-image vs tiled output (item 3): max / mean / p99 absolute difference;
//! * hallucination (item 3): procedural flats, steps, a soft edge and fine texture at two ISOs: residual
//!   structure in flats, edge overshoot, gradient energy kept;
//! * latency (item 4): first call and warm p50 / p95 for one whole-image size on the device used. Peak memory and
//!   named Mac/Windows targets are NOT measured here;
//! * held-out pixel error (item 5, the automatable part): MSE, PSNR, encoded PSNR, gradient RMSE per ISO stratum
//!   against the identity (noisy) input.
//!
//! Item 5's human preference and the classical-NR comparison are not computed. The report is always
//! `UNQUALIFIED`: the thresholds in `checks` are provisional values proposed by this tool, not ratified in the
//! protocol document, and a pass on them is necessary evidence, not qualification.

use crate::data::{FixedSet, PairConfig};
use crate::error::Result;
use crate::metrics::{self, Metrics};
use crate::model::{NafNet, model_input_chw};
use crate::noise::{STRATA_ISO, add_noise, reference_camera};
use crate::rng::Rng;
use crate::split::FileEntry;
use candle_core::Device;
use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct EvalConfig {
    pub pair: PairConfig,
    pub windows_per_file: usize,
    pub seed: u64,
    pub tile: usize,
    pub overlap: usize,
    /// Side of the whole-image latency probe.
    pub latency_size: usize,
    pub latency_repeats: usize,
    /// How many held-out examples the tiling comparison uses.
    pub tiling_examples: usize,
}

impl Default for EvalConfig {
    fn default() -> Self {
        EvalConfig {
            pair: PairConfig { patch: 256, ..PairConfig::default() },
            windows_per_file: 2,
            seed: 20_261_016,
            tile: 128,
            overlap: 32,
            latency_size: 512,
            latency_repeats: 5,
            tiling_examples: 6,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StratumReport {
    pub iso: f64,
    pub examples: usize,
    pub identity: Metrics,
    pub model: Metrics,
    pub psnr_gain_db: Option<f64>,
    pub encoded_psnr_gain_db: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TilingReport {
    pub tile: usize,
    pub overlap: usize,
    pub examples: usize,
    pub max_abs_diff: f64,
    pub p99_abs_diff: f64,
    pub mean_abs_diff: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PatternReport {
    pub pattern: &'static str,
    pub iso: f64,
    pub identity: Metrics,
    pub model: Metrics,
    /// Spatial standard deviation of the output inside the region (0 is a clean flat).
    pub identity_region_std: f64,
    pub model_region_std: f64,
    /// Amount the output exceeds the clean range inside the region, in clean-range units.
    pub identity_overshoot: f64,
    pub model_overshoot: f64,
    /// Output gradient energy over clean gradient energy; `None` when the clean pattern has none.
    pub gradient_energy_ratio: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LatencyReport {
    pub device: String,
    pub size: usize,
    pub first_ms: f64,
    pub warm_p50_ms: f64,
    pub warm_p95_ms: f64,
    pub repeats: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub pass: bool,
    pub value: f64,
    pub threshold: f64,
    pub note: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvalReport {
    /// Always `UNQUALIFIED`: see the module docs.
    pub status: &'static str,
    pub heldout_files: usize,
    pub skipped_files: Vec<(String, String)>,
    pub strata: Vec<StratumReport>,
    pub tiling: TilingReport,
    pub patterns: Vec<PatternReport>,
    pub latency: LatencyReport,
    pub checks: Vec<Check>,
    pub checks_are_provisional: &'static str,
    pub not_measured: Vec<&'static str>,
}

/// Procedural clean RGB patterns (linear, `[3, n, n]`), with the region the hallucination metrics read
/// (inside an 8-pixel border so zero padding at the image edge does not count).
fn patterns(n: usize) -> Vec<(&'static str, Vec<f32>)> {
    let make = |f: &dyn Fn(usize, usize) -> f32| -> Vec<f32> {
        let mut v = vec![0f32; 3 * n * n];
        for c in 0..3 {
            for y in 0..n {
                for x in 0..n {
                    v[c * n * n + y * n + x] = f(x, y);
                }
            }
        }
        v
    };
    let half = n / 2;
    vec![
        ("flat_shadow", make(&|_, _| 0.02)),
        ("flat_mid", make(&|_, _| 0.18)),
        ("flat_bright", make(&|_, _| 0.7)),
        ("hard_step", make(&|x, _| if x < half { 0.05 } else { 0.5 })),
        (
            "soft_edge",
            make(&|x, _| {
                let t = ((x as f32 - (half as f32 - 12.0)) / 24.0).clamp(0.0, 1.0);
                0.05 + 0.45 * t * t * (3.0 - 2.0 * t)
            }),
        ),
        ("fine_texture", make(&|x, y| 0.2 + 0.08 * ((x as f32 * 1.1).sin() * (y as f32 * 0.9).cos()))),
    ]
}

fn region(n: usize, border: usize) -> impl Iterator<Item = (usize, usize)> {
    (border..n.saturating_sub(border)).flat_map(move |y| (border..n.saturating_sub(border)).map(move |x| (x, y)))
}

fn region_values(img: &[f32], n: usize, border: usize) -> Vec<f32> {
    let mut v = Vec::new();
    for c in 0..3 {
        for (x, y) in region(n, border) {
            if let Some(&s) = img.get(c * n * n + y * n + x) {
                v.push(s);
            }
        }
    }
    v
}

fn overshoot(out: &[f32], clean: &[f32], n: usize, border: usize) -> f64 {
    let o = region_values(out, n, border);
    let c = region_values(clean, n, border);
    let omax = o.iter().copied().fold(f32::MIN, f32::max) as f64;
    let omin = o.iter().copied().fold(f32::MAX, f32::min) as f64;
    let cmax = c.iter().copied().fold(f32::MIN, f32::max) as f64;
    let cmin = c.iter().copied().fold(f32::MAX, f32::min) as f64;
    (omax - cmax).max(0.0) + (cmin - omin).max(0.0)
}

fn run_patterns(net: &NafNet, device: &Device, seed: u64) -> Result<Vec<PatternReport>> {
    let n = 128usize;
    let cam = reference_camera();
    let mut out = Vec::new();
    for (pi, (name, clean)) in patterns(n).into_iter().enumerate() {
        for &iso in &[3200.0f64, 12_800.0] {
            let params = cam.params_at(iso);
            let mut rng = Rng::from_parts(&[seed, 0x9a77, pi as u64, iso as u64]);
            let noisy = add_noise(&clean, params, &mut rng);
            let result = net.infer(&model_input_chw(&noisy, params), n, n, device)?;
            let b = 8;
            let (nr, rr) = (region_values(&noisy, n, b), region_values(&result, n, b));
            let ge_clean = metrics::gradient_energy(&clean, n, n);
            out.push(PatternReport {
                pattern: name,
                iso,
                identity: metrics::measure(&noisy, &clean, n, n),
                model: metrics::measure(&result, &clean, n, n),
                identity_region_std: metrics::std_dev(&nr),
                model_region_std: metrics::std_dev(&rr),
                identity_overshoot: overshoot(&noisy, &clean, n, b),
                model_overshoot: overshoot(&result, &clean, n, b),
                gradient_energy_ratio: (ge_clean > 1e-12).then(|| metrics::gradient_energy(&result, n, n) / ge_clean),
            });
        }
    }
    Ok(out)
}

fn run_latency(net: &NafNet, device: &Device, size: usize, repeats: usize) -> Result<LatencyReport> {
    let params = reference_camera().params_at(3200.0);
    let mut rng = Rng::new(77);
    let clean = vec![0.2f32; 3 * size * size];
    let noisy = add_noise(&clean, params, &mut rng);
    let input = model_input_chw(&noisy, params);
    let t = Instant::now();
    net.infer(&input, size, size, device)?;
    let first_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut warm = Vec::new();
    for _ in 0..repeats.max(1) {
        let t = Instant::now();
        net.infer(&input, size, size, device)?;
        warm.push(t.elapsed().as_secs_f64() * 1e3);
    }
    Ok(LatencyReport {
        device: format!("{device:?}"),
        size,
        first_ms,
        warm_p50_ms: metrics::quantile(&warm, 0.5),
        warm_p95_ms: metrics::quantile(&warm, 0.95),
        repeats: warm.len(),
    })
}

/// Evaluate `net` on the held-out files (`manifest.heldout` only: never train or val files).
pub fn evaluate(net: &NafNet, heldout: &[FileEntry], cfg: &EvalConfig, device: &Device) -> Result<EvalReport> {
    let set = FixedSet::build(heldout, &cfg.pair, cfg.windows_per_file, &STRATA_ISO, cfg.seed)?;
    let n = set.patch;
    let mut by_iso: Vec<(f64, Vec<Metrics>, Vec<Metrics>, f64, f64, usize)> = STRATA_ISO.iter().map(|&i| (i, vec![], vec![], 0.0, 0.0, 0)).collect();
    let mut tiling_diffs: Vec<f64> = Vec::new();
    let mut tiling_used = 0usize;
    let mut tiling_max = 0.0f64;
    for (iso, ex) in &set.examples {
        let out = net.infer(&ex.input, n, n, device)?;
        let noisy_rgb = ex.input.get(..3 * n * n).unwrap_or(&[]);
        let Some(slot) = by_iso.iter_mut().find(|s| s.0 == *iso) else { continue };
        slot.1.push(metrics::measure(noisy_rgb, &ex.clean, n, n));
        slot.2.push(metrics::measure(&out, &ex.clean, n, n));
        slot.3 += metrics::encoded_mse(noisy_rgb, &ex.clean);
        slot.4 += metrics::encoded_mse(&out, &ex.clean);
        slot.5 += 1;
        if tiling_used < cfg.tiling_examples {
            let tiled = net.infer_tiled(&ex.input, n, n, cfg.tile, cfg.overlap, device)?;
            for (a, b) in tiled.iter().zip(&out) {
                let d = (*a as f64 - *b as f64).abs();
                tiling_max = tiling_max.max(d);
                tiling_diffs.push(d);
            }
            tiling_used += 1;
        }
    }
    let strata: Vec<StratumReport> = by_iso
        .into_iter()
        .filter(|s| s.5 > 0)
        .map(|(iso, id, md, enc_id, enc_md, count)| {
            let identity = metrics::mean_metrics(&id, enc_id / count as f64);
            let model = metrics::mean_metrics(&md, enc_md / count as f64);
            let gain = |a: Option<f64>, b: Option<f64>| match (a, b) {
                (Some(x), Some(y)) => Some(y - x),
                _ => None,
            };
            StratumReport {
                iso,
                examples: count,
                psnr_gain_db: gain(identity.psnr_db, model.psnr_db),
                encoded_psnr_gain_db: gain(identity.encoded_psnr_db, model.encoded_psnr_db),
                identity,
                model,
            }
        })
        .collect();
    let tiling = TilingReport {
        tile: cfg.tile,
        overlap: cfg.overlap,
        examples: tiling_used,
        max_abs_diff: tiling_max,
        p99_abs_diff: metrics::quantile(&tiling_diffs, 0.99),
        mean_abs_diff: if tiling_diffs.is_empty() { 0.0 } else { tiling_diffs.iter().sum::<f64>() / tiling_diffs.len() as f64 },
    };
    let patterns = run_patterns(net, device, cfg.seed)?;
    let latency = run_latency(net, device, cfg.latency_size, cfg.latency_repeats)?;
    let checks = provisional_checks(&strata, &tiling, &patterns);
    Ok(EvalReport {
        status: "UNQUALIFIED",
        heldout_files: heldout.len(),
        skipped_files: set.skipped,
        strata,
        tiling,
        patterns,
        latency,
        checks,
        checks_are_provisional: "thresholds are proposed by denoise-train, not ratified in docs/denoise-qualification.md; passing them is necessary evidence only",
        not_measured: vec![
            "human preference on held-out rendered comparison (gate item 5)",
            "comparison against classical noise reduction (gate item 5)",
            "peak memory and latency on named Mac/Windows targets (gate item 4)",
            "independent reference for the colour/transfer contract in the product colour space (gate item 2)",
            "consented shoot-disjoint camera/ISO strata (outside the CC0-only scope)",
        ],
    })
}

/// Provisional pass/fail values over the numbers above.
pub fn provisional_checks(strata: &[StratumReport], tiling: &TilingReport, patterns: &[PatternReport]) -> Vec<Check> {
    let mut checks = Vec::new();
    let worst_gain = strata.iter().filter_map(|s| s.psnr_gain_db).fold(f64::INFINITY, f64::min);
    checks.push(Check {
        name: "never worse than the noisy input (every ISO stratum)",
        pass: strata.iter().all(|s| s.psnr_gain_db.is_some_and(|g| g > 0.0)),
        value: if worst_gain.is_finite() { worst_gain } else { f64::NAN },
        threshold: 0.0,
        note: "PSNR gain in dB, smallest over strata",
    });
    let high = strata.iter().filter(|s| s.iso >= 3200.0).filter_map(|s| s.psnr_gain_db).fold(f64::INFINITY, f64::min);
    checks.push(Check {
        name: "denoises at high ISO (>= 3200)",
        pass: high.is_finite() && high >= 2.0,
        value: if high.is_finite() { high } else { f64::NAN },
        threshold: 2.0,
        note: "smallest PSNR gain in dB over strata at ISO 3200 and above",
    });
    let bias = strata.iter().flat_map(|s| s.model.bias.iter().map(|b| b.abs())).fold(0.0f64, f64::max);
    checks.push(Check {
        name: "no channel bias",
        pass: bias <= 0.003,
        value: bias,
        threshold: 0.003,
        note: "largest |mean(output - clean)| over channels and strata, linear units (white = 1)",
    });
    let chroma = strata.iter().flat_map(|s| s.model.chroma_shift.iter().map(|b| b.abs())).fold(0.0f64, f64::max);
    checks.push(Check {
        name: "no chromaticity shift",
        pass: chroma <= 0.02,
        value: chroma,
        threshold: 0.02,
        note: "largest |mean change of R/G or B/G| over strata",
    });
    checks.push(Check {
        name: "tiled equals whole-image",
        pass: tiling.max_abs_diff <= 0.02,
        value: tiling.max_abs_diff,
        threshold: 0.02,
        note: "max absolute difference, linear units",
    });
    let flats: Vec<&PatternReport> = patterns.iter().filter(|p| p.pattern.starts_with("flat")).collect();
    let flat_ratio = flats.iter().map(|p| p.model_region_std / p.identity_region_std.max(1e-12)).fold(0.0f64, f64::max);
    checks.push(Check {
        name: "no structure invented in flats",
        pass: flat_ratio <= 1.0,
        value: flat_ratio,
        threshold: 1.0,
        note: "largest output / noisy-input spatial std over the flat patterns (below 1: noise only removed)",
    });
    let edges: Vec<&PatternReport> = patterns.iter().filter(|p| p.pattern.ends_with("edge") || p.pattern.ends_with("step")).collect();
    let over = edges.iter().map(|p| p.model_overshoot - p.identity_overshoot).fold(f64::NEG_INFINITY, f64::max);
    checks.push(Check {
        name: "no added edge overshoot",
        pass: over <= 0.0,
        value: if over.is_finite() { over } else { f64::NAN },
        threshold: 0.0,
        note: "model overshoot minus noisy-input overshoot, worst edge pattern",
    });
    let texture = patterns.iter().filter(|p| p.pattern == "fine_texture").filter_map(|p| p.gradient_energy_ratio).fold(f64::INFINITY, f64::min);
    checks.push(Check {
        name: "fine texture kept",
        pass: texture.is_finite() && (0.3..=1.5).contains(&texture),
        value: if texture.is_finite() { texture } else { f64::NAN },
        threshold: 0.3,
        note: "output / clean gradient energy of the texture pattern; includes residual noise, so read with the pixel metrics",
    });
    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NafConfig, zero_init_ending};
    use candle_core::DType;
    use candle_nn::{VarBuilder, VarMap};

    #[test]
    fn procedural_patterns_are_in_range_and_flats_are_flat() {
        for (name, p) in patterns(64) {
            assert_eq!(p.len(), 3 * 64 * 64);
            assert!(p.iter().all(|v| (0.0..=1.0).contains(v)), "{name}");
            if name.starts_with("flat") {
                assert_eq!(metrics::gradient_energy(&p, 64, 64), 0.0, "{name}");
            }
        }
        let tex = patterns(64).into_iter().find(|(n, _)| *n == "fine_texture").unwrap().1;
        assert!(metrics::gradient_energy(&tex, 64, 64) > 1e-4);
    }

    #[test]
    fn overshoot_counts_only_excursions_outside_the_clean_range() {
        let n = 32;
        let clean = vec![0.5f32; 3 * n * n];
        let mut out = clean.clone();
        out[n * n / 2 + 10] = 0.6;
        out[10 * n + 12] = 0.45;
        let o = overshoot(&out, &clean, n, 8);
        assert!((o - 0.15).abs() < 1e-6, "{o}");
        assert_eq!(overshoot(&clean, &clean, n, 8), 0.0);
    }

    #[test]
    fn the_untrained_identity_network_scores_zero_gain_and_the_checks_say_so() {
        // zero ending weights: the network returns its noisy input, so the model must tie the identity and the
        // "denoises at high ISO" check must fail while the no-regression pattern checks hold
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu);
        let net = NafNet::new(&NafConfig::tiny(), vb).unwrap();
        zero_init_ending(&varmap).unwrap();
        // also zero the ending bias so the output is exactly the input
        {
            let data = varmap.data().lock().unwrap();
            let b = data.get("ending.bias").unwrap();
            b.set(&b.as_tensor().zeros_like().unwrap()).unwrap();
        }
        let patterns = run_patterns(&net, &Device::Cpu, 1).unwrap();
        assert!(!patterns.is_empty());
        for p in &patterns {
            assert!((p.model.mse - p.identity.mse).abs() < 1e-9, "{}: identity network changed the image", p.pattern);
        }
        let tiling = TilingReport { tile: 128, overlap: 32, examples: 1, max_abs_diff: 0.0, p99_abs_diff: 0.0, mean_abs_diff: 0.0 };
        let strata = vec![StratumReport {
            iso: 3200.0,
            examples: 1,
            identity: Metrics::default(),
            model: Metrics::default(),
            psnr_gain_db: Some(0.0),
            encoded_psnr_gain_db: Some(0.0),
        }];
        let checks = provisional_checks(&strata, &tiling, &patterns);
        let get = |name: &str| checks.iter().find(|c| c.name.starts_with(name)).unwrap().pass;
        assert!(!get("denoises at high ISO"));
        assert!(!get("never worse"), "zero gain is not an improvement");
        assert!(get("tiled equals"));
    }

    #[test]
    fn the_report_serialises_with_the_unqualified_status() {
        let r = LatencyReport { device: "Cpu".into(), size: 8, first_ms: 1.0, warm_p50_ms: 0.5, warm_p95_ms: 0.7, repeats: 3 };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("warm_p95_ms"));
    }
}
