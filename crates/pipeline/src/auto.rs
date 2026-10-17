//! Auto tone and auto white balance (histogram / grey-world statistics on a proxy).
//!
//! Auto tone is deterministic and closed-loop: exposure sets the scene's key from its median
//! (an exact rule, since exposure is a plain gain before the tone curve), then the shaping
//! sliders (contrast, highlights, shadows, whites, blacks) are fitted against the output of a
//! model of the real tone stage ([`fit`]), so their values follow the pipeline's actual response
//! rather than a guess of it. Scene kinds (low-key, high-key, backlit) are continuous weights,
//! never branches: two frames of one burst that straddle a threshold get nearly the same values.

use lightcraft_color::cct::xy_to_temp_tint;
use lightcraft_color::perceptual::oklab_from_2020;
use lightcraft_color::{REC2020, Xy, bradford, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::Rgb32f;
use serde::{Deserialize, Serialize};

use crate::SourceInfo;
use crate::local::effective_wb;

/// Identity of the Auto rules below. Bump it when their output changes: offline receipts
/// (Personal Auto) bind baseline values to it.
pub const REVISION: &str = "lightcraft.deterministic-auto.v6";

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoTone {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub vibrance: f64,
    pub saturation: f64,
}

fn percentile(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let q = if q.is_finite() { q.clamp(0.0, 1.0) } else { 0.5 };
    sorted.get(((sorted.len() - 1) as f32 * q) as usize).copied().unwrap_or(0.0)
}

/// Smooth step: exactly 0 at `x <= -2·width`, exactly 1 at `x >= 2·width`, C¹ in between — so
/// scene weights are continuous across a threshold yet saturate (a cap is a cap once well
/// inside its region).
fn soft(x: f32, width: f32) -> f32 {
    let t = ((x + 2.0 * width) / (4.0 * width)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much a scene is low-key, high-key and backlit (each 0..=1), from its scene-EV percentiles
/// (log2 of luminance over middle grey, before exposure).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SceneKey {
    pub low: f32,
    pub high: f32,
    pub backlit: f32,
}

impl SceneKey {
    pub fn of(median: f32, p05: f32, p95: f32, p995: f32) -> SceneKey {
        // Keep deliberately dark and bright scenes in their intended key. A median-only target
        // turns a night frame grey and pushes snow/high-key frames too far down.
        let low = soft(-1.35 - median, 0.2) * soft(0.7 - p95, 0.25);
        let high = soft(median - 0.8, 0.2) * soft(p05 + 1.5, 0.25);
        let backlit = soft(p995 - 2.4, 0.25) * soft(-2.8 - p05, 0.25) * soft(p995 - median - 4.0, 0.3);
        let keyed = low.max(high);
        // low and high key exclude each other; the stronger one wins smoothly
        SceneKey { low: low * (1.0 - high * 0.5).min(1.0), high: high * (1.0 - low * 0.5).min(1.0), backlit: backlit * (1.0 - keyed * 0.5) }
    }
}

/// The scene as Auto sees it: the 512 px proxy's finite scene EVs (sorted) and Oklab chroma.
pub struct ScenePopulation {
    /// `log2(Y / 0.18)` of every finite, non-black proxy pixel, ascending.
    pub ev: Vec<f32>,
    /// Centre-weighted median: half the frame's, half the central ellipse's.
    pub median: f32,
    pub p05: f32,
    pub p95: f32,
    pub p995: f32,
    /// 90th-percentile Oklab chroma.
    pub chroma_p90: f32,
    /// Share (0..=1) of the proxy's channel samples that sit on a clipped plateau; 0 for
    /// non-raw sources unless supplied ([`AutoHints::clipped_share`]). Caps highlight recovery
    /// ([`fit::highlight_floor`]).
    pub clipped: f32,
}

/// A detected face as a fraction of the frame (0..=1, y down), the shape `lightcraft_faces::Face`
/// reports. Auto takes plain boxes so the pipeline does not depend on a detector or its weights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Optional facts Auto cannot measure from the proxy alone. `Default` changes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoHints<'a> {
    /// Detected faces: the exposure key follows them (see [`FACE_MIN_SHARE`]).
    pub faces: &'a [FaceBox],
    /// The decoder's own share of clipped channel samples, when it knows; raw sources otherwise
    /// get a plateau estimate from the proxy.
    pub clipped_share: Option<f32>,
}

/// Faces covering less than this share of the valid pixels (or fewer than `FACE_MIN_PIXELS`)
/// are too small to meter on and leave the key as it was.
pub const FACE_MIN_SHARE: f32 = 0.005;
const FACE_MIN_PIXELS: usize = 16;
/// Most boxes considered, and the furthest (EV) the face median may sit from the frame's before
/// it stops counting, so a mis-detection cannot move the key by more than half of this.
const MAX_FACES: usize = 64;
const FACE_MAX_OFFSET_EV: f32 = 2.5;
/// A channel only counts as clipped on a plateau at or above this (sensor white is 1.0 before
/// white balance; dark flat areas are not clipping), within `PLATEAU_TOL` of its maximum.
const PLATEAU_MIN: f32 = 0.9;
const PLATEAU_TOL: f32 = 0.995;

/// Squared radius (normalized: 1 at the frame's edge midpoints) of the central ellipse whose
/// median counts for half of the scene's key (about 28 % of the frame).
const CENTRE_R2: f32 = 0.36;

/// Measure `src` under `s`'s white balance (tone settings ignored), centre-weighted (see
/// [`CENTRE_R2`]). `None` for an empty or entirely invalid image.
pub fn scene_population(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<ScenePopulation> {
    scene_population_with(src, info, s, &AutoHints::default())
}

/// Share of channel samples on a clipped plateau: per channel, samples within `PLATEAU_TOL` of
/// the channel's maximum, when that maximum is at least `PLATEAU_MIN`. Non-finite samples are
/// skipped; 0 for an empty image.
fn plateau_share(img: &Rgb32f) -> f32 {
    let mut max = [0f32; 3];
    for p in &img.data {
        for (m, c) in max.iter_mut().zip(p) {
            if c.is_finite() && *c > *m {
                *m = *c;
            }
        }
    }
    let mut hit = 0usize;
    let mut total = 0usize;
    for p in &img.data {
        if !p.iter().all(|c| c.is_finite()) {
            continue;
        }
        total += 3;
        for (m, c) in max.iter().zip(p) {
            if *m >= PLATEAU_MIN && *c >= *m * PLATEAU_TOL {
                hit += 1;
            }
        }
    }
    if total == 0 { 0.0 } else { hit as f32 / total as f32 }
}

/// [`scene_population`] with optional [`AutoHints`]. With usable faces the key's median is half
/// the frame's, half the faces' (replacing the central ellipse's half); without them, exactly as
/// before.
pub fn scene_population_with(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, hints: &AutoHints) -> Option<ScenePopulation> {
    if src.width == 0 || src.height == 0 || src.data.is_empty() {
        return None;
    }
    let mut img = lightcraft_raster::resample::fit(src, 512, 512, lightcraft_raster::resample::Filter::Box);
    let clipped = match hints.clipped_share.or(info.clipped_share) {
        Some(c) if c.is_finite() => c.clamp(0.0, 1.0),
        Some(_) => 0.0,
        None if info.raw => plateau_share(&img),
        None => 0.0,
    };
    let faces: Vec<FaceBox> = hints
        .faces
        .iter()
        .filter(|f| [f.x0, f.y0, f.x1, f.y1].iter().all(|v| v.is_finite()) && f.x1 > f.x0 && f.y1 > f.y0)
        .take(MAX_FACES)
        .copied()
        .collect();
    let mut base = DevelopSettings { wb: s.wb, process: s.process, ..DevelopSettings::default() };
    base.light.exposure = 0.0;
    crate::local::scene_linear_pre(&mut img, info, &base);
    let (w, h) = (img.width.max(1) as f32, img.height.max(1) as f32);
    let mut ev = Vec::with_capacity(img.data.len());
    let mut centre = Vec::with_capacity(img.data.len() / 2);
    let mut face_ev = Vec::new();
    let mut chroma = Vec::with_capacity(img.data.len());
    for (i, c) in img.data.iter().enumerate() {
        let y = luminance_2020(*c);
        if !y.is_finite() || y <= 1e-6 {
            continue;
        }
        let e = (y / 0.18).log2();
        if !e.is_finite() {
            continue;
        }
        ev.push(e);
        let (px, py) = ((i % img.width) as f32 + 0.5, (i / img.width.max(1)) as f32 + 0.5);
        let (dx, dy) = ((px / w - 0.5) * 2.0, (py / h - 0.5) * 2.0);
        if dx * dx + dy * dy < CENTRE_R2 {
            centre.push(e);
        }
        let (fx, fy) = (px / w, py / h);
        if faces.iter().any(|f| fx >= f.x0 && fx < f.x1 && fy >= f.y0 && fy < f.y1) {
            face_ev.push(e);
        }
        let lab = oklab_from_2020(*c);
        let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
        if c.is_finite() {
            chroma.push(c);
        }
    }
    if ev.is_empty() {
        return None;
    }
    ev.sort_by(|a, b| a.total_cmp(b));
    centre.sort_by(|a, b| a.total_cmp(b));
    chroma.sort_by(|a, b| a.total_cmp(b));
    let global = percentile(&ev, 0.5);
    // Centre weighting: the key follows the subject as much as the frame (a dark subject
    // against a bright sky, a lit face in a dark room). Half the frame's median, half the
    // central ellipse's; without centre pixels (tiny images), the frame's.
    let mut median = if centre.is_empty() { global } else { 0.5 * global + 0.5 * percentile(&centre, 0.5) };
    // Faces: a lit face in a dark room (or a shaded one against a bright wall) is what the
    // viewer reads, so it takes the central ellipse's half of the key, bounded around the frame's.
    if face_ev.len() >= FACE_MIN_PIXELS && face_ev.len() as f32 >= FACE_MIN_SHARE * ev.len() as f32 {
        face_ev.sort_by(|a, b| a.total_cmp(b));
        let face = percentile(&face_ev, 0.5).clamp(global - FACE_MAX_OFFSET_EV, global + FACE_MAX_OFFSET_EV);
        median = 0.5 * global + 0.5 * face;
    }
    let (p05, p95, p995) = (percentile(&ev, 0.05), percentile(&ev, 0.95), percentile(&ev, 0.995));
    let chroma_p90 = percentile(&chroma, 0.9);
    Some(ScenePopulation { ev, median, p05, p95, p995, chroma_p90, clipped })
}

/// Compute auto tone values for `src` under the current white balance (ignores current tone values).
pub fn auto_tone(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> AutoTone {
    auto_tone_with(src, info, s, &AutoHints::default())
}

/// [`auto_tone`] with optional [`AutoHints`] (detected faces, the decoder's clipped share).
pub fn auto_tone_with(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, hints: &AutoHints) -> AutoTone {
    scene_population_with(src, info, s, hints).map(|pop| auto_tone_of(&pop, info)).unwrap_or_default()
}

/// How far under middle grey (EV) Auto keeps an ordinary scene's key median. The offset fades out
/// as a scene becomes low- or high-key (those keys carry their own targets). Measured on 7,524
/// finished photos (issue #28): Auto's median sat at 0.499 against the photographer's 0.371, 2.4x
/// as far from the camera render as the finals; 0.5 EV closes most of that gap without
/// underexposing (the 0.371 / 0.499 ratio is -0.43 EV in linear luminance, -0.9 EV on a 2.2 gamma
/// scale; 0.5 EV is the conservative end).
const ORDINARY_KEY_OFFSET: f32 = -0.5;

/// Scene EV (over middle grey) Auto aims a scene's key median at.
pub(crate) fn key_target(key: SceneKey) -> f32 {
    let keyed = key.low.max(key.high);
    -0.9 * key.low + 0.65 * key.high + ORDINARY_KEY_OFFSET * (1.0 - keyed)
}

/// Auto tone values for a measured scene ([`scene_population`], possibly with its `median`
/// replaced by a region-aware one).
pub fn auto_tone_of(pop: &ScenePopulation, info: &SourceInfo) -> AutoTone {
    let ScenePopulation { ev, median, p05, p95, p995, chroma_p90, clipped } = pop;
    let (ev, median, p05, p95, p995, chroma_p90) = (ev.as_slice(), *median, *p05, *p95, *p995, *chroma_p90);
    let key = SceneKey::of(median, p05, p95, p995);

    let target = key_target(key);
    let median_gain = 1.0 - 0.15 * key.low.max(key.high);
    // BaselineExposure has already been applied by the RAW loader. Keep an ordinary scene's
    // median on target instead of adding a second, undocumented underexposure bias here.
    let mut exposure = ((target - median) * median_gain).clamp(-4.0, 4.0);
    // Let highlight recovery work, but do not spend several stops on a dark foreground when a
    // small bright tail (sun, window, or lamp) defines the upper percentile.
    exposure -= key.backlit * (exposure - 2.0).max(0.0);
    let exposure = (exposure as f64 * 100.0).round() / 100.0;

    let shape = fit::fit_capped(ev, exposure as f32, key, info, *clipped);

    let colour_headroom = ((0.16 - chroma_p90) / 0.16).clamp(0.0, 1.0);
    let vibrance = (14.0 * colour_headroom).round();
    // Contrast raises rendered chroma: on a scene without colour headroom, take back what the
    // fitted contrast adds (v5's darker key fits more contrast; #28).
    let contrast_chroma = 0.2 * shape.contrast.max(0.0) as f32;
    let saturation = (3.0 - (8.0 + contrast_chroma) * (1.0 - colour_headroom)).round();
    AutoTone {
        exposure,
        contrast: shape.contrast,
        highlights: shape.highlights,
        shadows: shape.shadows,
        whites: shape.whites,
        blacks: shape.blacks,
        vibrance: vibrance as f64,
        saturation: saturation as f64,
    }
}

/// Closed-loop fit of the shaping sliders.
///
/// The scene's EV population (after exposure) is pushed through a per-pixel model of the finish
/// stage — the highlight/shadow log-luminance offsets exactly as `finish` applies them (with the
/// pixel's own luminance standing in for the edge-aware base plane) and the source's real
/// [`ToneMap`] — and the rendered histogram is compared with targets: a bright tail that reaches
/// white without clipping, blacks that reach near black, an ordinary mid-tone spread, and the key
/// exposure chose left where it is. Coordinate descent over a fixed slider order with a fixed
/// step schedule and fixed pass count makes the result a pure function of its inputs.
pub mod fit {
    use super::SceneKey;
    use crate::SourceInfo;
    use crate::tone::{GREY, LUT_MAX_EV, LUT_MIN_EV, ToneMap};

    /// Fitted slider values (integers, native slider units).
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Shape {
        pub contrast: f64,
        pub highlights: f64,
        pub shadows: f64,
        pub whites: f64,
        pub blacks: f64,
    }

    /// Sampled scene EVs after exposure, at most this many (a fixed stride over the sorted
    /// population keeps the sample's percentiles those of the whole).
    const SAMPLES: usize = 4096;
    const BINS: usize = 1024;
    const PASSES: usize = 4;
    /// Step schedule per pass (slider units); each pass refines the last.
    const STEPS: [f32; PASSES] = [16.0, 8.0, 4.0, 2.0];
    /// Furthest a slider moves in one pass (in steps).
    const MAX_MOVES: usize = 6;

    /// Auto's slider ranges: highlights only recover and blacks never lift: on 7,524 finished photos
    /// the photographer keeps or deepens the low end, and an Auto that lifted it moved the 10th
    /// percentile the right way on 46 % of them (#28). Shadows stay non-negative: a negative Shadows
    /// darkened saturated scenes into more rendered chroma; the 10th-percentile guard limits lifts.
    const AUTO_BOUNDS: [(f32, f32); 5] = [(-20.0, 30.0), (-90.0, 0.0), (0.0, 70.0), (-30.0, 40.0), (-35.0, 0.0)];
    /// Ranges for measured look targets ([`fit_with`]): a look may ask for lifted, faded blacks.
    const LOOK_BOUNDS: [(f32, f32); 5] = [(-20.0, 30.0), (-90.0, 0.0), (0.0, 70.0), (-30.0, 40.0), (-35.0, 20.0)];

    /// Keeps Auto from lifting the deep shadows: the rendered 10th percentile may not rise above
    /// where neutral shaping leaves it.
    #[derive(Clone, Copy)]
    struct Guard {
        /// Encoded 10th percentile under neutral shaping at the chosen exposure.
        p10: f32,
        weight: f32,
    }

    /// What the fit aims the rendered histogram at (all on the gamma-2.2 encoded scale). Auto
    /// derives them from the scene key; a look target ([`crate::look`]) supplies measured ones.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Targets {
        /// Encoded (sRGB-like) luminance of the 99.5th percentile.
        pub top: f32,
        /// Encoded luminance of the 1st percentile.
        pub bottom: f32,
        /// Encoded spread between the 5th and 95th percentiles.
        pub spread: f32,
        /// Encoded median.
        pub median: f32,
        /// Share of the frame allowed at or over white.
        pub clip: f32,
        /// Weights of the four terms above (clip and headroom are always weighted).
        pub w: [f32; 4],
    }

    fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// The perceptual scale targets live on (gamma 2.2; exact enough for histogram goals).
    pub fn encode(o: f32) -> f32 {
        o.clamp(0.0, 1.0).powf(1.0 / 2.2)
    }

    /// The tone map `finish` uses for this source kind.
    pub fn tone_map(info: &SourceInfo, contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        if let Some(curve) = info.camera_tone.as_ref().filter(|_| info.raw) {
            ToneMap::camera(curve, contrast, whites, blacks)
        } else if info.raw {
            ToneMap::new(contrast, whites, blacks)
        } else {
            ToneMap::display(contrast, whites, blacks)
        }
    }

    /// Rendered statistics of `ev` under slider values `v` (contrast, highlights, shadows,
    /// whites, blacks).
    struct Stats {
        /// Encoded output luminance at the 1st, 5th, 10th, 50th, 95th and 99.5th percentiles.
        p: [f32; 6],
        /// Share of the frame at or over white.
        clip: f32,
        /// 99.5th percentile of log luminance after the highlight/shadow offsets, before the
        /// tone map (which saturates: this stays informative where the output cannot).
        top_ev: f32,
    }

    fn stats(ev: &[f32], v: [f32; 5], tone: &ToneMap) -> Stats {
        let (hl, sh) = (v[1] / 100.0, v[2] / 100.0);
        let mut hist = [0u32; BINS];
        let mut ev_hist = [0u32; BINS];
        let mut clipped = 0u32;
        for &e in ev {
            let mut l = e;
            if hl != 0.0 || sh != 0.0 {
                let ws = 1.0 - smooth(-4.8, 0.3, l);
                let wh = smooth(-1.0, 2.8, l);
                l += sh * 1.7 * ws * ws.sqrt() + hl * 1.7 * wh;
            }
            let o = tone.apply(GREY * l.exp2());
            if o >= 0.985 {
                clipped += 1;
            }
            let b = ((encode(o) * (BINS - 1) as f32).round() as usize).min(BINS - 1);
            if let Some(h) = hist.get_mut(b) {
                *h += 1;
            }
            let eb = (((l - LUT_MIN_EV) / (LUT_MAX_EV - LUT_MIN_EV)).clamp(0.0, 1.0) * (BINS - 1) as f32).round() as usize;
            if let Some(h) = ev_hist.get_mut(eb.min(BINS - 1)) {
                *h += 1;
            }
        }
        let n = ev.len().max(1) as f32;
        let mut p = [0f32; 6];
        let mut acc = 0u32;
        let mut k = 0;
        let qs = [0.01, 0.05, 0.10, 0.5, 0.95, 0.995];
        for (i, h) in hist.iter().enumerate() {
            acc += h;
            while k < 6 && acc as f32 >= qs[k] * n {
                p[k] = i as f32 / (BINS - 1) as f32;
                k += 1;
            }
        }
        let mut acc = 0u32;
        let mut top_ev = LUT_MAX_EV;
        for (i, h) in ev_hist.iter().enumerate() {
            acc += h;
            if acc as f32 >= 0.995 * n {
                top_ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (BINS - 1) as f32;
                break;
            }
        }
        Stats { p, clip: clipped as f32 / n, top_ev }
    }

    /// Scene EV (over grey) at which `tone` reaches white.
    fn white_ev(tone: &ToneMap) -> f32 {
        let lut = tone.lut();
        let i = lut.iter().position(|o| *o >= 0.985).unwrap_or(lut.len().saturating_sub(1));
        LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (lut.len().max(2) - 1) as f32
    }

    fn cost(ev: &[f32], v: [f32; 5], info: &SourceInfo, t: &Targets, guard: Option<Guard>) -> f32 {
        let tone = tone_map(info, v[0] as f64, v[3] as f64, v[4] as f64);
        let Stats { p, clip, top_ev } = stats(ev, v, &tone);
        let terms = [p[5] - t.top, p[0] - t.bottom, (p[4] - p[1]) - t.spread, p[3] - t.median];
        let mut c = 0.0;
        for (d, w) in terms.iter().zip(t.w) {
            c += w * d * d;
        }
        c += 40.0 * (clip - t.clip).max(0.0).powi(2);
        if let Some(g) = guard {
            c += g.weight * (p[2] - g.p10).max(0.0).powi(2);
        }
        // the bright tail should sit under the tone map's white point (in EV, so this keeps
        // pulling while the output is saturated)
        c += t.w[0] * 0.25 * (top_ev - (white_ev(&tone) - 0.2)).max(0.0).powi(2);
        // keep sliders modest where they buy nothing (and break ties deterministically)
        for (x, reg) in v.iter().zip([0.03, 0.006, 0.008, 0.012, 0.012]) {
            c += reg * (x / 100.0).powi(2);
        }
        c
    }

    fn sample(ev_sorted: &[f32], exposure: f32) -> Vec<f32> {
        let n = ev_sorted.len();
        let stride = n.div_ceil(SAMPLES).max(1);
        (0..n).step_by(stride).filter_map(|i| ev_sorted.get(i)).map(|e| e + exposure).collect()
    }

    /// Auto's targets for a scene of this key. The median target is the scene's own rendered
    /// median under neutral shaping: where exposure put it, the fit leaves it.
    pub fn auto_targets(ev_sorted: &[f32], exposure: f32, key: SceneKey, info: &SourceInfo) -> Targets {
        let ev = sample(ev_sorted, exposure);
        let neutral = tone_map(info, 0.0, 0.0, 0.0);
        let p0 = stats(&ev, [0.0; 5], &neutral).p;
        let keyed = key.low.max(key.high);
        Targets {
            top: 0.95 - 0.12 * key.low,
            bottom: 0.05 + 0.08 * key.high,
            spread: 0.62 - 0.18 * keyed - 0.1 * key.backlit,
            median: p0[3],
            clip: 0.003 + 0.03 * key.high,
            w: [1.0 - 0.6 * key.low, 1.0 - 0.6 * key.high, 0.25 * (1.0 - 0.5 * keyed - 0.4 * key.backlit), 2.0],
        }
    }

    pub fn fit(ev_sorted: &[f32], exposure: f32, key: SceneKey, info: &SourceInfo) -> Shape {
        fit_capped(ev_sorted, exposure, key, info, 0.0)
    }

    /// [`fit`] with highlight recovery capped by the share of clipped channel samples
    /// ([`highlight_floor`]); 0 is [`fit`] exactly.
    pub fn fit_capped(ev_sorted: &[f32], exposure: f32, key: SceneKey, info: &SourceInfo, clipped: f32) -> Shape {
        if ev_sorted.is_empty() {
            return Shape::default();
        }
        let t = auto_targets(ev_sorted, exposure, key, info);
        fit_guarded_capped(ev_sorted, exposure, &t, key, info, clipped)
    }

    /// Lowest Highlights value Auto may fit given the share of clipped channel samples. Clipped
    /// channels carry no detail to recover: pulling Highlights down on them only greys the
    /// reconstructed neutral. Full range (-90) up to 0.5 % clipped, falling linearly to a fifth of
    /// it (-18) from 10 %, continuous in between; non-finite shares count as none.
    pub fn highlight_floor(clipped: f32) -> f32 {
        let c = if clipped.is_finite() { clipped } else { 0.0 };
        let t = ((c - 0.005) / 0.095).clamp(0.0, 1.0);
        AUTO_BOUNDS[1].0 * (1.0 - 0.8 * t)
    }

    /// Auto's own fit toward `t` (from [`auto_targets`], possibly reweighted): [`AUTO_BOUNDS`]
    /// and a guard that keeps the deep shadows from rising above neutral shaping (backlit scenes,
    /// whose dark foreground wants lifting, relax it).
    pub fn fit_guarded(ev_sorted: &[f32], exposure: f32, t: &Targets, key: SceneKey, info: &SourceInfo) -> Shape {
        fit_guarded_capped(ev_sorted, exposure, t, key, info, 0.0)
    }

    /// [`fit_guarded`] with the Highlights range capped for a clipped share ([`highlight_floor`]).
    pub fn fit_guarded_capped(ev_sorted: &[f32], exposure: f32, t: &Targets, key: SceneKey, info: &SourceInfo, clipped: f32) -> Shape {
        if ev_sorted.is_empty() || !exposure.is_finite() {
            return Shape::default();
        }
        let neutral = tone_map(info, 0.0, 0.0, 0.0);
        let p10 = stats(&sample(ev_sorted, exposure), [0.0; 5], &neutral).p[2];
        let weight = 8.0 * (1.0 - key.backlit).clamp(0.0, 1.0);
        let mut bounds = AUTO_BOUNDS;
        bounds[1].0 = highlight_floor(clipped);
        fit_core(ev_sorted, exposure, t, info, &bounds, Some(Guard { p10, weight }))
    }

    /// Fit the shaping sliders of `ev_sorted` (scene EVs before exposure) toward `t` (a measured
    /// look: lifted blacks allowed, no shadow guard).
    pub fn fit_with(ev_sorted: &[f32], exposure: f32, t: &Targets, info: &SourceInfo) -> Shape {
        fit_core(ev_sorted, exposure, t, info, &LOOK_BOUNDS, None)
    }

    fn fit_core(ev_sorted: &[f32], exposure: f32, t: &Targets, info: &SourceInfo, bounds: &[(f32, f32); 5], guard: Option<Guard>) -> Shape {
        if ev_sorted.is_empty() || !exposure.is_finite() {
            return Shape::default();
        }
        let ev = sample(ev_sorted, exposure);
        let t = &Targets { w: t.w.map(|w| if w.is_finite() { w.max(0.0) } else { 0.0 }), ..*t };
        let mut v = [0f32; 5];
        let mut best = cost(&ev, v, info, t, guard);
        for step in STEPS {
            // fixed order: the clipping sliders first, then the range, then contrast
            for i in [1usize, 2, 3, 4, 0] {
                for dir in [-1.0f32, 1.0] {
                    for _ in 0..MAX_MOVES {
                        let mut trial = v;
                        let Some(x) = trial.get_mut(i) else { break };
                        let Some(&(lo, hi)) = bounds.get(i) else { break };
                        let next = (*x + dir * step).clamp(lo, hi);
                        if next == *x {
                            break;
                        }
                        *x = next;
                        let c = cost(&ev, trial, info, t, guard);
                        if c < best - 1e-9 {
                            best = c;
                            v = trial;
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        let r = |x: f32| x.round() as f64;
        Shape { contrast: r(v[0]), highlights: r(v[1]), shadows: r(v[2]), whites: r(v[3]), blacks: r(v[4]) }
    }
}

/// An automatic black & white mix (slider values, red … magenta) for `src` under `s`'s white
/// balance and tone: each hue band's colourful pixels are pushed away from the image's mean
/// lightness — bands brighter than average get brighter, darker ones darker — so areas that
/// differ only in colour stay apart in grey. Bands with almost no colourful pixels stay at 0.
pub fn auto_bw_mix(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> [f64; 8] {
    use lightcraft_color::perceptual::{lab_to_lch, oklab_from_2020};
    let mut img = lightcraft_raster::resample::fit(src, 512, 512, lightcraft_raster::resample::Filter::Box);
    let base = DevelopSettings { wb: s.wb, light: s.light, ..DevelopSettings::default() };
    crate::local::scene_linear_pre(&mut img, info, &base);
    let gain = 2f32.powf(base.light.exposure as f32);
    let (mut mass, mut sum_l) = ([0f64; 8], [0f64; 8]);
    let (mut all_l, mut n) = (0f64, 0usize);
    for p in &img.data {
        let lch = lab_to_lch(oklab_from_2020(p.map(|v| (v * gain).max(0.0))));
        all_l += lch[0] as f64;
        n += 1;
        let k = (lch[1] / 0.2).min(1.0) as f64; // as in the B&W conversion
        if k < 0.05 {
            continue;
        }
        let w = crate::colorops::band_weights(lch[2]);
        for i in 0..8 {
            mass[i] += w[i] as f64 * k;
            sum_l[i] += w[i] as f64 * k * lch[0] as f64;
        }
    }
    if n == 0 {
        return [0.0; 8];
    }
    let mean = all_l / n as f64;
    let total: f64 = mass.iter().sum();
    std::array::from_fn(|i| {
        if total <= 0.0 || mass[i] / total < 0.01 {
            return 0.0;
        }
        let sep = sum_l[i] / mass[i] - mean;
        (sep * 400.0).clamp(-60.0, 60.0).round()
    })
}

/// Power of the low-chroma preference in [`neutral_wb`]'s pixel weights.
const NEUTRAL_POWER: i32 = 4;
/// Largest tint Auto WB proposes: real illuminants sit near the Planckian / daylight locus; a
/// bigger green–magenta estimate is a coloured scene, not a coloured light.
const MAX_TINT: f64 = 60.0;

/// The white balance that makes `src` neutral: a grey-world mean weighted towards mid-tone,
/// low-chroma pixels (a large coloured surface barely counts), as (temp, tint), unbounded.
/// The picker uses it on a sampled patch, which it neutralizes exactly.
pub fn neutral_wb(src: &Rgb32f, info: &SourceInfo) -> (f64, f64) {
    let img = lightcraft_raster::resample::fit(src, 256, 256, lightcraft_raster::resample::Filter::Box);
    let (mut acc, mut wsum) = ([0.0f64; 3], 0.0f64);
    for c in &img.data {
        if !c.iter().all(|v| v.is_finite() && *v >= 0.0) {
            continue;
        }
        let y = luminance_2020(*c);
        if !(0.01..=2.0).contains(&y) {
            continue;
        }
        let mx = c[0].max(c[1]).max(c[2]);
        let mn = c[0].min(c[1]).min(c[2]);
        let chroma = (mx - mn) / (mx + 1e-6);
        // (1 − chroma)⁴: a coloured surface's vote is ~30× below a cast grey's (a cast grey
        // reads as chroma 0.3–0.4 as shot), so a wall or a field that fills most of the frame
        // still loses to the neutrals that are there
        let w = (1.0 - chroma).powi(NEUTRAL_POWER) as f64 * (1.0 - ((y.log2() + 2.5) / 4.0).abs().min(1.0)) as f64;
        for i in 0..3 {
            acc[i] += c[i] as f64 * w;
        }
        wsum += w;
    }
    if wsum <= 0.0 {
        return (info.as_shot_temp, info.as_shot_tint);
    }
    let avg = acc.map(|v| v / wsum);
    if !avg.iter().all(|v| v.is_finite() && *v > 0.0) {
        return (info.as_shot_temp, info.as_shot_tint);
    }
    let camera = info.camera_color.as_deref().filter(|_| info.raw && !info.relative_wb);
    let seen = match camera.and_then(|cc| lightcraft_raw::color::neutral_white(&cc.tags, cc.developed_for, avg)) {
        Some(white) => white,
        None => {
            let shot = lightcraft_color::cct::temp_tint_to_xy(info.as_shot_temp, info.as_shot_tint);
            Xy::from_xyz(bradford(REC2020.white, shot).apply(REC2020.to_xyz().apply(avg)))
        }
    };
    let (t, tint) = xy_to_temp_tint(seen);
    let t = if t.is_finite() { t.clamp(2000.0, 50000.0).round() } else { info.as_shot_temp };
    let tint = if tint.is_finite() { tint.clamp(-150.0, 150.0).round() } else { info.as_shot_tint };
    (t, tint)
}

/// Auto white balance of a whole scene: [`neutral_wb`] with the tint held near the illuminant
/// locus ([`MAX_TINT`]): a green field or a magenta wall is a coloured scene, not a coloured
/// light, and a scene-wide estimate must not neutralize it.
pub fn auto_wb(src: &Rgb32f, info: &SourceInfo) -> (f64, f64) {
    let (t, tint) = neutral_wb(src, info);
    (t, tint.clamp(-MAX_TINT, MAX_TINT))
}

/// Temperature/tint currently in effect (for UI display).
pub fn current_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    effective_wb(info, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_image_gets_positive_exposure() {
        let img = Rgb32f::from_fn(64, 64, |x, _| [0.01 + x as f32 * 0.0003; 3]);
        let a = auto_tone(&img, &SourceInfo::default(), &DevelopSettings::default());
        assert!(a.exposure > 1.5, "{a:?}");
        let bright = Rgb32f::from_fn(64, 64, |x, _| [0.8 + x as f32 * 0.01; 3]);
        let b = auto_tone(&bright, &SourceInfo::default(), &DevelopSettings::default());
        assert!(b.exposure < -1.0, "{b:?}");
    }

    #[test]
    fn neutral_image_keeps_as_shot_wb() {
        let img = Rgb32f::from_fn(32, 32, |x, y| [0.05 + (x + y) as f32 * 0.004; 3]);
        let (t, tint) = auto_wb(&img, &SourceInfo::default());
        assert!((t - 6500.0).abs() < 150.0, "{t}");
        assert!(tint.abs() < 6.0, "{tint}");
    }

    #[test]
    fn blue_cast_is_corrected_by_higher_temp() {
        // A bluish cast should be neutralised by telling the pipeline the light was bluer (higher K).
        let img = Rgb32f::from_fn(32, 32, |_, _| [0.16, 0.18, 0.24]);
        let (t, _) = auto_wb(&img, &SourceInfo::default());
        assert!(t > 7000.0, "{t}");
        let mut s = DevelopSettings::default();
        s.wb.mode = lightcraft_develop::WbMode::Custom;
        s.wb.temp = t;
        let (t2, tint2) = auto_wb(&img, &SourceInfo::default());
        let _ = (t2, tint2);
        let mut out = img.clone();
        let (tt, ti) = auto_wb(&img, &SourceInfo::default());
        s.wb.temp = tt;
        s.wb.tint = ti;
        crate::local::scene_linear_pre(&mut out, &SourceInfo::default(), &s);
        let c = out.get(0, 0);
        assert!((c[0] - c[2]).abs() < 0.02, "{c:?}");
    }
}

#[cfg(test)]
mod bw_tests {
    use super::*;

    #[test]
    fn auto_bw_mix_pushes_light_and_dark_hues_apart() {
        // left: a light yellow, right: a dark blue (scene-linear Rec. 2020)
        let src = Rgb32f::from_fn(64, 32, |x, _| if x < 32 { [0.55, 0.5, 0.05] } else { [0.01, 0.02, 0.12] });
        let m = auto_bw_mix(&src, &SourceInfo::default(), &DevelopSettings::default());
        let (yellow, blue) = (m[2], m[5]);
        assert!(yellow > 0.0 && blue < 0.0, "{m:?}");
        // an image without colour leaves the mix alone
        let grey = Rgb32f::from_fn(16, 16, |x, _| [x as f32 / 16.0; 3]);
        assert_eq!(auto_bw_mix(&grey, &SourceInfo::default(), &DevelopSettings::default()), [0.0; 8]);
    }
}

pub mod regions;

#[cfg(test)]
mod auto_regression;

#[cfg(test)]
mod tint_tests {
    use super::*;

    #[test]
    fn auto_wb_and_picker_correct_green_with_positive_tint() {
        // The picker delegates a sampled patch to neutral_wb (exact, unbounded).
        for (rgb, sign) in [([0.18, 0.24, 0.18], 1.0), ([0.24, 0.18, 0.24], -1.0)] {
            let img = Rgb32f::filled(16, 16, rgb);
            let info = SourceInfo { raw: true, relative_wb: true, ..Default::default() };
            let (temp, tint) = neutral_wb(&img, &info);
            assert!(tint * sign > 0.0, "{rgb:?}: temp {temp}, tint {tint}");
            let mut s = DevelopSettings::default();
            s.wb.mode = lightcraft_develop::WbMode::Custom;
            s.wb.temp = temp;
            s.wb.tint = tint;
            let mut corrected = img;
            crate::local::white_balance(&mut corrected, &info, &s);
            let p = corrected.get(0, 0);
            let spread = p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
            assert!(spread < 0.003, "{rgb:?} -> {p:?}");
        }
    }
}
