//! MobileSAM (SAM v1 interface) input and output geometry in plain Rust: how a photo becomes the
//! model's `1024 × 1024` input, how prompts become the decoder's sparse embeddings, and how the
//! decoder's `256 × 256` low-resolution logits become a mask over the photo. No model, no candle:
//! everything here is testable against the reference values of the upstream code.
//!
//! Reference: `segment_anything/utils/transforms.py` (`ResizeLongestSide`),
//! `segment_anything/modeling/sam.py` (`preprocess`, `postprocess_masks`) and
//! `segment_anything/modeling/prompt_encoder.py` (`PositionEmbeddingRandom`, `_embed_points`,
//! `_embed_boxes`), Copyright (c) Meta Platforms, Inc. and affiliates, Apache License 2.0; MobileSAM
//! (Zhang et al., 2023, Apache License 2.0) keeps this pipeline unchanged and swaps the image encoder.
//!
//! Modified work (Apache License 2.0, §4(b)): translated to Rust by the Ember contributors in 2026
//! and restructured (the prompt embeddings are computed on the host from the checkpoint's tables, which
//! also gives box prompts, that `candle-transformers`' prompt encoder does not do correctly). See NOTICE.

use crate::{Error, Result};

/// Side of the square model input; the photo's long edge is resized to it.
pub const INPUT: usize = 1024;
/// Side of the low-resolution masks the decoder predicts (`INPUT / 4`).
pub const LOW_RES: usize = 256;
/// Side of the grid of mask logits Ember stores for an Object selection.
pub const OUTPUT_SIDE: usize = LOW_RES;
/// Channels of the prompt embeddings.
pub const EMBED_DIM: usize = 256;
/// Fourier features of the positional encoding per axis pair (`EMBED_DIM / 2`).
pub const GAUSSIAN_FEATURES: usize = EMBED_DIM / 2;
/// Per-channel mean the reference subtracts (on 0..255 values).
pub const PIXEL_MEAN: [f32; 3] = [123.675, 116.28, 103.53];
/// Per-channel standard deviation the reference divides by (on 0..255 values).
pub const PIXEL_STD: [f32; 3] = [58.395, 57.12, 57.375];
/// Largest source side accepted (a photo's model input is rendered at 1024 long edge; this bounds hostile sizes).
const MAX_SOURCE_SIDE: usize = 1 << 16;

/// The size `(width, height)` the photo is resized to: long edge `INPUT`, the other side scaled and
/// rounded half up (`ResizeLongestSide.get_preprocess_shape`), at least 1.
pub fn resized_shape(w: usize, h: usize) -> Result<(usize, usize)> {
    if w == 0 || h == 0 || w > MAX_SOURCE_SIDE || h > MAX_SOURCE_SIDE {
        return Err(Error::Model(format!("unsupported image size {w} × {h}")));
    }
    let scale = INPUT as f64 / w.max(h) as f64;
    let side = |n: usize| (((n as f64) * scale + 0.5) as usize).clamp(1, INPUT);
    Ok((side(w), side(h)))
}

/// A photo ready for the image encoder.
pub struct Prepared {
    /// `[3, INPUT, INPUT]` channel-first: the resized photo normalized with [`PIXEL_MEAN`] and
    /// [`PIXEL_STD`] in its top-left corner, zeros elsewhere (the reference pads after normalizing).
    pub pixels: Vec<f32>,
    /// Size of the resized (unpadded) photo inside the input.
    pub width: usize,
    pub height: usize,
}

/// Resize, normalize and pad an 8-bit RGB photo (`w × h`, row-major) like the reference `SamPredictor.set_image`.
pub fn prepare(rgb: &[u8], w: usize, h: usize) -> Result<Prepared> {
    let (nw, nh) = resized_shape(w, h)?;
    let resized = crate::mask::resize_rgb(rgb, w, h, nw, nh)?;
    let mut pixels = vec![0f32; 3 * INPUT * INPUT];
    for (c, plane) in pixels.chunks_mut(INPUT * INPUT).enumerate() {
        let (mean, std) = (PIXEL_MEAN.get(c).copied().unwrap_or(0.0), PIXEL_STD.get(c).copied().unwrap_or(1.0));
        for y in 0..nh {
            for x in 0..nw {
                let v = f32::from(resized.get((y * nw + x) * 3 + c).copied().unwrap_or(0));
                if let Some(d) = plane.get_mut(y * INPUT + x) {
                    *d = (v - mean) / std;
                }
            }
        }
    }
    Ok(Prepared { pixels, width: nw, height: nh })
}

