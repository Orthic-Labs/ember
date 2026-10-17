//! `denoise-train export-mosaics --in DIR --out DIR`: decode every raw under `DIR` with Ember's own decoder
//! (`lightcraft-raw`, the decode the product uses) and write what the Python trainer (`tools/train-denoise`) needs,
//! so the trainer sees exactly the product's black subtraction, white scaling, active area and opcode lists and no
//! third-party raw decoder is involved (decision ember-4a).
//!
//! Per raw file, named by the SHA-256 of the file's bytes:
//! * `<sha256>.npy`: the CFA mosaic as a NumPy `<u2` array `[height, width]`, `round(value * 65535)` of the
//!   black-subtracted, white-normalised sample clipped to [0, 1], cropped (at most one pixel on the top and left, and
//!   to even dimensions) so the pattern is **RGGB** at (0, 0);
//! * `<sha256>.json`: the [`Sidecar`] (CFA pattern before and after the crop, black and white levels, white balance,
//!   ISO, make and model). The sidecar is written last, so a sidecar means a complete `.npy`.
//!
//! Files that are not a single-plane 2x2 Bayer mosaic (X-Trans, linear DNG, undecodable files) are skipped and listed
//! in `skipped.json`; byte-identical duplicates are written once. Nothing here panics on a hostile file: every size is
//! bounded and every failure is an `Err` naming the file.

use crate::error::{Error, Result, data};
use crate::split::{hash_file, hex, scan_raws};
use lightcraft_raw::Normalized;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

pub const SIDECAR_VERSION: u32 = 1;
/// Largest side accepted (guards allocations from a hostile file); the same bound as the window loader.
pub const MAX_DIM: usize = 20_000;
/// Samples are stored as `round(value * SCALE)`.
pub const SCALE: f32 = 65535.0;
/// The raw file itself is read whole; cap it so a hostile path cannot exhaust memory.
const MAX_RAW_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const NPY_MAGIC: &[u8; 6] = b"\x93NUMPY";

/// What the Python trainer reads next to each mosaic (`tools/train-denoise/ember_denoise/prepare.py`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub format_version: u32,
    /// SHA-256 of the raw file's bytes (hex); the content hash the train/val/held-out split is derived from.
    pub sha256: String,
    pub source_name: String,
    /// File name of the mosaic beside this sidecar.
    pub npy: String,
    pub width: usize,
    pub height: usize,
    /// NumPy dtype of the samples: always `<u2`.
    pub dtype: String,
    pub scale: f32,
    /// The pattern of the stored mosaic at (0, 0): always `RGGB`.
    pub cfa_pattern: String,
    /// The decoder's pattern at the active area's (0, 0), before the crop.
    pub original_cfa: String,
    /// `[dx, dy]` dropped from the left and top of the active area to reach RGGB.
    pub crop_offset: [usize; 2],
    pub black_mean: f32,
    pub white_level: f32,
    pub wb_multipliers: Option<[f32; 3]>,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub iso: Option<u32>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub decoder: String,
}

/// A mosaic ready to write.
#[derive(Clone, Debug, PartialEq)]
pub struct Mosaic {
    pub width: usize,
    pub height: usize,
    pub samples: Vec<u16>,
    pub crop_offset: [usize; 2],
    pub original_cfa: String,
}

/// Cut a normalised single-plane Bayer buffer to the RGGB phase and quantise it. Not finite samples count as 0.
pub fn rggb_u16(n: &Normalized) -> Result<Mosaic> {
    let cfa = match (&n.cfa, n.cpp) {
        (Some(c), 1) if c.is_bayer() => c,
        _ => return data("not a single-plane 2x2 Bayer mosaic"),
    };
    let (w, h) = (n.width, n.height);
    if w < 4 || h < 4 || w > MAX_DIM || h > MAX_DIM {
        return data(format!("image {w}x{h} is outside 4..={MAX_DIM} per side"));
    }
    if w.checked_mul(h) != Some(n.data.len()) {
        return data("sample count does not match width x height");
    }
    // R is the only colour index 0 in a 2x2 Bayer pattern; put it at the origin and G, G, B follow
    let Some((dx, dy)) = [(0usize, 0usize), (1, 0), (0, 1), (1, 1)].into_iter().find(|&(x, y)| cfa.color_at(x, y) == 0) else {
        return data("no red site in the pattern");
    };
    if cfa.shifted(dx, dy).name() != "RGGB" {
        return data(format!("pattern {} does not reduce to RGGB", cfa.name()));
    }
    let w2 = (w - dx) / 2 * 2;
    let h2 = (h - dy) / 2 * 2;
    if w2 == 0 || h2 == 0 {
        return data("image too small after the crop");
    }
    let mut samples = Vec::with_capacity(w2 * h2);
    for y in 0..h2 {
        let start = (y + dy) * w + dx;
        let Some(row) = n.data.get(start..start + w2) else { return data("row outside the buffer") };
        samples.extend(row.iter().map(|&v| if v.is_finite() { (v.clamp(0.0, 1.0) * SCALE).round() as u16 } else { 0 }));
    }
    Ok(Mosaic { width: w2, height: h2, samples, crop_offset: [dx, dy], original_cfa: cfa.name() })
}

