//! NAFNet (Chen et al., "Simple Baselines for Image Restoration", MIT) re-implemented on candle, with the
//! official parameter names so a checkpoint is a plain safetensors file whose keys read like the reference
//! implementation: `intro`, `ending`, `encoders.{i}.{j}.*`, `middle_blks.{j}.*`, `decoders.{i}.{j}.*`,
//! `downs.{i}`, `ups.{i}.0` (1x1 conv without bias, then pixel shuffle) and per block `norm1/2`, `conv1..5`,
//! `sca.1`, `beta`, `gamma`. Tensor layouts match PyTorch (`[out, in/groups, kh, kw]`, `beta`/`gamma` as
//! `[1, c, 1, 1]`), so [`NafNet::load`] reads a PyTorch-exported state dict of the same shapes unchanged.
//!
//! The architecture code is written from the paper and the structural facts audited in
//! `docs/models/denoise-candidates.md`; no reference code or weights are copied.
//!
//! Differences from the reference, all deliberate:
//! * The input has [`IN_CHANNELS`] = 6 channels: noisy linear RGB plus a per-pixel noise-level map derived from
//!   `(a, b)` (see [`model_input_chw`]), so one network covers every ISO. The reference global skip adds the
//!   input to the output; here it adds the first three (RGB) channels.
//! * Width 16 with a small block budget (about 2 M parameters), see [`NafConfig::width16`].
//!
//! `crates/denoise`'s ONNX path loads plain U-Nets (no depthwise, no channel attention, no LayerNorm), so it cannot
//! run this architecture; the product reaches it through this crate (issue #200).

use crate::error::{Error, Result, config};
use candle_core::{DType, Device, Tensor};
use candle_nn::{Conv2dConfig, Init, Module, VarBuilder, conv2d, conv2d_no_bias};
use serde::{Deserialize, Serialize};

/// Noisy RGB + three noise-level channels.
pub const IN_CHANNELS: usize = 6;
pub const OUT_CHANNELS: usize = 3;
/// The noise-level channels are `sqrt(a·y + b)` times this, so a typical value is near 1.
pub const SIGMA_SCALE: f32 = 16.0;
const LN_EPS: f64 = 1e-6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NafConfig {
    pub in_channels: usize,
    pub out_channels: usize,
    pub width: usize,
    pub middle_blocks: usize,
    pub enc_blocks: Vec<usize>,
    pub dec_blocks: Vec<usize>,
}

impl NafConfig {
    /// The width-16 class: channels 16, 32, 64, 128 and 256 in the middle; 2 190 947 parameters.
    pub fn width16() -> NafConfig {
        NafConfig { in_channels: IN_CHANNELS, out_channels: OUT_CHANNELS, width: 16, middle_blocks: 3, enc_blocks: vec![1, 1, 1, 2], dec_blocks: vec![1, 1, 1, 1] }
    }

