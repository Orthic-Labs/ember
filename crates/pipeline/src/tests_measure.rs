//! Dense measurement (`crate::measure`, `docs/dense-measurement.md`): fixed layout, determinism,
//! tone / colour / spatial behaviour on synthetic images, resolution independence and hostile input.

use lightcraft_color::perceptual::{lch_to_lab, oklab_to_2020};
use lightcraft_color::{REC2020, SRGB};
use lightcraft_raster::{Plane, Rgb32f};

use crate::auto::fit::encode;
use crate::measure::{HUE_BANDS, LEN, MeasureError, Measurements, REFERENCE_COLOURS, RegionKind, RegionStats, SCHEMA, hue_band_centres, measure};

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// A smooth procedural scene in normalized coordinates: blue sky over brown ground, a left-to-right
/// shade, a soft red blob and a soft warm highlight. No edges, so every resolution samples the same
/// function and measures nearly the same.
fn scene(w: usize, h: usize) -> Rgb32f {
    Rgb32f::from_fn(w, h, |x, y| {
        let u = (x as f32 + 0.5) / w as f32;
        let v = (y as f32 + 0.5) / h as f32;
        let blob = |cx: f32, cy: f32, s: f32| (-((u - cx).powi(2) + (v - cy).powi(2)) / (2.0 * s * s)).exp();
        let base = mix([0.10, 0.22, 0.55], [0.32, 0.20, 0.07], smoothstep(0.35, 0.75, v));
        let shade = 0.7 + 0.6 * u;
        let shaded = base.map(|c| c * shade);
        let c = mix(shaded, [0.80, 0.06, 0.04], blob(0.25, 0.6, 0.10));
        let sun = blob(0.8, 0.2, 0.06);
        [c[0] + 0.5 * sun, c[1] + 0.45 * sun, c[2] + 0.3 * sun]
    })
}

/// A soft elliptical mask in normalized coordinates (alpha 1 inside, falling to 0 outside).
fn ellipse_mask(w: usize, h: usize) -> Plane {
    Plane::from_fn(w, h, |x, y| {
        let u = (x as f32 + 0.5) / w as f32 - 0.5;
        let v = (y as f32 + 0.5) / h as f32 - 0.5;
        let d = (u * u / 0.09 + v * v / 0.0625).sqrt();
        1.0 - smoothstep(0.8, 1.2, d)
    })
}

fn grey(w: usize, h: usize, v: f32) -> Rgb32f {
    Rgb32f::filled(w, h, [v; 3])
}

/// The value of the field called `name`.
fn value(m: &Measurements, name: &str) -> f32 {
    let i = Measurements::names().iter().position(|n| n == name).unwrap();
    m.to_vec()[i]
}

fn bits(m: &Measurements) -> Vec<u32> {
    m.to_vec().iter().map(|v| v.to_bits()).collect()
}

#[test]
fn layout_is_fixed_and_named() {
    let names = Measurements::names();
    let m = measure(&scene(64, 48), &[]).unwrap();
    assert_eq!(LEN, 202);
    assert_eq!(names.len(), 202);
    assert_eq!(m.to_vec().len(), 202);
    assert_eq!(m.schema, SCHEMA);
    assert_eq!(SCHEMA, "ember.dense-measure.v1");
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "names are unique");
    assert_eq!(names[0], "tone.lum.p0_5");
    assert_eq!(names[8], "tone.lum.p50");
    assert_eq!(names[16], "tone.lum.p99_5");
    assert_eq!(names[17], "tone.r.p0_5");
    assert_eq!(names[68], "clip.high");
    assert_eq!(names[69], "clip.low");
    assert_eq!(names[70], "chroma.p10");
    assert_eq!(names[75], "hue.red.share");
    assert_eq!(names[201], "region.skin.chroma.p50");
    for wanted in [
        "hue.red.chroma",
        "hue.magenta.offset",
        "region.sky.lum.p50",
        "tint.midtones.a",
        "neutral.b",
        "grid.chroma.r3c3",
        "radial.ring4",
        "local_contrast.r16pct",
    ] {
        assert!(names.iter().any(|n| n == wanted), "missing {wanted}");
    }
    assert_eq!(m.to_vec()[8], m.tone.lum[8]);
    assert!(m.validate().is_ok());
    assert!(m.to_vec().iter().all(|v| v.is_finite()));
}