/// A point prompt in the resized frame (pixels of the `width × height` photo inside the input).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PromptPoint {
    pub x: f32,
    pub y: f32,
    /// A point on the object (`true`) or on the background (`false`).
    pub positive: bool,
}

/// The checkpoint's prompt-encoder tables (`prompt_encoder.*`), flattened row-major.
#[derive(Clone, Debug, Default)]
pub struct PromptTables {
    /// `pe_layer.positional_encoding_gaussian_matrix`, `[2, GAUSSIAN_FEATURES]`.
    pub gaussian: Vec<f32>,
    /// `point_embeddings.0..3.weight`, `[4, EMBED_DIM]`: background, object, box top-left, box bottom-right.
    pub points: Vec<f32>,
    /// `not_a_point_embed.weight`, `[EMBED_DIM]`: the padding point.
    pub not_a_point: Vec<f32>,
}

impl PromptTables {
    fn check(&self) -> Result<()> {
        if self.gaussian.len() != 2 * GAUSSIAN_FEATURES || self.points.len() != 4 * EMBED_DIM || self.not_a_point.len() != EMBED_DIM {
            return Err(Error::Model("MobileSAM prompt tables have the wrong size".into()));
        }
        Ok(())
    }

    fn point_row(&self, i: usize) -> &[f32] {
        self.points.get(i * EMBED_DIM..(i + 1) * EMBED_DIM).unwrap_or_default()
    }

    /// `PositionEmbeddingRandom._pe_encoding` of the pixel `(x, y)` (its centre, `+ 0.5`, over the input).
    fn encode(&self, x: f32, y: f32) -> Vec<f32> {
        let cx = (x + 0.5) / INPUT as f32 * 2.0 - 1.0;
        let cy = (y + 0.5) / INPUT as f32 * 2.0 - 1.0;
        let mut out = vec![0f32; EMBED_DIM];
        for j in 0..GAUSSIAN_FEATURES {
            let gx = self.gaussian.get(j).copied().unwrap_or(0.0);
            let gy = self.gaussian.get(GAUSSIAN_FEATURES + j).copied().unwrap_or(0.0);
            let angle = (cx * gx + cy * gy) * std::f32::consts::TAU;
            if let Some(s) = out.get_mut(j) {
                *s = angle.sin();
            }
            if let Some(c) = out.get_mut(GAUSSIAN_FEATURES + j) {
                *c = angle.cos();
            }
        }
        out
    }
}

/// The decoder's sparse prompt embeddings, `[n, EMBED_DIM]` row-major, as `PromptEncoder.forward`
/// builds them: the points first (then, without a box, one padding point), then the box's two corners.
/// Coordinates are pixels of the resized photo. Returns the rows and their count.
pub fn sparse_embeddings(tables: &PromptTables, points: &[PromptPoint], bx: Option<[f32; 4]>) -> Result<(Vec<f32>, usize)> {
    tables.check()?;
    if points.is_empty() && bx.is_none() {
        return Err(Error::Model("no prompt".into()));
    }
    let finite = points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) && bx.is_none_or(|b| b.iter().all(|v| v.is_finite()));
    if !finite {
        return Err(Error::Model("a prompt coordinate is not a number".into()));
    }
    let mut out: Vec<f32> = Vec::with_capacity((points.len() + 2) * EMBED_DIM);
    let mut rows = 0usize;
    for p in points {
        let pe = tables.encode(p.x, p.y);
        let label = tables.point_row(usize::from(p.positive));
        out.extend(pe.iter().zip(label).map(|(a, b)| a + b));
        rows += 1;
    }
    match bx {
        None => {
            // the padding point: its positional part is zeroed, only the "not a point" embedding remains
            out.extend_from_slice(&tables.not_a_point);
            rows += 1;
        }
        Some([ax, ay, bx2, by2]) => {
            let (x0, x1) = (ax.min(bx2), ax.max(bx2));
            let (y0, y1) = (ay.min(by2), ay.max(by2));
            for (x, y, row) in [(x0, y0, 2), (x1, y1, 3)] {
                let pe = tables.encode(x, y);
                out.extend(pe.iter().zip(tables.point_row(row)).map(|(a, b)| a + b));
                rows += 1;
            }
        }
    }
    Ok((out, rows))
}

