//! End-to-end tests of the out-of-process decoder: the real `ember-av1-worker` binary, real AV1
//! data from ravif, mutated streams that make rav1d abort, and a worker that lies.
#![cfg(not(target_arch = "wasm32"))]

use lightcraft_av1dec::host::decode_with_worker;
use lightcraft_av1dec::{Error, Layout, decode_in_process};
use std::path::Path;
use std::time::Duration;

const WORKER: &str = env!("CARGO_BIN_EXE_ember-av1-worker");
const TIMEOUT: Duration = Duration::from_secs(60);

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

fn encoded(width: usize, height: usize, rgb: [u8; 3]) -> Vec<u8> {
    let px = vec![ravif::RGBA8::new(rgb[0], rgb[1], rgb[2], 255); width * height];
    let enc = ravif::Encoder::new().with_quality(80.0).with_alpha_quality(80.0).with_speed(10);
    let file = enc.encode_rgba(ravif::Img::new(&px[..], width, height)).expect("encode").avif_file;
    mdat_payload(&file).expect("mdat").to_vec()
}

#[test]
fn a_good_stream_decodes_identically_through_the_worker() {
    let obu = encoded(64, 48, [120, 60, 200]);
    let via_worker = decode_with_worker(Path::new(WORKER), &obu, TIMEOUT).expect("worker decode");
    let direct = decode_in_process(&obu).expect("in-process decode");
    assert_eq!((via_worker.width, via_worker.height), (64, 48));
    assert_eq!((via_worker.bit_depth, via_worker.layout, via_worker.colour), (direct.bit_depth, direct.layout, direct.colour));
    assert_ne!(via_worker.layout, Layout::I400);
    assert_eq!(via_worker.planes.len(), direct.planes.len());
    for (a, b) in via_worker.planes.iter().zip(&direct.planes) {
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert_eq!(a.data, b.data);
    }
}

#[test]
fn bad_input_is_an_error_from_the_worker() {
    assert!(matches!(decode_with_worker(Path::new(WORKER), &[], TIMEOUT), Err(Error::Empty)));
    assert!(decode_with_worker(Path::new(WORKER), &[0xFF; 64], TIMEOUT).is_err());
    let obu = encoded(64, 48, [10, 200, 90]);
    assert!(decode_with_worker(Path::new(WORKER), &obu[..obu.len() / 2], TIMEOUT).is_err());
}

/// The regression test for the abort: rav1d 1.1.0 panics inside `extern "C"` (and so aborts the
/// process) when a tile fails to decode. Mutated streams reach that; the host must see errors only.
#[test]
fn mutated_streams_never_take_the_host_down() {
    let good = encoded(96, 64, [30, 140, 220]);
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let (mut ok, mut err) = (0, 0);
    for _ in 0..120 {
        let mut b = good.clone();
        for _ in 0..1 + rnd() % 4 {
            let i = rnd() as usize % b.len();
            if let Some(x) = b.get_mut(i) {
                *x = rnd() as u8;
            }
        }
        match decode_with_worker(Path::new(WORKER), &b, TIMEOUT) {
            Ok(p) => {
                ok += 1;
                assert_eq!(p.planes.len(), if p.layout == Layout::I400 { 1 } else { 3 });
            }
            Err(_) => err += 1,
        }
    }
    assert_eq!(ok + err, 120);
    assert!(err > 0, "no mutation was refused; the test no longer exercises failures");
}

#[test]
fn a_missing_worker_is_an_error() {
    let r = decode_with_worker(Path::new("definitely/not/a/worker"), &[1, 2, 3], TIMEOUT);
    assert!(matches!(r, Err(Error::HelperMissing)), "{r:?}");
}

#[test]
fn a_worker_that_does_not_speak_the_protocol_is_an_error() {
    // Any program that reads stdin and prints something else: the shell on Windows, `cat` elsewhere.
    let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/cat" };
    if !cfg!(windows) && !Path::new(shell).is_file() {
        return;
    }
    let r = decode_with_worker(Path::new(shell), &[1, 2, 3, 4], Duration::from_secs(20));
    assert!(r.is_err(), "{r:?}");
}
