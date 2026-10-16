//! Classical raw-domain denoise on the CFA mosaic, before demosaicing (issue #201). Opt-in; off changes nothing.
//!
//! Original implementation from the published descriptions below (no code of darktable, RawTherapee, ART, LibRaw
//! or any other raw tool is used or ported):
//!
//! 1. **Noise model.** A black-subtracted, white-scaled sample has variance `a·x + b` (Poisson shot noise with
//!    gain `a`, plus Gaussian read noise of variance `b`), the model of A. Foi, M. Trimeche, V. Katkovnik and
//!    K. Egiazarian, "Practical Poissonian-Gaussian noise modeling and fitting for single-image raw-data",
//!    IEEE TIP 17(10), 2008. `(a, b)` are fitted per CFA colour from the image itself: on small blocks of each
//!    plane the noise level is read off a robust (median absolute deviation) estimate of a high-pass residual, and
//!    a line is fitted through the flat-block variances against block means, as in that paper's flat-area fit.
//! 2. **Variance stabilisation.** The generalised Anscombe transform (GAT) of the same paper turns the
//!    signal-dependent noise into approximately unit-variance Gaussian noise: `f(z) = 2/a·sqrt(a·z + 3/8·a² + b)`.
//! 3. **Non-local means** (A. Buades, B. Coll, J.-M. Morel, "A non-local algorithm for image denoising",
//!    CVPR 2005) on each of the four sub-sampled mosaic planes (the pixels of one row/column parity, so neighbours
//!    are same-colour). Patch distances are computed for a whole search offset at once with box sums, the
//!    fast formulation common to non-local means implementations, and the expected `2σ²` of two noisy copies of
//!    one patch is subtracted before weighting (bias-corrected distance, as in Kervrann & Boulanger, "Optimal
//!    spatial adaptation for patch-based image denoising", IEEE TIP 15(10), 2006). Noise is `σ = 1` after step 2,
//!    so one filtering parameter serves every plane and brightness.
//! 4. **Inverse.** The closed-form approximation of the exact unbiased inverse of the GAT, M. Mäkitalo and A. Foi,
//!    "Optimal inversion of the generalized Anscombe transformation for Poisson-Gaussian noise", IEEE TIP 22(1),
//!    2013 (the Poisson-Anscombe case: Mäkitalo & Foi, IEEE TIP 20(9), 2011), which avoids the bias of the plain
//!    algebraic inverse at low light.
//!
//! Work is CPU only (rayon) in overlapping tiles, so memory stays bounded and a tile border is exact. The work
//! per pixel is bounded: the search window shrinks deterministically for very large images
//! ([`OFFSET_MPIX_BUDGET`]), so cost per megapixel has a ceiling and the result never depends on timing
//! (preview = export). Samples at or above [`KEEP_ABOVE`] (clipped highlights) are left as they were, so
//! highlight reconstruction still finds them. Only Bayer mosaics of one sample per pixel are processed; anything
//! else, or an image too small for a patch search, is returned unchanged.

use crate::Normalized;
use crate::demosaic::reflect;
use rayon::prelude::*;

/// The strength an enabled setting starts at (`0` is off, `100` the strongest filtering).
pub const DEFAULT_STRENGTH: f64 = 50.0;

