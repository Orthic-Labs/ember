//! The histogram of a GPU render, counted on the device (issue #213).
//!
//! The counts are those of [`Histogram::of_srgb8`] on the same window: the same sampling stride
//! ([`Histogram::sample_stride`]), the same bins and the same integer luma, so the two agree
//! exactly. The CPU histogram stays the reference: when the counters cannot be used (the window
//! is outside the frame), the caller counts on the CPU.

use lightcraft_raster::Histogram;

use crate::ctx::{Buf, groups2};
use crate::render::Cx;

/// Words of the counter buffer: red, green, blue and luma (256 bins each), then the sampled total.
const WORDS: usize = 4 * Histogram::BINS + 1;

/// A window of the frame, `(x, y, w, h)` in its pixels.
pub(crate) type Window = (usize, usize, usize, usize);

/// Record the histogram of `window` of the `full.0 × full.1` frame in `src` (packed RGBA8 words,
/// red in the low byte, as the develop kernel writes them) onto `cx`. Returns the counters to pass
/// to [`read`] once the frame's commands have been submitted, or `None` when the window does not
/// lie inside the frame (the caller then counts on the CPU).
pub(crate) fn record(cx: &mut Cx<'_>, src: &Buf, full: (usize, usize), window: Window) -> Option<Buf> {
    let (w, h) = full;
    let (x0, y0, cw, ch) = window;
    let inside = |at: usize, len: usize, max: usize| at.checked_add(len).is_some_and(|end| end <= max);
    if cw == 0 || ch == 0 || !inside(x0, cw, w) || !inside(y0, ch, h) || src.len < w.checked_mul(h)? {
        return None;
    }
    let step = Histogram::sample_stride(cw.checked_mul(ch)?);
    let (sw, sh) = (cw.div_ceil(step), ch.div_ceil(step));
    let mut p = [0u32; 8];
    for (slot, v) in p.iter_mut().zip([cw, ch, w, x0, y0, step, sw, sh]) {
        *slot = u32::try_from(v).ok()?;
    }
    let counts = cx.zeroed(WORDS);
    cx.run("histogram", &p, &[Some(src), Some(&counts)], groups2(sw, sh, [16, 16]));
    Some(counts)
}

/// The histogram in the counters [`record`] returned. Reading submits what was recorded, so call
/// it after the frame's own readback. `None` for a malformed readback (the caller counts on the
/// CPU; a failed readback marks the render failed, see [`crate::ctx::failed`]).
pub(crate) fn read(cx: &mut Cx<'_>, counts: &Buf) -> Option<Histogram> {
    let v: Vec<u32> = cx.read(counts, WORDS);
    let n = Histogram::BINS;
    if v.len() != WORDS {
        return None;
    }
    let bins = |k: usize| v.get(k * n..(k + 1) * n).map(<[u32]>::to_vec);
    Some(Histogram { r: bins(0)?, g: bins(1)?, b: bins(2)?, luma: bins(3)?, total: *v.get(4 * n)?, hdr: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_raster::Rgba8;

    /// A synthetic frame: channels that wrap at different periods, so every bin is hit.
    fn frame(w: usize, h: usize) -> Rgba8 {
        Rgba8::from_fn(w, h, |x, y| [((x * 7 + y) % 256) as u8, ((x * 3 + y * 5) % 256) as u8, ((x ^ y) * 11 % 256) as u8, 255])
    }

    /// The packed words the develop kernel writes (red in the low byte).
    fn words(img: &Rgba8) -> Vec<u32> {
        img.data.iter().map(|p| u32::from_le_bytes(*p)).collect()
    }

    /// The GPU histogram equals the CPU one bin for bin, for the whole frame and for windows of
    /// it. Skips without a GPU adapter, like the other device tests.
    #[test]
    fn gpu_histogram_matches_the_cpu_one() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        // small frames sample every pixel (stride 1); the large one samples every second pixel
        for (w, h, windows) in
            [(301, 207, vec![(0, 0, 301, 207), (17, 9, 240, 150), (300, 206, 1, 1)]), (1201, 1003, vec![(0, 0, 1201, 1003), (5, 3, 1100, 990)])]
        {
            let img = frame(w, h);
            let packed = words(&img);
            for window in windows {
                let (x0, y0, cw, ch) = window;
                let want = Histogram::of_srgb8(&img.crop(x0, y0, cw, ch));
                let mut cx = Cx::new(gpu);
                let src = gpu.upload(&packed);
                let counts = record(&mut cx, &src, (w, h), window).expect("window inside the frame");
                let got = read(&mut cx, &counts).expect("counters read back");
                assert_eq!(got, want, "{w}×{h} window {window:?}");
            }
        }
    }

    /// A window outside the frame is left to the CPU.
    #[test]
    fn window_outside_the_frame_is_not_recorded() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let packed = words(&frame(16, 8));
        let mut cx = Cx::new(gpu);
        let src = gpu.upload(&packed);
        assert!(record(&mut cx, &src, (16, 8), (10, 0, 7, 8)).is_none());
        assert!(record(&mut cx, &src, (16, 8), (0, 0, 0, 8)).is_none());
    }
}