/// The NumPy `.npy` (format 1.0) bytes of a `[height, width]` little-endian `u16` array.
pub fn npy_u16_bytes(samples: &[u16], height: usize, width: usize) -> Result<Vec<u8>> {
    if height.checked_mul(width) != Some(samples.len()) {
        return data("npy: shape does not match the sample count");
    }
    let dict = format!("{{'descr': '<u2', 'fortran_order': False, 'shape': ({height}, {width}), }}");
    // magic (6) + version (2) + header length (2) + header, whose last byte is '\n'; total a multiple of 64
    let unpadded = NPY_MAGIC.len() + 2 + 2 + dict.len() + 1;
    let pad = (64 - unpadded % 64) % 64;
    let header_len = dict.len() + pad + 1;
    let Ok(header_len_u16) = u16::try_from(header_len) else { return data("npy: header too long") };
    let mut out = Vec::with_capacity(10 + header_len + samples.len() * 2);
    out.extend_from_slice(NPY_MAGIC);
    out.extend_from_slice(&[1, 0]);
    out.extend_from_slice(&header_len_u16.to_le_bytes());
    out.extend_from_slice(dict.as_bytes());
    out.extend(std::iter::repeat_n(b' ', pad));
    out.push(b'\n');
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Ok(out)
}

/// Parse `.npy` bytes written by [`npy_u16_bytes`] (strictly: format 1.0, `<u2`, C order, 2-D) into
/// `(samples, height, width)`.
pub fn parse_npy_u16(bytes: &[u8]) -> Result<(Vec<u16>, usize, usize)> {
    if bytes.get(..6) != Some(&NPY_MAGIC[..]) || bytes.get(6..8) != Some(&[1u8, 0][..]) {
        return data("npy: not a format 1.0 file");
    }
    let Some(Ok(len_bytes)) = bytes.get(8..10).map(<[u8; 2]>::try_from) else { return data("npy: truncated header") };
    let header_len = usize::from(u16::from_le_bytes(len_bytes));
    let Some(header) = bytes.get(10..10 + header_len) else { return data("npy: truncated header") };
    let header = std::str::from_utf8(header).map_err(|_| Error::Data("npy: header is not UTF-8".into()))?;
    if !header.contains("'descr': '<u2'") || !header.contains("'fortran_order': False") {
        return data("npy: needs dtype <u2 in C order");
    }
    let shape = header
        .split("'shape': (")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .ok_or_else(|| Error::Data("npy: no shape".into()))?;
    let dims: Vec<usize> = shape.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.parse::<usize>()).collect::<std::result::Result<_, _>>().map_err(|_| Error::Data("npy: bad shape".into()))?;
    let [height, width] = dims[..] else { return data("npy: needs a 2-D shape") };
    if height > MAX_DIM || width > MAX_DIM {
        return data("npy: shape above the size bound");
    }
    let count = height * width; // both <= MAX_DIM, so no overflow
    let body = bytes.get(10 + header_len..).unwrap_or(&[]);
    if body.len() != count * 2 {
        return data(format!("npy: {} data bytes, shape needs {}", body.len(), count * 2));
    }
    let samples = body.chunks_exact(2).filter_map(|c| <[u8; 2]>::try_from(c).ok().map(u16::from_le_bytes)).collect();
    Ok((samples, height, width))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Decode one raw and write its mosaic and sidecar into `out`. Returns the sidecar.
