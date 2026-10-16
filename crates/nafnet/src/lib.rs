//! NAFNet on candle (issue #200): the model graph, the safetensors plus `contract.json` file pair that
//! `denoise-train` writes, the noise-map construction, and whole-image and tiled inference.
//!
//! Both the product denoiser (`lightcraft-denoise`, feature `nafnet`) and the training tool depend on this one
//! definition, so the network that is trained is, by construction, the network that runs.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

// Re-exported so a dependent builds weights for its own tests against the exact candle version this crate uses.
pub use candle_core;
pub use candle_nn;

pub mod contract;
pub mod error;
pub mod model;

pub use contract::{CONTRACT_FILE, Contract, NafModel, WEIGHTS_FILE};
pub use error::{Error, Result};
pub use model::{IN_CHANNELS, NafConfig, NafNet, OUT_CHANNELS, SIGMA_SCALE, model_input_chw, zero_init_ending};