    /// A few thousand parameters, for tests.
    pub fn tiny() -> NafConfig {
        NafConfig { in_channels: IN_CHANNELS, out_channels: OUT_CHANNELS, width: 4, middle_blocks: 1, enc_blocks: vec![1, 1], dec_blocks: vec![1, 1] }
    }

    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.width > 256 {
            return config("width must be 1..=256");
        }
        if self.enc_blocks.is_empty() || self.enc_blocks.len() > 6 || self.enc_blocks.len() != self.dec_blocks.len() {
            return config("encoder and decoder need the same number (1..=6) of stages");
        }
        if self.in_channels < self.out_channels || self.out_channels == 0 {
            return config("the skip connection needs in_channels >= out_channels >= 1");
        }
        if self.enc_blocks.iter().chain(&self.dec_blocks).any(|&n| n > 64) || self.middle_blocks > 64 {
            return config("too many blocks in a stage");
        }
        Ok(())
    }

    /// Inputs are zero-padded (right, bottom) to a multiple of this, as in the reference.
    pub fn pad_multiple(&self) -> usize {
        1usize << self.enc_blocks.len()
    }

    /// Parameter count from the closed form: a block with `c` channels has `7c² + 33c` (convolutions, biases,
    /// both LayerNorms' affine terms, `beta`, `gamma`); see `docs/models/denoise-candidates.md`. Independent of
    /// the layer code, so a test compares the two.
    pub fn expected_params(&self) -> usize {
        let block = |c: usize| 7 * c * c + 33 * c;
        let mut total = self.in_channels * self.width * 9 + self.width; // intro
        let mut c = self.width;
        for &n in &self.enc_blocks {
            total += n * block(c);
            total += c * (2 * c) * 4 + 2 * c; // down: 2x2 stride 2, with bias
            c *= 2;
        }
        total += self.middle_blocks * block(c);
        for &n in &self.dec_blocks {
            total += c * (2 * c); // up: 1x1, no bias, then pixel shuffle to c / 2
            c /= 2;
            total += n * block(c);
        }
        total + c * self.out_channels * 9 + self.out_channels // ending
    }
}

fn conv_cfg(padding: usize, stride: usize, groups: usize) -> Conv2dConfig {
    Conv2dConfig { padding, stride, groups, ..Default::default() }
}

struct LayerNorm2d {
    weight: Tensor,
    bias: Tensor,
    c: usize,
}

impl LayerNorm2d {
    fn new(c: usize, vb: VarBuilder) -> Result<Self> {
        Ok(LayerNorm2d { weight: vb.get_with_hints(c, "weight", Init::Const(1.0))?, bias: vb.get_with_hints(c, "bias", Init::Const(0.0))?, c })
    }

    /// Per-pixel normalisation over the channels (not over space), epsilon 1e-6, as in the reference.
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let mu = x.mean_keepdim(1)?;
        let xc = x.broadcast_sub(&mu)?;
        let var = xc.sqr()?.mean_keepdim(1)?;
        let y = xc.broadcast_div(&var.affine(1.0, LN_EPS)?.sqrt()?)?;
        let w = self.weight.reshape((1, self.c, 1, 1))?;
        let b = self.bias.reshape((1, self.c, 1, 1))?;
        Ok(y.broadcast_mul(&w)?.broadcast_add(&b)?)
    }
}

fn simple_gate(x: &Tensor) -> Result<Tensor> {
    let c = x.dim(1)?;
    let half = c / 2;
    let a = x.narrow(1, 0, half)?;
    let b = x.narrow(1, half, half)?;
    Ok((&a * &b)?)
}

fn pixel_shuffle(x: &Tensor, r: usize) -> Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    let oc = c / (r * r);
    let y = x.reshape(vec![b, oc, r, r, h, w])?;
    let y = y.permute(vec![0usize, 1, 4, 2, 5, 3])?.contiguous()?;
    Ok(y.reshape((b, oc, h * r, w * r))?)
}

struct NafBlock {
    norm1: LayerNorm2d,
    norm2: LayerNorm2d,
    conv1: candle_nn::Conv2d,
    conv2: candle_nn::Conv2d,
    conv3: candle_nn::Conv2d,
    sca: candle_nn::Conv2d,
    conv4: candle_nn::Conv2d,
    conv5: candle_nn::Conv2d,
    beta: Tensor,
    gamma: Tensor,
}

