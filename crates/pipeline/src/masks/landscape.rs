//! Landscape classes (`MaskShape::Landscape { class }`): classical hue / texture / position
//! decisions on the same work-size proxy as Sky (see `sky`), one decision per frame shared by all
//! classes.
//!
//! | class (case-insensitive; `-` / `_` read as a space) | how |
//! |---|---|
//! | `sky` | the Sky decision itself (a DNG sky matte replaces it, like for the Sky shape) |
//! | `vegetation`, `plants`, `trees`, `foliage` | green OkLab hue ([`VEG_HUE`]°) with chroma, a texture boost, not sky |
//! | `water` | blue / cyan hue ([`WATER_HUE`]°) with chroma, low texture, below the horizon, not sky |
//! | `ground`, `natural ground`, `terrain` | the rest of the frame below the horizon (not sky, vegetation or water) |
//!
//! `mountains`, `architecture`, `buildings`, `artificial ground` and unknown names are not credible
//! from hue, texture and position alone (a hill, a wall and a road share them) and select nothing.
//!
//! The horizon is the first output row where the sky covers less than [`HORIZON_COVER`] of the
//! width after a row that it covered (so a frame without a sky band uses [`HORIZON_DEFAULT`]),
//! kept within [`HORIZON_CLAMP`]. Hue is the OkLab angle of the tone-mapped pixel, in degrees
//! (red 29, yellow 110, green 142, cyan 195, blue 264). Components smaller than 1 / [`SPECK_DIV`]
//! of the plane are dropped, the rest softened.

use lightcraft_raster::Plane;

use super::sky::Scene;
use super::smooth;

/// A landscape class this module can decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    Sky,
    Vegetation,
    Water,
    Ground,
}

impl Class {
    /// The class named `name`, or `None` for a class that is not decided (selects nothing).
    pub(super) fn parse(name: &str) -> Option<Class> {
        let n: String = name.chars().map(|c| if c == '-' || c == '_' { ' ' } else { c.to_ascii_lowercase() }).collect();
        match n.trim() {
            "sky" => Some(Class::Sky),
            "vegetation" | "plants" | "trees" | "foliage" => Some(Class::Vegetation),
            "water" => Some(Class::Water),
            "ground" | "natural ground" | "terrain" => Some(Class::Ground),
            _ => None,
        }
    }
}

/// The soft work-size planes of the classes other than sky.
pub(super) struct Planes {
    pub(super) vegetation: Plane,
    pub(super) water: Plane,
    pub(super) ground: Plane,
}

/// Green hue (degrees) rising over `.0`..`.1` and falling over `.2`..`.3`.
const VEG_HUE: [f32; 4] = [75.0, 105.0, 155.0, 180.0];
/// Blue-cyan hue (degrees), likewise.
const WATER_HUE: [f32; 4] = [160.0, 185.0, 270.0, 295.0];
/// OkLab chroma over which a hue counts (vegetation, water).
const VEG_CHROMA: (f32, f32) = (0.02, 0.06);
const WATER_CHROMA: (f32, f32) = (0.012, 0.04);
/// Vegetation weight is `0.5 + 0.5 × smooth(texture)` over these EV (lawns are flat, leaves are not).
const VEG_TEX: (f32, f32) = (0.04, 0.25);
/// Water weight falls with texture (EV) over these (ripples are fine, a shoreline is not).
const WATER_TEX: (f32, f32) = (0.12, 0.4);
/// Below this log luminance (EV) hue is unreliable: weights fade to 0 over [`DARK`].
const DARK: (f32, f32) = (-7.0, -5.0);
/// Where the sky covers at least this fraction of a row, the row is sky band.
const HORIZON_COVER: f32 = 0.3;
/// The horizon (fraction of the output height) of a frame without a sky band.
const HORIZON_DEFAULT: f32 = 0.35;
/// The horizon is kept within these.
const HORIZON_CLAMP: (f32, f32) = (0.15, 0.9);
/// Fraction of the output height over which "below the horizon" fades in (water; ground starts a bit earlier).
const BELOW_WATER: (f32, f32) = (-0.02, 0.04);
const BELOW_GROUND: (f32, f32) = (-0.05, 0.1);
/// Components smaller than 1 / this of the work plane are specks.
const SPECK_DIV: usize = 2000;
/// Softening of a class plane (sigma at `WORK_LONG`).
const SOFT_SIGMA: f32 = 1.0;

