//! Pair generation from CC0 raws.
//!
//! A *window* is a square crop of a decoded raw's black-subtracted, white-normalised Bayer mosaic (the clean
//! signal). A *pair* is made from one window:
//!
//! 1. optional exposure reduction (a darker scene exposes fewer photons), clamped to the sensor range;
//! 2. clean target = demosaic(clean mosaic);
//! 3. noisy input = demosaic(clip_at_white(noise(clean mosaic))), the noise being the Poisson-Gaussian model of
//!    [`crate::noise`] applied per mosaic sample, so it is spatially correlated by the demosaic exactly as the
//!    production raw development makes it;
//! 4. both cropped by [`MARGIN`] (demosaic border effects), as `[3, p, p]` CHW buffers in camera RGB, white level 1,
//!    no white balance or colour matrix (so `(a, b)` stay exact per channel; the product colour contract is
//!    the open item 2 of the learned-RGB gate in `docs/denoise-qualification.md`).
//!
//! The clean base-ISO source still carries its own (small) native noise, as any "clean" raw does; sources whose
//! recorded ISO exceeds [`MAX_SOURCE_ISO`] are skipped.
//!
//! Splits: [`TrainPool`] only ever opens files of the split it is given; the caller passes `manifest.train`.
//! [`FixedSet`] builds the deterministic validation and held-out sets from `manifest.val` / `manifest.heldout`.

use crate::error::{Error, Result, data};
use crate::model::model_input_chw;
use crate::noise::{NoiseParams, SensorPrior, add_noise, reference_camera};
use crate::rng::Rng;
use crate::split::FileEntry;
use lightcraft_raw::{Cfa, Method, Normalized, demosaic};
use rayon::prelude::*;
use std::path::Path;

/// Pixels dropped from every side after demosaicing.
pub const MARGIN: usize = 8;
/// Clean sources must be base-ISO-ish; higher recorded ISO means the "clean" target is already noisy.
pub const MAX_SOURCE_ISO: u32 = 800;
/// Largest source dimension accepted (guards allocations from a hostile file).
const MAX_DIM: usize = 20_000;

/// A clean mosaic crop (`side × side`, side = patch + 2·MARGIN) with its CFA anchored at its (0, 0).
#[derive(Clone, Debug)]
pub struct MosaicWindow {
    pub side: usize,
    pub data: Vec<f32>,
    pub cfa: Cfa,
}

#[derive(Clone, Debug)]
pub struct Pair {
    /// `[3, p, p]` CHW.
    pub clean: Vec<f32>,
    pub noisy: Vec<f32>,
    pub params: NoiseParams,
}

/// How windows are chosen and pairs made.
#[derive(Clone, Debug)]
pub struct PairConfig {
    pub patch: usize,
    pub demosaic: Method,
    /// Largest exposure reduction in stops (a pair draws uniformly from `0..=max`).
    pub max_underexpose_ev: f64,
    /// Spread of the log-normal jitter on the `(a, b)` the network is told (not on the noise itself).
    pub conditioning_jitter: f64,
    pub prior: SensorPrior,
}

impl Default for PairConfig {
    fn default() -> Self {
        PairConfig { patch: 128, demosaic: Method::Ppg, max_underexpose_ev: 3.0, conditioning_jitter: 0.15, prior: SensorPrior::default() }
    }
}

impl PairConfig {
    pub fn window_side(&self) -> usize {
        self.patch + 2 * MARGIN
    }
}

