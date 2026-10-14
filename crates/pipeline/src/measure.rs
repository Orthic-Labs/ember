//! Dense image measurement: a fixed-length, deterministic description of a display-linear
//! Rec. 2020 image (schema [`SCHEMA`], [`LEN`] values), the input of learned Auto.
//!
//! [`measure`] reads the same space [`crate::look::extract_linear`] reads (the 512 px import proxy,
//! corpus proxies, a rendered file decoded to the working space) and reports where its tones sit,
//! how colourful it is, which hues it holds, how colour casts through shadows and highlights, how
//! brightness and colour are spread over the frame, how much local contrast it has and what the
//! optional subject / sky / background / skin regions look like. Every field is defined in
//! normalized coordinates or relative radii, so a 256 px and a 512 px version of one image measure
//! nearly the same (`tests_measure.rs` states the tolerance per group).
//!
//! Determinism: statistics are exact order statistics (`f32::total_cmp` sorts, integer-ranked
//! percentiles) or f64 sums taken in pixel order on one thread; there is no randomness and no hash
//! iteration. The same input gives bit-identical output within one build on one platform. `powf`, `cbrt`, `atan2` and the other
//! transcendental functions come from the platform maths library, so another platform can differ by about one ulp.
//! Field list: `docs/dense-measurement.md`.
//!
//! Never-crash: pixels with a non-finite channel are skipped, every derived value is finite by
//! construction, input sizes are checked (and capped at [`MAX_PIXELS`]) before anything is
//! allocated, and large buffers are reserved fallibly.

use std::f64::consts::TAU;
use std::fmt;

use lightcraft_color::perceptual::{lab_to_lch, oklab_from_2020};
use lightcraft_color::{REC2020, SRGB, luminance_2020};
use lightcraft_raster::{Plane, Rgb32f};
use serde::{Deserialize, Serialize};

use crate::auto::fit::encode;

/// Identity of the field set below. Bump it when a field is added, removed, reordered or redefined:
/// models trained on one schema must not read another.
pub const SCHEMA: &str = "ember.dense-measure.v1";

/// Number of values in [`Measurements::to_vec`] / [`Measurements::names`].
pub const LEN: usize = 202;

/// Largest image [`measure`] accepts (64 megapixels).
pub const MAX_PIXELS: usize = 64_000_000;

/// Fewest finite pixels [`measure`] accepts.
pub const MIN_FINITE_PIXELS: usize = 16;

/// Linear channel values are limited to ±this before use, so no finite input can overflow a sum or
/// a cube root. Far above anything the encoded (clamped) statistics can tell apart.
const LIN_LIMIT: f32 = 1.0e6;
/// Display-linear luminance at or over which a pixel counts as clipped (as `look.rs` defines it).
const CLIP_LUMINANCE: f32 = 0.985;
/// Encoded luminance at or under which a pixel counts as crushed.
const CRUSHED_ENCODED: f32 = 1.0 / 255.0;
/// Oklab chroma a pixel needs to count towards a hue band.
const HUE_MIN_CHROMA: f32 = 0.02;
/// Summed band weight under which a hue band reports zeros.
const HUE_MIN_WEIGHT: f64 = 1e-6;
/// Oklab L under which a pixel is a shadow, and over which it is a highlight.
const SHADOW_L: f32 = 0.4;
const HIGHLIGHT_L: f32 = 0.7;
/// Neutral cast: Oklab L range and chroma ceiling of the pixels that should be grey.
const NEUTRAL_L: (f32, f32) = (0.3, 0.85);
const NEUTRAL_CHROMA: f32 = 0.08;
/// Spatial grid cells per side, and radial rings.
const GRID: usize = 4;
const RINGS: usize = 5;
/// A region under this share of the frame is reported absent.
const REGION_MIN_SHARE: f64 = 0.005;
/// Alpha a pixel needs to join a region's percentile sets.
const REGION_HARD_ALPHA: f32 = 0.5;
/// Local-contrast box half-widths as shares of the long edge: the box spans twice the share.
const LOCAL_RADII: [f64; 3] = [0.01, 0.04, 0.16];
/// Luminance floor under the log of the local-contrast measure.
const LOG_FLOOR: f32 = 1e-6;

