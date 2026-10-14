# Dense measurement

A fixed-length, deterministic description of one image: where its tones sit, how colourful it is, which hues it holds, how colour casts through shadows and highlights, how brightness and colour are spread over the frame, how much local contrast it has, and what its subject / sky / background / skin regions look like. It is the input of the learned Auto model (`crates/pipeline/src/measure.rs`, `crates/pipeline/src/tests_measure.rs`).

Schema id: **`ember.dense-measure.v1`** (`measure::SCHEMA`). A model trained on one schema must not read another: add, remove, reorder or redefine any field and the id changes.

```rust
use lightcraft_pipeline::measure::{measure, Measurements, RegionKind};

let m = measure(&proxy, &[(RegionKind::Sky, &sky_alpha)])?;   // proxy: &Rgb32f, sky_alpha: &Plane
let features: Vec<f32> = m.to_vec();                          // 202 values, all finite
let names: Vec<String> = Measurements::names();               // same length and order
m.validate()?;                                                // schema + finiteness (hostile JSON)
```

## Input

- The image is **display-linear Rec. 2020** `Rgb32f`: what `look::extract_linear` reads, which is also the space of the corpus proxies and the 512 px import proxy. Any size up to 64 megapixels (`MAX_PIXELS`); the 512 px proxy is the intended input.
- A pixel with any non-finite channel is skipped by every field. Fewer than 16 finite pixels is an error. Finite values are limited to ±1e6 before use so nothing can overflow.
- Region planes are `Plane` (`Image<f32>`) alphas, 0..=1, the image's exact width and height. A non-finite alpha reads as 0. At most one plane per `RegionKind` (`Subject`, `Sky`, `Background`, `Skin`; serde lowercase).

## Conventions

