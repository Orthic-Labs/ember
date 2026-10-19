//! Chart-calibrated camera profiles: our own colour measurement of one camera model.
//!
//! `camera.calibrate` ([`crate::cmd`]) photographs-in, profile-out: it fits DNG colour tags to a ColorChecker Classic
//! chart shot with the camera ([`lightcraft_raw::calibrate`]) and stores them per camera model as
//! `<model>_chart.json` next to the other camera profiles ([`crate::camera_profiles::dir`]). The format is our own
//! JSON (never a `.dcp`): the fitted `ColorMatrix` and `ForwardMatrix`, the camera neutral and illuminant they were
//! fitted at, the chart printing, and the fit's ΔE against the chart's reference colours.
//!
//! A raw without colour matrices of its own (not a DNG) starts from its model's chart profile when there is one
//! ([`ChartProfile::fill`], called first by `camera_preview::starting_colour`): the DNG colour model does the rest,
//! so white balance, Kelvin and the look all follow from the measured matrices. Profiles are read once per process
//! and replaced when `camera.calibrate` saves; a damaged or hostile file is ignored with a warning.
use lightcraft_color::{Mat3, Xy};
use lightcraft_raw::ColorData;
use lightcraft_raw::calibrate::{Calibration, ChartEdition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Format version of the profile files.
const VERSION: u32 = 1;
/// `kind` of a profile measured from a chart.
const KIND: &str = "chart";
/// Largest profile file read.
const MAX_FILE: u64 = 1 << 16;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartProfile {
    pub version: u32,
    /// Always `"chart"`.
    pub kind: String,
    /// The camera model as the files report it (Exif `Model`).
    pub model: String,
    pub make: Option<String>,
    /// Which printing of the chart the reference colours are for.
    pub edition: ChartEdition,
    /// Chromaticity of the light the chart was shot in.
    pub illuminant: [f64; 2],
    /// `ColorMatrix`: XYZ → camera under `illuminant`.
    color_matrix: [[f64; 3]; 3],
    /// `ForwardMatrix`: white-balanced camera → XYZ D50.
    forward_matrix: [[f64; 3]; 3],
    /// Camera neutral under `illuminant`, largest 1.
    pub neutral: [f64; 3],
    /// CIE76 ΔE of the fit against the chart's reference colours.
    pub delta_e_mean: f64,
    pub delta_e_max: f64,
    /// 0-based patch with the largest ΔE.
    pub worst_patch: usize,
    /// Unix seconds when it was fitted.
    pub created: u64,
}

impl ChartProfile {
    pub fn new(model: &str, make: Option<&str>, edition: ChartEdition, fit: &Calibration, created: u64) -> ChartProfile {
        ChartProfile {
            version: VERSION,
            kind: KIND.to_owned(),
            model: model.trim().to_owned(),
            make: make.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()),
            edition,
            illuminant: [fit.illuminant.x, fit.illuminant.y],
            color_matrix: fit.color_matrix.0,
            forward_matrix: fit.forward_matrix.0,
            neutral: fit.neutral,
            delta_e_mean: fit.delta_e_mean,
            delta_e_max: fit.delta_e_max,
            worst_patch: fit.worst_patch,
            created,
        }
    }

    pub fn color_matrix(&self) -> Mat3 {
        Mat3(self.color_matrix)
    }

    pub fn forward_matrix(&self) -> Mat3 {
        Mat3(self.forward_matrix)
    }

    /// The fit as [`lightcraft_raw::calibrate`] returns it.
    pub fn calibration(&self) -> Calibration {
        Calibration {
            color_matrix: self.color_matrix(),
            forward_matrix: self.forward_matrix(),
            neutral: self.neutral,
            illuminant: Xy::new(self.illuminant[0], self.illuminant[1]),
            delta_e_mean: self.delta_e_mean,
            delta_e_max: self.delta_e_max,
            worst_patch: self.worst_patch,
        }
    }

    /// Give `color` this camera's measured matrices unless it has colour matrices of its own; returns whether it did.
    pub fn fill(&self, color: &mut ColorData) -> bool {
        self.calibration().fill(color)
    }

    /// Whether the data is usable: bounded invertible matrices, a positive neutral, a plausible illuminant.
    fn valid(&self) -> bool {
        let bounded = |m: &[[f64; 3]; 3]| m.iter().flatten().all(|v| v.is_finite() && v.abs() < 64.0) && Mat3(*m).inverse().is_some();
        let [x, y] = self.illuminant;
        self.version == VERSION
            && self.kind == KIND
            && !self.model.trim().is_empty()
            && bounded(&self.color_matrix)
            && bounded(&self.forward_matrix)
            && self.neutral.iter().all(|v| v.is_finite() && *v > 0.0 && *v <= 1.0 + 1e-9)
            && x.is_finite()
            && y.is_finite()
            && x > 0.0
            && y > 0.05
            && x + y < 1.0
            && self.delta_e_mean.is_finite()
            && self.delta_e_max.is_finite()
    }
}

