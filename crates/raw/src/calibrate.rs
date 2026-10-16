//! Own camera colour calibration from a photographed ColorChecker Classic chart (24 patches).
//!
//! [`sample_chart`] averages the 24 patches of a developed (camera RGB, not white balanced, linear) picture from the
//! four corners of the chart's patch grid. [`fit_chart`] turns those camera values and the chart's published
//! reference colours into DNG colour tags ([`Calibration::fill`]), which [`crate::color`]'s DNG model then renders:
//!
//! 1. The camera neutral is the least-squares fit of `camera = neutral · Y` over the white and four grey patches.
//! 2. With `wb = 1 / neutral`, a `ForwardMatrix` (white-balanced camera → XYZ D50, the DNG spec's definition) is
//!    fitted by linear least squares over all 24 patches, constrained so camera white maps exactly to D50 white
//!    (each row is solved exactly in two unknowns, the third follows from the constraint).
//! 3. `ColorMatrix` (XYZ → camera) for the chart's illuminant is the inverse of
//!    `Bradford(D50 → illuminant) · ForwardMatrix · diag(1 / neutral)`, the relation DNG spec chapter 6 defines
//!    between the two matrices. The forward matrix does not depend on the illuminant; the colour matrix only
//!    anchors the Kelvin scale (the white a camera neutral stands for).
//!
//! Reference data: the CIE L*a*b* (D50, 2°) values X-Rite publishes for the ColorChecker Classic, in both printings
//! (before and after November 2014), as tabulated by BabelColor (Danny Pascale, "The ColorChecker (since 1976!)",
//! babelcolor.com/main_level/ColorChecker.htm) and X-Rite, "New color specifications for ColorChecker SG and
//! Classic Charts" (2016). The numbers were checked against the data tables in the BSD-3 `colour-science` project
//! (`colour/characterisation/datasets/colour_checkers/chromaticity_coordinates.py`). Measured colours of a printed
//! chart are facts, not code; no camera profile, matrix table or GPL data is used.

use crate::ColorData;
use lightcraft_color::{D50, Mat3, Xy, bradford, cct};
use lightcraft_raster::Rgb32f;
use serde::{Deserialize, Serialize};

/// Patches of a ColorChecker Classic, row-major from dark skin (top left) to black (bottom right).
pub const PATCHES: usize = 24;
const COLS: usize = 6;
const ROWS: usize = 4;
/// Patches 19 to 23 (white, neutral 8, 6.5, 5 and 3.5): the grey row without the black patch.
const NEUTRALS: std::ops::RangeInclusive<usize> = 18..=22;
/// A patch average at or above this is clipped (white level is 1.0).
const CLIP: f64 = 0.98;

/// Which printing of the chart the reference values are for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChartEdition {
    /// Charts made before November 2014 (the GretagMacbeth 2005 values).
    Before2014,
    /// Charts made after November 2014 (current Calibrite / X-Rite charts).
    #[default]
    After2014,
}

impl ChartEdition {
    pub fn name(self) -> &'static str {
        match self {
            ChartEdition::Before2014 => "before-2014",
            ChartEdition::After2014 => "after-2014",
        }
    }

    pub fn parse(s: &str) -> Option<ChartEdition> {
        match s.trim().to_ascii_lowercase().as_str() {
            "before-2014" | "before2014" | "2005" | "old" => Some(ChartEdition::Before2014),
            "after-2014" | "after2014" | "2014" | "new" | "current" => Some(ChartEdition::After2014),
            _ => None,
        }
    }

    /// The patches' CIE L*a*b* (D50) reference values.
    pub fn lab(self) -> &'static [[f64; 3]; PATCHES] {
        match self {
            ChartEdition::Before2014 => &LAB_BEFORE_2014,
            ChartEdition::After2014 => &LAB_AFTER_2014,
        }
    }
}

