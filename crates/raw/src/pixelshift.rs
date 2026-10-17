//! Pixel-shift ("multi-shot") merging: Sony ARQ and Pentax Pixel Shift Resolution (PEF and DNG).
//!
//! Sources: our own black-box analysis of CC0 samples from raw.pixls.us (Sony ILCE-7RM3 ARQ; Pentax K-1 II, K-3 III
//! and K-1 pixel-shift PEF and DNG files); no third-party decoder source was consulted. What the files settled:
//!
//! **Pentax.** A pixel-shift file holds four complete Bayer frames (the PEF's IFD0 raw plus three `SubIFDs` raws; the
//! DNG's four top-level IFDs, each with its raw in a `SubIFD`). Each frame is stored in sensor coordinates, so the
//! CFA phase is the same in every frame and the *scene* moves under it. Registering the frames of tripod shots
//! (phase correlation of the interpolated green plane over 60 to 80 textured tiles per file) gave integer shifts of
//! the scene content, relative to frame 0, of `(0, 0)`, `(+1, 0)`, `(+1, +1)`, `(0, +1)` in (rows, columns): the
//! sensor steps one pixel down, right, then up. A scene point `q` of frame 0's coordinates is therefore seen by
//! frame `k` at sensor pixel `q + SEQUENCE[k]`, under the filter the CFA has there, and the four frames together
//! put every one of R, G, G, B on every point. Tested against all other assignments of the four offsets to the
//! frames (the two greens of a point, which come from different frames, differ only by noise when the offsets are
//! right): on K-3 III and K-1 II tripod frames the offsets above have 20 times less green mismatch than the next best
//! order, in the PEF of both bodies and in the K-1 II DNG (IFD order = frame order). Frame order and count are
//! marked in the maker note: `0x0243` is a block whose byte 0 is 1 for a shot taken in pixel-shift mode, byte 2 the
//! frame count (4) and byte 7 the frame's index (0 to 3, in the same block of every frame). Multi-frame files that are
//! not pixel shift (a three-frame bracket on the K-3 III) carry a zero block there, and a single frame carries
//! `00 00 01 00 ...`.
//!
//! **Sony ARQ.** The camera has already merged the four frames: the raw IFD is a 14-bit uncompressed image with four
//! samples per pixel (photometric 32892, `CFAPlaneColor` 0 1 1 2) in the order R, G1, G2, B, every plane registered
//! to the same pixel (the cross-correlation of G1 with G2 peaks at zero shift, and no plane shows the CFA phase). The
//! four frames' brightness drift (the exposure of an indoor sample falls about 0.3% over the sequence) survives in
//! the class means: the mean of plane `c` over the pixels of parity class `q` is `b[s]` with `s = parity(c) - q`,
//! where `b` is the brightness of the frame shifted by `s`, and `b` falls monotonically down the sequence above. A
//! least-squares fit of that model leaves residuals of 2e-4 against 1.2e-3 for no model, and swapping the roles of
//! the two green planes leaves them at 1.1e-3, which fixes that G1 is the sample taken at CFA position (0, 1) and
//! G2 the one at (1, 0) of an RGGB cell, and that the frame shifted by `(0, 0)` is the unshifted one. So the
//! unshifted frame's own mosaic is recoverable from the planes: at an even-even pixel plane R, at even-odd G1, at
//! odd-even G2, at odd-odd B.
//!
//! The merge: with the planes registered, the full-colour value of a point is its R, G (the mean of the two
//! greens) and B, with no demosaic. Where the frames disagree (something moved), the two greens, which come from
//! different frames, differ by more than noise explains: those pixels (grown by [`DILATE`]) are taken from a
//! single-frame demosaic of the unshifted frame instead. The noise is fitted from the image itself (variance of the
//! green difference as a line in the green level, from the lower quartile of the differences of each level band, so
//! that moving areas inflate it little); nothing about the camera's gain is assumed.

use crate::demosaic::{Method, demosaic};
use crate::{BlackLevel, Cfa, Normalized, RawError, Result};
use lightcraft_tiff::{ByteOrder, Entry, Ifd, Tiff, makernote, tags as t};
use rayon::prelude::*;

/// Scene shift (rows, columns) of frame `k` against frame 0, as measured on the Pentax files: scene point `q` of
/// frame 0 is at sensor pixel `q + SEQUENCE[k]` in frame `k`.
pub(crate) const SEQUENCE: [(usize, usize); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];

/// The Pentax maker-note block that marks a pixel-shift frame (byte 0 = 1, byte 2 = 4 frames, byte 7 = index).
const PENTAX_SHIFT_BLOCK: u16 = 0x0243;
/// Photometric value of the Sony ARQ raw IFD (four sequentially captured samples per pixel).
pub(crate) const ARQ_PHOTOMETRIC: u16 = 32892;

