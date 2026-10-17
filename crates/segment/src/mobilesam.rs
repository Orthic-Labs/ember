//! MobileSAM (Zhang et al., 2023): the Segment Anything interface with a 5M-parameter TinyViT image
//! encoder, in pure Rust on candle. The default model behind Object masks: an ungated, Apache-2.0
//! checkpoint of 40.6 MB (see `fetch::MOBILESAM_FILES` and docs/ai-masks.md).
//!
//! - [`MobileSam::encode`] runs the image encoder once per photo (`1024 × 1024` input).
//! - [`MobileSam::segment`] turns clicks (and/or a box) into one mask: the decoder is cheap, so each
//!   further click reuses the [`MobileEncoded`] features.
//!
//! The network pieces (TinyViT encoder, mask decoder, prompt-encoder tables) are
//! `candle_transformers::models::segment_anything` (MIT OR Apache-2.0), loaded from the
//! `lmz/candle-sam` safetensors conversion of the official `mobile_sam.pt`. Prompt embeddings, the input
//! geometry and the mask post-processing are [`crate::mobilesam_prep`]. The weights are not part of Ember:
//! they are fetched only when the user asks.
//!
//! Without a usable GPU path (or when a GPU op is unsupported) the model runs on the CPU.

use std::path::Path;
use std::sync::Arc;

use candle_core::{DType, Device, Tensor};
use candle_nn::{Module, VarBuilder};
use candle_transformers::models::segment_anything::mask_decoder::MaskDecoder;
use candle_transformers::models::segment_anything::prompt_encoder::PromptEncoder;
use candle_transformers::models::segment_anything::tiny_vit::{TinyViT, tiny_vit_5m};

use crate::mobilesam_prep::{self as prep, EMBED_DIM, GAUSSIAN_FEATURES, INPUT, PromptPoint, PromptTables};
use crate::{Click, Error, Prediction, Result, weights};

/// Most points one prompt takes.
pub const MAX_POINTS: usize = 64;
/// A box smaller than this (a fraction of the photo, per side) is not a box.
const MIN_BOX: f32 = 1.0 / 512.0;

/// The network, built on one device.
struct Parts {
    device: Device,
    encoder: TinyViT,
    decoder: MaskDecoder,
    /// The decoder's positional encoding of the `64 × 64` embedding grid (constant).
    image_pe: Tensor,
    /// The decoder's dense prompt embedding when there is no mask prompt (constant).
    dense: Tensor,
    tables: PromptTables,
}

fn var_builder<'a>(w: &Arc<weights::Weights>, device: &Device) -> VarBuilder<'a> {
    VarBuilder::from_backend(Box::new(weights::Backend(w.clone())), DType::F32, device.clone())
}

fn host_vec(vb: &VarBuilder<'_>, shape: (usize, usize), name: &str) -> Result<Vec<f32>> {
    Ok(vb.get(shape, name)?.flatten_all()?.to_vec1::<f32>()?)
}

impl Parts {
    fn build(w: &Arc<weights::Weights>, device: &Device) -> Result<Self> {
        let vb = var_builder(w, device);
        let encoder = tiny_vit_5m(vb.pp("image_encoder"))?;
        let prompt_encoder = PromptEncoder::new(EMBED_DIM, (INPUT / 16, INPUT / 16), (INPUT, INPUT), 16, vb.pp("prompt_encoder"))?;
        let decoder = MaskDecoder::new(EMBED_DIM, 3, 3, 256, vb.pp("mask_decoder"))?;
        let image_pe = prompt_encoder.get_dense_pe()?;
        // (the sparse part of an empty prompt is empty; the dense part is the constant "no mask" embedding)
        let (_, dense) = prompt_encoder.forward(None, None, None)?;
        let pvb = vb.pp("prompt_encoder");
        let mut points = Vec::with_capacity(4 * EMBED_DIM);
        for i in 0..4 {
            points.extend(host_vec(&pvb.pp("point_embeddings").pp(i), (1, EMBED_DIM), "weight")?);
        }
        let tables = PromptTables {
            gaussian: host_vec(&pvb.pp("pe_layer"), (2, GAUSSIAN_FEATURES), "positional_encoding_gaussian_matrix")?,
            points,
            not_a_point: host_vec(&pvb.pp("not_a_point_embed"), (1, EMBED_DIM), "weight")?,
        };
        Ok(Parts { device: device.clone(), encoder, decoder, image_pe, dense, tables })
    }
}

/// A loaded MobileSAM.
pub struct MobileSam {
    weights: Arc<weights::Weights>,
    parts: Parts,
}

/// A photo run through the image encoder: what clicks and boxes are decoded from.
pub struct MobileEncoded {
    embeddings: Tensor,
    /// Size of the resized photo inside the model input (the part that is not padding).
    width: usize,
    height: usize,
}

impl MobileSam {
    /// Load from a directory holding [`crate::MOBILESAM_FILE`], on the best device here.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, crate::best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        let path = dir.join(crate::MOBILESAM_FILE);
        if !path.is_file() {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        let weights = weights::Weights::open(&path)?;
        let parts = Parts::build(&weights, &device)?;
        Ok(Self { weights, parts })
    }

    pub fn device(&self) -> &Device {
        &self.parts.device
    }

