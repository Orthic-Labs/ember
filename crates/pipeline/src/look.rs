//! Look targets: the rendered statistics of sample photos, applied to another photo by refitting
//! Auto toward them.
//!
//! A preset copies slider values; a look target copies the *result*. [`extract`] measures what
//! a set of sample renders have in common (where their tones sit, how much headroom they keep,
//! how colourful they are) and [`apply`] runs the closed-loop fit Auto uses
//! ([`crate::auto::fit`]) with those statistics as the targets, then renders the proxy through
//! the real pipeline to correct exposure, so every photo gets its own slider values yet lands on
//! the same histogram shape. Deterministic & resolution-independent.
//!
//! Spatial statistics (#203): a target also carries a 3 x 3 grid of cell medians over the
//! normalized frame, and [`Weighting`] says whether a match uses that grid (and, with a photo's
//! detected faces, weights the cells the faces cover). Off by default, so a target written before
//! the grid existed matches exactly as it always did.

use lightcraft_color::perceptual::oklab_from_2020;
use lightcraft_color::transfer::decode_srgb8;
use lightcraft_color::{REC2020, SRGB, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::{Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};

use crate::auto::fit::{Targets, encode, fit_with, tone_map};
use crate::auto::{AutoTone, FACE_MIN_SHARE, FaceBox, SceneKey, scene_population};
use crate::output::{DeepImage, DeepSamples, OutputDepth, OutputSpace};
use crate::{RenderRequest, SourceInfo};

pub const SCHEMA: &str = "lightcraft.look-target.v1";

/// Cells per side of the spatial grid (row-major, top-left first).
pub const GRID: usize = 3;
/// Number of grid cells.
pub const CELLS: usize = GRID * GRID;
/// With face weighting, a cell covered entirely by a face weighs `1 + FACE_WEIGHT` times a plain cell.
const FACE_WEIGHT: f32 = 3.0;
/// Most face boxes a match considers (as Auto does).
const MAX_FACES: usize = 64;
/// Sampling lattice (per side) the face coverage of each cell is measured on: normalized, so the
/// result does not depend on the photo's resolution.
const FACE_LATTICE: usize = 96;

/// What a set of sample renders have in common (all on the gamma-2.2 encoded output scale).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookTarget {
    pub schema: String,
    /// Samples the target was extracted from.
    pub samples: usize,
    /// Encoded output luminance at the 1st, 5th, 50th, 95th and 99.5th percentiles.
    pub luminance: [f32; 5],
    /// Share of the frame at or over white.
    pub clip: f32,
    /// Oklab chroma of the output at the 50th and 90th percentiles.
    pub chroma: [f32; 2],
    /// Per-cell statistics of the 3 x 3 grid (absent in targets written before #203).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<Grid>,
    /// How a photo is matched to this target. The default (off) is the frame-wide match.
    #[serde(default)]
    pub weighting: Weighting,
}

/// Per-cell medians over the 3 x 3 grid of the normalized frame, row-major from the top-left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    /// Per cell: encoded output luminance median, then Oklab chroma median.
    pub cells: [[f32; 2]; CELLS],
}

/// How a photo is matched to a look target. Both switches default to off, so existing targets keep
/// their frame-wide match; a target file enables them with `"weighting": {"spatial": true, "faces": true}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Weighting {
    /// Match the mean of the grid's cell medians instead of the frame-wide median.
    pub spatial: bool,
    /// With `spatial`, weight the cells the photo's detected faces cover (no faces: no change).
    pub faces: bool,
}

impl LookTarget {
    /// Rejects targets that could not have come from [`extract`] (hostile JSON).
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!("unsupported look target schema `{}`", self.schema));
        }
        if self.samples == 0 {
            return Err("a look target needs at least one sample".into());
        }
        let l = &self.luminance;
        if !l.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) || l.windows(2).any(|w| w[1] < w[0]) {
            return Err("look target luminance percentiles must be finite, 0..=1 and ascending".into());
        }
        if !self.clip.is_finite() || !(0.0..=1.0).contains(&self.clip) {
            return Err("look target clip share must be finite and 0..=1".into());
        }
        if !self.chroma.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) || self.chroma[1] < self.chroma[0] {
            return Err("look target chroma must be finite, 0..=1 and ascending".into());
        }
        if let Some(g) = &self.grid
            && !g.cells.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        {
            return Err("look target grid must be finite and 0..=1".into());
        }
        Ok(())
    }
}

