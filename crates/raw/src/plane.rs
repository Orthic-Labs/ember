//! Bounded unsigned grayscale TIFF extraction without RAW development or rescaling.
//! A plane carries codec-reconstructed values; it does not establish mask meaning or registration.

use lightcraft_tiff::{Tiff, image::Layout, tags};

use crate::{RawData, RawError, Result, tiffraw};

pub const MAX_PLANE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 8 * 1024 * 1024;
const MAX_EDGE: u32 = 8192;
const MAX_CHUNKS: usize = 256;

/// Actual JPEG XL header metadata, independent of TIFF's nominal layout.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JxlCoding {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    pub frame_encoding: String,
    pub xyb_encoded: bool,
    pub colour_encoding: String,
    pub intensity_target: f32,
}

/// One chunk's nominal coordinates plus actual coding metadata.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PlaneChunk {
    pub index: usize,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub compression: u16,
    pub jxl: Option<JxlCoding>,
}

/// Row-major, codec-reconstructed unsigned samples in the nominal code range.
/// Lossy encoding does not preserve original pre-encoding integers.
#[derive(Debug, Clone, PartialEq)]
pub struct GrayPlane {
    pub width: u32,
    pub height: u32,
    pub bits: u16,
    pub samples: Vec<u16>,
    pub chunks: Vec<PlaneChunk>,
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
    if info.offsets.len() != count || info.byte_counts.len() != count || info.byte_counts.contains(&0) {
        return Err(RawError::Corrupt("plane chunk coverage is incomplete or ambiguous".into()));
    }
    let mut coding = Vec::with_capacity(count);
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
        let jxl = if info.compression == tags::compression::JPEG_XL {
            #[cfg(feature = "jxl")]
            {
                let start = usize::try_from(chunk.offset).map_err(|_| RawError::Limit("plane chunk offset"))?;
                let end = usize::try_from(end).map_err(|_| RawError::Limit("plane chunk end"))?;
                let source = bytes.get(start..end).ok_or_else(|| RawError::Corrupt("plane chunk bytes".into()))?;
                Some(crate::jxl::coding(
                    source,
                    chunk.width as usize,
                    chunk.height as usize,
                    info.width.saturating_sub(chunk.x).min(chunk.width) as usize,
                    info.height.saturating_sub(chunk.y).min(chunk.height) as usize,
                    u32::from(bits),
                )?)
            }
            #[cfg(not(feature = "jxl"))]
            {
                return Err(RawError::Unsupported("JPEG XL support disabled".into()));
            }
        } else {
            None
        };
        coding.push(PlaneChunk {
            index: chunk.index,
            x: chunk.x,
            y: chunk.y,
            width: chunk.width,
            height: chunk.height,
            compression: info.compression,
            jxl,
        });
    }
    let RawData::U16(samples) = tiffraw::read_image(bytes, &info, tiff.order, tiffraw::Packing::Msb)? else {
        return Err(RawError::Corrupt("grayscale decoder returned floating samples".into()));
    };
    if samples.len() as u64 != pixels || (bits == 8 && samples.iter().any(|&s| s > 255)) {
        return Err(RawError::Corrupt("grayscale decoder sample range or count mismatch".into()));
    }
    Ok(GrayPlane { width: info.width, height: info.height, bits, samples, chunks: coding })
}

// Original 8x8 horizontal ramp [0,32,64,96,128,160,192,255], repeated eight rows.
// Encoded with libjxl0.12.0 cjxl -d1 -m0 -e3 --num_threads=0 -x color_space=Gra_D65_Rel_Lin.
// Embedded79-byte synthetic fixture; no real photo, catalog or third-party imagery.
#[cfg(all(test, feature = "jxl"))]
pub(crate) const LOSSY_GRAY_8X8: &[u8] = &[
    255, 10, 65, 64, 82, 180, 8, 0, 19, 8, 1, 0, 4, 1, 103, 19, 40, 1, 0, 80, 161, 50, 202, 184, 193, 203, 185, 158, 47, 63, 116, 88, 76, 219, 184,
    206, 208, 86, 219, 22, 150, 36, 138, 140, 99, 147, 116, 2, 135, 36, 177, 74, 3, 136, 82, 102, 174, 102, 176, 42, 73, 36, 71, 220, 141, 36, 128,
    131, 2, 26, 255, 46, 96, 147, 1, 164, 6, 0, 5,
];
// Independent libjxl0.12.0 djxl reconstruction, same linear-gray output,8bit,one thread.
#[cfg(all(test, feature = "jxl"))]
pub(crate) const LOSSY_GRAY_REFERENCE: &[u8] = &[
    0, 31, 65, 95, 128, 160, 193, 252, 0, 31, 64, 95, 127, 160, 192, 253, 0, 31, 65, 94, 128, 160, 193, 252, 0, 31, 64, 95, 128, 160, 192, 252, 0,
    31, 65, 95, 127, 160, 192, 253, 0, 31, 64, 95, 128, 160, 193, 253, 1, 31, 64, 95, 128, 160, 192, 252, 0, 31, 65, 95, 127, 160, 192, 253,
];

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

    #[cfg(feature = "jxl")]
    #[test]
    fn tiled_lossy_linear_gray_reconstructs_codes_and_retains_cropped_chunk_metadata() {
        let mut image = ifd(13, 8, 8);
        image.set(tags::COMPRESSION, Value::Short(vec![tags::compression::JPEG_XL]));
        image.set_image(ImageData::Tiles { tile_width: 8, tile_height: 8, tiles: vec![LOSSY_GRAY_8X8.to_vec(), LOSSY_GRAY_8X8.to_vec()] });
        let bytes = TiffWriter::new(ByteOrder::Little, false).write(&[image]).unwrap();
        let plane = gray_tiff(&bytes).unwrap();
        assert_eq!((plane.width, plane.height, plane.bits), (13, 8, 8));
        for (row, reference) in plane.samples.chunks_exact(13).zip(LOSSY_GRAY_REFERENCE.chunks_exact(8)) {
            let expected = reference.iter().chain(reference.iter().take(5));
            assert!(row.iter().zip(expected).all(|(a, b)| a.abs_diff(u16::from(*b)) <= 1));
        }
        assert_eq!(plane.chunks.len(), 2);
        assert_eq!(plane.chunks[1].x, 8);
        for chunk in &plane.chunks {
            let coding = chunk.jxl.as_ref().unwrap();
            assert_eq!((coding.width, coding.height, coding.bits), (8, 8, 8));
            assert_eq!(coding.frame_encoding, "vardct");
            assert!(coding.xyb_encoded);
        }
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
    fn refuses_zero_byte_counts_instead_of_inferring_pixel_extents() {
        let mut image = ifd(2, 2, 8);
        image.set_image(ImageData::Strips { rows_per_strip: 2, strips: vec![vec![17; 4]] });
        let mut bytes = TiffWriter::new(ByteOrder::Little, false).write(&[image]).unwrap();
        let offset = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let entries = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
        let mut replaced = false;
        for index in 0..entries {
            let entry = offset + 2 + index * 12;
            if u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap()) == tags::STRIP_BYTE_COUNTS {
                bytes[entry + 8..entry + 12].fill(0);
                replaced = true;
            }
        }
        assert!(replaced);
        assert!(matches!(gray_tiff(&bytes), Err(RawError::Corrupt(message)) if message.contains("ambiguous")));
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
