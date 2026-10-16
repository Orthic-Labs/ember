//! Sky (and the scene classes built on it, see `landscape`): a classical whole-frame decision.
//!
//! Used when the source carries no sky matte and no model segmentation (issue LR-MASK-SKY). The
//! decision runs on a plane of at most [`WORK_LONG`] px on the long edge (the frame sampled through
//! its crop / orientation / lens corrections, white-balanced, like the Subject's), so a preview, an
//! export and a window of either select the same region; only the upsample and the edge snap run at
//! output size.
//!
//! 1. Per work pixel: log2 luminance `L` (EV), OkLab `a`, `b`, and the *texture* (the standard
//!    deviation of `L` under a Gaussian of [`TEX_SIGMA`] px).
//! 2. A pixel is a sky *candidate* when `bright × colour × smoothness` is at least [`CAND_MIN`]:
//!    `bright` rises over [`BRIGHT_LO`]..[`BRIGHT_HI`] EV, `colour` is `0.4 + 0.6 × max(blue, neutral
//!    bright)` (blue: OkLab `b` below zero without a red `a`; neutral bright: a bright, nearly
//!    chroma-free overcast white; warm bright: sunset / dusk hues, see [`WARM_L`] for the gates that keep warm
//!    walls and skin out), and `smoothness` falls over [`TEX_LO`]..[`TEX_HI`] EV of texture
//!    (foliage, ground and roofs are textured; sky and clouds mostly are not).
//! 3. Candidates *connected to the top edge* (the top [`SEED_FRAC`] of the rows are the seeds) are
//!    kept by an iterative 4-connected flood fill that does not step across an edge: a neighbour
//!    more than [`STEP_L`] EV or [`STEP_AB`] OkLab units away stops the fill (the horizon line, a
//!    branch, a roof). A smooth blue lake below the horizon is a candidate but is never reached.
//!    Enclosed holes up to 1 / [`HOLE_DIV`] of the plane (clouds, birds, the sun) are filled.
//! 4. The horizon cut: sky weight falls over [`HORIZON_LO`]..[`HORIZON_HI`] of the output height
//!    (measured in the output, so a crop's top is its own sky edge), and the region's soft alpha is
//!    scaled by its colour score ([`COLOUR_LO`]..[`COLOUR_HI`]) so bright smooth but un-skylike
//!    surfaces (a white wall under the sky) stay weak.
//! 5. The region is softened (Gaussian), upsampled and snapped to the photo's edges by the same
//!    guided filter as Subject's (`subject::refine`).
//!
//! Warm skies qualify only when bright (absolute and relative to the frame's bright end), very smooth
//! and connected to the top; a warm wall that touches the top is darker or textured, and what passes the
//! colour term still needs the top connection and the horizon cut.
//!
//! The decision does not follow the render's exposure slider: darkening the picture does not move
//! the sky. No sky found is an empty plane, never NaN.

use lightcraft_raster::{Plane, Rgb32f};

use super::landscape::{self, Class, Planes};
use super::smooth;
use super::subject::{self, AB_SCALE, L_SCALE, MIN_SIDE, WORK_LONG};
use crate::geometry::Frame;

