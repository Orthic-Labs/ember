//! The candle NAFNet backend (issue #200, feature `nafnet`): runs the denoiser that `denoise-train` produces, which
//! the ONNX plain-U-Net path cannot (depthwise convolution, channel attention and LayerNorm are not in its
//! operator set). The graph, parameter names, noise-map construction and tiling are the shared `lightcraft-nafnet`
//! crate's, so the network that was trained is the one that runs.
//!
//! Selection is by what is installed: [`NafDenoiser::open_installed`] answers `None` for a model folder that is not
//! a NAFNet install (no `contract.json` beside `model.safetensors`), and the caller keeps its ONNX behaviour.
//!
//! The network works on demosaiced camera RGB, not on the packed Bayer planes of [`crate::run::TileRunner`], so it
//! is a whole-picture operation ([`NafDenoiser::denoise_rgb`]) rather than a tile runner.

use std::path::Path;

use lightcraft_nafnet::candle_core::Device;
use lightcraft_nafnet::{IN_CHANNELS, NafModel, OUT_CHANNELS, model_input_chw};

use crate::run::{Error, MAX_PIXELS};

pub use lightcraft_nafnet::{CONTRACT_FILE, WEIGHTS_FILE};

/// Tile side and overlap, in pixels, for memory-bounded inference. The channel attention sees only each tile, so
/// tiled output differs slightly from whole-image output (the qualification eval reports by how much).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiling {
    pub tile: usize,
    pub overlap: usize,
}

pub struct NafDenoiser {
    model: NafModel,
}

impl NafDenoiser {
    /// `Ok(None)` when `dir` is not a NAFNet install; `Err` when it is one but cannot be loaded (malformed
    /// contract, a missing or wrongly shaped tensor, an unreadable file).
    pub fn open_installed(dir: &Path) -> Result<Option<NafDenoiser>, Error> {
        if !NafModel::is_installed(dir) {
            return Ok(None);
        }
        let model = NafModel::open(dir, &Device::Cpu).map_err(|e| Error::Load(e.to_string()))?;
        Ok(Some(NafDenoiser { model }))
    }

