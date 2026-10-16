//! Opt-in denoise backend on ONNX Runtime (DirectML on Windows, CoreML on Apple), behind the cargo feature
//! `onnx-accel` (off by default; the desktop app does not enable it yet; issue #31).
//!
//! [`OnnxRunner`] is a [`TileRunner`] with exactly the interface of the CPU runner (`lightcraft_denoise::runtime`) and
//! the GPU runner (`lightcraft_gpu::nn`): one tile in as four planes `[R, G1, G2, B]` of `tile × tile` cells (NCHW,
//! batch 1, `f32`), one answer out as camera RGB, three planes of `2·tile × 2·tile` pixels (CHW, `f32`, the model's own
//! scale). It loads the same `.onnx` file Ember installs for its own runners. The engine treats it as a third
//! candidate under the same agreement check against the CPU (`crates/engine/src/denoise.rs`), so a wrong or silent
//! near-zero answer from an execution provider is never used.
//!
//! **ONNX Runtime is not bundled.** It is loaded at run time from the folder named by the environment variable
//! [`RUNTIME_DIR_ENV`] (`LIGHTCRAFT_ONNXRUNTIME_DIR`), which must hold `onnxruntime.dll` (Windows) or
//! `libonnxruntime.dylib` (macOS), and beside it `DirectML.dll` on Windows. Without it every load is an error that
//! starts with [`NOT_FOUND`]; nothing panics and nothing falls back to ONNX Runtime's own CPU provider, so the engine's
//! runners stay in charge.
//!
//! Everything sized by a model file or a caller is checked: tile sizes through [`tile_shape`] (checked arithmetic) and
//! tensor lengths and shapes before and after every run.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use lightcraft_denoise::run::Error;

/// Planes the model takes: the packed Bayer cells `[R, G1, G2, B]`.
pub const IN_CHANNELS: usize = 4;
/// Planes the model gives: camera RGB.
pub const OUT_CHANNELS: usize = 3;
/// Output pixels per input cell along each side (the model ends in a 2× DepthToSpace).
pub const SCALE: usize = 2;
/// Smallest and largest tile side, as `lightcraft_denoise::run::denoise_bayer` accepts them.
pub const MIN_TILE: usize = 16;
pub const MAX_TILE: usize = 2048;

/// Environment variable naming the folder that holds the ONNX Runtime library.
pub const RUNTIME_DIR_ENV: &str = "LIGHTCRAFT_ONNXRUNTIME_DIR";
/// Start of the error when no ONNX Runtime library can be found.
pub const NOT_FOUND: &str = "ONNX Runtime not found";

/// Sizes of one tile's tensors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileShape {
    pub tile: usize,
    /// `IN_CHANNELS × tile × tile`.
    pub input_len: usize,
    /// `OUT_CHANNELS × (2·tile) × (2·tile)`.
    pub output_len: usize,
}

impl TileShape {
    /// The input tensor's dimensions, NCHW with batch 1.
    pub fn input_dims(&self) -> [usize; 4] {
        [1, IN_CHANNELS, self.tile, self.tile]
    }

    /// The output tensor's dimensions, NCHW with batch 1.
    pub fn output_dims(&self) -> [usize; 4] {
        let side = self.tile.saturating_mul(SCALE);
        [1, OUT_CHANNELS, side, side]
    }
}

/// Tensor sizes for `tile × tile` cells, or an error for a tile outside [`MIN_TILE`]..=[`MAX_TILE`] or one whose sizes
/// overflow.
pub fn tile_shape(tile: usize) -> Result<TileShape, Error> {
    if !(MIN_TILE..=MAX_TILE).contains(&tile) {
        return Err(Error::Input(format!("a tile of {tile} cells is outside {MIN_TILE} to {MAX_TILE}")));
    }
    let too_big = || Error::Input("the tile's tensors are too large".into());
    let cells = tile.checked_mul(tile).ok_or_else(too_big)?;
    let side = tile.checked_mul(SCALE).ok_or_else(too_big)?;
    let pixels = side.checked_mul(side).ok_or_else(too_big)?;
    Ok(TileShape {
        tile,
        input_len: cells.checked_mul(IN_CHANNELS).ok_or_else(too_big)?,
        output_len: pixels.checked_mul(OUT_CHANNELS).ok_or_else(too_big)?,
    })
}

/// Check a tile before it goes to the model: the right length and only finite samples.
pub fn check_input(shape: &TileShape, input: &[f32]) -> Result<(), Error> {
    if input.len() != shape.input_len || input.iter().any(|v| !v.is_finite()) {
        return Err(Error::Input("the tile has the wrong size or non-finite values".into()));
    }
    Ok(())
}

/// Check what the model gave: dimensions `[1, 3, 2·tile, 2·tile]` (when the runtime reports them) and exactly that many
/// samples.
pub fn check_output(shape: &TileShape, dims: &[i64], len: usize) -> Result<(), Error> {
    let want = shape.output_dims();
    let dims_ok = dims.len() == want.len() && dims.iter().zip(want).all(|(&d, w)| usize::try_from(d).ok() == Some(w));
    if !dims_ok || len != shape.output_len {
        return Err(Error::Model(format!("{len} numbers in shape {dims:?}, not {} in shape {want:?}", shape.output_len)));
    }
    Ok(())
}

#[cfg(all(feature = "onnx-accel", not(target_arch = "wasm32")))]
mod runner;
#[cfg(all(feature = "onnx-accel", not(target_arch = "wasm32")))]
pub use runner::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_shapes_are_checked() {
        let s = tile_shape(512).unwrap();
        assert_eq!((s.input_len, s.output_len), (4 * 512 * 512, 3 * 1024 * 1024));
        assert_eq!(s.input_dims(), [1, 4, 512, 512]);
        assert_eq!(s.output_dims(), [1, 3, 1024, 1024]);
        assert!(tile_shape(0).is_err());
        assert!(tile_shape(MIN_TILE - 1).is_err());
        assert!(tile_shape(MAX_TILE + 1).is_err());
        assert!(tile_shape(usize::MAX).is_err());
        assert!(tile_shape(MAX_TILE).is_ok());
    }

    #[test]
    fn input_and_output_lengths_are_enforced() {
        let s = tile_shape(16).unwrap();
        assert!(check_input(&s, &vec![0.5; s.input_len]).is_ok());
        assert!(check_input(&s, &vec![0.5; s.input_len - 1]).is_err());
        let mut bad = vec![0.5; s.input_len];
        bad[3] = f32::NAN;
        assert!(check_input(&s, &bad).is_err());
        bad[3] = f32::INFINITY;
        assert!(check_input(&s, &bad).is_err());

        assert!(check_output(&s, &[1, 3, 32, 32], s.output_len).is_ok());
        assert!(check_output(&s, &[1, 3, 32, 32], s.output_len + 1).is_err());
        assert!(check_output(&s, &[1, 3, 32], s.output_len).is_err());
        assert!(check_output(&s, &[1, 3, 16, 16], s.output_len).is_err());
        assert!(check_output(&s, &[1, 3, -32, 32], s.output_len).is_err());
    }
}