/// Percentiles are ranked in per-mille (integers), so no float rounding picks the rank.
const TONE_PERMILLE: [u64; 17] = [5, 10, 20, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950, 980, 990, 995];
const TONE_NAMES: [&str; 17] =
    ["p0_5", "p1", "p2", "p5", "p10", "p20", "p30", "p40", "p50", "p60", "p70", "p80", "p90", "p95", "p98", "p99", "p99_5"];
const CHROMA_PERMILLE: [u64; 5] = [100, 250, 500, 750, 900];
const CHROMA_NAMES: [&str; 5] = ["p10", "p25", "p50", "p75", "p90"];
const REGION_PERMILLE: [u64; 5] = [50, 250, 500, 750, 950];
const REGION_NAMES: [&str; 5] = ["p5", "p25", "p50", "p75", "p95"];

/// The hue bands, in output order.
pub const HUE_BANDS: [&str; 8] = ["red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta"];
/// Reference colours of [`HUE_BANDS`] (same order): linear sRGB values, whose Oklch hues are the
/// band centres.
pub(crate) const REFERENCE_COLOURS: [[f32; 3]; 8] =
    [[1.0, 0.0, 0.0], [1.0, 0.5, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 0.0, 1.0], [0.5, 0.0, 1.0], [1.0, 0.0, 1.0]];
const TINT_NAMES: [&str; 3] = ["shadows", "midtones", "highlights"];
const LOCAL_NAMES: [&str; 3] = ["r1pct", "r4pct", "r16pct"];

/// What a region plane marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegionKind {
    Subject,
    Sky,
    Background,
    Skin,
}

impl RegionKind {
    /// Output order.
    pub const ALL: [RegionKind; 4] = [RegionKind::Subject, RegionKind::Sky, RegionKind::Background, RegionKind::Skin];

    /// Lowercase name, as used in field names (`region.sky.lum.p50`).
    pub const fn name(self) -> &'static str {
        match self {
            RegionKind::Subject => "subject",
            RegionKind::Sky => "sky",
            RegionKind::Background => "background",
            RegionKind::Skin => "skin",
        }
    }

    /// Position in [`RegionKind::ALL`].
    pub const fn index(self) -> usize {
        match self {
            RegionKind::Subject => 0,
            RegionKind::Sky => 1,
            RegionKind::Background => 2,
            RegionKind::Skin => 3,
        }
    }
}

/// Why an image could not be measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeasureError {
    /// Zero width or height.
    EmptyImage,
    /// The pixel buffer does not match the stated size.
    MalformedImage,
    /// More than [`MAX_PIXELS`] pixels.
    TooLarge { pixels: usize },
    /// Fewer than [`MIN_FINITE_PIXELS`] pixels with all channels finite.
    TooFewPixels { finite: usize },
    /// A region plane's size differs from the image's.
    RegionSizeMismatch(RegionKind),
    /// The same region kind was supplied twice.
    DuplicateRegion(RegionKind),
    /// A working buffer could not be allocated.
    OutOfMemory,
}

impl fmt::Display for MeasureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeasureError::EmptyImage => write!(f, "cannot measure an empty image"),
            MeasureError::MalformedImage => write!(f, "image buffer does not match its size"),
            MeasureError::TooLarge { pixels } => write!(f, "image of {pixels} pixels exceeds the {MAX_PIXELS} pixel measurement limit"),
            MeasureError::TooFewPixels { finite } => write!(f, "only {finite} finite pixels; at least {MIN_FINITE_PIXELS} are needed"),
            MeasureError::RegionSizeMismatch(k) => write!(f, "the {} region plane is not the image's size", k.name()),
            MeasureError::DuplicateRegion(k) => write!(f, "the {} region was supplied twice", k.name()),
            MeasureError::OutOfMemory => write!(f, "out of memory while measuring"),
        }
    }
}

impl std::error::Error for MeasureError {}

/// Encoded tone percentiles (see [`Measurements::tone`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tone {
    /// Encoded luminance (`luminance_2020`, gamma 2.2) percentiles 0.5, 1, 2, 5, 10, 20, 30, 40, 50, 60, 70, 80, 90, 95, 98, 99, 99.5.
    pub lum: [f32; 17],
    /// Encoded red channel, same percentiles.
    pub r: [f32; 17],
    /// Encoded green channel, same percentiles.
    pub g: [f32; 17],
    /// Encoded blue channel, same percentiles.
    pub b: [f32; 17],
}

