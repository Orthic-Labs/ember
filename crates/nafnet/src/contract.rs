//! The installed form of a NAFNet denoiser: a folder holding `model.safetensors` (F32, reference parameter names
//! and PyTorch layouts) and the `contract.json` that `denoise-train` writes beside the weights. The contract says
//! which network the weights are (the architecture config) and how the input is built; both files are untrusted
//! input, so sizes are capped and every mismatch is an error, never a panic.

use crate::error::{Error, Result};
use crate::model::{IN_CHANNELS, NafConfig, NafNet, OUT_CHANNELS, SIGMA_SCALE};
use candle_core::Device;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

pub const WEIGHTS_FILE: &str = "model.safetensors";
pub const CONTRACT_FILE: &str = "contract.json";
/// Largest `contract.json` read.
pub const MAX_CONTRACT_BYTES: u64 = 1024 * 1024;
/// Largest weights file read (the width-16 network is about 9 MB).
pub const MAX_WEIGHTS_BYTES: u64 = 512 * 1024 * 1024;

/// The parts of `contract.json` inference depends on; the rest (training provenance) is ignored.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Contract {
    pub format_version: u32,
    pub model: NafConfig,
    pub sigma_scale: f32,
}

impl Contract {
    pub fn parse(bytes: &[u8]) -> Result<Contract> {
        if bytes.len() as u64 > MAX_CONTRACT_BYTES {
            return Err(Error::Config("contract.json is too large".into()));
        }
        let c: Contract = serde_json::from_slice(bytes)?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<()> {
        if self.format_version != 1 {
            return Err(Error::Config(format!("contract format {} is not supported (this build reads 1)", self.format_version)));
        }
        self.model.validate()?;
        if self.model.in_channels != IN_CHANNELS || self.model.out_channels != OUT_CHANNELS {
            return Err(Error::Config(format!("the contract needs {IN_CHANNELS} input and {OUT_CHANNELS} output channels")));
        }
        if !self.sigma_scale.is_finite() || (self.sigma_scale - SIGMA_SCALE).abs() > 1e-6 {
            return Err(Error::Config(format!("the contract's sigma scale {} is not the {SIGMA_SCALE} this build builds", self.sigma_scale)));
        }
        Ok(())
    }
}

fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(Error::Data(format!("{} is larger than {max} bytes", path.display())));
    }
    Ok(bytes)
}

/// A loaded network with the contract it was loaded under.
pub struct NafModel {
    pub net: NafNet,
    pub contract: Contract,
}

impl NafModel {
    /// Whether `dir` holds a NAFNet model (a `contract.json` beside `model.safetensors`). Selection is by this
    /// alone: a folder without them is some other kind of model and is left to the caller.
    pub fn is_installed(dir: &Path) -> bool {
        dir.join(CONTRACT_FILE).is_file() && dir.join(WEIGHTS_FILE).is_file()
    }

    /// Load `dir/contract.json` and `dir/model.safetensors`. A malformed contract, a missing or wrongly shaped
    /// tensor, or an unreadable file is an error.
    pub fn open(dir: &Path, device: &Device) -> Result<NafModel> {
        let contract = Contract::parse(&read_capped(&dir.join(CONTRACT_FILE), MAX_CONTRACT_BYTES)?)?;
        let weights = read_capped(&dir.join(WEIGHTS_FILE), MAX_WEIGHTS_BYTES)?;
        let net = NafNet::from_safetensors_bytes(&contract.model, weights, device)?;
        Ok(NafModel { net, contract })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::DType;
    use candle_nn::{VarBuilder, VarMap};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ember-nafnet-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn contract_value(cfg: &NafConfig) -> serde_json::Value {
        // the shape denoise-train writes, with its extra provenance entries
        serde_json::json!({
            "format_version": 1,
            "model": cfg,
            "weights_format": "safetensors",
            "sigma_scale": SIGMA_SCALE,
            "loss": "l1",
            "splits": {"salt": "x"},
        })
    }

    fn contract_json(cfg: &NafConfig) -> String {
        contract_value(cfg).to_string()
    }

    /// Random weights (beta and gamma too, so every block does something) saved as a folder.
    fn install(dir: &Path, cfg: &NafConfig) -> (VarMap, NafNet) {
        let varmap = VarMap::new();
        let net = NafNet::new(cfg, VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu)).unwrap();
        for (name, v) in varmap.data().lock().unwrap().iter() {
            if name.ends_with("beta") || name.ends_with("gamma") {
                let t = candle_core::Tensor::rand(-0.5f32, 0.5f32, v.as_tensor().dims(), &Device::Cpu).unwrap();
                v.set(&t).unwrap();
            }
        }
        varmap.save(dir.join(WEIGHTS_FILE)).unwrap();
        std::fs::write(dir.join(CONTRACT_FILE), contract_json(cfg)).unwrap();
        (varmap, net)
    }