/// Statistics of one rendered image: encoded luminance percentiles (1, 5, 50, 95, 99.5),
/// clip share, Oklab chroma percentiles (50, 90).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stats {
    luminance: [f32; 5],
    clip: f32,
    chroma: [f32; 2],
}

/// Statistics of a display-linear Rec. 2020 image (what the pipeline renders, before the output
/// transfer). `None` for an empty image.
fn stats_linear(img: &Rgb32f) -> Option<Stats> {
    if img.width == 0 || img.height == 0 || img.data.is_empty() {
        return None;
    }
    let mut lum = Vec::with_capacity(img.data.len());
    let mut chroma = Vec::with_capacity(img.data.len());
    let mut clipped = 0usize;
    for &lin in &img.data {
        let y = luminance_2020(lin);
        if !y.is_finite() {
            continue;
        }
        if y >= 0.985 {
            clipped += 1;
        }
        lum.push(encode(y));
        let lab = oklab_from_2020(lin.map(|v| v.max(0.0)));
        let c = lab[1].hypot(lab[2]);
        if c.is_finite() {
            chroma.push(c);
        }
    }
    if lum.is_empty() {
        return None;
    }
    lum.sort_by(f32::total_cmp);
    chroma.sort_by(f32::total_cmp);
    let q = |v: &[f32], q: f32| v.get(((v.len().saturating_sub(1)) as f32 * q) as usize).copied().unwrap_or(0.0);
    let luminance = [q(&lum, 0.01), q(&lum, 0.05), q(&lum, 0.5), q(&lum, 0.95), q(&lum, 0.995)];
    Some(Stats { luminance, clip: clipped as f32 / lum.len() as f32, chroma: [q(&chroma, 0.5), q(&chroma, 0.9)] })
}

/// Grid cell of the normalized point (`fx`, `fy`, 0..=1, y down).
fn cell_index(fx: f32, fy: f32) -> usize {
    let col = ((fx * GRID as f32) as usize).min(GRID - 1);
    let row = ((fy * GRID as f32) as usize).min(GRID - 1);
    row * GRID + col
}

/// Median of `v` (the lower middle for an even count, as the frame statistics take it); 0 when empty.
fn median_of(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get((v.len().saturating_sub(1)) / 2).copied().unwrap_or(0.0)
}

/// Per-cell encoded luminance and chroma medians of a display-linear image. A cell without pixels
/// takes the frame's median luminance. `None` for an empty image.
fn grid_linear(img: &Rgb32f) -> Option<Grid> {
    if img.width == 0 || img.height == 0 || img.data.is_empty() {
        return None;
    }
    let (w, h) = (img.width as f32, img.height as f32);
    let mut lum: Vec<Vec<f32>> = vec![Vec::new(); CELLS];
    let mut chroma: Vec<Vec<f32>> = vec![Vec::new(); CELLS];
    let mut frame = Vec::with_capacity(img.data.len());
    for (i, &lin) in img.data.iter().enumerate() {
        let y = luminance_2020(lin);
        if !y.is_finite() {
            continue;
        }
        let cell = cell_index(((i % img.width) as f32 + 0.5) / w, ((i / img.width) as f32 + 0.5) / h);
        let e = encode(y);
        lum[cell].push(e);
        frame.push(e);
        let lab = oklab_from_2020(lin.map(|v| v.max(0.0)));
        let c = lab[1].hypot(lab[2]);
        if c.is_finite() {
            chroma[cell].push(c);
        }
    }
    if frame.is_empty() {
        return None;
    }
    let frame_median = median_of(frame);
    let cells = std::array::from_fn(|i| {
        let l = if lum[i].is_empty() { frame_median } else { median_of(std::mem::take(&mut lum[i])) };
        [l, median_of(std::mem::take(&mut chroma[i]))]
    });
    Some(Grid { cells })
}