/// Share of the frame at the tone extremes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// Share of pixels with display-linear luminance at or over 0.985 (`look.rs`'s clip definition).
    pub high: f32,
    /// Share of pixels with encoded luminance at or under 1/255.
    pub low: f32,
}

/// One hue band ([`HUE_BANDS`] order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HueBand {
    /// Summed band weight over the count of all finite pixels.
    pub share: f32,
    /// Weighted circular mean of (pixel hue − band centre), radians; positive is towards larger Oklch hue.
    pub offset: f32,
    /// Weighted mean Oklab chroma.
    pub chroma: f32,
    /// Weighted mean Oklab L.
    pub lum: f32,
}

/// One tonal range (shadows, midtones, highlights).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TintRange {
    /// Share of finite pixels in the range.
    pub share: f32,
    /// Mean Oklab a over the range (0 when empty).
    pub a: f32,
    /// Mean Oklab b over the range (0 when empty).
    pub b: f32,
}

/// Colour of the pixels that should be grey.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neutral {
    pub a: f32,
    pub b: f32,
}

/// 4 × 4 spatial grid, row-major (row 0 is the top row).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    /// Mean encoded luminance per cell.
    pub lum: [f32; 16],
    /// Mean Oklab chroma per cell.
    pub chroma: [f32; 16],
}

/// One region's statistics. All zeros (with `present` 0) when the region is absent or too small.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionStats {
    /// 1 when the region was supplied and covers at least 0.5 % of the frame, else 0.
    pub present: f32,
    /// Mean alpha over the finite pixels.
    pub share: f32,
    /// Encoded luminance percentiles 5, 25, 50, 75, 95 over pixels with alpha ≥ 0.5.
    pub lum: [f32; 5],
    /// Alpha-weighted mean Oklab L.
    pub l: f32,
    /// Alpha-weighted mean Oklab a.
    pub a: f32,
    /// Alpha-weighted mean Oklab b.
    pub b: f32,
    /// Oklab chroma median over pixels with alpha ≥ 0.5.
    pub chroma_p50: f32,
}

/// The dense measurement of one image: [`LEN`] values in a fixed order ([`Measurements::to_vec`],
/// named by [`Measurements::names`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurements {
    /// Always [`SCHEMA`].
    pub schema: String,
    /// 4 channels × 17 percentiles = 68.
    pub tone: Tone,
    /// 2.
    pub clip: Clip,
    /// Oklab chroma percentiles 10, 25, 50, 75, 90 (5).
    pub chroma: [f32; 5],
    /// 8 bands × 4 = 32, in [`HUE_BANDS`] order.
    pub hue: [HueBand; 8],
    /// Shadows (L < 0.4), midtones (0.4 ..= 0.7), highlights (L > 0.7): 3 × 3 = 9.
    pub tint: [TintRange; 3],
    /// Mean Oklab a, b over pixels with 0.3 ≤ L ≤ 0.85 and chroma < 0.08 (2).
    pub neutral: Neutral,
    /// 16 + 16 = 32.
    pub grid: Grid,
    /// Mean encoded luminance in rings of normalized distance from the centre (over the half
    /// diagonal): [0, .2), [.2, .4), [.4, .6), [.6, .8), [.8, 1] (5).
    pub radial: [f32; 5],
    /// Mean |log2 luminance − its box blur| for box radii of 1 %, 4 % and 16 % of the long edge (3).
    pub local_contrast: [f32; 3],
    /// Subject, sky, background, skin: 4 × 11 = 44.
    pub regions: [RegionStats; 4],
}

impl Default for Measurements {
    /// All zeros, current schema.
    fn default() -> Self {
        Measurements {
            schema: SCHEMA.to_string(),
            tone: Tone::default(),
            clip: Clip::default(),
            chroma: [0.0; 5],
            hue: <[HueBand; 8]>::default(),
            tint: <[TintRange; 3]>::default(),
            neutral: Neutral::default(),
            grid: Grid::default(),
            radial: [0.0; 5],
            local_contrast: [0.0; 3],
            regions: <[RegionStats; 4]>::default(),
        }
    }
}

