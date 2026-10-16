//! Subject (and, by inversion, Background): a classical saliency + region segmenter.
//!
//! Used when the source carries no person matte and no model segmentation (issue #27). Everything
//! up to the final upsample runs on a plane downsampled to [`WORK_LONG`] px on the long edge, so a
//! preview and an export of the same photo select the same region; only the upsample and the edge
//! refinement run at output size.
//!
//! 1. Features per work pixel: log2 luminance and OkLab a, b, each scaled so that 1.0 is about
//!    "clearly different" ([`L_SCALE`] EV, [`AB_SCALE`] OkLab units).
//! 2. Saliency = distance to the nearest of four frame-edge background colours (per-side
//!    medians) + [`CS_WEIGHT`] × a two-scale centre-surround (difference of Gaussians), smoothed
//!    and weakly weighted towards the frame centre (never below [`CENTRE_FLOOR`]).
//! 3. Otsu threshold (never below [`THRESH_FLOOR`] of the peak), 4-connected components by an iterative
//!    flood fill; the components holding at least [`KEEP_MASS`] of the best one's saliency mass
//!    stay; small enclosed holes are filled.
//! 4. The region is softened, upsampled bilinearly and snapped to the photo's edges with a guided
//!    filter on log luminance, then lightly smoothed.
//!
//! A flat frame (no saliency above [`FLAT`]), a degenerate size or an empty result falls back to
//! the old weak centre ellipse, so the result is never NaN and never silently empty.

use lightcraft_geom::{Affine, Point};
use lightcraft_raster::{Plane, Rgb32f};

use super::{bilinear, frame_px, guided_cross, smooth, smooth_plane, tonemap_for_select};
use crate::for_rows;
use crate::geometry::Frame;

/// Long edge of the working plane (smaller frames are used as they are).
pub(super) const WORK_LONG: usize = 256;
/// Below this a work plane is too small to segment.
pub(super) const MIN_SIDE: usize = 8;
/// Feature scales: log2-luminance EV and OkLab a/b that count as one unit of difference.
pub(super) const L_SCALE: f32 = 1.5;
pub(super) const AB_SCALE: f32 = 0.1;
/// Weight of the centre-surround term against the border-colour distance.
const CS_WEIGHT: f32 = 0.6;
/// The centre prior multiplies saliency by 1 at the frame centre, down to this far out.
const CENTRE_FLOOR: f32 = 0.75;
/// Peak saliency below which the frame counts as flat (about half an EV of contrast).
const FLAT: f32 = 0.35;
/// The selection threshold is at least this fraction of the peak saliency.
const THRESH_FLOOR: f32 = 0.3;
/// Components with at least this fraction of the best component's saliency mass are kept.
const KEEP_MASS: f32 = 0.5;
/// Enclosed background holes up to 1 / this of the work plane are filled.
const HOLE_DIV: usize = 5;
/// A selection smaller than 1 / this of the frame is no subject.
const MIN_AREA_DIV: usize = 500;

/// The distance (in the old heuristic's centre-ellipse units) of output position `px, py` from
/// the ellipse's centre.
fn ellipse_distance(m: &Affine, px: f64, py: f64) -> f32 {
    let n = m.apply(Point::new(px, py));
    let d = (((n.x - 0.5) / 0.35).powi(2) + ((n.y - 0.55) / 0.4).powi(2)).sqrt() as f32;
    if d.is_finite() { d } else { 2.0 }
}

/// Half-strength centre ellipse, softened: what a frame without a subject yields.
fn fallback(frame: &Frame, w: usize, h: usize) -> Plane {
    let mut out = Plane::new(w, h);
    let m = frame.out_to_norm(w, h);
    for_rows(&mut out.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let d = ellipse_distance(&m, x as f64 + 0.5, y as f64 + 0.5);
            *v = 0.5 * (1.0 - smooth(0.6, 1.0, d));
        }
    });
    smooth_plane(&mut out, 0.015 * frame_px(frame, w));
    // the box blur's running sums leave rounding residue (about -1e-8 where the plane is 0)
    out.data.iter_mut().for_each(|v| *v = if v.is_finite() { v.clamp(0.0, 0.5) } else { 0.0 });
    out
}

