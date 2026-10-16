//! Mask evaluation: each mask becomes an alpha plane at output resolution.
//!
//! Shapes are defined in normalized oriented-image coordinates; sizes are in "long-edge units".
//! Components combine with Add (max), Subtract (a·(1−c)) and Intersect (a·c).
//!
//! AI shapes (Sky, Subject, Background, People) use the segmentation [`Mattes`] the source carries
//! (DNG semantic masks, e.g. iPhone ProRAW's) when it has a matching one, else a heuristic.

use lightcraft_develop::{BrushStroke, LocalAdjustments, Mask, MaskOp, MaskShape, SegMask};
use lightcraft_geom::{Orientation, Point};
use lightcraft_raster::{Image, Plane, Rgb32f};

use crate::for_rows;
use crate::geometry::Frame;

mod landscape;
mod sky;
mod subject;
pub use sky::SceneProxy;
pub(crate) use sky::{scene_proxy, uses_scene};
pub use subject::SubjectProxy;
pub(crate) use subject::{subject_proxy, uses_subject};

/// The Subject alpha plane of `img` / `log_l` (before exposure): Lens Blur's depth proxy.
pub(crate) fn subject_matte(frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane) -> Plane {
    subject::subject_alpha(frame, w, h, img, log_l)
}

/// What an embedded segmentation matte selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatteKind {
    Sky,
    /// A whole person (all people in the photo).
    Person,
    Skin,
    Hair,
    Teeth,
    Glasses,
}

/// Segmentation mattes that came with the source: 0..255 alpha planes, each covering the whole
/// (EXIF-oriented) source image at its own resolution. Several mattes of one kind (e.g. one per
/// person) are combined with max.
#[derive(Clone, Default, PartialEq)]
pub struct Mattes {
    mattes: Vec<(MatteKind, Image<u8>)>,
}

impl std::fmt::Debug for Mattes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.mattes.iter().map(|(k, m)| (k, m.width, m.height))).finish()
    }
}

impl Mattes {
    /// Add a matte (empty ones are ignored).
    pub fn push(&mut self, kind: MatteKind, matte: Image<u8>) {
        if matte.width > 0 && matte.height > 0 && matte.data.len() == matte.width * matte.height {
            self.mattes.push((kind, matte));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mattes.is_empty()
    }

    /// Memory held (bytes).
    pub fn bytes(&self) -> usize {
        self.mattes.iter().map(|(_, m)| m.data.len()).sum()
    }

    fn of(&self, kinds: &[MatteKind]) -> Vec<&Image<u8>> {
        self.mattes.iter().filter(|(k, _)| kinds.contains(k)).map(|(_, m)| m).collect()
    }

    /// The mattes a shape uses instead of its heuristic (empty: none fits).
    fn for_shape(&self, shape: &MaskShape) -> Vec<&Image<u8>> {
        use MatteKind::*;
        // the whole person: a person matte, else the union of the parts
        let person = || {
            let p = self.of(&[Person]);
            if p.is_empty() { self.of(&[Skin, Hair, Teeth, Glasses]) } else { p }
        };
        match shape {
            MaskShape::Sky => self.of(&[Sky]),
            MaskShape::Landscape { class } if landscape::Class::parse(class) == Some(landscape::Class::Sky) => self.of(&[Sky]),
            MaskShape::Subject => person(),
            MaskShape::People { parts, .. } => {
                let wanted: Vec<MatteKind> = parts
                    .iter()
                    .filter_map(|p| {
                        let p = p.to_ascii_lowercase();
                        [("skin", Skin), ("hair", Hair), ("teeth", Teeth), ("glasses", Glasses)]
                            .into_iter()
                            .find(|(n, _)| p.contains(n))
                            .map(|(_, k)| k)
                    })
                    .collect();
                let whole =
                    parts.is_empty() || parts.iter().any(|p| matches!(p.to_ascii_lowercase().as_str(), "person" | "entire person" | "entireperson"));
                // parts without a matte (lips, clothes, …) keep the heuristic
                if whole { person() } else { self.of(&wanted) }
            }
            _ => Vec::new(),
        }
    }
}

pub struct Evaluated {
    /// The mask's id ([`Mask::id`]).
    pub id: u32,
    pub alpha: Plane,
    pub adjust: LocalAdjustments,
}

#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The visible masks with components, evaluated in order (`mattes`: the source's, see [`Mattes`]).
#[allow(clippy::too_many_arguments)]
pub fn evaluate(masks: &[Mask], frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, ev: f32, mattes: Option<&Mattes>) -> Vec<Evaluated> {
    masks
        .iter()
        .filter(|m| m.visible && !m.components.is_empty())
        .map(|m| Evaluated { id: m.id, alpha: evaluate_one(m, frame, w, h, img, log_l, ev, mattes), adjust: m.adjust })
        .collect()
}

/// The alpha plane of mask `m` (whether visible or not): its components combined, inverted and
/// scaled by its amount.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_one(m: &Mask, frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, ev: f32, mattes: Option<&Mattes>) -> Plane {
    let mut alpha = Plane::new(w, h);
    let mut first = true;
    for comp in &m.components {
        let mut c = shape_alpha(&comp.shape, frame, w, h, img, log_l, ev, mattes);
        if comp.invert {
            c.data.iter_mut().for_each(|v| *v = 1.0 - *v);
        }
        if first && comp.op != MaskOp::Intersect {
            alpha = c;
            first = false;
            continue;
        }
        for (a, c) in alpha.data.iter_mut().zip(&c.data) {
            *a = match comp.op {
                MaskOp::Add => a.max(*c),
                MaskOp::Subtract => *a * (1.0 - c),
                MaskOp::Intersect => *a * c,
            };
        }
        first = false;
    }
    if m.feather > 0.0 {
        // Feather: a Gaussian of the finished selection, its radius relative to the long edge
        let k = (m.feather / 100.0).clamp(0.0, 1.0) as f32;
        let sigma = 0.02 * frame.output_long(w, h) as f32 * k;
        if sigma > 0.3 {
            alpha = lightcraft_raster::blur::gaussian(&alpha, sigma);
        }
    }
    if m.refine > 0.0 {
        // Refine Edges: the mask's edges follow the photo's (window up to ~4 % of the long edge,
        // the width of a soft brush edge or gradient)
        let k = (m.refine / 100.0).clamp(0.0, 1.0) as f32;
        let sigma = (0.04 * frame.output_long(w, h) as f32 * k).max(1.0);
        let refined = guided_cross(log_l, &alpha, sigma, 0.02);
        for (a, r) in alpha.data.iter_mut().zip(&refined.data) {
            *a += (r - *a) * k.sqrt();
        }
    }
    if m.invert {
        alpha.data.iter_mut().for_each(|v| *v = 1.0 - *v);
    }
    let amt = (m.adjust.amount / 100.0) as f32;
    if (amt - 1.0).abs() > 1e-6 {
        alpha.data.iter_mut().for_each(|v| *v *= amt);
    }
    alpha
}

/// Per-pixel positions in long-edge units for output pixel centres.
fn for_each_pos(frame: &Frame, w: usize, h: usize, out: &mut Plane, f: impl Fn(Point, usize) -> f32 + Sync + Send) {
    let m = frame.out_to_norm(w, h);
    let (ow, oh, l) = (frame.ow, frame.oh, frame.ow.max(frame.oh));
    for_rows(&mut out.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            *v = f(Point::new(n.x * ow / l, n.y * oh / l), y * w + x);
        }
    });
}