impl Measurements {
    /// Visits every value with its dotted name, in the schema's fixed order. The one place that
    /// defines order and names, so `to_vec`, `names` and `validate` cannot disagree.
    fn walk(&self, f: &mut dyn FnMut(String, f32)) {
        let channels = [("lum", &self.tone.lum), ("r", &self.tone.r), ("g", &self.tone.g), ("b", &self.tone.b)];
        for (channel, values) in channels {
            for (p, v) in TONE_NAMES.iter().zip(values) {
                f(format!("tone.{channel}.{p}"), *v);
            }
        }
        f("clip.high".to_string(), self.clip.high);
        f("clip.low".to_string(), self.clip.low);
        for (p, v) in CHROMA_NAMES.iter().zip(&self.chroma) {
            f(format!("chroma.{p}"), *v);
        }
        for (band, b) in HUE_BANDS.iter().zip(&self.hue) {
            f(format!("hue.{band}.share"), b.share);
            f(format!("hue.{band}.offset"), b.offset);
            f(format!("hue.{band}.chroma"), b.chroma);
            f(format!("hue.{band}.lum"), b.lum);
        }
        for (range, t) in TINT_NAMES.iter().zip(&self.tint) {
            f(format!("tint.{range}.share"), t.share);
            f(format!("tint.{range}.a"), t.a);
            f(format!("tint.{range}.b"), t.b);
        }
        f("neutral.a".to_string(), self.neutral.a);
        f("neutral.b".to_string(), self.neutral.b);
        for (what, values) in [("lum", &self.grid.lum), ("chroma", &self.grid.chroma)] {
            for (i, v) in values.iter().enumerate() {
                f(format!("grid.{what}.r{}c{}", i / GRID, i % GRID), *v);
            }
        }
        for (i, v) in self.radial.iter().enumerate() {
            f(format!("radial.ring{i}"), *v);
        }
        for (n, v) in LOCAL_NAMES.iter().zip(&self.local_contrast) {
            f(format!("local_contrast.{n}"), *v);
        }
        for (kind, r) in RegionKind::ALL.iter().zip(&self.regions) {
            let k = kind.name();
            f(format!("region.{k}.present"), r.present);
            f(format!("region.{k}.share"), r.share);
            for (p, v) in REGION_NAMES.iter().zip(&r.lum) {
                f(format!("region.{k}.lum.{p}"), *v);
            }
            f(format!("region.{k}.oklab.l"), r.l);
            f(format!("region.{k}.oklab.a"), r.a);
            f(format!("region.{k}.oklab.b"), r.b);
            f(format!("region.{k}.chroma.p50"), r.chroma_p50);
        }
    }

    /// The [`LEN`] values in schema order. Every value is finite (a non-finite value in a
    /// hand-built or deserialized struct reads as 0; [`Measurements::validate`] rejects it).
    pub fn to_vec(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(LEN);
        self.walk(&mut |_, v| out.push(if v.is_finite() { v } else { 0.0 }));
        out
    }

    /// Stable dotted names of [`Measurements::to_vec`]'s values, same length and order.
    pub fn names() -> Vec<String> {
        let mut out = Vec::with_capacity(LEN);
        Self::default().walk(&mut |name, _| out.push(name));
        out
    }

    /// Rejects a measurement that could not have come from [`measure`] (hostile JSON): wrong schema
    /// or any non-finite value.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!("unsupported dense measurement schema `{}`", self.schema));
        }
        let mut bad: Option<String> = None;
        self.walk(&mut |name, v| {
            if bad.is_none() && !v.is_finite() {
                bad = Some(name);
            }
        });
        match bad {
            Some(name) => Err(format!("dense measurement value `{name}` is not finite")),
            None => Ok(()),
        }
    }

    /// The statistics of one region.
    pub fn region(&self, kind: RegionKind) -> &RegionStats {
        // `kind.index()` is 0..4 by construction, not input-derived
        &self.regions[kind.index()]
    }
}

/// The Oklch hue (radians, −π..π) of each hue band's centre, in [`HUE_BANDS`] order: the hue of the
/// band's reference colour (linear sRGB, converted to Rec. 2020 and Oklab).
pub fn hue_band_centres() -> [f32; 8] {
    let to_2020 = SRGB.to_space(&REC2020);
    let mut out = [0.0f32; 8];
    for (o, rgb) in out.iter_mut().zip(&REFERENCE_COLOURS) {
        *o = lab_to_lch(oklab_from_2020(to_2020.apply_f32(*rgb)))[2];
    }
    out
}

/// (band index, centre hue), ascending by hue.
type Centres = [(usize, f64); 8];