- **Encoded** means gamma 2.2: `encode(v) = clamp(v, 0, 1)^(1/2.2)` (`auto::fit::encode`, the scale Auto's targets live on). Encoded luminance is `encode(luminance_2020(rgb))`; encoded R, G, B encode each channel.
- **Oklab** values are `oklab_from_2020` of the pixel with negative channels set to 0. Chroma is `hypot(a, b)`, hue is `atan2(b, a)` in radians.
- **Percentile p** of n sorted values is element `floor((n - 1) * p / 100)` (the rank rule of `auto.rs` and `look.rs`, computed in integers per mille so rounding cannot move a rank). An empty set reports 0.
- All sums are f64, taken in pixel order on one thread; sorts use `f32::total_cmp`. The same input gives bit-identical output.
- Everything is in normalized coordinates or relative radii, so a 256 px and a 512 px version of one image measure nearly the same.
- "Zeros" below means the field reports 0 and the group still occupies its slots, so the layout never changes.

## Fields

202 values in this order. Names are dotted and stable; `Measurements::names()` returns them.

| # | Group | Count | Names | Meaning |
| --- | --- | --- | --- | --- |
| 1 | Tone | 68 | `tone.{lum,r,g,b}.{p0_5,p1,p2,p5,p10,p20,p30,p40,p50,p60,p70,p80,p90,p95,p98,p99,p99_5}` | encoded luminance, then encoded R, G, B: percentiles 0.5, 1, 2, 5, 10, 20, 30, 40, 50, 60, 70, 80, 90, 95, 98, 99, 99.5 over all finite pixels |
| 2 | Clipping | 2 | `clip.high`, `clip.low` | share of pixels with display-linear luminance >= 0.985 (the definition `look.rs` uses; encoded, that is >= 0.993, not >= 1.0); share with encoded luminance <= 1/255 |
| 3 | Chroma | 5 | `chroma.{p10,p25,p50,p75,p90}` | Oklab chroma percentiles 10, 25, 50, 75, 90 |
| 4 | Hue bands | 32 | `hue.{red,orange,yellow,green,aqua,blue,purple,magenta}.{share,offset,chroma,lum}` | see below |
| 5 | Tonal tint | 9 | `tint.{shadows,midtones,highlights}.{share,a,b}` | by Oklab L: shadows L < 0.4, midtones 0.4 ..= 0.7, highlights L > 0.7. `share` is the share of finite pixels in the range; `a`, `b` are the mean Oklab a, b over it (0 when empty) |
| 6 | Neutral cast | 2 | `neutral.a`, `neutral.b` | mean Oklab a, b over pixels with 0.3 <= L <= 0.85 and chroma < 0.08 (0 when none) |
| 7 | Spatial grid | 32 | `grid.lum.r{0..3}c{0..3}`, then `grid.chroma.r{0..3}c{0..3}` | 4 x 4 cells, row-major from the top-left, cell = `floor(x * 4 / w)`, `floor(y * 4 / h)`: mean encoded luminance (16), then mean Oklab chroma (16). An empty cell (images under 4 px) is 0 |
| 8 | Radial | 5 | `radial.ring{0..4}` | mean encoded luminance in rings of pixel-centre distance from the frame centre over the half diagonal: [0, .2), [.2, .4), [.4, .6), [.6, .8), [.8, 1] |
| 9 | Local contrast | 3 | `local_contrast.{r1pct,r4pct,r16pct}` | mean of `abs(v - boxblur(v))` for `v = log2(max(luminance, 1e-6))`, box radii of 1 %, 4 %, 16 % of the long edge (at least 1 px) |
| 10 | Regions | 44 | `region.{subject,sky,background,skin}.{present,share,lum.p5,lum.p25,lum.p50,lum.p75,lum.p95,oklab.l,oklab.a,oklab.b,chroma.p50}` | see below |

Total: 68 + 2 + 5 + 32 + 9 + 2 + 32 + 5 + 3 + 44 = **202**.

### Hue bands

The eight band centres are computed in code, not tabulated: the Oklch hue of each reference colour, given as linear sRGB and converted to Rec. 2020 then Oklab (`hue_band_centres()`): red (1, 0, 0), orange (1, 0.5, 0), yellow (1, 1, 0), green (0, 1, 0), aqua (0, 1, 1), blue (0, 0, 1), purple (0.5, 0, 1), magenta (1, 0, 1). Sorted by hue, they split the hue circle into eight gaps of unequal width.

Only pixels with Oklab chroma >= 0.02 count. A pixel's hue falls between two neighbouring centres (circularly) and splits linearly between them: weight to the upper centre `t = (hue - lower) / (upper - lower)`, to the lower `1 - t`; the weights sum to 1. Per band:

- `share`: summed weight over the count of **all** finite pixels (so the eight shares sum to the share of coloured pixels, not to 1).
- `offset`: weighted circular mean of (pixel hue - band centre), radians, `atan2(sum w sin d, sum w cos d)`. Positive means towards larger Oklch hue. Within one gap it is bounded by the gap.
- `chroma`, `lum`: weighted mean Oklab chroma and Oklab L.

A band with summed weight under 1e-6 reports zeros (share 0 as well).

### Regions

- `present`: 1 when the plane was supplied, covers at least 0.5 % of the frame and holds at least one pixel with alpha >= 0.5; else 0 and the other ten values are zeros. (The last condition is stricter than the share alone: a faint mask with no pixel at alpha >= 0.5 has no percentile set.)
- `share`: mean alpha over the finite pixels.
- `lum.p5 ... p95`: encoded luminance percentiles 5, 25, 50, 75, 95 over pixels with alpha >= 0.5.
- `oklab.l`, `oklab.a`, `oklab.b`: alpha-weighted mean Oklab L, a, b over every pixel with alpha > 0.
- `chroma.p50`: Oklab chroma median over pixels with alpha >= 0.5.

## Local contrast details

Box means come from a summed-area table in f64 (exact, fixed order). The box is clipped at the frame edge: the mean is over the pixels inside it. A radius is a real number of pixels (share x long edge, at least 1); the boxes at the two neighbouring integer radii are blended by the fraction so the radius scales smoothly with resolution. A non-finite pixel is stood in for by the frame's mean log luminance inside the table and left out of the average. Scratch is one f64 per pixel (about 0.5 GB at the 64 MP cap, 2 MB at 512 px); allocation failure is an error, not an abort.

## Errors

`MeasureError`: `EmptyImage` (zero width or height), `MalformedImage` (buffer length differs from width x height), `TooLarge { pixels }` (over 64 MP), `TooFewPixels { finite }` (under 16), `RegionSizeMismatch(kind)`, `DuplicateRegion(kind)`, `OutOfMemory`.

`Measurements::validate()` rejects a wrong `schema` or any non-finite value (a deserialized struct is checked; unknown JSON fields are refused). `to_vec()` reads a non-finite value of a hand-built struct as 0.

## Resolution independence

`tests_measure.rs` measures one smooth procedural scene (with a soft elliptical region) at 256 x 192 and 512 x 384 and requires every value to agree within a per-group tolerance. Hue-band and tint statistics other than `share` are compared only when both sides hold at least 3 % of the frame in that band or range.

| Group | Tolerance |
| --- | --- |
| tone percentiles | 0.03 |
| clipping, chroma percentiles | 0.01 |
| hue share / offset (rad) / chroma / lum | 0.015 / 0.05 / 0.01 / 0.02 |
| tint share / a, b; neutral cast | 0.015 / 0.01; 0.01 |
| grid lum / grid chroma | 0.02 / 0.01 |
| radial rings | 0.02 |
| local contrast | 0.015 |
| region present / share / oklab / chroma / lum percentiles | exact / 0.01 / 0.01 / 0.01 / 0.03 |