/// Hue of OkLab `a, b` in degrees 0..360.
fn hue_deg(a: f32, b: f32) -> f32 {
    let h = b.atan2(a).to_degrees();
    if h.is_finite() { if h < 0.0 { h + 360.0 } else { h } } else { 0.0 }
}

fn band(h: f32, e: [f32; 4]) -> f32 {
    smooth(e[0], e[1], h) * (1.0 - smooth(e[2], e[3], h))
}

/// The horizon: output height fraction where the sky band of `sky` ends (see the module doc).
fn horizon(sky: &Plane, w: usize, h: usize) -> f32 {
    let mut seen = false;
    for y in 0..h {
        let Some(row) = sky.data.get(y * w..(y + 1) * w) else { break };
        let cover = row.iter().filter(|v| **v > 0.5).count() as f32 / w.max(1) as f32;
        if cover >= HORIZON_COVER {
            seen = true;
        } else if seen {
            return (y as f32 / h as f32).clamp(HORIZON_CLAMP.0, HORIZON_CLAMP.1);
        }
    }
    if seen { HORIZON_CLAMP.1 } else { HORIZON_DEFAULT }
}

/// Drop specks (components under 1 / [`SPECK_DIV`] of the plane) of `p` (selected where `>= 0.4`)
/// and soften the rest.
fn clean(p: &Plane, sc: &Scene) -> Plane {
    let sel: Vec<bool> = p.data.iter().map(|v| *v >= 0.4).collect();
    let (labels, comps) = super::subject::components(&sel, &[], sc.w, sc.h);
    let mut out = p.clone();
    for (v, l) in out.data.iter_mut().zip(&labels) {
        let keep = *l > 0 && comps.get(*l as usize - 1).is_some_and(|c| c.size * SPECK_DIV >= sc.w * sc.h);
        if !keep {
            *v = 0.0;
        }
    }
    let soft = sc.blur(&out, SOFT_SIGMA);
    soft.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

/// Vegetation, water and ground of `sc`, given its `sky` plane.
pub(super) fn decide(sc: &Scene, sky: &Plane) -> Planes {
    let (w, h) = (sc.w, sc.h);
    let hor = horizon(sky, w, h);
    let at = |p: &Plane, i: usize| p.data.get(i).copied().unwrap_or(0.0);
    let mut veg = Plane::new(w, h);
    let mut water = Plane::new(w, h);
    for i in 0..w * h {
        let (lum, a, b, tex) = (at(&sc.lum, i), at(&sc.a, i), at(&sc.b, i), at(&sc.tex, i));
        let (hue, chroma) = (hue_deg(a, b), a.hypot(b));
        let not_sky = 1.0 - at(sky, i).clamp(0.0, 1.0);
        let lit = smooth(DARK.0, DARK.1, lum);
        let y = sc.y_frac(i / w.max(1));
        let v = band(hue, VEG_HUE) * smooth(VEG_CHROMA.0, VEG_CHROMA.1, chroma) * (0.5 + 0.5 * smooth(VEG_TEX.0, VEG_TEX.1, tex)) * lit * not_sky;
        let wt = band(hue, WATER_HUE)
            * smooth(WATER_CHROMA.0, WATER_CHROMA.1, chroma)
            * (1.0 - smooth(WATER_TEX.0, WATER_TEX.1, tex))
            * lit
            * smooth(hor + BELOW_WATER.0, hor + BELOW_WATER.1, y)
            * not_sky;
        if let (Some(o), Some(p)) = (veg.data.get_mut(i), water.data.get_mut(i)) {
            *o = if v.is_finite() { v } else { 0.0 };
            *p = if wt.is_finite() { wt } else { 0.0 };
        }
    }
    let vegetation = clean(&veg, sc);
    let water = clean(&water, sc);
    let mut ground = Plane::new(w, h);
    for (i, g) in ground.data.iter_mut().enumerate() {
        let y = sc.y_frac(i / w.max(1));
        let taken = at(sky, i).max(at(&vegetation, i)).max(at(&water, i)).clamp(0.0, 1.0);
        *g = smooth(hor + BELOW_GROUND.0, hor + BELOW_GROUND.1, y) * (1.0 - taken);
    }
    let ground = ground.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
    Planes { vegetation, water, ground: sc.blur(&ground, SOFT_SIGMA) }
}
