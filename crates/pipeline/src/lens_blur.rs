//! Lens Blur: a depth-aware bokeh blur in scene-linear light (issue #54; see `docs/lens-blur.md`).
//!
//! No depth model ships, so the depth is a two-layer proxy taken from the Subject matte
//! ([`masks::subject_matte`]): the subject sits at [`SUBJECT_DEPTH`], the background at
//! [`BACKGROUND_DEPTH`], and the matte's soft edge is the transition between them. The focal band
//! (`focal_near`..`focal_far`, in the same 0..100 units) stays sharp; the further a pixel's depth is
//! from the band, the larger its circle of confusion, up to [`MAX_RADIUS`] of the long edge at
//! Amount 100 (reached [`RAMP`] depth units from the band).
//!
//! The render is a gather over a Vogel disc (a deterministic spiral that fills a disc evenly),
//! read from a box-filtered pyramid so a large disc needs only 12..48 taps without aliasing:
//!
//! - **Radius** is a share of the output's long edge ([`LensBlur`] is resolution independent), and
//!   the Subject decision is made on a fixed-size proxy of the whole frame, so a preview and an
//!   export of the same photo blur the same regions by the same share of the picture.
//! - **No halos:** a tap only counts when its own circle of confusion reaches the pixel being
//!   drawn (scatter-as-gather), so a sharp subject never smears into the blurred background.
//! - **Highlight boost:** pixels are averaged with a weight that grows with their scene-linear
//!   luminance, so highlights bloom into bright discs instead of averaging away.
//! - **Cat's eye:** the disc is cut to the overlap of two discs shifted along the radius from the
//!   frame centre, more so towards the edges (Bokeh Shape), as mechanical vignetting does.
//!
//! Near-field blur (a band that leaves the subject out of focus) blurs the subject itself but does
//! not spread over the sharp background behind it: the gather radius is each pixel's own.

use lightcraft_color::luminance_2020;
use lightcraft_develop::{DevelopSettings, LensBlur};
use lightcraft_raster::{Plane, Rgb32f};

use crate::geometry::Frame;
use crate::{PixelWindow, for_rows, masks};

/// Blur radius at Amount 100, as a share of the long edge.
const MAX_RADIUS: f64 = 0.02;
/// The most a blur radius is in output pixels, whatever the size of the output.
const MAX_RADIUS_PX: f32 = 400.0;
/// Depth (0..100) the proxy gives the subject and the background.
const SUBJECT_DEPTH: f64 = 25.0;
const BACKGROUND_DEPTH: f64 = 100.0;
/// Depth units from the focal band at which the blur reaches its full radius.
const RAMP: f64 = 50.0;
/// Radii under this many pixels are not blurred (and fade in over the next pixel).
const MIN_RADIUS: f32 = 0.5;
/// Scene-linear luminance range over which the highlight weight rises (1.0 = white).
const HIGHLIGHT_LO: f32 = 0.6;
const HIGHLIGHT_HI: f32 = 2.0;
/// Weight a highlight gets at Highlight Boost 100, on top of 1.
const BOOST_MAX: f32 = 10.0;
/// Levels of the box-filtered pyramid above the image.
const MAX_LEVELS: usize = 8;
const GOLDEN_ANGLE: f32 = 2.399_963_2;

#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

/// Whether `s` asks for a Lens Blur that does something (the section's eye is on, Amount above zero).
pub fn active(s: &DevelopSettings) -> bool {
    s.lens_blur.active() && s.section_enabled("lensBlur")
}

/// Largest blur radius (output pixels) of `lb`, `px_per_long` output pixels to the long edge.
fn max_radius(lb: &LensBlur, px_per_long: f64) -> f32 {
    let amount = finite_or(lb.amount, 0.0).clamp(0.0, 100.0) / 100.0;
    ((MAX_RADIUS * amount * finite_or(px_per_long, 0.0).max(0.0)) as f32).min(MAX_RADIUS_PX)
}

/// `win` (a window of the `full_w × full_h` output) grown by the blur radius, so the pixels it
/// keeps gather from the same neighbours as in the whole render.
pub(crate) fn grow_window(s: &DevelopSettings, px_per_long: f64, win: PixelWindow, full_w: usize, full_h: usize) -> PixelWindow {
    if !active(s) {
        return win;
    }
    let m = max_radius(&s.lens_blur, px_per_long).ceil() as usize + 2;
    let (x0, y0) = (win.x.saturating_sub(m), win.y.saturating_sub(m));
    let x1 = win.x.saturating_add(win.w).saturating_add(m).min(full_w);
    let y1 = win.y.saturating_add(win.h).saturating_add(m).min(full_h);
    PixelWindow { x: x0, y: y0, w: x1.saturating_sub(x0), h: y1.saturating_sub(y0) }.clamped(full_w, full_h)
}

