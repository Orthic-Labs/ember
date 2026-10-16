use super::{AutoTone, SceneKey, auto_tone, key_target, percentile};
use lightcraft_color::perceptual::oklab_from_2020;
use lightcraft_color::transfer::decode_srgb8;
use lightcraft_color::{REC2020, SRGB, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo};

fn finite_outputs(a: AutoTone) -> bool {
    [a.exposure, a.contrast, a.highlights, a.shadows, a.whites, a.blacks, a.vibrance, a.saturation].iter().all(|v| v.is_finite())
}

fn source_ev(src: &Rgb32f) -> Vec<f32> {
    let mut values: Vec<_> = src
        .data
        .iter()
        .filter_map(|c| {
            let y = luminance_2020(*c);
            if !y.is_finite() || y <= 1e-6 {
                return None;
            }
            let v = (y / 0.18).log2();
            v.is_finite().then_some(v)
        })
        .collect();
    values.sort_by(|a, b| a.total_cmp(b));
    values
}

fn median_after(src: &Rgb32f, a: AutoTone) -> f32 {
    let values = source_ev(src);
    percentile(&values, 0.5) + a.exposure as f32
}

fn low_key_scene() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, y| if (x + y) % 17 == 0 { [0.04, 0.035, 0.03] } else { [0.01, 0.01, 0.012] })
}

fn dark_scene_with_skin_highlight() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, y| if x < 13 && y < 13 { [0.35, 0.16, 0.09] } else { [0.01, 0.01, 0.012] })
}

fn ordinary_underexposed_scene() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, _| {
        if x < 13 {
            [0.55, 0.48, 0.4]
        } else if x < 45 {
            [0.045, 0.04, 0.035]
        } else {
            [0.02, 0.018, 0.016]
        }
    })
}

fn high_key_scene() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, y| if (x + y) % 11 == 0 { [0.65, 0.65, 0.62] } else { [1.1, 1.1, 1.05] })
}

fn backlit_scene() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, _| if x < 52 { [0.012, 0.01, 0.009] } else { [4.0, 3.8, 3.4] })
}

fn saturated_scene() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, y| if (x + y) % 2 == 0 { [0.02, 0.05, 0.8] } else { [0.7, 0.03, 0.02] })
}

fn neutral_scene() -> Rgb32f {
    Rgb32f::from_fn(64, 64, |x, y| {
        let v = 0.08 + (x + y) as f32 * 0.002;
        [v, v, v]
    })
}

fn render_with(src: &Rgb32f, settings: &DevelopSettings) -> Rgba8 {
    let info = SourceInfo::default();
    crate::render(src, &info, settings, &RenderRequest::fit(64, 64)).image
}

fn auto_settings(src: &Rgb32f) -> DevelopSettings {
    let info = SourceInfo::default();
    let mut settings = DevelopSettings::default();
    let auto = auto_tone(src, &info, &settings);
    settings.light.exposure = auto.exposure;
    settings.light.contrast = auto.contrast;
    settings.light.highlights = auto.highlights;
    settings.light.shadows = auto.shadows;
    settings.light.whites = auto.whites;
    settings.light.blacks = auto.blacks;
    settings.color.vibrance = auto.vibrance;
    settings.color.saturation = auto.saturation;
    settings
}

fn auto_render(src: &Rgb32f) -> Rgba8 {
    render_with(src, &auto_settings(src))
}

fn legacy_settings(src: &Rgb32f) -> DevelopSettings {
    let values = source_ev(src);
    let median = percentile(&values, 0.5);
    let exposure = (-median * 0.85 - 0.1).clamp(-4.0, 4.0);
    let p01 = percentile(&values, 0.01) + exposure;
    let p05 = percentile(&values, 0.05) + exposure;
    let p95 = percentile(&values, 0.95) + exposure;
    let p995 = percentile(&values, 0.995) + exposure;
    let mut settings = DevelopSettings::default();
    settings.light.exposure = exposure as f64;
    settings.light.contrast = ((6.5 - (p95 - p05)) * 6.0).clamp(-20.0, 30.0) as f64;
    settings.light.highlights = (if p995 > 2.2 { -((p995 - 2.2) * 38.0).min(90.0) } else { 0.0 }) as f64;
    settings.light.shadows = (if p05 < -4.0 { ((-4.0 - p05) * 22.0).min(70.0) } else { 0.0 }) as f64;
    settings.light.whites = (if p995 < 1.8 { ((1.8 - p995) * 25.0).min(40.0) } else { -((p995 - 3.5).max(0.0) * 10.0).min(30.0) }) as f64;
    settings.light.blacks = (if p01 > -5.0 { -((p01 + 5.0) * 10.0).min(35.0) } else { ((-7.0 - p01).max(0.0) * 8.0).min(20.0) }) as f64;
    settings.color.vibrance = 12.0;
    settings.color.saturation = 3.0;
    settings
}

