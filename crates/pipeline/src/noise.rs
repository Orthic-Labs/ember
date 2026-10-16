//! Noise estimation and Auto Noise Reduction.
//!
//! [`estimate`] fits a signal-dependent noise model `var(Y) = a·Y + b` (shot + read noise, in
//! scene-linear luminance) to the flattest patches of an image: the frame is tiled, each tile's
//! residual after a 3×3 Laplacian is measured (Immerkær's estimator), and per intensity band the
//! quietest tiles define the noise floor, so texture and edges (which only add residual) drop
//! out. [`auto_noise_reduction`] turns the model into the noise-reduction sliders. Everything is
//! deterministic: fixed tiling, fixed bands, fixed arithmetic order.

use lightcraft_color::luminance_2020;
use lightcraft_raster::Rgb32f;
use serde::{Deserialize, Serialize};

/// Fitted noise of one image (see [`estimate`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NoiseEstimate {
    /// `var(Y) = a·Y + b` in scene-linear luminance (middle grey = 0.18), at native resolution.
    pub a: f32,
    pub b: f32,
    /// Relative luminance noise (σ / Y) at middle grey.
    pub luminance_mid: f32,
    /// Relative luminance noise three stops below middle grey.
    pub luminance_shadow: f32,
    /// Chromaticity noise (σ of R/Y and B/Y, averaged) in mid-tone patches.
    pub chroma_mid: f32,
    /// Flat tiles the fit used.
    pub tiles: usize,
}

/// Sliders Auto Noise Reduction sets (native slider units).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct AutoNoise {
    pub luminance: f64,
    pub color: f64,
}

const TILE: usize = 8;
/// Longest edge of the centre window the estimate reads (bounds the cost on huge frames).
const WINDOW: usize = 1536;
/// Intensity bands: log2(Y / 0.18) from `BAND_MIN_EV` in `BAND_EV` steps.
const BANDS: usize = 11;
const BAND_MIN_EV: f32 = -7.0;
const BAND_EV: f32 = 1.0;
/// A band needs this many tiles before its noise floor counts.
const MIN_TILES: usize = 6;
/// The quietest share of a band's tiles that defines its floor.
const FLOOR_Q: f32 = 0.2;

/// Immerkær's noise estimator over one tile of a plane: `σ² = Σ r² / 36·n` with `r` the 3×3
/// Laplacian `[1 −2 1; −2 4 −2; 1 −2 1]` over interior samples. Returns (mean, σ²).
fn tile_noise(plane: &[f32], w: usize, x0: usize, y0: usize) -> Option<(f32, f32)> {
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for y in y0..y0 + TILE {
        for x in x0..x0 + TILE {
            let v = *plane.get(y * w + x)?;
            if !v.is_finite() {
                return None;
            }
            sum += v as f64;
            n += 1;
        }
    }
    let mean = (sum / n.max(1) as f64) as f32;
    let at = |x: usize, y: usize| plane.get(y * w + x).copied().unwrap_or(0.0) as f64;
    let mut acc = 0.0f64;
    let mut m = 0usize;
    for y in y0 + 1..y0 + TILE - 1 {
        for x in x0 + 1..x0 + TILE - 1 {
            let r = at(x - 1, y - 1) - 2.0 * at(x, y - 1) + at(x + 1, y - 1) - 2.0 * at(x - 1, y) + 4.0 * at(x, y) - 2.0 * at(x + 1, y)
                + at(x - 1, y + 1)
                - 2.0 * at(x, y + 1)
                + at(x + 1, y + 1);
            acc += r * r;
            m += 1;
        }
    }
    if m == 0 {
        return None;
    }
    Some((mean, (acc / (36.0 * m as f64)) as f32))
}

fn band_of(mean: f32) -> Option<usize> {
    if !mean.is_finite() || mean <= 0.0 {
        return None;
    }
    let ev = (mean / 0.18).log2();
    let i = ((ev - BAND_MIN_EV) / BAND_EV).floor();
    (i >= 0.0 && i < BANDS as f32).then_some(i as usize)
}