/// The alpha plane of one component shape. AI shapes use the source's matching `mattes`, if any.
#[allow(clippy::too_many_arguments)]
pub fn shape_alpha(shape: &MaskShape, frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, ev: f32, mattes: Option<&Mattes>) -> Plane {
    let fitting = mattes.map(|m| m.for_shape(shape)).unwrap_or_default();
    if !fitting.is_empty() {
        return matte_alpha(&fitting, frame, w, h);
    }
    let mut out = Plane::new(w, h);
    let to_long = |p: Point| frame.norm_to_long(p);
    // `img`/`log_l` are before exposure: range masks select on the exposed values.
    let gain = ev.exp2();
    match shape {
        MaskShape::Linear { start, end } => {
            let (a, b) = (to_long(*start), to_long(*end));
            let d = b - a;
            let len2 = d.dot(d).max(1e-12);
            for_each_pos(frame, w, h, &mut out, |p, _| {
                let t = ((p - a).dot(d) / len2) as f32;
                1.0 - smooth(0.0, 1.0, t)
            });
        }
        MaskShape::Radial { center, rx, ry, angle, feather, invert } => {
            let c = to_long(*center);
            let (s, co) = (-angle.to_radians()).sin_cos();
            let f = (*feather / 100.0).clamp(0.0, 1.0) as f32;
            let (rx, ry) = (rx.max(1e-6), ry.max(1e-6));
            let inv = *invert;
            for_each_pos(frame, w, h, &mut out, |p, _| {
                let d = p - c;
                let (x, y) = (d.x * co - d.y * s, d.x * s + d.y * co);
                let r = ((x / rx).powi(2) + (y / ry).powi(2)).sqrt() as f32;
                let a = 1.0 - smooth(1.0 - f.max(0.01), 1.0, r);
                if inv { 1.0 - a } else { a }
            });
        }
        MaskShape::Brush { strokes } => rasterize_brush(strokes, frame, w, h, img, log_l, &mut out),
        MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } => {
            let (lo, hi, lf, hf) = (*lo as f32, *hi as f32, (*lo_feather as f32).max(1e-3), (*hi_feather as f32).max(1e-3));
            for (v, l) in out.data.iter_mut().zip(&log_l.data) {
                // map log luminance to a 0..1 perceptual scale (−8..+4 EV)
                let y = ((l + ev + 8.0) / 12.0).clamp(0.0, 1.0);
                *v = smooth(lo - lf, lo, y) * (1.0 - smooth(hi, hi + hf, y));
            }
        }
        MaskShape::ColorRange { samples, refine } => {
            let tol = color_range_tolerance(*refine);
            let samples: Vec<[f32; 3]> = samples.iter().map(|s| [s[0] as f32, s[1] as f32, s[2] as f32]).collect();
            for (v, c) in out.data.iter_mut().zip(&img.data) {
                let lab = lightcraft_color::perceptual::oklab_from_2020(tonemap_for_select(c.map(|v| v * gain)));
                let d = samples
                    .iter()
                    .map(|s| ((lab[1] - s[1]).powi(2) + (lab[2] - s[2]).powi(2) + 0.25 * (lab[0] - s[0]).powi(2)).sqrt())
                    .fold(f32::MAX, f32::min);
                *v = 1.0 - smooth(tol * 0.5, tol, d);
            }
        }
        MaskShape::Sky => {
            // Classical whole-frame sky decision (see `sky`): bright, smooth, blue-ish or neutral
            // regions connected to the top edge, above the horizon. A DNG sky matte replaces it above.
            out = sky::scene_alpha(frame, w, h, img, log_l, landscape::Class::Sky);
        }
        MaskShape::Object { seg, detail, edge, .. } | MaskShape::Prompt { seg, detail, edge, .. } if seg.is_some() || !detail.is_empty() => {
            // Edge: below 0 a steeper transition (up to 8× the logits), above 0 feathered (up to
            // 2 % of the long edge, so previews and exports match)
            let e = (*edge / 100.0).clamp(-1.0, 1.0) as f32;
            let gain = if e < 0.0 { 1.0 - 7.0 * e } else { 1.0 };
            sample_seg(seg.as_ref(), detail, gain, frame, w, h, &mut out);
            if e > 0.0 {
                smooth_plane(&mut out, e * 0.02 * frame_px(frame, w));
            }
        }
        // not computed yet (no clicks, or a build without the model): nothing selected
        MaskShape::Object { hint, .. } if hint.is_empty() => {}
        MaskShape::Prompt { .. } => {}
        MaskShape::Subject | MaskShape::Object { .. } | MaskShape::People { .. } => {
            // Classical saliency + region segmenter (see `subject`); a model segmentation or a
            // DNG matte replaces it above.
            out = subject::subject_alpha(frame, w, h, img, log_l);
        }
        MaskShape::Background => {
            let mut s = shape_alpha(&MaskShape::Subject, frame, w, h, img, log_l, ev, mattes);
            s.data.iter_mut().for_each(|v| *v = 1.0 - *v);
            out = s;
        }
        MaskShape::Landscape { class } => {
            // Hue / texture / position classes on the Sky proxy (see `landscape`); a class that is not
            // credible from those (mountains, architecture, ...) selects nothing.
            if let Some(c) = landscape::Class::parse(class) {
                out = sky::scene_alpha(frame, w, h, img, log_l, c);
            }
        }
        MaskShape::DepthRange { .. } => {}
    }
    out
}

/// A decoded [`SegMask`]: its logits and the normalized rectangle they cover.
struct SegGrid {
    logits: Vec<f32>,
    side: usize,
    r: [f64; 4],
}

impl SegGrid {
    fn new(seg: &SegMask) -> Option<SegGrid> {
        Some(SegGrid { logits: seg.logits()?, side: seg.side as usize, r: seg.bounds()? })
    }

    fn contains(&self, n: Point) -> bool {
        n.x >= self.r[0] && n.x <= self.r[2] && n.y >= self.r[1] && n.y <= self.r[3]
    }

    /// The bilinearly sampled logit at normalized image point `n` (cell centres at
    /// `(i + 0.5) / side` of the rectangle).
    fn logit(&self, n: Point) -> f32 {
        let side = self.side;
        let at = |x: usize, y: usize| self.logits.get(y.min(side - 1) * side + x.min(side - 1)).copied().unwrap_or(-16.0);
        let u = (n.x - self.r[0]) / (self.r[2] - self.r[0]);
        let v = (n.y - self.r[1]) / (self.r[3] - self.r[1]);
        let (fx, fy) = ((u * side as f64 - 0.5) as f32, (v * side as f64 - 0.5) as f32);
        let (fx, fy) = (fx.clamp(0.0, (side - 1) as f32), fy.clamp(0.0, (side - 1) as f32));
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        let bot = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
        top * (1.0 - ty) + bot * ty
    }
}

/// Stored model segmentations at each output pixel: the zoomed-in `detail` passes inside
/// their rectangles (the highest of them where they overlap), the whole-photo `seg`
/// elsewhere; the logit (times `gain`: > 1 is a harder edge) through a sigmoid. Damaged
/// data selects nothing.
fn sample_seg(seg: Option<&SegMask>, detail: &[SegMask], gain: f32, frame: &Frame, w: usize, h: usize, out: &mut Plane) {
    let coarse = seg.and_then(SegGrid::new);
    // (capped: a hostile document can hold many)
    let patches: Vec<SegGrid> = detail.iter().take(lightcraft_develop::segmask::MAX_DETAIL).filter_map(SegGrid::new).collect();
    if coarse.is_none() && patches.is_empty() {
        return;
    }
    let m = frame.out_to_norm(w, h);
    for_rows(&mut out.data, w, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let n = m.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            let fine = patches.iter().filter(|p| p.contains(n)).map(|p| p.logit(n)).reduce(f32::max);
            let l = fine.or_else(|| coarse.as_ref().map(|c| c.logit(n))).unwrap_or(-16.0);
            *v = 1.0 / (1.0 + (-l * gain).exp());
        }
    });
}

fn frame_px(frame: &Frame, w: usize) -> f32 {
    frame.px_per_long(w) as f32
}

fn smooth_plane(p: &mut Plane, sigma: f32) {
    *p = lightcraft_raster::blur::gaussian(p, sigma.max(0.5));
}

/// The union (max) of `mattes` (0..255, over the EXIF-oriented source) at output resolution,
/// sampled like the image: user orientation, lens / perspective warp, crop and flips; area-averaged
/// first when the output is much smaller than the matte, bilinear otherwise.
fn matte_alpha(mattes: &[&Image<u8>], frame: &Frame, w: usize, h: usize) -> Plane {
    let mut out = Plane::new(w, h);
    let o2t = frame.out_to_oriented(w, h);
    let ppl = frame.px_per_long(w).max(1e-9);
    for m in mattes {
        let oriented =
            if frame.orient == Orientation::Normal { std::borrow::Cow::Borrowed(*m) } else { std::borrow::Cow::Owned(m.oriented(frame.orient)) };
        // matte px per output px
        let k = oriented.width as f64 / frame.ow * frame.ow.max(frame.oh) / ppl;
        let base = box_down(&oriented, if k >= 2.0 { (k.floor() as usize).min(64) } else { 1 });
        let (sx, sy) = ((base.width as f64 / frame.ow) as f32, (base.height as f64 / frame.oh) as f32);
        for_rows(&mut out.data, w, |y, row| {
            for (x, v) in row.iter_mut().enumerate() {
                let s = match &frame.warp {
                    Some(wp) => {
                        let Some((_, s)) = wp.frame(&o2t, x, y) else {
                            continue;
                        };
                        s
                    }
                    None => o2t.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5)),
                };
                *v = v.max(bilinear(&base, s.x as f32 * sx, s.y as f32 * sy) / 255.0);
            }
        });
    }
    out
}

/// `m` area-averaged by `f × f` (0..255 values kept); `f == 1` converts only.
fn box_down(m: &Image<u8>, f: usize) -> Plane {
    let f = f.max(1);
    let (bw, bh) = (m.width.div_ceil(f).max(1), m.height.div_ceil(f).max(1));
    let mut out = Plane::new(bw, bh);
    for_rows(&mut out.data, bw, |by, row| {
        let rows = by * f..((by + 1) * f).min(m.height);
        for (bx, v) in row.iter_mut().enumerate() {
            let cols = bx * f..((bx + 1) * f).min(m.width);
            let (mut sum, mut n) = (0u32, 0u32);
            for y in rows.clone() {
                for x in cols.clone() {
                    sum += m.data.get(y * m.width + x).copied().unwrap_or(0) as u32;
                    n += 1;
                }
            }
            *v = sum as f32 / n.max(1) as f32;
        }
    });
    out
}