fn output_luma(image: &Rgba8) -> Vec<f32> {
    image
        .data
        .iter()
        .map(|p| {
            let c = [decode_srgb8(p[0]), decode_srgb8(p[1]), decode_srgb8(p[2])];
            c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722
        })
        .collect()
}

fn output_chroma(image: &Rgba8) -> Vec<f32> {
    image
        .data
        .iter()
        .map(|p| {
            let encoded = [decode_srgb8(p[0]), decode_srgb8(p[1]), decode_srgb8(p[2])];
            let c = SRGB.to_space(&REC2020).apply_f32(encoded);
            let lab = oklab_from_2020(c);
            lab[1].hypot(lab[2])
        })
        .collect()
}

fn mean(values: &[f32]) -> f32 {
    values.iter().sum::<f32>() / values.len() as f32
}

#[test]
fn low_key_auto_keeps_night_scene_dark_in_render() {
    let src = low_key_scene();
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let median = median_after(&src, a);
    let pixels = output_luma(&auto_render(&src));
    let legacy = output_luma(&render_with(&src, &legacy_settings(&src)));
    let lit = pixels.iter().filter(|v| **v > 0.65).count() as f32 / pixels.len() as f32;
    assert!(
        median < -1.0 && lit < 0.2 && mean(&pixels) < mean(&legacy) - 0.02,
        "median {median:.2}, lit {lit:.2}, mean {:.3}/{:.3}, settings {a:?}",
        mean(&pixels),
        mean(&legacy)
    );
    assert!(a.exposure > 1.5 && a.exposure < 3.2, "{a:?}");
}

#[test]
fn high_key_auto_preserves_bright_scene_intent_in_render() {
    let src = high_key_scene();
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let median = median_after(&src, a);
    let pixels = output_luma(&auto_render(&src));
    let legacy = output_luma(&render_with(&src, &legacy_settings(&src)));
    let bright = pixels.iter().filter(|v| **v > 0.25).count() as f32 / pixels.len() as f32;
    assert!(
        median > 0.5 && bright > 0.75 && mean(&pixels) > mean(&legacy) + 0.02,
        "median {median:.2}, bright {bright:.2}, mean {:.3}/{:.3}, settings {a:?}",
        mean(&pixels),
        mean(&legacy)
    );
    assert!(a.exposure > -2.0 && a.exposure < -0.7, "{a:?}");
}

#[test]
fn ordinary_underexposed_auto_reaches_middle_exposure_in_render() {
    let src = ordinary_underexposed_scene();
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let median = median_after(&src, a);
    let pixels = output_luma(&auto_render(&src));
    let legacy_settings = legacy_settings(&src);
    let legacy = output_luma(&render_with(&src, &legacy_settings));
    let clipped = |v: &[f32]| v.iter().filter(|v| **v > 0.98).count() as f32 / v.len() as f32;
    // reaches the ordinary key (a little under middle grey, as the finals do: #28), still lifts
    // the dark body of the frame by well over a stop, and clips no more of the bright wall than
    // the legacy rule did
    let mid = |v: &[f32]| v.iter().filter(|v| (0.1..0.98).contains(*v)).sum::<f32>() / v.len() as f32;
    let key = key_target(SceneKey::default());
    assert!(
        (median - key).abs() < 0.1 && a.exposure > 1.0 && clipped(&pixels) <= clipped(&legacy),
        "median {median:.2} (key {key:.2}), mean {:.3}/{:.3}, mid {:.3}/{:.3}, clipped {:.3}/{:.3}, legacy {:.2} EV, settings {a:?}",
        mean(&pixels),
        mean(&legacy),
        mid(&pixels),
        mid(&legacy),
        clipped(&pixels),
        clipped(&legacy),
        legacy_settings.light.exposure
    );
}