/// Patch radius on a sub-sampled plane: 5 × 5 patches (10 × 10 sensor pixels).
const PATCH_R: usize = 2;
/// Largest search radius on a plane: 13 × 13 candidate patches.
const SEARCH_R_MAX: usize = 6;
/// Cost ceiling, in (candidate offsets × plane megapixels): the full window up to 16 MP per plane (a 64 MP sensor);
/// above that the window shrinks so the cost per megapixel stays bounded.
const OFFSET_MPIX_BUDGET: f64 = 169.0 * 16.0;
/// Side of a tile's core on a plane.
const TILE: usize = 96;
/// Side of a noise-estimation block on a plane.
const EST_BLOCK: usize = 8;
/// A plane smaller than this (either side) is left alone.
const MIN_PLANE: usize = 8;
/// Blocks holding a sample above this are not used to fit the noise (clipping flattens the noise).
const FIT_CLIP: f32 = 0.95;
/// Samples at or above this are kept as they were.
const KEEP_ABOVE: f32 = 0.99;
/// Smallest shot-noise gain used, so the transform stays finite for a noiseless fit.
const A_MIN: f64 = 1e-6;
/// Colours distinguished when fitting noise (a mosaic has at most four).
const COLOURS: usize = 4;
/// Share of the flat blocks' variances taken as the noise level of a brightness bin (below the median: the
/// flattest blocks carry the least texture).
const FLAT_QUANTILE: f32 = 0.4;

/// Poisson-Gaussian noise parameters: `var(x) = a·x + b` for a signal `x` (white = 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseParams {
    pub a: f64,
    pub b: f64,
}

impl NoiseParams {
    fn new(a: f64, b: f64) -> Option<NoiseParams> {
        if !a.is_finite() || !b.is_finite() {
            return None;
        }
        Some(NoiseParams { a: a.max(A_MIN), b: b.max(0.0) })
    }

    /// GAT output for `z = 0`: subtracted from every stabilised value so they stay small (precision in `f32`).
    fn offset(self) -> f64 {
        2.0 / self.a * (0.375 * self.a * self.a + self.b).sqrt()
    }

    /// The generalised Anscombe transform, shifted by [`Self::offset`].
    fn forward(self, z: f32, offset: f64) -> f32 {
        let t = self.a * f64::from(z) + 0.375 * self.a * self.a + self.b;
        (2.0 / self.a * t.max(0.0).sqrt() - offset) as f32
    }

    /// Closed-form approximation of the exact unbiased inverse (Mäkitalo & Foi 2013, Poisson-Gaussian, zero offset).
    fn inverse(self, stabilised: f32, offset: f64) -> f32 {
        let d = f64::from(stabilised) + offset;
        let s = self.b / (self.a * self.a);
        let root = 1.5f64.sqrt();
        let u = if d >= root {
            let i = 1.0 / d;
            0.25 * d * d + 0.25 * root * i - 11.0 / 8.0 * i * i + 5.0 / 8.0 * root * i * i * i - 0.125 - s
        } else {
            // below the closed form's range: the algebraic inverse (the two agree at the boundary)
            0.25 * d * d - 0.375 - s
        };
        (self.a * u) as f32
    }
}

/// One sub-sampled plane of the mosaic: the pixels `(2i + px, 2j + py)`.
struct Plane {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

fn extract(n: &Normalized, px: usize, py: usize) -> Plane {
    let w = (n.width + 1 - px) / 2;
    let h = (n.height + 1 - py) / 2;
    let mut data = Vec::with_capacity(w * h);
    for j in 0..h {
        let row = (2 * j + py) * n.width + px;
        data.extend((0..w).map(|i| n.data.get(row + 2 * i).copied().unwrap_or(0.0)));
    }
    Plane { w, h, data }
}

/// `(mean, variance)` of the noise of every flat-looking `EST_BLOCK` block of a plane. The variance comes from the
/// median absolute deviation of the residual against the mean of the four neighbours (white noise of variance
/// `σ²` leaves `1.25·σ²`), so a few pixels of texture or an edge do not move it.
fn block_stats(p: &Plane) -> Vec<(f32, f32)> {
    if p.w < EST_BLOCK + 2 || p.h < EST_BLOCK + 2 {
        return Vec::new();
    }
    let (nbx, nby) = ((p.w - 2) / EST_BLOCK, (p.h - 2) / EST_BLOCK);
    (0..nby)
        .into_par_iter()
        .flat_map_iter(|by| {
            (0..nbx).filter_map(move |bx| {
                let mut res = [0f32; EST_BLOCK * EST_BLOCK];
                let (mut sum, mut max) = (0f32, f32::NEG_INFINITY);
                for (k, r) in res.iter_mut().enumerate() {
                    let x = 1 + bx * EST_BLOCK + k % EST_BLOCK;
                    let y = 1 + by * EST_BLOCK + k / EST_BLOCK;
                    let c = p.data.get(y * p.w + x).copied().unwrap_or(0.0);
                    let at = |xx: usize, yy: usize| p.data.get(yy * p.w + xx).copied().unwrap_or(0.0);
                    *r = c - 0.25 * (at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1));
                    sum += c;
                    max = max.max(c);
                }
                if !sum.is_finite() || !max.is_finite() || max > FIT_CLIP || res.iter().any(|v| !v.is_finite()) {
                    return None;
                }
                let mid = EST_BLOCK * EST_BLOCK / 2;
                res.sort_unstable_by(f32::total_cmp);
                let med = 0.5 * (res[mid - 1] + res[mid]);
                for r in res.iter_mut() {
                    *r = (*r - med).abs();
                }
                res.sort_unstable_by(f32::total_cmp);
                let sigma = 1.4826 * 0.5 * (res[mid - 1] + res[mid]);
                Some((sum / (EST_BLOCK * EST_BLOCK) as f32, sigma * sigma / 1.25))
            })
        })
        .collect()
}