/// Bilinear sample at continuous pixel coordinates (pixel centres at +0.5), edges extended.
fn bilinear(p: &Plane, x: f32, y: f32) -> f32 {
    // (held a pixel outside, which reads the same edge: an infinite coordinate reads the edge
    // instead of a NaN weight and a neighbour index past isize::MAX)
    let (fx, fy) = (x.clamp(-1.0, p.width as f32 + 1.0) - 0.5, y.clamp(-1.0, p.height as f32 + 1.0) - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as isize, y0 as isize);
    let a = p.get_clamped(x0, y0) + (p.get_clamped(x0 + 1, y0) - p.get_clamped(x0, y0)) * tx;
    let b = p.get_clamped(x0, y0 + 1) + (p.get_clamped(x0 + 1, y0 + 1) - p.get_clamped(x0, y0 + 1)) * tx;
    a + (b - a) * ty
}

/// A rough display mapping for colour picking (so samples taken on screen match).
pub(crate) fn tonemap_for_select(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| v / (1.0 + v))
}

/// Colour range: the OkLab distance (a, b at full weight, L at a quarter) at which a pixel leaves
/// the selection for a Refine amount of 0..100. A pixel is fully selected up to half of it and
/// fades out over the other half (CPU here, `mask.wgsl` kind 3; both take this value).
///
/// Issue #157: `0.04 + 0.16·refine` selected the whole photo. Through [`tonemap_for_select`] a
/// slightly teal mid-grey (sRGB 120,150,155) and a neutral light grey (200,198,195) are only ~0.063
/// apart — L 0.60 vs 0.72, a/b ≤ 0.02 — so at the default Refine 50 (full selection below 0.06)
/// the grey patch was 99 % selected, and at Refine 85 / 100 still most of it. A JND is ~0.01 on
/// this scale, so Refine 50 now holds a surface up to ~2 JND from its sample and drops everything
/// beyond ~4.5 (full to 0.0225, gone at 0.045); Refine 85 is gone at 0.066, past that grey patch;
/// Refine 100 reaches 0.075 and Refine 0 is the sample's own shade (full to 0.0075, gone at 0.015).
pub fn color_range_tolerance(refine: f64) -> f32 {
    0.015 + 0.06 * (refine / 100.0).clamp(0.0, 1.0) as f32
}

/// A brush stroke resolved to output pixels: dab centres (spacing r/4 along the path), radius,
/// hard-core radius, flow and density (0..1), and whether it erases.
pub struct BrushDabs {
    pub dabs: Vec<Point>,
    pub r: f64,
    pub hard: f64,
    pub flow: f32,
    pub density: f32,
    pub erase: bool,
    /// Auto Mask: paint only pixels like the one under each dab, then snap to edges.
    pub auto: bool,
}

/// Output pixels a brush sample must move from the last dab to count as movement: below this the
/// pointer is held still (the same screen position mapped again, up to float rounding).
const STATIONARY_PX: f64 = 1e-6;

/// Most dabs one stroke holds (32 MiB of centres). A segment is capped on its own, but a stroke of
/// many hostile segments (an agent, a crafted XMP or catalog) would otherwise add up without
/// bound; past this the rest of the stroke lays nothing (upstream #1520).
pub const MAX_STROKE_DABS: usize = 2_000_000;

/// The dabs of stroke `s` in a `w × h` output.
pub fn brush_dabs(s: &BrushStroke, frame: &Frame, w: usize, h: usize) -> BrushDabs {
    let to_out = frame.norm_to_out(w, h);
    let ppl = frame.px_per_long(w);
    let r = (s.size * ppl).max(0.5);
    let hard = r * (1.0 - (s.feather / 100.0).clamp(0.0, 1.0));
    // Densify the path so dabs overlap (spacing r/4). Coverage builds up by passes, not by how
    // long the button is held: a sample where the pointer hasn't moved (a stationary hold records
    // one per frame) lays no dab, so waiting in place adds nothing (#518).
    let mut dabs: Vec<Point> = Vec::new();
    let mut prev = None;
    for p in &s.points {
        let q = to_out.apply(*p);
        if push_dabs(&mut dabs, prev, q, r) {
            prev = Some(q);
        }
    }
    BrushDabs {
        dabs,
        r,
        hard,
        flow: (s.flow / 100.0).clamp(0.0, 1.0) as f32,
        density: (s.density / 100.0).clamp(0.0, 1.0) as f32,
        erase: s.erase,
        auto: s.auto_mask,
    }
}

/// Distance between consecutive dabs of a brush of radius `r` (pixels): a quarter of the radius,
/// so neighbouring dabs overlap and a stroke reads as continuous however fast it was painted.
#[inline]
pub fn dab_spacing(r: f64) -> f64 {
    (r / 4.0).max(0.5)
}

/// Append the dabs of one path segment, from the previous sample `prev` (already stamped) to `q`,
/// for a brush of radius `r`: evenly spaced at most [`dab_spacing`] apart, ending on `q`. The
/// committed stroke ([`brush_dabs`]) and the live one the UI draws while the button is held place
/// their dabs with this, so they cover the same pixels (issue #517). A sample where the pointer
/// hasn't moved (a stationary hold records one per frame) or that is not a finite point lays
/// nothing, so holding the brush still adds no coverage (#518). Returns whether `q` was stamped;
/// callers move `prev` on only then. A stroke holding [`MAX_STROKE_DABS`] takes no more.
pub fn push_dabs(dabs: &mut Vec<Point>, prev: Option<Point>, q: Point, r: f64) -> bool {
    let room = MAX_STROKE_DABS.saturating_sub(dabs.len());
    if room == 0 || !(q.x.is_finite() && q.y.is_finite()) {
        return false;
    }
    if let Some(prev) = prev {
        let dist = prev.dist(q);
        if dist.is_nan() || dist <= STATIONARY_PX {
            return false;
        }
        let n = (dist / dab_spacing(r)).ceil();
        // a hostile or degenerate segment (huge) gets no in-between dabs rather than a huge
        // allocation: 1 M dabs is far more than any on-screen stroke needs
        let n = if n.is_finite() { n.clamp(0.0, 1_000_000.0) as usize } else { 0 };
        // the in-between dabs and `q` together fit what is left of the stroke's budget
        let n = n.min(room);
        for k in 1..n {
            dabs.push(prev.lerp(q, k as f64 / n as f64));
        }
    }
    dabs.push(q);
    true
}

/// How much one dab paints at distance `dd` from its centre: 1 inside the hard core `hard`,
/// smoothly falling to 0 at the radius `r`.
#[inline]
pub fn dab_alpha(dd: f64, r: f64, hard: f64) -> f32 {
    if dd > r {
        0.0
    } else if dd <= hard {
        1.0
    } else {
        1.0 - smooth(hard as f32, r as f32, dd as f32)
    }
}

/// A stroke's coverage `v` after another dab paints `a` there: flow accumulates up to density.
#[inline]
pub fn accumulate(v: f32, a: f32, flow: f32, density: f32) -> f32 {
    (v + a * flow * (1.0 - v)).min(density)
}

/// Auto Mask tolerances: a pixel takes a dab's paint fully up to half of these from the colour
/// under the dab centre, none beyond them (log2 luminance, chromaticity `rgb / Y`).
pub const AUTO_TOL_EV: f32 = 0.5;
pub const AUTO_TOL_CHROMA: f32 = 0.25;

/// Chromaticity `rgb / Y` (as colour noise reduction uses it).
#[inline]
pub fn chromaticity(c: [f32; 3]) -> [f32; 3] {
    let y = lightcraft_color::luminance_2020(c).max(1e-6);
    [c[0] / y, c[1] / y, c[2] / y]
}

/// Auto Mask: how much a pixel (log luminance `l`, chromaticity `ch`) is like the reference.
#[inline]
pub fn auto_similarity(l: f32, ch: [f32; 3], rl: f32, rch: [f32; 3]) -> f32 {
    let dl = (l - rl) / AUTO_TOL_EV;
    let dc = ((ch[0] - rch[0]).powi(2) + (ch[1] - rch[1]).powi(2) + (ch[2] - rch[2]).powi(2)).sqrt() / AUTO_TOL_CHROMA;
    1.0 - smooth(0.5, 1.0, (dl * dl + dc * dc).sqrt())
}

/// The output pixel a dab at `d` samples its reference colour from. The position is taken at f32
/// precision (like the GPU kernel): a dab on a pixel boundary must not fall on either side of it
/// by an f64 rounding error, which depends on where the window of a zoomed view starts.
#[inline]
pub fn dab_pixel(d: Point, w: usize, h: usize) -> usize {
    let x = ((d.x as f32).floor().max(0.0) as usize).min(w - 1);
    let y = ((d.y as f32).floor().max(0.0) as usize).min(h - 1);
    y * w + x
}