fn sorted_centres() -> Centres {
    let mut c: Centres = [(0, 0.0); 8];
    for (i, (slot, hue)) in c.iter_mut().zip(hue_band_centres()).enumerate() {
        *slot = (i, f64::from(hue));
    }
    c.sort_by(|x, y| x.1.total_cmp(&y.1));
    c
}

/// Splits a hue between its two neighbouring band centres (circularly): `(band, weight, centre)`
/// for the lower and the upper neighbour, weights summing to 1 and falling linearly with angular
/// distance. The returned centres are unwrapped, so `theta − centre` is the signed offset inside the
/// gap (never across ±π).
fn split(sorted: &Centres, theta: f64) -> [(usize, f64, f64); 2] {
    let n = sorted.len();
    // number of centres at or below theta: the lower neighbour is the last of them (wrapping)
    let j = sorted.partition_point(|c| c.1 <= theta);
    let (Some(&(lower_band, lower_hue)), Some(&(upper_band, upper_hue))) = (sorted.get((j + n - 1) % n), sorted.get(j % n)) else {
        return [(0, 0.0, 0.0); 2];
    };
    let lower_hue = if j == 0 { lower_hue - TAU } else { lower_hue };
    let upper_hue = if j == n { upper_hue + TAU } else { upper_hue };
    let gap = upper_hue - lower_hue;
    let t = if gap > 0.0 { ((theta - lower_hue) / gap).clamp(0.0, 1.0) } else { 0.0 };
    [(lower_band, 1.0 - t, lower_hue), (upper_band, t, upper_hue)]
}

#[derive(Clone, Copy, Default)]
struct BandAcc {
    weight: f64,
    sin: f64,
    cos: f64,
    chroma: f64,
    l: f64,
}

#[derive(Clone, Copy, Default)]
struct MeanAcc {
    n: u64,
    a: f64,
    b: f64,
}

#[derive(Clone, Copy, Default)]
struct CellAcc {
    n: u64,
    lum: f64,
    chroma: f64,
}

#[derive(Default)]
struct RegionAcc {
    alpha: f64,
    l: f64,
    a: f64,
    b: f64,
    lum: Vec<f32>,
    chroma: Vec<f32>,
}

#[inline]
fn finite3(p: &[f32; 3]) -> bool {
    p[0].is_finite() && p[1].is_finite() && p[2].is_finite()
}

#[inline]
fn sanitize(p: [f32; 3]) -> [f32; 3] {
    p.map(|v| v.clamp(-LIN_LIMIT, LIN_LIMIT))
}

/// An f64 statistic as the finite f32 it reports.
#[inline]
fn fin(v: f64) -> f32 {
    let f = v as f32;
    if f.is_finite() { f } else { 0.0 }
}

#[inline]
fn mean(sum: f64, n: u64) -> f64 {
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// The value at per-mille rank `permille` of a sorted slice: element `⌊(n−1)·permille/1000⌋` (the
/// rank rule `auto.rs` and `look.rs` use, in integer arithmetic). 0 for an empty slice.
fn pick(sorted: &[f32], permille: u64) -> f32 {
    let Some(last) = sorted.len().checked_sub(1) else { return 0.0 };
    let rank = (last as u64).saturating_mul(permille) / 1000;
    usize::try_from(rank).ok().and_then(|i| sorted.get(i)).copied().unwrap_or(0.0)
}

fn quantiles<const N: usize>(sorted: &[f32], permille: &[u64; N]) -> [f32; N] {
    let mut out = [0.0f32; N];
    for (o, &p) in out.iter_mut().zip(permille) {
        *o = pick(sorted, p);
    }
    out
}

/// An empty vector with room for `n` elements, or [`MeasureError::OutOfMemory`].
fn reserved<T>(n: usize) -> Result<Vec<T>, MeasureError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| MeasureError::OutOfMemory)?;
    Ok(v)
}