#[test]
fn measuring_twice_is_bit_identical() {
    let img = scene(96, 72);
    let mask = ellipse_mask(96, 72);
    let a = measure(&img, &[(RegionKind::Subject, &mask)]).unwrap();
    let b = measure(&img, &[(RegionKind::Subject, &mask)]).unwrap();
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(a, b);
}

#[test]
fn doubling_the_light_raises_the_median_by_the_encode_power() {
    let ramp = |gain: f32| Rgb32f::from_fn(128, 64, |x, _| [gain * (0.02 + 0.38 * x as f32 / 127.0); 3]);
    let one = measure(&ramp(1.0), &[]).unwrap();
    let two = measure(&ramp(2.0), &[]).unwrap();
    let ratio = two.tone.lum[8] / one.tone.lum[8];
    let expected = 2.0f32.powf(1.0 / 2.2);
    assert!((ratio / expected - 1.0).abs() < 0.02, "median ratio {ratio} vs {expected}");
    // a grey ramp: every channel follows the luminance
    assert!((two.tone.g[8] / one.tone.g[8] / expected - 1.0).abs() < 0.02);
    assert_eq!(one.clip.high, 0.0);
    assert_eq!(two.clip.high, 0.0);
}

#[test]
fn uniform_grey_has_no_chroma_hue_or_cast() {
    let m = measure(&grey(32, 24, 0.18), &[]).unwrap();
    assert!(m.chroma.iter().all(|&c| c < 0.005), "{:?}", m.chroma);
    for (band, b) in HUE_BANDS.iter().zip(&m.hue) {
        assert_eq!(b.share, 0.0, "{band}");
        assert_eq!((b.offset, b.chroma, b.lum), (0.0, 0.0, 0.0), "{band}");
    }
    assert!(m.neutral.a.abs() < 0.005 && m.neutral.b.abs() < 0.005, "{:?}", m.neutral);
    // Oklab L of 0.18 is 0.565: all midtone
    assert!((m.tint[1].share - 1.0).abs() < 1e-6);
    assert_eq!(m.tint[0].share, 0.0);
    assert_eq!(m.tint[2].share, 0.0);
    let want = encode(0.18);
    assert!(m.tone.lum.iter().all(|v| (v - want).abs() < 1e-5));
    assert!(m.tone.r.iter().zip(&m.tone.b).all(|(r, b)| (r - b).abs() < 1e-6));
    assert!(m.local_contrast.iter().all(|&c| c < 1e-6), "{:?}", m.local_contrast);
}

#[test]
fn band_centres_are_computed_distinct_and_spread() {
    let centres = hue_band_centres();
    let mut sorted = centres;
    sorted.sort_by(f32::total_cmp);
    assert!(sorted.iter().all(|c| c.abs() <= std::f32::consts::PI));
    let gaps: Vec<f32> = sorted.windows(2).map(|p| p[1] - p[0]).chain(std::iter::once(sorted[0] + std::f32::consts::TAU - sorted[7])).collect();
    assert!(gaps.iter().all(|&g| g > 0.2 && g < 1.5), "gaps {gaps:?}");
    assert!((gaps.iter().sum::<f32>() - std::f32::consts::TAU).abs() < 1e-4);
    // red sits near 29 degrees, blue near 264 (-96): the centres follow the colours, not the names
    assert!(centres[0] > 0.3 && centres[0] < 0.7, "red {}", centres[0]);
    assert!(centres[5] < -1.3 && centres[5] > -2.0, "blue {}", centres[5]);
}