/// Fit `var = a·mean + b` through the flat blocks' `(mean, variance)` pairs: blocks are grouped by brightness,
/// each group contributes the variance of its flattest blocks ([`FLAT_QUANTILE`]), and a line is fitted through the
/// groups. When the image has (almost) one brightness only, the line is not determined: the noise level at
/// that brightness is kept and split evenly between shot and read noise (the stabilisation is right there).
fn fit_noise(mut stats: Vec<(f32, f32)>) -> Option<NoiseParams> {
    if stats.len() < 4 {
        return None;
    }
    stats.sort_unstable_by(|p, q| p.0.total_cmp(&q.0));
    let bins = (stats.len() / 32).clamp(1, 8);
    let per_bin = stats.len() / bins;
    let quantile = |chunk: &[(f32, f32)]| {
        let mut v: Vec<f32> = chunk.iter().map(|s| s.1).collect();
        v.sort_unstable_by(f32::total_cmp);
        let at = ((v.len() as f32 * FLAT_QUANTILE) as usize).min(v.len().saturating_sub(1));
        v.get(at).copied().unwrap_or(0.0)
    };
    let points: Vec<(f64, f64)> = stats
        .chunks(per_bin.max(1))
        .filter(|c| c.len() >= 2)
        .map(|c| (c.iter().map(|s| f64::from(s.0)).sum::<f64>() / c.len() as f64, f64::from(quantile(c))))
        .collect();
    let np = points.len() as f64;
    let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / np, points.iter().map(|p| p.1).sum::<f64>() / np);
    let (lo, hi) = (points.first().map_or(0.0, |p| p.0), points.last().map_or(0.0, |p| p.0));
    let single_level = |m: f64, v: f64| {
        let v = v.max(0.0);
        let a = if m > 0.01 { 0.5 * v / m } else { A_MIN };
        NoiseParams::new(a, v - a * m)
    };
    if points.len() < 2 || hi - lo < 0.03 {
        return single_level(mx, quantile(&stats).into());
    }
    let sxx: f64 = points.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let (mut a, mut b) = (slope, my - slope * mx);
    if a <= 0.0 {
        // no brightness dependence found: all of it is read noise
        a = A_MIN;
        b = my - a * mx;
    }
    if b < 0.0 {
        // a negative offset is not a noise level: refit through the origin
        b = 0.0;
        let (sm2, smv) = points.iter().fold((0.0, 0.0), |(s2, sv), p| (s2 + p.0 * p.0, sv + p.0 * p.1));
        a = if sm2 > 0.0 { smv / sm2 } else { A_MIN };
    }
    NoiseParams::new(a, b)
}