#[test]
fn auto_tone_does_not_leave_ordinary_raw_midtone_underexposed() {
    // This scene is deliberately encoded below middle grey, while its bright region keeps it
    // out of the low-key branch. A RAW loader may have already applied BaselineExposure; Auto
    // must target the decoded scene itself and land its median on the ordinary key (0.5 EV under
    // middle grey, #28), neither left underexposed nor lifted past it.
    let src = ordinary_underexposed_scene();
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let median = median_after(&src, a);
    let key = key_target(SceneKey::default());
    assert!((median - key).abs() < 0.08, "decoded median should reach the ordinary key {key:.2}: {median:.3}, settings {a:?}");
    assert!(median < -0.3 && median > -0.7, "ordinary key moved: {median:.3}, settings {a:?}");
}

#[test]
fn key_target_is_pinned_and_fades_the_ordinary_offset_for_keyed_scenes() {
    let at = |low: f32, high: f32| key_target(SceneKey { low, high, backlit: 0.0 });
    // ordinary scenes sit half a stop under middle grey
    assert!((at(0.0, 0.0) + 0.5).abs() < 1e-6, "{}", at(0.0, 0.0));
    assert!((key_target(SceneKey::default()) + 0.5).abs() < 1e-6);
    // fully keyed scenes keep their own targets, no extra darkening
    assert!((at(1.0, 0.0) + 0.9).abs() < 1e-6, "{}", at(1.0, 0.0));
    assert!((at(0.0, 1.0) - 0.65).abs() < 1e-6, "{}", at(0.0, 1.0));
    // halfway: the offset is half, the keyed share is half
    assert!((at(0.5, 0.0) + 0.7).abs() < 1e-6, "{}", at(0.5, 0.0));
    // never darker than a full low-key scene's target
    assert!(at(1.0, 0.0) <= at(0.5, 0.0) && at(0.5, 0.0) <= at(0.0, 0.0));
}

#[test]
fn a_deep_shadow_tail_is_not_lifted_by_default() {
    // 30 % of the frame is near black, the body sits a stop under grey, a bright part above it:
    // an ordinary scene whose deep shadows Auto used to lift (blacks up to +20).
    let src = Rgb32f::from_fn(64, 64, |x, _| {
        if x < 19 {
            [0.003, 0.003, 0.003]
        } else if x < 45 {
            [0.09, 0.085, 0.08]
        } else {
            [0.35, 0.33, 0.3]
        }
    });
    for info in [SourceInfo::default(), SourceInfo { raw: true, ..Default::default() }] {
        let a = auto_tone(&src, &info, &DevelopSettings::default());
        assert!(a.shadows <= 10.0, "shadows may lift a deep tail only slightly: {a:?}");
        assert!(a.blacks <= 0.0, "blacks must not lift a deep tail: {a:?}");
    }
}

#[test]
fn auto_tone_tracks_a_serialized_baseline_gain_once() {
    // A DNG BaselineExposure is a multiplicative scene gain in load_bytes. Scaling this
    // decoded proxy by +2 EV must move Auto exposure by −2 EV, leaving effective tone stable.
    let src = ordinary_underexposed_scene();
    let mut baseline = src.clone();
    baseline.map_in_place(|p| p.map(|v| v * 4.0));
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let b = auto_tone(&baseline, &SourceInfo::default(), &DevelopSettings::default());
    assert!((a.exposure - b.exposure - 2.0).abs() < 0.12, "BaselineExposure drifted Auto: {a:?} vs {b:?}");
    let effective_a = median_after(&src, a);
    let effective_b = median_after(&baseline, b);
    assert!((effective_a - effective_b).abs() < 0.12, "effective medians diverged: {effective_a:.3}/{effective_b:.3}");
}

#[test]
fn low_key_auto_keeps_small_skin_highlight_distinct() {
    let src = dark_scene_with_skin_highlight();
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let pixels = output_luma(&auto_render(&src));
    assert!(a.exposure > 1.5 && a.exposure < 3.2, "{a:?}");
    assert!(pixels[4 * 64 + 4] > pixels[40 * 64 + 40] + 0.04, "skin/detail collapsed: {a:?}");
}