/// Luminance (log2 EV, scene-linear) over which a pixel counts as bright enough for sky.
const BRIGHT_LO: f32 = -3.5;
const BRIGHT_HI: f32 = -1.0;
/// OkLab `b` (negative: blue) over which a pixel counts as blue, and the `a` (red) that spoils it.
const BLUE_B: (f32, f32) = (0.01, 0.06);
const BLUE_A: (f32, f32) = (0.03, 0.08);
/// A bright (EV) low-chroma (OkLab) pixel counts as overcast white sky.
const NEUTRAL_L: (f32, f32) = (-2.5, -0.5);
const NEUTRAL_CHROMA: (f32, f32) = (0.03, 0.08);
/// Warm sky (sunset, dusk): OkLab hue 0..[`WARM_HUE_HI`]° (red through orange to yellow, fading out over
/// 15°) or from [`WARM_HUE_MAGENTA`]° (pink-magenta dusk, fading in over 20°), with chroma of at least
/// 0.02 to 0.05 (grey is the neutral term's).
const WARM_HUE_HI: f32 = 100.0;
const WARM_HUE_MAGENTA: f32 = 315.0;
const WARM_CHROMA: (f32, f32) = (0.02, 0.05);
/// A warm pixel must be bright in absolute terms (EV; a warm wall in shade or a skin tone is darker than
/// a lit sunset sky, which stays over about -1 EV even at dusk) ...
const WARM_L: (f32, f32) = (-1.6, -0.6);
/// ... and within this many EV (fading over the second) of the frame's 90th-percentile luminance, so a
/// warm surface that is not among the frame's brightest is no sky. A sun disc does not move the
/// percentile unless it covers over a tenth of the frame.
const WARM_REL: (f32, f32) = (1.5, 2.5);
/// ... and very smooth: texture (EV) over which the warm term falls (stricter than [`TEX_LO`]..[`TEX_HI`];
/// a warm sky is a clean gradient, plaster, skin, sand and wood are not).
const WARM_TEX: (f32, f32) = (0.05, 0.2);
/// Texture (EV) at the work scale over which smoothness falls from 1 to 0, and the Gaussian sigma (at
/// `WORK_LONG`) it is measured under.
const TEX_LO: f32 = 0.15;
const TEX_HI: f32 = 0.6;
const TEX_SIGMA: f32 = 1.5;
/// A pixel with `bright × colour × smoothness` of at least this is a candidate.
const CAND_MIN: f32 = 0.3;
/// The top fraction of the rows (at least one) whose candidates seed the flood fill.
const SEED_FRAC: f32 = 0.02;
/// The fill does not step to a neighbour further away than this in log luminance (EV) ...
const STEP_L: f32 = 0.35;
/// ... or in OkLab a / b (Euclidean).
const STEP_AB: f32 = 0.05;
/// Enclosed non-sky holes up to 1 / this of the work plane are filled.
const HOLE_DIV: usize = 8;
/// The colour score over which the region's alpha rises from 0 to 1.
const COLOUR_LO: f32 = 0.3;
const COLOUR_HI: f32 = 0.7;
/// Output heights (fraction) over which the sky weight falls from 1 to 0: the horizon cut.
const HORIZON_LO: f32 = 0.55;
const HORIZON_HI: f32 = 0.85;
/// Softening of the region before it is upsampled (sigma at `WORK_LONG`).
const SOFT_SIGMA: f32 = 1.5;

/// Features of the work-size plane the scene decisions read.
pub(super) struct Scene {
    pub(super) w: usize,
    pub(super) h: usize,
    /// log2 luminance, EV.
    pub(super) lum: Plane,
    /// OkLab a and b (tone-mapped like colour picking, see `tonemap_for_select`).
    pub(super) a: Plane,
    pub(super) b: Plane,
    /// Standard deviation of `lum` under a Gaussian of [`TEX_SIGMA`] px, EV.
    pub(super) tex: Plane,
    /// Sigma scale for a work plane smaller than `WORK_LONG` (sigmas are given at `WORK_LONG`).
    pub(super) k: f32,
    /// 90th percentile of `lum`, EV (the frame's bright end).
    pub(super) lum_p90: f32,
}

impl Scene {
    /// From the scaled features of `subject::features`.
    fn new(f: &[Plane; 3], w: usize, h: usize) -> Scene {
        let k = (w.max(h) as f32 / WORK_LONG as f32).min(1.0);
        let lum = f[0].map(|v| v * L_SCALE);
        let a = f[1].map(|v| v * AB_SCALE);
        let b = f[2].map(|v| v * AB_SCALE);
        let s = (TEX_SIGMA * k).max(0.5);
        let mean = lightcraft_raster::blur::gaussian(&lum, s);
        let mean_sq = lightcraft_raster::blur::gaussian(&lum.map(|v| v * v), s);
        let tex = mean_sq.zip_map(&mean, |q, m| {
            let v = (q - m * m).max(0.0).sqrt();
            if v.is_finite() { v } else { 0.0 }
        });
        let mut sorted: Vec<f32> = lum.data.iter().copied().filter(|v| v.is_finite()).collect();
        sorted.sort_by(f32::total_cmp);
        let lum_p90 = sorted.get(sorted.len() * 9 / 10).copied().unwrap_or(0.0);
        Scene { w, h, lum, a, b, tex, k, lum_p90 }
    }

    pub(super) fn blur(&self, p: &Plane, sigma: f32) -> Plane {
        lightcraft_raster::blur::gaussian(p, (sigma * self.k).max(0.5))
    }

    /// Output-relative height of work row `y`, 0 (top) .. 1.
    pub(super) fn y_frac(&self, y: usize) -> f32 {
        (y as f32 + 0.5) / self.h.max(1) as f32
    }
}

/// The whole-frame scene decision (sky and the landscape classes) as soft planes over the whole
/// output, at work size, so a windowed render selects the same regions as the full render. Derived
/// data: it never takes part in comparing frames.
pub struct SceneProxy {
    sky: Plane,
    classes: Planes,
}