/// Decode `path` and cut up to `count` windows of `side` pixels from it at deterministic pseudo-random
/// positions. Windows that are almost black or heavily clipped are skipped. Files that are not a usable
/// Bayer raw (X-Trans, linear DNG, too high ISO) are an `Err` the caller reports and skips.
pub fn load_windows(path: &Path, side: usize, count: usize, rng: &mut Rng) -> Result<Vec<MosaicWindow>> {
    let path_s = path.display().to_string();
    let bad = |why: String| Error::Raw { path: path_s.clone(), why };
    let bytes = std::fs::read(path)?;
    let raw = lightcraft_raw::decode(&bytes).map_err(|e| bad(e.to_string()))?;
    drop(bytes);
    if let Some(iso) = raw.metadata.iso
        && iso > MAX_SOURCE_ISO
    {
        return Err(bad(format!("recorded ISO {iso} is above {MAX_SOURCE_ISO}: not a clean source")));
    }
    let n: Normalized = raw.normalized().map_err(|e| bad(e.to_string()))?;
    drop(raw);
    let cfa = match (&n.cfa, n.cpp) {
        (Some(c), 1) if c.is_bayer() => c.clone(),
        _ => return Err(bad("not a single-plane Bayer mosaic".into())),
    };
    let (w, h) = (n.width, n.height);
    if w < side || h < side || w > MAX_DIM || h > MAX_DIM || n.data.len() != w * h {
        return Err(bad(format!("image {w}x{h} is unusable for {side}-pixel windows")));
    }
    let mut out = Vec::with_capacity(count);
    let mut attempts = 0usize;
    while out.len() < count && attempts < count * 12 {
        attempts += 1;
        // even offsets keep the CFA phase of the window equal to the source's
        let x0 = rng.below((w - side) / 2 + 1) * 2;
        let y0 = rng.below((h - side) / 2 + 1) * 2;
        let mut win = Vec::with_capacity(side * side);
        for y in 0..side {
            let row = (y0 + y) * w + x0;
            let Some(r) = n.data.get(row..row + side) else { return Err(bad("window outside the image".into())) };
            win.extend(r.iter().map(|&v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }));
        }
        let mean = win.iter().map(|&v| v as f64).sum::<f64>() / win.len() as f64;
        let clipped = win.iter().filter(|&&v| v >= 0.999).count() as f64 / win.len() as f64;
        if mean < 0.004 || clipped > 0.1 {
            continue;
        }
        out.push(MosaicWindow { side, data: win, cfa: cfa.shifted(x0, y0) });
    }
    if out.is_empty() {
        return Err(bad("no usable window (too dark or clipped)".into()));
    }
    Ok(out)
}

fn demosaic_crop(mosaic: Vec<f32>, win: &MosaicWindow, patch: usize, method: Method) -> Vec<f32> {
    let n = Normalized { width: win.side, height: win.side, cpp: 1, data: mosaic, cfa: Some(win.cfa.clone()) };
    let rgb = demosaic(&n, method);
    let mut out = vec![0f32; 3 * patch * patch];
    for y in 0..patch {
        for x in 0..patch {
            if let Some(px) = rgb.data.get((y + MARGIN) * win.side + x + MARGIN) {
                for c in 0..3 {
                    out[c * patch * patch + y * patch + x] = if px[c].is_finite() { px[c] } else { 0.0 };
                }
            }
        }
    }
    out
}

/// Make the pair for `win` with noise `params` (what the sensor really did) and an exposure gain.
pub fn make_pair(win: &MosaicWindow, cfg: &PairConfig, params: NoiseParams, gain: f32, rng: &mut Rng) -> Pair {
    let clean_mosaic: Vec<f32> = win.data.iter().map(|&v| (v * gain).clamp(0.0, 1.0)).collect();
    let noisy_mosaic: Vec<f32> = add_noise(&clean_mosaic, params, rng).into_iter().map(|v| v.min(1.0)).collect();
    let clean = demosaic_crop(clean_mosaic, win, cfg.patch, cfg.demosaic);
    let clean = clean.into_iter().map(|v| v.clamp(0.0, 1.0)).collect();
    let noisy = demosaic_crop(noisy_mosaic, win, cfg.patch, cfg.demosaic);
    Pair { clean, noisy, params }
}

/// Apply one of the 8 dihedral transforms to a `[c, n, n]` CHW buffer (`code` 0..8; 0 is the identity).
pub fn dihedral(src: &[f32], c: usize, n: usize, code: u8) -> Vec<f32> {
    let (flip_h, flip_v, transpose) = (code & 1 != 0, code & 2 != 0, code & 4 != 0);
    let mut dst = vec![0f32; src.len()];
    for ch in 0..c {
        for y in 0..n {
            for x in 0..n {
                let (mut sx, mut sy) = (x, y);
                if transpose {
                    std::mem::swap(&mut sx, &mut sy);
                }
                if flip_h {
                    sx = n - 1 - sx;
                }
                if flip_v {
                    sy = n - 1 - sy;
                }
                if let (Some(d), Some(s)) = (dst.get_mut(ch * n * n + y * n + x), src.get(ch * n * n + sy * n + sx)) {
                    *d = *s;
                }
            }
        }
    }
    dst
}