/// File name of `model`'s chart profile (`<model>_chart.json`, the name sanitised like [`crate::camera_profiles::file_name`]).
pub fn file_name(model: &str) -> String {
    let base = crate::camera_profiles::file_name(model);
    format!("{}_chart.json", base.strip_suffix(".json").unwrap_or(&base))
}

/// Read and validate a chart profile file.
pub fn load(path: &Path) -> Result<ChartProfile, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > MAX_FILE {
        return Err(format!("{}: {size} bytes is too large for a chart profile", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let profile: ChartProfile = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if !profile.valid() {
        return Err(format!("{}: not a usable chart profile (version {}, model {:?})", path.display(), profile.version, profile.model));
    }
    Ok(profile)
}

type Cache = Mutex<HashMap<String, Option<Arc<ChartProfile>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Counts the profiles saved in this process: part of the render cache keys ([`crate::camera_profiles::cache_key`]).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The chart profile for camera `model`, if one was saved; read once per process.
pub fn get(model: &str) -> Option<Arc<ChartProfile>> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache.get(model) {
        return hit.clone();
    }
    let path: Option<PathBuf> = crate::camera_profiles::dir().map(|d| d.join(file_name(model)));
    let profile = match path {
        Some(path) if path.is_file() => match load(&path) {
            Ok(p) if p.model.trim() == model => Some(Arc::new(p)),
            Ok(p) => {
                eprintln!("ember: ignoring chart profile {}: it is for {:?}, not {model:?}", path.display(), p.model);
                None
            }
            Err(e) => {
                eprintln!("ember: ignoring chart profile {e}");
                None
            }
        },
        _ => None,
    };
    cache.insert(model.to_owned(), profile.clone());
    profile
}

/// Write `profile` to the camera profiles folder as `<model>_chart.json` and make it the one [`get`] returns;
/// returns the path.
pub fn save(profile: ChartProfile) -> Result<PathBuf, String> {
    if !profile.valid() {
        return Err("not a usable chart profile".into());
    }
    let dir = crate::camera_profiles::dir().ok_or("no configuration folder for camera profiles")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file_name(&profile.model));
    let json = serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?;
    lightcraft_catalog::written::replace(&path, &json, true).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    cache.insert(profile.model.clone(), Some(Arc::new(profile)));
    GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_color::D65;
    use lightcraft_raw::calibrate::{fit_chart, reference_xyz};

    fn fit() -> Calibration {
        let m = Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]]);
        let adapt = lightcraft_color::bradford(lightcraft_color::D50, D65);
        let patches: Vec<[f64; 3]> = reference_xyz(ChartEdition::After2014).iter().map(|x| m.apply(adapt.apply(*x)).map(|v| v * 0.4)).collect();
        fit_chart(&patches, ChartEdition::After2014, D65).unwrap()
    }

    #[test]
    fn a_profile_round_trips_through_json_and_fills_a_raw_without_matrices() {
        let calibration = fit();
        let profile = ChartProfile::new(" ILCE-9 ", Some("SONY"), ChartEdition::After2014, &calibration, 7);
        assert_eq!(profile.model, "ILCE-9");
        assert!(profile.valid());
        let json = serde_json::to_string(&profile).unwrap();
        assert!(json.contains("\"kind\":\"chart\"") && json.contains("after-2014"));
        let back: ChartProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, profile);
        let mut color = ColorData::default();
        assert!(back.fill(&mut color));
        assert_eq!(color.color_matrix[0], Some(calibration.color_matrix));
        assert_eq!(color.forward_matrix[0], Some(calibration.forward_matrix));
        assert!(!back.fill(&mut color), "colour matrices already there are kept");
    }

    #[test]
    fn damaged_profiles_are_not_valid() {
        let good = ChartProfile::new("X", None, ChartEdition::default(), &fit(), 0);
        let mut p = good.clone();
        p.color_matrix[0][0] = f64::NAN;
        assert!(!p.valid());
        let mut p = good.clone();
        p.forward_matrix = [[0.0; 3]; 3];
        assert!(!p.valid());
        let mut p = good.clone();
        p.neutral = [1.0, 0.0, 0.5];
        assert!(!p.valid());
        let mut p = good.clone();
        p.version = 2;
        assert!(!p.valid());
        let mut p = good.clone();
        p.kind = "fit".into();
        assert!(!p.valid());
        let mut p = good;
        p.illuminant = [0.3, 0.0];
        assert!(!p.valid());
    }

    #[test]
    fn file_names_are_sanitised_and_apart_from_the_jpeg_fitted_profiles() {
        assert_eq!(file_name("X-T4"), "X-T4_chart.json");
        assert_eq!(file_name("Canon EOS R5"), "Canon_EOS_R5_chart.json");
        assert_ne!(file_name("X-T4"), crate::camera_profiles::file_name("X-T4"));
    }
}