/// Guided-filter refinement of an Auto Mask stroke of radius `r` px: (sigma px, epsilon EV²). The
/// stroke keeps the larger of its own and the refined alpha.
pub fn auto_refine(r: f64) -> (f32, f32) {
    ((0.25 * r).max(1.0) as f32, 0.02)
}

/// Guided filter of `p` steered by `guide` (He et al.), clamped to 0..1: `p`'s edges snap to the
/// guide's.
pub fn guided_cross(guide: &Plane, p: &Plane, sigma: f32, eps: f32) -> Plane {
    use lightcraft_raster::blur::gaussian;
    let mi = gaussian(guide, sigma);
    let mp = gaussian(p, sigma);
    let cip = gaussian(&guide.zip_map(p, |a, b| a * b), sigma);
    let cii = gaussian(&guide.map(|a| a * a), sigma);
    let mut a = Plane::new(p.width, p.height);
    let mut b = Plane::new(p.width, p.height);
    for i in 0..p.data.len() {
        let var = (cii.data[i] - mi.data[i] * mi.data[i]).max(0.0);
        let k = (cip.data[i] - mi.data[i] * mp.data[i]) / (var + eps);
        a.data[i] = k;
        b.data[i] = mp.data[i] - k * mi.data[i];
    }
    let (ma, mb) = (gaussian(&a, sigma), gaussian(&b, sigma));
    let mut q = Plane::new(p.width, p.height);
    for i in 0..q.data.len() {
        q.data[i] = (ma.data[i] * guide.data[i] + mb.data[i]).clamp(0.0, 1.0);
    }
    q
}