/// One training example in the shape the network and loss use: `input` is `[6, p, p]`, `clean` `[3, p, p]`,
/// `weight` `[3, p, p]` is the per-sample loss weight (see [`loss_weights`]).
#[derive(Clone, Debug)]
pub struct Example {
    pub input: Vec<f32>,
    pub clean: Vec<f32>,
    pub weight: Vec<f32>,
    pub params: NoiseParams,
}

/// `1 / (sqrt(clean) + 0.03)`: an L1 loss weighted so errors count roughly like errors in a square-root encoding
/// (first-order `|sqrt(p) - sqrt(c)| ~ |p - c| / (2 sqrt(c))`), so a dark patch is not drowned by a bright one.
pub fn loss_weights(clean: &[f32]) -> Vec<f32> {
    clean.iter().map(|&c| 1.0 / (c.clamp(0.0, 1.0).sqrt() + 0.03)).collect()
}

fn to_example(pair: &Pair, tell: NoiseParams) -> Example {
    Example { input: model_input_chw(&pair.noisy, tell), weight: loss_weights(&pair.clean), clean: pair.clean.clone(), params: pair.params }
}

/// A ring of clean windows from the training files, refreshed a file at a time.
pub struct TrainPool {
    files: Vec<FileEntry>,
    order: Vec<usize>,
    next: usize,
    windows: Vec<MosaicWindow>,
    capacity: usize,
    per_file: usize,
    cursor: usize,
    cfg: PairConfig,
    seed: u64,
    visits: u64,
    /// Files that failed to load (reported once, then skipped).
    pub skipped: Vec<(String, String)>,
}

impl TrainPool {
    /// `files` must be `manifest.train` only.
    pub fn new(files: &[FileEntry], cfg: PairConfig, capacity: usize, per_file: usize, seed: u64) -> Result<TrainPool> {
        if files.is_empty() || capacity == 0 || per_file == 0 {
            return data("the training pool needs files, a capacity and windows per file");
        }
        let mut order: Vec<usize> = (0..files.len()).collect();
        let mut rng = Rng::from_parts(&[seed, 0x5eed]);
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        let mut pool = TrainPool {
            files: files.to_vec(),
            order,
            next: 0,
            windows: Vec::new(),
            capacity,
            per_file,
            cursor: 0,
            cfg,
            seed,
            visits: 0,
            skipped: Vec::new(),
        };
        // fill until full, or until every file has been tried once and failed to fill it
        let mut tried = 0usize;
        while pool.windows.len() < pool.capacity && tried < pool.files.len().max(1) * 4 {
            pool.visit_next();
            tried += 1;
        }
        if pool.windows.is_empty() {
            return data(format!("no training file yielded a usable window; skipped: {:?}", pool.skipped));
        }
        Ok(pool)
    }