/// Rows per band of the single-frame fallback demosaic (even, so that every band starts on the CFA phase of row 0).
const BAND: usize = 128;
/// Rows demosaiced above and below a band for the interpolation to see its neighbours (even).
const MARGIN: usize = 8;
/// A green pair is moving when it differs by more than this many standard deviations of the fitted noise.
const NOISE_K: f32 = 5.0;
/// Pixels around a moving pixel that follow it into the fallback.
const DILATE: usize = 2;

/// A merged picture: full-colour samples, no CFA.
pub(crate) struct Merged {
    /// `width × height × 3` samples, R, G, B, black at [`Merged::black`].
    pub data: Vec<u16>,
    /// The (uniform) black level of `data`.
    pub black: f32,
    /// Pixels that came from the single-frame demosaic because the frames disagree there.
    #[allow(dead_code)] // read by the tests; kept for callers that report how much of a picture moved
    pub moving: usize,
}

/// The frame index of a Pentax maker note that marks a pixel-shift frame (see the module notes), `None` otherwise.
pub(crate) fn pentax_frame_index(note: &makernote::MakerNote) -> Option<usize> {
    let &[shot, _, frames, _, _, _, _, index, ..] = note.ifd.bytes(PENTAX_SHIFT_BLOCK)? else { return None };
    (shot == 1 && frames == 4 && index < 4).then_some(usize::from(index))
}

/// The four frames in index order. `frames` pairs each raw IFD with its frame index; it must be a permutation of 0..4.
pub(crate) fn order_frames<T: Copy>(frames: Vec<(T, usize)>) -> Option<[T; 4]> {
    if frames.len() != 4 {
        return None;
    }
    let mut slots: [Option<T>; 4] = [None; 4];
    for (frame, index) in frames {
        let slot = slots.get_mut(index)?;
        if slot.is_some() {
            return None;
        }
        *slot = Some(frame);
    }
    let [Some(a), Some(b), Some(c), Some(d)] = slots else { return None };
    Some([a, b, c, d])
}

fn note_index(bytes: &[u8], order: ByteOrder, make: &str, entry: &Entry) -> Option<usize> {
    let note = makernote::parse_makernote(bytes, entry.offset, entry.count() as u64, order, make)?;
    pentax_frame_index(&note)
}

/// The four raw IFDs of a Pentax pixel-shift PEF in frame order: IFD0's own raw and the raw `SubIFDs`, each with
/// the maker note of its own Exif IFD. `None` for any other file (including a PEF of one frame).
pub(crate) fn pef_frames<'a>(tiff: &'a Tiff, bytes: &[u8]) -> Option<[&'a Ifd; 4]> {
    let ifd0 = tiff.ifds.first()?;
    let make = ifd0.string(t::MAKE).unwrap_or_default();
    let mut found = Vec::new();
    for ifd in std::iter::once(ifd0).chain(ifd0.sub_ifds.iter()) {
        if ifd.u16(t::PHOTOMETRIC) != Some(t::photometric::CFA) {
            continue;
        }
        let entry = ifd.exif.as_deref()?.get(t::MAKER_NOTE)?;
        found.push((ifd, note_index(bytes, tiff.order, &make, entry)?));
    }
    order_frames(found)
}

/// The four raw IFDs of a Pentax pixel-shift DNG in frame order: each top-level IFD carries the Pentax maker note in
/// its `DNGPrivateData` and its raw in a `SubIFD`. `None` for any other file.
pub(crate) fn dng_frames<'a>(tiff: &'a Tiff, bytes: &[u8]) -> Option<[&'a Ifd; 4]> {
    let make = tiff.ifds.first()?.string(t::MAKE).unwrap_or_default();
    let mut found = Vec::new();
    for top in &tiff.ifds {
        let Some(entry) = top.get(t::DNG_PRIVATE_DATA) else { continue };
        if !entry.value.as_bytes().is_some_and(|b| b.starts_with(b"PENTAX \0")) {
            return None;
        }
        let is_raw = |i: &&Ifd| i.u16(t::PHOTOMETRIC) == Some(t::photometric::CFA) && i.u32(t::NEW_SUBFILE_TYPE).unwrap_or(0) == 0;
        let raw = top.walk().into_iter().find(is_raw)?;
        found.push((raw, note_index(bytes, tiff.order, &make, entry)?));
    }
    order_frames(found)
}

