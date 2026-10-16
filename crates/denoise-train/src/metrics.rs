//! Image metrics for the qualification eval. All buffers are `[3, h, w]` CHW linear RGB, white level 1.

use serde::Serialize;

pub fn mse(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| ((*x as f64) - (*y as f64)).powi(2)).sum::<f64>() / n as f64
}

/// Peak-1 PSNR in dB; `None` for a zero error (no finite value), never infinity.
pub fn psnr_from_mse(mse: f64) -> Option<f64> {
    if mse > 0.0 && mse.is_finite() { Some(10.0 * (1.0 / mse).log10()) } else { None }
}

/// A display-like encoding for a second PSNR that weighs shadows the way a viewer does (gamma 1/2.2 of the
/// clamped linear value). Linear MSE alone is dominated by highlights.
pub fn encode(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

pub fn encoded_mse(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| ((encode(*x) as f64) - (encode(*y) as f64)).powi(2)).sum::<f64>() / n as f64
}

/// Signed mean error per channel (output minus clean): a colour or transfer error shows as a channel offset.
pub fn channel_bias(out: &[f32], clean: &[f32], pixels: usize) -> [f64; 3] {
    let mut bias = [0f64; 3];
    if pixels == 0 {
        return bias;
    }
    for (c, slot) in bias.iter_mut().enumerate() {
        let (Some(o), Some(k)) = (out.get(c * pixels..(c + 1) * pixels), clean.get(c * pixels..(c + 1) * pixels)) else { return [0.0; 3] };
        *slot = o.iter().zip(k).map(|(x, y)| (*x as f64) - (*y as f64)).sum::<f64>() / pixels as f64;
    }
    bias
}

/// Mean difference of the chromaticity ratios (R/G, B/G) between output and clean, over pixels whose clean
/// green exceeds 0.02 (ratios of near-black are noise).
pub fn chroma_shift(out: &[f32], clean: &[f32], pixels: usize) -> [f64; 2] {
    let (mut sr, mut sb, mut n) = (0.0f64, 0.0f64, 0usize);
    for i in 0..pixels {
        let (Some(&cr), Some(&cg), Some(&cb)) = (clean.get(i), clean.get(pixels + i), clean.get(2 * pixels + i)) else { break };
        let (Some(&or), Some(&og), Some(&ob)) = (out.get(i), out.get(pixels + i), out.get(2 * pixels + i)) else { break };
        if cg > 0.02 && og > 1e-4 {
            sr += (or / og) as f64 - (cr / cg) as f64;
            sb += (ob / og) as f64 - (cb / cg) as f64;
            n += 1;
        }
    }
    if n == 0 { [0.0, 0.0] } else { [sr / n as f64, sb / n as f64] }
}

fn gradients(img: &[f32], h: usize, w: usize, mut f: impl FnMut(usize, f64, f64)) {
    for c in 0..3 {
        for y in 0..h {
            for x in 0..w {
                let at = |yy: usize, xx: usize| img.get(c * h * w + yy * w + xx).copied().unwrap_or(0.0) as f64;
                let v = at(y, x);
                let dx = if x + 1 < w { at(y, x + 1) - v } else { 0.0 };
                let dy = if y + 1 < h { at(y + 1, x) - v } else { 0.0 };
                f(c * h * w + y * w + x, dx, dy);
            }
        }
    }
}

/// RMS difference between the forward-difference gradients of `out` and `clean`: how far edges and texture moved.
pub fn gradient_rmse(out: &[f32], clean: &[f32], h: usize, w: usize) -> f64 {
    let mut go = Vec::with_capacity(3 * h * w * 2);
    gradients(out, h, w, |_, dx, dy| {
        go.push(dx);
        go.push(dy);
    });
    let mut k = 0usize;
    let mut sum = 0.0f64;
    let mut n = 0usize;
    gradients(clean, h, w, |_, dx, dy| {
        if let (Some(a), Some(b)) = (go.get(k), go.get(k + 1)) {
            sum += (a - dx).powi(2) + (b - dy).powi(2);
            n += 2;
        }
        k += 2;
    });
    if n == 0 { 0.0 } else { (sum / n as f64).sqrt() }
}