/// Blur `img` (scene-linear, white balanced, before exposure) as `lb` asks. `frame` carries the
/// whole frame's Subject decision; `px_per_long` is the output's pixels per long edge.
pub(crate) fn apply(img: &mut Rgb32f, lb: &LensBlur, frame: &Frame, px_per_long: f64) {
    let (w, h) = (img.width, img.height);
    if !lb.active() || w == 0 || h == 0 || img.data.len() != w * h {
        return;
    }
    let log_l = img.map(crate::local::log_lum);
    let alpha = masks::subject_matte(frame, w, h, img, &log_l);
    drop(log_l);
    let (off, full) = match frame.view {
        Some(v) => ((v.x, v.y), (v.full_w, v.full_h)),
        None => ((0.0, 0.0), (w as f64, h as f64)),
    };
    blur(img, lb, &alpha, px_per_long, off, full);
}

/// The circle of confusion (radius in pixels) of every pixel, from the subject matte `alpha`.
fn coc_plane(alpha: &Plane, lb: &LensBlur, rmax: f32) -> Plane {
    let (a, b) = (finite_or(lb.focal_near, 0.0).clamp(0.0, 100.0), finite_or(lb.focal_far, 40.0).clamp(0.0, 100.0));
    let (near, far) = (a.min(b), a.max(b));
    alpha.map(|al| {
        let al = if al.is_finite() { al.clamp(0.0, 1.0) as f64 } else { 0.0 };
        let depth = BACKGROUND_DEPTH - al * (BACKGROUND_DEPTH - SUBJECT_DEPTH);
        let dist = (near - depth).max(depth - far).max(0.0);
        rmax * (dist / RAMP).min(1.0) as f32
    })
}

/// [`apply`] with the subject matte given. `off` and `full`: the offset of `img` in, and the size
/// of, the whole output (for the cat's-eye shape).
fn blur(img: &mut Rgb32f, lb: &LensBlur, alpha: &Plane, px_per_long: f64, off: (f64, f64), full: (f64, f64)) {
    let (w, h) = (img.width, img.height);
    let rmax = max_radius(lb, px_per_long);
    if w == 0 || h == 0 || img.data.len() != w * h || alpha.width != w || alpha.data.len() != w * h || rmax < MIN_RADIUS {
        return;
    }
    let coc = coc_plane(alpha, lb, rmax);
    if coc.data.iter().all(|r| *r < MIN_RADIUS) {
        return;
    }
    let boost = (finite_or(lb.highlight_boost, 0.0).clamp(0.0, 100.0) / 100.0) as f32 * BOOST_MAX;
    let shape = (finite_or(lb.shape, 0.0).clamp(0.0, 100.0) / 100.0) as f32;
    let out = {
        let base: &Rgb32f = img;
        let pyr = Pyramid::new(base, boost);
        let tables = [vogel(12), vogel(24), vogel(48)];
        let half_long = (0.5 * full.0.max(full.1)).max(1.0);
        let centre = (0.5 * full.0, 0.5 * full.1);
        let mut out = vec![[0f32; 3]; w * h];
        for_rows(&mut out, w, |y, row| {
            for (x, o) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let orig = base.data.get(i).copied().unwrap_or_default();
                let r = coc.data.get(i).copied().unwrap_or(0.0);
                if r < MIN_RADIUS {
                    *o = orig;
                    continue;
                }
                let table = if r < 3.0 {
                    0
                } else if r < 12.0 {
                    1
                } else {
                    2
                };
                let Some(taps) = tables.get(table) else {
                    *o = orig;
                    continue;
                };
                // the cat's eye: two discs of radius 1 centred ±`c` along the radius from the frame centre
                let rx = ((x as f64 + 0.5 + off.0 - centre.0) / half_long) as f32;
                let ry = ((y as f64 + 0.5 + off.1 - centre.1) / half_long) as f32;
                let rho = (rx * rx + ry * ry).sqrt();
                let squeeze = 0.5 * shape * rho.min(1.0);
                let (dx, dy) = if shape > 0.0 && rho > 1e-4 { (rx / rho * squeeze, ry / rho * squeeze) } else { (0.0, 0.0) };
                let lens = dx != 0.0 || dy != 0.0;
                // taps are about `spacing` apart: read the level of the pyramid filtered that wide
                let spacing = r * (std::f32::consts::PI / taps.len().max(1) as f32).sqrt();
                let level = (spacing.max(1.0).log2().round() as usize).min(pyr.top());
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut acc = pyr.texel(0, x, y);
                for t in taps {
                    let (ux, uy) = (t[0], t[1]);
                    if lens && ((ux - dx).powi(2) + (uy - dy).powi(2) > 1.0 || (ux + dx).powi(2) + (uy + dy).powi(2) > 1.0) {
                        continue;
                    }
                    let (sx, sy) = (fx + ux * r, fy + uy * r);
                    let dist = (ux * ux + uy * uy).sqrt() * r;
                    // the tap counts when its own circle of confusion reaches this pixel
                    let theirs = coc.get_clamped(sx.floor() as isize, sy.floor() as isize);
                    let wt = (theirs - dist + 1.0).clamp(0.0, 1.0);
                    if wt <= 0.0 {
                        continue;
                    }
                    for (a, v) in acc.iter_mut().zip(pyr.sample(level, sx, sy)) {
                        *a += wt * v;
                    }
                }
                let gathered = if acc[3] > 1e-9 { [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3]] } else { orig };
                let k = (r - MIN_RADIUS).clamp(0.0, 1.0);
                *o = if k >= 1.0 || !orig.iter().all(|v| v.is_finite()) {
                    gathered
                } else {
                    [orig[0] + k * (gathered[0] - orig[0]), orig[1] + k * (gathered[1] - orig[1]), orig[2] + k * (gathered[2] - orig[2])]
                };
            }
        });
        out
    };
    img.data = out;
}