/// The share (0..=1) of each cell the faces cover, measured on a normalized lattice. All zero
/// unless the usable faces cover at least [`FACE_MIN_SHARE`] of the frame (as Auto's key does).
fn face_shares(faces: &[FaceBox]) -> [f32; CELLS] {
    let usable: Vec<FaceBox> = faces
        .iter()
        .filter(|f| [f.x0, f.y0, f.x1, f.y1].iter().all(|v| v.is_finite()) && f.x1 > f.x0 && f.y1 > f.y0)
        .take(MAX_FACES)
        .copied()
        .collect();
    let mut hit = [0usize; CELLS];
    let mut seen = [0usize; CELLS];
    for sy in 0..FACE_LATTICE {
        for sx in 0..FACE_LATTICE {
            let (fx, fy) = ((sx as f32 + 0.5) / FACE_LATTICE as f32, (sy as f32 + 0.5) / FACE_LATTICE as f32);
            let cell = cell_index(fx, fy);
            seen[cell] += 1;
            if usable.iter().any(|f| fx >= f.x0 && fx < f.x1 && fy >= f.y0 && fy < f.y1) {
                hit[cell] += 1;
            }
        }
    }
    let total: usize = hit.iter().sum();
    if (total as f32) < FACE_MIN_SHARE * (FACE_LATTICE * FACE_LATTICE) as f32 {
        return [0.0; CELLS];
    }
    std::array::from_fn(|c| hit[c] as f32 / seen[c].max(1) as f32)
}

/// The frame-wide median a match aims at. The look's own median, unless spatial weighting is on:
/// then the mean of its cell medians, each cell weighted by `1 + FACE_WEIGHT * share`, where the
/// share is how much of that cell this photo's faces cover (plain cells when face weighting is off
/// or there are no faces). The look's grid and this photo's faces sit at the same normalized places.
fn look_median(look: &LookTarget, faces: &[FaceBox]) -> f32 {
    let Some(grid) = look.grid.as_ref().filter(|_| look.weighting.spatial) else { return look.luminance[2] };
    let shares = if look.weighting.faces { face_shares(faces) } else { [0.0; CELLS] };
    let (mut sum, mut total) = (0.0f32, 0.0f32);
    for (cell, share) in grid.cells.iter().zip(shares) {
        let w = 1.0 + FACE_WEIGHT * share;
        sum += w * cell[0];
        total += w;
    }
    sum / total
}

/// An sRGB 8-bit render (a JPEG / PNG the user exported) as display-linear Rec. 2020.
fn srgb8_to_linear(img: &Rgba8) -> Rgb32f {
    let to_2020 = SRGB.to_space(&REC2020);
    Rgb32f::from_fn(img.width, img.height, |x, y| {
        let p = img.get(x, y);
        to_2020.apply_f32([decode_srgb8(p[0]), decode_srgb8(p[1]), decode_srgb8(p[2])])
    })
}

fn target_of(stats: &[Stats], grids: &[Grid]) -> Option<LookTarget> {
    if stats.is_empty() || grids.len() != stats.len() {
        return None;
    }
    // each statistic is the median across samples, so one odd frame does not pull the look
    let median = |f: &dyn Fn(&Stats) -> f32| {
        let mut v: Vec<f32> = stats.iter().map(f).collect();
        v.sort_by(f32::total_cmp);
        v.get((v.len() - 1) / 2).copied().unwrap_or(0.0)
    };
    let mut luminance = [0f32; 5];
    for (i, l) in luminance.iter_mut().enumerate() {
        *l = median(&|s| s.luminance.get(i).copied().unwrap_or(0.0));
    }
    for i in 1..5 {
        luminance[i] = luminance[i].max(luminance[i - 1]);
    }
    let chroma_lo = median(&|s| s.chroma[0]);
    let chroma = [chroma_lo, median(&|s| s.chroma[1]).max(chroma_lo)];
    // each grid cell is the median across samples too
    let cells = std::array::from_fn(|i| {
        [median_of(grids.iter().map(|g| g.cells[i][0]).collect()), median_of(grids.iter().map(|g| g.cells[i][1]).collect())]
    });
    let t = LookTarget {
        schema: SCHEMA.into(),
        samples: stats.len(),
        luminance,
        clip: median(&|s| s.clip),
        chroma,
        grid: Some(Grid { cells }),
        weighting: Weighting::default(),
    };
    t.validate().ok().map(|_| t)
}

/// Statistics and grid of one display-linear sample (`None` for an empty one).
fn measure_linear(img: &Rgb32f) -> Option<(Stats, Grid)> {
    Some((stats_linear(img)?, grid_linear(img)?))
}