/// Mean squared forward-difference gradient: the image's structure energy.
pub fn gradient_energy(img: &[f32], h: usize, w: usize) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0usize;
    gradients(img, h, w, |_, dx, dy| {
        sum += dx * dx + dy * dy;
        n += 2;
    });
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// Population standard deviation of a slice.
pub fn std_dev(v: &[f32]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
    (v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

/// `q`-quantile (0..=1) by nearest rank of an unsorted list; 0 when empty.
pub fn quantile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    v.get(idx).copied().unwrap_or(0.0)
}

/// Everything the eval says about one output against its clean reference.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Metrics {
    pub mse: f64,
    /// `None`: zero error.
    pub psnr_db: Option<f64>,
    pub encoded_psnr_db: Option<f64>,
    pub gradient_rmse: f64,
    /// Signed per-channel mean error, R, G, B.
    pub bias: [f64; 3],
    /// Mean change of (R/G, B/G).
    pub chroma_shift: [f64; 2],
}

pub fn measure(out: &[f32], clean: &[f32], h: usize, w: usize) -> Metrics {
    let m = mse(out, clean);
    Metrics {
        mse: m,
        psnr_db: psnr_from_mse(m),
        encoded_psnr_db: psnr_from_mse(encoded_mse(out, clean)),
        gradient_rmse: gradient_rmse(out, clean, h, w),
        bias: channel_bias(out, clean, h * w),
        chroma_shift: chroma_shift(out, clean, h * w),
    }
}

/// Mean of per-image metrics (PSNR is recomputed from the mean error, not averaged in dB).
pub fn mean_metrics(list: &[Metrics], mean_encoded_mse: f64) -> Metrics {
    let n = list.len().max(1) as f64;
    let avg = |f: &dyn Fn(&Metrics) -> f64| list.iter().map(f).sum::<f64>() / n;
    let m = avg(&|x| x.mse);
    Metrics {
        mse: m,
        psnr_db: psnr_from_mse(m),
        encoded_psnr_db: psnr_from_mse(mean_encoded_mse),
        gradient_rmse: avg(&|x| x.gradient_rmse),
        bias: [avg(&|x| x.bias[0]), avg(&|x| x.bias[1]), avg(&|x| x.bias[2])],
        chroma_shift: [avg(&|x| x.chroma_shift[0]), avg(&|x| x.chroma_shift[1])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_images_have_zero_error_and_no_finite_psnr() {
        let img: Vec<f32> = (0..3 * 4 * 4).map(|i| i as f32 / 48.0).collect();
        let m = measure(&img, &img, 4, 4);
        assert_eq!(m.mse, 0.0);
        assert_eq!(m.psnr_db, None);
        assert_eq!(m.gradient_rmse, 0.0);
        assert_eq!(m.bias, [0.0; 3]);
        assert_eq!(m.chroma_shift, [0.0; 2]);
    }

    #[test]
    fn a_constant_offset_is_all_bias_and_a_known_psnr() {
        let clean = vec![0.5f32; 3 * 4 * 4];
        let out: Vec<f32> = clean.iter().map(|v| v + 0.1).collect();
        let m = measure(&out, &clean, 4, 4);
        assert!((m.mse - 0.01).abs() < 1e-9);
        assert!((m.psnr_db.unwrap() - 20.0).abs() < 1e-6);
        for b in m.bias {
            assert!((b - 0.1).abs() < 1e-6);
        }
        assert_eq!(m.gradient_rmse, 0.0, "a constant shift moves no edge");
    }

    #[test]
    fn a_channel_gain_shows_as_a_chroma_shift() {
        let n = 16;
        let clean = vec![0.4f32; 3 * n];
        let mut out = clean.clone();
        for v in &mut out[..n] {
            *v *= 1.1; // red up 10 percent
        }
        let s = chroma_shift(&out, &clean, n);
        assert!((s[0] - 0.1).abs() < 1e-5, "R/G moved by {}", s[0]);
        assert!(s[1].abs() < 1e-6);
    }

    #[test]
    fn gradient_metrics_see_a_blurred_edge() {
        let (h, w) = (4usize, 8usize);
        let mut sharp = vec![0f32; 3 * h * w];
        let mut soft = sharp.clone();
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    sharp[c * h * w + y * w + x] = if x < 4 { 0.1 } else { 0.9 };
                    soft[c * h * w + y * w + x] = 0.1 + 0.8 * ((x as f32 - 1.0) / 6.0).clamp(0.0, 1.0);
                }
            }
        }
        assert!(gradient_rmse(&soft, &sharp, h, w) > 0.05);
        assert!(gradient_energy(&soft, h, w) < gradient_energy(&sharp, h, w));
        assert_eq!(gradient_rmse(&sharp, &sharp, h, w), 0.0);
    }

    #[test]
    fn quantiles_and_std() {
        assert_eq!(quantile(&[3.0, 1.0, 2.0], 0.5), 2.0);
        assert_eq!(quantile(&[], 0.5), 0.0);
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0], 1.0), 4.0);
        assert!((std_dev(&[1.0, 3.0]) - 1.0).abs() < 1e-9);
    }
}