/// The work plane size: the long edge at most [`WORK_LONG`], aspect kept.
pub(super) fn work_size(w: usize, h: usize) -> (usize, usize) {
    let long = w.max(h);
    if long <= WORK_LONG {
        return (w, h);
    }
    let k = WORK_LONG as f64 / long as f64;
    (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1))
}

/// Area-average `img` (linear RGB) and `log_l` onto `dw × dh` and convert to scaled features
/// `[L, a, b]`. `None` when the inputs disagree in size.
pub(super) fn features(img: &Rgb32f, log_l: &Plane, w: usize, h: usize, dw: usize, dh: usize) -> Option<[Plane; 3]> {
    if img.data.len() != w * h || log_l.data.len() != w * h || img.width != w || log_l.width != w {
        return None;
    }
    let mut f = [Plane::new(dw, dh), Plane::new(dw, dh), Plane::new(dw, dh)];
    for dy in 0..dh {
        let (y0, y1) = (dy * h / dh, ((dy + 1) * h / dh).max(dy * h / dh + 1).min(h));
        for dx in 0..dw {
            let (x0, x1) = (dx * w / dw, ((dx + 1) * w / dw).max(dx * w / dw + 1).min(w));
            let (mut rgb, mut l, mut n) = ([0f32; 3], 0f32, 0f32);
            for y in y0..y1 {
                let (Some(ir), Some(lr)) = (img.data.get(y * w + x0..y * w + x1), log_l.data.get(y * w + x0..y * w + x1)) else {
                    continue;
                };
                for (c, ll) in ir.iter().zip(lr) {
                    if c.iter().all(|v| v.is_finite()) && ll.is_finite() {
                        for (a, v) in rgb.iter_mut().zip(c) {
                            *a += v.max(0.0);
                        }
                        l += *ll;
                        n += 1.0;
                    }
                }
            }
            let (rgb, l) = if n > 0.0 { (rgb.map(|v| v / n), l / n) } else { ([0.0; 3], -10.0) };
            let lab = lightcraft_color::perceptual::oklab_from_2020(tonemap_for_select(rgb));
            let vals = [l / L_SCALE, lab[1] / AB_SCALE, lab[2] / AB_SCALE];
            for (p, v) in f.iter_mut().zip(vals) {
                if let Some(o) = p.data.get_mut(dy * dw + dx) {
                    *o = if v.is_finite() { v } else { 0.0 };
                }
            }
        }
    }
    Some(f)
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// The four frame-edge background colours: the per-channel median of each side's `bw`-px strip
/// (`bw` at most half the smaller side).
fn border_colours(f: &[Plane; 3], dw: usize, dh: usize, bw: usize) -> Vec<[f32; 3]> {
    let strips = [(0, dw, 0, bw), (0, dw, dh - bw, dh), (0, bw, 0, dh), (dw - bw, dw, 0, dh)];
    strips
        .iter()
        .map(|&(x0, x1, y0, y1)| {
            let mut ch: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for y in y0..y1 {
                for x in x0..x1 {
                    for (c, p) in ch.iter_mut().zip(f) {
                        c.push(p.data.get(y * dw + x).copied().unwrap_or(0.0));
                    }
                }
            }
            let [mut a, mut b, mut c] = ch;
            [median(&mut a), median(&mut b), median(&mut c)]
        })
        .collect()
}

/// Otsu's threshold of non-negative `s` (peak `smax` > 0) on 64 bins.
fn otsu(s: &[f32], smax: f32) -> f32 {
    const BINS: usize = 64;
    let mut hist = [0f32; BINS];
    for v in s {
        let b = ((v / smax) * BINS as f32) as usize;
        if let Some(c) = hist.get_mut(b.min(BINS - 1)) {
            *c += 1.0;
        }
    }
    let total: f32 = hist.iter().sum();
    let sum_all: f32 = hist.iter().enumerate().map(|(i, c)| i as f32 * c).sum();
    let (mut wb, mut sb, mut best, mut best_i) = (0f32, 0f32, -1f32, 0usize);
    for (i, c) in hist.iter().enumerate() {
        wb += c;
        sb += i as f32 * c;
        let wf = total - wb;
        if wb <= 0.0 {
            continue;
        }
        if wf <= 0.0 {
            break;
        }
        let (mb, mf) = (sb / wb, (sum_all - sb) / wf);
        let var = wb * wf * (mb - mf) * (mb - mf);
        if var > best {
            best = var;
            best_i = i;
        }
    }
    (best_i + 1) as f32 / BINS as f32 * smax
}

pub(super) struct Comp {
    pub(super) mass: f32,
    pub(super) size: usize,
    pub(super) border: bool,
}

/// 4-connected components of `sel` by an iterative flood fill (O(n), no recursion): per-pixel
/// labels (0 = none; label `k` is `comps[k - 1]`) and each component's `weight` mass (missing
/// weights count 0), size and whether it touches the frame edge.
pub(super) fn components(sel: &[bool], weight: &[f32], w: usize, h: usize) -> (Vec<u32>, Vec<Comp>) {
    let n = w * h;
    let mut labels = vec![0u32; n];
    let mut comps: Vec<Comp> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..n {
        if sel.get(start) != Some(&true) || labels.get(start) != Some(&0) {
            continue;
        }
        let id = comps.len() as u32 + 1;
        let mut c = Comp { mass: 0.0, size: 0, border: false };
        if let Some(l) = labels.get_mut(start) {
            *l = id;
        }
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            c.size += 1;
            c.mass += weight.get(i).copied().unwrap_or(0.0);
            c.border |= x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            let mut visit = |j: usize| {
                if sel.get(j) != Some(&true) {
                    return;
                }
                let Some(l) = labels.get_mut(j) else { return };
                if *l == 0 {
                    *l = id;
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
        comps.push(c);
    }
    (labels, comps)
}

/// The whole-frame part of the Subject decision (border colours, threshold, kept components) as a
/// soft region plane over the whole output, so a windowed render selects the same region as the
/// full render (see [`subject_proxy`]). Derived data: it never takes part in comparing frames.
pub struct SubjectProxy {
    /// The softened region at work size; `None`: no subject found (flat frame), use the fallback.
    region: Option<Plane>,
}

impl std::fmt::Debug for SubjectProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubjectProxy")
    }
}

impl PartialEq for SubjectProxy {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Whether any mask of `s` selects with the Subject heuristic (a Subject, Background or People
/// shape, or an Object with clicks but no stored segmentation): the renders that need a
/// [`SubjectProxy`].
pub(crate) fn uses_subject(s: &lightcraft_develop::DevelopSettings) -> bool {
    use lightcraft_develop::MaskShape;
    s.masks.iter().flat_map(|m| &m.components).any(|c| match &c.shape {
        MaskShape::Subject | MaskShape::Background | MaskShape::People { .. } => true,
        MaskShape::Object { hint, seg, detail, .. } => !hint.is_empty() && seg.is_none() && detail.is_empty(),
        _ => false,
    })
}

/// Decide the Subject of the whole `full_w × full_h` output of `frame` (a frame that is not a
/// window) on a work-size proxy of it: the source sampled through the frame (kept for the last few
/// views by `local::frame_proxy`, like the airlight's) and white-balanced. A render of the frame,
/// whole or windowed, then carries this in [`Frame::subject`] and only refines the edges on its
/// own pixels.
pub(crate) fn subject_proxy(
    src: &Rgb32f,
    info: &crate::SourceInfo,
    s: &lightcraft_develop::DevelopSettings,
    frame: &Frame,
    full_w: usize,
    full_h: usize,
) -> std::sync::Arc<SubjectProxy> {
    let (dw, dh) = work_size(full_w, full_h);
    let region = if dw >= MIN_SIDE && dh >= MIN_SIDE {
        let mut img = Rgb32f::clone(&crate::local::frame_proxy(src, frame, dw, dh));
        crate::local::white_balance(&mut img, info, s);
        let log_l = img.map(crate::local::log_lum);
        features(&img, &log_l, dw, dh, dw, dh).and_then(|f| decide(&f, dw, dh, &frame.out_to_norm(dw, dh)))
    } else {
        None
    };
    std::sync::Arc::new(SubjectProxy { region })
}

/// Saliency, threshold and kept components of the work-size features `f` (`m`: work pixels to
/// normalized coordinates, for the centre prior), as a softened 0..1 region plane. `None`: flat
/// frame or no usable region.
fn decide(f: &[Plane; 3], dw: usize, dh: usize, m: &Affine) -> Option<Plane> {
    let dlong = dw.max(dh);
    // sigmas are given at WORK_LONG and shrink with smaller frames
    let k = (dlong as f32 / WORK_LONG as f32).min(1.0);
    let blur = |p: &Plane, s: f32| lightcraft_raster::blur::gaussian(p, (s * k).max(0.5));

    // 2. saliency: distance to the nearest edge colour + centre-surround, centre-weighted
    let bw = ((0.03 * dlong as f32).round() as usize).clamp(1, (dw.min(dh) / 2).max(1));
    let bgs = border_colours(f, dw, dh, bw);
    let mut sal = Plane::new(dw, dh);
    for (i, v) in sal.data.iter_mut().enumerate() {
        let (Some(a), Some(b), Some(c)) = (f[0].data.get(i), f[1].data.get(i), f[2].data.get(i)) else {
            continue;
        };
        *v = bgs.iter().map(|g| ((a - g[0]).powi(2) + (b - g[1]).powi(2) + (c - g[2]).powi(2)).sqrt()).fold(f32::MAX, f32::min);
    }
    for (s_small, s_large) in [(1.5, 6.0), (3.0, 12.0)] {
        let small = [blur(&f[0], s_small), blur(&f[1], s_small), blur(&f[2], s_small)];
        let large = [blur(&f[0], s_large), blur(&f[1], s_large), blur(&f[2], s_large)];
        for (i, v) in sal.data.iter_mut().enumerate() {
            let at = |p: &Plane| p.data.get(i).copied().unwrap_or(0.0);
            let d2: f32 = small.iter().zip(&large).map(|(s, l)| (at(s) - at(l)).powi(2)).sum();
            *v += 0.5 * CS_WEIGHT * d2.sqrt();
        }
    }
    let mut sal = blur(&sal, 1.0);
    for (i, v) in sal.data.iter_mut().enumerate() {
        let (x, y) = ((i % dw) as f64 + 0.5, (i / dw) as f64 + 0.5);
        let prior = 1.0 - (1.0 - CENTRE_FLOOR) * smooth(0.6, 1.4, ellipse_distance(m, x, y));
        *v = if v.is_finite() { (*v * prior).max(0.0) } else { 0.0 };
    }
    let smax = sal.data.iter().copied().fold(0.0, f32::max);
    if smax < FLAT {
        return None;
    }

    // 3. threshold, strongest component(s), enclosed holes
    let t = otsu(&sal.data, smax).max(THRESH_FLOOR * smax);
    let sel: Vec<bool> = sal.data.iter().map(|v| *v >= t).collect();
    let (labels, comps) = components(&sel, &sal.data, dw, dh);
    let best = comps.iter().map(|c| c.mass).fold(0.0, f32::max);
    if best <= 0.0 {
        return None;
    }
    let mut region: Vec<bool> = labels.iter().map(|l| *l > 0 && comps.get(*l as usize - 1).is_some_and(|c| c.mass >= KEEP_MASS * best)).collect();
    let inv: Vec<bool> = region.iter().map(|r| !r).collect();
    let (hole_labels, holes) = components(&inv, &[], dw, dh);
    for (r, l) in region.iter_mut().zip(&hole_labels) {
        if *l > 0 && holes.get(*l as usize - 1).is_some_and(|c| !c.border && c.size * HOLE_DIV <= dw * dh) {
            *r = true;
        }
    }
    if region.iter().filter(|r| **r).count() * MIN_AREA_DIV < dw * dh {
        return None;
    }
    let mut plane = Plane::new(dw, dh);
    for (v, r) in plane.data.iter_mut().zip(&region) {
        *v = if *r { 1.0 } else { 0.0 };
    }
    Some(blur(&plane, 1.5))
}

/// Step 4: upsample the work-size `region` onto the output pixels, snap it to the photo's edges and
/// smooth it. `off` and `full`: the output's offset in, and the size of, the whole output the
/// region covers (a window of a larger output, or `(0, 0)` and `(w, h)`).
pub(super) fn refine(region: &Plane, frame: &Frame, w: usize, h: usize, log_l: &Plane, off: (f64, f64), full: (f64, f64)) -> Plane {
    let (dw, dh) = (region.width, region.height);
    let mut up = Plane::new(w, h);
    let (kx, ky) = (dw as f64 / full.0.max(1.0), dh as f64 / full.1.max(1.0));
    for_rows(&mut up.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let (u, t) = (((x as f64 + 0.5 + off.0) * kx) as f32, ((y as f64 + 0.5 + off.1) * ky) as f32);
            *v = bilinear(region, u, t).clamp(0.0, 1.0);
        }
    });
    let sigma = ((1.5 / kx) as f32).max(2.0);
    let refined = guided_cross(log_l, &up, sigma, 0.02);
    let mut out = up.zip_map(&refined, |u, r| if r.is_finite() { r } else { u });
    smooth_plane(&mut out, 0.003 * frame_px(frame, w));
    out.data.iter_mut().for_each(|v| *v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
    out
}

/// The Subject alpha plane (`w × h`, 0..1) of the output `img` / `log_l` (both before exposure).
/// A frame carrying a [`SubjectProxy`] (every render of a photo with a Subject / Background mask)
/// takes the region from it; otherwise the region is decided on `img` itself.
pub(super) fn subject_alpha(frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane) -> Plane {
    if w == 0 || h == 0 {
        return Plane::new(w, h);
    }
    if log_l.width != w || log_l.data.len() != w * h {
        return fallback(frame, w, h);
    }
    if let Some(p) = &frame.subject {
        return match &p.region {
            Some(region) => {
                let (off, full) = match frame.view {
                    Some(v) => ((v.x, v.y), (v.full_w, v.full_h)),
                    None => ((0.0, 0.0), (w as f64, h as f64)),
                };
                refine(region, frame, w, h, log_l, off, full)
            }
            None => fallback(frame, w, h),
        };
    }
    let (dw, dh) = work_size(w, h);
    if dw < MIN_SIDE || dh < MIN_SIDE {
        return fallback(frame, w, h);
    }
    let Some(f) = features(img, log_l, w, h, dw, dh) else {
        return fallback(frame, w, h);
    };
    let m = frame.out_to_norm(w, h) * Affine::scale(w as f64 / dw as f64, h as f64 / dh as f64);
    match decide(&f, dw, dh, &m) {
        Some(region) => refine(&region, frame, w, h, log_l, (0.0, 0.0), (w as f64, h as f64)),
        None => fallback(frame, w, h),
    }
}
