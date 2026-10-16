use crate::{Error, Layout, decode};

/// The payload of the first `mdat` box of an ISO-BMFF file.
fn mdat_payload(file: &[u8]) -> Option<&[u8]> {
    let mut at = 0usize;
    while at + 8 <= file.len() {
        let size = u32::from_be_bytes(file.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = file.get(at + 4..at + 8)?;
        let end = if size == 0 { file.len() } else { at.checked_add(size)? };
        if kind == b"mdat" {
            return file.get(at + 8..end);
        }
        if size < 8 {
            return None;
        }
        at = end;
    }
    None
}

/// A small AVIF made with the encoder Ember exports with, and its AV1 item data.
fn encoded(width: usize, height: usize, rgb: [u8; 3]) -> Vec<u8> {
    let px = vec![ravif::RGBA8::new(rgb[0], rgb[1], rgb[2], 255); width * height];
    let enc = ravif::Encoder::new().with_quality(90.0).with_alpha_quality(90.0).with_speed(10);
    let file = enc.encode_rgba(ravif::Img::new(&px[..], width, height)).expect("encode").avif_file;
    mdat_payload(&file).expect("mdat").to_vec()
}

#[test]
fn decodes_a_small_still_made_by_ravif() {
    let obu = encoded(64, 48, [128, 128, 128]);
    let pic = decode(&obu).expect("decode");
    assert_eq!((pic.width, pic.height), (64, 48));
    assert!(matches!(pic.bit_depth, 8 | 10 | 12));
    assert_ne!(pic.layout, Layout::I400);
    assert_eq!(pic.planes.len(), 3);
    let y = &pic.planes[0];
    assert_eq!((y.width, y.height, y.data.len()), (64, 48, 64 * 48));
    let max = f64::from((1u32 << pic.bit_depth) - 1);
    let mean = y.data.iter().map(|&v| f64::from(v)).sum::<f64>() / y.data.len() as f64 / max;
    assert!((mean - 128.0 / 255.0).abs() < 0.03, "mean luma {mean}");
    assert!(y.data.iter().all(|&v| u32::from(v) <= (1 << pic.bit_depth) - 1));
    // The stream says what the encoder tagged: full-range BT.601, sRGB.
    let c = pic.colour.expect("sequence header");
    assert!(c.full_range, "{c:?}");
}

#[test]
fn an_odd_size_keeps_its_chroma_dimensions() {
    let obu = encoded(33, 17, [200, 40, 40]);
    let pic = decode(&obu).expect("decode");
    assert_eq!((pic.width, pic.height), (33, 17));
    if let Some((sx, sy)) = pic.layout.shifts() {
        let c = &pic.planes[1];
        assert_eq!((c.width, c.height), ((33 + sx as usize) >> sx, (17 + sy as usize) >> sy));
        assert_eq!(c.data.len(), c.width * c.height);
    }
}

#[test]
fn empty_and_garbage_input_is_an_error() {
    assert!(matches!(decode(&[]), Err(Error::Empty)));
    assert!(decode(&[0]).is_err());
    assert!(decode(&[0xFF; 64]).is_err());
    let mut x = 0x2545_F491_u32;
    let noise: Vec<u8> = (0..512)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect();
    assert!(decode(&noise).is_err());
}

#[test]
fn a_truncated_stream_is_an_error() {
    let obu = encoded(64, 48, [10, 200, 90]);
    assert!(decode(&obu[..obu.len() / 2]).is_err());
    assert!(decode(&obu[..4.min(obu.len())]).is_err());
}

#[test]
fn decoding_twice_is_independent() {
    let obu = encoded(32, 32, [90, 90, 90]);
    let a = decode(&obu).expect("first");
    let b = decode(&obu).expect("second");
    assert_eq!(a.planes[0].data, b.planes[0].data);
}