/// Black level per colour (R, G, B): the mean over the black pattern's cells of that colour. `cfa` is the layout
/// anchored at the origin the black pattern is indexed from (the active area's).
pub(crate) fn black_per_colour(black: &BlackLevel, cfa: &Cfa) -> [f32; 3] {
    let (mut sum, mut n) = ([0f64; 3], [0u32; 3]);
    for y in 0..16usize {
        for x in 0..16usize {
            let c = usize::from(cfa.color_at(x, y));
            if let (Some(s), Some(k)) = (sum.get_mut(c), n.get_mut(c)) {
                *s += f64::from(black.at(x, y, 0, 1));
                *k += 1;
            }
        }
    }
    let mean = |c: usize| match (sum.get(c), n.get(c)) {
        (Some(&s), Some(&k)) if k > 0 => (s / f64::from(k)) as f32,
        _ => black.mean(),
    };
    [mean(0), mean(1), mean(2)]
}

/// Which of the two green samples (0, 1: in row-major order of the 2×2 cell) the site `[y][x]` takes.
fn green_slots(cfa: &Cfa) -> [[usize; 2]; 2] {
    let mut slots = [[0usize; 2]; 2];
    let mut next = 0usize;
    for (y, row) in slots.iter_mut().enumerate() {
        for (x, slot) in row.iter_mut().enumerate() {
            if cfa.color_at(x, y) == 1 {
                *slot = next.min(1);
                next += 1;
            }
        }
    }
    slots
}

/// The sample of a merged point (0 = R, 1 = first green, 2 = second green, 3 = B) that a CFA site yields.
fn plane_of(cfa: &Cfa, slots: &[[usize; 2]; 2], x: usize, y: usize) -> usize {
    match cfa.color_at(x, y) {
        0 => 0,
        2 => 3,
        _ => 1 + slots.get(y & 1).and_then(|r| r.get(x & 1)).copied().unwrap_or(0),
    }
}

fn check_bayer(cfa: &Cfa, w: usize, h: usize, len: usize) -> Result<()> {
    if !cfa.is_bayer() {
        return Err(RawError::Unsupported("pixel shift on a CFA that is not a 2×2 Bayer cell".into()));
    }
    if w < 4 || h < 4 || w.checked_mul(h).is_none_or(|n| n != len) {
        return Err(RawError::Corrupt("pixel-shift frame size does not match the image".into()));
    }
    Ok(())
}

/// The gain that brings `frame` to the brightness of `reference`: the median over 32×32 tiles of the ratio of
/// their black-subtracted sums, so that something moving in a few tiles does not bias it (drift of a fraction of a
/// percent between frames, seen in every sample file, would otherwise tint the merge and trip the motion test).
fn frame_gain(frame: &[u16], reference: &[u16], w: usize, h: usize, black: f32) -> f32 {
    const TILE: usize = 32;
    let n = (TILE * TILE) as f64;
    let mut ratios = Vec::new();
    for ty in 0..h / TILE {
        for tx in 0..w / TILE {
            let (mut a, mut b) = (0f64, 0f64);
            for y in ty * TILE..(ty + 1) * TILE {
                let start = y * w + tx * TILE;
                if let (Some(f), Some(r)) = (frame.get(start..start + TILE), reference.get(start..start + TILE)) {
                    a += f.iter().map(|&v| f64::from(v)).sum::<f64>();
                    b += r.iter().map(|&v| f64::from(v)).sum::<f64>();
                }
            }
            let (a, b) = (a - n * f64::from(black), b - n * f64::from(black));
            if b >= 16.0 * n && a > 0.0 {
                ratios.push(b / a);
            }
        }
    }
    if ratios.len() < 4 {
        return 1.0;
    }
    ratios.sort_by(f64::total_cmp);
    ratios.get(ratios.len() / 2).map_or(1.0, |&g| (g as f32).clamp(0.5, 2.0))
}

