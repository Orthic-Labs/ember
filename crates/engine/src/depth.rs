//! The depth map of a photo that embeds one, as the normalized plane Depth Range selects on.
//!
//! Sources, all read from the file alone: Google Pixel and other Android portrait JPEGs (Dynamic Depth container
//! item or the older `GDepth:Data`, [`lightcraft_meta::find_depth`]) and iPhone portrait HEIC photos (the auxiliary
//! depth / disparity image, when the build has the HEIF decoder).
//!
//! The map is stored at low resolution (long edge at most [`MAX_EDGE`]) as depth 0 (nearest point of this photo)
//! to 1 (farthest), over the whole EXIF-oriented image, and is sampled in normalized image coordinates when a mask
//! is evaluated, so previews and exports select the same places. Normalizing to the photo's own span (not to the
//! camera's declared near and far) is what the Depth Range sliders mean: how much of this scene's depth is in.

use lightcraft_codecs::depth::DepthCodes;
use lightcraft_geom::Orientation;
use lightcraft_meta::RangeFormat;
use lightcraft_raster::Plane;

/// The longest edge of the stored depth plane (larger maps are averaged down).
pub const MAX_EDGE: usize = 512;

/// How the codes of a depth image relate to distance from the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Encoding {
    /// GDepth / Dynamic Depth: codes 0..1 through `format` between `near` and `far` (metres).
    Range { format: RangeFormat, near: Option<f64>, far: Option<f64> },
    /// A larger code is nearer (inverse distance), as Apple's portrait depth.
    Disparity,
    /// A larger code is farther.
    Distance,
}

/// The depth plane of the photo in `bytes`: `Ok(None)` for one without a depth map, an error for a depth map that
/// can't be read. `exif_orientation` turns a JPEG's map upright with its picture (a HEIC's already is).
pub fn embedded_depth(bytes: &[u8], exif_orientation: Orientation) -> Result<Option<Plane>, String> {
    match lightcraft_codecs::sniff(bytes) {
        Some(lightcraft_codecs::Format::Jpeg) => {
            let Some(d) = lightcraft_meta::find_depth(bytes).map_err(|e| e.to_string())? else { return Ok(None) };
            let codes = lightcraft_codecs::depth::decode_codes(&d.image).map_err(|e| e.to_string())?;
            let plane = to_plane(&codes, Encoding::Range { format: d.format, near: d.near, far: d.far })?;
            Ok(Some(plane.into_oriented(exif_orientation)))
        }
        Some(lightcraft_codecs::Format::Heif) => {
            let Some(d) = lightcraft_codecs::depth::heif_depth(bytes).map_err(|e| e.to_string())? else { return Ok(None) };
            to_plane(&d.codes, if d.disparity { Encoding::Disparity } else { Encoding::Distance }).map(Some)
        }
        _ => Ok(None),
    }
}

