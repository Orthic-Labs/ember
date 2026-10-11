//! Region-aware Auto: the subject, the background and the sky get their own treatment.
//!
//! A global fit cannot serve a subject against a washed-out background: the subject wants
//! lifting, the background wants its contrast, blacks and haze back. Here the photo is split
//! into regions by the Subject and Sky masks (today the classical heuristics in
//! [`crate::masks`]; a segmentation model drops in behind the same shapes), the global fit takes
//! its key from the subject instead of the centre ellipse, and each region is then fitted on its
//! own pixels with the same closed-loop machinery. The result is the eight global values plus
//! real, editable masks ("Auto: subject", "Auto: background", "Auto: sky") carrying local
//! adjustments, so what Auto did per region is visible and can be changed or deleted.
//!
//! Without a usable subject region (a flat frame, a subject filling the frame) this is exactly
//! the global Auto. Everything is a pure function of the proxy pixels, the source kind and the
//! white balance: fixed proxy size, fixed thresholds, fixed step schedules.

use lightcraft_color::luminance_2020;
use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_raster::{Plane, Rgb32f};
use serde::{Deserialize, Serialize};

use super::fit::{auto_targets, encode, fit_with, tone_map};
use super::{AutoTone, SceneKey, auto_tone_of, key_target, percentile, scene_population};
use crate::SourceInfo;
use crate::geometry::Frame;
use crate::output::{OutputDepth, OutputSpace};
use crate::tone::GREY;

/// Names of the masks region Auto writes (and replaces on the next run).
pub const NAME_PREFIX: &str = "Auto: ";

/// Proxy long edge the regions are measured on.
const PROXY: usize = 512;
/// A region counts when at least this share of the frame's valid pixels is inside it...
const MIN_SHARE: f32 = 0.04;
/// ...and a subject that fills more than this leaves no background to treat separately.
const MAX_SUBJECT_SHARE: f32 = 0.9;
/// A pixel belongs to a region when the region's alpha is at least this (the subject heuristic
/// gives exactly 0.5 to a flat centre: that is not a subject).
const INSIDE: f32 = 0.6;
/// A region with less spread than this (5th to 95th percentile, EV) is uniform: nothing to fit.
const MIN_SPREAD: f32 = 0.1;
/// Bisection steps of the subject's local exposure against the real render (0.008 EV over
/// [`SUBJECT_EV`]).
const SETTLE_STEPS: usize = 8;
/// How far the subject's exposure moves away from the global one, in EV: a dark subject is
/// lifted (a soft mask applies only part of it, so this is more than one needs), a bright one is
/// left bright (a white dress, snow: intended, and highlight recovery handles clipping), only
/// pulled back a little when it sits far above the key.
const SUBJECT_EV: (f32, f32) = (-0.5, 2.0);
/// Most dehaze a background gets.
const MAX_DEHAZE: f32 = 25.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Subject,
    Background,
    Sky,
}

impl Region {
    pub fn name(self) -> String {
        let what = match self {
            Region::Subject => "subject",
            Region::Background => "background",
            Region::Sky => "sky",
        };
        format!("{NAME_PREFIX}{what}")
    }

    /// The mask components covering exactly the pixels the region was fitted on: the
    /// background is everything that is neither subject nor sky.
    pub fn components(self) -> Vec<MaskComponent> {
        let add = |shape| MaskComponent { name: None, op: MaskOp::Add, invert: false, shape };
        match self {
            Region::Subject => vec![add(MaskShape::Subject)],
            Region::Background => {
                vec![add(MaskShape::Background), MaskComponent { name: None, op: MaskOp::Subtract, invert: false, shape: MaskShape::Sky }]
            }
            Region::Sky => vec![add(MaskShape::Sky)],
        }
    }
}

/// What Auto decided for one region.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionAdjust {
    pub region: Region,
    /// Share of the frame's valid pixels inside the region (0..=1).
    pub share: f32,
    pub adjust: LocalAdjustments,
}