#[test]
fn each_reference_colour_lands_in_its_own_band() {
    let to_2020 = SRGB.to_space(&REC2020);
    for (k, (name, rgb)) in HUE_BANDS.iter().zip(&REFERENCE_COLOURS).enumerate() {
        let m = measure(&Rgb32f::filled(16, 16, to_2020.apply_f32(*rgb)), &[]).unwrap();
        assert!(m.hue[k].share >= 0.9, "{name}: own band holds {}", m.hue[k].share);
        let total: f32 = m.hue.iter().map(|b| b.share).sum();
        assert!((total - 1.0).abs() < 1e-4, "{name}: weights sum to {total}");
        assert!(m.hue[k].offset.abs() < 1e-3, "{name}: offset {}", m.hue[k].offset);
        assert!(m.hue[k].chroma > 0.1, "{name}: chroma {}", m.hue[k].chroma);
    }
}

#[test]
fn a_hue_rotation_moves_the_offset_the_right_way() {
    let centres = hue_band_centres();
    let delta = 0.05f32;
    for (k, name) in HUE_BANDS.iter().enumerate() {
        for sign in [1.0f32, -1.0] {
            let lab = lch_to_lab([0.6, 0.06, centres[k] + sign * delta]);
            let m = measure(&Rgb32f::filled(16, 16, oklab_to_2020(lab)), &[]).unwrap();
            let band = m.hue[k];
            assert!(band.share > 0.7, "{name} {sign}: share {}", band.share);
            assert!((band.offset - sign * delta).abs() < 0.005, "{name} {sign}: offset {} for a {} rad rotation", band.offset, sign * delta);
            assert!(band.offset * sign > 0.0);
        }
    }
}

#[test]
fn a_half_dark_half_bright_image_shows_in_the_grid() {
    let img = Rgb32f::from_fn(64, 48, |x, _| if x < 32 { [0.04; 3] } else { [0.6; 3] });
    let m = measure(&img, &[]).unwrap();
    for row in 0..4 {
        for left in 0..2 {
            for right in 2..4 {
                let (l, r) = (m.grid.lum[row * 4 + left], m.grid.lum[row * 4 + right]);
                assert!(r > l + 0.3, "row {row}: left {l} right {r}");
            }
        }
    }
    assert!((m.grid.lum[0] - encode(0.04)).abs() < 1e-5);
    assert!((m.grid.lum[3] - encode(0.6)).abs() < 1e-5);
    // a grey frame has no chroma anywhere
    assert!(m.grid.chroma.iter().all(|&c| c < 0.005));
}

#[test]
fn a_vignette_darkens_the_rings_outward() {
    let (w, h) = (256usize, 192usize);
    let half_diagonal = 0.5 * ((w * w + h * h) as f32).sqrt();
    let img = Rgb32f::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - 0.5 * w as f32, y as f32 + 0.5 - 0.5 * h as f32);
        let r = dx.hypot(dy) / half_diagonal;
        [0.9 * (1.0 - 0.85 * r); 3]
    });
    let m = measure(&img, &[]).unwrap();
    assert!(m.radial.windows(2).all(|p| p[1] < p[0]), "{:?}", m.radial);
    assert!(m.radial.iter().all(|&v| v > 0.0));
}

#[test]
fn clipping_shares_count_white_and_crushed_pixels() {
    let img = Rgb32f::from_fn(32, 32, |x, y| {
        if x < 16 {
            [1.2; 3]
        } else if y < 16 {
            [0.0; 3]
        } else {
            [0.2; 3]
        }
    });
    let m = measure(&img, &[]).unwrap();
    assert!((m.clip.high - 0.5).abs() < 1e-6, "{:?}", m.clip);
    assert!((m.clip.low - 0.25).abs() < 1e-6, "{:?}", m.clip);
    // encoded values saturate at white
    assert_eq!(m.tone.lum[16], 1.0);
    assert_eq!(m.tone.lum[0], 0.0);
}

#[test]
fn local_contrast_sees_texture_not_flat_fields() {
    let flat = measure(&grey(64, 64, 0.3), &[]).unwrap();
    assert!(flat.local_contrast.iter().all(|&c| c < 1e-6));
    let checker = Rgb32f::from_fn(64, 64, |x, y| if (x + y) % 2 == 0 { [0.1; 3] } else { [0.4; 3] });
    let m = measure(&checker, &[]).unwrap();
    assert!(m.local_contrast.iter().all(|&c| c > 0.3), "{:?}", m.local_contrast);
}