/// Decoded depth codes as a plane of relative depth: averaged down to [`MAX_EDGE`], converted to distance by
/// `encoding`, and scaled so the nearest point is 0 and the farthest 1. A map without any variation (or damaged)
/// is an error: there is nothing to select on.
pub fn to_plane(codes: &DepthCodes, encoding: Encoding) -> Result<Plane, String> {
    let (w, h) = (codes.width, codes.height);
    if w == 0 || h == 0 || codes.max == 0 || codes.data.len() != w.saturating_mul(h) {
        return Err("the depth image is empty or damaged".into());
    }
    let f = w.max(h).div_ceil(MAX_EDGE).max(1);
    let (bw, bh) = (w.div_ceil(f), h.div_ceil(f));
    let max = f64::from(codes.max);
    let mut depth = Vec::with_capacity(bw * bh);
    for by in 0..bh {
        for bx in 0..bw {
            let (mut sum, mut n) = (0.0f64, 0u32);
            for y in by * f..((by + 1) * f).min(h) {
                for x in bx * f..((bx + 1) * f).min(w) {
                    sum += f64::from(codes.data.get(y * w + x).copied().unwrap_or(0));
                    n += 1;
                }
            }
            depth.push(distance((sum / f64::from(n.max(1)) / max).clamp(0.0, 1.0), &encoding));
        }
    }
    let (lo, hi) = depth.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &d| (lo.min(d), hi.max(d)));
    if !(lo.is_finite() && hi.is_finite() && hi - lo > 1e-6) {
        return Err("the depth map has no variation".into());
    }
    let mut plane = Plane::new(bw, bh);
    for (p, d) in plane.data.iter_mut().zip(&depth) {
        *p = ((d - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    }
    Ok(plane)
}

/// A code `v` (0..1) as a quantity that grows with distance from the camera (its scale is irrelevant: the
/// caller normalizes). `RangeInverse` is `far·near / (far − v·(far − near))`; `RangeLinear` is affine in `v`.
/// Missing or unusable `near` / `far` leave the code itself.
fn distance(v: f64, encoding: &Encoding) -> f64 {
    match *encoding {
        Encoding::Disparity => 1.0 - v,
        Encoding::Distance => v,
        Encoding::Range { format, near, far } => match (format, near, far) {
            (RangeFormat::Linear, Some(n), Some(f)) if f < n => 1.0 - v,
            (RangeFormat::Inverse, Some(n), Some(f)) if n > 0.0 && f > n => f * n / (f - v * (f - n)),
            _ => v,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(w: usize, h: usize, max: u32, data: &[u16]) -> DepthCodes {
        DepthCodes { width: w, height: h, max, data: data.to_vec() }
    }

    fn close(a: f32, b: f64) {
        assert!((f64::from(a) - b).abs() < 1e-4, "{a} vs {b}");
    }

    #[test]
    fn range_linear_is_affine_in_the_code() {
        let c = codes(6, 1, 255, &[0, 51, 102, 153, 204, 255]);
        let p = to_plane(&c, Encoding::Range { format: RangeFormat::Linear, near: Some(0.5), far: Some(5.0) }).unwrap();
        for (i, v) in [0.0, 0.2, 0.4, 0.6, 0.8, 1.0].into_iter().enumerate() {
            close(p.data[i], v);
        }
        // without near and far, and with them reversed
        let p = to_plane(&c, Encoding::Range { format: RangeFormat::Linear, near: None, far: None }).unwrap();
        close(p.data[1], 0.2);
        let p = to_plane(&c, Encoding::Range { format: RangeFormat::Linear, near: Some(5.0), far: Some(0.5) }).unwrap();
        close(p.data[1], 0.8);
    }

    #[test]
    fn range_inverse_follows_the_gdepth_formula() {
        // d = far · near / (far − v · (far − near)): near 1 m at code 0, far 10 m at code 255
        let c = codes(6, 1, 255, &[0, 51, 102, 153, 204, 255]);
        let p = to_plane(&c, Encoding::Range { format: RangeFormat::Inverse, near: Some(1.0), far: Some(10.0) }).unwrap();
        for (i, v) in [0.0, 0.2, 0.4, 0.6, 0.8, 1.0].into_iter().enumerate() {
            let metres = 10.0 / (10.0 - 9.0 * v);
            close(p.data[i], (metres - 1.0) / 9.0);
        }
        // finer steps near the camera: the middle code is well below the middle of the span
        assert!(p.data[3] < 0.15, "{}", p.data[3]);
        // unusable near / far fall back to the code
        let p = to_plane(&c, Encoding::Range { format: RangeFormat::Inverse, near: Some(0.0), far: Some(10.0) }).unwrap();
        close(p.data[2], 0.4);
    }

    #[test]
    fn disparity_and_distance_orient_near_to_far() {
        let c = codes(3, 1, 65535, &[0, 32768, 65535]);
        let p = to_plane(&c, Encoding::Disparity).unwrap();
        assert_eq!((p.data[0], p.data[2]), (1.0, 0.0));
        let p = to_plane(&c, Encoding::Distance).unwrap();
        assert_eq!((p.data[0], p.data[2]), (0.0, 1.0));
    }

    #[test]
    fn large_maps_are_averaged_down() {
        let (w, h) = (2048, 1024);
        let data: Vec<u16> = (0..w * h).map(|i| ((i % w) / 8) as u16).collect();
        let p = to_plane(&codes(w, h, 255, &data), Encoding::Distance).unwrap();
        assert_eq!((p.width, p.height), (512, 256));
        assert_eq!((p.data[0], p.data[511]), (0.0, 1.0));
    }

    #[test]
    fn flat_or_damaged_maps_are_errors() {
        assert!(to_plane(&codes(2, 2, 255, &[7; 4]), Encoding::Distance).is_err());
        assert!(to_plane(&codes(2, 2, 255, &[1, 2, 3]), Encoding::Distance).is_err());
        assert!(to_plane(&codes(0, 2, 255, &[]), Encoding::Distance).is_err());
        assert!(to_plane(&codes(2, 1, 0, &[1, 2]), Encoding::Distance).is_err());
    }

    // ---- whole files ----

    fn crc32(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    /// An 8-bit grey PNG with a stored (uncompressed) zlib stream.
    fn png_grey8(w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend(crc32(&body).to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in pixels.chunks(w as usize) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend((raw.len() as u16).to_le_bytes());
        z.extend((!(raw.len() as u16)).to_le_bytes());
        z.extend_from_slice(&raw);
        let (mut a, mut b) = (1u32, 0u32);
        for &v in &raw {
            a = (a + u32::from(v)) % 65521;
            b = (b + a) % 65521;
        }
        z.extend(((b << 16) | a).to_be_bytes());
        let mut ihdr = Vec::new();
        ihdr.extend(w.to_be_bytes());
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 0, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn b64(data: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = c.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
            for k in 0..=c.len() {
                s.push(A[((n >> (18 - 6 * k)) & 63) as usize] as char);
            }
            s.extend(std::iter::repeat_n('=', 3 - c.len()));
        }
        s
    }

    fn app1(sig: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0xff, 0xe1];
        v.extend_from_slice(&((sig.len() + payload.len()) as u16 + 2).to_be_bytes());
        v.extend_from_slice(sig);
        v.extend_from_slice(payload);
        v
    }

    /// A grey `w` × `h` JPEG with the GDepth depth map `depth_png` in its extended XMP.
    fn pixel_style_jpeg(w: u32, h: u32, depth_png: &[u8]) -> Vec<u8> {
        let pixels = vec![128u8; (w * h * 3) as usize];
        let image = lightcraft_codecs::EncodeImage::new(w, h, 3, lightcraft_codecs::Samples::U8(&pixels));
        let jpeg = lightcraft_codecs::encode_jpeg(&image, 90, lightcraft_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
        let ext = format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:GDepth=\"http://ns.google.com/photos/1.0/depthmap/\" GDepth:Format=\"RangeInverse\" \
             GDepth:Near=\"1\" GDepth:Far=\"10\" GDepth:Mime=\"image/png\" GDepth:Data=\"{}\"/></rdf:RDF></x:xmpmeta>",
            b64(depth_png)
        );
        let mut guid_and_lengths = vec![b'0'; 32];
        guid_and_lengths.extend((ext.len() as u32).to_be_bytes());
        guid_and_lengths.extend(0u32.to_be_bytes());
        guid_and_lengths.extend_from_slice(ext.as_bytes());
        // (our segment follows the SOI of the encoder's JPEG)
        let mut file = vec![0xff, 0xd8];
        file.extend(app1(b"http://ns.adobe.com/xmp/extension/\0", &guid_and_lengths));
        file.extend_from_slice(&jpeg[2..]);
        file
    }

    /// Codes 0, 36, … 252 across 8 columns, 4 rows: near on the left.
    fn ramp_png() -> Vec<u8> {
        png_grey8(8, 4, &(0..32).map(|i| (i % 8) as u8 * 36).collect::<Vec<u8>>())
    }

    #[test]
    fn a_pixel_style_jpeg_yields_a_normalized_depth_plane() {
        let file = pixel_style_jpeg(32, 16, &ramp_png());
        let p = embedded_depth(&file, Orientation::Normal).unwrap().unwrap();
        assert_eq!((p.width, p.height), (8, 4));
        // RangeInverse, near 1 m, far 10 m
        let want = |x: usize| {
            let v = (x as f64 * 36.0) / 255.0;
            let d = |v: f64| 10.0 / (10.0 - 9.0 * v);
            (d(v) - d(0.0)) / (d(252.0 / 255.0) - d(0.0))
        };
        for x in 0..8 {
            close(p.data[x], want(x));
            close(p.data[24 + x], want(x));
        }
        // turned upright with the picture: EXIF orientation 6 (rotate 90 clockwise) makes the map 4 × 8
        let turned = embedded_depth(&file, Orientation::from_exif(6)).unwrap().unwrap();
        assert_eq!((turned.width, turned.height), (4, 8));
        // the left edge (near) is the top of the turned picture
        assert!(turned.get(1, 0) < 0.01 && turned.get(1, 7) > 0.99, "{} {}", turned.get(1, 0), turned.get(1, 7));
    }

    #[test]
    fn photos_without_depth_and_damaged_depth() {
        let plain = {
            let image = lightcraft_codecs::EncodeImage::new(8, 8, 3, lightcraft_codecs::Samples::U8(&[90u8; 192]));
            lightcraft_codecs::encode_jpeg(&image, 90, lightcraft_codecs::ChromaSubsampling::S444, &Default::default()).unwrap()
        };
        assert!(matches!(embedded_depth(&plain, Orientation::Normal), Ok(None)));
        assert!(matches!(embedded_depth(b"not an image", Orientation::Normal), Ok(None)));
        // a flat map has nothing to select on; a cut-short PNG does not decode
        let flat = pixel_style_jpeg(8, 8, &png_grey8(8, 4, &[9; 32]));
        assert!(embedded_depth(&flat, Orientation::Normal).is_err());
        let png = ramp_png();
        let cut = pixel_style_jpeg(8, 8, &png[..png.len() - 30]);
        assert!(embedded_depth(&cut, Orientation::Normal).is_err());
        // cut anywhere: an error or nothing, never a panic
        let file = pixel_style_jpeg(8, 8, &png);
        for n in (0..file.len()).step_by(7) {
            let _ = embedded_depth(&file[..n], Orientation::Normal);
        }
    }

    /// The loaded photo carries the depth map, and a Depth Range mask uses it: the near (left) part of the photo is
    /// darkened and the far part is not; a photo without depth is not touched by the mask.
    #[test]
    fn depth_range_mask_edits_a_loaded_photo_by_its_embedded_depth() {
        use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
        use lightcraft_pipeline::{RenderRequest, render};
        let file = pixel_style_jpeg(32, 16, &ramp_png());
        let (img, info) = crate::files::load_bytes(&file, 64).unwrap();
        assert_eq!((img.width, img.height), (32, 16));
        assert!(info.mattes.as_ref().is_some_and(|m| m.has_depth()));
        let mask = |hi: f64| {
            let shape = MaskShape::DepthRange { lo: 0.0, hi, feather: 0.0 };
            Mask {
                components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
                adjust: LocalAdjustments { exposure: -3.0, ..Default::default() },
                ..Default::default()
            }
        };
        let s = DevelopSettings { masks: vec![mask(0.1)], ..Default::default() };
        let req = RenderRequest::fit(32, 16);
        let green = |s: &DevelopSettings, x: usize| render(&img, &info, s, &req).image.get(x, 8)[1] as i32;
        let plain = DevelopSettings::default();
        assert!(green(&s, 2) + 40 < green(&plain, 2), "the near part is darkened");
        assert_eq!(green(&s, 29), green(&plain, 29), "the far part is not");
        // a photo without a depth map: the mask selects nothing
        let bare = lightcraft_pipeline::SourceInfo { mattes: None, ..info.clone() };
        assert_eq!(render(&img, &bare, &s, &req).image.get(2, 8)[1] as i32, green(&plain, 2));
    }
}
