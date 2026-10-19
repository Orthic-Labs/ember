//! Offline plane extraction: no library import, development, provider calls or source writes.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use lightcraft_photo_ai::digest;
use lightcraft_raw::plane::{GrayPlane, MAX_PLANE_BYTES, gray_tiff};
use serde_json::json;

const USAGE: &str = "ai decision plane --input PRIVATE_TIFF --sha256 EXPECTED_SHA256 --out NEW_PGM";

pub fn run(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut expected = None;
    let mut output = None;
    let mut index = 0usize;
    while let Some(option) = args.get(index) {
        let slot = match option.as_str() {
            "--input" => &mut input,
            "--sha256" => &mut expected,
            "--out" => &mut output,
            _ => return Err(USAGE.into()),
        };
        if slot.is_some() {
            return Err("duplicate plane option".into());
        }
        index = index.saturating_add(1);
        *slot = Some(args.get(index).ok_or("missing plane option value")?.clone());
        index = index.saturating_add(1);
    }
    let input = input.ok_or(USAGE)?;
    let expected = expected.ok_or(USAGE)?;
    let output = output.ok_or(USAGE)?;
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err("plane SHA256 must be 64 lowercase hexadecimal characters".into());
    }
    let source = read_input(Path::new(&input))?;
    if digest(&source) != expected {
        return Err("plane source SHA256 mismatch".into());
    }
    // Existing output (including a link to input) fails before decoding; the publish at the end
    // still refuses to replace one that appears meanwhile. A failed run leaves no output file.
    if crate::output_file::name_taken(Path::new(&output)) {
        return Err("new plane output: the output already exists".into());
    }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().map_err(|e| format!("plane worker: {e}"))?;
    let plane = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool.install(|| gray_tiff(&source))))
        .map_err(|_| "plane decoder panicked")?
        .map_err(|e| e.to_string())?;
    let pgm = pgm(&plane)?;
    crate::output_file::publish_new(Path::new(&output), &pgm).map_err(|e| format!("plane output write: {e}"))?;
    println!(
        "{}",
        json!({
            "schema": "ember.gray-plane-extract.v1", "extractor_revision": "gray-plane.v2",
            "input_sha256": expected, "pgm_sha256": digest(&pgm), "pgm_bytes": pgm.len(),
            "width": plane.width, "height": plane.height, "bits": plane.bits,
            "samples": plane.samples.len(), "sample_order": "row-major",
            "sample_encoding": if plane.bits == 8 { "u8" } else { "u16-big-endian" },
            "nominal_max": if plane.bits == 8 { 255 } else { 65535 },
            "sample_fidelity": "codec_reconstructed_unsigned_samples",
            "pre_lossy_encoding_codes_verified": false, "chunks": plane.chunks,
            "orientation": 1, "rescaled": false, "semantic_label": null,
            "image_registration_verified": false, "training_eligible": false,
            "source_writes": false, "decoder_threads": 1
        })
    );
    Ok(())
}

fn read_input(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("plane input: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("plane metadata: {e}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PLANE_BYTES as u64 {
        return Err("plane input must be a regular file of at most 32 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PLANE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("plane read: {e}"))?;
    if bytes.len() > MAX_PLANE_BYTES {
        return Err("plane grew beyond input cap".into());
    }
    Ok(bytes)
}

fn pgm(plane: &GrayPlane) -> Result<Vec<u8>, String> {
    let max = match plane.bits {
        8 => 255u16,
        16 => 65535u16,
        _ => return Err("unsupported PGM bit depth".into()),
    };
    let count = u64::from(plane.width).checked_mul(u64::from(plane.height)).ok_or("plane count overflow")?;
    if count != plane.samples.len() as u64 || plane.samples.iter().any(|&s| s > max) {
        return Err("invalid PGM sample count or range".into());
    }
    let mut bytes = format!("P5\n{} {}\n{max}\n", plane.width, plane.height).into_bytes();
    for &value in &plane.samples {
        if plane.bits == 8 {
            bytes.push(u8::try_from(value).map_err(|_| "PGM sample exceeds 8 bits")?);
        } else {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgm_has_exact_sample_codes_and_network_byte_order() {
        let plane = GrayPlane { width: 2, height: 1, bits: 16, samples: vec![1, 65535], chunks: vec![] };
        assert_eq!(pgm(&plane).unwrap(), b"P5\n2 1\n65535\n\x00\x01\xff\xff");
        let plane = GrayPlane { width: 2, height: 1, bits: 8, samples: vec![0, 255], chunks: vec![] };
        assert_eq!(pgm(&plane).unwrap(), b"P5\n2 1\n255\n\x00\xff");
    }
}