#[test]
fn a_mask_over_the_bright_half_reports_the_bright_tones() {
    let (w, h) = (64usize, 48usize);
    let bright = [0.5, 0.45, 0.4];
    let img = Rgb32f::from_fn(w, h, |x, _| if x < w / 2 { [0.04; 3] } else { bright });
    let mask = Plane::from_fn(w, h, |x, _| if x >= w / 2 { 1.0 } else { 0.0 });
    let m = measure(&img, &[(RegionKind::Sky, &mask)]).unwrap();
    let sky = m.region(RegionKind::Sky);
    assert_eq!(sky.present, 1.0);
    assert!((sky.share - 0.5).abs() < 1e-6, "{}", sky.share);
    let want = encode(lightcraft_color::luminance_2020(bright));
    assert!(sky.lum.iter().all(|v| (v - want).abs() < 1e-5), "{:?} vs {want}", sky.lum);
    assert!(sky.lum[2] > m.tone.lum[8] + 0.3, "region p50 {} vs global {}", sky.lum[2], m.tone.lum[8]);
    assert!(sky.l > 0.6 && sky.chroma_p50 > 0.0);
    assert_eq!(value(&m, "region.sky.lum.p50"), sky.lum[2]);
    // the regions that were not supplied are absent and all zeros
    for kind in [RegionKind::Subject, RegionKind::Background, RegionKind::Skin] {
        assert_eq!(*m.region(kind), RegionStats::default(), "{kind:?}");
        assert_eq!(m.region(kind).present, 0.0);
    }
}

#[test]
fn a_tiny_region_is_absent() {
    let (w, h) = (64usize, 64usize);
    let img = scene(w, h);
    // 1 pixel of 4096 is 0.02 % of the frame, under the 0.5 % floor
    let dot = Plane::from_fn(w, h, |x, y| if (x, y) == (10, 10) { 1.0 } else { 0.0 });
    let m = measure(&img, &[(RegionKind::Skin, &dot)]).unwrap();
    assert_eq!(*m.region(RegionKind::Skin), RegionStats::default());
    // a faint mask has share but no pixel at alpha 0.5: absent too
    let faint = Plane::filled(w, h, 0.3);
    let m = measure(&img, &[(RegionKind::Skin, &faint)]).unwrap();
    assert_eq!(m.region(RegionKind::Skin).present, 0.0);
}

/// Per-group tolerance of the 256 × 192 vs 512 × 384 comparison. Smooth scene, same normalized
/// function: differences are sampling (half a pixel of a 256 px frame) only. Tone percentiles in
/// the tails get the widest band because the encode curve is steepest there.
fn tolerance(name: &str) -> f32 {
    let parts: Vec<&str> = name.split('.').collect();
    match parts.as_slice() {
        ["tone", ..] => 0.03,
        ["clip", ..] | ["chroma", ..] => 0.01,
        ["hue", _, "share"] => 0.015,
        ["hue", _, "offset"] => 0.05,
        ["hue", _, "chroma"] => 0.01,
        ["hue", _, _] => 0.02,
        ["tint", _, "share"] => 0.015,
        ["tint", ..] | ["neutral", ..] => 0.01,
        ["grid", "lum", _] | ["radial", _] => 0.02,
        ["grid", ..] => 0.01,
        ["local_contrast", _] => 0.015,
        ["region", _, "present"] => 0.0,
        ["region", _, "share"] | ["region", _, "oklab", _] | ["region", _, "chroma", _] => 0.01,
        ["region", ..] => 0.03,
        _ => 0.0,
    }
}