/// Fit the noise model of `src` (scene-linear, white-balanced or not). `scale` is how many
/// native pixels one pixel of `src` averages along an edge (1 for the original; a proxy
/// box-fitted from a longer edge passes `native_long / src_long`): averaging `scale²` pixels
/// divides the noise variance by `scale²`, which the fit undoes. `None` when the image is too
/// small or has no flat patches (nothing to measure, not an error).
pub fn estimate(src: &Rgb32f, scale: f32) -> Option<NoiseEstimate> {
    let (w, h) = (src.width, src.height);
    if w < TILE * 2 || h < TILE * 2 || src.data.len() != w * h {
        return None;
    }
    let scale = if scale.is_finite() && scale >= 1.0 { scale } else { 1.0 };
    // the centre window (deterministic; keeps huge frames cheap)
    let ww = w.min(WINDOW);
    let wh = h.min(WINDOW);
    let x0 = (w - ww) / 2;
    let y0 = (h - wh) / 2;
    let mut lum = Vec::with_capacity(ww * wh);
    let mut cr = Vec::with_capacity(ww * wh);
    let mut cb = Vec::with_capacity(ww * wh);
    for y in y0..y0 + wh {
        for x in x0..x0 + ww {
            let p = src.get(x, y);
            let yl = luminance_2020(p);
            lum.push(yl);
            let d = yl.max(1e-6);
            cr.push(p[0] / d);
            cb.push(p[2] / d);
        }
    }
    // tile measurements per band
    let mut bands: Vec<Vec<(f32, f32)>> = vec![Vec::new(); BANDS];
    let mut chroma_var = Vec::new();
    for ty in 0..wh / TILE {
        for tx in 0..ww / TILE {
            let Some((mean, var)) = tile_noise(&lum, ww, tx * TILE, ty * TILE) else { continue };
            let Some(b) = band_of(mean) else { continue };
            if let Some(v) = bands.get_mut(b) {
                v.push((mean, var));
            }
            // chroma in the mid-tones (−2..+1 EV around grey)
            if (-2.0..1.0).contains(&(mean / 0.18).log2())
                && let (Some((_, vr)), Some((_, vb))) = (tile_noise(&cr, ww, tx * TILE, ty * TILE), tile_noise(&cb, ww, tx * TILE, ty * TILE))
            {
                chroma_var.push((vr + vb) * 0.5);
            }
        }
    }
    // per band, the quietest tiles are the noise floor
    let mut pts: Vec<(f32, f32, f32)> = Vec::new(); // (mean, var, weight)
    let mut used = 0usize;
    for v in bands.iter_mut() {
        if v.len() < MIN_TILES {
            continue;
        }
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        let k = ((v.len() as f32 * FLOOR_Q).ceil() as usize).clamp(1, v.len());
        let quiet = v.get(..k)?;
        let mean = quiet.iter().map(|t| t.0 as f64).sum::<f64>() / k as f64;
        let var = quiet.iter().map(|t| t.1 as f64).sum::<f64>() / k as f64;
        pts.push((mean as f32, var as f32, k as f32));
        used += k;
    }
    if pts.is_empty() {
        return None;
    }
    // weighted least squares var = a·mean + b, a, b ≥ 0
    let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for &(x, y, wgt) in &pts {
        let (x, y, wgt) = (x as f64, y as f64, wgt as f64);
        sw += wgt;
        sx += wgt * x;
        sy += wgt * y;
        sxx += wgt * x * x;
        sxy += wgt * x * y;
    }
    let det = sw * sxx - sx * sx;
    let (mut a, mut b) = if det.abs() > 1e-18 { ((sw * sxy - sx * sy) / det, (sxx * sy - sx * sxy) / det) } else { (0.0, sy / sw.max(1e-18)) };
    if a < 0.0 {
        a = 0.0;
        b = sy / sw.max(1e-18);
    }
    if b < 0.0 {
        b = 0.0;
        a = if sxx > 0.0 { (sxy / sxx).max(0.0) } else { 0.0 };
    }
    let s2 = (scale * scale) as f64;
    let (a, b) = ((a * s2) as f32, (b * s2) as f32);
    let sigma = |y: f32| (a * y + b).max(0.0).sqrt();
    let chroma_mid = if chroma_var.is_empty() {
        0.0
    } else {
        chroma_var.sort_by(f32::total_cmp);
        let k = ((chroma_var.len() as f32 * FLOOR_Q).ceil() as usize).clamp(1, chroma_var.len());
        let v = chroma_var.get(..k)?.iter().map(|v| *v as f64).sum::<f64>() / k as f64;
        (v as f32 * scale * scale).max(0.0).sqrt()
    };
    let est = NoiseEstimate { a, b, luminance_mid: sigma(0.18) / 0.18, luminance_shadow: sigma(0.0225) / 0.0225, chroma_mid, tiles: used };
    [est.a, est.b, est.luminance_mid, est.luminance_shadow, est.chroma_mid].iter().all(|v| v.is_finite()).then_some(est)
}