/// ColorChecker Classic, before November 2014 (X-Rite 2016 / BabelColor, D50).
const LAB_BEFORE_2014: [[f64; 3]; PATCHES] = [
    [37.986, 13.555, 14.059],
    [65.711, 18.13, 17.81],
    [49.927, -4.88, -21.905],
    [43.139, -13.095, 21.905],
    [55.112, 8.844, -25.399],
    [70.719, -33.397, -0.199],
    [62.661, 36.067, 57.096],
    [40.02, 10.41, -45.964],
    [51.124, 48.239, 16.248],
    [30.325, 22.976, -21.587],
    [72.532, -23.709, 57.255],
    [71.941, 19.363, 67.857],
    [28.778, 14.179, -50.297],
    [55.261, -38.342, 31.37],
    [42.101, 53.378, 28.19],
    [81.733, 4.039, 79.819],
    [51.935, 49.986, -14.574],
    [51.038, -28.631, -28.638],
    [96.539, -0.425, 1.186],
    [81.257, -0.638, -0.335],
    [66.766, -0.734, -0.504],
    [50.867, -0.153, -0.27],
    [35.656, -0.421, -1.231],
    [20.461, -0.079, -0.973],
];

/// ColorChecker Classic, after November 2014 (X-Rite 2016, D50).
const LAB_AFTER_2014: [[f64; 3]; PATCHES] = [
    [37.54, 14.37, 14.92],
    [64.66, 19.27, 17.5],
    [49.32, -3.82, -22.54],
    [43.46, -12.74, 22.72],
    [54.94, 9.61, -24.79],
    [70.48, -32.26, -0.37],
    [62.73, 35.83, 56.5],
    [39.43, 10.75, -45.17],
    [50.57, 48.64, 16.67],
    [30.1, 22.54, -20.87],
    [71.77, -24.13, 58.19],
    [71.51, 18.24, 67.37],
    [28.37, 15.42, -49.8],
    [54.38, -39.72, 32.27],
    [42.43, 51.05, 28.62],
    [81.8, 2.67, 80.41],
    [50.63, 51.28, -14.12],
    [49.57, -29.71, -28.32],
    [95.19, -1.03, 2.93],
    [81.29, -0.57, 0.44],
    [66.89, -0.75, -0.06],
    [50.76, -0.13, 0.14],
    [35.63, -0.46, -0.48],
    [20.64, 0.07, -0.46],
];

const DELTA: f64 = 6.0 / 29.0;

/// CIE L*a*b* → XYZ relative to `white`.
pub fn lab_to_xyz(lab: [f64; 3], white: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let inv = |t: f64| if t > DELTA { t * t * t } else { 3.0 * DELTA * DELTA * (t - 4.0 / 29.0) };
    [white[0] * inv(fx), white[1] * inv(fy), white[2] * inv(fz)]
}

