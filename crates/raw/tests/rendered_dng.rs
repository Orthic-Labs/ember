//! Render to DNG: a linear DNG of a rendered image reads back as the same pixels and colours.

use lightcraft_color::{Mat3, PROPHOTO, REC2020, SRGB};
use lightcraft_raw::{Method, color, write_rendered_dng};

fn image(w: usize, h: usize) -> Vec<f32> {
    (0..w * h).flat_map(|i| [(i % w) as f32 / w as f32, (i / w) as f32 / h as f32, 0.25 + (i % 7) as f32 * 0.25]).collect()
}

fn close(a: &Mat3, b: &Mat3, tol: f64) -> bool {
    a.0.iter().flatten().zip(b.0.iter().flatten()).all(|(x, y)| (x - y).abs() < tol)
}

#[test]
fn rendered_dng_round_trips_pixels_and_colour() {
    let (w, h) = (300usize, 70usize);
    let rgb = image(w, h);
    for space in [SRGB, PROPHOTO] {
        let meta = lightcraft_meta::Metadata { make: Some("Maker".into()), model: Some("Model".into()), ..Default::default() };
        let bytes = write_rendered_dng(w, h, &rgb, &space, &meta, None).unwrap();
        let raw = lightcraft_raw::decode(&bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp, raw.cfa.is_none()), (w, h, 3, true));
        assert_eq!(raw.metadata.model.as_deref(), Some("Model"));
        let dev = raw.develop(Method::Ahd).unwrap();
        for (p, q) in rgb.as_chunks::<3>().0.iter().zip(&dev.data) {
            for k in 0..3 {
                // above-white samples (1.25, 1.5, 1.75) are clipped by white level 1 on develop
                let want = p[k].min(1.0);
                assert!((q[k].min(1.0) - want).abs() <= want * 1e-3 + 1e-4, "{space:?} {p:?} {q:?}");
            }
        }
        // the camera transform is the space → Rec. 2020 matrix under a neutral white
        let t = color::camera_transform(&raw, color::as_shot_white_xy(&raw));
        assert!(!t.matrix_is_fallback);
        assert!(t.wb.iter().all(|v| (v - 1.0).abs() < 1e-3), "{:?}", t.wb);
        assert!(close(&t.matrix, &space.to_space(&REC2020), 2e-3), "{}: {:?} vs {:?}", space.name, t.matrix, space.to_space(&REC2020));
    }
}

#[test]
fn rendered_dng_rejects_bad_input_without_panicking() {
    let m = lightcraft_meta::Metadata::default();
    assert!(write_rendered_dng(0, 4, &[], &SRGB, &m, None).is_err());
    assert!(write_rendered_dng(4, 4, &[0.5; 10], &SRGB, &m, None).is_err());
    assert!(write_rendered_dng(usize::MAX, 3, &[0.5; 10], &SRGB, &m, None).is_err());
    let odd = [f32::NAN, f32::INFINITY, -2.0, 70000.0, 0.5, 1.0];
    let raw = lightcraft_raw::decode(&write_rendered_dng(2, 1, &odd, &SRGB, &m, None).unwrap()).unwrap();
    let dev = raw.develop(Method::Bilinear).unwrap();
    assert!(dev.data.iter().flatten().all(|v| v.is_finite() && *v >= 0.0));
}

/// A smooth test image (what lossy coding is for): a gradient with a gentle diagonal wave.
#[cfg(all(feature = "jxl", feature = "jxl-encode", not(target_arch = "wasm32")))]
fn smooth(w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [x * 0.8 + 0.1, y * 0.7 + 0.1, 0.5 + 0.3 * ((x + y) * 6.0).sin() * 0.5]
        })
        .collect()
}

#[cfg(all(feature = "jxl", feature = "jxl-encode", not(target_arch = "wasm32")))]
#[test]
fn lossy_jxl_dng_has_the_dng_17_tags_and_reads_back_close_to_the_source() {
    use lightcraft_raw::{DngCompression, RawData, write_rendered_dng_with};
    use lightcraft_tiff::{Tiff, tags as t};
    let (w, h) = (300usize, 70usize); // several tiles, edge tiles included
    let rgb = smooth(w, h);
    let meta = lightcraft_meta::Metadata::default();
    let bytes = write_rendered_dng_with(w, h, &rgb, &SRGB, &meta, None, DngCompression::JxlLossy { tile: 128, quality: 90 }).unwrap();
    let tiff = Tiff::parse(&bytes).unwrap();
    let ifd = &tiff.ifds[0];
    assert_eq!(ifd.u16(t::COMPRESSION), Some(52546));
    assert_eq!(ifd.u16(t::SAMPLE_FORMAT), Some(3));
    assert_eq!(ifd.u16(t::SAMPLES_PER_PIXEL), Some(3));
    assert_eq!(ifd.u32(t::TILE_WIDTH), Some(128));
    assert_eq!(ifd.u64s(t::TILE_OFFSETS).map(|v| v.len()), Some(3 * 1));
    assert_eq!(ifd.bytes(t::DNG_VERSION), Some(&[1u8, 7, 1, 0][..]));
    let d = ifd.f64(t::JXL_DISTANCE).unwrap();
    assert!((d - 1.0).abs() < 1e-5, "distance {d}"); // quality 90 → 0.1 + 10 × 0.09
    assert_eq!(ifd.u32(t::JXL_EFFORT), Some(3));
    assert_eq!(ifd.u32(t::JXL_DECODE_SPEED), Some(0));
    let raw = lightcraft_raw::decode(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.cpp), (w, h, 3));
    let RawData::F32(px) = &raw.data else { panic!("float samples") };
    assert_eq!(px.len(), rgb.len());
    let mse = rgb.iter().zip(px).map(|(a, b)| f64::from(a - b).powi(2)).sum::<f64>() / rgb.len() as f64;
    let psnr = 10.0 * (1.0 / mse.max(1e-12)).log10();
    assert!(psnr > 30.0, "PSNR {psnr:.1} dB");
    // and it is smaller than the lossless Deflate rendering of the same image
    let plain = lightcraft_raw::write_rendered_dng(w, h, &rgb, &SRGB, &meta, None).unwrap();
    assert!(bytes.len() < plain.len(), "{} vs {}", bytes.len(), plain.len());
}

#[cfg(all(feature = "jxl", feature = "jxl-encode", not(target_arch = "wasm32")))]
#[test]
fn lossy_jxl_dng_with_a_malformed_tile_fails_to_decode() {
    use lightcraft_raw::{DngCompression, write_rendered_dng_with};
    use lightcraft_tiff::{Tiff, tags as t};
    let (w, h) = (64usize, 64usize);
    let meta = lightcraft_meta::Metadata::default();
    let mut bytes = write_rendered_dng_with(w, h, &smooth(w, h), &SRGB, &meta, None, DngCompression::JxlLossy { tile: 64, quality: 80 }).unwrap();
    let ifd = Tiff::parse(&bytes).unwrap().ifds.remove(0);
    let (off, len) = (ifd.u64s(t::TILE_OFFSETS).unwrap()[0] as usize, ifd.u64s(t::TILE_BYTE_COUNTS).unwrap()[0] as usize);
    bytes[off..off + len.min(64)].fill(0xA5);
    assert!(lightcraft_raw::decode(&bytes).is_err());
    // the writer refuses what it cannot code lossily
    let m = lightcraft_meta::Metadata::default();
    let lossy = DngCompression::JxlLossy { tile: 64, quality: 80 };
    assert!(write_rendered_dng_with(0, 4, &[], &SRGB, &m, None, lossy).is_err());
}