/// The fitted noise of each CFA colour of a Bayer mosaic (`None` where the image gave too little to fit, or the
/// data are not a one-sample-per-pixel Bayer mosaic).
pub fn estimate_noise(n: &Normalized) -> [Option<NoiseParams>; COLOURS] {
    let mut out = [None; COLOURS];
    let Some(cfa) = n.cfa.as_ref().filter(|c| c.is_bayer()) else { return out };
    if n.cpp != 1 || n.data.len() != n.width * n.height || n.width < 2 * MIN_PLANE || n.height < 2 * MIN_PLANE {
        return out;
    }
    let mut stats: [Vec<(f32, f32)>; COLOURS] = Default::default();
    for (px, py) in PARITIES {
        let colour = usize::from(cfa.color_at(px, py)).min(COLOURS - 1);
        if let Some(s) = stats.get_mut(colour) {
            s.extend(block_stats(&extract(n, px, py)));
        }
    }
    for (o, s) in out.iter_mut().zip(stats) {
        *o = fit_noise(s);
    }
    out
}

const PARITIES: [(usize, usize); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];

/// The non-local-means filtering parameter `h` (in stabilised units, noise `σ = 1`) of a strength (`0..=100`).
fn filter_h(strength: f64) -> f32 {
    let t = (strength / 100.0).clamp(0.0, 1.0) as f32;
    0.35 + 1.15 * t
}

/// The largest search radius whose cost stays inside [`OFFSET_MPIX_BUDGET`] for a plane of `pixels`.
fn search_radius(pixels: usize) -> usize {
    let mp = pixels as f64 / 1e6;
    (1..=SEARCH_R_MAX).rev().find(|&r| ((2 * r + 1) * (2 * r + 1)) as f64 * mp <= OFFSET_MPIX_BUDGET).unwrap_or(1)
}

/// Non-local means of a stabilised plane (noise `σ = 1`): every pixel becomes the average of the pixels whose
/// patch looks like its own, weighted by `exp(−max(d − 2, 0) / h²)` (`d` the mean squared patch difference).
fn nlm_plane(p: &Plane, h: f32, radius: usize) -> Vec<f32> {
    let tiles: Vec<(usize, usize)> = (0..p.h.div_ceil(TILE)).flat_map(|ty| (0..p.w.div_ceil(TILE)).map(move |tx| (tx * TILE, ty * TILE))).collect();
    let results: Vec<(usize, usize, usize, usize, Vec<f32>)> = tiles
        .par_iter()
        .map(|&(x0, y0)| {
            let (tw, th) = (TILE.min(p.w - x0), TILE.min(p.h - y0));
            (x0, y0, tw, th, nlm_tile(p, x0, y0, tw, th, h, radius))
        })
        .collect();
    let mut out = vec![0f32; p.w * p.h];
    for (x0, y0, tw, th, px) in results {
        for y in 0..th {
            let dst = (y0 + y) * p.w + x0;
            if let (Some(d), Some(s)) = (out.get_mut(dst..dst + tw), px.get(y * tw..(y + 1) * tw)) {
                d.copy_from_slice(s);
            }
        }
    }
    out
}