/// Auto's global values plus the regions it treats separately (only those with a non-zero
/// adjustment).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionAuto {
    #[serde(flatten)]
    pub global: AutoTone,
    pub regions: Vec<RegionAdjust>,
}

impl RegionAuto {
    pub fn global_only(global: AutoTone) -> RegionAuto {
        RegionAuto { global, regions: Vec::new() }
    }

    /// The masks to write into a photo's settings, ids from `first_id` up, marked as Auto's
    /// (`Mask::auto`). Replace the masks [`is_auto_mask`] accepts first.
    pub fn masks(&self, first_id: u32) -> Option<Vec<Mask>> {
        self.regions
            .iter()
            .enumerate()
            .map(|(i, r)| {
                Some(Mask {
                    id: first_id.checked_add(u32::try_from(i).ok()?)?,
                    name: r.region.name(),
                    components: r.region.components(),
                    adjust: r.adjust,
                    auto: true,
                    ..Default::default()
                })
            })
            .collect()
    }

    /// Allocate a free contiguous range without trusting existing mask IDs.
    pub fn masks_for(&self, existing: &[Mask]) -> Option<Vec<Mask>> {
        if self.regions.is_empty() {
            return Some(Vec::new());
        }
        let offset = u32::try_from(self.regions.len().checked_sub(1)?).ok()?;
        let mut ids: Vec<u32> = existing.iter().map(|mask| mask.id).collect();
        ids.sort_unstable();
        let mut first = 1u32;
        for id in ids {
            if id < first {
                continue;
            }
            if id > first.checked_add(offset)? {
                break;
            }
            first = id.checked_add(1)?;
        }
        self.masks(first)
    }
}

/// A mask Auto wrote and nobody changed since: it carries Auto's provenance flag and still has
/// the name and components Auto gave it. The next Auto replaces it; a mask a person made
/// (never flagged, whatever its name), renamed or extended stays.
pub fn is_auto_mask(m: &Mask) -> bool {
    m.auto && [Region::Subject, Region::Background, Region::Sky].iter().any(|r| m.name == r.name() && m.components == r.components())
}

/// The alpha planes of the three regions on the proxy (same size as a 512 px box fit of the
/// source), for inspection and tests. `None` for an empty image.
pub struct RegionPlanes {
    pub subject: Plane,
    pub background: Plane,
    pub sky: Plane,
}