#[test]
fn a_scene_measures_the_same_at_two_resolutions() {
    let small = measure(&scene(256, 192), &[(RegionKind::Subject, &ellipse_mask(256, 192))]).unwrap();
    let large = measure(&scene(512, 384), &[(RegionKind::Subject, &ellipse_mask(512, 384))]).unwrap();
    let names = Measurements::names();
    let (a, b) = (small.to_vec(), large.to_vec());
    let at = |v: &[f32], key: String| v[names.iter().position(|n| *n == key).unwrap()];
    // the scene holds enough colour in enough bands to make the comparison meaningful
    assert!(small.hue.iter().filter(|b| b.share >= 0.03).count() >= 3, "{:?}", small.hue);
    assert_eq!(small.region(RegionKind::Subject).present, 1.0);
    for (i, name) in names.iter().enumerate() {
        let parts: Vec<&str> = name.split('.').collect();
        // a sparsely populated band or range has no stable mean: compare it only when both sides hold some
        let sparse = match parts.as_slice() {
            ["hue", band, field] if *field != "share" => {
                let key = format!("hue.{band}.share");
                at(a.as_slice(), key.clone()) < 0.03 || at(b.as_slice(), key) < 0.03
            }
            ["tint", range, field] if *field != "share" => {
                let key = format!("tint.{range}.share");
                at(a.as_slice(), key.clone()) < 0.03 || at(b.as_slice(), key) < 0.03
            }
            _ => false,
        };
        if sparse {
            continue;
        }
        let tol = tolerance(name);
        assert!((a[i] - b[i]).abs() <= tol, "{name}: {} at 256 vs {} at 512 (tolerance {tol})", a[i], b[i]);
    }
}

#[test]
fn hostile_images_are_errors_not_panics() {
    // every pixel non-finite
    let nan = Rgb32f::filled(32, 32, [f32::NAN; 3]);
    assert_eq!(measure(&nan, &[]), Err(MeasureError::TooFewPixels { finite: 0 }));
    // zero size
    assert_eq!(measure(&Rgb32f::new(0, 0), &[]), Err(MeasureError::EmptyImage));
    assert_eq!(measure(&Rgb32f::new(8, 0), &[]), Err(MeasureError::EmptyImage));
    // 15 finite pixels are too few, 16 are enough
    let mut four = grey(4, 4, 0.3);
    assert!(measure(&four, &[]).is_ok());
    four.data[5] = [f32::NAN, 0.3, 0.3];
    assert_eq!(measure(&four, &[]), Err(MeasureError::TooFewPixels { finite: 15 }));
    // over the pixel cap (no buffer is allocated: the size is checked first)
    let huge = Rgb32f { width: 10_000, height: 10_000, data: Vec::new() };
    assert_eq!(measure(&huge, &[]), Err(MeasureError::TooLarge { pixels: 100_000_000 }));
    let overflow = Rgb32f { width: usize::MAX / 2, height: 3, data: Vec::new() };
    assert_eq!(measure(&overflow, &[]), Err(MeasureError::TooLarge { pixels: usize::MAX }));
    // buffer that does not match the stated size
    let short = Rgb32f { width: 8, height: 8, data: vec![[0.5; 3]; 10] };
    assert_eq!(measure(&short, &[]), Err(MeasureError::MalformedImage));
}

#[test]
fn hostile_regions_are_errors() {
    let img = scene(32, 32);
    let wrong = Plane::filled(16, 16, 1.0);
    assert_eq!(measure(&img, &[(RegionKind::Sky, &wrong)]), Err(MeasureError::RegionSizeMismatch(RegionKind::Sky)));
    let narrow = Plane::filled(32, 31, 1.0);
    assert_eq!(measure(&img, &[(RegionKind::Skin, &narrow)]), Err(MeasureError::RegionSizeMismatch(RegionKind::Skin)));
    let short = Plane { width: 32, height: 32, data: vec![1.0; 10] };
    assert_eq!(measure(&img, &[(RegionKind::Subject, &short)]), Err(MeasureError::RegionSizeMismatch(RegionKind::Subject)));
    let ok = Plane::filled(32, 32, 1.0);
    assert_eq!(measure(&img, &[(RegionKind::Sky, &ok), (RegionKind::Sky, &ok)]), Err(MeasureError::DuplicateRegion(RegionKind::Sky)));
    // all four kinds together are fine, in any order
    let all = [(RegionKind::Skin, &ok), (RegionKind::Background, &ok), (RegionKind::Sky, &ok), (RegionKind::Subject, &ok)];
    let m = measure(&img, &all).unwrap();
    assert!(RegionKind::ALL.iter().all(|k| m.region(*k).present == 1.0));
    // a non-finite alpha reads as 0: a mask of NaN is absent
    let nan = Plane::filled(32, 32, f32::NAN);
    let m = measure(&img, &[(RegionKind::Sky, &nan)]).unwrap();
    assert_eq!(m.region(RegionKind::Sky).present, 0.0);
    assert!(m.validate().is_ok());
}