/// XYZ relative to `white` → CIE L*a*b*.
pub fn xyz_to_lab(xyz: [f64; 3], white: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| if t > DELTA * DELTA * DELTA { t.cbrt() } else { t / (3.0 * DELTA * DELTA) + 4.0 / 29.0 };
    let (fx, fy, fz) = (f(xyz[0] / white[0]), f(xyz[1] / white[1]), f(xyz[2] / white[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn delta_e76(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The chart's reference colours as XYZ relative to D50 white (Y = 1).
pub fn reference_xyz(edition: ChartEdition) -> [[f64; 3]; PATCHES] {
    let white = D50.to_xyz();
    edition.lab().map(|lab| lab_to_xyz(lab, white))
}

/// The DNG colour tags a chart fit gives a camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    /// `ColorMatrix`: XYZ → camera under `illuminant`.
    pub color_matrix: Mat3,
    /// `ForwardMatrix`: white-balanced camera → XYZ D50; maps (1, 1, 1) to D50 white.
    pub forward_matrix: Mat3,
    /// Camera values of a neutral under the chart's light, largest 1.
    pub neutral: [f64; 3],
    /// The light the chart was shot in (the white `color_matrix` is anchored at).
    pub illuminant: Xy,
    /// CIE76 ΔE of the fitted colours against the reference, over the 24 patches.
    pub delta_e_mean: f64,
    pub delta_e_max: f64,
    /// Index (0-based) of the patch with the largest ΔE.
    pub worst_patch: usize,
}

/// The DNG `CalibrationIlluminant` code nearest `xy` in mired among Standard Illuminant A, D50 and D65.
pub fn illuminant_code(xy: Xy) -> u16 {
    let t = cct::xy_to_temp_tint(xy).0.max(1000.0);
    [(17u16, 2856.0f64), (23, 5003.0), (21, 6504.0)]
        .into_iter()
        .min_by(|a, b| (1e6 / t - 1e6 / a.1).abs().total_cmp(&(1e6 / t - 1e6 / b.1).abs()))
        .map_or(21, |c| c.0)
}

impl Calibration {
    /// Write these matrices into `color` unless it has a colour matrix of its own; returns whether it did.
    pub fn fill(&self, color: &mut ColorData) -> bool {
        if crate::color::has_matrix(color) {
            return false;
        }
        color.illuminant = [illuminant_code(self.illuminant), 0];
        color.color_matrix = [Some(self.color_matrix), None];
        color.forward_matrix = [Some(self.forward_matrix), None];
        color.camera_calibration = [None, None];
        color.analog_balance = None;
        true
    }
}

/// Fit DNG colour tags to the camera values of the 24 chart patches (linear camera RGB, black subtracted, not white
/// balanced), shot under `illuminant`.
pub fn fit_chart(patches: &[[f64; 3]], edition: ChartEdition, illuminant: Xy) -> Result<Calibration, String> {
    if patches.len() != PATCHES {
        return Err(format!("a ColorChecker Classic has {PATCHES} patches, got {}", patches.len()));
    }
    if patches.iter().flatten().any(|v| !v.is_finite()) {
        return Err("a patch value is not a number".into());
    }
    let d50 = D50.to_xyz();
    let reference = reference_xyz(edition);
    // camera neutral: camera = n · Y over the grey row
    let (mut num, mut den) = ([0.0f64; 3], 0.0f64);
    for i in NEUTRALS {
        let (Some(p), Some(r)) = (patches.get(i), reference.get(i)) else { continue };
        for c in 0..3 {
            num[c] += p[c] * r[1];
        }
        den += r[1] * r[1];
    }
    let n = num.map(|v| v / den);
    if n.iter().any(|v| !(*v > 1e-6)) {
        return Err("the grey patches are black or negative: check exposure and black level".into());
    }
    let m = n.iter().cloned().fold(f64::MIN, f64::max);
    let neutral = n.map(|v| v / m);
    // white-balanced and exposure-normalised: a neutral of luminance Y reads (Y, Y, Y)
    let a: Vec<[f64; 3]> = patches.iter().map(|p| [p[0] / n[0], p[1] / n[1], p[2] / n[2]]).collect();
    let mut rows = [[0.0f64; 3]; 3];
    for (j, row) in rows.iter_mut().enumerate() {
        let d = d50[j];
        let (mut suu, mut suv, mut svv, mut sut, mut svt) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (ai, ri) in a.iter().zip(&reference) {
            let u = ai[0] - ai[2];
            let v = ai[1] - ai[2];
            let t = ri[j] - ai[2] * d;
            suu += u * u;
            suv += u * v;
            svv += v * v;
            sut += u * t;
            svt += v * t;
        }
        let det = suu * svv - suv * suv;
        if !det.is_finite() || det.abs() <= 1e-12 * (suu * svv).max(1e-30) {
            return Err("the patch colours are degenerate: is this a colour chart?".into());
        }
        let r1 = (sut * svv - suv * svt) / det;
        let r2 = (suu * svt - suv * sut) / det;
        *row = [r1, r2, d - r1 - r2];
    }
    let forward_matrix = Mat3(rows);
    if forward_matrix.0.iter().flatten().any(|v| !v.is_finite() || v.abs() > 64.0) || forward_matrix.inverse().is_none() {
        return Err("the fitted matrix is not usable".into());
    }
    let (mut sum, mut max, mut worst) = (0.0f64, 0.0f64, 0usize);
    for (i, (ai, lab)) in a.iter().zip(edition.lab()).enumerate() {
        let de = delta_e76(xyz_to_lab(forward_matrix.apply(*ai), d50), *lab);
        sum += de;
        if de > max {
            max = de;
            worst = i;
        }
    }
    let to_xyz = bradford(D50, illuminant).mul(&forward_matrix).mul(&Mat3::diag(1.0 / neutral[0], 1.0 / neutral[1], 1.0 / neutral[2]));
    let color_matrix = to_xyz.inverse().ok_or("the fitted matrix is not usable")?;
    Ok(Calibration { color_matrix, forward_matrix, neutral, illuminant, delta_e_mean: sum / PATCHES as f64, delta_e_max: max, worst_patch: worst })
}

/// The point at fractions (`s` along the top edge, `t` down the left edge) of the quadrilateral `c`
/// (top left, top right, bottom right, bottom left).
fn grid_point(c: &[[f64; 2]; 4], s: f64, t: f64) -> [f64; 2] {
    let at = |k: usize| (1.0 - t) * ((1.0 - s) * c[0][k] + s * c[1][k]) + t * ((1.0 - s) * c[3][k] + s * c[2][k]);
    [at(0), at(1)]
}

/// The corners of an axis-aligned chart region `[x, y, width, height]` (picture fractions), top left first.
pub fn corners_of_rect(rect: [f64; 4]) -> [[f64; 2]; 4] {
    let [x, y, w, h] = rect;
    [[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

/// Average the 24 patches of `img` (linear camera RGB) in a window half the size of a patch around its centre.
/// `corners` (picture fractions 0..1): outer corners of the patch grid, patch 1 (dark skin) top left, patch 6 top
/// right, patch 24 (black) bottom right, patch 19 (white) bottom left; a rotated chart gives them rotated.
pub fn sample_chart(img: &Rgb32f, corners: &[[f64; 2]; 4]) -> Result<[[f64; 3]; PATCHES], String> {
    if img.width == 0 || img.height == 0 || img.data.len() != img.width * img.height {
        return Err("no picture".into());
    }
    if corners.iter().flatten().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
        return Err("chart corners must be fractions of the picture, 0 to 1".into());
    }
    let (w, h) = (img.width as f64, img.height as f64);
    let c = corners.map(|p| [p[0] * w, p[1] * h]);
    let len = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
    let cell_w = (len(c[0], c[1]) + len(c[3], c[2])) / 2.0 / COLS as f64;
    let cell_h = (len(c[0], c[3]) + len(c[1], c[2])) / 2.0 / ROWS as f64;
    let half = 0.25 * cell_w.min(cell_h);
    if half < 1.5 {
        return Err(format!("the chart is too small in the picture: patches are {:.0} px", cell_w.min(cell_h)));
    }
    let mut out = [[0.0f64; 3]; PATCHES];
    let mut lum = [0.0f64; PATCHES];
    for (i, (slot, l)) in out.iter_mut().zip(lum.iter_mut()).enumerate() {
        let (col, row) = (i % COLS, i / COLS);
        let [cx, cy] = grid_point(&c, (col as f64 + 0.5) / COLS as f64, (row as f64 + 0.5) / ROWS as f64);
        let x0 = (cx - half).floor().max(0.0) as usize;
        let x1 = ((cx + half).ceil().max(0.0) as usize).min(img.width);
        let y0 = (cy - half).floor().max(0.0) as usize;
        let y1 = ((cy + half).ceil().max(0.0) as usize).min(img.height);
        if x1 <= x0 || y1 <= y0 {
            return Err(format!("patch {} is outside the picture", i + 1));
        }
        let (mut sum, mut lsum, mut lsq, mut n) = ([0.0f64; 3], 0.0f64, 0.0f64, 0usize);
        for y in y0..y1 {
            for x in x0..x1 {
                let Some(p) = img.data.get(y * img.width + x) else { continue };
                let p = p.map(f64::from);
                let v = (p[0] + p[1] + p[2]) / 3.0;
                for k in 0..3 {
                    sum[k] += p[k];
                }
                lsum += v;
                lsq += v * v;
                n += 1;
            }
        }
        if n == 0 {
            return Err(format!("patch {} is outside the picture", i + 1));
        }
        let nf = n as f64;
        *slot = sum.map(|v| v / nf);
        *l = lsum / nf;
        if slot.iter().any(|v| *v >= CLIP) {
            return Err(format!("patch {} is clipped: lower the exposure", i + 1));
        }
        let sd = (lsq / nf - *l * *l).max(0.0).sqrt();
        if *l > 0.03 && sd / *l > 0.25 {
            return Err(format!("patch {} is not uniform: the chart corners are probably misplaced", i + 1));
        }
    }
    if !(18..PATCHES - 1).all(|i| lum[i] > lum[i + 1]) {
        return Err("the grey row (patches 19 to 24) does not darken from white to black: give the corners starting at the top left of the dark skin patch, white at the bottom left".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{camera_to_xyz_d50, camera_transform_of};
    use lightcraft_color::D65;

    /// A plausible camera: XYZ → camera.
    fn camera() -> Mat3 {
        Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])
    }

    fn close(a: &Mat3, b: &Mat3, tol: f64) -> bool {
        (0..3).all(|i| (0..3).all(|j| (a.0[i][j] - b.0[i][j]).abs() <= tol * (1.0 + b.0[i][j].abs())))
    }

    /// Camera values of every patch for a camera `m` (XYZ → camera) lighting the chart with `illuminant`, scaled by `exposure`.
    fn camera_patches(m: &Mat3, illuminant: Xy, edition: ChartEdition, exposure: f64) -> Vec<[f64; 3]> {
        let adapt = bradford(D50, illuminant);
        reference_xyz(edition).iter().map(|x| m.apply(adapt.apply(*x)).map(|v| v * exposure)).collect()
    }

    /// The chart drawn into a picture: the 6 × 4 grid inside `region`, each patch a flat square on a dark gap.
    fn render(patches: &[[f64; 3]], region: [f64; 4]) -> Rgb32f {
        let (w, h) = (720usize, 480usize);
        Rgb32f::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            let (u, v) = ((fx - region[0]) / region[2] * COLS as f64, (fy - region[1]) / region[3] * ROWS as f64);
            if u < 0.0 || v < 0.0 || u >= COLS as f64 || v >= ROWS as f64 {
                return [0.4, 0.4, 0.4];
            }
            let (fu, fv) = (u.fract(), v.fract());
            if !(0.1..0.9).contains(&fu) || !(0.1..0.9).contains(&fv) {
                return [0.01, 0.01, 0.01];
            }
            let p = patches.get(v as usize * COLS + u as usize).copied().unwrap_or([0.0; 3]);
            p.map(|c| c as f32)
        })
    }

    const REGION: [f64; 4] = [0.1, 0.15, 0.8, 0.7];

    #[test]
    fn lab_and_xyz_round_trip_and_white_is_100() {
        let white = D50.to_xyz();
        for lab in LAB_AFTER_2014.iter().chain(&LAB_BEFORE_2014) {
            let back = xyz_to_lab(lab_to_xyz(*lab, white), white);
            assert!(delta_e76(back, *lab) < 1e-9, "{lab:?} {back:?}");
        }
        let w = xyz_to_lab(white, white);
        assert!((w[0] - 100.0).abs() < 1e-9 && w[1].abs() < 1e-9 && w[2].abs() < 1e-9);
        // the grey row is nearly neutral and darkens from the white patch
        for edition in [ChartEdition::Before2014, ChartEdition::After2014] {
            let lab = edition.lab();
            assert!((18..24).all(|i| lab[i][1].abs() < 1.5 && lab[i][2].abs() < 3.0), "{edition:?}");
            assert!((18..23).all(|i| lab[i][0] > lab[i + 1][0]), "{edition:?}");
        }
    }

    #[test]
    fn recovers_a_known_camera_from_a_rendered_chart() {
        let m = camera();
        let illuminant = D65;
        let truth = camera_patches(&m, illuminant, ChartEdition::After2014, 0.35);
        let img = render(&truth, REGION);
        let sampled = sample_chart(&img, &corners_of_rect(REGION)).unwrap();
        let fit = fit_chart(&sampled, ChartEdition::After2014, illuminant).unwrap();
        assert!(fit.delta_e_mean < 0.05 && fit.delta_e_max < 0.2, "{} {}", fit.delta_e_mean, fit.delta_e_max);
        // ColorMatrix = M / max(M · white): the camera's own matrix up to the scale a neutral of 1 implies
        let mm = m.apply(illuminant.to_xyz()).iter().cloned().fold(f64::MIN, f64::max);
        let expect = Mat3(m.0.map(|r| r.map(|v| v / mm)));
        assert!(close(&fit.color_matrix, &expect, 2e-3), "{:?} vs {:?}", fit.color_matrix, expect);
        // ForwardMatrix = Bradford(illuminant → D50) · M⁻¹ · diag(M · white)
        let n = m.apply(illuminant.to_xyz());
        let forward = bradford(illuminant, D50).mul(&m.inverse().unwrap()).mul(&Mat3::diag(n[0], n[1], n[2]));
        assert!(close(&fit.forward_matrix, &forward, 2e-3), "{:?} vs {:?}", fit.forward_matrix, forward);
        let white = fit.forward_matrix.apply([1.0; 3]);
        let d50 = D50.to_xyz();
        assert!((0..3).all(|i| (white[i] - d50[i]).abs() < 1e-9), "camera white must map to D50: {white:?}");
        // the exposure is not part of the answer
        let dark = camera_patches(&m, illuminant, ChartEdition::After2014, 0.1);
        let fit2 = fit_chart(&dark, ChartEdition::After2014, illuminant).unwrap();
        assert!(close(&fit2.forward_matrix, &forward, 1e-9));
    }

    #[test]
    fn a_rotated_chart_and_the_other_edition_are_fitted_too() {
        let m = camera();
        let truth = camera_patches(&m, D65, ChartEdition::Before2014, 0.5);
        // the chart turned upside down: patch 1 is at the bottom right
        let upside = render(&truth.iter().rev().copied().collect::<Vec<_>>(), REGION);
        let corners = [[0.9, 0.85], [0.1, 0.85], [0.1, 0.15], [0.9, 0.15]];
        let sampled = sample_chart(&upside, &corners).unwrap();
        let fit = fit_chart(&sampled, ChartEdition::Before2014, D65).unwrap();
        assert!(fit.delta_e_mean < 0.05, "{}", fit.delta_e_mean);
        // misordered corners are caught by the grey-row check
        assert!(sample_chart(&upside, &corners_of_rect(REGION)).unwrap_err().contains("grey row"));
    }

    #[test]
    fn noisy_patches_still_calibrate_within_tolerance() {
        let m = camera();
        let mut seed = 12345u64;
        let mut noise = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.02
        };
        let noisy: Vec<[f64; 3]> =
            camera_patches(&m, D65, ChartEdition::After2014, 0.4).into_iter().map(|p| p.map(|v| v * (1.0 + noise()))).collect();
        let fit = fit_chart(&noisy, ChartEdition::After2014, D65).unwrap();
        assert!(fit.delta_e_mean < 2.0, "{}", fit.delta_e_mean);
        let n = m.apply(D65.to_xyz());
        let forward = bradford(D65, D50).mul(&m.inverse().unwrap()).mul(&Mat3::diag(n[0], n[1], n[2]));
        assert!(close(&fit.forward_matrix, &forward, 0.06), "{:?} vs {:?}", fit.forward_matrix, forward);
    }

    #[test]
    fn the_filled_tags_render_the_chart_as_the_reference() {
        let m = camera();
        let edition = ChartEdition::After2014;
        let patches = camera_patches(&m, D65, edition, 0.3);
        let fit = fit_chart(&patches, edition, D65).unwrap();
        let mut color = ColorData::default();
        assert!(fit.fill(&mut color));
        assert_eq!(color.illuminant[0], 21);
        assert!(!fit.fill(&mut color), "a file with matrices keeps its own");
        let t = camera_transform_of(&color, D65);
        assert!(!t.matrix_is_fallback);
        let to_xyz = camera_to_xyz_d50(&color, D65);
        let reference = reference_xyz(edition);
        let white = to_xyz.apply(patches[18]);
        for (p, r) in patches.iter().zip(&reference) {
            let xyz = to_xyz.apply(*p);
            for k in 0..3 {
                let (got, want) = (xyz[k] / white[1], r[k] / reference[18][1]);
                assert!((got - want).abs() < 2e-3, "{got} vs {want}");
            }
        }
        // a grey patch is neutral in the working space
        let g = t.matrix.apply(std::array::from_fn(|k| patches[20][k] * f64::from(t.wb[k])));
        assert!((g[0] - g[1]).abs() < 1e-6 * g[1].abs().max(1.0) && (g[1] - g[2]).abs() < 1e-6 * g[1].abs().max(1.0), "{g:?}");
    }

    #[test]
    fn bad_charts_are_refused() {
        let m = camera();
        let good = camera_patches(&m, D65, ChartEdition::After2014, 0.4);
        assert!(fit_chart(&good[..23], ChartEdition::After2014, D65).is_err());
        let mut nan = good.clone();
        nan[3][1] = f64::NAN;
        assert!(fit_chart(&nan, ChartEdition::After2014, D65).is_err());
        assert!(fit_chart(&vec![[0.2, 0.2, 0.2]; PATCHES], ChartEdition::After2014, D65).is_err(), "a flat grey is no chart");
        assert!(fit_chart(&vec![[0.0; 3]; PATCHES], ChartEdition::After2014, D65).is_err());
        // clipped patch, too small a chart, corners outside the picture
        let mut clipped = good.clone();
        clipped[18] = [1.0, 1.0, 1.0];
        let img = render(&clipped, REGION);
        assert!(sample_chart(&img, &corners_of_rect(REGION)).unwrap_err().contains("clipped"));
        let img = render(&good, REGION);
        assert!(sample_chart(&img, &corners_of_rect([0.1, 0.1, 0.01, 0.01])).unwrap_err().contains("too small"));
        assert!(sample_chart(&img, &corners_of_rect([0.5, 0.5, 0.8, 0.8])).is_err());
        assert!(sample_chart(&img, &[[f64::NAN, 0.0]; 4]).is_err());
        // windows centred on patch boundaries take in the gaps
        assert!(sample_chart(&img, &corners_of_rect([0.1 + 0.8 / 12.0, 0.15, 0.8, 0.7])).is_err());
    }

    #[test]
    fn illuminant_codes_follow_the_nearest_standard_light() {
        assert_eq!(illuminant_code(cct::temp_tint_to_xy(2900.0, 0.0)), 17);
        assert_eq!(illuminant_code(cct::temp_tint_to_xy(5000.0, 0.0)), 23);
        assert_eq!(illuminant_code(D65), 21);
        assert_eq!(ChartEdition::parse("2014"), Some(ChartEdition::After2014));
        assert_eq!(ChartEdition::parse("old"), Some(ChartEdition::Before2014));
        assert_eq!(ChartEdition::parse("x"), None);
    }
}