pub fn region_planes(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<RegionPlanes> {
    regions(src, info, s).map(|r| RegionPlanes { subject: r.subject, background: r.background, sky: r.sky })
}

/// A pixel inside a region: its alpha is at least this.
pub fn inside(alpha: f32) -> bool {
    alpha >= INSIDE
}

/// The proxy as the masks see it: white-balanced scene-linear pixels, their log luminance, and
/// the alpha planes of the three regions.
struct Regions {
    /// The proxy as decoded, before white balance (what a render takes).
    raw: Rgb32f,
    img: Rgb32f,
    /// Scene EV of each proxy pixel (`None` where invalid).
    ev: Vec<Option<f32>>,
    subject: Plane,
    background: Plane,
    sky: Plane,
}

fn regions(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<Regions> {
    if src.width == 0 || src.height == 0 || src.data.is_empty() {
        return None;
    }
    let raw = lightcraft_raster::resample::fit(src, PROXY, PROXY, lightcraft_raster::resample::Filter::Box);
    let mut img = raw.clone();
    let base = DevelopSettings { wb: s.wb, ..DevelopSettings::default() };
    crate::local::scene_linear_pre(&mut img, info, &base);
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return None;
    }
    let log_l = img.map(crate::local::log_lum);
    // the masks are evaluated on the uncropped, unrotated proxy (the heuristics are
    // frame-relative; a segmentation covers the whole photo too)
    let frame = Frame::new(w, h, &base, false);
    let plane = |shape: MaskShape| {
        let m = Mask { components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], ..Default::default() };
        crate::masks::evaluate_one(&m, &frame, w, h, &img, &log_l, 0.0)
    };
    let subject = plane(MaskShape::Subject);
    let sky = plane(MaskShape::Sky);
    let mut background = Plane::new(w, h);
    for ((b, s), k) in background.data.iter_mut().zip(&subject.data).zip(&sky.data) {
        *b = (1.0 - s.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
    }
    let ev = img
        .data
        .iter()
        .map(|c| {
            let y = luminance_2020(*c);
            if !y.is_finite() || y <= 1e-6 {
                return None;
            }
            let e = (y / 0.18).log2();
            e.is_finite().then_some(e)
        })
        .collect();
    Some(Regions { raw, img, ev, subject, background, sky })
}

/// Sorted scene EVs of the pixels inside `alpha`, and their share of the valid pixels.
fn population(ev: &[Option<f32>], alpha: &Plane) -> (Vec<f32>, f32) {
    let mut out = Vec::new();
    let mut valid = 0usize;
    for (e, a) in ev.iter().zip(&alpha.data) {
        let Some(e) = e else { continue };
        valid += 1;
        if *a >= INSIDE {
            out.push(*e);
        }
    }
    out.sort_by(|a, b| a.total_cmp(b));
    let share = if valid == 0 { 0.0 } else { out.len() as f32 / valid as f32 };
    (out, share)
}

/// A region worth fitting: present, and not uniform.
fn usable(ev: &[f32]) -> bool {
    !ev.is_empty() && percentile(ev, 0.95) - percentile(ev, 0.05) >= MIN_SPREAD
}

fn round_ev(v: f32) -> f64 {
    ((v as f64) * 20.0).round() / 20.0
}

/// `region − global`, clamped to the region's own range, as a local slider value.
fn local(region: f64, global: f64, lo: f64, hi: f64) -> f64 {
    (region - global).clamp(lo, hi).round()
}

/// Encoded luminance a scene EV renders to under the neutral tone map at `exposure`.
fn rendered(info: &SourceInfo, ev: f32, exposure: f32) -> f32 {
    encode(tone_map(info, 0.0, 0.0, 0.0).apply(GREY * (ev + exposure).exp2()))
}

/// Encoded median of the rendered proxy inside `alpha` (`None` without any pixel inside).
fn region_median(linear: &Rgb32f, alpha: &Plane) -> Option<f32> {
    let mut v: Vec<f32> = linear
        .data
        .iter()
        .zip(&alpha.data)
        .filter(|(_, a)| **a >= INSIDE)
        .map(|(c, _)| encode(luminance_2020(*c)))
        .filter(|y| y.is_finite())
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(percentile(&v, 0.5))
}

/// Region-aware Auto for `src` under `s`'s white balance (tone values ignored).
pub fn region_auto(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> RegionAuto {
    let Some(mut pop) = scene_population(src, info, s) else { return RegionAuto::global_only(AutoTone::default()) };
    let Some(r) = regions(src, info, s) else { return RegionAuto::global_only(auto_tone_of(&pop, info)) };
    let (subject_ev, subject_share) = population(&r.ev, &r.subject);
    let (background_ev, background_share) = population(&r.ev, &r.background);
    let (sky_ev, sky_share) = population(&r.ev, &r.sky);
    let has_subject = (MIN_SHARE..=MAX_SUBJECT_SHARE).contains(&subject_share) && usable(&subject_ev);
    if !has_subject {
        return RegionAuto::global_only(auto_tone_of(&pop, info));
    }

    // the key follows the subject: half the frame's median, half the subject's
    let subject_median = percentile(&subject_ev, 0.5);
    pop.median = 0.5 * percentile(&pop.ev, 0.5) + 0.5 * subject_median;
    let global = auto_tone_of(&pop, info);
    let key = SceneKey::of(pop.median, pop.p05, pop.p95, pop.p995);
    let exposure = global.exposure as f32;
    let mut out = RegionAuto::global_only(global);

    // --- subject: its median to the scene's key target, then its own shaping
    let want_ev = key_target(key);
    let delta = (want_ev - (subject_median + exposure)).clamp(SUBJECT_EV.0, SUBJECT_EV.1);
    // Each region moves only in its restoring direction (a subject is lifted, a background gets
    // contrast, blacks and haze back, a sky its highlights): a region's fit against whole-scene
    // targets would otherwise flatten a subject or brighten a background, and a heuristic mask
    // is not precise enough to be trusted with more.
    let mut t = auto_targets(&subject_ev, exposure + delta, key, info);
    t.w = [t.w[0] * 0.3, t.w[1] * 0.3, t.w[2], t.w[3]];
    let sh = fit_with(&subject_ev, exposure + delta, &t, info);
    let mut subject = LocalAdjustments { exposure: round_ev(delta), shadows: local(sh.shadows, global.shadows, 0.0, 40.0), ..Default::default() };

    // --- background: keep its key, restore contrast, blacks and haze
    let background = (background_share >= MIN_SHARE && usable(&background_ev)).then(|| {
        let t = auto_targets(&background_ev, exposure, key, info);
        let sh = fit_with(&background_ev, exposure, &t, info);
        // haze: lifted blacks (the 5th percentile well above black under the global render) in
        // a flat region (a bright wall has lifted blacks too, but spread); 0 at 0.12, full at
        // 0.37 encoded, full below 0.5 EV of spread, none above 1.5
        let tone = tone_map(info, global.contrast, global.whites, global.blacks);
        let p05 = encode(tone.apply(GREY * (percentile(&background_ev, 0.05) + exposure).exp2()));
        let spread = percentile(&background_ev, 0.95) - percentile(&background_ev, 0.05);
        let flat = ((1.5 - spread) / 1.0).clamp(0.0, 1.0);
        let dehaze = ((p05 - 0.12) / 0.25 * MAX_DEHAZE * flat).clamp(0.0, MAX_DEHAZE).round() as f64;
        LocalAdjustments {
            contrast: local(sh.contrast, global.contrast, 0.0, 30.0),
            blacks: local(sh.blacks, global.blacks, -35.0, 0.0),
            dehaze,
            ..Default::default()
        }
    });

    // --- sky: recover its highlights
    let sky = (sky_share >= MIN_SHARE && usable(&sky_ev)).then(|| {
        let t = auto_targets(&sky_ev, exposure, key, info);
        let sh = fit_with(&sky_ev, exposure, &t, info);
        LocalAdjustments {
            highlights: local(sh.highlights, global.highlights, -60.0, 0.0),
            whites: local(sh.whites, global.whites, -30.0, 0.0),
            ..Default::default()
        }
    });

    // --- settle the subject's exposure against the real render: the local sliders' units are
    // not the global ones, so the subject's median is bisected to its target on the proxy
    // rendered as the photo will be: the un-white-balanced proxy under the photo's own settings
    // (white balance, the person's masks, their other edits; an earlier Auto's masks replaced),
    // the global values, and Auto's masks after theirs (monotone in the local exposure)
    let want = rendered(info, want_ev, 0.0);
    let mut base = DevelopSettings { crop: Default::default(), orientation: Default::default(), ..s.clone() };
    base = crate::look::settings_with(&base, global);
    base.masks.retain(|m| !is_auto_mask(m));
    let build = |subject: LocalAdjustments| {
        let mut regions = vec![RegionAdjust { region: Region::Subject, share: subject_share, adjust: subject }];
        if let Some(a) = background {
            regions.push(RegionAdjust { region: Region::Background, share: background_share, adjust: a });
        }
        if let Some(a) = sky {
            regions.push(RegionAdjust { region: Region::Sky, share: sky_share, adjust: a });
        }
        regions
    };
    // rendered at the proxy's own size (a fit request upscales a smaller source)
    let req =
        crate::RenderRequest { space: OutputSpace::Rec2020, depth: OutputDepth::F32Linear, ..crate::RenderRequest::fit(r.img.width, r.img.height) };
    let render_median = |e: f32| -> Option<f32> {
        let mut d = base.clone();
        let masks = RegionAuto { global, regions: build(LocalAdjustments { exposure: e as f64, ..subject }) }.masks_for(&d.masks)?;
        d.masks.extend(masks);
        let rendered = crate::render(&r.raw, info, &d, &req);
        let linear = rendered.deep.as_ref().and_then(crate::look::deep_to_linear)?;
        if linear.width != r.img.width || linear.height != r.img.height {
            return None;
        }
        region_median(&linear, &r.subject)
    };
    let (mut lo, mut hi) = SUBJECT_EV;
    let mut settled = None;
    for _ in 0..SETTLE_STEPS {
        let mid = 0.5 * (lo + hi);
        let Some(m) = render_median(mid) else { break };
        settled = Some(mid);
        if m < want { lo = mid } else { hi = mid }
    }
    if let Some(e) = settled {
        subject.exposure = round_ev(e);
    }

    let unchanged = LocalAdjustments::default();
    out.regions = build(subject).into_iter().filter(|r| r.adjust != unchanged).collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RenderRequest;
    use lightcraft_color::transfer::decode_srgb8;
    use lightcraft_develop::WbMode;

    /// A dark subject (centre disc, `subject` scene-linear, about 4 EV under the background at
    /// 0.02) on a bright, washed-out background: low contrast, lifted blacks, slightly blue.
    fn washed_out_with(subject: f32) -> Rgb32f {
        Rgb32f::from_fn(128, 128, |x, y| {
            let (dx, dy) = (x as f32 - 63.5, y as f32 - 63.5);
            let t = ((x * 7 + y * 3) % 17) as f32 / 17.0;
            if dx * dx + dy * dy < 26.0 * 26.0 {
                let v = subject * (1.0 + 1.5 * t);
                [v * 1.1, v, v * 0.9]
            } else {
                let v = 0.5 + 0.12 * t;
                [v * 0.95, v, v * 1.08]
            }
        })
    }

    fn washed_out() -> Rgb32f {
        washed_out_with(0.02)
    }

    fn inside(x: usize, y: usize, w: usize, h: usize) -> bool {
        let (dx, dy) = (x as f32 - (w as f32 - 1.0) / 2.0, y as f32 - (h as f32 - 1.0) / 2.0);
        (dx * dx + dy * dy).sqrt() < 0.2 * w as f32
    }

    /// Encoded luminance of a render, per pixel.
    fn encoded(src: &Rgb32f, d: &DevelopSettings) -> Vec<f32> {
        let r = crate::render(src, &SourceInfo::default(), d, &RenderRequest::fit(128, 128)).image;
        assert_eq!((r.width, r.height), (src.width, src.height));
        r.data.iter().map(|p| encode((0.2126 * decode_srgb8(p[0]) + 0.7152 * decode_srgb8(p[1]) + 0.0722 * decode_srgb8(p[2])) as f32)).collect()
    }

    fn sorted(v: impl Iterator<Item = f32>) -> Vec<f32> {
        let mut v: Vec<f32> = v.collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v
    }

    /// Encoded median of the geometric subject disc, and median and 5th percentile of the rest.
    fn medians(src: &Rgb32f, d: &DevelopSettings) -> (f32, f32, f32) {
        let e = encoded(src, d);
        let is_disc = |i: &usize| inside(i % src.width, i / src.width, src.width, src.height);
        let sub = sorted((0..e.len()).filter(is_disc).map(|i| e[i]));
        let bg = sorted((0..e.len()).filter(|i| !is_disc(i)).map(|i| e[i]));
        (percentile(&sub, 0.5), percentile(&bg, 0.5), percentile(&bg, 0.05))
    }

    fn with(r: &RegionAuto) -> DevelopSettings {
        let mut d = crate::look::settings_with(&DevelopSettings::default(), r.global);
        d.masks = r.masks(1).unwrap();
        d
    }

    #[test]
    fn a_dark_subject_on_a_washed_out_background_gets_two_treatments() {
        let src = washed_out();
        let info = SourceInfo::default();
        let r = region_auto(&src, &info, &DevelopSettings::default());
        let subject = r.regions.iter().find(|x| x.region == Region::Subject).expect("subject region");
        let background = r.regions.iter().find(|x| x.region == Region::Background).expect("background region");
        assert!(subject.adjust.exposure > 0.3, "subject lifted: {:?}", subject.adjust);
        assert!(
            background.adjust.blacks < 0.0 || background.adjust.contrast > 0.0 || background.adjust.dehaze > 0.0,
            "background restored: {:?}",
            background.adjust
        );
        // the real render: subject near its target, background blacks deeper than global Auto's
        let global_only = crate::look::settings_with(&DevelopSettings::default(), r.global);
        let (s0, _, b0) = medians(&src, &global_only);
        let (s1, _, b1) = medians(&src, &with(&r));
        // the region as Auto sees it (the heuristic's subject includes the disc's edge band)
        // lands on the key target in the real render
        let planes = region_planes(&src, &info, &DevelopSettings::default()).unwrap();
        let e = encoded(&src, &with(&r));
        let region = sorted(e.iter().zip(&planes.subject.data).filter(|(_, a)| super::inside(**a)).map(|(v, _)| *v));
        let want = rendered(&info, key_target(SceneKey::default()), 0.0);
        let got = percentile(&region, 0.5);
        assert!((got - want).abs() < 0.06, "subject region median {got} vs target {want}; {r:?}");
        assert!(s1 > s0 + 0.05, "subject brighter than under global Auto: {s1} vs {s0}");
        assert!(b1 < b0 - 0.02, "background blacks deeper: {b1} vs {b0}");
        // repeatable
        assert_eq!(r, region_auto(&src, &info, &DevelopSettings::default()));
    }

    #[test]
    fn mask_ids_use_free_gaps_and_refuse_overflow() {
        let r = RegionAuto {
            global: super::super::auto_tone(&washed_out(), &SourceInfo::default(), &DevelopSettings::default()),
            regions: [Region::Subject, Region::Background, Region::Sky]
                .into_iter()
                .map(|region| RegionAdjust { region, share: 0.25, adjust: LocalAdjustments::default() })
                .collect(),
        };
        assert!(r.masks(u32::MAX - 1).is_none());
        let existing = [Mask { id: 1, ..Default::default() }, Mask { id: 3, ..Default::default() }, Mask { id: u32::MAX, ..Default::default() }];
        let masks = r.masks_for(&existing).unwrap();
        assert_eq!(masks.iter().map(|mask| mask.id).collect::<Vec<_>>(), [4, 5, 6]);
        assert_eq!(RegionAuto::global_only(r.global).masks_for(&existing).unwrap().len(), 0);
    }

    #[test]
    fn masks_are_named_and_replaceable() {
        let r = region_auto(&washed_out(), &SourceInfo::default(), &DevelopSettings::default());
        let masks = r.masks(7).unwrap();
        assert!(masks.iter().all(is_auto_mask));
        assert!(masks.iter().all(|m| m.auto));
        assert_eq!(masks[0].id, 7);
        assert_eq!(masks[0].name, "Auto: subject");
        let mut renamed = masks[0].clone();
        renamed.name = "Face".into();
        assert!(!is_auto_mask(&renamed));
        let mut brushed = masks[0].clone();
        brushed.components.push(MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes: vec![] } });
        assert!(!is_auto_mask(&brushed));
        // a person's mask that happens to look like Auto's is theirs
        let theirs = Mask { auto: false, ..masks[0].clone() };
        assert!(!is_auto_mask(&theirs));
        // the background mask covers what the background was fitted on: not the sky
        let bg = masks.iter().find(|m| m.name == "Auto: background").expect("background");
        assert_eq!(bg.components.len(), 2);
        assert!(matches!(bg.components[1], MaskComponent { op: MaskOp::Subtract, shape: MaskShape::Sky, .. }));
        // round trip keeps the provenance
        let json = serde_json::to_string(&masks[0]).unwrap();
        assert!(json.contains("\"auto\":true"), "{json}");
        assert!(is_auto_mask(&serde_json::from_str::<Mask>(&json).unwrap()));
    }

    /// The settle step predicts the output: the subject lands on its target under the photo's
    /// own white balance and with the person's masks in place.
    #[test]
    fn settling_sees_the_photo_s_white_balance_and_masks() {
        // a milder subject (about 2 EV under), so the lift has headroom under its cap
        let src = washed_out_with(0.08);
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.wb.mode = WbMode::Custom;
        s.wb.temp = info.as_shot_temp + 1500.0;
        s.wb.tint = info.as_shot_tint - 20.0;
        let wb = crate::local::wb_matrix_for(&info, &s).expect("custom white balance differs from as-shot");
        assert!(wb.iter().enumerate().any(|(row, values)| {
            values.iter().enumerate().any(|(column, value)| {
                let identity = if row == column { 1.0 } else { 0.0 };
                (value - identity).abs() > 1e-4
            })
        }));
        // a darkening mask of theirs over the whole frame, and a stale Auto mask to replace
        s.masks.push(Mask {
            id: 3,
            name: "mine".into(),
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::LuminanceRange { lo: 0.0, hi: 1.0, lo_feather: 0.0, hi_feather: 0.0 },
            }],
            adjust: LocalAdjustments { exposure: -0.3, ..Default::default() },
            ..Default::default()
        });
        s.masks.push(Mask {
            id: 4,
            name: "Auto: subject".into(),
            components: Region::Subject.components(),
            adjust: LocalAdjustments { exposure: 1.5, ..Default::default() },
            auto: true,
            ..Default::default()
        });
        let r = region_auto(&src, &info, &s);
        // the settle compensated for their darkening (and did not run into the cap doing it)
        let alone = region_auto(&src, &info, &DevelopSettings { masks: vec![], ..s.clone() });
        let lift = |r: &RegionAuto| r.regions.iter().find(|x| x.region == Region::Subject).map(|x| x.adjust.exposure).unwrap_or(0.0);
        assert!(lift(&r) > lift(&alone) && lift(&r) < SUBJECT_EV.1 as f64, "{} vs {}", lift(&r), lift(&alone));
        // apply as the engine does
        let mut d = crate::look::settings_with(&s, r.global);
        d.masks.retain(|m| !is_auto_mask(m));
        d.masks.extend(r.masks_for(&d.masks).unwrap());
        assert!(d.masks.iter().any(|m| m.name == "mine"));
        assert_eq!(d.masks.iter().filter(|m| m.name == "Auto: subject").count(), 1);
        let planes = region_planes(&src, &info, &s).unwrap();
        let e = encoded(&src, &d);
        let region = sorted(e.iter().zip(&planes.subject.data).filter(|(_, a)| super::inside(**a)).map(|(v, _)| *v));
        let want = rendered(&info, key_target(SceneKey::default()), 0.0);
        let got = percentile(&region, 0.5);
        assert!((got - want).abs() < 0.06, "subject region median {got} vs target {want}; {r:?}");
    }

    #[test]
    fn a_flat_frame_or_a_frame_filling_subject_is_global_auto() {
        let info = SourceInfo::default();
        let flat = Rgb32f::filled(64, 64, [0.05, 0.05, 0.05]);
        let r = region_auto(&flat, &info, &DevelopSettings::default());
        assert_eq!(r.global, super::super::auto_tone(&flat, &info, &DevelopSettings::default()));
        assert!(r.regions.is_empty(), "{:?}", r.regions);
        // hostile pixels never break it
        let bad = Rgb32f::from_fn(32, 32, |x, _| if x % 3 == 0 { [f32::NAN, -1.0, f32::INFINITY] } else { [0.2, 0.2, 0.2] });
        let _ = region_auto(&bad, &info, &DevelopSettings::default());
        let empty = Rgb32f::new(0, 0);
        assert!(region_auto(&empty, &info, &DevelopSettings::default()).regions.is_empty());
    }
}
