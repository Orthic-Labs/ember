//! A physically based Poisson-Gaussian sensor noise model, applied in the raw (linear) domain.
//!
//! A sample `x` (black-subtracted, white level = 1) is the expected signal. The sensor counts `K ~ Poisson(x / a)`
//! quanta, each worth `a` in normalised units, then adds Gaussian read/ADC noise of variance `b`:
//! `y = a * K + sqrt(b) * N(0, 1)`. So `E[y] = x` and `Var[y] = a * x + b`, the `var(Y) = a·Y + b` model that
//! `lightcraft_pipeline::noise` fits (there in scene-linear luminance; here per raw sample).
//!
//! Per-ISO parameters follow from a camera description: at gain ratio `r = ISO / base_iso` the full well in
//! normalised units shrinks, so `a = r / full_well_e` (shot noise grows with gain), and read noise in electrons
//! `read_e` plus a fixed ADC term `adc` (normalised units) give `b = (read_e * a)^2 + adc^2`.
//!
//! The constants of [`SensorPrior`] are plausible ranges, not a calibrated camera: they make the network see many
//! different sensors so it conditions on `(a, b)` instead of memorising one. Per-camera calibration is separate work.

use crate::rng::Rng;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseParams {
    pub a: f32,
    pub b: f32,
}

impl NoiseParams {
    /// `a·x + b` for a non-negative signal.
    pub fn variance_at(self, x: f32) -> f32 {
        self.a * x.max(0.0) + self.b
    }

    pub fn sigma_at(self, x: f32) -> f32 {
        self.variance_at(x).max(0.0).sqrt()
    }

    /// Both parameters finite, `a > 0`, `b >= 0`.
    pub fn is_valid(self) -> bool {
        self.a.is_finite() && self.b.is_finite() && self.a > 0.0 && self.b >= 0.0
    }

    /// The same model with `a` and `b` scaled by independent log-normal factors of spread `sigma_ln`: the
    /// conditioning the network sees at train time, so a noisy estimate of `(a, b)` at inference does not hurt.
    pub fn jittered(self, sigma_ln: f64, rng: &mut Rng) -> NoiseParams {
        let fa = (sigma_ln * rng.normal()).exp() as f32;
        let fb = (sigma_ln * rng.normal()).exp() as f32;
        NoiseParams { a: self.a * fa, b: self.b * fb }
    }
}

/// One sensor: how many quanta fill it at base ISO and how noisy its read-out is.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraModel {
    pub base_iso: f64,
    /// Full-well capacity at base ISO, in electrons.
    pub full_well_e: f64,
    /// Input-referred read noise, in electrons.
    pub read_e: f64,
    /// Gain-independent ADC/quantisation noise, in normalised units (white = 1).
    pub adc: f64,
}

impl CameraModel {
    pub fn params_at(&self, iso: f64) -> NoiseParams {
        let r = (iso / self.base_iso).max(1e-3);
        let a = r / self.full_well_e;
        let b = (self.read_e * a).powi(2) + self.adc * self.adc;
        NoiseParams { a: a as f32, b: b as f32 }
    }
}

/// The ranges training draws cameras and ISOs from.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorPrior {
    pub iso_min: f64,
    pub iso_max: f64,
    pub base_iso: f64,
    pub full_well_e: (f64, f64),
    pub read_e: (f64, f64),
    pub adc: (f64, f64),
}

impl Default for SensorPrior {
    fn default() -> Self {
        SensorPrior { iso_min: 100.0, iso_max: 25_600.0, base_iso: 100.0, full_well_e: (20_000.0, 90_000.0), read_e: (1.5, 6.0), adc: (5e-5, 3e-4) }
    }
}

impl SensorPrior {
    /// A random camera and ISO (ISO log-uniform, so every stop is equally represented).
    pub fn sample(&self, rng: &mut Rng) -> (CameraModel, f64) {
        let cam = CameraModel {
            base_iso: self.base_iso,
            full_well_e: rng.log_range(self.full_well_e.0, self.full_well_e.1),
            read_e: rng.log_range(self.read_e.0, self.read_e.1),
            adc: rng.log_range(self.adc.0, self.adc.1),
        };
        let iso = rng.log_range(self.iso_min, self.iso_max);
        (cam, iso)
    }

    pub fn sample_params(&self, rng: &mut Rng) -> NoiseParams {
        let (cam, iso) = self.sample(rng);
        cam.params_at(iso)
    }
}

/// The fixed camera of the evaluation strata (mid-range of the prior), so strata are comparable between runs.
pub fn reference_camera() -> CameraModel {
    CameraModel { base_iso: 100.0, full_well_e: 45_000.0, read_e: 3.0, adc: 1.5e-4 }
}