#[test]
fn backlit_auto_limits_foreground_brightening_and_recovers_tail() {
    let src = backlit_scene();
    let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    let image = auto_render(&src);
    let pixels = output_luma(&image);
    let legacy = output_luma(&render_with(&src, &legacy_settings(&src)));
    let highlights = pixels.iter().filter(|v| **v > 0.98).count() as f32 / pixels.len() as f32;
    let legacy_highlights = legacy.iter().filter(|v| **v > 0.98).count() as f32 / legacy.len() as f32;
    assert!(a.exposure <= 2.0 && a.highlights < -50.0, "settings {a:?}");
    assert!(
        highlights <= legacy_highlights && mean(&pixels) < mean(&legacy) - 0.01,
        "highlight clip share {highlights:.2}/{legacy_highlights:.2}, mean {:.3}/{:.3}, settings {a:?}",
        mean(&pixels),
        mean(&legacy)
    );
}

#[test]
fn saturated_auto_reduces_rendered_colour_boost() {
    let a = auto_tone(&saturated_scene(), &SourceInfo::default(), &DevelopSettings::default());
    let image = auto_render(&saturated_scene());
    let chroma = output_chroma(&image);
    let legacy = output_chroma(&render_with(&saturated_scene(), &legacy_settings(&saturated_scene())));
    let mean_chroma = mean(&chroma);
    let legacy_mean = mean(&legacy);
    assert!(a.vibrance <= 4.0 && a.saturation <= 0.0, "{a:?}");
    assert!(mean_chroma < legacy_mean - 0.002, "rendered chroma {mean_chroma:.3}/{legacy_mean:.3}, settings {a:?}");
}

#[test]
fn neutral_auto_retains_gentle_rendered_colour_lift() {
    let a = auto_tone(&neutral_scene(), &SourceInfo::default(), &DevelopSettings::default());
    let image = auto_render(&neutral_scene());
    let chroma = output_chroma(&image);
    let max = chroma.into_iter().fold(0.0, f32::max);
    assert!(a.vibrance >= 12.0 && a.saturation >= 2.0, "{a:?}");
    assert!(max < 0.01, "neutral rendered chroma {max:.3}, settings {a:?}");
}

