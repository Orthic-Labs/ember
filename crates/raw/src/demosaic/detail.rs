//! "Raw Details" demosaicing: the opt-in higher-quality Bayer reconstruction ([`super::Method::RawDetails`]).
//!
//! An original composition of published ideas, written from their descriptions (no code of any other
//! demosaicer is used or ported; RCD, darktable, RawTherapee, LibRaw and dcraw are not sources):
//!
//! 1. **Green candidates.** At every red/blue site a horizontal and a vertical green estimate: the mean of the two
//!    adjacent greens plus a quarter of the own-colour second difference (the gradient-corrected predictor of
//!    J. F. Hamilton Jr. & J. E. Adams, US patent 5,629,734, 1997). Each is clamped to the two greens it lies
//!    between, but keeps a quarter of any overshoot, so a step edge does not ring and a fine pattern (a zone
//!    plate) is not flattened the way a hard clamp flattens it.
//! 2. **Soft direction.** Instead of choosing a direction per pixel, the two candidates are blended by inverse
//!    squared directional activity summed over the site and its four diagonal (also red/blue) neighbours, in the
//!    spirit of the gradient-weighted, threshold-free blending of I. Pekkucuksen & Y. Altunbasak, "Gradient based
//!    threshold free color filter array interpolation" (ICIP 2010) and the adaptive weighting of L. Zhang & X. Wu,
//!    "Color demosaicking via directional linear minimum mean square-error estimation" (IEEE TIP 14(12), 2005).
//! 3. **Red and blue** by colour-difference (C − G) interpolation: along the row or column at green sites, and
//!    along the diagonal with the smaller variation (weighted) at the opposite-colour sites.
//! 4. **Green refinement.** Each red/blue site's green is re-estimated from the colour differences of its four
//!    green neighbours, which by now carry interpolated red/blue values (the refinement idea of D. Menon,
//!    S. Andriani & G. Calvagno, "Demosaicing with directional filtering and a posteriori decision",
//!    IEEE TIP 16(1), 2007), weighted by the same direction share; red and blue are then redone from it.
//!
//! A constant-colour mosaic and every known sample are reproduced exactly. Work is done in overlapping tiles
//! (bounded memory, rayon-parallel); the margin covers the whole dependency chain, so a tile border is exact.

use super::{Mosaic, clamp_between};
use crate::Rgb32f;
use rayon::prelude::*;

const TILE: usize = 128;
/// Rows/columns a result depends on: green candidates 2, direction sums +1, first completion +1, refinement +1,
/// second completion +1; one more for safety.
const MARGIN: usize = 8;
/// Keeps a division by an activity of zero finite (data are normalised, white = 1.0).
const EPS: f32 = 1e-3;
/// Share of an overshoot past the two nearest greens that a green candidate keeps.
const OVERSHOOT: f32 = 0.25;

pub(crate) fn raw_details(m: &Mosaic) -> Rgb32f {
    let (w, h) = (m.w, m.h);
    let tiles: Vec<(usize, usize)> = (0..h.div_ceil(TILE)).flat_map(|ty| (0..w.div_ceil(TILE)).map(move |tx| (tx * TILE, ty * TILE))).collect();
    let results: Vec<(usize, usize, usize, usize, Vec<[f32; 3]>)> = tiles
        .par_iter()
        .map(|&(x0, y0)| {
            let (tw, th) = (TILE.min(w - x0), TILE.min(h - y0));
            (x0, y0, tw, th, tile(m, x0, y0, tw, th))
        })
        .collect();
    let mut out = Rgb32f::new(w, h);
    for (x0, y0, tw, th, px) in results {
        for y in 0..th {
            out.data[(y0 + y) * w + x0..(y0 + y) * w + x0 + tw].copy_from_slice(&px[y * tw..(y + 1) * tw]);
        }
    }
    out
}

/// `v` pulled back inside `a`..`b`, keeping [`OVERSHOOT`] of what lay outside.
#[inline]
fn soft_clamp(v: f32, a: f32, b: f32) -> f32 {
    let c = clamp_between(v, a, b);
    c + OVERSHOOT * (v - c)
}