fn nlm_tile(p: &Plane, x0: usize, y0: usize, tw: usize, th: usize, h: f32, radius: usize) -> Vec<f32> {
    let m = radius + PATCH_R;
    let (bw, bh) = (tw + 2 * m, th + 2 * m);
    // the tile with a mirrored margin of search plus patch radius, so every offset has data
    let buf: Vec<f32> = (0..bh)
        .flat_map(|j| {
            (0..bw).map(move |i| {
                let (x, y) = (reflect((x0 + i) as isize - m as isize, p.w), reflect((y0 + j) as isize - m as isize, p.h));
                p.data.get(y * p.w + x).copied().unwrap_or(0.0)
            })
        })
        .collect();
    let at = |x: usize, y: usize| buf.get(y * bw + x).copied().unwrap_or(0.0);
    let (dw, dh) = (tw + 2 * PATCH_R, th + 2 * PATCH_R);
    let patch = 2 * PATCH_R + 1;
    let inv_np = 1.0 / (patch * patch) as f32;
    let inv_h2 = 1.0 / (h * h);
    // the pixel itself counts with weight 1
    let mut acc: Vec<f32> = (0..th).flat_map(|j| (0..tw).map(move |i| (i, j))).map(|(i, j)| at(m + i, m + j)).collect();
    let mut wsum = vec![1f32; tw * th];
    let mut sq = vec![0f32; dw * dh];
    let mut hs = vec![0f32; tw * dh];
    let r = radius as isize;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx == 0 && dy == 0 {
                continue;
            }
            // squared difference with the pixel at offset (dx, dy), over the core plus the patch margin
            for j in 0..dh {
                let y = m - PATCH_R + j;
                let y2 = (y as isize + dy) as usize;
                for i in 0..dw {
                    let x = m - PATCH_R + i;
                    let x2 = (x as isize + dx) as usize;
                    let d = at(x, y) - at(x2, y2);
                    if let Some(s) = sq.get_mut(j * dw + i) {
                        *s = d * d;
                    }
                }
            }
            // box sums over the patch, horizontally then vertically
            for j in 0..dh {
                for i in 0..tw {
                    let base = j * dw + i;
                    let s = sq.get(base..base + patch).map_or(0.0, |w| w.iter().sum::<f32>());
                    if let Some(o) = hs.get_mut(j * tw + i) {
                        *o = s;
                    }
                }
            }
            for j in 0..th {
                for i in 0..tw {
                    let mut dist = 0f32;
                    for k in 0..patch {
                        dist += hs.get((j + k) * tw + i).copied().unwrap_or(0.0);
                    }
                    let excess = (dist * inv_np - 2.0).max(0.0) * inv_h2;
                    if excess < 12.0 {
                        let w = (-excess).exp();
                        let cand = at((m as isize + i as isize + dx) as usize, (m as isize + j as isize + dy) as usize);
                        if let (Some(a), Some(s)) = (acc.get_mut(j * tw + i), wsum.get_mut(j * tw + i)) {
                            *a += w * cand;
                            *s += w;
                        }
                    }
                }
            }
        }
    }
    acc.iter().zip(&wsum).map(|(a, w)| a / w).collect()
}