/// `n` points evenly filling the unit disc (Vogel's spiral).
fn vogel(n: usize) -> Vec<[f32; 2]> {
    (0..n)
        .map(|i| {
            let r = ((i as f32 + 0.5) / n as f32).sqrt();
            let a = i as f32 * GOLDEN_ANGLE;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// A pyramid level: premultiplied `[r·w, g·w, b·w, w]`.
struct Level {
    w: usize,
    h: usize,
    data: Vec<[f32; 4]>,
}

/// The image as `[r·w, g·w, b·w, w]` (`w`: the highlight weight) at full size (computed on the fly)
/// and then box-filtered to half size, quarter size, ... Averaging premultiplied values and
/// dividing by the summed weight gives the highlight-weighted mean.
struct Pyramid<'a> {
    base: &'a Rgb32f,
    boost: f32,
    up: Vec<Level>,
}

impl<'a> Pyramid<'a> {
    fn new(base: &'a Rgb32f, boost: f32) -> Self {
        let mut p = Pyramid { base, boost, up: Vec::new() };
        while p.up.len() < MAX_LEVELS {
            let k = p.up.len();
            let (pw, ph) = p.size(k);
            if pw.max(ph) <= 2 {
                break;
            }
            let (nw, nh) = (pw.div_ceil(2), ph.div_ceil(2));
            let mut data = vec![[0f32; 4]; nw * nh];
            for_rows(&mut data, nw, |y, row| {
                let y0 = 2 * y;
                let y1 = (y0 + 1).min(ph.saturating_sub(1));
                for (x, o) in row.iter_mut().enumerate() {
                    let x0 = 2 * x;
                    let x1 = (x0 + 1).min(pw.saturating_sub(1));
                    let t = [p.texel(k, x0, y0), p.texel(k, x1, y0), p.texel(k, x0, y1), p.texel(k, x1, y1)];
                    *o = std::array::from_fn(|c| 0.25 * (t[0][c] + t[1][c] + t[2][c] + t[3][c]));
                }
            });
            p.up.push(Level { w: nw, h: nh, data });
        }
        p
    }

    /// The highest level.
    fn top(&self) -> usize {
        self.up.len()
    }

    fn size(&self, level: usize) -> (usize, usize) {
        match level.checked_sub(1).and_then(|l| self.up.get(l)) {
            Some(l) => (l.w, l.h),
            None => (self.base.width, self.base.height),
        }
    }

    fn texel(&self, level: usize, x: usize, y: usize) -> [f32; 4] {
        match level.checked_sub(1) {
            None => premult(self.base.data.get(y * self.base.width + x).copied().unwrap_or_default(), self.boost),
            Some(l) => self.up.get(l).and_then(|l| l.data.get(y * l.w + x)).copied().unwrap_or_default(),
        }
    }

    /// Bilinear sample at (`bx`, `by`) in full-size pixel coordinates (pixel centres at +0.5).
    fn sample(&self, level: usize, bx: f32, by: f32) -> [f32; 4] {
        let (lw, lh) = self.size(level);
        let s = 1.0 / (1usize << level.min(30)) as f32;
        let (u, v) = (bx * s - 0.5, by * s - 0.5);
        let (x0, y0) = (u.floor(), v.floor());
        let (tx, ty) = (u - x0, v - y0);
        let at = |i: f32, n: usize| (i as i64).clamp(0, n.max(1) as i64 - 1) as usize;
        let (xa, xb, ya, yb) = (at(x0, lw), at(x0 + 1.0, lw), at(y0, lh), at(y0 + 1.0, lh));
        let (a, b, c, d) = (self.texel(level, xa, ya), self.texel(level, xb, ya), self.texel(level, xa, yb), self.texel(level, xb, yb));
        let top: [f32; 4] = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * tx);
        let bottom: [f32; 4] = std::array::from_fn(|k| c[k] + (d[k] - c[k]) * tx);
        std::array::from_fn(|k| top[k] + (bottom[k] - top[k]) * ty)
    }
}

/// `c` times its highlight weight, and the weight.
fn premult(c: [f32; 3], boost: f32) -> [f32; 4] {
    if !c.iter().all(|v| v.is_finite()) {
        return [0.0; 4];
    }
    let c = c.map(|v| v.clamp(-1e6, 1e6));
    let wgt = 1.0 + boost * smooth(HIGHLIGHT_LO, HIGHLIGHT_HI, luminance_2020(c).max(0.0));
    [c[0] * wgt, c[1] * wgt, c[2] * wgt, wgt]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{RenderRequest, SourceInfo, render};

    fn lens(amount: f64) -> LensBlur {
        LensBlur { amount, ..LensBlur::default() }
    }

    /// A checker with cells of `cell` pixels, between `lo` and `hi`.
    fn checker(x: usize, y: usize, cell: usize, lo: f32, hi: f32) -> f32 {
        if (x / cell + y / cell).is_multiple_of(2) { lo } else { hi }
    }

    /// Mean and standard deviation of the red channel over the box.
    fn stats(img: &Rgb32f, x0: usize, y0: usize, x1: usize, y1: usize) -> (f32, f32) {
        let v: Vec<f32> = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| img.get(x, y)[0]).collect();
        let n = v.len() as f32;
        let m = v.iter().sum::<f32>() / n;
        (m, (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / n).sqrt())
    }

    /// A disc subject matte (1 inside `r` of the centre, 0 outside, a one-`soft` ramp), sizes in
    /// normalized long-edge units so two resolutions of it agree.
    fn disc_alpha(w: usize, h: usize, r: f32, soft: f32) -> Plane {
        let long = w.max(h) as f32;
        Plane::from_fn(w, h, |x, y| {
            let (dx, dy) = ((x as f32 + 0.5 - 0.5 * w as f32) / long, (y as f32 + 0.5 - 0.5 * h as f32) / long);
            1.0 - smooth(r - soft, r + soft, (dx * dx + dy * dy).sqrt())
        })
    }

    fn textured(w: usize, h: usize, cell: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| [checker(x, y, cell, 0.1, 0.5); 3])
    }

    #[test]
    fn inactive_settings_are_inactive() {
        let mut s = DevelopSettings::default();
        assert!(!active(&s));
        s.lens_blur.amount = 30.0;
        assert!(active(&s));
        s.lens_blur.enabled = Some(false);
        assert!(!active(&s));
        s.lens_blur.enabled = None;
        s.set_section_enabled("lensBlur", false);
        assert!(!active(&s));
        assert!(!LensBlur { amount: f64::NAN, ..LensBlur::default() }.active());
    }

    #[test]
    fn amount_zero_changes_nothing() {
        let (w, h) = (96, 64);
        let src = textured(w, h, 4);
        let mut img = src.clone();
        blur(&mut img, &lens(0.0), &disc_alpha(w, h, 0.2, 0.02), w as f64, (0.0, 0.0), (w as f64, h as f64));
        assert_eq!(img.data, src.data);
    }

    #[test]
    fn the_subject_stays_sharp_and_the_background_blurs() {
        let (w, h) = (240, 160);
        let src = textured(w, h, 4);
        let mut img = src.clone();
        let alpha = disc_alpha(w, h, 0.2, 0.01);
        blur(&mut img, &LensBlur { amount: 100.0, ..LensBlur::default() }, &alpha, w as f64, (0.0, 0.0), (w as f64, h as f64));
        // the subject (a disc of 0.2 of the long edge = 48 px) is untouched, bit for bit
        for y in 60..100 {
            for x in 100..140 {
                assert_eq!(img.get(x, y), src.get(x, y), "{x},{y}");
            }
        }
        // the background loses almost all of its 4-px checker
        let (_, before) = stats(&src, 4, 4, 44, 44);
        let (mean, after) = stats(&img, 4, 4, 44, 44);
        assert!(after < 0.5 * before, "{after} vs {before}");
        assert!((mean - 0.3).abs() < 0.05, "the mean stays {mean}");
        assert!(img.data.iter().all(|p| p.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn a_sharp_subject_does_not_bleed_into_the_background() {
        // a bright subject on a dark background: no background pixel next to the subject may take
        // the subject's brightness beyond what its own neighbourhood holds
        let (w, h) = (240, 160);
        let src = Rgb32f::from_fn(w, h, |x, y| {
            let (dx, dy) = ((x as f32 + 0.5 - 120.0) / 240.0, (y as f32 + 0.5 - 80.0) / 240.0);
            if (dx * dx + dy * dy).sqrt() < 0.15 { [0.8; 3] } else { [0.05; 3] }
        });
        let mut img = src.clone();
        blur(
            &mut img,
            &LensBlur { amount: 100.0, highlight_boost: 0.0, ..LensBlur::default() },
            &disc_alpha(w, h, 0.15, 0.005),
            240.0,
            (0.0, 0.0),
            (240.0, 160.0),
        );
        // 2 px outside the disc (radius 36 px): a plain blur would put about 0.3 here
        let v = img.get(120 + 36 + 2, 80)[0];
        assert!(v < 0.2, "{v}");
    }

    #[test]
    fn highlight_boost_lets_bright_points_bloom() {
        let (w, h) = (160, 120);
        let src = Rgb32f::from_fn(w, h, |x, y| if (30..32).contains(&x) && (30..32).contains(&y) { [4.0; 3] } else { [0.05; 3] });
        let alpha = Plane::new(w, h);
        let render_with = |boost: f64| {
            let mut img = src.clone();
            blur(&mut img, &LensBlur { amount: 100.0, highlight_boost: boost, ..LensBlur::default() }, &alpha, 160.0, (0.0, 0.0), (160.0, 120.0));
            img.get(33, 31)[0]
        };
        let (plain, boosted) = (render_with(0.0), render_with(100.0));
        assert!(boosted > 1.5 * plain && plain > 0.15, "{plain} {boosted}");
    }

    #[test]
    fn cat_eye_changes_only_the_edges() {
        let (w, h) = (160, 120);
        let src =
            Rgb32f::from_fn(
                w,
                h,
                |x, y| if [10, 80].iter().any(|b| (*b..*b + 2).contains(&x)) && (60..62).contains(&y) { [4.0; 3] } else { [0.05; 3] },
            );
        let alpha = Plane::new(w, h);
        let run = |shape: f64| {
            let mut img = src.clone();
            blur(&mut img, &LensBlur { amount: 100.0, shape, ..LensBlur::default() }, &alpha, 160.0, (0.0, 0.0), (160.0, 120.0));
            img
        };
        let (round, eye) = (run(0.0), run(100.0));
        // at the centre the disc is round, so the spot looks the same
        let centre: f32 = (70..90).map(|x| (round.get(x, 60)[0] - eye.get(x, 60)[0]).abs()).sum();
        assert!(centre < 1e-3, "{centre}");
        // near the left edge it is squeezed along the radius
        let edge: f32 = (0..30).map(|x| (round.get(x, 60)[0] - eye.get(x, 60)[0]).abs()).sum();
        assert!(edge > 0.01, "{edge}");
    }

    #[test]
    fn the_same_picture_at_two_sizes_blurs_alike() {
        let scene = |w: usize, h: usize| {
            let long = w.max(h) as f32;
            Rgb32f::from_fn(w, h, |x, y| {
                let (u, v) = ((x as f32 + 0.5) / long, (y as f32 + 0.5) / long);
                let t = 0.3 + 0.15 * (u * 40.0).sin() * (v * 30.0).cos();
                [t; 3]
            })
        };
        let lb = LensBlur { amount: 100.0, ..LensBlur::default() };
        let run = |w: usize, h: usize| {
            let mut img = scene(w, h);
            blur(&mut img, &lb, &disc_alpha(w, h, 0.18, 0.02), w as f64, (0.0, 0.0), (w as f64, h as f64));
            img
        };
        let (small, big) = (run(160, 120), run(320, 240));
        let mut diff = 0.0f32;
        for y in 0..120 {
            for x in 0..160 {
                let avg = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().map(|(dx, dy)| big.get(2 * x + dx, 2 * y + dy)[0]).sum::<f32>() / 4.0;
                diff += (avg - small.get(x, y)[0]).abs();
            }
        }
        let mean = diff / (160.0 * 120.0);
        assert!(mean < 0.02, "mean difference {mean}");
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        for (w, h) in [(1, 1), (2, 3), (7, 1), (1, 9)] {
            let mut img = textured(w, h, 1);
            blur(&mut img, &lens(100.0), &Plane::new(w, h), 1000.0, (0.0, 0.0), (w as f64, h as f64));
            assert!(img.data.iter().all(|p| p.iter().all(|v| v.is_finite())), "{w}x{h}");
        }
        let mut img = Rgb32f::from_fn(40, 30, |x, _| if x == 5 { [f32::NAN, 1.0, f32::INFINITY] } else { [0.4; 3] });
        let lb = LensBlur { amount: 100.0, focal_near: f64::NAN, shape: f64::NAN, highlight_boost: f64::INFINITY, ..LensBlur::default() };
        blur(&mut img, &lb, &Plane::new(40, 30), 40.0, (0.0, 0.0), (40.0, 30.0));
        let mut img = Rgb32f::from_fn(40, 30, |_, _| [0.4; 3]);
        blur(&mut img, &lens(100.0), &Plane::new(3, 3), 40.0, (0.0, 0.0), (40.0, 30.0));
    }

    /// A bright, textured disc on a dark, finely textured background, as a source photo.
    fn photo(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let (dx, dy) = ((x as f32 + 0.5 - 0.5 * w as f32) / w as f32, (y as f32 + 0.5 - 0.5 * h as f32) / w as f32);
            if (dx * dx + dy * dy).sqrt() < 0.2 { [checker(x, y, 3, 0.6, 0.9); 3] } else { [checker(x, y, 4, 0.03, 0.05); 3] }
        })
    }

    fn render_with(s: &DevelopSettings) -> lightcraft_raster::Rgba8 {
        render(&photo(240, 160), &SourceInfo::default(), s, &RenderRequest::fit(240, 160)).image
    }

    fn luma_stats(img: &lightcraft_raster::Rgba8, x0: usize, y0: usize, x1: usize, y1: usize) -> (f32, f32) {
        let v: Vec<f32> = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| img.get(x, y)[1] as f32).collect();
        let n = v.len() as f32;
        let m = v.iter().sum::<f32>() / n;
        (m, (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / n).sqrt())
    }

    #[test]
    fn a_render_with_amount_zero_is_the_render_without_lens_blur() {
        let plain = render_with(&DevelopSettings::default());
        let mut s = DevelopSettings::default();
        s.lens_blur.focal_near = 20.0;
        s.lens_blur.shape = 80.0;
        s.lens_blur.highlight_boost = 100.0;
        assert_eq!(render_with(&s).data, plain.data);
    }

    #[test]
    fn a_render_keeps_the_subject_sharp_and_blurs_the_background() {
        let plain = render_with(&DevelopSettings::default());
        let mut s = DevelopSettings::default();
        s.lens_blur.amount = 100.0;
        let blurred = render_with(&s);
        // inside the subject (a disc 48 px wide of the long edge: the centre box is well inside it)
        let (m0, sd0) = luma_stats(&plain, 100, 60, 140, 100);
        let (m1, sd1) = luma_stats(&blurred, 100, 60, 140, 100);
        assert!(sd1 > 0.7 * sd0 && (m1 - m0).abs() < 8.0, "subject: {m0} {sd0} -> {m1} {sd1}");
        // a corner of the background
        let (_, bd0) = luma_stats(&plain, 4, 4, 40, 36);
        let (_, bd1) = luma_stats(&blurred, 4, 4, 40, 36);
        assert!(bd1 < 0.6 * bd0, "background: {bd0} -> {bd1}");
    }
}