    /// Denoise a noisy linear camera-RGB picture (`rgb`: three planes of `width × height`, white level 1, no white
    /// balance) for noise parameters `var(y) = a·y + b`. Gives three planes of the same size. `tiling: None` runs
    /// the whole picture in one call.
    pub fn denoise_rgb(&self, rgb: &[f32], width: usize, height: usize, a: f32, b: f32, tiling: Option<Tiling>) -> Result<Vec<f32>, Error> {
        let n = width
            .checked_mul(height)
            .filter(|&n| n > 0 && n <= MAX_PIXELS)
            .ok_or_else(|| Error::Input("the picture has no size, or is too large".into()))?;
        if rgb.len() != OUT_CHANNELS * n {
            return Err(Error::Input("the picture does not have three planes of width × height samples".into()));
        }
        if rgb.iter().any(|v| !v.is_finite()) {
            return Err(Error::Input("the picture has samples that are not finite".into()));
        }
        if !(a.is_finite() && b.is_finite() && a > 0.0 && b >= 0.0) {
            return Err(Error::Input("the noise parameters need a finite a > 0 and b >= 0".into()));
        }
        let input = model_input_chw(rgb, a, b);
        debug_assert_eq!(input.len(), IN_CHANNELS * n);
        let device = Device::Cpu;
        let out = match tiling {
            None => self.model.net.infer(&input, height, width, &device),
            Some(t) => {
                if t.tile < 16 || t.overlap.saturating_mul(2) >= t.tile {
                    return Err(Error::Input("the tile must be at least 16 and exceed twice the overlap".into()));
                }
                self.model.net.infer_tiled(&input, height, width, t.tile, t.overlap, &device)
            }
        }
        .map_err(|e| Error::Runtime(e.to_string()))?;
        if out.len() != OUT_CHANNELS * n || out.iter().any(|v| !v.is_finite()) {
            return Err(Error::Model("a wrong number of samples, or samples that are not finite".into()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_nafnet::candle_core::{DType, Tensor};
    use lightcraft_nafnet::candle_nn::{VarBuilder, VarMap};
    use lightcraft_nafnet::{NafConfig, NafNet, zero_init_ending};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ember-denoise-naf-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A random-weight tiny network saved as an installed folder; returns the trained-side network too.
    fn install(dir: &Path, zero_ending: bool) -> NafNet {
        let cfg = NafConfig::tiny();
        let varmap = VarMap::new();
        let net = NafNet::new(&cfg, VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu)).unwrap();
        for (name, v) in varmap.data().lock().unwrap().iter() {
            if name.ends_with("beta") || name.ends_with("gamma") {
                v.set(&Tensor::rand(-0.5f32, 0.5f32, v.as_tensor().dims(), &Device::Cpu).unwrap()).unwrap();
            }
        }
        if zero_ending {
            zero_init_ending(&varmap).unwrap();
        }
        varmap.save(dir.join(WEIGHTS_FILE)).unwrap();
        let contract = serde_json::json!({"format_version": 1, "model": cfg, "sigma_scale": lightcraft_nafnet::SIGMA_SCALE});
        std::fs::write(dir.join(CONTRACT_FILE), contract.to_string()).unwrap();
        net
    }

    fn picture(w: usize, h: usize) -> Vec<f32> {
        (0..3 * w * h).map(|i| ((i * 37 % 101) as f32) / 101.0).collect()
    }

    #[test]
    fn a_folder_that_is_not_a_nafnet_install_is_left_alone() {
        let dir = scratch("none");
        assert!(NafDenoiser::open_installed(&dir).unwrap().is_none());
        std::fs::write(dir.join("model.onnx"), b"x").unwrap();
        assert!(NafDenoiser::open_installed(&dir).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_installed_model_matches_the_training_forward() {
        let dir = scratch("same");
        let trained = install(&dir, false);
        let den = NafDenoiser::open_installed(&dir).unwrap().expect("installed");
        std::fs::remove_dir_all(&dir).ok();
        let (w, h) = (22, 18);
        let rgb = picture(w, h);
        let (a, b) = (0.01f32, 0.0004f32);
        let want = trained.infer(&model_input_chw(&rgb, a, b), h, w, &Device::Cpu).unwrap();
        assert_eq!(den.denoise_rgb(&rgb, w, h, a, b, None).unwrap(), want);
    }

    #[test]
    fn tiled_matches_whole_within_tolerance() {
        let dir = scratch("tiled");
        install(&dir, true);
        let den = NafDenoiser::open_installed(&dir).unwrap().expect("installed");
        std::fs::remove_dir_all(&dir).ok();
        let (w, h) = (45, 37);
        let rgb = picture(w, h);
        let whole = den.denoise_rgb(&rgb, w, h, 0.01, 0.0004, None).unwrap();
        let tiled = den.denoise_rgb(&rgb, w, h, 0.01, 0.0004, Some(Tiling { tile: 24, overlap: 6 })).unwrap();
        let worst = whole.iter().zip(&tiled).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-5, "worst difference {worst}");
    }

    #[test]
    fn a_broken_install_or_bad_input_is_an_error() {
        let dir = scratch("broken");
        install(&dir, false);
        let den = NafDenoiser::open_installed(&dir).unwrap().expect("installed");
        let rgb = picture(20, 20);
        assert!(den.denoise_rgb(&rgb, 20, 20, 0.0, 0.0, None).is_err(), "a = 0 is not a noise model");
        assert!(den.denoise_rgb(&rgb[1..], 20, 20, 0.01, 0.0, None).is_err());
        assert!(den.denoise_rgb(&rgb, 20, 20, 0.01, 0.0, Some(Tiling { tile: 8, overlap: 1 })).is_err());
        let mut bad = rgb.clone();
        bad[3] = f32::NAN;
        assert!(den.denoise_rgb(&bad, 20, 20, 0.01, 0.0, None).is_err());
        std::fs::write(dir.join(CONTRACT_FILE), b"{ not json").unwrap();
        assert!(NafDenoiser::open_installed(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
