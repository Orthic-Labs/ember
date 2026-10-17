//! Depth images: the PNG / JPEG a depth map is embedded as, and the HEIF auxiliary depth image of an iPhone
//! portrait photo, decoded to their sample codes (no colour management: the values are codes, not colours).
//!
//! Turning the codes into distances is the caller's: it depends on how the file describes them.

use crate::convert::check_size;
use crate::{DecodeOptions, Error, Format, Result, sniff};

/// The largest depth image decoded (pixels). Depth maps are small; a bigger one is not one.
const MAX_PIXELS: u64 = 1 << 26;

/// One channel of a decoded depth image, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepthCodes {
    pub width: usize,
    pub height: usize,
    /// The largest code the file's sample size can hold (255 or 65535; a HEIF's is `2^bits − 1`).
    pub max: u32,
    pub data: Vec<u16>,
}

fn options() -> DecodeOptions {
    DecodeOptions { max_pixels: MAX_PIXELS, ..Default::default() }
}

/// Decode a PNG or JPEG depth image to its first channel's codes (8- or 16-bit grey, or the first channel of a
/// colour image).
pub fn decode_codes(bytes: &[u8]) -> Result<DepthCodes> {
    let format = sniff(bytes).ok_or(Error::UnknownFormat)?;
    std::panic::catch_unwind(|| match format {
        Format::Png => png(bytes),
        Format::Jpeg => jpeg(bytes),
        other => Err(Error::Unsupported(other, "a depth map is a PNG or JPEG image")),
    })
    .unwrap_or_else(|_| Err(Error::Malformed(format, "decoder panicked".into())))
}

fn png(bytes: &[u8]) -> Result<DepthCodes> {
    let f = Format::Png;
    let err = |e: &dyn std::fmt::Display| Error::Malformed(f, e.to_string());
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    dec.set_limits(png::Limits { bytes: MAX_PIXELS.saturating_mul(8).min(usize::MAX as u64) as usize });
    let mut reader = dec.read_info().map_err(|e| err(&e))?;
    let (w, h) = {
        let i = reader.info();
        (i.width, i.height)
    };
    check_size(f, u64::from(w), u64::from(h), &options())?;
    let size = reader.output_buffer_size().ok_or_else(|| err(&"buffer size overflow"))?;
    let mut buf = vec![0u8; size];
    let out = reader.next_frame(&mut buf).map_err(|e| err(&e))?;
    buf.truncate(out.buffer_size());
    let channels = match out.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(err(&"palette not expanded")),
    };
    let (data, max): (Vec<u16>, u32) = match out.bit_depth {
        png::BitDepth::Sixteen => {
            let be = buf.as_chunks::<2>().0.iter().step_by(channels).copied();
            (be.map(u16::from_be_bytes).collect(), 65535)
        }
        _ => (buf.iter().step_by(channels).map(|&v| u16::from(v)).collect(), 255),
    };
    let (width, height) = (out.width as usize, out.height as usize);
    if data.len() != width.saturating_mul(height) {
        return Err(err(&"the image data is incomplete"));
    }
    Ok(DepthCodes { width, height, max, data })
}

fn jpeg(bytes: &[u8]) -> Result<DepthCodes> {
    let m = crate::jpeg::parse_markers(bytes).ok_or_else(|| Error::Malformed(Format::Jpeg, "missing SOI".into()))?;
    check_size(Format::Jpeg, u64::from(m.width), u64::from(m.height), &options())?;
    let (width, height, channels, codes) = crate::jpeg::decode_codes8(bytes)?;
    let data: Vec<u16> = codes.iter().step_by(channels.max(1)).map(|&v| u16::from(v)).collect();
    if data.len() != width.saturating_mul(height) {
        return Err(Error::Malformed(Format::Jpeg, "the image data is incomplete".into()));
    }
    Ok(DepthCodes { width, height, max: 255, data })
}