/// ISO strata the held-out evaluation reports (the reference camera at these gains).
pub const STRATA_ISO: [f64; 6] = [800.0, 1600.0, 3200.0, 6400.0, 12_800.0, 25_600.0];

/// One noisy sample of signal `x`. Not clipped: the caller clips at the sensor's white level, because a
/// clipped sample is no longer unbiased (see the tests, which stay below white).
pub fn noisy_sample(x: f32, p: NoiseParams, rng: &mut Rng) -> f32 {
    if !p.is_valid() || !x.is_finite() {
        return x;
    }
    let a = p.a as f64;
    let lambda = x.max(0.0) as f64 / a;
    let shot = a * rng.poisson(lambda);
    let read = (p.b as f64).sqrt() * rng.normal();
    (shot + read) as f32
}

/// Noisy copy of `clean` (every sample independent).
pub fn add_noise(clean: &[f32], p: NoiseParams, rng: &mut Rng) -> Vec<f32> {
    clean.iter().map(|&x| noisy_sample(x, p, rng)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moments(x: f32, p: NoiseParams, n: usize, seed: u64) -> (f64, f64) {
        let mut rng = Rng::new(seed);
        let (mut s, mut s2) = (0.0f64, 0.0f64);
        for _ in 0..n {
            let v = noisy_sample(x, p, &mut rng) as f64;
            s += v;
            s2 += v * v;
        }
        let mean = s / n as f64;
        (mean, s2 / n as f64 - mean * mean)
    }

    #[test]
    fn mean_is_the_signal_and_variance_follows_a_x_plus_b() {
        // lambda = x / a spans 0.5 (exact Poisson branch) to 5000 (normal branch)
        let cases = [(0.001f32, 2e-3f32, 1e-5f32), (0.02, 1e-3, 4e-6), (0.18, 5e-4, 2e-5), (0.5, 1e-4, 1e-6), (0.0, 1e-3, 3e-5)];
        for (i, &(x, a, b)) in cases.iter().enumerate() {
            let p = NoiseParams { a, b };
            let n = 300_000;
            let (mean, var) = moments(x, p, n, 11 + i as u64);
            let want_var = p.variance_at(x) as f64;
            // the standard error of the sample mean is sqrt(var / n); allow 6 of them
            let tol_mean = 6.0 * (want_var / n as f64).sqrt() + 1e-7;
            assert!((mean - x as f64).abs() < tol_mean, "x={x}: mean {mean} vs {x} (tol {tol_mean})");
            assert!((var / want_var - 1.0).abs() < 0.03, "x={x}: var {var} vs {want_var}");
        }
    }

    #[test]
    fn pure_shot_noise_variance_is_proportional_to_the_signal() {
        let p = NoiseParams { a: 1e-3, b: 0.0 };
        let (_, v1) = moments(0.1, p, 200_000, 3);
        let (_, v4) = moments(0.4, p, 200_000, 4);
        assert!((v4 / v1 / 4.0 - 1.0).abs() < 0.05, "ratio {}", v4 / v1);
    }

    #[test]
    fn iso_scales_a_linearly_and_b_quadratically_above_the_adc_floor() {
        let cam = CameraModel { base_iso: 100.0, full_well_e: 40_000.0, read_e: 3.0, adc: 0.0 };
        let (p1, p2) = (cam.params_at(800.0), cam.params_at(1600.0));
        assert!((p2.a / p1.a - 2.0).abs() < 1e-5);
        assert!((p2.b / p1.b - 4.0).abs() < 1e-4);
        assert!(p1.is_valid() && p2.is_valid());
    }

    #[test]
    fn the_prior_only_makes_valid_params_inside_its_ranges() {
        let prior = SensorPrior::default();
        let mut rng = Rng::new(5);
        for _ in 0..1000 {
            let (cam, iso) = prior.sample(&mut rng);
            assert!((prior.iso_min..=prior.iso_max).contains(&iso));
            assert!(cam.params_at(iso).is_valid());
        }
    }

    #[test]
    fn invalid_params_leave_the_signal_alone() {
        let mut rng = Rng::new(1);
        assert_eq!(noisy_sample(0.3, NoiseParams { a: 0.0, b: 1.0 }, &mut rng), 0.3);
        assert_eq!(noisy_sample(0.3, NoiseParams { a: f32::NAN, b: 1.0 }, &mut rng), 0.3);
    }

    #[test]
    fn noise_is_reproducible() {
        let clean = vec![0.1f32; 64];
        let p = NoiseParams { a: 1e-3, b: 1e-5 };
        assert_eq!(add_noise(&clean, p, &mut Rng::new(9)), add_noise(&clean, p, &mut Rng::new(9)));
    }
}