#[test]
fn non_finite_pixels_are_skipped_and_outputs_stay_finite() {
    let clean = scene(64, 48);
    let mut dirty = clean.clone();
    for (i, p) in dirty.data.iter_mut().enumerate() {
        if i % 7 == 0 {
            *p = [f32::NAN, 0.2, 0.2];
        } else if i % 11 == 0 {
            *p = [0.2, f32::INFINITY, 0.2];
        } else if i % 13 == 0 {
            *p = [f32::NEG_INFINITY, 0.2, 0.2];
        }
    }
    let mask = ellipse_mask(64, 48);
    let m = measure(&dirty, &[(RegionKind::Subject, &mask)]).unwrap();
    assert!(m.validate().is_ok());
    assert_eq!(m.to_vec().len(), LEN);
    assert!(m.to_vec().iter().all(|v| v.is_finite()));
    // the skipped pixels are a scatter: the picture's statistics barely move
    let c = measure(&clean, &[(RegionKind::Subject, &mask)]).unwrap();
    assert!((m.tone.lum[8] - c.tone.lum[8]).abs() < 0.03, "{} vs {}", m.tone.lum[8], c.tone.lum[8]);
    assert!((m.clip.high - c.clip.high).abs() < 0.01);
    assert!(m.local_contrast.iter().all(|v| v.is_finite()));
}

#[test]
fn absurd_finite_values_cannot_overflow() {
    let img = Rgb32f::from_fn(32, 32, |x, _| match x % 4 {
        0 => [f32::MAX; 3],
        1 => [f32::MIN; 3],
        2 => [1.0e30, 0.0, 0.0],
        _ => [0.2; 3],
    });
    let m = measure(&img, &[]).unwrap();
    assert!(m.validate().is_ok());
    assert!(m.to_vec().iter().all(|v| v.is_finite()));
}

#[test]
fn single_row_and_column_images_measure() {
    let row = Rgb32f::from_fn(64, 1, |x, _| [0.1 + 0.005 * x as f32; 3]);
    let col = Rgb32f::from_fn(1, 64, |_, y| [0.1 + 0.005 * y as f32; 3]);
    for img in [row, col] {
        let m = measure(&img, &[]).unwrap();
        assert!(m.validate().is_ok());
        assert!(m.to_vec().iter().all(|v| v.is_finite()));
    }
}

#[test]
fn validate_and_json_refuse_what_measure_never_makes() {
    let m = measure(&scene(48, 36), &[(RegionKind::Sky, &ellipse_mask(48, 36))]).unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back: Measurements = serde_json::from_str(&json).unwrap();
    assert!(back.validate().is_ok());
    assert_eq!(back.to_vec().len(), LEN);
    assert!(m.to_vec().iter().zip(back.to_vec()).all(|(a, b)| (a - b).abs() <= 1e-6 * a.abs().max(1.0)));
    assert!(serde_json::from_str::<Measurements>(&json.replacen('{', "{\"extra\":1,", 1)).is_err());
    assert_eq!(serde_json::to_string(&RegionKind::Background).unwrap(), "\"background\"");

    let mut bad = m.clone();
    bad.schema = "other".into();
    assert!(bad.validate().is_err());
    let mut bad = m.clone();
    bad.tone.lum[3] = f32::NAN;
    assert!(bad.validate().is_err());
    assert!(bad.to_vec().iter().all(|v| v.is_finite()), "to_vec reads a non-finite value as 0");
    assert_eq!(bad.to_vec().len(), LEN);
    let mut bad = m.clone();
    bad.regions[1].l = f32::INFINITY;
    assert!(bad.validate().is_err());
}