/// Index of `(x, y)` in a `bw × bh` tile, clamped to it (values outside the tile margin are never used).
#[inline]
fn at(bw: usize, bh: usize, x: isize, y: isize) -> usize {
    y.clamp(0, bh as isize - 1) as usize * bw + x.clamp(0, bw as isize - 1) as usize
}

/// The first of two inverse-square weights as a share of their sum: `1/(EPS+a)²` against `1/(EPS+b)²`.
/// A half when the weights are not finite numbers (NaN or infinite samples).
#[inline]
fn share(a: f32, b: f32) -> f32 {
    let (ia, ib) = (1.0 / (EPS + a), 1.0 / (EPS + b));
    let (wa, wb) = (ia * ia, ib * ib);
    let total = wa + wb;
    if total.is_finite() && total > 0.0 { wa / total } else { 0.5 }
}

/// Camera RGB for every pixel of a tile from its mosaic, its colours and a complete green plane.
fn complete(mos: &[f32], col: &[u8], g: &[f32], bw: usize, bh: usize) -> Vec<[f32; 3]> {
    let d: Vec<f32> = mos.iter().zip(g).map(|(m, g)| m - g).collect();
    let mut out = vec![[0f32; 3]; bw * bh];
    for y in 0..bh as isize {
        for x in 0..bw as isize {
            let i = at(bw, bh, x, y);
            let own = usize::from(col[i]);
            let mut p = [0f32; 3];
            p[1] = g[i];
            if own == 1 {
                // a green site: the colour of its row neighbours along the row, the other along the column
                let row = usize::from(col[at(bw, bh, x + 1, y)]);
                let column = 2usize.saturating_sub(row);
                p[row] = g[i] + (d[at(bw, bh, x - 1, y)] + d[at(bw, bh, x + 1, y)]) * 0.5;
                p[column] = g[i] + (d[at(bw, bh, x, y - 1)] + d[at(bw, bh, x, y + 1)]) * 0.5;
            } else {
                // a red/blue site: its own sample, and the opposite colour from the diagonal neighbours
                p[own] = mos[i];
                let (nw, ne, sw, se) = (at(bw, bh, x - 1, y - 1), at(bw, bh, x + 1, y - 1), at(bw, bh, x - 1, y + 1), at(bw, bh, x + 1, y + 1));
                let gc = g[i];
                let down = (d[nw] - d[se]).abs() + (g[nw] - gc).abs() + (g[se] - gc).abs();
                let up = (d[ne] - d[sw]).abs() + (g[ne] - gc).abs() + (g[sw] - gc).abs();
                let s = share(down, up);
                p[2usize.saturating_sub(own)] = gc + s * (d[nw] + d[se]) * 0.5 + (1.0 - s) * (d[ne] + d[sw]) * 0.5;
            }
            out[i] = p;
        }
    }
    out
}