/// Region alpha at pixel `i`: finite, clamped to 0..=1; 0 outside the plane.
fn alpha_at(plane: &Plane, i: usize) -> f32 {
    match plane.data.get(i) {
        Some(a) if a.is_finite() => a.clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// The image's pixel count after the structural checks.
fn checked_pixels(img: &Rgb32f) -> Result<usize, MeasureError> {
    if img.width == 0 || img.height == 0 {
        return Err(MeasureError::EmptyImage);
    }
    let n = img.width.saturating_mul(img.height);
    if n > MAX_PIXELS {
        return Err(MeasureError::TooLarge { pixels: n });
    }
    if img.data.len() != n {
        return Err(MeasureError::MalformedImage);
    }
    Ok(n)
}

/// The supplied region planes by [`RegionKind::index`], checked against the image.
fn region_slots<'a>(img: &Rgb32f, n: usize, regions: &[(RegionKind, &'a Plane)]) -> Result<[Option<&'a Plane>; 4], MeasureError> {
    let mut slots: [Option<&'a Plane>; 4] = [None; 4];
    for &(kind, plane) in regions {
        let Some(slot) = slots.get_mut(kind.index()) else { continue };
        if slot.is_some() {
            return Err(MeasureError::DuplicateRegion(kind));
        }
        if plane.width != img.width || plane.height != img.height || plane.data.len() != n {
            return Err(MeasureError::RegionSizeMismatch(kind));
        }
        *slot = Some(plane);
    }
    Ok(slots)
}

/// Sorted percentiles of one encoded tone channel over the finite pixels.
fn tone_channel(img: &Rgb32f, finite: usize, channel: impl Fn([f32; 3]) -> f32) -> Result<[f32; 17], MeasureError> {
    let mut values: Vec<f32> = reserved(finite)?;
    for &p in &img.data {
        if finite3(&p) {
            values.push(channel(sanitize(p)));
        }
    }
    values.sort_unstable_by(f32::total_cmp);
    Ok(quantiles(&values, &TONE_PERMILLE))
}

/// Mean |v − box mean of v| of the log2 luminance, for the three [`LOCAL_RADII`].
///
/// Box means come from a summed-area table in f64 (exact, fixed summation order). The window is
/// clipped at the frame's edge (the mean over the pixels inside it). A radius is a real number of
/// pixels (share × long edge, at least 1): the boxes at the two neighbouring integer radii are
/// blended by the fraction, so the radius scales smoothly with resolution. A non-finite pixel is
/// stood in for by the frame's mean log luminance inside the table and left out of the average.
fn local_contrast_means(img: &Rgb32f, finite: usize) -> Result<[f32; 3], MeasureError> {
    let (w, h) = (img.width, img.height);
    let log_lum = |p: [f32; 3]| f64::from(luminance_2020(sanitize(p)).max(LOG_FLOOR)).log2();
    let mut total = 0.0f64;
    for &p in &img.data {
        if finite3(&p) {
            total += log_lum(p);
        }
    }
    let fill = total / finite.max(1) as f64;

    // sat[(y * stride) + x] = sum of the log luminance over pixels [0, x) × [0, y)
    let stride = w + 1;
    let cells = stride.checked_mul(h + 1).ok_or(MeasureError::TooLarge { pixels: usize::MAX })?;
    let mut sat: Vec<f64> = reserved(cells)?;
    sat.resize(stride, 0.0);
    for (y, row) in img.data.chunks_exact(w).enumerate() {
        let above = y * stride;
        sat.push(0.0);
        let mut run = 0.0f64;
        for (x, &p) in row.iter().enumerate() {
            run += if finite3(&p) { log_lum(p) } else { fill };
            let up = sat.get(above + x + 1).copied().unwrap_or(0.0);
            sat.push(up + run);
        }
    }

    let mean_at = |x: usize, y: usize, r: usize| -> f64 {
        let (x0, x1) = (x.saturating_sub(r), x.saturating_add(r).min(w - 1));
        let (y0, y1) = (y.saturating_sub(r), y.saturating_add(r).min(h - 1));
        let at = |cx: usize, cy: usize| sat.get(cy * stride + cx).copied().unwrap_or(0.0);
        let sum = at(x1 + 1, y1 + 1) - at(x0, y1 + 1) - at(x1 + 1, y0) + at(x0, y0);
        sum / ((x1 - x0 + 1) * (y1 - y0 + 1)) as f64
    };

    let long_edge = w.max(h) as f64;
    // (integer radius below, fraction towards the next)
    let radii: [(usize, f64); 3] = LOCAL_RADII.map(|share| {
        // box width 2r + 1 equals 2 x share x long edge, so the box covers the same share of the
        // frame at every resolution
        let r = ((2.0 * share * long_edge - 1.0) / 2.0).max(1.0);
        let lo = r.floor();
        (lo as usize, r - lo)
    });
    let mut sums = [0.0f64; 3];
    for (y, row) in img.data.chunks_exact(w).enumerate() {
        for (x, &p) in row.iter().enumerate() {
            if !finite3(&p) {
                continue;
            }
            let v = log_lum(p);
            for (sum, &(lo, t)) in sums.iter_mut().zip(&radii) {
                let blur = if t > 0.0 { (1.0 - t) * mean_at(x, y, lo) + t * mean_at(x, y, lo.saturating_add(1)) } else { mean_at(x, y, lo) };
                *sum += (v - blur).abs();
            }
        }
    }
    let denom = finite.max(1) as f64;
    Ok(sums.map(|s| fin(s / denom)))
}

/// Measures a display-linear Rec. 2020 image (see the module docs), with optional region alpha
/// planes (values 0..=1, the image's width and height; a non-finite alpha reads as 0).
///
/// Errors: zero-size, wrongly sized or over-[`MAX_PIXELS`] image, fewer than [`MIN_FINITE_PIXELS`]
/// finite pixels, a region plane of another size, the same [`RegionKind`] twice.
pub fn measure(img: &Rgb32f, regions: &[(RegionKind, &Plane)]) -> Result<Measurements, MeasureError> {
    let n = checked_pixels(img)?;
    let slots = region_slots(img, n, regions)?;
    let finite = img.data.iter().filter(|p| finite3(p)).count();
    if finite < MIN_FINITE_PIXELS {
        return Err(MeasureError::TooFewPixels { finite });
    }
    let denom = finite as f64;
    let (w, h) = (img.width, img.height);
    let (wf, hf) = (w as f64, h as f64);
    let half_diagonal = 0.5 * wf.hypot(hf);
    let centres = sorted_centres();

    let mut chroma_values: Vec<f32> = reserved(finite)?;
    let mut hue_acc = [BandAcc::default(); 8];
    let mut tint_acc = [MeanAcc::default(); 3];
    let mut neutral_acc = MeanAcc::default();
    let mut grid_acc = [CellAcc::default(); GRID * GRID];
    let mut ring_acc = [CellAcc::default(); RINGS];
    let (mut clip_high, mut clip_low) = (0u64, 0u64);
    let mut region_acc: [RegionAcc; 4] = Default::default();
    for (acc, slot) in region_acc.iter_mut().zip(&slots) {
        let Some(plane) = slot else { continue };
        let hard = img.data.iter().zip(&plane.data).filter(|&(p, a)| finite3(p) && a.is_finite() && *a >= REGION_HARD_ALPHA).count();
        acc.lum = reserved(hard)?;
        acc.chroma = reserved(hard)?;
    }

    for (y, row) in img.data.chunks_exact(w).enumerate() {
        let grid_row = (y * GRID / h).min(GRID - 1);
        let dy = y as f64 + 0.5 - 0.5 * hf;
        for (x, &raw) in row.iter().enumerate() {
            if !finite3(&raw) {
                continue;
            }
            let s = sanitize(raw);
            let luminance = luminance_2020(s);
            let encoded = encode(luminance);
            let lab = oklab_from_2020(s.map(|v| v.max(0.0)));
            let [l, c, theta] = lab_to_lch(lab);
            let (l64, a64, b64, c64, e64) = (f64::from(l), f64::from(lab[1]), f64::from(lab[2]), f64::from(c), f64::from(encoded));

            if luminance >= CLIP_LUMINANCE {
                clip_high += 1;
            }
            if encoded <= CRUSHED_ENCODED {
                clip_low += 1;
            }
            chroma_values.push(c);

            if c >= HUE_MIN_CHROMA {
                let theta = f64::from(theta);
                for (band, weight, centre) in split(&centres, theta) {
                    if weight <= 0.0 {
                        continue;
                    }
                    let (sin, cos) = (theta - centre).sin_cos();
                    if let Some(acc) = hue_acc.get_mut(band) {
                        acc.weight += weight;
                        acc.sin += weight * sin;
                        acc.cos += weight * cos;
                        acc.chroma += weight * c64;
                        acc.l += weight * l64;
                    }
                }
            }

            let range = if l < SHADOW_L {
                0
            } else if l > HIGHLIGHT_L {
                2
            } else {
                1
            };
            if let Some(acc) = tint_acc.get_mut(range) {
                acc.n += 1;
                acc.a += a64;
                acc.b += b64;
            }
            if (NEUTRAL_L.0..=NEUTRAL_L.1).contains(&l) && c < NEUTRAL_CHROMA {
                neutral_acc.n += 1;
                neutral_acc.a += a64;
                neutral_acc.b += b64;
            }

            if let Some(cell) = grid_acc.get_mut(grid_row * GRID + (x * GRID / w).min(GRID - 1)) {
                cell.n += 1;
                cell.lum += e64;
                cell.chroma += c64;
            }
            let dx = x as f64 + 0.5 - 0.5 * wf;
            let ring = ((dx.hypot(dy) / half_diagonal * RINGS as f64) as usize).min(RINGS - 1);
            if let Some(acc) = ring_acc.get_mut(ring) {
                acc.n += 1;
                acc.lum += e64;
            }

            for (acc, slot) in region_acc.iter_mut().zip(&slots) {
                let Some(plane) = slot else { continue };
                let alpha = alpha_at(plane, y * w + x);
                if alpha <= 0.0 {
                    continue;
                }
                let weight = f64::from(alpha);
                acc.alpha += weight;
                acc.l += weight * l64;
                acc.a += weight * a64;
                acc.b += weight * b64;
                if alpha >= REGION_HARD_ALPHA {
                    acc.lum.push(encoded);
                    acc.chroma.push(c);
                }
            }
        }
    }

    chroma_values.sort_unstable_by(f32::total_cmp);
    let chroma = quantiles(&chroma_values, &CHROMA_PERMILLE);
    drop(chroma_values);

    let mut hue = <[HueBand; 8]>::default();
    for (out, acc) in hue.iter_mut().zip(&hue_acc) {
        if acc.weight >= HUE_MIN_WEIGHT {
            *out = HueBand {
                share: fin(acc.weight / denom),
                offset: fin(acc.sin.atan2(acc.cos)),
                chroma: fin(acc.chroma / acc.weight),
                lum: fin(acc.l / acc.weight),
            };
        }
    }
    let mut tint = <[TintRange; 3]>::default();
    for (out, acc) in tint.iter_mut().zip(&tint_acc) {
        *out = TintRange { share: fin(acc.n as f64 / denom), a: fin(mean(acc.a, acc.n)), b: fin(mean(acc.b, acc.n)) };
    }
    let neutral = Neutral { a: fin(mean(neutral_acc.a, neutral_acc.n)), b: fin(mean(neutral_acc.b, neutral_acc.n)) };
    let mut grid = Grid::default();
    for ((lum_out, chroma_out), cell) in grid.lum.iter_mut().zip(grid.chroma.iter_mut()).zip(&grid_acc) {
        *lum_out = fin(mean(cell.lum, cell.n));
        *chroma_out = fin(mean(cell.chroma, cell.n));
    }
    let mut radial = [0.0f32; RINGS];
    for (out, acc) in radial.iter_mut().zip(&ring_acc) {
        *out = fin(mean(acc.lum, acc.n));
    }

    let tone = Tone {
        lum: tone_channel(img, finite, |s| encode(luminance_2020(s)))?,
        r: tone_channel(img, finite, |s| encode(s[0]))?,
        g: tone_channel(img, finite, |s| encode(s[1]))?,
        b: tone_channel(img, finite, |s| encode(s[2]))?,
    };
    let local_contrast = local_contrast_means(img, finite)?;

    let mut region_stats = <[RegionStats; 4]>::default();
    for ((out, acc), slot) in region_stats.iter_mut().zip(region_acc.iter_mut()).zip(&slots) {
        let share = acc.alpha / denom;
        if slot.is_none() || share < REGION_MIN_SHARE || acc.lum.is_empty() || acc.alpha <= 0.0 {
            continue;
        }
        acc.lum.sort_unstable_by(f32::total_cmp);
        acc.chroma.sort_unstable_by(f32::total_cmp);
        *out = RegionStats {
            present: 1.0,
            share: fin(share),
            lum: quantiles(&acc.lum, &REGION_PERMILLE),
            l: fin(acc.l / acc.alpha),
            a: fin(acc.a / acc.alpha),
            b: fin(acc.b / acc.alpha),
            chroma_p50: pick(&acc.chroma, 500),
        };
    }

    Ok(Measurements {
        schema: SCHEMA.to_string(),
        tone,
        clip: Clip { high: fin(clip_high as f64 / denom), low: fin(clip_low as f64 / denom) },
        chroma,
        hue,
        tint,
        neutral,
        grid,
        radial,
        local_contrast,
        regions: region_stats,
    })
}