/// Denoise a Bayer mosaic in place with the strength (`0..=100`, see [`DEFAULT_STRENGTH`]). A strength that is
/// zero, negative or not a number, a non-Bayer or multi-sample image and one too small leave `n` untouched, bit for bit.
pub fn denoise_in_place(n: &mut Normalized, strength: f64) {
    if !strength.is_finite() || strength <= 0.0 {
        return;
    }
    let params = estimate_noise(n);
    if params.iter().all(Option::is_none) {
        return;
    }
    let Some(cfa) = n.cfa.clone() else { return };
    let h = filter_h(strength);
    for (px, py) in PARITIES {
        let colour = usize::from(cfa.color_at(px, py)).min(COLOURS - 1);
        let Some(Some(noise)) = params.get(colour).copied() else { continue };
        let mut plane = extract(n, px, py);
        let offset = noise.offset();
        let original = std::mem::take(&mut plane.data);
        plane.data = original.iter().map(|&z| noise.forward(z, offset)).collect();
        let smooth = nlm_plane(&plane, h, search_radius(plane.w * plane.h));
        for j in 0..plane.h {
            for i in 0..plane.w {
                let (Some(&old), Some(&s)) = (original.get(j * plane.w + i), smooth.get(j * plane.w + i)) else { continue };
                let v = noise.inverse(s, offset);
                if old < KEEP_ABOVE
                    && v.is_finite()
                    && let Some(dst) = n.data.get_mut((2 * j + py) * n.width + 2 * i + px)
                {
                    *dst = v;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cfa;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }

        fn normal(&mut self) -> f64 {
            (-2.0 * self.next().ln()).sqrt() * (std::f64::consts::TAU * self.next()).cos()
        }

        /// Poisson by Knuth's product method (means up to a few hundred).
        fn poisson(&mut self, lambda: f64) -> f64 {
            let limit = (-lambda).exp();
            let (mut k, mut p) = (0.0, 1.0);
            loop {
                p *= self.next();
                if p <= limit {
                    return k;
                }
                k += 1.0;
            }
        }
    }

    /// A Bayer mosaic of the signal `signal(x, y)` with Poisson-Gaussian noise (`a`, `b`).
    fn noisy(w: usize, h: usize, a: f64, b: f64, seed: u64, signal: impl Fn(usize, usize) -> f64) -> Normalized {
        let mut rng = Rng(seed | 1);
        let data = (0..w * h)
            .map(|i| {
                let x = signal(i % w, i / w);
                (a * rng.poisson(x / a) + b.sqrt() * rng.normal()) as f32
            })
            .collect();
        Normalized { width: w, height: h, cpp: 1, data, cfa: Cfa::bayer("RGGB") }
    }

    fn rmse(n: &Normalized, border: usize, truth: impl Fn(usize, usize) -> f64) -> f64 {
        let (mut se, mut cnt) = (0.0, 0usize);
        for y in border..n.height - border {
            for x in border..n.width - border {
                let d = f64::from(n.data[y * n.width + x]) - truth(x, y);
                se += d * d;
                cnt += 1;
            }
        }
        (se / cnt as f64).sqrt()
    }

    #[test]
    fn flat_mosaic_noise_drops_by_at_least_forty_percent() {
        let level = 0.4;
        let mut n = noisy(192, 192, 2e-3, 2e-5, 7, |_, _| level);
        let before = rmse(&n, 12, |_, _| level);
        denoise_in_place(&mut n, DEFAULT_STRENGTH);
        let after = rmse(&n, 12, |_, _| level);
        assert!(after <= 0.6 * before, "rmse {before:.5} -> {after:.5}");
        assert!(n.data.iter().all(|v| v.is_finite()));
    }

    /// 10-90 % rise width of a rising profile, in samples (linear interpolation).
    fn rise_width(profile: &[f64], lo: f64, hi: f64) -> f64 {
        let cross = |level: f64| {
            let i = profile.iter().position(|&v| v >= level).unwrap_or(0).max(1);
            let (p, q) = (profile[i - 1], profile[i]);
            (i - 1) as f64 + if q > p { (level - p) / (q - p) } else { 0.0 }
        };
        cross(lo + 0.9 * (hi - lo)) - cross(lo + 0.1 * (hi - lo))
    }

    #[test]
    fn a_step_edge_keeps_its_width() {
        let (lo, hi, edge) = (0.2, 0.6, 64);
        let step = |x: usize, _| if x < edge { lo } else { hi };
        let mut n = noisy(128, 128, 1e-3, 1e-5, 11, step);
        let width_of = |n: &Normalized| {
            let profile: Vec<f64> = (40..88).map(|x| (16..112).map(|y| f64::from(n.data[y * n.width + x])).sum::<f64>() / 96.0).collect();
            rise_width(&profile, lo, hi)
        };
        let clean = Normalized { data: (0..128 * 128).map(|i| step(i % 128, 0) as f32).collect(), ..n.clone() };
        let before = width_of(&clean);
        denoise_in_place(&mut n, DEFAULT_STRENGTH);
        let after = width_of(&n);
        assert!(after <= before + 1.0, "10-90 width {before:.2} -> {after:.2}");
        // and it still denoised the flat sides
        let noisy_again = noisy(128, 128, 1e-3, 1e-5, 11, step);
        assert!(rmse(&n, 12, step) < 0.7 * rmse(&noisy_again, 12, step));
    }

    #[test]
    fn off_is_bit_identical() {
        let n = noisy(64, 48, 2e-3, 2e-5, 3, |x, y| 0.1 + 0.005 * (x + y) as f64);
        for s in [0.0, -5.0, f64::NAN, f64::NEG_INFINITY] {
            let mut m = n.clone();
            denoise_in_place(&mut m, s);
            assert!(m.data.iter().zip(&n.data).all(|(a, b)| a.to_bits() == b.to_bits()), "strength {s}");
        }
        // images this stage does not handle are left as they were, too
        let mut x = Normalized { cfa: Some(Cfa::xtrans()), ..n.clone() };
        denoise_in_place(&mut x, 80.0);
        assert!(x.data.iter().zip(&n.data).all(|(a, b)| a.to_bits() == b.to_bits()));
        let mut rgb = Normalized { width: 8, height: 8, cpp: 3, data: vec![0.3; 192], cfa: None };
        denoise_in_place(&mut rgb, 80.0);
        assert!(rgb.data.iter().all(|&v| v == 0.3));
    }

    #[test]
    fn the_result_does_not_depend_on_scheduling() {
        let n = noisy(200, 130, 2e-3, 2e-5, 5, |x, _| 0.1 + 0.004 * x as f64);
        let (mut a, mut b) = (n.clone(), n);
        denoise_in_place(&mut a, 70.0);
        denoise_in_place(&mut b, 70.0);
        assert!(a.data.iter().zip(&b.data).all(|(p, q)| p.to_bits() == q.to_bits()));
    }

    #[test]
    fn shot_noise_gain_is_recovered_from_a_ramp() {
        let a = 2e-3;
        let n = noisy(256, 192, a, 2e-5, 9, |x, _| 0.05 + 0.85 * x as f64 / 256.0);
        let fitted = estimate_noise(&n);
        let green = fitted[1].expect("green fitted");
        assert!(green.a > 0.5 * a && green.a < 1.5 * a, "a = {} for {a}", green.a);
    }

    #[test]
    fn degenerate_sizes_and_bad_samples_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (2, 2), (3, 17), (15, 200), (16, 16), (17, 33), (31, 31), (200, 3), (40, 18)] {
            let mut n = noisy(w, h, 2e-3, 2e-5, 13, |_, _| 0.3);
            denoise_in_place(&mut n, 100.0);
            assert_eq!((n.width, n.height, n.data.len()), (w, h, w * h));
            assert!(n.data.iter().all(|v| v.is_finite()), "{w}x{h}");
        }
        let mut n = noisy(48, 40, 2e-3, 2e-5, 17, |_, _| 0.3);
        for (i, v) in n.data.iter_mut().enumerate() {
            if i.is_multiple_of(11) {
                *v = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 5.0, -3.0][i / 11 % 5];
            }
        }
        denoise_in_place(&mut n, 100.0);
        assert_eq!(n.data.len(), 48 * 40);
        let mut flat = Normalized { width: 40, height: 40, cpp: 1, data: vec![0.5; 1600], cfa: Cfa::bayer("GRBG") };
        denoise_in_place(&mut flat, 100.0);
        assert!(flat.data.iter().all(|v| v.is_finite()));
        let mut clipped = Normalized { width: 40, height: 40, cpp: 1, data: vec![1.0; 1600], cfa: Cfa::bayer("BGGR") };
        denoise_in_place(&mut clipped, 100.0);
        assert!(clipped.data.iter().all(|&v| v == 1.0), "clipped samples are kept");
    }

    #[test]
    fn inverse_undoes_the_transform_closely_at_high_signal() {
        let p = NoiseParams { a: 1e-3, b: 1e-5 };
        let off = p.offset();
        for z in [0.2f32, 0.5, 0.9] {
            let back = p.inverse(p.forward(z, off), off);
            assert!((back - z).abs() < 0.01, "{z} -> {back}");
        }
    }
}