fn tile(m: &Mosaic, x0: usize, y0: usize, tw: usize, th: usize) -> Vec<[f32; 3]> {
    let (bw, bh) = (tw + 2 * MARGIN, th + 2 * MARGIN);
    let n = bw * bh;
    let (ox, oy) = (x0 as isize - MARGIN as isize, y0 as isize - MARGIN as isize);
    let mut mos = vec![0f32; n];
    let mut col = vec![0u8; n];
    for y in 0..bh {
        for x in 0..bw {
            mos[y * bw + x] = m.get(ox + x as isize, oy + y as isize);
            col[y * bw + x] = m.color(ox + x as isize, oy + y as isize);
        }
    }
    let ix = |x: isize, y: isize| at(bw, bh, x, y);
    // 1. directional green candidates and their activity at red/blue sites
    let mut cand_h = vec![0f32; n];
    let mut cand_v = vec![0f32; n];
    let mut act_h = vec![0f32; n];
    let mut act_v = vec![0f32; n];
    for y in 0..bh as isize {
        for x in 0..bw as isize {
            let i = ix(x, y);
            if col[i] == 1 {
                continue;
            }
            let c = mos[i];
            let (l, r, l2, r2) = (mos[ix(x - 1, y)], mos[ix(x + 1, y)], mos[ix(x - 2, y)], mos[ix(x + 2, y)]);
            let lap = 2.0 * c - l2 - r2;
            cand_h[i] = soft_clamp((l + r) * 0.5 + lap * 0.25, l, r);
            act_h[i] = (l - r).abs() + lap.abs();
            let (u, d, u2, d2) = (mos[ix(x, y - 1)], mos[ix(x, y + 1)], mos[ix(x, y - 2)], mos[ix(x, y + 2)]);
            let lap = 2.0 * c - u2 - d2;
            cand_v[i] = soft_clamp((u + d) * 0.5 + lap * 0.25, u, d);
            act_v[i] = (u - d).abs() + lap.abs();
        }
    }
    // 2. soft direction: horizontal share of every red/blue site, and the blended green plane
    let mut horizontal = vec![0.5f32; n];
    let mut g = mos.clone();
    for y in 0..bh as isize {
        for x in 0..bw as isize {
            let i = ix(x, y);
            if col[i] == 1 {
                continue;
            }
            // the site and its four diagonal neighbours are all red/blue sites
            let sum = |a: &[f32]| a[i] + a[ix(x - 1, y - 1)] + a[ix(x + 1, y - 1)] + a[ix(x - 1, y + 1)] + a[ix(x + 1, y + 1)];
            let s = share(sum(&act_h), sum(&act_v));
            horizontal[i] = s;
            g[i] = s * cand_h[i] + (1.0 - s) * cand_v[i];
        }
    }
    // 3. red and blue from that green
    let first = complete(&mos, &col, &g, bw, bh);
    // 4. green again from the colour differences of its four green neighbours, then red and blue again
    let mut refined = g.clone();
    for y in 0..bh as isize {
        for x in 0..bw as isize {
            let i = ix(x, y);
            if col[i] == 1 {
                continue;
            }
            let k = usize::from(col[i]);
            let diff = |dx: isize, dy: isize| {
                let j = ix(x + dx, y + dy);
                g[j] - first[j][k]
            };
            let s = horizontal[i];
            refined[i] = mos[i] + s * (diff(-1, 0) + diff(1, 0)) * 0.5 + (1.0 - s) * (diff(0, -1) + diff(0, 1)) * 0.5;
        }
    }
    let done = complete(&mos, &col, &refined, bw, bh);
    let mut out = Vec::with_capacity(tw * th);
    for y in MARGIN..MARGIN + th {
        out.extend_from_slice(&done[y * bw + MARGIN..y * bw + MARGIN + tw]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{Method, demosaic, mosaic_from_rgb, psnr};
    use crate::{Cfa, Rgb32f};

    /// A photo-like scene: sharp luminance structure (disc, stripes, diagonal edge, fine texture) over a smoothly
    /// varying colour.
    fn edgy_scene(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32, y as f32);
            let d = ((fx - w as f32 * 0.35).powi(2) + (fy - h as f32 * 0.4).powi(2)).sqrt();
            let lum = if d < w as f32 * 0.18 {
                0.85
            } else if x > w * 2 / 3 && (y / 3).is_multiple_of(2) {
                0.15
            } else if fx + fy * 0.5 > w as f32 * 1.1 {
                0.7
            } else {
                0.35 + 0.1 * ((fx * 0.7).sin() * (fy * 0.9).cos())
            };
            let tint = [0.9 + 0.1 * (fx / w as f32), 0.8, 0.55 + 0.3 * (fy / h as f32)];
            [lum * tint[0], lum * tint[1], lum * tint[2]]
        })
    }

    /// A zone plate: cos(α·r²), whose spatial frequency grows linearly with the radius to 0.35 cycles per pixel
    /// at the edge of the frame, past the red/blue Nyquist limit of a Bayer mosaic.
    fn zone_plate(size: usize) -> Rgb32f {
        let a = 0.35 * std::f32::consts::PI / (size as f32 / 2.0);
        Rgb32f::from_fn(size, size, |x, y| {
            let r2 = (x as f32 - size as f32 / 2.0).powi(2) + (y as f32 - size as f32 / 2.0).powi(2);
            let v = 0.5 + 0.4 * (a * r2).cos();
            [v, v * 0.9 + 0.03, v * 0.8 + 0.05]
        })
    }

    fn run(img: &Rgb32f, cfa: &Cfa, m: Method) -> Rgb32f {
        demosaic(&mosaic_from_rgb(img, cfa), m)
    }

    #[test]
    fn beats_the_default_on_edges_and_a_zone_plate() {
        for pattern in ["RGGB", "GRBG", "BGGR", "GBRG"] {
            let cfa = Cfa::bayer(pattern).unwrap();
            let edgy = edgy_scene(96, 80);
            let (ahd, new) = (psnr(&edgy, &run(&edgy, &cfa, Method::Ahd), 4), psnr(&edgy, &run(&edgy, &cfa, Method::RawDetails), 4));
            assert!(new > ahd + 0.3, "{pattern} edges: raw details {new:.2} dB, ahd {ahd:.2} dB");
            let zone = zone_plate(128);
            let (ahd, new) = (psnr(&zone, &run(&zone, &cfa, Method::Ahd), 4), psnr(&zone, &run(&zone, &cfa, Method::RawDetails), 4));
            assert!(new > ahd + 1.0, "{pattern} zone plate: raw details {new:.2} dB, ahd {ahd:.2} dB");
        }
    }

    #[test]
    fn flat_regions_stay_flat_and_neutral_edges_stay_neutral() {
        for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let cfa = Cfa::bayer(pattern).unwrap();
            let flat = Rgb32f::filled(40, 36, [0.3, 0.55, 0.8]);
            for p in &run(&flat, &cfa, Method::RawDetails).data {
                assert!((p[0] - 0.3).abs() < 1e-5 && (p[1] - 0.55).abs() < 1e-5 && (p[2] - 0.8).abs() < 1e-5, "{pattern} {p:?}");
            }
            // a vertical and a horizontal grey step: no zipper, so no colour appears along the edge and the flat sides
            // away from it are untouched
            for vertical in [true, false] {
                let step = Rgb32f::from_fn(48, 48, |x, y| {
                    let v = if (if vertical { x } else { y }) < 24 { 0.2 } else { 0.8 };
                    [v; 3]
                });
                let out = run(&step, &cfa, Method::RawDetails);
                for y in 4..44 {
                    for x in 4..44 {
                        let (p, q) = (out.get(x, y), step.get(x, y));
                        assert!((p[0] - p[1]).abs() < 1e-3 && (p[2] - p[1]).abs() < 1e-3, "{pattern} vertical {vertical} zipper at {x},{y}: {p:?}");
                        assert!((p[1] - q[1]).abs() < 1e-3, "{pattern} vertical {vertical} green at {x},{y}: {p:?} vs {q:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_default_method_is_unchanged_by_the_new_one() {
        // the toggle is off for every existing edit: the default method is still AHD, and its result is the AHD result
        assert_eq!(Method::default(), Method::Ahd);
        let cfa = Cfa::bayer("RGGB").unwrap();
        let n = mosaic_from_rgb(&edgy_scene(70, 50), &cfa);
        let m = super::super::Mosaic { w: n.width, h: n.height, data: &n.data, cfa: &cfa };
        let direct = super::super::ahd::ahd(&m);
        let by_method = demosaic(&n, Method::default());
        assert_eq!(direct.data.len(), by_method.data.len());
        assert!(direct.data.iter().zip(&by_method.data).all(|(a, b)| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())));
    }

    #[test]
    fn tile_borders_are_exact() {
        // one tile over the whole image (all of its margin is mirrored picture) must give the very pixels that the
        // 128-pixel tiles give: a margin shorter than the dependency chain would show at the seams
        let cfa = Cfa::bayer("RGGB").unwrap();
        let img = edgy_scene(2 * super::TILE + 9, super::TILE + 7);
        let n = mosaic_from_rgb(&img, &cfa);
        let m = super::super::Mosaic { w: n.width, h: n.height, data: &n.data, cfa: &cfa };
        let tiled = super::raw_details(&m);
        let whole = super::tile(&m, 0, 0, n.width, n.height);
        assert_eq!(tiled.data.len(), whole.len());
        assert!(tiled.data.iter().zip(&whole).all(|(a, b)| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())));
    }
}
