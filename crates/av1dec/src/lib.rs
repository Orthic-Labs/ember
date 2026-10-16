//! Safe AV1 still-picture decoding over [`rav1d`](https://crates.io/crates/rav1d), the Rust port of dav1d.
//!
//! [`decode`] takes one AV1 temporal unit's OBUs (an AVIF image item's data, or a bare `.obu` still),
//! decodes the first picture and returns it as owned, tightly packed 16-bit sample planes with the
//! stream's own colour description. The caller does the YCbCr to RGB conversion.
//!
//! rav1d offers no safe Rust API (its Rust-level functions are crate-private), so the `native` module
//! calls the C-ABI `dav1d_*` entry points. All `unsafe` is there, every block carries a `SAFETY`
//! comment, and the API here is safe: the context and the picture are owned by drop guards that
//! close and unref them, planes are copied out into owned `Vec`s before the picture is released,
//! and every dimension, stride and size is validated with checked arithmetic first. Decoding is
//! single-threaded, pictures over [`MAX_PIXELS`] are refused (the decoder enforces the same limit
//! through its frame-size setting), and malformed input is an [`Error`], never a panic.
//!
//! On wasm32 rav1d is not built and [`decode`] returns [`Error::Unavailable`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(not(target_arch = "wasm32"))]
mod native;

/// Largest picture accepted, in pixels (2^28, i.e. 16384 x 16384).
pub const MAX_PIXELS: u64 = 1 << 28;
/// Largest width or height accepted.
pub const MAX_DIMENSION: u32 = 65_536;

/// Decoder failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("AV1 decoding is not available in this build")]
    Unavailable,
    #[error("no AV1 data")]
    Empty,
    #[error("malformed AV1 data: {0}")]
    Malformed(&'static str),
    #[error("unsupported AV1 stream: {0}")]
    Unsupported(&'static str),
    #[error("AV1 picture too large: {0}x{1}")]
    TooLarge(u64, u64),
    #[error("AV1 decoder error {0}")]
    Decoder(i32),
}

/// Chroma layout of a decoded picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Luma only.
    I400,
    /// Chroma halved in both directions.
    I420,
    /// Chroma halved horizontally.
    I422,
    /// Full-resolution chroma.
    I444,
}

impl Layout {
    /// Horizontal and vertical chroma subsampling shifts (0 or 1); `None` for luma only.
    pub fn shifts(self) -> Option<(u32, u32)> {
        match self {
            Layout::I400 => None,
            Layout::I420 => Some((1, 1)),
            Layout::I422 => Some((1, 0)),
            Layout::I444 => Some((0, 0)),
        }
    }
}

/// One sample plane, tightly packed (`data.len() == width * height`).
#[derive(Clone, Debug)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u16>,
}

/// The colour description in the AV1 sequence header (H.273 code points, 2 = unspecified).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colour {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    /// Samples use the full code range (otherwise studio / limited range).
    pub full_range: bool,
    /// The stream carried an explicit colour description (otherwise the code points are defaults).
    pub described: bool,
}

/// A decoded picture: luma first, then Cb and Cr unless [`Layout::I400`].
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// 8, 10 or 12.
    pub bit_depth: u8,
    pub layout: Layout,
    pub planes: Vec<Plane>,
    pub colour: Option<Colour>,
}

/// Decode the first picture of an AV1 stream given as OBUs.
pub fn decode(data: &[u8]) -> Result<Picture, Error> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::decode(data)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = data;
        Err(Error::Unavailable)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
