//! Ember denoiser training tooling (issue #200): CC0 pair generation, a NAFNet-width16-class model on candle,
//! the training loop and the qualification eval.
//!
//! This is tooling, not product code: nothing in the product depends on it (`xtask layers` registers it as an
//! exempt tool crate), it is pure Rust, and its outputs (weights, reports) go under the scratch root and are never
//! committed. Run it through the `denoise-train` binary; see `docs/denoise-qualification.md`.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod data;
pub mod error;
pub mod eval;
pub mod metrics;
pub mod model;
pub mod noise;
pub mod rng;
pub mod split;
pub mod train;

pub use error::{Error, Result};