impl std::fmt::Debug for SceneProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SceneProxy")
    }
}

impl PartialEq for SceneProxy {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl SceneProxy {
    fn plane(&self, class: Class) -> &Plane {
        match class {
            Class::Sky => &self.sky,
            Class::Vegetation => &self.classes.vegetation,
            Class::Water => &self.classes.water,
            Class::Ground => &self.classes.ground,
        }
    }
}

/// Whether any mask of `s` selects with the Sky heuristic or a Landscape class: the renders that
/// need a [`SceneProxy`].
pub(crate) fn uses_scene(s: &lightcraft_develop::DevelopSettings) -> bool {
    use lightcraft_develop::MaskShape;
    s.masks.iter().flat_map(|m| &m.components).any(|c| matches!(&c.shape, MaskShape::Sky | MaskShape::Landscape { .. }))
}

/// Decide the scene of the whole `full_w × full_h` output of `frame` (a frame that is not a window)
/// on a work-size proxy of it, like `subject::subject_proxy`.
pub(crate) fn scene_proxy(
    src: &Rgb32f,
    info: &crate::SourceInfo,
    s: &lightcraft_develop::DevelopSettings,
    frame: &Frame,
    full_w: usize,
    full_h: usize,
) -> std::sync::Arc<SceneProxy> {
    let (dw, dh) = subject::work_size(full_w, full_h);
    let decided = if dw >= MIN_SIDE && dh >= MIN_SIDE {
        let mut img = Rgb32f::clone(&crate::local::frame_proxy(src, frame, dw, dh));
        crate::local::white_balance(&mut img, info, s);
        let log_l = img.map(crate::local::log_lum);
        subject::features(&img, &log_l, dw, dh, dw, dh).map(|f| decide(&Scene::new(&f, dw, dh)))
    } else {
        None
    };
    std::sync::Arc::new(decided.unwrap_or_else(|| empty(dw, dh)))
}

fn empty(w: usize, h: usize) -> SceneProxy {
    SceneProxy { sky: Plane::new(w, h), classes: Planes { vegetation: Plane::new(w, h), water: Plane::new(w, h), ground: Plane::new(w, h) } }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// Sky colour score 0..1 of a pixel: `bright × (0.4 + 0.6 × max(blue, neutral bright))`.
fn sky_colour(sc: &Scene, i: usize) -> (f32, f32) {
    let at = |p: &Plane| p.data.get(i).copied().unwrap_or(0.0);
    let (lum, a, b, tex) = (at(&sc.lum), at(&sc.a), at(&sc.b), at(&sc.tex));
    let bright = smooth(BRIGHT_LO, BRIGHT_HI, lum);
    let blue = smooth(BLUE_B.0, BLUE_B.1, -b) * (1.0 - smooth(BLUE_A.0, BLUE_A.1, a));
    let neutral = smooth(NEUTRAL_L.0, NEUTRAL_L.1, lum) * (1.0 - smooth(NEUTRAL_CHROMA.0, NEUTRAL_CHROMA.1, a.hypot(b)));
    let hue = b.atan2(a).to_degrees().rem_euclid(360.0);
    let warm_hue =
        if hue <= 180.0 { 1.0 - smooth(WARM_HUE_HI, WARM_HUE_HI + 15.0, hue) } else { smooth(WARM_HUE_MAGENTA, WARM_HUE_MAGENTA + 20.0, hue) };
    let warm = warm_hue
        * smooth(WARM_CHROMA.0, WARM_CHROMA.1, a.hypot(b))
        * smooth(WARM_L.0, WARM_L.1, lum)
        * smooth(-WARM_REL.1, -WARM_REL.0, lum - sc.lum_p90)
        * (1.0 - smooth(WARM_TEX.0, WARM_TEX.1, tex));
    (bright, 0.4 + 0.6 * finite(blue.max(neutral).max(warm)))
}

/// Candidates reachable from the top edge: an iterative (stack) 4-connected fill over `cand` that
/// does not cross a step of more than [`STEP_L`] EV or [`STEP_AB`] OkLab units between neighbours.
fn flood_from_top(cand: &[bool], sc: &Scene) -> Vec<bool> {
    let (w, h) = (sc.w, sc.h);
    let mut reached = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    let seed_rows = ((SEED_FRAC * h as f32).ceil() as usize).clamp(1, h.max(1));
    for i in 0..(seed_rows * w).min(w * h) {
        if cand.get(i) == Some(&true)
            && let Some(r) = reached.get_mut(i)
        {
            *r = true;
            stack.push(i);
        }
    }
    let at = |p: &Plane, i: usize| p.data.get(i).copied().unwrap_or(0.0);
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut visit = |j: usize| {
            if cand.get(j) != Some(&true) || reached.get(j) != Some(&false) {
                return;
            }
            let dl = (at(&sc.lum, i) - at(&sc.lum, j)).abs();
            let dab = (at(&sc.a, i) - at(&sc.a, j)).hypot(at(&sc.b, i) - at(&sc.b, j));
            if dl > STEP_L || dab > STEP_AB {
                return;
            }
            if let Some(r) = reached.get_mut(j) {
                *r = true;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    reached
}

/// Fill the holes of `region` that touch no frame edge and are at most 1 / `div` of the plane.
pub(super) fn fill_holes(region: &mut [bool], w: usize, h: usize, div: usize) {
    let inv: Vec<bool> = region.iter().map(|r| !r).collect();
    let (labels, holes) = subject::components(&inv, &[], w, h);
    for (r, l) in region.iter_mut().zip(&labels) {
        if *l > 0 && holes.get(*l as usize - 1).is_some_and(|c| !c.border && c.size * div <= w * h) {
            *r = true;
        }
    }
}

/// The soft sky plane (work size, 0..1) of `sc`: all-zero when there is no sky.
fn decide_sky(sc: &Scene) -> Plane {
    let (w, h) = (sc.w, sc.h);
    let n = w * h;
    let mut colour = vec![0f32; n];
    let mut cand = vec![false; n];
    for i in 0..n {
        let at = |p: &Plane| p.data.get(i).copied().unwrap_or(0.0);
        let (bright, col) = sky_colour(sc, i);
        let flat = 1.0 - smooth(TEX_LO, TEX_HI, at(&sc.tex));
        let score = finite(bright * col);
        if let (Some(c), Some(k)) = (colour.get_mut(i), cand.get_mut(i)) {
            *c = score;
            *k = score * flat >= CAND_MIN;
        }
    }
    let mut region = flood_from_top(&cand, sc);
    fill_holes(&mut region, w, h, HOLE_DIV);
    let mut plane = Plane::new(w, h);
    for (v, r) in plane.data.iter_mut().zip(&region) {
        *v = if *r { 1.0 } else { 0.0 };
    }
    let mut soft = sc.blur(&plane, SOFT_SIGMA);
    for (i, v) in soft.data.iter_mut().enumerate() {
        let col = colour.get(i).copied().unwrap_or(0.0);
        let horizon = 1.0 - smooth(HORIZON_LO, HORIZON_HI, sc.y_frac(i / w.max(1)));
        *v = finite(*v * smooth(COLOUR_LO, COLOUR_HI, col) * horizon).clamp(0.0, 1.0);
    }
    soft
}

/// Sky and every landscape class of `sc`.
fn decide(sc: &Scene) -> SceneProxy {
    let sky = decide_sky(sc);
    let classes = landscape::decide(sc, &sky);
    SceneProxy { sky, classes }
}

/// The alpha plane (`w × h`, 0..1) of `class` for the output `img` / `log_l` (both before exposure).
/// A frame carrying a [`SceneProxy`] (every render of a photo with a Sky / Landscape mask) takes the
/// region from it; otherwise the scene is decided on `img` itself.
pub(super) fn scene_alpha(frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, class: Class) -> Plane {
    if w == 0 || h == 0 || log_l.width != w || log_l.data.len() != w * h {
        return Plane::new(w, h);
    }
    if let Some(p) = &frame.scene {
        let (off, full) = match frame.view {
            Some(v) => ((v.x, v.y), (v.full_w, v.full_h)),
            None => ((0.0, 0.0), (w as f64, h as f64)),
        };
        let region = p.plane(class);
        if region.width == 0 || region.data.len() != region.width * region.height {
            return Plane::new(w, h);
        }
        return subject::refine(region, frame, w, h, log_l, off, full);
    }
    let (dw, dh) = subject::work_size(w, h);
    if dw < MIN_SIDE || dh < MIN_SIDE {
        return Plane::new(w, h);
    }
    let Some(f) = subject::features(img, log_l, w, h, dw, dh) else {
        return Plane::new(w, h);
    };
    let proxy = decide(&Scene::new(&f, dw, dh));
    subject::refine(proxy.plane(class), frame, w, h, log_l, (0.0, 0.0), (w as f64, h as f64))
}
