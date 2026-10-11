//! Look targets: the rendered statistics of sample photos, applied to another photo by refitting
//! Auto toward them.
//!
//! A preset copies slider values; a look target copies the *result*. [`extract`] measures what
//! a set of sample renders have in common (where their tones sit, how much headroom they keep,
//! how colourful they are) and [`apply`] runs the closed-loop fit Auto uses
//! ([`crate::auto::fit`]) with those statistics as the targets, then renders the proxy through
//! the real pipeline to correct exposure, so every photo gets its own slider values yet lands on
//! the same histogram shape. Deterministic & resolution-independent.

use lightcraft_color::perceptual::oklab_from_2020;
use lightcraft_color::transfer::decode_srgb8;
use lightcraft_color::{REC2020, SRGB, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::{Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};

use crate::auto::fit::{Targets, encode, fit_with, tone_map};
use crate::auto::{AutoTone, SceneKey, scene_population};
use crate::output::{DeepImage, DeepSamples, OutputDepth, OutputSpace};
use crate::{RenderRequest, SourceInfo};

pub const SCHEMA: &str = "lightcraft.look-target.v1";

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

/// An sRGB 8-bit render (a JPEG / PNG the user exported) as display-linear Rec. 2020.
fn srgb8_to_linear(img: &Rgba8) -> Rgb32f {
    let to_2020 = SRGB.to_space(&REC2020);
    Rgb32f::from_fn(img.width, img.height, |x, y| {
        let p = img.get(x, y);
        to_2020.apply_f32([decode_srgb8(p[0]), decode_srgb8(p[1]), decode_srgb8(p[2])])
    })
}

fn target_of(stats: &[Stats]) -> Option<LookTarget> {
    if stats.is_empty() {
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
    let t = LookTarget { schema: SCHEMA.into(), samples: stats.len(), luminance, clip: median(&|s| s.clip), chroma };
    t.validate().ok().map(|_| t)
}

/// The look target of `samples`: the user's finished photos as sRGB 8-bit renders. `None`
/// without a usable sample.
pub fn extract(samples: &[Rgba8]) -> Option<LookTarget> {
    let stats: Vec<_> = samples.iter().filter_map(|s| stats_linear(&srgb8_to_linear(s))).collect();
    target_of(&stats)
}

/// [`extract`] for samples already decoded to the display-linear working space (what
/// `lightcraft_codecs::Decoded::to_working` gives for a rendered file).
pub fn extract_linear(samples: &[Rgb32f]) -> Option<LookTarget> {
    let stats: Vec<_> = samples.iter().filter_map(stats_linear).collect();
    target_of(&stats)
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
    look.validate()?;
    let Some(pop) = scene_population(src, info, s) else { return Ok(AutoTone::default()) };
    let key = SceneKey::of(pop.median, pop.p05, pop.p95, pop.p995);
    let keyed = key.low.max(key.high);
    let t = Targets {
        top: look.luminance[4],
        bottom: look.luminance[0],
        spread: look.luminance[3] - look.luminance[1],
        median: look.luminance[2],
        clip: look.clip.max(0.002),
        w: [1.0, 1.0, 0.5 * (1.0 - 0.5 * keyed), 2.0],
    };
    let guess = exposure_for_median(pop.median, look.luminance[2], info);
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
    let want = look.luminance[2];
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
}