/// Arrange four Bayer frames (in [`SEQUENCE`] order, `w × h` samples each, `cfa` anchored at (0, 0)) as four samples
/// per scene point: `[R, G1, G2, B]`, where G1 is the green taken at the first green site of the CFA cell in
/// row-major order. Each frame is brought to frame 0's brightness and every sample is moved to a uniform black,
/// the mean of `black` (R, G, B), which is returned. Samples a frame lacks at the last row or column (its scene
/// shifted off the sensor) are taken two pixels in, from a site of the same colour. `equalise` brings frames 1 to 3
/// to frame 0's brightness first ([`frame_gain`]); without it the samples are only moved to the uniform black.
pub(crate) fn quads_from_frames(frames: [&[u16]; 4], w: usize, h: usize, cfa: &Cfa, black: [f32; 3], equalise: bool) -> Result<(Vec<u16>, f32)> {
    for f in &frames {
        check_bayer(cfa, w, h, f.len())?;
    }
    let slots = green_slots(cfa);
    // plane[k][py][px]: which of R, G1, G2, B frame k delivers at a point of parity (py, px)
    let mut plane = [[[0usize; 2]; 2]; 4];
    for py in 0..2usize {
        for px in 0..2usize {
            let mut seen = [false; 4];
            for (k, &(sy, sx)) in SEQUENCE.iter().enumerate() {
                let p = plane_of(cfa, &slots, (px + sx) & 1, (py + sy) & 1);
                if let Some(cell) = plane.get_mut(k).and_then(|a| a.get_mut(py)).and_then(|a| a.get_mut(px)) {
                    *cell = p;
                }
                if let Some(s) = seen.get_mut(p) {
                    *s = true;
                }
            }
            if !seen.iter().all(|&s| s) {
                return Err(RawError::Unsupported("the shifted frames do not cover R, G, G and B at every point".into()));
            }
        }
    }
    let black_out = (black[0] + black[1] + black[2]) / 3.0;
    let mean_black = (black[0] + 2.0 * black[1] + black[2]) / 4.0;
    let mut gains = [1f32; 4];
    if equalise {
        for k in 1..4 {
            if let (Some(g), Some(f)) = (gains.get_mut(k), frames.get(k)) {
                *g = frame_gain(f, frames[0], w, h, mean_black);
            }
        }
    }
    let total = w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or(RawError::Limit("image too large"))?;
    let mut quads = vec![0u16; total];
    quads.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            for (k, &(sy, sx)) in SEQUENCE.iter().enumerate() {
                let (mut py, mut px) = (y + sy, x + sx);
                if py >= h {
                    py -= 2;
                }
                if px >= w {
                    px -= 2;
                }
                let v = frames.get(k).and_then(|f| f.get(py * w + px)).copied().unwrap_or(0);
                let colour = usize::from(cfa.color_at((x + sx) & 1, (y + sy) & 1));
                let b = black.get(colour).copied().unwrap_or(0.0);
                let g = gains.get(k).copied().unwrap_or(1.0);
                let out = ((f32::from(v) - b) * g + black_out).round().clamp(0.0, 65535.0) as u16;
                let p = plane.get(k).and_then(|a| a.get(y & 1)).and_then(|a| a.get(x & 1)).copied().unwrap_or(0);
                if let Some(s) = row.get_mut(x * 4 + p) {
                    *s = out;
                }
            }
        }
    });
    Ok((quads, black_out))
}

/// The noise of the difference of the two green samples of a point, as variance = `a · level + b` (levels above
/// black), fitted from the image: for sixteen bands of the level (on a square-root scale) the lower quartile of
/// the absolute differences (a quartile of a zero-mean difference is `0.3186 σ`, and moving areas, which only
/// add large differences, inflate it little) gives σ; a line is fitted to the variances weighted by the band
/// populations, with both coefficients kept non-negative. `None` when too few samples fill any band.
fn noise_model(quads: &[u16], w: usize, h: usize, black: f32, white: f32) -> Option<(f32, f32)> {
    const BINS: usize = 16;
    let range = white - black;
    if range <= 0.0 {
        return None;
    }
    let sat = white * 0.98;
    let mut diffs: Vec<Vec<f32>> = vec![Vec::new(); BINS];
    let mut means = [0f64; BINS];
    for y in (0..h).step_by(5) {
        for x in (0..w).step_by(7) {
            let i = (y * w + x) * 4;
            let (Some(&ga), Some(&gb)) = (quads.get(i + 1), quads.get(i + 2)) else { continue };
            let (ga, gb) = (f32::from(ga), f32::from(gb));
            if ga >= sat || gb >= sat {
                continue;
            }
            let m = ((ga + gb) * 0.5 - black).max(0.0);
            let bin = (((m / range).sqrt() * BINS as f32) as usize).min(BINS - 1);
            if let (Some(d), Some(s)) = (diffs.get_mut(bin), means.get_mut(bin)) {
                d.push((ga - gb).abs());
                *s += f64::from(m);
            }
        }
    }
    // (weight, level, variance) per populated band
    let mut points: Vec<(f64, f64, f64)> = Vec::new();
    for (d, &sum) in diffs.iter_mut().zip(&means) {
        if d.len() < 200 {
            continue;
        }
        let k = d.len() / 4;
        let quartile = f64::from(*d.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1);
        let sigma = quartile / 0.3186;
        points.push((d.len() as f64, sum / d.len() as f64, sigma * sigma));
    }
    let (mut sw, mut sm, mut smm, mut sv, mut smv) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for &(wt, m, v) in &points {
        sw += wt;
        sm += wt * m;
        smm += wt * m * m;
        sv += wt * v;
        smv += wt * m * v;
    }
    if points.is_empty() || sw <= 0.0 {
        return None;
    }
    let det = sw * smm - sm * sm;
    let (mut a, mut b) = if points.len() < 2 || det.abs() < 1e-9 {
        (0.0, sv / sw)
    } else {
        let a = (sw * smv - sm * sv) / det;
        (a, (sv - a * sm) / sw)
    };
    if b < 0.0 {
        b = 0.0;
        a = if smm > 0.0 { smv / smm } else { 0.0 };
    }
    if a < 0.0 {
        a = 0.0;
        b = sv / sw;
    }
    Some((a as f32, b as f32))
}