/// The look target of `samples`: the user's finished photos as sRGB 8-bit renders. `None`
/// without a usable sample.
pub fn extract(samples: &[Rgba8]) -> Option<LookTarget> {
    let (stats, grids): (Vec<_>, Vec<_>) = samples.iter().filter_map(|s| measure_linear(&srgb8_to_linear(s))).unzip();
    target_of(&stats, &grids)
}

/// [`extract`] for samples already decoded to the display-linear working space (what
/// `lightcraft_codecs::Decoded::to_working` gives for a rendered file).
pub fn extract_linear(samples: &[Rgb32f]) -> Option<LookTarget> {
    let (stats, grids): (Vec<_>, Vec<_>) = samples.iter().filter_map(measure_linear).unzip();
    target_of(&stats, &grids)
}

/// The exposure (−4..=4 EV) that puts the scene median at the look's median through the neutral
/// tone map (bisection: the tone map is monotone).
fn exposure_for_median(median_ev: f32, target: f32, info: &SourceInfo) -> f32 {
    let tone = tone_map(info, 0.0, 0.0, 0.0);
    let rendered = |exposure: f32| encode(tone.apply(crate::tone::GREY * (median_ev + exposure).exp2()));
    let (mut lo, mut hi) = (-4.0f32, 4.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if rendered(mid) < target { lo = mid } else { hi = mid }
    }
    0.5 * (lo + hi)
}

/// A float linear Rec. 2020 deep render as an image (`None` for any other sample format).
pub(crate) fn deep_to_linear(d: &DeepImage) -> Option<Rgb32f> {
    let DeepSamples::F32(v) = &d.samples else { return None };
    if d.space != OutputSpace::Rec2020 || v.len() != d.width * d.height * 3 {
        return None;
    }
    let mut out = Rgb32f::new(d.width, d.height);
    for (p, c) in out.data.iter_mut().zip(v.as_chunks::<3>().0) {
        *p = *c;
    }
    Some(out)
}

fn round2(v: f32) -> f64 {
    ((v as f64) * 100.0).round() / 100.0
}

/// Bisection steps on exposure against the real render (0.008 EV resolution over −4..=4).
const EXPOSURE_STEPS: usize = 10;

/// Auto values that take `src` (under `s`'s other settings: white balance, profile, crop…) to
/// `look`. Scene-correction logic is Auto's own fit; only its targets change. The key weights
/// still soften the contrast term so a night frame is not forced to a daylight spread. Exposure
/// is then settled against the real pipeline: the 512 px proxy is rendered with the fitted
/// shaping and exposure bisected until the rendered median meets the look's.
pub fn apply(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, look: &LookTarget) -> Result<AutoTone, String> {
    apply_with(src, info, s, look, &[])
}

/// [`apply`] with the photo's detected `faces` (fractions of the frame). They only matter when the
/// look's [`Weighting::faces`] is on; otherwise the result is exactly [`apply`]'s.
pub fn apply_with(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, look: &LookTarget, faces: &[FaceBox]) -> Result<AutoTone, String> {
    look.validate()?;
    let Some(pop) = scene_population(src, info, s) else { return Ok(AutoTone::default()) };
    let key = SceneKey::of(pop.median, pop.p05, pop.p95, pop.p995);
    let keyed = key.low.max(key.high);
    // the frame-wide median the match aims at (the look's own unless spatial weighting is on)
    let want = look_median(look, faces);
    let t = Targets {
        top: look.luminance[4],
        bottom: look.luminance[0],
        spread: look.luminance[3] - look.luminance[1],
        median: want,
        clip: look.clip.max(0.002),
        w: [1.0, 1.0, 0.5 * (1.0 - 0.5 * keyed), 2.0],
    };
    let guess = exposure_for_median(pop.median, want, info);
    let shape = fit_with(&pop.ev, guess, &t, info);
    let mut out = AutoTone {
        exposure: round2(guess),
        contrast: shape.contrast,
        highlights: shape.highlights,
        shadows: shape.shadows,
        whites: shape.whites,
        blacks: shape.blacks,
        vibrance: 0.0,
        saturation: 0.0,
    };
    // settle exposure against the real render (monotone in exposure: bisection)
    let proxy = lightcraft_raster::resample::fit(src, 512, 512, lightcraft_raster::resample::Filter::Box);
    let req = RenderRequest { space: OutputSpace::Rec2020, depth: OutputDepth::F32Linear, ..RenderRequest::fit(512, 512) };
    let render_median = |exposure: f64| -> Option<Stats> {
        let r = crate::render(&proxy, info, &settings_with(s, AutoTone { exposure, ..out }), &req);
        r.deep.as_ref().and_then(deep_to_linear).and_then(|l| stats_linear(&l))
    };
    let (mut lo, mut hi) = ((guess - 2.0).max(-4.0) as f64, (guess + 2.0).min(4.0) as f64);
    let mut rendered = None;
    for _ in 0..EXPOSURE_STEPS {
        let mid = 0.5 * (lo + hi);
        let Some(st) = render_median(mid) else { break };
        rendered = Some((mid, st));
        if st.luminance[2] < want { lo = mid } else { hi = mid }
    }
    if let Some((exposure, st)) = rendered {
        out.exposure = round2(exposure as f32);
        // colourfulness: move vibrance by how far the render's chroma sits from the look's
        let have = st.chroma[1].max(1e-4);
        let want = look.chroma[1].max(1e-4);
        out.vibrance = (40.0 * (want / have).log2()).clamp(-40.0, 40.0).round() as f64;
    }
    Ok(out)
}