impl NafBlock {
    fn new(c: usize, vb: VarBuilder) -> Result<Self> {
        let dw = c * 2;
        let ffn = c * 2;
        Ok(NafBlock {
            norm1: LayerNorm2d::new(c, vb.pp("norm1"))?,
            norm2: LayerNorm2d::new(c, vb.pp("norm2"))?,
            conv1: conv2d(c, dw, 1, conv_cfg(0, 1, 1), vb.pp("conv1"))?,
            conv2: conv2d(dw, dw, 3, conv_cfg(1, 1, dw), vb.pp("conv2"))?,
            conv3: conv2d(dw / 2, c, 1, conv_cfg(0, 1, 1), vb.pp("conv3"))?,
            sca: conv2d(dw / 2, dw / 2, 1, conv_cfg(0, 1, 1), vb.pp("sca").pp("1"))?,
            conv4: conv2d(c, ffn, 1, conv_cfg(0, 1, 1), vb.pp("conv4"))?,
            conv5: conv2d(ffn / 2, c, 1, conv_cfg(0, 1, 1), vb.pp("conv5"))?,
            beta: vb.get_with_hints((1, c, 1, 1), "beta", Init::Const(0.0))?,
            gamma: vb.get_with_hints((1, c, 1, 1), "gamma", Init::Const(0.0))?,
        })
    }

    fn forward(&self, inp: &Tensor) -> Result<Tensor> {
        let x = self.norm1.forward(inp)?;
        let x = self.conv1.forward(&x)?;
        let x = self.conv2.forward(&x)?;
        let x = simple_gate(&x)?;
        // simplified channel attention: global average pool, 1x1 conv, channel-wise scale
        let pooled = x.mean_keepdim(2)?.mean_keepdim(3)?;
        let x = x.broadcast_mul(&self.sca.forward(&pooled)?)?;
        let x = self.conv3.forward(&x)?;
        let y = (inp + x.broadcast_mul(&self.beta)?)?;
        let x = self.conv4.forward(&self.norm2.forward(&y)?)?;
        let x = simple_gate(&x)?;
        let x = self.conv5.forward(&x)?;
        Ok((&y + x.broadcast_mul(&self.gamma)?)?)
    }
}

pub struct NafNet {
    cfg: NafConfig,
    intro: candle_nn::Conv2d,
    ending: candle_nn::Conv2d,
    encoders: Vec<Vec<NafBlock>>,
    downs: Vec<candle_nn::Conv2d>,
    middle: Vec<NafBlock>,
    ups: Vec<candle_nn::Conv2d>,
    decoders: Vec<Vec<NafBlock>>,
}

impl NafNet {
    /// Build the network over `vb`: fresh variables when `vb` is backed by a `VarMap`, loaded tensors (shapes
    /// checked) when it is backed by a safetensors file.
    pub fn new(cfg: &NafConfig, vb: VarBuilder) -> Result<NafNet> {
        cfg.validate()?;
        let intro = conv2d(cfg.in_channels, cfg.width, 3, conv_cfg(1, 1, 1), vb.pp("intro"))?;
        let mut c = cfg.width;
        let mut encoders = Vec::new();
        let mut downs = Vec::new();
        for (i, &n) in cfg.enc_blocks.iter().enumerate() {
            let mut blocks = Vec::new();
            for j in 0..n {
                blocks.push(NafBlock::new(c, vb.pp("encoders").pp(i).pp(j))?);
            }
            encoders.push(blocks);
            downs.push(conv2d(c, 2 * c, 2, conv_cfg(0, 2, 1), vb.pp("downs").pp(i))?);
            c *= 2;
        }
        let mut middle = Vec::new();
        for j in 0..cfg.middle_blocks {
            middle.push(NafBlock::new(c, vb.pp("middle_blks").pp(j))?);
        }
        let mut ups = Vec::new();
        let mut decoders = Vec::new();
        for (i, &n) in cfg.dec_blocks.iter().enumerate() {
            ups.push(conv2d_no_bias(c, 2 * c, 1, conv_cfg(0, 1, 1), vb.pp("ups").pp(i).pp(0))?);
            c /= 2;
            let mut blocks = Vec::new();
            for j in 0..n {
                blocks.push(NafBlock::new(c, vb.pp("decoders").pp(i).pp(j))?);
            }
            decoders.push(blocks);
        }
        let ending = conv2d(c, cfg.out_channels, 3, conv_cfg(1, 1, 1), vb.pp("ending"))?;
        Ok(NafNet { cfg: cfg.clone(), intro, ending, encoders, downs, middle, ups, decoders })
    }