/// A mask of the points whose two green samples disagree beyond [`NOISE_K`] standard deviations of `model`
/// (clipped samples are not judged), grown by [`DILATE`], and the number of points in it.
fn moving_mask(quads: &[u16], w: usize, h: usize, black: f32, white: f32, model: (f32, f32)) -> (Vec<u8>, usize) {
    let sat = white * 0.98;
    let mut raw = vec![0u8; w * h];
    raw.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, m) in row.iter_mut().enumerate() {
            let i = (y * w + x) * 4;
            let (Some(&ga), Some(&gb)) = (quads.get(i + 1), quads.get(i + 2)) else { continue };
            let (ga, gb) = (f32::from(ga), f32::from(gb));
            if ga >= sat || gb >= sat {
                continue;
            }
            let level = ((ga + gb) * 0.5 - black).max(0.0);
            let sigma = (model.0 * level + model.1).max(1.0).sqrt();
            *m = u8::from((ga - gb).abs() > NOISE_K * sigma);
        }
    });
    // horizontal then vertical grow
    let mut tmp = vec![0u8; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let Some(src) = raw.get(y * w..(y + 1) * w) else { return };
        for (x, o) in row.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(DILATE), (x + DILATE).min(w - 1));
            *o = u8::from(src.get(lo..=hi).is_some_and(|s| s.iter().any(|&m| m != 0)));
        }
    });
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (lo, hi) = (y.saturating_sub(DILATE), (y + DILATE).min(h - 1));
        for yy in lo..=hi {
            if let Some(src) = tmp.get(yy * w..(yy + 1) * w) {
                for (o, &s) in row.iter_mut().zip(src) {
                    *o |= s;
                }
            }
        }
    });
    let moving = out.iter().filter(|&&m| m != 0).count();
    (out, moving)
}

/// Merge four-sample points `[R, G1, G2, B]` (`w × h` of them, black at `black`, clipping at `white`) into RGB with
/// the moving points replaced by a single-frame demosaic of the unshifted frame (see the module notes). `cfa` is the
/// layout of that frame anchored at (0, 0): the sample of the cell site `(x, y)` is plane R / G1 / G2 / B per
/// [`plane_of`].
pub(crate) fn merge_quads(quads: &[u16], w: usize, h: usize, cfa: &Cfa, black: f32, white: f32) -> Result<Merged> {
    let total = w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or(RawError::Limit("image too large"))?;
    if quads.len() != total {
        return Err(RawError::Corrupt("pixel-shift samples do not match the image size".into()));
    }
    check_bayer(cfa, w, h, total / 4)?;
    let range = white - black;
    if range <= 0.0 {
        return Err(RawError::Corrupt("white level not above black in a pixel-shift image".into()));
    }
    let mut data = vec![0u16; total / 4 * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let q = |k: usize| u32::from(quads.get(i + k).copied().unwrap_or(0));
            let o = x * 3;
            if let Some(px) = row.get_mut(o..o + 3) {
                px.copy_from_slice(&[q(0) as u16, ((q(1) + q(2) + 1) / 2) as u16, q(3) as u16]);
            }
        }
    });
    let mut moving = 0;
    if let Some(model) = noise_model(quads, w, h, black, white) {
        let (mask, count) = moving_mask(quads, w, h, black, white, model);
        moving = count;
        if count > 0 {
            let slots = green_slots(cfa);
            let base = [[plane_of(cfa, &slots, 0, 0), plane_of(cfa, &slots, 1, 0)], [plane_of(cfa, &slots, 0, 1), plane_of(cfa, &slots, 1, 1)]];
            for y0 in (0..h).step_by(BAND) {
                let y1 = (y0 + BAND).min(h);
                if !mask.get(y0 * w..y1 * w).is_some_and(|m| m.iter().any(|&v| v != 0)) {
                    continue;
                }
                let (ey0, ey1) = (y0.saturating_sub(MARGIN), (y1 + MARGIN).min(h));
                let mut mosaic = vec![0f32; w * (ey1 - ey0)];
                for (by, row) in mosaic.chunks_mut(w).enumerate() {
                    let y = ey0 + by;
                    for (x, o) in row.iter_mut().enumerate() {
                        let p = base.get(y & 1).and_then(|r| r.get(x & 1)).copied().unwrap_or(0);
                        let v = quads.get((y * w + x) * 4 + p).copied().unwrap_or(0);
                        *o = (f32::from(v) - black) / range;
                    }
                }
                let n = Normalized { width: w, height: ey1 - ey0, cpp: 1, data: mosaic, cfa: Some(cfa.clone()) };
                let rgb = demosaic(&n, Method::Ppg);
                for y in y0..y1 {
                    for x in 0..w {
                        if mask.get(y * w + x).copied().unwrap_or(0) == 0 {
                            continue;
                        }
                        let at = (y * w + x) * 3;
                        let (Some(px), Some(out)) = (rgb.data.get((y - ey0) * w + x), data.get_mut(at..at + 3)) else { continue };
                        for (o, &v) in out.iter_mut().zip(px) {
                            *o = (v * range + black).round().clamp(0.0, 65535.0) as u16;
                        }
                    }
                }
            }
        }
    }
    Ok(Merged { data, black, moving })
}

