//! MobileSAM against the real checkpoint.
//!
//! `LIGHTCRAFT_MOBILESAM_DIR=<folder holding mobile_sam-tiny-vitt.safetensors> cargo test -p lightcraft-segment --release --test mobilesam -- --nocapture`.
//! Skipped (passes) without it: the checkpoint is a 40.6 MB download, never part of the repository.

use std::path::PathBuf;

use lightcraft_segment::mobilesam::MobileSam;
use lightcraft_segment::{Click, Model};

fn dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("LIGHTCRAFT_MOBILESAM_DIR")?);
    lightcraft_segment::is_mobilesam_dir(&d).then_some(d)
}

const W: usize = 320;
const H: usize = 240;
/// The disc's centre and radius, in pixels.
const DISC: (f32, f32, f32) = (200.0, 120.0, 60.0);

/// A bright disc on a dark vertical gradient.
fn photo() -> Vec<u8> {
    let mut rgb = vec![0u8; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let inside = (x as f32 + 0.5 - DISC.0).hypot(y as f32 + 0.5 - DISC.1) <= DISC.2;
            let v = if inside { [235, 220, 60] } else { [20 + (y * 40 / H) as u8, 30, 50] };
            rgb[(y * W + x) * 3..(y * W + x) * 3 + 3].copy_from_slice(&v);
        }
    }
    rgb
}

/// IoU between the logits (`side²` over the whole photo) and the disc.
fn iou_with_disc(logits: &[f32], side: usize) -> f32 {
    let (mut inter, mut union) = (0usize, 0usize);
    for oy in 0..side {
        for ox in 0..side {
            let (x, y) = ((ox as f32 + 0.5) / side as f32 * W as f32, (oy as f32 + 0.5) / side as f32 * H as f32);
            let truth = (x - DISC.0).hypot(y - DISC.1) <= DISC.2;
            let predicted = logits[oy * side + ox] > 0.0;
            inter += usize::from(truth && predicted);
            union += usize::from(truth || predicted);
        }
    }
    inter as f32 / union.max(1) as f32
}

#[test]
fn the_downloaded_checkpoint_matches_the_pinned_size_and_hash() {
    let Some(dir) = dir() else {
        eprintln!("skipped: set LIGHTCRAFT_MOBILESAM_DIR");
        return;
    };
    lightcraft_segment::validate_model_dir(&dir).unwrap();
}

#[test]
fn a_click_and_a_box_select_the_disc() {
    let Some(dir) = dir() else {
        eprintln!("skipped: set LIGHTCRAFT_MOBILESAM_DIR");
        return;
    };
    let mut model = MobileSam::load(&dir).unwrap();
    eprintln!("device: {:?}", model.device());
    let rgb = photo();
    let t = std::time::Instant::now();
    let enc = model.encode(&rgb, W, H).unwrap();
    eprintln!("encode {W}x{H}: {:?} (resized {:?})", t.elapsed(), enc.resized());
    assert_eq!(enc.resized(), (1024, 768));

    let click = Click { x: DISC.0 / W as f32, y: DISC.1 / H as f32, positive: true };
    let t = std::time::Instant::now();
    let p = model.segment(&enc, &[click], None).unwrap();
    eprintln!("click: {:?}, predicted IoU {:.3}", t.elapsed(), p.score);
    let iou = iou_with_disc(&p.logits, lightcraft_segment::mobilesam_prep::OUTPUT_SIDE);
    eprintln!("click IoU with the disc: {iou:.3}");
    assert!(iou > 0.7, "the click should select the disc: IoU {iou}");

    // a box around the disc
    let (x0, y0, x1, y1) = ((DISC.0 - DISC.2) / W as f32, (DISC.1 - DISC.2) / H as f32, (DISC.0 + DISC.2) / W as f32, (DISC.1 + DISC.2) / H as f32);
    let p = model.segment(&enc, &[], Some([x0, y0, x1, y1])).unwrap();
    let iou = iou_with_disc(&p.logits, lightcraft_segment::mobilesam_prep::OUTPUT_SIDE);
    eprintln!("box IoU with the disc: {iou:.3}");
    assert!(iou > 0.7, "the box should select the disc: IoU {iou}");

    // a click on the background that excludes the disc: the disc is not selected
    let bg = Click { x: 0.1, y: 0.1, positive: true };
    let p = model.segment(&enc, &[bg], None).unwrap();
    let iou = iou_with_disc(&p.logits, lightcraft_segment::mobilesam_prep::OUTPUT_SIDE);
    eprintln!("background click IoU with the disc: {iou:.3}");
    assert!(iou < 0.3, "a click far from the disc should not select it: IoU {iou}");

    // errors, not panics
    assert!(model.segment(&enc, &[], None).is_err());
    assert!(model.segment(&enc, &[], Some([0.5, 0.5, 0.5, 0.9])).is_err());
    assert!(model.segment(&enc, &[Click { x: f32::NAN, y: 0.5, positive: true }], None).is_err());
}

#[test]
fn the_model_enum_loads_mobilesam_and_text_needs_grounding() {
    let Some(dir) = dir() else {
        eprintln!("skipped: set LIGHTCRAFT_MOBILESAM_DIR");
        return;
    };
    let mut model = Model::load(&dir).unwrap();
    // (a folder that also holds SAM 3 would load SAM 3 instead; this one holds MobileSAM alone)
    if lightcraft_segment::is_model_dir(&dir) {
        return;
    }
    assert!(!model.supports_text());
    let mut scene = model.encode(&photo(), W, H).unwrap();
    let e = model.segment_text(&mut scene, "disc").err().unwrap();
    assert!(matches!(e, lightcraft_segment::Error::NeedsGrounding), "{e}");
    let mask = model.segment_box(&mut scene, [0.4, 0.2, 0.9, 0.8], &[]).unwrap();
    assert_eq!(mask.side, lightcraft_segment::mobilesam_prep::OUTPUT_SIDE);
}