pub fn export_one(path: &Path, out: &Path) -> Result<Sidecar> {
    let path_s = path.display().to_string();
    let bad = |why: String| Error::Raw { path: path_s.clone(), why };
    let len = std::fs::metadata(path)?.len();
    if len > MAX_RAW_BYTES {
        return Err(bad(format!("file is {len} bytes, above the {MAX_RAW_BYTES} cap")));
    }
    let digest = hash_file(path)?;
    let sha = hex(&digest);
    let bytes = std::fs::read(path)?;
    let raw = lightcraft_raw::decode(&bytes).map_err(|e| bad(e.to_string()))?;
    drop(bytes);
    let normalized = raw.normalized().map_err(|e| bad(e.to_string()))?;
    let mosaic = rggb_u16(&normalized).map_err(|e| bad(e.to_string()))?;
    let npy_name = format!("{sha}.npy");
    write_atomic(&out.join(&npy_name), &npy_u16_bytes(&mosaic.samples, mosaic.height, mosaic.width)?)?;
    let sidecar = Sidecar {
        format_version: SIDECAR_VERSION,
        sha256: sha.clone(),
        source_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        npy: npy_name,
        width: mosaic.width,
        height: mosaic.height,
        dtype: "<u2".into(),
        scale: SCALE,
        cfa_pattern: "RGGB".into(),
        original_cfa: mosaic.original_cfa,
        crop_offset: mosaic.crop_offset,
        black_mean: raw.black.mean(),
        white_level: raw.white.first().copied().unwrap_or(0.0),
        wb_multipliers: raw.wb_multipliers,
        as_shot_neutral: raw.color.as_shot_neutral,
        iso: raw.metadata.iso,
        make: raw.metadata.make.clone(),
        model: raw.metadata.model.clone(),
        decoder: format!("lightcraft-denoise-train {}", env!("CARGO_PKG_VERSION")),
    };
    write_atomic(&out.join(format!("{sha}.json")), &serde_json::to_vec_pretty(&sidecar)?)?;
    Ok(sidecar)
}

#[derive(Debug, Default, Serialize)]
pub struct ExportSummary {
    pub written: usize,
    pub skipped: Vec<(String, String)>,
    pub duplicates: Vec<String>,
}