/// Merge four Bayer frames in [`SEQUENCE`] order into RGB: [`quads_from_frames`] then [`merge_quads`]. `black` is
/// per colour (R, G, B), `white` the clip level of the frames, `equalise` as in [`quads_from_frames`].
pub(crate) fn merge_frames(frames: [&[u16]; 4], w: usize, h: usize, cfa: &Cfa, black: [f32; 3], white: f32, equalise: bool) -> Result<Merged> {
    let (quads, black_out) = quads_from_frames(frames, w, h, cfa, black, equalise)?;
    merge_quads(&quads, w, h, cfa, black_out, white)
}

/// Merge a Sony ARQ's samples (`w × h` pixels of `[R, G1, G2, B]`; black per plane) into RGB. The unshifted frame's
/// mosaic that the fallback demosaics is RGGB (see the module notes).
pub(crate) fn merge_arq(mut quads: Vec<u16>, w: usize, h: usize, black: [f32; 4], white: f32) -> Result<Merged> {
    let black_out = black.iter().sum::<f32>() / 4.0;
    quads.par_chunks_mut(4).for_each(|px| {
        for (v, &b) in px.iter_mut().zip(&black) {
            *v = (f32::from(*v) - b + black_out).round().clamp(0.0, 65535.0) as u16;
        }
    });
    merge_quads(&quads, w, h, &Cfa::bayer_static("RGGB"), black_out, white)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic textured scene: the RGB of scene point (x, y), in 100..=15000.
    fn scene(x: usize, y: usize) -> [u16; 3] {
        let h = |a: usize, b: usize| ((a.wrapping_mul(2654435761) ^ b.wrapping_mul(40503)).wrapping_add(a * b) % 4000) as u16;
        [
            2000 + (x * 37 % 5000) as u16 + h(x, y) / 4,
            4000 + (y * 53 % 6000) as u16 + h(y, x) / 4,
            1500 + ((x + y) * 29 % 4000) as u16 + h(x + 1, y + 2) / 4,
        ]
    }

    /// The frames a sensor with `cfa` records of `scene` shifted as [`SEQUENCE`] says, black `black` added.
    fn frames_of(w: usize, h: usize, cfa: &Cfa, black: u16, scene: &dyn Fn(usize, usize, usize) -> [u16; 3]) -> Vec<Vec<u16>> {
        (0..4)
            .map(|k| {
                let (sy, sx) = SEQUENCE[k];
                let mut f = vec![0u16; w * h];
                for y in 0..h {
                    for x in 0..w {
                        // the sensor pixel (x, y) sees scene point (x - sx, y - sy); the frame's first row/column see nothing: black
                        let c = usize::from(cfa.color_at(x, y));
                        f[y * w + x] = match (x.checked_sub(sx), y.checked_sub(sy)) {
                            (Some(qx), Some(qy)) => black + scene(k, qx, qy)[c],
                            _ => black,
                        };
                    }
                }
                f
            })
            .collect()
    }

    fn refs(f: &[Vec<u16>]) -> [&[u16]; 4] {
        [&f[0], &f[1], &f[2], &f[3]]
    }

    #[test]
    fn a_static_four_frame_set_merges_to_the_known_rgb_exactly() {
        for name in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let cfa = Cfa::bayer_static(name);
            let (w, h) = (64, 48);
            let frames = frames_of(w, h, &cfa, 64, &|_, x, y| scene(x, y));
            let m = merge_frames(refs(&frames), w, h, &cfa, [64.0; 3], 16000.0, false).unwrap();
            assert_eq!(m.black, 64.0);
            assert_eq!(m.moving, 0, "{name}");
            // every scene point whose four frames all have it on the sensor (not the last row and column)
            for y in 0..h - 1 {
                for x in 0..w - 1 {
                    let got = &m.data[(y * w + x) * 3..(y * w + x) * 3 + 3];
                    let want = scene(x, y).map(|v| v + 64);
                    assert_eq!(got, want, "{name} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn per_colour_black_levels_are_moved_to_one_black() {
        let cfa = Cfa::bayer_static("RGGB");
        let (w, h) = (32, 32);
        // frames recorded with black 50 / 70 / 90 on R / G / B: the merge subtracts each and adds the mean, 70
        let mut frames = frames_of(w, h, &cfa, 0, &|_, x, y| scene(x, y));
        for f in &mut frames {
            for y in 0..h {
                for x in 0..w {
                    f[y * w + x] += [50, 70, 90][usize::from(cfa.color_at(x, y))];
                }
            }
        }
        let m = merge_frames(refs(&frames), w, h, &cfa, [50.0, 70.0, 90.0], 16000.0, false).unwrap();
        assert_eq!(m.black, 70.0);
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                assert_eq!(m.data[(y * w + x) * 3..(y * w + x) * 3 + 3], scene(x, y).map(|v| v + 70), "({x}, {y})");
            }
        }
    }

    #[test]
    fn frame_order_decides_the_result() {
        let cfa = Cfa::bayer_static("RGGB");
        let (w, h) = (48, 32);
        let frames = frames_of(w, h, &cfa, 64, &|_, x, y| scene(x, y));
        let right = merge_frames(refs(&frames), w, h, &cfa, [64.0; 3], 16000.0, false).unwrap();
        // frames 1 and 2 exchanged: the offsets no longer match the data, the picture is wrong
        let swapped = [&frames[0][..], &frames[2][..], &frames[1][..], &frames[3][..]];
        let wrong = merge_frames(swapped, w, h, &cfa, [64.0; 3], 16000.0, false).unwrap();
        assert_ne!(right.data, wrong.data);
        let differs = |&(x, y): &(usize, usize)| {
            let at = (y * w + x) * 3;
            right.data[at..at + 3] != wrong.data[at..at + 3]
        };
        let n = (1..h - 1).flat_map(|y| (1..w - 1).map(move |x| (x, y))).filter(differs).count();
        assert!(n > (w - 2) * (h - 2) / 2, "only {n} points differ");
    }

    #[test]
    fn frames_are_put_in_index_order() {
        assert_eq!(order_frames(vec![("c", 2), ("a", 0), ("d", 3), ("b", 1)]), Some(["a", "b", "c", "d"]));
        assert_eq!(order_frames(vec![("a", 0), ("b", 1), ("c", 2), ("d", 3)]), Some(["a", "b", "c", "d"]));
        // a repeated index, an index out of range and a wrong count are not pixel shift
        assert_eq!(order_frames(vec![("a", 0), ("b", 1), ("c", 1), ("d", 3)]), None);
        assert_eq!(order_frames(vec![("a", 0), ("b", 1), ("c", 2), ("d", 4)]), None);
        assert_eq!(order_frames(vec![("a", 0), ("b", 1), ("c", 2)]), None);
        assert_eq!(order_frames::<u8>(Vec::new()), None);
    }

    /// A maker note whose block `0x0243` holds `block`, laid out as Pentax's is (`PENTAX \0`, byte order, an IFD at +10,
    /// offsets relative to the start of the note) and parsed the way a file's would be.
    fn note_with(block: &[u8]) -> makernote::MakerNote {
        let mut note = b"PENTAX \0II".to_vec();
        note.extend_from_slice(&1u16.to_le_bytes());
        note.extend_from_slice(&PENTAX_SHIFT_BLOCK.to_le_bytes());
        note.extend_from_slice(&7u16.to_le_bytes());
        note.extend_from_slice(&(block.len() as u32).to_le_bytes());
        let data_at = (note.len() + 4 + 4) as u32;
        if block.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..block.len()].copy_from_slice(block);
            note.extend_from_slice(&inline);
        } else {
            note.extend_from_slice(&data_at.to_le_bytes());
        }
        note.extend_from_slice(&0u32.to_le_bytes());
        if block.len() > 4 {
            note.extend_from_slice(block);
        }
        makernote::parse_makernote(&note, 0, note.len() as u64, ByteOrder::Little, "PENTAX").unwrap()
    }

    #[test]
    fn the_maker_note_block_names_the_frame() {
        // byte 0 = 1 (pixel shift), byte 2 = 4 frames, byte 7 = index: the layout of every frame of the samples
        let mut block = [1u8, 1, 4, 0, 0x20, 0, 1, 0, 1, 6];
        for index in 0..4u8 {
            block[7] = index;
            assert_eq!(pentax_frame_index(&note_with(&block)), Some(usize::from(index)));
        }
        // a single frame (`00 00 01 00 ...`), a three-frame bracket (zeros) and a block that is too short
        assert_eq!(pentax_frame_index(&note_with(&[0, 0, 1, 0, 0, 0, 0, 0, 0, 0])), None);
        assert_eq!(pentax_frame_index(&note_with(&[0; 10])), None);
        assert_eq!(pentax_frame_index(&note_with(&[1, 1, 4])), None);
        // an index outside 0..4
        assert_eq!(pentax_frame_index(&note_with(&[1, 1, 4, 0, 0, 0, 1, 9, 1, 6])), None);
    }

    #[test]
    fn something_that_moved_in_one_frame_comes_from_the_unshifted_frame() {
        let cfa = Cfa::bayer_static("RGGB");
        let (w, h) = (256, 192);
        let flat = [3000u16, 5000, 2000];
        // an object in frame 3 only: it adds 4000 to every sample of that frame inside the square
        let inside = |x: usize, y: usize| (100..140).contains(&x) && (80..120).contains(&y);
        let frames = frames_of(w, h, &cfa, 64, &|k, x, y| {
            let mut v = flat;
            if k == 3 && inside(x, y) {
                v = v.map(|c| c + 4000);
            }
            v
        });
        let m = merge_frames(refs(&frames), w, h, &cfa, [64.0; 3], 16000.0, true).unwrap();
        assert!(m.moving > 40 * 40 && m.moving < 70 * 70, "{} moving points", m.moving);
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let got = &m.data[(y * w + x) * 3..(y * w + x) * 3 + 3];
                // a demosaic of the flat frame 0 reproduces its constant colour; static points merge exactly
                for (c, &g) in got.iter().enumerate() {
                    let want = i32::from(flat[c]) + 64;
                    assert!((i32::from(g) - want).abs() <= 1, "({x}, {y}) channel {c}: {g} against {want}");
                }
            }
        }
    }

    #[test]
    fn an_arq_merges_its_planes_and_falls_back_to_the_rggb_mosaic() {
        let (w, h) = (256, 192);
        let flat = [3000u16, 5000, 2000];
        let mut quads = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                // the unshifted frame's own samples (G2 at odd rows, even columns) stay clean: it did not see the object
                let moved = (60..100).contains(&x) && (50..90).contains(&y) && !(y % 2 == 1 && x % 2 == 0);
                // black 512 on every plane; the second green plane is brighter where something moved
                quads.extend_from_slice(&[512 + flat[0], 512 + flat[1], 512 + flat[1] + if moved { 3000 } else { 0 }, 512 + flat[2]]);
            }
        }
        let m = merge_arq(quads, w, h, [512.0; 4], 16000.0).unwrap();
        assert_eq!(m.black, 512.0);
        assert!(m.moving > 40 * 40, "{} moving points", m.moving);
        for px in m.data.chunks(3) {
            for (c, &g) in px.iter().enumerate() {
                assert!((i32::from(g) - (i32::from(flat[c]) + 512)).abs() <= 1, "{px:?}");
            }
        }
    }

    #[test]
    fn a_static_arq_with_noise_keeps_every_point() {
        // noise as Poisson-like Gaussian-ish jitter from a deterministic generator: nothing may be called moving
        let (w, h) = (400, 300);
        let mut s = 12345u32;
        let mut jitter = |amp: i32| {
            let mut sum = 0i32;
            for _ in 0..4 {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                sum += ((s >> 16) as i32 % (2 * amp + 1)) - amp;
            }
            sum / 2
        };
        let mut quads = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let level = 1000 + ((x * 13 + y * 7) % 6000) as i32;
                let amp = 3 + ((level as f64).sqrt() / 4.0) as i32;
                for _ in 0..4 {
                    quads.push((512 + level + jitter(amp)).clamp(0, 65535) as u16);
                }
            }
        }
        let m = merge_arq(quads, w, h, [512.0; 4], 16000.0).unwrap();
        assert!(m.moving < w * h / 1000, "{} of {} points called moving", m.moving, w * h);
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        let cfa = Cfa::bayer_static("RGGB");
        let f = vec![0u16; 64];
        let short = vec![0u16; 63];
        assert!(merge_frames([&f, &f, &f, &short], 8, 8, &cfa, [0.0; 3], 100.0, true).is_err());
        assert!(merge_frames([&f, &f, &f, &f], 2, 32, &cfa, [0.0; 3], 100.0, true).is_err());
        assert!(merge_frames([&f, &f, &f, &f], 8, 8, &Cfa::xtrans(), [0.0; 3], 100.0, true).is_err());
        assert!(merge_quads(&vec![0u16; 8 * 8 * 4], 8, 8, &cfa, 100.0, 50.0).is_err());
        assert!(merge_quads(&[0u16; 5], 8, 8, &cfa, 0.0, 100.0).is_err());
        assert!(merge_arq(Vec::new(), 8, 8, [0.0; 4], 100.0).is_err());
    }
}