/// The index of the best mask by predicted IoU (the first on ties), ignoring NaN; `None` when empty.
pub fn best_mask(iou: &[f32]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, v) in iou.iter().enumerate() {
        if v.is_nan() {
            continue;
        }
        if best.is_none_or(|(_, b)| *v > b) {
            best = Some((i, *v));
        }
    }
    best.map(|(i, _)| i)
}

/// The decoder's `LOW_RES × LOW_RES` logits (over the padded input) resampled to `side × side`
/// logits over the whole photo, which is stretched to the square like every stored Ember mask.
///
/// This is `postprocess_masks` in one bilinear step: the part of the input the photo covers
/// (`width × height` of `INPUT`) is cropped, and the output samples it at half-pixel centres, so the
/// padding never leaks into the mask. Thresholding the result at 0 gives the reference's mask.
pub fn resample_logits(low: &[f32], width: usize, height: usize, side: usize) -> Result<Vec<f32>> {
    if low.len() != LOW_RES * LOW_RES || width == 0 || height == 0 || width > INPUT || height > INPUT || side == 0 || side > 4096 {
        return Err(Error::Model(format!("cannot resample {} logits for a {width} × {height} photo to {side}²", low.len())));
    }
    let ratio = INPUT as f32 / LOW_RES as f32;
    let at = |x: usize, y: usize| low.get(y.min(LOW_RES - 1) * LOW_RES + x.min(LOW_RES - 1)).copied().unwrap_or(0.0);
    // sample position (in low-res pixels, half-pixel centres) of output index `o` of `side`, over `extent` input pixels
    let position = |o: usize, extent: usize| (((o as f32 + 0.5) / side as f32 * extent as f32) / ratio - 0.5).max(0.0);
    let mut out = vec![0f32; side * side];
    for oy in 0..side {
        let fy = position(oy, height);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        for ox in 0..side {
            let fx = position(ox, width);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
            let bottom = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
            if let Some(o) = out.get_mut(oy * side + ox) {
                *o = top * (1.0 - ty) + bottom * ty;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tables whose values are easy to follow: the Gaussian matrix selects `x` for feature 0 and `y` for
    /// feature 1 (with unit frequency), the label embeddings are constants 10, 20, 30, 40 and the
    /// padding point's is 50.
    fn tables() -> PromptTables {
        let mut gaussian = vec![0f32; 2 * GAUSSIAN_FEATURES];
        gaussian[0] = 1.0; // x -> feature 0
        gaussian[GAUSSIAN_FEATURES + 1] = 1.0; // y -> feature 1
        let mut points = Vec::new();
        for k in [10.0, 20.0, 30.0, 40.0] {
            points.extend(std::iter::repeat_n(k, EMBED_DIM));
        }
        PromptTables { gaussian, points, not_a_point: vec![50.0; EMBED_DIM] }
    }

    #[test]
    fn long_edge_resizes_to_1024_rounding_half_up() {
        // reference: scale = 1024 / max(h, w); side = int(side * scale + 0.5)
        assert_eq!(resized_shape(1500, 1000).unwrap(), (1024, 683));
        assert_eq!(resized_shape(640, 480).unwrap(), (1024, 768));
        assert_eq!(resized_shape(480, 640).unwrap(), (768, 1024));
        assert_eq!(resized_shape(1024, 1024).unwrap(), (1024, 1024));
        assert_eq!(resized_shape(6000, 4000).unwrap(), (1024, 683));
        assert_eq!(resized_shape(3, 1000).unwrap(), (3, 1024));
        // never a zero side, whatever the aspect ratio
        assert_eq!(resized_shape(60000, 1).unwrap(), (1024, 1));
        assert!(resized_shape(0, 5).is_err() && resized_shape(5, 0).is_err() && resized_shape(1 << 20, 4).is_err());
    }

    #[test]
    fn prepare_normalizes_with_the_reference_mean_and_std_and_pads_with_zeros() {
        // a white 4 × 2 photo: scaled to 1024 × 512
        let rgb = vec![255u8; 4 * 2 * 3];
        let p = prepare(&rgb, 4, 2).unwrap();
        assert_eq!((p.width, p.height), (1024, 512));
        assert_eq!(p.pixels.len(), 3 * INPUT * INPUT);
        let at = |c: usize, y: usize, x: usize| p.pixels[(c * INPUT + y) * INPUT + x];
        // (255 - mean) / std per channel
        assert!((at(0, 0, 0) - 2.248_908).abs() < 1e-4, "{}", at(0, 0, 0));
        assert!((at(1, 0, 0) - 2.428_571).abs() < 1e-4, "{}", at(1, 0, 0));
        assert!((at(2, 0, 0) - 2.640_000).abs() < 1e-3, "{}", at(2, 0, 0));
        // the last row and column of the photo
        assert!((at(0, 511, 1023) - 2.248_908).abs() < 1e-4);
        // the padding, below the photo: zeros (padded after normalizing)
        assert_eq!(at(0, 512, 0), 0.0);
        assert_eq!(at(2, 1023, 1023), 0.0);
        // a black photo is -mean / std
        let p = prepare(&[0u8; 2 * 2 * 3], 2, 2).unwrap();
        assert!((p.pixels[0] + 2.117_9).abs() < 1e-3, "{}", p.pixels[0]);
        assert!(prepare(&[0; 5], 2, 2).is_err());
    }

    #[test]
    fn prepare_keeps_the_orientation_of_a_gradient() {
        // left half black, right half white: the resized photo keeps it (no transposition or flip)
        let mut rgb = vec![0u8; 8 * 4 * 3];
        for y in 0..4 {
            for x in 4..8 {
                for c in 0..3 {
                    rgb[(y * 8 + x) * 3 + c] = 255;
                }
            }
        }
        let p = prepare(&rgb, 8, 4).unwrap();
        assert_eq!((p.width, p.height), (1024, 512));
        assert!(p.pixels[10] < -2.0, "left is dark");
        assert!(p.pixels[1013] > 2.0, "right is bright");
        assert!(p.pixels[300 * INPUT + 10] < -2.0, "and the same lower down");
    }

    #[test]
    fn a_point_embedding_is_its_fourier_features_plus_its_label_embedding() {
        let t = tables();
        // pixel (511.5, 511.5) is the centre of the input: normalized coordinate 0 -> sin 0 = 0, cos 0 = 1
        let (rows, n) = sparse_embeddings(&t, &[PromptPoint { x: 511.5, y: 511.5, positive: true }], None).unwrap();
        assert_eq!(n, 2, "the point and the padding point");
        assert_eq!(rows.len(), 2 * EMBED_DIM);
        assert!((rows[0] - 20.0).abs() < 1e-5, "sin(0) + object label (20): {}", rows[0]);
        assert!((rows[GAUSSIAN_FEATURES] - 21.0).abs() < 1e-5, "cos(0) + 20: {}", rows[GAUSSIAN_FEATURES]);
        // the padding point is exactly the "not a point" embedding
        assert!(rows[EMBED_DIM..].iter().all(|v| (*v - 50.0).abs() < 1e-6));
        // a background point takes label 0 (10)
        let (rows, _) = sparse_embeddings(&t, &[PromptPoint { x: 511.5, y: 511.5, positive: false }], None).unwrap();
        assert!((rows[0] - 10.0).abs() < 1e-5);
        // x = 767.5 -> normalized 0.5 -> angle 0.5 * 2π = π -> sin ~ 0, cos = -1 (feature 0 uses x)
        let (rows, _) = sparse_embeddings(&t, &[PromptPoint { x: 767.5, y: 511.5, positive: true }], None).unwrap();
        assert!((rows[GAUSSIAN_FEATURES] - 19.0).abs() < 1e-4, "cos(π) + 20: {}", rows[GAUSSIAN_FEATURES]);
        // y is still the centre: feature 1 is sin 0 and cos 0
        assert!((rows[1] - 20.0).abs() < 1e-4 && (rows[GAUSSIAN_FEATURES + 1] - 21.0).abs() < 1e-4);
        // y moved to 767.5: feature 1's cos becomes -1
        let (rows, _) = sparse_embeddings(&t, &[PromptPoint { x: 511.5, y: 767.5, positive: true }], None).unwrap();
        assert!((rows[GAUSSIAN_FEATURES + 1] - 19.0).abs() < 1e-4, "{}", rows[GAUSSIAN_FEATURES + 1]);
    }

    #[test]
    fn a_box_is_two_corner_rows_without_a_padding_point() {
        let t = tables();
        let (rows, n) = sparse_embeddings(&t, &[], Some([511.5, 511.5, 767.5, 767.5])).unwrap();
        assert_eq!((n, rows.len()), (2, 2 * EMBED_DIM));
        // top-left: centre of the input + label 2 (30); bottom-right: π on both axes + label 3 (40)
        assert!((rows[0] - 30.0).abs() < 1e-5 && (rows[GAUSSIAN_FEATURES] - 31.0).abs() < 1e-5);
        assert!((rows[EMBED_DIM + GAUSSIAN_FEATURES] - 39.0).abs() < 1e-4);
        // corners given the other way round are sorted
        let (swapped, _) = sparse_embeddings(&t, &[], Some([767.5, 767.5, 511.5, 511.5])).unwrap();
        assert_eq!(rows, swapped);
        // points and a box: the points come first, no padding row
        let (rows, n) = sparse_embeddings(&t, &[PromptPoint { x: 511.5, y: 511.5, positive: true }], Some([0.0, 0.0, 100.0, 100.0])).unwrap();
        assert_eq!((n, rows.len()), (3, 3 * EMBED_DIM));
        assert!((rows[0] - 20.0).abs() < 1e-5);
    }

    #[test]
    fn hostile_prompts_are_errors() {
        let t = tables();
        assert!(sparse_embeddings(&t, &[], None).is_err());
        assert!(sparse_embeddings(&t, &[PromptPoint { x: f32::NAN, y: 0.0, positive: true }], None).is_err());
        assert!(sparse_embeddings(&t, &[], Some([0.0, 0.0, f32::INFINITY, 1.0])).is_err());
        assert!(sparse_embeddings(&PromptTables::default(), &[PromptPoint { x: 1.0, y: 1.0, positive: true }], None).is_err());
    }

    #[test]
    fn the_best_mask_is_the_highest_iou() {
        assert_eq!(best_mask(&[0.2, 0.9, 0.5]), Some(1));
        assert_eq!(best_mask(&[0.7, 0.7]), Some(0), "the first wins a tie");
        assert_eq!(best_mask(&[f32::NAN, 0.1]), Some(1));
        assert_eq!(best_mask(&[]), None);
        assert_eq!(best_mask(&[f32::NAN]), None);
    }

    #[test]
    fn logits_resample_to_the_photo_and_never_leak_the_padding() {
        // a 3:2 photo (1024 × 683 in the input): the decoder says "object" (+8) over the left half of the
        // photo's area, and also (+8) all over the padding below it, which must not reach the output
        let (w, h) = (1024usize, 683usize);
        let mut low = vec![-8f32; LOW_RES * LOW_RES];
        let valid_rows = h.div_ceil(4); // 171
        for y in 0..LOW_RES {
            for x in 0..LOW_RES {
                let inside = y < valid_rows;
                low[y * LOW_RES + x] = if !inside || x < LOW_RES / 2 { 8.0 } else { -8.0 };
            }
        }
        let out = resample_logits(&low, w, h, 64).unwrap();
        assert_eq!(out.len(), 64 * 64);
        let selected = |x: usize, y: usize| out[y * 64 + x] > 0.0;
        // left half selected, right half not, top and bottom rows alike
        assert!(selected(5, 0) && selected(5, 32) && selected(5, 63));
        assert!(!selected(58, 0) && !selected(58, 32));
        // the bottom-right corner is photo, not padding: the padding's +8 only blends in within a low-res pixel
        assert!(!selected(58, 63), "{}", out[63 * 64 + 58]);
        // the boundary is at the middle of the photo
        let row: Vec<bool> = (0..64).map(|x| selected(x, 20)).collect();
        let edge = row.iter().position(|s| !s).unwrap();
        assert!((31..=33).contains(&edge), "edge at {edge}");
    }

    #[test]
    fn resampling_a_constant_is_that_constant_and_bad_input_is_an_error() {
        let low = vec![3.5f32; LOW_RES * LOW_RES];
        assert!(resample_logits(&low, 1024, 1024, 32).unwrap().iter().all(|v| (*v - 3.5).abs() < 1e-5));
        assert!(resample_logits(&low, 300, 1024, 9).unwrap().iter().all(|v| (*v - 3.5).abs() < 1e-5));
        assert!(resample_logits(&low[1..], 1024, 1024, 32).is_err());
        assert!(resample_logits(&low, 0, 1024, 32).is_err());
        assert!(resample_logits(&low, 2000, 1024, 32).is_err());
        assert!(resample_logits(&low, 1024, 1024, 0).is_err());
    }

    #[test]
    fn a_square_photo_maps_low_res_pixels_one_to_one_at_native_side() {
        // at side = LOW_RES over the whole input the resampling is the identity (half-pixel centres align)
        let low: Vec<f32> = (0..LOW_RES * LOW_RES).map(|i| (i % 97) as f32 - 40.0).collect();
        let out = resample_logits(&low, INPUT, INPUT, LOW_RES).unwrap();
        assert!(out.iter().zip(&low).all(|(a, b)| (a - b).abs() < 1e-3));
    }
}