/// The depth map of a HEIF photo (the primary picture's `auxl` image of a depth type), upright like the picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeifDepth {
    pub codes: DepthCodes,
    /// The codes are disparity (a larger code is nearer), not depth.
    pub disparity: bool,
}

/// The depth map of a HEIC photo: `Ok(None)` when it has none, or when this build has no HEIF decoder (the
/// `heif` feature).
#[cfg(feature = "heif")]
pub fn heif_depth(bytes: &[u8]) -> Result<Option<HeifDepth>> {
    let f = Format::Heif;
    let d = lightcraft_heif::decode_depth(bytes, MAX_PIXELS).map_err(|e| match e {
        lightcraft_heif::Error::Unsupported(why) => Error::Unsupported(f, why),
        lightcraft_heif::Error::Limit(m) | lightcraft_heif::Error::Malformed(m) => Error::Malformed(f, m),
    })?;
    Ok(d.map(|d| HeifDepth {
        codes: DepthCodes {
            width: d.width as usize,
            height: d.height as usize,
            max: (1u32 << u32::from(d.bit_depth).clamp(1, 16)) - 1,
            data: d.data,
        },
        disparity: d.disparity,
    }))
}

#[cfg(not(feature = "heif"))]
pub fn heif_depth(_bytes: &[u8]) -> Result<Option<HeifDepth>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_png(w: u32, h: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn grey_pngs_decode_to_their_codes() {
        let eight = encode_png(3, 2, png::ColorType::Grayscale, png::BitDepth::Eight, &[0, 10, 20, 30, 40, 255]);
        assert_eq!(decode_codes(&eight).unwrap(), DepthCodes { width: 3, height: 2, max: 255, data: vec![0, 10, 20, 30, 40, 255] });
        let sixteen = encode_png(2, 1, png::ColorType::Grayscale, png::BitDepth::Sixteen, &[0x01, 0x02, 0xff, 0xff]);
        assert_eq!(decode_codes(&sixteen).unwrap(), DepthCodes { width: 2, height: 1, max: 65535, data: vec![0x0102, 0xffff] });
    }

    #[test]
    fn colour_and_alpha_pngs_give_their_first_channel() {
        let rgb = encode_png(2, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[7, 1, 1, 9, 2, 2]);
        assert_eq!(decode_codes(&rgb).unwrap().data, vec![7, 9]);
        let ga = encode_png(2, 1, png::ColorType::GrayscaleAlpha, png::BitDepth::Eight, &[5, 255, 6, 255]);
        assert_eq!(decode_codes(&ga).unwrap().data, vec![5, 6]);
    }

    #[test]
    fn damaged_images_are_errors() {
        let good = encode_png(8, 8, png::ColorType::Grayscale, png::BitDepth::Eight, &[3; 64]);
        for cut in [0, 4, 12, 30] {
            assert!(decode_codes(&good[..cut]).is_err(), "cut at {cut}");
        }
        assert!(decode_codes(b"GIF89a....").is_err());
        assert!(decode_codes(&[0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0]).is_err());
        for cut in 0..good.len() {
            let _ = decode_codes(&good[..cut]);
        }
    }

    #[test]
    fn jpeg_depth_maps_decode_as_codes() {
        // an 8-bit grey JPEG, as a Pixel stores its depth map
        let pixels: Vec<u8> = (0..16 * 16).map(|i| (i % 16 * 16) as u8).collect();
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 95).encode(&pixels, 16, 16, jpeg_encoder::ColorType::Luma).unwrap();
        let d = decode_codes(&jpeg).unwrap();
        assert_eq!((d.width, d.height, d.max, d.data.len()), (16, 16, 255, 256));
        assert!(d.data[0] < 24 && d.data[15] > 220, "{:?}", &d.data[..16]);
    }

    #[test]
    fn heif_depth_follows_the_feature() {
        // not a HEIF file: no depth in a build without the decoder, an error in one with
        let r = heif_depth(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic");
        assert_eq!(r.is_err(), cfg!(feature = "heif"), "{r:?}");
    }
}