    /// Run the image encoder on an RGB photo (8-bit, row-major, `w × h`). If the GPU path fails, the model
    /// moves to the CPU and tries again once.
    pub fn encode(&mut self, rgb: &[u8], w: usize, h: usize) -> Result<MobileEncoded> {
        let prepared = prep::prepare(rgb, w, h)?;
        match self.encode_prepared(&prepared) {
            Err(e) if !self.parts.device.is_cpu() => {
                log::warn!("MobileSAM failed on {:?} ({e}); retrying on the CPU", self.parts.device);
                self.parts = Parts::build(&self.weights, &Device::Cpu)?;
                self.encode_prepared(&prepared)
            }
            other => other,
        }
    }

    fn encode_prepared(&self, p: &prep::Prepared) -> Result<MobileEncoded> {
        let pixels = Tensor::from_slice(&p.pixels, (1, 3, INPUT, INPUT), &self.parts.device)?;
        let embeddings = self.parts.encoder.forward(&pixels)?;
        // (a GPU reports a failed kernel at the first synchronization, not at the call that queued it)
        self.parts.device.synchronize()?;
        Ok(MobileEncoded { embeddings, width: p.width, height: p.height })
    }

    /// Segment one object. Prompt positions are normalized to the photo (0..1); `bx` is
    /// `[x0, y0, x1, y1]`. One click alone gives SAM's three alternatives, of which the best by predicted
    /// IoU is kept; several clicks, or a box, give the single mask the model predicts for them.
    ///
    /// The logits are `OUTPUT_SIDE²` over the whole photo (stretched to the square, like every stored
    /// mask); the score is the model's predicted IoU.
    pub fn segment(&self, enc: &MobileEncoded, clicks: &[Click], bx: Option<[f32; 4]>) -> Result<Prediction> {
        if clicks.len() > MAX_POINTS {
            return Err(Error::Model(format!("at most {MAX_POINTS} clicks")));
        }
        let (w, h) = (enc.width as f32, enc.height as f32);
        let mut points = Vec::with_capacity(clicks.len());
        for c in clicks {
            if !(c.x.is_finite() && c.y.is_finite()) {
                return Err(Error::Model("a click is not a position".into()));
            }
            points.push(PromptPoint { x: c.x.clamp(0.0, 1.0) * w, y: c.y.clamp(0.0, 1.0) * h, positive: c.positive });
        }
        let bx = match bx {
            None => None,
            Some(b) => {
                if !b.iter().all(|v| v.is_finite()) {
                    return Err(Error::Model("the box is not a position".into()));
                }
                let [x0, y0, x1, y1] = b.map(|v| v.clamp(0.0, 1.0));
                if x1 - x0 < MIN_BOX || y1 - y0 < MIN_BOX {
                    return Err(Error::Model("the box is empty".into()));
                }
                Some([x0 * w, y0 * h, x1 * w, y1 * h])
            }
        };
        let (rows, n) = prep::sparse_embeddings(&self.parts.tables, &points, bx)?;
        let sparse = Tensor::from_vec(rows, (1, n, EMBED_DIM), &self.parts.device)?;
        let multimask = points.len() == 1 && bx.is_none();
        let (masks, iou) = self.parts.decoder.forward(&enc.embeddings, &self.parts.image_pe, &sparse, &self.parts.dense, multimask)?;
        let iou = iou.flatten_all()?.to_vec1::<f32>()?;
        let best = prep::best_mask(&iou).ok_or_else(|| Error::Model("the model returned no mask".into()))?;
        let low = masks.get(0)?.get(best)?.flatten_all()?.to_vec1::<f32>()?;
        if low.iter().any(|v| !v.is_finite()) {
            return Err(Error::Model("the model returned an invalid mask".into()));
        }
        let logits = prep::resample_logits(&low, enc.width, enc.height, prep::OUTPUT_SIDE)?;
        Ok(Prediction { logits, score: iou.get(best).copied().unwrap_or(0.0).clamp(0.0, 1.0) })
    }
}

impl MobileEncoded {
    /// The encoder output `[1, 256, 64, 64]` (tests).
    pub fn embeddings(&self) -> &Tensor {
        &self.embeddings
    }

    /// Size of the resized photo inside the model input.
    pub fn resized(&self) -> (usize, usize) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-mobilesam-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_folder_without_the_checkpoint_is_not_installed() {
        let dir = scratch("empty");
        let e = MobileSam::load_on(&dir, Device::Cpu).err().unwrap();
        assert!(matches!(e, Error::Missing(_)), "{e}");
        assert!(e.to_string().contains("not found"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_checkpoint_is_an_error_not_a_crash() {
        let dir = scratch("damaged");
        std::fs::write(dir.join(crate::MOBILESAM_FILE), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
        let e = MobileSam::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains(crate::MOBILESAM_FILE), "{e}");
        // a header that claims tensors the file does not have (a truncated download)
        let header = r#"{"image_encoder.x":{"dtype":"F32","shape":[4,4],"data_offsets":[0,64]}}"#;
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        std::fs::write(dir.join(crate::MOBILESAM_FILE), bytes).unwrap();
        assert!(MobileSam::load_on(&dir, Device::Cpu).is_err());
        // a well-formed file without the model's tensors
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(dir.join(crate::MOBILESAM_FILE), bytes).unwrap();
        let e = MobileSam::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("image_encoder"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
