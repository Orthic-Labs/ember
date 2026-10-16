//! The NAFNet model lives in the shared `lightcraft-nafnet` crate, so the network that is trained here is the one
//! the product runs (issue #200). This module re-exports it and adds the one training-side convenience: building
//! the network input from this crate's [`NoiseParams`].

use crate::noise::NoiseParams;

pub use lightcraft_nafnet::{IN_CHANNELS, NafConfig, NafNet, OUT_CHANNELS, SIGMA_SCALE, zero_init_ending};

/// The network input for one noisy linear RGB image (`3·n` samples, CHW, `n` pixels): the RGB itself and, per
/// channel, the expected noise sigma `sqrt(a·max(y, 0) + b)` scaled by [`SIGMA_SCALE`]. See
/// [`lightcraft_nafnet::model_input_chw`].
pub fn model_input_chw(noisy: &[f32], p: NoiseParams) -> Vec<f32> {
    lightcraft_nafnet::model_input_chw(noisy, p.a, p.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noise_level_channels_follow_the_model() {
        let p = NoiseParams { a: 0.01, b: 0.0004 };
        let inp = model_input_chw(&[0.0, 0.25, -0.1, f32::NAN], p);
        assert_eq!(inp.len(), 8);
        assert!((inp[4] - p.sigma_at(0.0) * SIGMA_SCALE).abs() < 1e-6);
        assert!((inp[5] - (0.01f32 * 0.25 + 0.0004).sqrt() * SIGMA_SCALE).abs() < 1e-6);
        assert_eq!(inp[6], inp[4], "negative samples read as black");
        assert_eq!(inp[7], inp[4], "NaN reads as black");
    }
}