#[test]
fn invalid_pixels_never_make_auto_non_finite() {
    let mixed = Rgb32f::from_fn(32, 32, |x, y| match (x + y) % 7 {
        0 => [f32::NAN, 0.1, 0.1],
        1 => [f32::INFINITY, f32::NEG_INFINITY, 0.1],
        _ => [0.12, 0.1, 0.09],
    });
    let a = auto_tone(&mixed, &SourceInfo::default(), &DevelopSettings::default());
    assert!(finite_outputs(a), "{a:?}");

    let invalid = Rgb32f::filled(8, 8, [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
    assert!(finite_outputs(auto_tone(&invalid, &SourceInfo::default(), &DevelopSettings::default())));
}

#[test]
fn empty_input_returns_neutral_finite_settings() {
    let a = auto_tone(&Rgb32f::new(0, 0), &SourceInfo::default(), &DevelopSettings::default());
    assert_eq!(a, AutoTone::default());
}

#[test]
fn auto_is_a_pure_function_of_its_input() {
    for src in [low_key_scene(), ordinary_underexposed_scene(), high_key_scene(), backlit_scene()] {
        let a = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
        let b = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
        assert_eq!(a, b);
        let raw = SourceInfo { raw: true, ..Default::default() };
        assert_eq!(auto_tone(&src, &raw, &DevelopSettings::default()), auto_tone(&src, &raw, &DevelopSettings::default()));
    }
}

#[test]
fn burst_frames_across_a_key_threshold_get_nearly_the_same_auto() {
    // Two frames of one burst, 6 % apart in brightness, straddling the low-key boundary
    // (median −1.35 EV). A branch on that boundary moved exposure by almost a stop between them.
    let scale = |g: f32| {
        let mut s = low_key_scene();
        s.map_in_place(|p| p.map(|v| v * g));
        s
    };
    let (a, b) = (scale(6.9), scale(7.3));
    let ta = auto_tone(&a, &SourceInfo::default(), &DevelopSettings::default());
    let tb = auto_tone(&b, &SourceInfo::default(), &DevelopSettings::default());
    let ma = percentile(&source_ev(&a), 0.5);
    let mb = percentile(&source_ev(&b), 0.5);
    assert!(ma < -1.35 && mb > -1.35, "frames should straddle the threshold: {ma:.2} / {mb:.2}");
    assert!((ta.exposure - tb.exposure).abs() < 0.3, "exposure jumped across a burst: {ta:?} vs {tb:?}");
    for (x, y) in
        [(ta.contrast, tb.contrast), (ta.highlights, tb.highlights), (ta.shadows, tb.shadows), (ta.whites, tb.whites), (ta.blacks, tb.blacks)]
    {
        assert!((x - y).abs() <= 12.0, "a shaping slider jumped across a burst: {ta:?} vs {tb:?}");
    }
}

#[test]
fn fitted_sliders_track_the_tone_map_of_the_source_kind() {
    // The fit reads the real tone map: a raw (Reinhard shoulder, highlight headroom) and a
    // display-referred source (clips at 1.0) get different recovery for the same bright tail.
    let src = ordinary_underexposed_scene();
    let raw = auto_tone(&src, &SourceInfo { raw: true, ..Default::default() }, &DevelopSettings::default());
    let jpeg = auto_tone(&src, &SourceInfo::default(), &DevelopSettings::default());
    assert!(jpeg.highlights < raw.highlights, "display source needs more recovery: raw {raw:?}, jpeg {jpeg:?}");
}

#[test]
fn auto_is_centre_weighted_and_so_spatially_sensitive() {
    // A dark subject in the middle of a bright frame: the histogram alone would darken the
    // frame; the subject should still come up. The same pixels shuffled (same histogram) get a
    // different Auto, as a spatial Auto must.
    let framed = Rgb32f::from_fn(64, 64, |x, y| {
        let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
        if dx * dx + dy * dy < 14.0 * 14.0 { [0.012, 0.011, 0.01] } else { [0.6, 0.58, 0.55] }
    });
    let shuffled = Rgb32f::from_fn(64, 64, |x, y| {
        let (sx, sy) = ((x * 37 + y * 11) % 64, (x * 5 + y * 23) % 64);
        framed.get(sx, sy)
    });
    let a = auto_tone(&framed, &SourceInfo::default(), &DevelopSettings::default());
    let b = auto_tone(&shuffled, &SourceInfo::default(), &DevelopSettings::default());
    assert!(a.exposure > b.exposure + 0.3, "centre subject should weigh more: framed {a:?}, shuffled {b:?}");
    // a flat frame is unaffected by weighting (every pixel is the same)
    let flat = Rgb32f::filled(64, 64, [0.05, 0.05, 0.05]);
    assert_eq!(
        auto_tone(&flat, &SourceInfo::default(), &DevelopSettings::default()),
        auto_tone(&flat, &SourceInfo::default(), &DevelopSettings::default())
    );
}

#[test]
fn auto_wb_resists_a_large_coloured_surface_and_holds_tint_near_the_locus() {
    use super::auto_wb;
    // a blue-cast scene: 70 % of the frame is a red wall, 30 % a true grey; the cast is the light
    let cast = [0.8f32, 0.9, 1.25];
    let scene = Rgb32f::from_fn(64, 64, |x, _| {
        let base = if x < 45 { [0.5, 0.12, 0.1] } else { [0.2, 0.2, 0.2] };
        [base[0] * cast[0], base[1] * cast[1], base[2] * cast[2]]
    });
    let info = SourceInfo { raw: true, relative_wb: true, ..Default::default() };
    let (temp, tint) = auto_wb(&scene, &info);
    let mut s = DevelopSettings::default();
    s.wb.mode = lightcraft_develop::WbMode::Custom;
    s.wb.temp = temp;
    s.wb.tint = tint;
    let mut corrected = scene.clone();
    crate::local::white_balance(&mut corrected, &info, &s);
    let g = corrected.get(60, 10);
    let spread = (g[0].max(g[1]).max(g[2]) - g[0].min(g[1]).min(g[2])) / g[1];
    assert!(spread < 0.2, "grey should come out near neutral despite the red wall: {g:?} (temp {temp}, tint {tint})");
    // a green scene is not a green light: Auto's tint stays bounded, the picker's does not
    let foliage = Rgb32f::from_fn(32, 32, |_, _| [0.08, 0.3, 0.05]);
    let (_, tint) = auto_wb(&foliage, &info);
    assert!(tint.abs() <= 60.0, "{tint}");
    let (_, exact) = super::neutral_wb(&foliage, &info);
    assert!(exact > 60.0, "{exact}");
    // hostile pixels never break it
    let bad = Rgb32f::from_fn(16, 16, |x, _| if x % 2 == 0 { [f32::NAN, -1.0, f32::INFINITY] } else { [0.2, 0.2, 0.2] });
    let (t, ti) = auto_wb(&bad, &info);
    assert!(t.is_finite() && ti.is_finite());
}