/// `s` with `a`'s eight values.
pub fn settings_with(s: &DevelopSettings, a: AutoTone) -> DevelopSettings {
    let mut d = s.clone();
    d.light.exposure = a.exposure;
    d.light.contrast = a.contrast;
    d.light.highlights = a.highlights;
    d.light.shadows = a.shadows;
    d.light.whites = a.whites;
    d.light.blacks = a.blacks;
    d.color.vibrance = a.vibrance;
    d.color.saturation = a.saturation;
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &Rgb32f, a: AutoTone) -> Rgba8 {
        crate::render(src, &SourceInfo::default(), &settings_with(&DevelopSettings::default(), a), &RenderRequest::fit(96, 96)).image
    }

    fn scene(gain: f32) -> Rgb32f {
        Rgb32f::from_fn(96, 96, |x, y| {
            let t = (x as f32 / 96.0) * 0.9 + 0.05;
            let v = (0.02 + t * t * 0.9) * gain;
            let warm = 1.0 + 0.25 * (y as f32 / 96.0);
            [v * warm, v, v * (2.0 - warm)]
        })
    }

    fn rendered_stats(img: &Rgba8) -> Stats {
        stats_linear(&srgb8_to_linear(img)).unwrap()
    }

    #[test]
    fn extract_then_apply_reproduces_a_look_on_a_different_exposure() {
        // the "look": a sample rendered one stop brighter and flatter than the scene would be
        let sample = render(&scene(1.0), AutoTone { exposure: 1.0, contrast: -15.0, ..Default::default() });
        let look = extract(std::slice::from_ref(&sample)).unwrap();
        assert_eq!(look.samples, 1);
        // a darker frame of the same scene
        let darker = scene(0.35);
        let a = apply(&darker, &SourceInfo::default(), &DevelopSettings::default(), &look).unwrap();
        let got = rendered_stats(&render(&darker, a));
        assert!((got.luminance[2] - look.luminance[2]).abs() < 0.03, "median {:.3} vs look {:.3}: {a:?}", got.luminance[2], look.luminance[2]);
        assert!((got.luminance[4] - look.luminance[4]).abs() < 0.08, "top {:.3} vs look {:.3}: {a:?}", got.luminance[4], look.luminance[4]);
        assert!(a.exposure > 1.8, "needs more exposure than the sample did: {a:?}");
        assert!(a.vibrance.abs() <= 12.0, "same scene, same colourfulness: {a:?}");
        // repeat-stable
        assert_eq!(a, apply(&darker, &SourceInfo::default(), &DevelopSettings::default(), &look).unwrap());
    }

    #[test]
    fn a_muted_look_pulls_vibrance_down() {
        let sample = render(&scene(1.0), AutoTone { vibrance: -40.0, saturation: -30.0, ..Default::default() });
        let look = extract(&[sample]).unwrap();
        let a = apply(&scene(1.0), &SourceInfo::default(), &DevelopSettings::default(), &look).unwrap();
        assert!(a.vibrance < -10.0, "{a:?}");
    }

    #[test]
    fn extract_uses_the_median_sample() {
        let bright = render(&scene(1.0), AutoTone { exposure: 2.0, ..Default::default() });
        let normal = render(&scene(1.0), AutoTone::default());
        let dark = render(&scene(1.0), AutoTone { exposure: -2.0, ..Default::default() });
        let three = extract(&[bright, normal.clone(), dark]).unwrap();
        let one = extract(std::slice::from_ref(&normal)).unwrap();
        assert!((three.luminance[2] - one.luminance[2]).abs() < 1e-6, "{three:?} vs {one:?}");
        assert_eq!(three.samples, 3);
        assert!(extract(&[]).is_none());
        assert!(extract(&[Rgba8::new(0, 0)]).is_none());
        // the linear entry point agrees with the sRGB one
        let lin = extract_linear(&[srgb8_to_linear(&normal)]).unwrap();
        assert_eq!(lin, one);
    }

    #[test]
    fn hostile_targets_are_refused() {
        let good = extract(&[render(&scene(1.0), AutoTone::default())]).unwrap();
        let mut bad = good.clone();
        bad.luminance[3] = f32::NAN;
        assert!(apply(&scene(1.0), &SourceInfo::default(), &DevelopSettings::default(), &bad).is_err());
        let mut bad = good.clone();
        bad.schema = "other".into();
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.luminance = [0.9, 0.8, 0.5, 0.3, 0.1];
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.clip = 2.0;
        assert!(bad.validate().is_err());
        let json = serde_json::to_string(&good).unwrap();
        assert_eq!(serde_json::from_str::<LookTarget>(&json).unwrap(), good);
        assert!(
            serde_json::from_str::<LookTarget>(
                r#"{"schema":"lightcraft.look-target.v1","samples":1,"luminance":[0,0,0,0,0],"clip":0,"chroma":[0,0],"extra":1}"#
            )
            .is_err()
        );
        // an empty source is neutral, not an error
        assert_eq!(apply(&Rgb32f::new(0, 0), &SourceInfo::default(), &DevelopSettings::default(), &good).unwrap(), AutoTone::default());
    }

    /// A grey vertical ramp: `dark` in the bottom third, `bright` in the top third, `mid` between.
    fn bands(n: usize, top: f32, bottom: f32) -> Rgb32f {
        Rgb32f::from_fn(n, n, |_, y| {
            let t = (y as f32 + 0.5) / n as f32;
            let v = if t < 1.0 / 3.0 { top } else if t < 2.0 / 3.0 { 0.2 } else { bottom };
            [v, v, v]
        })
    }

    /// A smooth grey gradient (the same scene at any size).
    fn gradient(n: usize) -> Rgb32f {
        Rgb32f::from_fn(n, n, |_, y| {
            let t = (y as f32 + 0.5) / n as f32;
            let v = 0.02 + 0.9 * t * t;
            [v, v, v]
        })
    }

    /// `look` with its grid set to `cells` and the given weighting.
    fn with_grid(look: &LookTarget, cells: [f32; CELLS], weighting: Weighting) -> LookTarget {
        LookTarget { grid: Some(Grid { cells: cells.map(|l| [l, 0.1]) }), weighting, ..look.clone() }
    }

    #[test]
    fn the_grid_separates_regions() {
        // bright top third, dark bottom third: the top row's cells are brighter than the bottom row's
        let grid = grid_linear(&bands(96, 0.7, 0.05)).unwrap();
        let top = grid.cells[0..GRID].iter().map(|c| c[0]).fold(f32::MAX, f32::min);
        let bottom = grid.cells[CELLS - GRID..].iter().map(|c| c[0]).fold(f32::MIN, f32::max);
        assert!(top > bottom + 0.4, "top {top:.3} vs bottom {bottom:.3}");
        // the middle row is the flat 0.2 band
        for c in &grid.cells[GRID..2 * GRID] {
            assert!((c[0] - encode(0.2)).abs() < 0.02, "{c:?}");
        }
        // a single-sample target carries the grid, and the grid is what the sample measured
        let target = extract_linear(&[bands(96, 0.7, 0.05)]).unwrap();
        assert_eq!(target.grid, Some(grid));
    }

    #[test]
    fn the_grid_is_resolution_independent() {
        let small = grid_linear(&gradient(96)).unwrap();
        let large = grid_linear(&gradient(192)).unwrap();
        for (a, b) in small.cells.iter().zip(&large.cells) {
            assert!((a[0] - b[0]).abs() < 0.02, "{a:?} vs {b:?}");
        }
        // face coverage is measured on a normalized lattice too
        let face = [FaceBox { x0: 0.6, y0: 0.6, x1: 0.9, y1: 0.9 }];
        let shares = face_shares(&face);
        assert!(shares[CELLS - 1] > 0.4 && shares[0] == 0.0, "{shares:?}");
        assert!(shares.iter().all(|s| (0.0..=1.0).contains(s)));
    }

    #[test]
    fn face_weighting_moves_the_target_toward_the_face_tone() {
        // a look whose grid is mid-grey everywhere except the bottom-right cell, where a face sits
        let mut cells = [0.5f32; CELLS];
        cells[CELLS - 1] = 0.2;
        let base = extract(&[render(&scene(1.0), AutoTone::default())]).unwrap();
        let face = [FaceBox { x0: 0.7, y0: 0.7, x1: 0.95, y1: 0.95 }];
        let plain = with_grid(&base, cells, Weighting { spatial: true, faces: false });
        let weighted = with_grid(&base, cells, Weighting { spatial: true, faces: true });
        let uniform = (8.0 * 0.5 + 0.2) / 9.0;
        assert!((look_median(&plain, &face) - uniform).abs() < 1e-6, "spatial without face weighting is the cell mean");
        let m = look_median(&weighted, &face);
        assert!(m < uniform - 0.02 && m > 0.2, "face weighting pulls toward the face tone: {m:.4} vs mean {uniform:.4}");
        // no faces: face weighting changes nothing
        assert_eq!(look_median(&weighted, &[]), look_median(&plain, &[]));
        // a face that is too small to meter on counts for nothing
        let speck = [FaceBox { x0: 0.7, y0: 0.7, x1: 0.701, y1: 0.701 }];
        assert_eq!(look_median(&weighted, &speck), look_median(&plain, &speck));
    }

    #[test]
    fn spatial_matching_follows_the_grid_and_the_disabled_path_is_bit_identical() {
        let darker = scene(0.35);
        let info = SourceInfo::default();
        let sample = render(&scene(1.0), AutoTone { exposure: 1.0, contrast: -15.0, ..Default::default() });
        let look = extract(&[sample]).unwrap();
        // no grid, or grid present but weighting off: both are the frame-wide match, bit for bit
        let no_grid = LookTarget { grid: None, ..look.clone() };
        let off = apply(&darker, &info, &DevelopSettings::default(), &no_grid).unwrap();
        assert_eq!(apply_with(&darker, &info, &DevelopSettings::default(), &look, &[FaceBox { x0: 0.2, y0: 0.2, x1: 0.8, y1: 0.8 }]).unwrap(), off);
        assert_eq!(look_median(&look, &[]).to_bits(), look.luminance[2].to_bits());
        // an old target file (no grid, no weighting) reads as the frame-wide match
        let old = r#"{"schema":"lightcraft.look-target.v1","samples":1,"luminance":[0.01,0.05,0.5,0.95,0.99],"clip":0,"chroma":[0.02,0.1]}"#;
        let parsed: LookTarget = serde_json::from_str(old).unwrap();
        assert_eq!((parsed.grid.is_none(), parsed.weighting), (true, Weighting::default()));
        // spatial weighting on, with a grid: a different (but finite, repeat-stable) fit
        let spatial = LookTarget { weighting: Weighting { spatial: true, faces: true }, ..look.clone() };
        let face = [FaceBox { x0: 0.3, y0: 0.3, x1: 0.7, y1: 0.7 }];
        let on = apply_with(&darker, &info, &DevelopSettings::default(), &spatial, &face).unwrap();
        assert!(on.exposure.is_finite() && on.vibrance.is_finite(), "{on:?}");
        assert_eq!(on, apply_with(&darker, &info, &DevelopSettings::default(), &spatial, &face).unwrap());
        // hostile grids are refused
        let mut bad = look.clone();
        bad.grid = Some(Grid { cells: [[f32::NAN, 0.0]; CELLS] });
        assert!(bad.validate().is_err());
    }
}