    /// Load a checkpoint written by [`candle_nn::VarMap::save`] (or any safetensors file with these names and
    /// shapes). Read into memory and parsed safely: no memory mapping.
    pub fn load(cfg: &NafConfig, path: &std::path::Path, device: &Device) -> Result<NafNet> {
        NafNet::from_safetensors_bytes(cfg, std::fs::read(path)?, device)
    }

    /// As [`Self::load`], from a file already read into memory.
    pub fn from_safetensors_bytes(cfg: &NafConfig, bytes: Vec<u8>, device: &Device) -> Result<NafNet> {
        let vb = VarBuilder::from_buffered_safetensors(bytes, DType::F32, device)?;
        NafNet::new(cfg, vb)
    }

    pub fn config(&self) -> &NafConfig {
        &self.cfg
    }

    /// `x`: `[n, in_channels, h, w]`, any `h`, `w`. Returns `[n, out_channels, h, w]`: the network's residual
    /// added to the noisy RGB (the first three input channels).
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, _, h, w) = x.dims4()?;
        let m = self.cfg.pad_multiple();
        let (ph, pw) = ((m - h % m) % m, (m - w % m) % m);
        let padded = if ph > 0 || pw > 0 { x.pad_with_zeros(2, 0, ph)?.pad_with_zeros(3, 0, pw)? } else { x.clone() };

        let mut y = self.intro.forward(&padded)?;
        let mut skips = Vec::with_capacity(self.encoders.len());
        for (blocks, down) in self.encoders.iter().zip(&self.downs) {
            for b in blocks {
                y = b.forward(&y)?;
            }
            skips.push(y.clone());
            y = down.forward(&y)?;
        }
        for b in &self.middle {
            y = b.forward(&y)?;
        }
        for ((blocks, up), skip) in self.decoders.iter().zip(&self.ups).zip(skips.iter().rev()) {
            y = pixel_shuffle(&up.forward(&y)?, 2)?;
            y = (&y + skip)?;
            for b in blocks {
                y = b.forward(&y)?;
            }
        }
        let y = self.ending.forward(&y)?;
        let rgb = padded.narrow(1, 0, self.cfg.out_channels)?;
        let out = (&y + &rgb)?;
        Ok(out.narrow(2, 0, h)?.narrow(3, 0, w)?)
    }

    /// Whole-image inference for one `[in_channels, h, w]` CHW buffer; returns `[out_channels, h, w]`.
    pub fn infer(&self, input: &[f32], h: usize, w: usize, device: &Device) -> Result<Vec<f32>> {
        let want = self.cfg.in_channels * h * w;
        if h == 0 || w == 0 || input.len() != want {
            return Err(Error::Data(format!("input has {} samples, a {h}x{w} image with {} channels needs {want}", input.len(), self.cfg.in_channels)));
        }
        let x = Tensor::from_vec(input.to_vec(), (1, self.cfg.in_channels, h, w), device)?;
        let y = self.forward(&x)?.flatten_all()?;
        Ok(y.to_vec1::<f32>()?)
    }

    /// Tiled inference: overlapping `tile`-sized windows blended with a linear ramp across each overlap, the way
    /// a memory-bounded runner would. Channel attention sees only each tile, so this differs from [`Self::infer`];
    /// the evaluation reports by how much.
    pub fn infer_tiled(&self, input: &[f32], h: usize, w: usize, tile: usize, overlap: usize, device: &Device) -> Result<Vec<f32>> {
        let cin = self.cfg.in_channels;
        let cout = self.cfg.out_channels;
        if tile == 0 || overlap * 2 >= tile {
            return config("tile must exceed twice the overlap");
        }
        if input.len() != cin * h * w || h == 0 || w == 0 {
            return Err(Error::Data("input size does not match h, w and channels".into()));
        }
        if h <= tile && w <= tile {
            return self.infer(input, h, w, device);
        }
        let step = tile - overlap;
        let starts = |n: usize| -> Vec<usize> {
            let mut v = Vec::new();
            let mut s = 0usize;
            loop {
                let s_clamped = s.min(n.saturating_sub(tile));
                if v.last() != Some(&s_clamped) {
                    v.push(s_clamped);
                }
                if s + tile >= n {
                    break;
                }
                s += step;
            }
            v
        };
        let mut acc = vec![0f32; cout * h * w];
        let mut wsum = vec![0f32; h * w];
        for &y0 in &starts(h) {
            for &x0 in &starts(w) {
                let th = tile.min(h - y0);
                let tw = tile.min(w - x0);
                let mut sub = vec![0f32; cin * th * tw];
                for c in 0..cin {
                    for y in 0..th {
                        let src = c * h * w + (y0 + y) * w + x0;
                        let dst = c * th * tw + y * tw;
                        sub[dst..dst + tw].copy_from_slice(&input[src..src + tw]);
                    }
                }
                let out = self.infer(&sub, th, tw, device)?;
                // weight ramps up over `overlap` pixels from each tile edge that is not the image edge
                let ramp = |i: usize, n: usize, at_start_edge: bool, at_end_edge: bool| -> f32 {
                    let lo = if at_start_edge || overlap == 0 { 1.0 } else { ((i + 1) as f32 / (overlap + 1) as f32).min(1.0) };
                    let hi = if at_end_edge || overlap == 0 { 1.0 } else { ((n - i) as f32 / (overlap + 1) as f32).min(1.0) };
                    lo.min(hi)
                };
                for y in 0..th {
                    let wy = ramp(y, th, y0 == 0, y0 + th >= h);
                    for x in 0..tw {
                        let wx = ramp(x, tw, x0 == 0, x0 + tw >= w);
                        let wgt = wy * wx;
                        let o = (y0 + y) * w + x0 + x;
                        wsum[o] += wgt;
                        for c in 0..cout {
                            acc[c * h * w + o] += wgt * out[c * th * tw + y * tw + x];
                        }
                    }
                }
            }
        }
        for c in 0..cout {
            for o in 0..h * w {
                let s = wsum[o];
                acc[c * h * w + o] = if s > 0.0 { acc[c * h * w + o] / s } else { 0.0 };
            }
        }
        Ok(acc)
    }
}