/// Stamp brush strokes into `out` (max-combine; erase strokes subtract). Auto Mask strokes weight
/// each dab by [`auto_similarity`] to the pixel under its centre and are refined with
/// [`guided_cross`] on log luminance.
fn rasterize_brush(strokes: &[BrushStroke], frame: &Frame, w: usize, h: usize, img: &Rgb32f, log_l: &Plane, out: &mut Plane) {
    if w == 0 || h == 0 {
        return;
    }
    for s in strokes {
        let BrushDabs { dabs, r, hard, flow, density: dens, erase, auto } = brush_dabs(s, frame, w, h);
        let mut stroke_alpha = Plane::new(w, h);
        for d in &dabs {
            let refc = auto.then(|| {
                let j = dab_pixel(*d, w, h);
                (log_l.data[j], chromaticity(img.data[j]))
            });
            let (x0, x1) = (((d.x - r).floor().max(0.0)) as usize, ((d.x + r).ceil().max(0.0) as usize).min(w));
            let (y0, y1) = (((d.y - r).floor().max(0.0)) as usize, ((d.y + r).ceil().max(0.0) as usize).min(h));
            for y in y0..y1 {
                for x in x0..x1 {
                    let dd = Point::new(x as f64 + 0.5, y as f64 + 0.5).dist(*d);
                    if dd > r {
                        continue;
                    }
                    let mut a = dab_alpha(dd, r, hard);
                    let i = y * w + x;
                    if let Some((rl, rch)) = refc {
                        a *= auto_similarity(log_l.data[i], chromaticity(img.data[i]), rl, rch);
                    }
                    let v = &mut stroke_alpha.data[i];
                    // flow accumulates within a stroke up to density
                    *v = accumulate(*v, a, flow, dens);
                }
            }
        }
        if auto {
            let (sigma, eps) = auto_refine(r);
            let q = guided_cross(log_l, &stroke_alpha, sigma, eps);
            // the refinement fills gaps along edges; it never takes paint away
            stroke_alpha = stroke_alpha.zip_map(&q, f32::max);
        }
        for (o, a) in out.data.iter_mut().zip(&stroke_alpha.data) {
            *o = if erase { *o * (1.0 - a) } else { o.max(*a) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::{DevelopSettings, MaskComponent};

    fn frame(w: usize, h: usize) -> Frame {
        Frame::new(w, h, &DevelopSettings::default(), true)
    }

    #[test]
    fn linear_gradient_ramps() {
        let f = frame(100, 50);
        let img = Rgb32f::new(100, 50);
        let l = Plane::new(100, 50);
        let a = shape_alpha(&MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 0.0) }, &f, 100, 50, &img, &l, 0.0, None);
        assert!(a.get(1, 25) > 0.99 && a.get(98, 25) < 0.01);
        assert!((a.get(50, 25) - 0.5).abs() < 0.05);
    }

    #[test]
    fn stored_segmentations_render_at_any_size_and_follow_the_crop() {
        // the left half selected, on a 16² logit grid
        let side = 16;
        let logits: Vec<f32> = (0..side * side).map(|i| if i % side < side / 2 { 12.0 } else { -12.0 }).collect();
        let seg = SegMask::from_logits(side, &logits);
        for shape in [
            MaskShape::Object { hint: vec![Point::new(0.25, 0.5)], exclude: vec![], seg: Some(seg.clone()), detail: vec![], edge: 0.0 },
            MaskShape::Prompt { text: "left".into(), seg: Some(seg.clone()), detail: vec![], edge: 0.0 },
        ] {
            for (w, h) in [(40, 20), (400, 200)] {
                let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
                assert!(a.get(1, h / 2) > 0.99 && a.get(w - 2, h / 2) < 0.01, "{w}×{h}");
                if w >= 400 {
                    let soft = (0..w).filter(|x| (0.05..0.95).contains(&a.get(*x, h / 2))).count();
                    assert!((2..30).contains(&soft), "an anti-aliased edge, not a ramp: {soft} px");
                }
            }
        }
        // cropped to the right half: nothing left selected
        let mut d = DevelopSettings::default();
        d.crop.geometry.rect = lightcraft_geom::Rect::from_center(Point::new(0.75, 0.5), 0.5, 1.0);
        let f = Frame::new(40, 20, &d, true);
        let shape = MaskShape::Prompt { text: "left".into(), seg: Some(seg), detail: vec![], edge: 0.0 };
        let a = shape_alpha(&shape, &f, 20, 20, &Rgb32f::new(20, 20), &Plane::new(20, 20), 0.0, None);
        assert!(a.data.iter().all(|v| *v < 0.05));
    }

    #[test]
    fn detail_patches_replace_the_coarse_mask_inside_their_rectangle() {
        // coarse: nothing; a patch over the right half selects its own left half (x 0.5..0.75)
        let coarse = SegMask::from_logits(4, &[-12.0; 16]);
        let side = 16;
        let l: Vec<f32> = (0..side * side).map(|i| if i % side < side / 2 { 12.0 } else { -12.0 }).collect();
        let patch = SegMask::from_logits_in(side, &l, [0.5, 0.0, 1.0, 1.0]);
        let shape = MaskShape::Object { hint: vec![Point::new(0.6, 0.5)], exclude: vec![], seg: Some(coarse), detail: vec![patch], edge: 0.0 };
        let (w, h) = (200, 100);
        let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
        assert!(a.get(20, 50) < 0.01, "outside the patch: the coarse mask");
        assert!(a.get(120, 50) > 0.99, "inside the patch, its selected half");
        assert!(a.get(190, 50) < 0.01, "inside the patch, its unselected half");
    }

    #[test]
    fn a_hostile_number_of_detail_patches_is_capped() {
        // built in memory (a deserialized document is capped already): only the first
        // MAX_DETAIL are decoded and sampled, so this needs 4 × 4 MB, not 20 000 × 4 MB
        let side = 1024;
        let l: Vec<f32> = vec![12.0; side * side];
        let patch = SegMask::from_logits_in(side, &l, [0.0, 0.0, 0.5, 1.0]);
        let shape = MaskShape::Object { hint: vec![Point::new(0.2, 0.5)], exclude: vec![], seg: None, detail: vec![patch; 20_000], edge: 0.0 };
        let (w, h) = (64, 32);
        let t = std::time::Instant::now();
        let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
        assert!(a.get(5, 16) > 0.99 && a.get(60, 16) < 0.01);
        assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
    }

    #[test]
    fn the_edge_setting_hardens_or_feathers_the_transition() {
        let side = 16;
        let l: Vec<f32> = (0..side * side).map(|i| ((i % side) as f32 - 7.5) * -0.6).collect(); // a gentle ramp
        let seg = SegMask::from_logits(side, &l);
        let (w, h) = (400, 40);
        let soft_px = |edge: f64| {
            let shape = MaskShape::Prompt { text: "x".into(), seg: Some(seg.clone()), detail: vec![], edge };
            let a = shape_alpha(&shape, &frame(w, h), w, h, &Rgb32f::new(w, h), &Plane::new(w, h), 0.0, None);
            assert!(a.get(2, h / 2) > 0.9 && a.get(w - 3, h / 2) < 0.1, "still the same selection at {edge}");
            (0..w).filter(|x| (0.1..0.9).contains(&a.get(*x, h / 2))).count()
        };
        let (hard, plain, soft) = (soft_px(-100.0), soft_px(0.0), soft_px(100.0));
        assert!(hard < plain && plain < soft, "transition widths {hard} < {plain} < {soft}");
    }

    #[test]
    fn ai_shapes_without_a_segmentation_select_nothing() {
        let f = frame(30, 30);
        let (img, l) = (Rgb32f::new(30, 30), Plane::new(30, 30));
        for shape in [
            MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
            MaskShape::Prompt { text: "sky".into(), seg: None, detail: vec![], edge: 0.0 },
            MaskShape::Prompt { text: "sky".into(), seg: Some(SegMask { side: 4, data: "damaged!".into(), rect: None }), detail: vec![], edge: 0.0 },
        ] {
            assert!(shape_alpha(&shape, &f, 30, 30, &img, &l, 0.0, None).data.iter().all(|v| *v == 0.0));
        }
    }

    /// A grey scene (`f` gives each pixel's linear value) and its log luminance.
    fn grey_scene(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> (Rgb32f, Plane) {
        let img = Rgb32f::from_fn(w, h, |x, y| {
            let v = f(x, y);
            [v, v, v]
        });
        let l = img.map(crate::local::log_lum);
        (img, l)
    }

    fn disc(x: usize, y: usize, c: (f32, f32), r: f32) -> bool {
        ((x as f32 + 0.5 - c.0).powi(2) + (y as f32 + 0.5 - c.1).powi(2)).sqrt() < r
    }

    fn subject_of(img: &Rgb32f, l: &Plane) -> Plane {
        shape_alpha(&MaskShape::Subject, &frame(img.width, img.height), img.width, img.height, img, l, 0.0, None)
    }

    #[test]
    fn subject_finds_an_off_centre_bright_disc_in_every_quadrant() {
        let (w, h) = (320, 240);
        let r = 0.1 * w as f32;
        let quadrants = [(0.25, 0.3), (0.75, 0.3), (0.25, 0.75), (0.75, 0.75)];
        for (qx, qy) in quadrants {
            let c = (qx * w as f32, qy * h as f32);
            let (img, l) = grey_scene(w, h, |x, y| if disc(x, y, c, r) { 0.8 } else { 0.04 });
            let a = subject_of(&img, &l);
            assert!(a.data.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            let at = |dx: f32, dy: f32| a.get((c.0 + dx) as usize, (c.1 + dy) as usize);
            assert!(at(0.0, 0.0) > 0.8 && at(0.5 * r, 0.0) > 0.8, "inside, quadrant {qx},{qy}: {} {}", at(0.0, 0.0), at(0.5 * r, 0.0));
            // outside: a disc radius beyond the edge, and the opposite quadrant's centre
            let opposite = a.get(((1.0 - qx) * w as f32) as usize, ((1.0 - qy) * h as f32) as usize);
            assert!(opposite < 0.2 && at(0.0, 2.5 * r * if qy < 0.5 { 1.0 } else { -1.0 }) < 0.2, "outside, quadrant {qx},{qy}: {opposite}");
        }
    }

    #[test]
    fn subject_of_a_uniform_field_is_finite_and_weak() {
        for (w, h) in [(320, 240), (64, 48), (5, 3), (1, 1)] {
            let (img, l) = grey_scene(w, h, |_, _| 0.3);
            let a = subject_of(&img, &l);
            assert_eq!((a.width, a.height), (w, h));
            assert!(a.data.iter().all(|v| v.is_finite() && (0.0..=0.5 + 1e-4).contains(v)), "{w}×{h}");
            // (a frame of a few pixels is nearly all ellipse: only the range is checked there)
            let mean = a.data.iter().sum::<f32>() / a.data.len() as f32;
            assert!(w < 64 || mean < 0.3, "{w}×{h}: mean {mean}");
        }
    }

    #[test]
    fn subject_and_background_sum_to_one() {
        let (w, h) = (320, 240);
        let c = (0.3 * w as f32, 0.4 * h as f32);
        for (img, l) in [grey_scene(w, h, |x, y| if disc(x, y, c, 40.0) { 0.7 } else { 0.05 }), grey_scene(w, h, |_, _| 0.2)] {
            let f = frame(w, h);
            let s = shape_alpha(&MaskShape::Subject, &f, w, h, &img, &l, 0.0, None);
            let b = shape_alpha(&MaskShape::Background, &f, w, h, &img, &l, 0.0, None);
            assert!(s.data.iter().zip(&b.data).all(|(s, b)| (s + b - 1.0).abs() < 1e-5));
        }
    }

    #[test]
    fn subject_keeps_the_larger_of_two_blobs() {
        let (w, h) = (320, 240);
        let (big, small) = ((0.3 * w as f32, 0.5 * h as f32), (0.8 * w as f32, 0.25 * h as f32));
        // areas 0.15 w vs 0.06 w radius: about 6 : 1
        let (img, l) = grey_scene(w, h, |x, y| if disc(x, y, big, 0.15 * w as f32) || disc(x, y, small, 0.06 * w as f32) { 0.8 } else { 0.04 });
        let a = subject_of(&img, &l);
        assert!(a.get(big.0 as usize, big.1 as usize) > 0.8, "the big blob: {}", a.get(big.0 as usize, big.1 as usize));
        assert!(a.get(small.0 as usize, small.1 as usize) < 0.2, "the small blob: {}", a.get(small.0 as usize, small.1 as usize));
    }

    /// A bright block off the frame edges: its mask edge lies within 2 px of the block's edge. The
    /// region is found on a plane half this size (512 px → 256), so it is placed to within one
    /// work pixel (2 px) before the guided filter pulls it onto the photo's step edge.
    #[test]
    fn subject_edge_snaps_to_a_step_edge() {
        let (w, h) = (512, 384);
        let (img, l) = grey_scene(w, h, |x, y| if (150..330).contains(&x) && (100..284).contains(&y) { 0.8 } else { 0.04 });
        let a = subject_of(&img, &l);
        let row = h / 2;
        let first = (0..w).find(|x| a.get(*x, row) >= 0.5).expect("a selected pixel");
        let last = (0..w).rev().find(|x| a.get(*x, row) >= 0.5).expect("a selected pixel");
        assert!((first as f32 - 150.0).abs() <= 2.0, "left edge at {first}, step at 150");
        assert!((last as f32 - 329.0).abs() <= 2.0, "right edge at {last}, step at 329");
        assert!(a.get(240, row) > 0.9 && a.get(40, row) < 0.1 && a.get(470, row) < 0.1);
    }

    #[test]
    fn a_source_matte_still_overrides_the_subject_heuristic() {
        let (w, h) = (64, 48);
        let (img, l) = grey_scene(w, h, |_, _| 0.3);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Person, Image::filled(8, 8, 255u8));
        let f = frame(w, h);
        let with = |shape: &MaskShape| shape_alpha(shape, &f, w, h, &img, &l, 0.0, Some(&mattes));
        assert!(with(&MaskShape::Subject).data.iter().all(|v| *v > 0.99), "the person matte wins over the heuristic (weak on a flat frame)");
        assert!(with(&MaskShape::Background).data.iter().all(|v| *v < 0.01));
    }

    #[test]
    fn radial_inside_and_invert() {
        let f = frame(100, 100);
        let img = Rgb32f::new(100, 100);
        let l = Plane::new(100, 100);
        let shape = MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.2, ry: 0.2, angle: 0.0, feather: 20.0, invert: false };
        let a = shape_alpha(&shape, &f, 100, 100, &img, &l, 0.0, None);
        assert!(a.get(50, 50) > 0.99 && a.get(5, 5) < 0.01);
        let m = Mask { components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: true, shape }], ..Default::default() };
        let e = evaluate(&[m], &f, 100, 100, &img, &l, 0.0, None);
        assert!(e[0].alpha.get(50, 50) < 0.01);
    }

    /// Issue #517: dabs along a long segment (a fast stroke: two pointer samples far apart) are at
    /// most [`dab_spacing`] apart, painting dab by dab (the live stroke) or the whole path at once
    /// (the committed one).
    #[test]
    fn a_long_segment_gets_dabs_at_most_the_spacing_apart() {
        let r = 12.0;
        let pts = [Point::new(10.0, 20.0), Point::new(610.0, 260.0), Point::new(611.0, 260.0), Point::new(40.0, 400.0)];
        let mut live = Vec::new();
        let mut prev = None;
        for q in pts {
            if push_dabs(&mut live, prev, q, r) {
                prev = Some(q);
            }
        }
        assert!(live.len() > 3 * 600 / 12, "{} dabs", live.len());
        assert!(live.windows(2).all(|w| w[0].dist(w[1]) <= dab_spacing(r) + 1e-9));
        assert_eq!(live.first(), pts.first());
        assert_eq!(live.last(), pts.last());
        // the committed stroke places the same dabs (in output pixels; this frame maps 1:1 scaled)
        let (w, h) = (1000, 500);
        let f = frame(w, h);
        let stroke = BrushStroke { points: pts.map(|p| Point::new(p.x / w as f64, p.y / h as f64)).to_vec(), ..Default::default() };
        let committed = brush_dabs(&stroke, &f, w, h);
        let mut again = Vec::new();
        let mut prev = None;
        for p in &stroke.points {
            let q = f.norm_to_out(w, h).apply(*p);
            if push_dabs(&mut again, prev, q, committed.r) {
                prev = Some(q);
            }
        }
        assert_eq!(committed.dabs, again);
        assert!(committed.dabs.windows(2).all(|w| w[0].dist(w[1]) <= dab_spacing(committed.r) + 1e-9));
        // many huge segments stop at the stroke budget instead of adding up without bound
        let mut big = Vec::new();
        let mut at = Point::new(0.0, 0.0);
        for i in 0..5 {
            let q = Point::new(if i % 2 == 0 { 1e12 } else { 0.0 }, 0.0);
            if push_dabs(&mut big, Some(at), q, 1.0) {
                at = q;
            }
        }
        assert!(big.len() <= MAX_STROKE_DABS, "{} dabs", big.len());
        assert!(!push_dabs(&mut big, Some(at), Point::new(5.0, 5.0), 1.0), "a full stroke takes no more");
        // a hostile segment places no in-between dabs
        let mut v = Vec::new();
        assert!(!push_dabs(&mut v, Some(Point::new(f64::NAN, 0.0)), Point::new(1.0, 1.0), r));
        assert!(!push_dabs(&mut v, None, Point::new(f64::INFINITY, 1.0), r));
        push_dabs(&mut v, Some(Point::new(0.0, 0.0)), Point::new(1e300, 1.0), 0.0);
        assert!(v.len() <= 1_000_002, "{}", v.len());
    }

    #[test]
    fn brush_and_subtract() {
        let f = frame(200, 100);
        let img = Rgb32f::new(200, 100);
        let l = Plane::new(200, 100);
        let stroke = BrushStroke { points: vec![Point::new(0.1, 0.5), Point::new(0.9, 0.5)], size: 0.05, ..Default::default() };
        let erase = BrushStroke { points: vec![Point::new(0.5, 0.5)], size: 0.05, erase: true, feather: 0.0, ..Default::default() };
        let m = Mask {
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes: vec![stroke, erase] } }],
            ..Default::default()
        };
        let e = evaluate(&[m], &f, 200, 100, &img, &l, 0.0, None);
        let a = &e[0].alpha;
        assert!(a.get(40, 50) > 0.9, "{}", a.get(40, 50));
        assert!(a.get(100, 50) < 0.05, "erased centre {}", a.get(100, 50));
        assert!(a.get(40, 5) < 0.01);
    }

    /// #518: holding the brush still adds no coverage. A press plus any number of stationary
    /// pointer samples paints exactly the single dab of the press, whatever the frame rate.
    #[test]
    fn holding_the_brush_still_adds_no_coverage() {
        let (w, h) = (200, 100);
        let f = frame(w, h);
        let img = Rgb32f::new(w, h);
        let l = Plane::new(w, h);
        let p = Point::new(0.5, 0.5);
        let spot = |samples: usize, flow: f64| {
            let s = BrushStroke { points: vec![p; samples], size: 0.1, feather: 80.0, flow, ..Default::default() };
            shape_alpha(&MaskShape::Brush { strokes: vec![s] }, &f, w, h, &img, &l, 0.0, None)
        };
        for flow in [100.0, 40.0] {
            let press = spot(1, flow);
            // the feathered edge is soft to begin with
            let edge = press.get(100 + 14, 50);
            assert!(edge > 0.05 && edge < 0.6, "flow {flow}: edge {edge}");
            for n in [2, 5, 60, 600] {
                let held = spot(n, flow);
                assert!(held.data == press.data, "flow {flow}: {n} stationary samples hardened the edge {edge} → {}", held.get(100 + 14, 50));
            }
        }
        let s = BrushStroke { points: vec![p; 60], size: 0.1, ..Default::default() };
        assert_eq!(brush_dabs(&s, &f, w, h).dabs.len(), 1, "one dab for the press");
        // a stroke that pauses partway lays the same dabs as one that doesn't
        let (a, b) = (Point::new(0.2, 0.5), Point::new(0.8, 0.5));
        let paused = BrushStroke { points: vec![a, p, p, p, p, b], flow: 30.0, ..Default::default() };
        let smooth = BrushStroke { points: vec![a, p, b], flow: 30.0, ..Default::default() };
        assert_eq!(brush_dabs(&paused, &f, w, h).dabs, brush_dabs(&smooth, &f, w, h).dabs);
    }

    /// Flow builds up by passes: going back over an area in the same stroke adds coverage.
    #[test]
    fn a_second_pass_builds_up_with_low_flow() {
        let (w, h) = (200, 100);
        let f = frame(w, h);
        let img = Rgb32f::new(w, h);
        let l = Plane::new(w, h);
        let (a, b) = (Point::new(0.2, 0.5), Point::new(0.8, 0.5));
        let paint = |points: Vec<Point>| {
            let s = BrushStroke { points, size: 0.05, feather: 50.0, flow: 20.0, ..Default::default() };
            shape_alpha(&MaskShape::Brush { strokes: vec![s] }, &f, w, h, &img, &l, 0.0, None)
        };
        let once = paint(vec![a, b]);
        let twice = paint(vec![a, b, a]);
        let thrice = paint(vec![a, b, a, b]);
        let (o, t, th) = (once.get(100, 50), twice.get(100, 50), thrice.get(100, 50));
        assert!(o > 0.05 && o < 0.99, "one pass {o}");
        assert!(t > o + 0.02 && th > t + 0.01, "passes build up: {o} → {t} → {th}");
    }

    #[test]
    fn auto_mask_stops_at_edges() {
        // dark left half, bright right half; a stroke along the boundary, centred on the dark side
        let (w, h) = (200, 100);
        let f = frame(w, h);
        let img = Rgb32f::from_fn(w, h, |x, _| if x < 100 { [0.03; 3] } else { [0.6, 0.5, 0.4] });
        let l = img.map(crate::local::log_lum);
        let stroke = |auto_mask| BrushStroke {
            points: vec![Point::new(0.47, 0.2), Point::new(0.47, 0.8)],
            size: 0.08,
            feather: 0.0,
            auto_mask,
            ..Default::default()
        };
        let comp = |auto| MaskShape::Brush { strokes: vec![stroke(auto)] };
        let plain = shape_alpha(&comp(false), &f, w, h, &img, &l, 0.0, None);
        let auto = shape_alpha(&comp(true), &f, w, h, &img, &l, 0.0, None);
        // without Auto Mask the brush spills over the edge; with it, it stays on the dark side
        assert!(plain.get(105, 50) > 0.9, "{}", plain.get(105, 50));
        assert!(auto.get(105, 50) < 0.05, "spill {}", auto.get(105, 50));
        assert!(auto.get(110, 50) < 0.02);
        assert!(auto.get(90, 50) > 0.9, "painted side {}", auto.get(90, 50));
        assert!(auto.get(99, 50) > 0.8, "up to the edge {}", auto.get(99, 50));
        // on a flat area Auto Mask paints like the plain brush
        let flat = Rgb32f::from_fn(w, h, |_, _| [0.2; 3]);
        let fl = flat.map(crate::local::log_lum);
        let a = shape_alpha(&comp(true), &f, w, h, &flat, &fl, 0.0, None);
        let b = shape_alpha(&comp(false), &f, w, h, &flat, &fl, 0.0, None);
        assert!((a.get(94, 50) - b.get(94, 50)).abs() < 0.02, "{} vs {}", a.get(94, 50), b.get(94, 50));
    }

    /// Refine Edges: a soft mask over a hard edge in the photo snaps to that edge.
    #[test]
    fn refine_edges_follows_the_photo() {
        let (w, h) = (120usize, 80usize);
        // left half dark, right half bright
        let img = Rgb32f::from_fn(w, h, |x, _| if x < 60 { [0.05; 3] } else { [0.6; 3] });
        let log_l = Plane::from_fn(w, h, |x, _| if x < 60 { (0.05f32).log2() } else { (0.6f32).log2() });
        let f = Frame::new(w, h, &Default::default(), true);
        let soft = Mask {
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Linear { start: Point::new(0.75, 0.5), end: Point::new(0.25, 0.5) },
            }],
            ..Default::default()
        };
        let refined = Mask { refine: 100.0, ..soft.clone() };
        let step = |m: &Mask| {
            let a = evaluate_one(m, &f, w, h, &img, &log_l, 0.0, None);
            a.data[40 * w + 63] - a.data[40 * w + 56]
        };
        let (plain, sharp) = (step(&soft), step(&refined));
        assert!(sharp > plain * 1.5 && sharp > 0.1, "the edge in the mask follows the photo's: {plain} → {sharp}");
    }

    /// A soft left-to-right ramp mask over a photo with a hard luminance step at x = 0.5 of the
    /// width, evaluated at `w × (2w/3)` pixels; returns the alpha row through the middle.
    fn ramp_row(w: usize, feather: f64, refine: f64) -> Vec<f32> {
        let h = w * 2 / 3;
        let step = w / 2;
        let img = Rgb32f::from_fn(w, h, |x, _| if x < step { [0.05; 3] } else { [0.6; 3] });
        let log_l = Plane::from_fn(w, h, |x, _| if x < step { (0.05f32).log2() } else { (0.6f32).log2() });
        let f = Frame::new(w, h, &Default::default(), true);
        let m = Mask {
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Linear { start: Point::new(0.75, 0.5), end: Point::new(0.25, 0.5) },
            }],
            feather,
            refine,
            ..Default::default()
        };
        let a = evaluate_one(&m, &f, w, h, &img, &log_l, 0.0, None);
        a.data[(h / 2) * w..(h / 2 + 1) * w].to_vec()
    }

    /// Feather 0 is the mask as drawn, and a default `Mask` deserializes from an old edit with it.
    #[test]
    fn feather_zero_is_the_mask_as_drawn() {
        let a = ramp_row(120, 0.0, 0.0);
        let b = ramp_row(120, 0.0, 0.0);
        assert_eq!(a, b);
        let old: Mask = serde_json::from_str(r#"{"id":3}"#).unwrap_or_default();
        assert_eq!(old.feather, 0.0);
        // a hard-edged shape stays hard at 0 and softens above it
        let hard = |feather: f64| {
            let (w, h) = (100usize, 60usize);
            let img = Rgb32f::from_fn(w, h, |_, _| [0.3; 3]);
            let log_l = img.map(crate::local::log_lum);
            let f = Frame::new(w, h, &Default::default(), true);
            let m = Mask {
                components: vec![MaskComponent {
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Radial { center: Point::new(0.5, 0.3), rx: 0.2, ry: 0.2, angle: 0.0, feather: 0.0, invert: false },
                }],
                feather,
                ..Default::default()
            };
            evaluate_one(&m, &f, w, h, &img, &log_l, 0.0, None)
        };
        let (zero, soft) = (hard(0.0), hard(60.0));
        let edge = |p: &Plane| (0..100).map(|x| p.data[18 * 100 + x]).filter(|v| *v > 0.05 && *v < 0.95).count();
        assert!(edge(&soft) > edge(&zero) + 2, "feather softens: {} vs {}", edge(&zero), edge(&soft));
    }

    /// Edge refinement keeps the mask's transition on the photo's luminance step.
    #[test]
    fn refined_edge_stays_on_the_luminance_step() {
        let w = 240;
        let row = ramp_row(w, 0.0, 100.0);
        let step = w / 2;
        // the mask's 0.5 crossing, interpolated, sits within 2 px of the step
        let i = (1..w).find(|&i| row[i - 1] < 0.5 && row[i] >= 0.5).or_else(|| (1..w).find(|&i| row[i - 1] >= 0.5 && row[i] < 0.5));
        let i = i.expect("the refined mask crosses 0.5");
        assert!((i as i64 - step as i64).abs() <= 2, "crossing at {i}, step at {step}");
    }

    /// Normalized radii: the same feather / refine at two resolutions gives the same alpha.
    #[test]
    fn feather_and_refine_do_not_depend_on_the_resolution() {
        for (feather, refine) in [(50.0, 0.0), (0.0, 60.0), (40.0, 40.0)] {
            let (small, big) = (ramp_row(150, feather, refine), ramp_row(300, feather, refine));
            let worst = (0..150).map(|x| (small[x] - (big[2 * x] + big[2 * x + 1]) / 2.0).abs()).fold(0.0f32, f32::max);
            assert!(worst < 0.08, "feather {feather} refine {refine}: differs by {worst}");
        }
    }

    /// Issue #157: a colour-range sample of one flat patch must not select a clearly different
    /// patch. The two patches are the ones from the report (sRGB 120,150,155 and 200,198,195),
    /// sampled the way `mask.sampleColor` does (3×3 mean of the selection-space OkLab).
    #[test]
    fn color_range_keeps_to_the_sampled_colour() {
        use lightcraft_color::{REC2020, SRGB, transfer::decode_srgb8};
        let (w, h) = (120usize, 40usize);
        let m = SRGB.to_space(&REC2020);
        let patch = |rgb: [u8; 3]| m.apply_f32(rgb.map(decode_srgb8));
        let (left, right) = (patch([120, 150, 155]), patch([200, 198, 195]));
        let img = Rgb32f::from_fn(w, h, |x, _| if x < 60 { left } else { right });
        let log_l = img.map(crate::local::log_lum);
        let f = Frame::new(w, h, &Default::default(), true);
        let mut sample = [0f64; 3];
        for y in 19..=21 {
            for x in 29..=31 {
                let lab = lightcraft_color::perceptual::oklab_from_2020(tonemap_for_select(img.data[y * w + x]));
                for k in 0..3 {
                    sample[k] += lab[k] as f64 / 9.0;
                }
            }
        }
        let alpha = |refine| {
            let shape = MaskShape::ColorRange { samples: vec![sample], refine };
            let a = shape_alpha(&shape, &f, w, h, &img, &log_l, 0.0, None);
            (a.get(30, 20), a.get(90, 20))
        };
        for refine in [0.0, 50.0, 85.0] {
            let (l, r) = alpha(refine);
            assert!(l > 0.95, "refine {refine}: sampled patch {l}");
            assert!(r < 0.05, "refine {refine}: other patch {r}");
        }
        // Refine 100 reaches furthest (the grey is ~0.063 away, the fade ends at 0.075), but the
        // other patch stays mostly out
        let (l, r) = alpha(100.0);
        assert!(l > 0.95 && r < 0.3, "refine 100: {l} vs {r}");
    }

    /// A half-resolution matte selecting the left `frac` of a `w × h` source.
    fn left_matte(w: usize, h: usize, frac: f64) -> Image<u8> {
        Image::from_fn(w / 2, h / 2, |x, _| if (x as f64 + 0.5) < frac * (w / 2) as f64 { 255 } else { 0 })
    }

    /// The Sky mask follows the source's sky matte where it has one (here: the bottom-left of a
    /// uniformly bright blue photo, where the heuristic sees no sky, and not its top right, where
    /// it does), through user orientation, at any output size; without one it keeps the heuristic.
    #[test]
    fn sky_uses_the_sources_matte() {
        let (w, h) = (200, 100);
        let img = Rgb32f::filled(w, h, [0.5, 0.7, 1.4]);
        let l = img.map(crate::local::log_lum);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Sky, left_matte(w, h, 0.5));
        let f = frame(w, h);
        let heuristic = shape_alpha(&MaskShape::Sky, &f, w, h, &img, &l, 0.0, None);
        let matte = shape_alpha(&MaskShape::Sky, &f, w, h, &img, &l, 0.0, Some(&mattes));
        assert!(heuristic.get(150, 5) > 0.9 && heuristic.get(25, 90) < 0.1, "{} {}", heuristic.get(150, 5), heuristic.get(25, 90));
        assert!(matte.get(150, 5) < 0.01 && matte.get(25, 90) > 0.99, "{} {}", matte.get(150, 5), matte.get(25, 90));
        // mattes of other kinds leave the heuristic alone
        let mut hair = Mattes::default();
        hair.push(MatteKind::Hair, left_matte(w, h, 0.5));
        assert_eq!(shape_alpha(&MaskShape::Sky, &f, w, h, &img, &l, 0.0, Some(&hair)), heuristic);
        // rotated 90° clockwise, the source's left half is the top half; a preview and a larger
        // render agree
        let s = lightcraft_develop::DevelopSettings { orientation: Orientation::Rotate90, ..Default::default() };
        let f = Frame::new(w, h, &s, true);
        for (ow, oh) in [(100, 200), (25, 50)] {
            let small = Rgb32f::filled(ow, oh, [0.5, 0.7, 1.4]);
            let a = shape_alpha(&MaskShape::Sky, &f, ow, oh, &small, &small.map(crate::local::log_lum), 0.0, Some(&mattes));
            let at = |x: f64, y: f64| a.get((x * ow as f64) as usize, (y * oh as f64) as usize);
            assert!(at(0.5, 0.2) > 0.99 && at(0.5, 0.8) < 0.01, "{ow}×{oh}: {} {}", at(0.5, 0.2), at(0.5, 0.8));
        }
    }

    /// Subject, Background and People use person mattes: the whole person is the union of the
    /// parts; a People part without a matte keeps the heuristic.
    #[test]
    fn people_and_subject_use_person_mattes() {
        let (w, h) = (200, 100);
        let img = Rgb32f::filled(w, h, [0.2; 3]);
        let l = img.map(crate::local::log_lum);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Skin, left_matte(w, h, 0.25));
        let right: Image<u8> = Image::from_fn(w / 2, h / 2, |x, _| if x >= 75 { 255 } else { 0 });
        mattes.push(MatteKind::Hair, right);
        let f = frame(w, h);
        let eval = |shape: &MaskShape| shape_alpha(shape, &f, w, h, &img, &l, 0.0, Some(&mattes));
        let subject = eval(&MaskShape::Subject);
        assert!(subject.get(10, 50) > 0.99 && subject.get(190, 50) > 0.99 && subject.get(100, 50) < 0.01);
        let background = eval(&MaskShape::Background);
        assert!(background.get(100, 50) > 0.99 && background.get(10, 50) < 0.01);
        let hair = eval(&MaskShape::People { person: 0, parts: vec!["Hair".into()] });
        assert!(hair.get(190, 50) > 0.99 && hair.get(10, 50) < 0.01);
        let lips = MaskShape::People { person: 0, parts: vec!["Lips".into()] };
        assert_eq!(eval(&lips), shape_alpha(&lips, &f, w, h, &img, &l, 0.0, None));
    }

    // ---- Sky connectivity and Landscape classes (synthetic scenes, 256 × 160 so the work plane is the picture) ----

    /// Deterministic per-pixel noise 0..1.
    fn noise(x: usize, y: usize) -> f32 {
        let n = (x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663);
        ((n ^ (n >> 13)).wrapping_mul(0x5bd1e995) >> 8) as f32 / 16_777_216.0
    }

    /// Strongly textured grey-brown ground (about 3 EV of per-pixel luminance noise, far over the texture limit).
    fn rough(x: usize, y: usize) -> [f32; 3] {
        let v = 0.05 + 0.5 * noise(x, y);
        [v * 1.1, v, v * 0.9]
    }

    /// Smooth blue sky whose luminance falls by 0.3 over `rows`, from `y0`.
    fn blue(y: usize, y0: usize, rows: usize) -> [f32; 3] {
        let s = 1.0 - 0.3 * (y - y0) as f32 / rows as f32;
        [0.4 * s, 0.6 * s, 1.2 * s]
    }

    fn scene_of(shape: &MaskShape, img: &Rgb32f) -> Plane {
        let l = img.map(crate::local::log_lum);
        shape_alpha(shape, &frame(img.width, img.height), img.width, img.height, img, &l, 0.0, None)
    }

    fn class(name: &str) -> MaskShape {
        MaskShape::Landscape { class: name.into() }
    }

    /// Sky over rows 0..30, a textured band (rows 30..50), a smooth blue patch not connected to the
    /// top (rows 50..90, well above the horizon cut, so only connectivity can exclude it), textured
    /// ground below.
    fn lake_scene() -> Rgb32f {
        Rgb32f::from_fn(256, 160, |x, y| match y {
            0..30 => blue(y, 0, 30),
            30..50 => rough(x, y),
            50..90 => [0.32, 0.48, 0.96],
            _ => rough(x, y),
        })
    }

    #[test]
    fn sky_is_high_at_the_top_and_low_on_textured_ground() {
        // blue gradient over the top half, textured ground below
        let img = Rgb32f::from_fn(256, 160, |x, y| if y < 80 { blue(y, 0, 80) } else { rough(x, y) });
        let a = scene_of(&MaskShape::Sky, &img);
        for (x, y) in [(10, 5), (128, 10), (245, 20), (128, 40)] {
            assert!(a.get(x, y) > 0.9, "sky at {x},{y}: {}", a.get(x, y));
        }
        for (x, y) in [(10, 150), (128, 120), (245, 100), (128, 155)] {
            assert!(a.get(x, y) < 0.05, "ground at {x},{y}: {}", a.get(x, y));
        }
        assert!(a.data.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    }

    #[test]
    fn a_blue_patch_below_the_horizon_is_not_sky_but_water() {
        let img = lake_scene();
        let sky = scene_of(&MaskShape::Sky, &img);
        assert!(sky.get(128, 10) > 0.9, "sky {}", sky.get(128, 10));
        assert!(sky.get(128, 70) < 0.1, "a smooth blue patch not connected to the top is no sky: {}", sky.get(128, 70));
        let water = scene_of(&class("water"), &img);
        assert!(water.get(128, 70) > 0.8, "water {}", water.get(128, 70));
        assert!(water.get(128, 10) < 0.1 && water.get(128, 140) < 0.1, "{} {}", water.get(128, 10), water.get(128, 140));
    }

    /// A green textured band under the sky is vegetation (not sky, not ground); the ground below is ground.
    #[test]
    fn a_green_textured_band_is_vegetation() {
        let img = Rgb32f::from_fn(256, 160, |x, y| match y {
            0..40 => blue(y, 0, 40),
            40..100 => {
                let n = noise(x, y);
                [0.05 + 0.1 * n, 0.2 + 0.4 * n, 0.04 + 0.05 * n]
            }
            _ => rough(x, y),
        });
        let veg = scene_of(&class("Vegetation"), &img);
        assert!(veg.get(128, 70) > 0.8, "vegetation {}", veg.get(128, 70));
        assert!(veg.get(128, 20) < 0.1 && veg.get(128, 140) < 0.1, "{} {}", veg.get(128, 20), veg.get(128, 140));
        assert!(scene_of(&MaskShape::Sky, &img).get(128, 70) < 0.1, "foliage is not sky");
        let ground = scene_of(&class("ground"), &img);
        assert!(ground.get(128, 140) > 0.7 && ground.get(128, 70) < 0.2 && ground.get(128, 20) < 0.1);
        let sky = scene_of(&class("sky"), &img);
        assert!(sky.get(128, 10) > 0.9 && sky.get(128, 140) < 0.05);
    }

    #[test]
    fn landscape_classes_without_a_credible_decision_select_nothing() {
        let img = lake_scene();
        for name in ["mountains", "architecture", "buildings", "artificial ground", "", "nonsense"] {
            assert!(scene_of(&class(name), &img).data.iter().all(|v| *v == 0.0), "{name}");
        }
        // names are case-insensitive and read - / _ as a space
        assert_eq!(scene_of(&class("Natural_Ground"), &img), scene_of(&class("natural ground"), &img));
        assert_eq!(scene_of(&class("SKY"), &img), scene_of(&MaskShape::Sky, &img));
    }

    #[test]
    fn landscape_sky_follows_the_sky_matte() {
        let (w, h) = (200, 100);
        let img = Rgb32f::filled(w, h, [0.5, 0.7, 1.4]);
        let l = img.map(crate::local::log_lum);
        let mut mattes = Mattes::default();
        mattes.push(MatteKind::Sky, left_matte(w, h, 0.5));
        let a = shape_alpha(&class("sky"), &frame(w, h), w, h, &img, &l, 0.0, Some(&mattes));
        assert!(a.get(150, 5) < 0.01 && a.get(25, 90) > 0.99);
    }

    #[test]
    fn sky_and_landscape_are_finite_on_degenerate_input() {
        for (w, h) in [(1, 1), (3, 3), (7, 40), (40, 7), (64, 48)] {
            for img in [Rgb32f::filled(w, h, [0.0; 3]), Rgb32f::filled(w, h, [1.0e6; 3]), Rgb32f::filled(w, h, [0.5, 0.7, 1.4])] {
                for shape in [MaskShape::Sky, class("water"), class("vegetation"), class("ground")] {
                    let a = scene_of(&shape, &img);
                    assert_eq!((a.width, a.height), (w, h));
                    assert!(a.data.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{w}x{h}");
                }
            }
        }
    }

    /// A sunset: a smooth orange to pink-red gradient over the top half, dark textured ground below.
    #[test]
    fn a_warm_sunset_gradient_is_sky() {
        let img = Rgb32f::from_fn(256, 160, |x, y| {
            if y < 80 {
                let t = y as f32 / 80.0;
                [0.9, 0.5 - 0.1 * t, 0.25 + 0.25 * t]
            } else {
                let v = 0.02 + 0.1 * noise(x, y);
                [v * 1.2, v, v * 0.8]
            }
        });
        let a = scene_of(&MaskShape::Sky, &img);
        for (x, y) in [(10, 5), (128, 10), (245, 20), (128, 40), (30, 60)] {
            assert!(a.get(x, y) > 0.8, "warm sky at {x},{y}: {}", a.get(x, y));
        }
        for (x, y) in [(10, 150), (128, 120), (245, 100)] {
            assert!(a.get(x, y) < 0.05, "ground at {x},{y}: {}", a.get(x, y));
        }
    }

    /// A smooth warm-beige wall of mid luminance filling the frame and touching the top is no sky.
    #[test]
    fn a_warm_beige_wall_touching_the_top_is_not_sky() {
        let img = Rgb32f::filled(256, 160, [0.3, 0.25, 0.18]);
        let a = scene_of(&MaskShape::Sky, &img);
        for (x, y) in [(128, 3), (10, 20), (245, 40), (128, 100), (30, 150)] {
            assert!(a.get(x, y) < 0.1, "wall at {x},{y}: {}", a.get(x, y));
        }
        // a wall with the same colour but textured, under a blue sky, stays out too
        let img = Rgb32f::from_fn(256, 160, |x, y| if y < 40 { blue(y, 0, 40) } else { [0.3, 0.25, 0.18].map(|c| c * (0.6 + 0.8 * noise(x, y))) });
        let a = scene_of(&MaskShape::Sky, &img);
        assert!(a.get(128, 10) > 0.9 && a.get(128, 100) < 0.05, "{} {}", a.get(128, 10), a.get(128, 100));
    }
}
