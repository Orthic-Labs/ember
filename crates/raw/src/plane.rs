//! Bounded unsigned grayscale TIFF extraction without RAW development or rescaling.
//! A plane carries stored code values; it does not establish mask meaning or image registration.

use lightcraft_tiff::{Tiff, image::Layout, tags};

use crate::{RawData, RawError, Result, tiffraw};

pub const MAX_PLANE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 8 * 1024 * 1024;
const MAX_EDGE: u32 = 8192;
const MAX_CHUNKS: usize = 256;

/// Row-major, unsigned integer samples in their original nominal code range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayPlane {
    pub width: u32,
    pub height: u32,
    pub bits: u16,
    pub samples: Vec<u16>,
}

/// Read one identity-oriented, 8/16-bit BlackIsZero TIFF image. Supports uncompressed
/// samples & JPEG XL through the existing Rust decoder. Missing tiles, truncated extents,
/// non-identity orientation & other sample/layout conventions return errors.
pub fn gray_tiff(bytes: &[u8]) -> Result<GrayPlane> {
    if bytes.len() > MAX_PLANE_BYTES {
        return Err(RawError::Limit("grayscale TIFF input exceeds 32 MiB"));
    }
    let tiff = Tiff::parse(bytes)?;
    if tiff.ifds.len() != 1 {
        return Err(RawError::Unsupported("grayscale TIFF requires one IFD".into()));
    }
    let ifd = tiff.ifds.first().ok_or_else(|| RawError::Corrupt("missing grayscale IFD".into()))?;
    if !ifd.sub_ifds.is_empty() || ifd.exif.is_some() || ifd.gps.is_some() || ifd.interop.is_some() {
        return Err(RawError::Unsupported("grayscale TIFF child IFDs".into()));
    }
    if ifd.u16(tags::ORIENTATION).unwrap_or(1) != 1 {
        return Err(RawError::Unsupported("grayscale TIFF orientation must be identity".into()));
    }
    let info = ifd.image()?;
    let bits = info.bits_per_sample.first().copied().ok_or_else(|| RawError::Corrupt("missing plane bit depth".into()))?;
    if info.bits_per_sample.len() != 1
        || !matches!(bits, 8 | 16)
        || info.samples_per_pixel != 1
        || info.sample_format != 1
        || info.photometric != 1
        || info.planar != 1
        || info.predictor != 1
    {
        return Err(RawError::Unsupported("grayscale TIFF requires unsigned 8/16-bit BlackIsZero samples without prediction".into()));
    }
    let pixels = u64::from(info.width).checked_mul(u64::from(info.height)).ok_or(RawError::Limit("plane dimensions"))?;
    if info.width > MAX_EDGE || info.height > MAX_EDGE || pixels == 0 || pixels > MAX_PIXELS {
        return Err(RawError::Limit("plane exceeds 8192 edge or 8 Mi pixels"));
    }
    if !matches!(info.compression, tags::compression::NONE | tags::compression::JPEG_XL) {
        return Err(RawError::Unsupported(format!("grayscale TIFF compression {}", info.compression)));
    }
    let (cw, ch) = match info.layout {
        Layout::Strips { rows_per_strip } => (info.width, rows_per_strip),
        Layout::Tiles { tile_width, tile_height } => (tile_width, tile_height),
    };
    if cw == 0 || ch == 0 || cw > MAX_EDGE || ch > MAX_EDGE {
        return Err(RawError::Limit("plane chunk dimensions"));
    }
    let chunks = u64::from(info.width.div_ceil(cw)).checked_mul(u64::from(info.height.div_ceil(ch))).ok_or(RawError::Limit("plane chunk count"))?;
    let count = usize::try_from(chunks).map_err(|_| RawError::Limit("plane chunk count"))?;
    let nominal_samples =
        u64::from(cw).checked_mul(u64::from(ch)).and_then(|n| n.checked_mul(chunks)).ok_or(RawError::Limit("plane chunk allocations"))?;
    if count == 0 || count > MAX_CHUNKS || nominal_samples > MAX_PIXELS * 2 {
        return Err(RawError::Limit("plane exceeds chunk allocation budget"));
    }
    if info.offsets.len() != count || info.byte_counts.len() != count {
        return Err(RawError::Corrupt("plane chunk coverage is incomplete or ambiguous".into()));
    }
    for chunk in info.chunks(bytes.len() as u64) {
        let end = chunk.offset.checked_add(chunk.len).ok_or(RawError::Limit("plane chunk extent"))?;
        if chunk.len == 0 || chunk.offset >= bytes.len() as u64 || end > bytes.len() as u64 {
            return Err(RawError::Corrupt("plane chunk extent is truncated".into()));
        }
        if info.compression == tags::compression::NONE {
            let required = u64::from(chunk.width)
                .checked_mul(u64::from(chunk.height))
                .and_then(|n| n.checked_mul(u64::from(bits / 8)))
                .ok_or(RawError::Limit("plane chunk bytes"))?;
            if chunk.len != required {
                return Err(RawError::Corrupt("uncompressed plane chunk length mismatch".into()));
            }
        }
    }
    let RawData::U16(samples) = tiffraw::read_image(bytes, &info, tiff.order, tiffraw::Packing::Msb)? else {
        return Err(RawError::Corrupt("grayscale decoder returned floating samples".into()));
    };
    if samples.len() as u64 != pixels || (bits == 8 && samples.iter().any(|&s| s > 255)) {
        return Err(RawError::Corrupt("grayscale decoder sample range or count mismatch".into()));
    }
    Ok(GrayPlane { width: info.width, height: info.height, bits, samples })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{ByteOrder, IfdBuilder, ImageData, TiffWriter, Value};

    fn ifd(width: u32, height: u32, bits: u16) -> IfdBuilder {
        IfdBuilder::new()
            .with(tags::IMAGE_WIDTH, Value::Long(vec![width]))
            .with(tags::IMAGE_LENGTH, Value::Long(vec![height]))
            .with(tags::BITS_PER_SAMPLE, Value::Short(vec![bits]))
            .with(tags::SAMPLES_PER_PIXEL, Value::Short(vec![1]))
            .with(tags::PHOTOMETRIC, Value::Short(vec![1]))
    }

    #[test]
    fn preserves_integer_codes_in_both_byte_orders() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let codes = [0u16, 1, 32768, 65535];
            let bytes = codes.iter().flat_map(|v| if order == ByteOrder::Little { v.to_le_bytes() } else { v.to_be_bytes() }).collect();
            let mut image = ifd(2, 2, 16);
            image.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![bytes] });
            let plane = gray_tiff(&TiffWriter::new(order, false).write(&[image]).unwrap()).unwrap();
            assert_eq!(plane.samples, codes);
            assert_eq!((plane.width, plane.height, plane.bits), (2, 2, 16));
        }
    }

    #[cfg(feature = "jxl")]
    #[test]
    fn tiled_lossless_jxl_retains_edge_codes_without_rescaling() {
        use zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions};
        let mut tiles = Vec::new();
        for values in [vec![0u8, 1, 42, 255, 128, 63], vec![17u8, 239, 7, 200]] {
            let width = values.len() / 2;
            let mut coded = Vec::new();
            zune_jpegxl::JxlSimpleEncoder::new(&values, EncoderOptions::new(width, 2, ColorSpace::Luma, BitDepth::Eight)).encode(&mut coded).unwrap();
            tiles.push(coded);
        }
        let mut image = ifd(5, 2, 8);
        image.set(tags::COMPRESSION, Value::Short(vec![tags::compression::JPEG_XL]));
        image.set_image(ImageData::Tiles { tile_width: 3, tile_height: 2, tiles });
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(std::slice::from_ref(&image)).unwrap();
        let plane = gray_tiff(&bytes).unwrap();
        assert_eq!(plane.samples, [0, 1, 42, 17, 239, 255, 128, 63, 7, 200]);
        assert_eq!(plane.bits, 8);
        let mut corrupt = image.clone();
        corrupt.set_image(ImageData::Tiles { tile_width: 3, tile_height: 2, tiles: vec![vec![0; 8], vec![0; 8]] });
        assert!(gray_tiff(&TiffWriter::new(ByteOrder::Little, false).write(&[corrupt]).unwrap()).is_err());
    }

    #[test]
    fn rejects_missing_tile_truncation_and_unbounded_nominal_tile() {
        let mut image = ifd(3, 2, 8);
        image.set_image(ImageData::Tiles { tile_width: 2, tile_height: 2, tiles: vec![vec![0; 4]] });
        assert!(gray_tiff(&TiffWriter::new(ByteOrder::Little, false).write(&[image]).unwrap()).is_err());
        let mut short = ifd(2, 2, 8);
        short.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![vec![0; 3]] });
        assert!(gray_tiff(&TiffWriter::new(ByteOrder::Little, false).write(&[short]).unwrap()).is_err());
        let mut huge = ifd(1, 1, 8);
        huge.set_image(ImageData::Tiles { tile_width: u32::MAX, tile_height: u32::MAX, tiles: vec![vec![0]] });
        assert!(gray_tiff(&TiffWriter::new(ByteOrder::Little, false).write(&[huge]).unwrap()).is_err());
    }

    #[test]
    fn refuses_orientation_and_white_is_zero_instead_of_changing_mask_coordinates() {
        for (tag, value) in [(tags::ORIENTATION, 6), (tags::PHOTOMETRIC, 0)] {
            let mut image = ifd(1, 1, 8);
            image.set(tag, Value::Short(vec![value]));
            image.set_image(ImageData::Strips { rows_per_strip: 1, strips: vec![vec![17]] });
            assert!(gray_tiff(&TiffWriter::new(ByteOrder::Little, false).write(&[image]).unwrap()).is_err());
        }
    }
}