/// Relative luminance noise at middle grey of a clean base-ISO raw: below it Auto Noise leaves
/// luminance NR at 0.
const CLEAN_LUM: f32 = 0.010;
/// Chromaticity noise of a clean raw's mid-tones.
const CLEAN_CHROMA: f32 = 0.010;

/// The noise-reduction sliders for a measured noise level. `iso` (when known) stands in for a
/// missing measurement; `raw` uses a colour NR floor of 25.
pub fn auto_noise_reduction(est: Option<NoiseEstimate>, iso: Option<u32>, raw: bool) -> AutoNoise {
    let floor = if raw { 25.0 } else { 0.0 };
    let (lum, col) = match est {
        Some(e) => {
            // a stop more noise ≈ 24 slider points; shadows count for a third of the
            // judgement (high-ISO shadows are where NR is wanted most)
            let l_mid = (e.luminance_mid / CLEAN_LUM).max(1e-6).log2();
            let l_sh = (e.luminance_shadow / (CLEAN_LUM * 2.0)).max(1e-6).log2();
            let lum = 24.0 * (0.67 * l_mid + 0.33 * l_sh).max(0.0);
            let c = (e.chroma_mid / CLEAN_CHROMA).max(1e-6).log2();
            (lum, floor + 20.0 * c.max(0.0))
        }
        None => match iso {
            Some(iso) if iso > 0 => {
                let stops = (iso as f32 / 400.0).log2().max(0.0);
                (16.0 * stops, floor + 12.0 * stops)
            }
            _ => (0.0, floor),
        },
    };
    AutoNoise { luminance: (lum.clamp(0.0, 80.0) as f64).round(), color: (col.clamp(0.0, 100.0) as f64).round() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random in −0.5..0.5.
    fn hash_noise(x: usize, y: usize, seed: u32) -> f32 {
        let mut h = (x as u32).wrapping_mul(0x9E37_79B9) ^ (y as u32).wrapping_mul(0x85EB_CA6B) ^ seed;
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^= h >> 12;
        (h & 0xFFFF) as f32 / 65535.0 - 0.5
    }

    /// Gaussian-ish noise (sum of four uniforms) with the given σ.
    fn gauss(x: usize, y: usize, sigma: f32) -> f32 {
        (hash_noise(x, y, 1) + hash_noise(x, y, 2) + hash_noise(x, y, 3) + hash_noise(x, y, 4)) * sigma / (4.0f32 / 12.0).sqrt()
    }

    fn noisy_gradient(sigma_mid: f32, shot: bool) -> Rgb32f {
        Rgb32f::from_fn(256, 256, |x, y| {
            let yl = 0.02 + 0.6 * (x as f32 / 255.0);
            let sigma = if shot { sigma_mid * (yl / 0.18).sqrt() } else { sigma_mid };
            let v = (yl + gauss(x, y, sigma)).max(1e-4);
            [v; 3]
        })
    }

    #[test]
    fn recovers_additive_noise_level() {
        let img = noisy_gradient(0.01, false);
        let e = estimate(&img, 1.0).unwrap();
        let sigma_mid = e.luminance_mid * 0.18;
        assert!((sigma_mid - 0.01).abs() < 0.003, "{e:?}");
        assert!(e.tiles > 50);
    }

    #[test]
    fn recovers_signal_dependent_noise_and_scale() {
        let img = noisy_gradient(0.01, true);
        let e = estimate(&img, 1.0).unwrap();
        // σ²(0.18) = 1e-4 → a ≈ 1e-4 / 0.18
        assert!(e.a > 2e-4 && e.a < 9e-4, "{e:?}");
        assert!(e.luminance_shadow > e.luminance_mid, "shot noise is relatively worse in shadows: {e:?}");
        // a proxy that averaged 2×2 native pixels reports four times the variance
        let scaled = estimate(&img, 2.0).unwrap();
        assert!((scaled.a / e.a - 4.0).abs() < 0.05 && (scaled.luminance_mid / e.luminance_mid - 2.0).abs() < 0.02);
    }

    #[test]
    fn texture_and_edges_do_not_count_as_noise() {
        let clean = Rgb32f::from_fn(256, 256, |x, y| {
            let v = if (x / 32 + y / 32) % 2 == 0 { 0.05 } else { 0.4 };
            [v + gauss(x, y, 0.004); 3]
        });
        let e = estimate(&clean, 1.0).unwrap();
        assert!(e.luminance_mid * 0.18 < 0.007, "edges leaked into the estimate: {e:?}");
        let textured = Rgb32f::from_fn(256, 256, |x, y| {
            let fine = if (x + y) % 2 == 0 { 0.1 } else { -0.1 };
            let v = 0.3 + if (x / 16) % 3 == 0 { fine } else { 0.0 };
            [v + gauss(x, y, 0.004); 3]
        });
        let t = estimate(&textured, 1.0).unwrap();
        assert!(t.luminance_mid * 0.18 < 0.007, "texture leaked into the estimate: {t:?}");
    }

    #[test]
    fn chroma_noise_is_measured_on_chromaticity() {
        let grey = Rgb32f::from_fn(128, 128, |x, y| [0.2 + gauss(x, y, 0.004); 3]);
        let coloured = Rgb32f::from_fn(128, 128, |x, y| {
            let n = gauss(x, y, 0.004);
            [0.2 + n + gauss(x + 777, y, 0.01), 0.2 + n, 0.2 + n - gauss(x, y + 777, 0.01)]
        });
        let g = estimate(&grey, 1.0).unwrap();
        let c = estimate(&coloured, 1.0).unwrap();
        assert!(c.chroma_mid > g.chroma_mid * 3.0, "{g:?} vs {c:?}");
    }

    #[test]
    fn sliders_follow_noise_and_never_leave_range() {
        let clean = auto_noise_reduction(
            Some(NoiseEstimate { luminance_mid: 0.008, luminance_shadow: 0.02, chroma_mid: 0.006, ..Default::default() }),
            None,
            true,
        );
        assert_eq!(clean, AutoNoise { luminance: 0.0, color: 25.0 });
        let noisy = auto_noise_reduction(
            Some(NoiseEstimate { luminance_mid: 0.08, luminance_shadow: 0.3, chroma_mid: 0.05, ..Default::default() }),
            None,
            true,
        );
        assert!(noisy.luminance > 40.0 && noisy.luminance <= 80.0 && noisy.color > 50.0 && noisy.color <= 100.0, "{noisy:?}");
        let absurd = auto_noise_reduction(
            Some(NoiseEstimate { luminance_mid: 1e9, luminance_shadow: 1e9, chroma_mid: 1e9, ..Default::default() }),
            None,
            false,
        );
        assert_eq!(absurd, AutoNoise { luminance: 80.0, color: 100.0 });
        let nan = auto_noise_reduction(Some(NoiseEstimate { luminance_mid: f32::NAN, ..Default::default() }), None, false);
        assert!(nan.luminance.is_finite() && nan.color.is_finite());
        // ISO stands in for a missing measurement
        assert_eq!(auto_noise_reduction(None, Some(100), true), AutoNoise { luminance: 0.0, color: 25.0 });
        let iso = auto_noise_reduction(None, Some(6400), true);
        assert!(iso.luminance >= 60.0 && iso.color > 60.0, "{iso:?}");
        assert_eq!(auto_noise_reduction(None, None, false), AutoNoise::default());
    }

    #[test]
    fn hostile_input_is_refused_not_a_panic() {
        assert!(estimate(&Rgb32f::new(0, 0), 1.0).is_none());
        assert!(estimate(&Rgb32f::new(8, 8), 1.0).is_none());
        let nan = Rgb32f::filled(64, 64, [f32::NAN; 3]);
        assert!(estimate(&nan, 1.0).is_none());
        let black = Rgb32f::filled(64, 64, [0.0; 3]);
        assert!(estimate(&black, 1.0).is_none());
        let img = noisy_gradient(0.01, false);
        assert!(estimate(&img, f32::NAN).is_some() && estimate(&img, -3.0).is_some());
        let a = estimate(&img, 1.0);
        assert_eq!(a, estimate(&img, 1.0), "repeat stable");
    }
}