    fn input(h: usize, w: usize) -> Vec<f32> {
        (0..IN_CHANNELS * h * w).map(|i| ((i * 31 % 97) as f32) / 97.0).collect()
    }

    #[test]
    fn an_installed_folder_gives_the_training_forward_exactly() {
        let dir = scratch("roundtrip");
        let cfg = NafConfig::tiny();
        let (_, trained) = install(&dir, &cfg);
        assert!(NafModel::is_installed(&dir));
        let model = NafModel::open(&dir, &Device::Cpu).unwrap();
        assert_eq!(model.contract.model, cfg);
        let (h, w) = (18, 22);
        let x = input(h, w);
        let want = trained.infer(&x, h, w, &Device::Cpu).unwrap();
        let got = model.net.infer(&x, h, w, &Device::Cpu).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(got, want);
    }

    #[test]
    fn tiled_inference_matches_the_whole_image_when_the_residual_is_local() {
        // With the final convolution's weights zeroed every tile returns its input plus one constant, so a correct
        // blend (offsets, ramps, normalisation, coverage) reproduces the whole-image output within rounding.
        let dir = scratch("tiled");
        let cfg = NafConfig::tiny();
        let (varmap, _) = install(&dir, &cfg);
        crate::model::zero_init_ending(&varmap).unwrap();
        varmap.save(dir.join(WEIGHTS_FILE)).unwrap();
        let model = NafModel::open(&dir, &Device::Cpu).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let (h, w) = (45, 37);
        let x = input(h, w);
        let whole = model.net.infer(&x, h, w, &Device::Cpu).unwrap();
        let tiled = model.net.infer_tiled(&x, h, w, 24, 6, &Device::Cpu).unwrap();
        assert_eq!(tiled.len(), whole.len());
        let worst = tiled.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-5, "worst difference {worst}");
    }

    #[test]
    fn a_missing_tensor_is_an_error() {
        let dir = scratch("missing");
        let cfg = NafConfig::tiny();
        let varmap = VarMap::new();
        NafNet::new(&cfg, VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu)).unwrap();
        varmap.data().lock().unwrap().remove("decoders.0.0.conv3.weight");
        varmap.save(dir.join(WEIGHTS_FILE)).unwrap();
        std::fs::write(dir.join(CONTRACT_FILE), contract_json(&cfg)).unwrap();
        let res = NafModel::open(&dir, &Device::Cpu);
        std::fs::remove_dir_all(&dir).ok();
        assert!(res.is_err());
    }

    #[test]
    fn a_malformed_contract_is_an_error() {
        let cfg = NafConfig::tiny();
        assert!(Contract::parse(contract_json(&cfg).as_bytes()).is_ok());
        assert!(Contract::parse(b"not json").is_err());
        assert!(Contract::parse(b"{}").is_err());
        let with = |key: &str, value: serde_json::Value| {
            let mut v = contract_value(&cfg);
            v[key] = value;
            v.to_string()
        };
        assert!(Contract::parse(with("format_version", serde_json::json!(2)).as_bytes()).is_err());
        assert!(Contract::parse(with("sigma_scale", serde_json::json!(4.0)).as_bytes()).is_err());
        let mut wrong_width = cfg.clone();
        wrong_width.in_channels = 3;
        assert!(Contract::parse(contract_json(&wrong_width).as_bytes()).is_err(), "3 input channels is not this contract");
        let mut no_stages = cfg.clone();
        no_stages.enc_blocks.clear();
        no_stages.dec_blocks.clear();
        assert!(Contract::parse(contract_json(&no_stages).as_bytes()).is_err());
        // end to end: a folder whose contract is malformed
        let dir = scratch("badcontract");
        install(&dir, &cfg);
        std::fs::write(dir.join(CONTRACT_FILE), b"{\"format_version\": 1").unwrap();
        let res = NafModel::open(&dir, &Device::Cpu);
        std::fs::remove_dir_all(&dir).ok();
        assert!(res.is_err());
    }

    #[test]
    fn a_folder_without_the_pair_is_not_a_nafnet_install() {
        let dir = scratch("empty");
        assert!(!NafModel::is_installed(&dir));
        assert!(NafModel::open(&dir, &Device::Cpu).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