/// Export every raw under `input` into `out` (created). A file that cannot be used is listed in `skipped.json` and
/// the run continues; no raw at all is an error.
pub fn export_dir(input: &Path, out: &Path) -> Result<ExportSummary> {
    let paths = scan_raws(input)?;
    if paths.is_empty() {
        return Err(Error::Data(format!("no raw files under {}", input.display())));
    }
    std::fs::create_dir_all(out)?;
    let mut summary = ExportSummary::default();
    let mut seen: HashSet<String> = HashSet::new();
    for p in paths {
        let sha = match hash_file(&p) {
            Ok(h) => hex(&h),
            Err(e) => {
                summary.skipped.push((p.display().to_string(), e.to_string()));
                continue;
            }
        };
        if !seen.insert(sha) {
            summary.duplicates.push(p.display().to_string());
            continue;
        }
        match export_one(&p, out) {
            Ok(_) => summary.written += 1,
            Err(e) => summary.skipped.push((p.display().to_string(), e.to_string())),
        }
    }
    std::fs::write(out.join("skipped.json"), serde_json::to_vec_pretty(&summary)?)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_raw::Cfa;

    fn normalized(cfa: &str, w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Normalized {
        let data = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect();
        Normalized { width: w, height: h, cpp: 1, data, cfa: Cfa::bayer(cfa) }
    }

    fn sample_sidecar() -> Sidecar {
        Sidecar {
            format_version: SIDECAR_VERSION,
            sha256: "ab".repeat(32),
            source_name: "a.ARW".into(),
            npy: format!("{}.npy", "ab".repeat(32)),
            width: 6,
            height: 4,
            dtype: "<u2".into(),
            scale: SCALE,
            cfa_pattern: "RGGB".into(),
            original_cfa: "BGGR".into(),
            crop_offset: [1, 1],
            black_mean: 512.0,
            white_level: 16383.0,
            wb_multipliers: Some([2.1, 1.0, 1.6]),
            as_shot_neutral: Some([0.48, 1.0, 0.62]),
            iso: Some(100),
            make: Some("Sony".into()),
            model: Some("ILCE-7M3".into()),
            decoder: "test".into(),
        }
    }

    #[test]
    fn the_sidecar_round_trips_through_json_with_and_without_optional_fields() {
        let full = sample_sidecar();
        let back: Sidecar = serde_json::from_slice(&serde_json::to_vec_pretty(&full).unwrap()).unwrap();
        assert_eq!(back, full);
        let bare = Sidecar { wb_multipliers: None, as_shot_neutral: None, iso: None, make: None, model: None, ..full };
        let back: Sidecar = serde_json::from_slice(&serde_json::to_vec_pretty(&bare).unwrap()).unwrap();
        assert_eq!(back, bare);
    }

    #[test]
    fn the_sidecar_field_names_are_the_ones_prepare_py_reads() {
        let v = serde_json::to_value(sample_sidecar()).unwrap();
        for k in ["format_version", "sha256", "source_name", "npy", "width", "height", "dtype", "scale", "cfa_pattern", "iso", "black_mean", "white_level", "crop_offset"] {
            assert!(v.get(k).is_some(), "sidecar lacks `{k}`");
        }
        assert_eq!(v["dtype"], "<u2");
        assert_eq!(v["cfa_pattern"], "RGGB");
    }

    #[test]
    fn npy_round_trips_and_has_an_aligned_header() {
        let samples: Vec<u16> = (0..6 * 4).map(|i| (i * 2731 % 65536) as u16).collect();
        let bytes = npy_u16_bytes(&samples, 4, 6).unwrap();
        let header_len = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
        assert_eq!((10 + header_len) % 64, 0, "NumPy wants the data 64-byte aligned");
        assert_eq!(bytes[10 + header_len - 1], b'\n');
        let (back, h, w) = parse_npy_u16(&bytes).unwrap();
        assert_eq!((h, w, back), (4, 6, samples));
    }

    #[test]
    fn a_malformed_npy_is_an_error_not_a_panic() {
        let good = npy_u16_bytes(&[1, 2, 3, 4], 2, 2).unwrap();
        assert!(parse_npy_u16(&[]).is_err());
        assert!(parse_npy_u16(&good[..good.len() - 1]).is_err(), "truncated data");
        assert!(parse_npy_u16(&good[..12]).is_err(), "truncated header");
        let mut wrong = good.clone();
        let text = String::from_utf8_lossy(&wrong).replace("<u2", "<f4");
        wrong = text.into_bytes();
        assert!(parse_npy_u16(&wrong).is_err(), "wrong dtype");
        let huge = String::from_utf8_lossy(&good).replace("(2, 2)", "(99999999, 99999999)").into_bytes();
        assert!(parse_npy_u16(&huge).is_err(), "shape above the bound");
        assert!(npy_u16_bytes(&[1, 2, 3], 2, 2).is_err());
    }

    #[test]
    fn a_bggr_mosaic_is_cropped_to_rggb_and_keeps_its_samples() {
        // value encodes the source position so the crop can be checked exactly
        let n = normalized("BGGR", 9, 7, |x, y| (y * 9 + x) as f32 / 65535.0);
        let m = rggb_u16(&n).unwrap();
        assert_eq!(m.crop_offset, [1, 1]);
        assert_eq!(m.original_cfa, "BGGR");
        assert_eq!((m.width, m.height), (8, 6), "even dimensions after dropping one pixel top and left");
        assert_eq!(m.samples[0], (9 + 1) as u16, "first sample is the source's (1, 1)");
        assert_eq!(m.samples[m.width], (2 * 9 + 1) as u16, "second row starts at the source's (1, 2)");
    }

    #[test]
    fn every_bayer_layout_reduces_to_rggb_with_the_right_offset() {
        for (name, off) in [("RGGB", [0, 0]), ("GRBG", [1, 0]), ("GBRG", [0, 1]), ("BGGR", [1, 1])] {
            let m = rggb_u16(&normalized(name, 8, 8, |_, _| 0.5)).unwrap();
            assert_eq!(m.crop_offset, off, "{name}");
        }
    }

    #[test]
    fn samples_are_clipped_quantised_and_never_not_finite() {
        let n = normalized("RGGB", 4, 4, |x, _| match x {
            0 => -0.2,
            1 => 1.7,
            2 => f32::NAN,
            _ => 0.5,
        });
        let m = rggb_u16(&n).unwrap();
        assert_eq!(&m.samples[..4], &[0, 65535, 0, 32768]);
    }

    #[test]
    fn unusable_buffers_are_errors() {
        let mut xtrans = normalized("RGGB", 8, 8, |_, _| 0.1);
        xtrans.cfa = Some(Cfa::xtrans());
        assert!(rggb_u16(&xtrans).is_err());
        let mut rgb = normalized("RGGB", 8, 8, |_, _| 0.1);
        rgb.cpp = 3;
        assert!(rggb_u16(&rgb).is_err());
        let mut short = normalized("RGGB", 8, 8, |_, _| 0.1);
        short.data.pop();
        assert!(rggb_u16(&short).is_err());
        assert!(rggb_u16(&normalized("RGGB", 2, 2, |_, _| 0.1)).is_err());
        let mut none = normalized("RGGB", 8, 8, |_, _| 0.1);
        none.cfa = None;
        assert!(rggb_u16(&none).is_err());
    }

    #[test]
    fn an_empty_or_missing_input_directory_is_an_error() {
        let dir = std::env::temp_dir().join(format!("ember-export-mosaics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(export_dir(&dir, &dir.join("out")).is_err());
        assert!(export_dir(&dir.join("missing"), &dir.join("out")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