    /// Decode the next file of the shuffled order and put its windows into the ring.
    pub fn visit_next(&mut self) {
        let Some(&idx) = self.order.get(self.next % self.order.len().max(1)) else { return };
        self.next += 1;
        self.visits += 1;
        let Some(entry) = self.files.get(idx) else { return };
        let mut rng = Rng::from_parts(&[self.seed, 0xf11e, self.visits]);
        match load_windows(&entry.path, self.cfg.window_side(), self.per_file, &mut rng) {
            Ok(ws) => {
                for w in ws {
                    if self.windows.len() < self.capacity {
                        self.windows.push(w);
                    } else {
                        let at = self.cursor % self.capacity;
                        if let Some(slot) = self.windows.get_mut(at) {
                            *slot = w;
                        }
                        self.cursor += 1;
                    }
                }
            }
            Err(e) => {
                let key = entry.path.display().to_string();
                if !self.skipped.iter().any(|(k, _)| *k == key) {
                    self.skipped.push((key, e.to_string()));
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// A batch for training step `step`: every example's randomness comes from `(seed, step, index)`, so the
    /// batch is the same whatever the thread count.
    pub fn batch(&self, step: u64, size: usize) -> Vec<Example> {
        let cfg = &self.cfg;
        (0..size)
            .into_par_iter()
            .filter_map(|i| {
                let mut rng = Rng::from_parts(&[self.seed, 0xba7c, step, i as u64]);
                let win = self.windows.get(rng.below(self.windows.len()))?;
                let params = cfg.prior.sample_params(&mut rng);
                let gain = (-(rng.uniform() * cfg.max_underexpose_ev)).exp2() as f32;
                let pair = make_pair(win, cfg, params, gain, &mut rng);
                let tell = params.jittered(cfg.conditioning_jitter, &mut rng);
                let code = (rng.next_u64() % 8) as u8;
                let n = cfg.patch;
                let aug = Pair { clean: dihedral(&pair.clean, 3, n, code), noisy: dihedral(&pair.noisy, 3, n, code), params };
                Some(to_example(&aug, tell))
            })
            .collect()
    }
}

/// A deterministic example set from a list of files (validation or held-out): fixed windows, fixed noise.
pub struct FixedSet {
    pub patch: usize,
    /// `(iso, example)`; `iso` is 0 for examples drawn from the prior rather than a stratum.
    pub examples: Vec<(f64, Example)>,
    pub skipped: Vec<(String, String)>,
}

impl FixedSet {
    /// `windows_per_file` windows per file. With `strata` empty every window gets one random (camera, ISO)
    /// from the prior; otherwise every window is rendered once per ISO of `strata` on the reference camera.
    /// Everything derives from `seed` and the file's content hash, so the set does not change between runs or
    /// when other files are added.
    pub fn build(files: &[FileEntry], cfg: &PairConfig, windows_per_file: usize, strata: &[f64], seed: u64) -> Result<FixedSet> {
        let mut set = FixedSet { patch: cfg.patch, examples: Vec::new(), skipped: Vec::new() };
        let cam = reference_camera();
        for entry in files {
            let key = u64::from_str_radix(entry.sha256.get(..16).unwrap_or("0"), 16).unwrap_or(0);
            let mut rng = Rng::from_parts(&[seed, 0xf1d5, key]);
            let wins = match load_windows(&entry.path, cfg.window_side(), windows_per_file, &mut rng) {
                Ok(w) => w,
                Err(e) => {
                    set.skipped.push((entry.path.display().to_string(), e.to_string()));
                    continue;
                }
            };
            for (wi, win) in wins.iter().enumerate() {
                let mut one = |iso: f64, params: NoiseParams, tag: u64| {
                    let mut r = Rng::from_parts(&[seed, key, wi as u64, tag]);
                    let pair = make_pair(win, cfg, params, 1.0, &mut r);
                    // evaluation tells the network the exact parameters: no conditioning jitter
                    set.examples.push((iso, to_example(&pair, params)));
                };
                if strata.is_empty() {
                    let mut r = Rng::from_parts(&[seed, key, wi as u64, 0xcafe]);
                    let (c, iso) = cfg.prior.sample(&mut r);
                    one(0.0, c.params_at(iso), 0);
                } else {
                    for (si, &iso) in strata.iter().enumerate() {
                        one(iso, cam.params_at(iso), si as u64 + 1);
                    }
                }
            }
        }
        if set.examples.is_empty() {
            return data(format!("no usable example from {} files; skipped: {:?}", files.len(), set.skipped));
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic RGGB window with a smooth gradient and some structure; no raw file needed.
    fn window(side: usize, level: f32) -> MosaicWindow {
        let cfa = Cfa::bayer("RGGB").unwrap();
        let data = (0..side * side)
            .map(|i| {
                let (x, y) = (i % side, i / side);
                level * (0.5 + 0.5 * ((x as f32 / 9.0).sin() * (y as f32 / 7.0).cos()))
            })
            .collect();
        MosaicWindow { side, data, cfa }
    }

    fn cfg() -> PairConfig {
        PairConfig { patch: 32, ..PairConfig::default() }
    }

    #[test]
    fn dihedral_is_a_permutation_and_code_zero_is_the_identity() {
        let n = 5;
        let src: Vec<f32> = (0..3 * n * n).map(|i| i as f32).collect();
        assert_eq!(dihedral(&src, 3, n, 0), src);
        for code in 0..8u8 {
            let mut got = dihedral(&src, 3, n, code);
            got.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(got, src, "code {code} loses or repeats samples");
        }
        // the 8 codes give 8 distinct images of an asymmetric source
        let mut seen = std::collections::HashSet::new();
        for code in 0..8u8 {
            seen.insert(dihedral(&src, 3, n, code).iter().map(|v| *v as i64).collect::<Vec<_>>());
        }
        assert_eq!(seen.len(), 8);
    }

    #[test]
    fn a_pair_has_the_patch_shape_and_the_clean_target_is_in_range() {
        let c = cfg();
        let win = window(c.window_side(), 0.6);
        let params = NoiseParams { a: 2e-3, b: 1e-4 };
        let pair = make_pair(&win, &c, params, 1.0, &mut Rng::new(1));
        assert_eq!(pair.clean.len(), 3 * 32 * 32);
        assert_eq!(pair.noisy.len(), pair.clean.len());
        assert!(pair.clean.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(pair.noisy.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn the_noisy_input_differs_from_clean_by_about_the_modelled_sigma() {
        let c = cfg();
        let win = window(c.window_side(), 0.5);
        let params = NoiseParams { a: 4e-3, b: 4e-4 };
        let pair = make_pair(&win, &c, params, 1.0, &mut Rng::new(2));
        let n = pair.clean.len() as f64;
        let mse = pair.clean.iter().zip(&pair.noisy).map(|(c, y)| ((c - y) as f64).powi(2)).sum::<f64>() / n;
        let mean_clean = pair.clean.iter().map(|&v| v as f64).sum::<f64>() / n;
        let raw_sigma = params.sigma_at(mean_clean as f32) as f64;
        // demosaicing averages neighbours, so the RGB error is somewhat below the raw sigma, not far above it
        let rmse = mse.sqrt();
        assert!(rmse > 0.3 * raw_sigma && rmse < 2.0 * raw_sigma, "rmse {rmse} vs raw sigma {raw_sigma}");
    }

    #[test]
    fn pairs_are_reproducible_and_noise_differs_between_seeds() {
        let c = cfg();
        let win = window(c.window_side(), 0.4);
        let p = NoiseParams { a: 1e-3, b: 1e-4 };
        let a = make_pair(&win, &c, p, 1.0, &mut Rng::new(5));
        let b = make_pair(&win, &c, p, 1.0, &mut Rng::new(5));
        let d = make_pair(&win, &c, p, 1.0, &mut Rng::new(6));
        assert_eq!(a.noisy, b.noisy);
        assert_ne!(a.noisy, d.noisy);
        assert_eq!(a.clean, d.clean, "the clean target does not depend on the noise draw");
    }

    #[test]
    fn exposure_gain_darkens_the_clean_target() {
        let c = cfg();
        let win = window(c.window_side(), 0.8);
        let p = NoiseParams { a: 1e-3, b: 1e-5 };
        let bright = make_pair(&win, &c, p, 1.0, &mut Rng::new(1));
        let dark = make_pair(&win, &c, p, 0.25, &mut Rng::new(1));
        let mean = |v: &[f32]| v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
        assert!((mean(&dark.clean) / mean(&bright.clean) - 0.25).abs() < 0.02);
    }

    #[test]
    fn loss_weights_favour_dark_samples_and_stay_finite() {
        let w = loss_weights(&[0.0, 0.01, 0.25, 1.0, -1.0, 7.0]);
        assert!(w.iter().all(|v| v.is_finite() && *v > 0.0));
        assert!(w[0] > w[1] && w[1] > w[2] && w[2] > w[3]);
        assert_eq!(w[0], w[4]);
        assert_eq!(w[3], w[5]);
    }

    #[test]
    fn training_never_opens_a_file_outside_the_list_it_is_given() {
        // an unreadable path is skipped and reported; with nothing usable the pool refuses to start
        let entry = FileEntry { path: "definitely/not/here.arw".into(), sha256: "0".repeat(64) };
        let res = TrainPool::new(&[entry], cfg(), 8, 2, 1);
        assert!(res.is_err());
        assert!(TrainPool::new(&[], cfg(), 8, 2, 1).is_err());
    }
}