/// Zero the final convolution's weights (its bias keeps its random start, which the first optimiser steps
/// remove), so a fresh network is "noisy RGB plus a constant" and training starts from the identity rather than
/// from a random residual. Call once on a new `VarMap`, never on a resumed one.
pub fn zero_init_ending(varmap: &candle_nn::VarMap) -> Result<()> {
    let data = varmap.data().lock().map_err(|_| Error::Config("variable map lock poisoned".into()))?;
    if let Some(v) = data.get("ending.weight") {
        v.set(&v.as_tensor().zeros_like()?)?;
    }
    Ok(())
}

/// The network input for one noisy linear RGB image (`3·n` samples, CHW, `n` pixels): the RGB itself and, per
/// channel, the expected noise sigma `sqrt(a·max(y, 0) + b)` at that sample, scaled by [`SIGMA_SCALE`]. The map is
/// a deterministic function of the noisy sample and `(a, b)`: nothing but the conditioning leaks into it.
pub fn model_input_chw(noisy: &[f32], a: f32, b: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(noisy.len() * 2);
    out.extend_from_slice(noisy);
    out.extend(noisy.iter().map(|&y| {
        let v = if y.is_finite() { y.max(0.0) } else { 0.0 };
        (a * v + b).max(0.0).sqrt() * SIGMA_SCALE
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_nn::VarMap;

    fn build(cfg: &NafConfig) -> (VarMap, NafNet) {
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu);
        let net = NafNet::new(cfg, vb).unwrap();
        (varmap, net)
    }

    fn param_count(varmap: &VarMap) -> usize {
        varmap.all_vars().iter().map(|v| v.as_tensor().elem_count()).sum()
    }

    #[test]
    fn parameter_count_matches_the_closed_form() {
        for cfg in [NafConfig::tiny(), NafConfig::width16()] {
            let (varmap, _) = build(&cfg);
            assert_eq!(param_count(&varmap), cfg.expected_params(), "{cfg:?}");
        }
        let n = NafConfig::width16().expected_params();
        assert!((1_500_000..3_000_000).contains(&n), "the width-16 class is about 2 M parameters, not {n}");
    }

    #[test]
    fn variable_names_follow_the_reference_implementation() {
        let (varmap, _) = build(&NafConfig::tiny());
        let data = varmap.data().lock().unwrap();
        for key in [
            "intro.weight",
            "intro.bias",
            "ending.weight",
            "ending.bias",
            "encoders.0.0.norm1.weight",
            "encoders.0.0.norm2.bias",
            "encoders.0.0.conv1.weight",
            "encoders.0.0.conv2.weight",
            "encoders.0.0.conv3.bias",
            "encoders.0.0.sca.1.weight",
            "encoders.0.0.sca.1.bias",
            "encoders.0.0.conv4.weight",
            "encoders.0.0.conv5.weight",
            "encoders.0.0.beta",
            "encoders.0.0.gamma",
            "downs.0.weight",
            "downs.0.bias",
            "middle_blks.0.norm1.weight",
            "ups.0.0.weight",
            "decoders.0.0.gamma",
        ] {
            assert!(data.contains_key(key), "missing {key}; have {:?}", data.keys().collect::<Vec<_>>());
        }
        assert!(!data.contains_key("ups.0.0.bias"), "the up convolution has no bias in the reference");
        let beta = data.get("encoders.0.0.beta").unwrap();
        assert_eq!(beta.as_tensor().dims(), &[1, 4, 1, 1]);
        let dw = data.get("encoders.0.0.conv2.weight").unwrap();
        assert_eq!(dw.as_tensor().dims(), &[8, 1, 3, 3], "depthwise: [dw, 1, 3, 3]");
    }

    #[test]
    fn forward_keeps_the_size_for_any_input() {
        let (_, net) = build(&NafConfig::tiny());
        // 18 x 20 is not a multiple of the pad multiple 4
        let (h, w) = (18, 20);
        let input: Vec<f32> = (0..IN_CHANNELS * h * w).map(|i| ((i * 31 % 97) as f32) / 97.0).collect();
        let out = net.infer(&input, h, w, &Device::Cpu).unwrap();
        assert_eq!(out.len(), OUT_CHANNELS * h * w);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_zero_initialised_ending_makes_the_network_the_identity_on_rgb() {
        let (varmap, net) = build(&NafConfig::tiny());
        zero_init_ending(&varmap).unwrap();
        let (h, w) = (18, 20);
        let input: Vec<f32> = (0..IN_CHANNELS * h * w).map(|i| ((i * 31 % 97) as f32) / 97.0).collect();
        let out = net.infer(&input, h, w, &Device::Cpu).unwrap();
        // the ending bias is still random, so compare after removing it: out - rgb is constant per channel
        for c in 0..OUT_CHANNELS {
            let plane = |v: &[f32]| v[c * h * w..(c + 1) * h * w].to_vec();
            let diffs: Vec<f32> = plane(&out).iter().zip(plane(&input)).map(|(o, i)| o - i).collect();
            let first = diffs[0];
            assert!(diffs.iter().all(|d| (d - first).abs() < 1e-5), "channel {c} is not input + constant");
        }
    }

    #[test]
    fn a_checkpoint_round_trips_through_safetensors_bit_exactly() {
        let cfg = NafConfig::tiny();
        let (varmap, net) = build(&cfg);
        let (h, w) = (16, 16);
        let input: Vec<f32> = (0..IN_CHANNELS * h * w).map(|i| ((i * 13 % 89) as f32) / 89.0).collect();
        let want = net.infer(&input, h, w, &Device::Cpu).unwrap();
        let path = std::env::temp_dir().join(format!("ember-naf-roundtrip-{}.safetensors", std::process::id()));
        varmap.save(&path).unwrap();
        let loaded = NafNet::load(&cfg, &path, &Device::Cpu).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.infer(&input, h, w, &Device::Cpu).unwrap(), want);
    }

    #[test]
    fn loading_a_checkpoint_of_the_wrong_shape_is_an_error() {
        let (varmap, _) = build(&NafConfig::tiny());
        let path = std::env::temp_dir().join(format!("ember-naf-shape-{}.safetensors", std::process::id()));
        varmap.save(&path).unwrap();
        let mut other = NafConfig::tiny();
        other.width = 8;
        let res = NafNet::load(&other, &path, &Device::Cpu);
        std::fs::remove_file(&path).ok();
        assert!(res.is_err());
    }

    #[test]
    fn pixel_shuffle_matches_the_depth_to_space_definition() {
        // channel c*4 + 2*dy + dx at (y, x) lands at (2y + dy, 2x + dx) of channel c
        let (c, h, w) = (2usize, 2usize, 3usize);
        let data: Vec<f32> = (0..c * 4 * h * w).map(|i| i as f32).collect();
        let x = Tensor::from_vec(data.clone(), (1, c * 4, h, w), &Device::Cpu).unwrap();
        let y = pixel_shuffle(&x, 2).unwrap();
        assert_eq!(y.dims(), &[1, c, 2 * h, 2 * w]);
        let out = y.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        for ch in 0..c {
            for yy in 0..h {
                for xx in 0..w {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let src = data[((ch * 4 + 2 * dy + dx) * h + yy) * w + xx];
                            let dst = out[(ch * 2 * h + 2 * yy + dy) * 2 * w + 2 * xx + dx];
                            assert_eq!(src, dst);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tiled_inference_blends_tiles_into_one_consistent_picture() {
        let (varmap, net) = build(&NafConfig::tiny());
        let (h, w) = (40, 36);
        let input: Vec<f32> = (0..IN_CHANNELS * h * w).map(|i| ((i * 7 % 101) as f32) / 101.0).collect();
        // with a zero ending every tile returns its input plus the same constant, so a correct blend (weights
        // normalised, every pixel covered, tiles placed at the right offsets) reproduces whole-image output
        zero_init_ending(&varmap).unwrap();
        let whole = net.infer(&input, h, w, &Device::Cpu).unwrap();
        let tiled = net.infer_tiled(&input, h, w, 24, 6, &Device::Cpu).unwrap();
        assert_eq!(tiled.len(), whole.len());
        for (a, b) in tiled.iter().zip(&whole) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
        // a tile that covers the image is the whole-image path exactly
        assert_eq!(net.infer_tiled(&input, h, w, 64, 8, &Device::Cpu).unwrap(), whole);
    }

    #[test]
    fn the_noise_level_channels_follow_the_model() {
        let inp = model_input_chw(&[0.0, 0.25, -0.1, f32::NAN], 0.01, 0.0004);
        assert_eq!(inp.len(), 8);
        assert!((inp[4] - 0.02 * SIGMA_SCALE).abs() < 1e-6);
        assert!((inp[5] - (0.01f32 * 0.25 + 0.0004).sqrt() * SIGMA_SCALE).abs() < 1e-6);
        assert_eq!(inp[6], inp[4], "negative samples read as black");
        assert_eq!(inp[7], inp[4], "NaN reads as black");
    }
}
