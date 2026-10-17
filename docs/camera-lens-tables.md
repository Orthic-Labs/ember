# Camera maker-note vignetting and lateral CA tables

Issue #242. Cameras write their own lens-correction tables into the file. Ember reads the ones whose layout is documented
into one maker-neutral model and applies them under an opt-in develop setting, **Use camera lens corrections**
(`optics.camera_corrections`, off by default, not written while off, so existing settings and their hashes are
unchanged). It supplies only what the lens profile (embedded or database) does not already correct, so nothing is applied
twice. Without a table, with the setting off, or on a body that was not validated, nothing changes (bit-identical).

## Model

`lightcraft_geom::CameraCorrection` (`crates/geom/src/camera_lens.rs`), stored on the photo's `EmbeddedLens::camera`:

- `vignette`: a `RadialTable` of **relative illumination** (1 on the axis) against the radius on the sensor. The correction
  multiplies by its reciprocal (bounded to 0.25 to 20 times, so a corrupt table cannot blow a corner out).
- `ca`: two `RadialTable`s for the red and blue planes: a point at radius `r` in green lies at `r · (1 + s(r))` in that
  plane, `s` the table's value. The correction samples the plane there.
- A `RadialTable` is up to 16 knots, strictly increasing, linear between knots and flat outside. Radii are in half
  diagonals of the recorded frame (0 on the axis, 1 at the corners), so previews, exports and every orientation use the
  same table (resolution independence).

`lightcraft_raw::camera_correction(bytes)` reads it; `lightcraft_engine::lens_profiles::attach_camera` stores it. The
pipeline (`crates/pipeline/src/optics.rs`, `Warp::cam_vig`, `Warp::cam_ca`) applies it on the CPU (the GPU kernels have no
table lookup, so `Warp::needs_f64` sends the geometry step to the CPU while it is in use). The Optics panel shows the
checkbox for photos whose file carries tables.

## Makers

| Maker | Tables | Status | Source |
|---|---|---|---|
| Fujifilm | `0xf010` VignettingParams, `0xf00f` ChromaticAberrationParams (raw IFD) | **Vignetting read and applied** for the 59 validated bodies (`crates/raw/src/vendor/raf_lens.rs`). CA table decoded (`camera_ca`) and tested, **not applied**: see below. | tag names: [ExifTool FujiFilm tag names](https://exiftool.org/TagNames/FujiFilm.html); layout, units and scale: our own analysis of CC0 RAFs, [fuji-lens-corrections.md](fuji-lens-corrections.md) |
| Sony | `VignettingCorrParams` int16s[16], `ChromaticAberrationCorrParams` int16s[32] in the enciphered `Tag9405b` (A7 III) and `Tag9416` (A7 IV) blocks | **Located and read out, not applied.** The tables are found and look right (monotone, zero on the axis), but two bodies and five photographs without a flat field could not fix the units, the radius spacing or the channel order: see [Sony analysis](#sony-analysis-242). | [ExifTool Sony tag names](https://exiftool.org/TagNames/Sony.html); layout and attempted decoding: our own analysis of real ARWs |
| Panasonic | `0x008a` ShadingCompensation only | **Skipped.** The maker note holds an On/Off flag, no table. The RW2 distortion table (`0x0119`) is already applied as a lens warp. | [ExifTool Panasonic tag names](https://exiftool.org/TagNames/Panasonic.html) |
| Olympus / OM | `0x050c` / `0x1012` ShadingCompensation (On/Off), `0x0206` LensDistortionParams int16s[6] | **Skipped.** Shading is an On/Off flag, no table; the only table is distortion, and no CA data is documented. | [ExifTool Olympus tag names](https://exiftool.org/TagNames/Olympus.html) |

No code from darktable, RawTherapee, ART, LibRaw, rawspeed, lensfun, dcraw or ExifTool was read or ported. ExifTool's
published HTML tag tables were read as documentation of tag ids, names and counts.

## Why Fujifilm's CA is not applied

On the raw mosaics the table's sign and shape are right (median correlation 0.93 over 46 curves, at about 0.7 of its
amplitude), but applied to the planes after Ember's demosaic it moved them the wrong way (median measured shift 0.10 to
0.13 px, more than 1.5 times worse on 11 of 28 curves: X-T4 blue 0.16 to 0.58 px). A correction before demosaicing would
be needed, so `APPLY_CA` in `raf_lens.rs` is `false` and only the vignetting table leaves the reader. The application
path for CA tables is implemented and tested in the pipeline (a synthetic CA source is aligned to under 35 % of its
fringing) for the day a maker's table is verified.

## Sony analysis (#242)

Five ARWs of Adrian's own, from two bodies, were read (never committed). All have vignetting and lateral CA correction
set to Auto in the maker note (`0x2011`, `0x2012`; distortion `0x2013` is Auto on the A7 IV and Off on the A7 III).

| File | Body | Lens | Focal length | Aperture | Raw |
|---|---|---|---|---|---|
| DSC05009 | ILCE-7M3 | FE 90mm F2.8 Macro G OSS | 90 mm | f/2.8 | uncompressed 6048x4024 |
| DSC05025 | ILCE-7M3 | FE 90mm F2.8 Macro G OSS | 90 mm | f/2.8 | uncompressed 6048x4024 |
| 2023-ADR09594 | ILCE-7M4 | Samyang AF 35-150mm F2-2.8 | 111 mm | f/2.8 | lossless compressed 5120x3584 (4608x3072 active) |
| 2025-SST04202 | ILCE-7M4 | Samyang AF 35-150mm F2-2.8 | 63 mm | f/2.8 | uncompressed 7040x4688 |
| 2026-SST04579 | ILCE-7M4 | Samyang AF 35-150mm F2-2.8 | 140 mm | f/2.8 | uncompressed 7040x4688 |

The 2025 and 2026 frames are also 16:9 crops of the sensor (`0x74c7`/`0x74c8`: 7008x3944 at 12,372).

### Where the tables are

The Sony maker note in these files is a bare IFD (no `SONY DSC` header, offsets from the start of the file). The blocks
are enciphered with the substitution already used in `arw.rs` (`p^3 mod 249`). Deciphered, these places hold plausible
tables in the files; the other offsets ExifTool lists for the same tables are alternatives for other bodies and give
shifted copies or neighbouring data here:

| Body | Block | Vignetting (int16s[16]) | Chromatic aberration (int16s[32]) | Distortion (int16s[16]) |
|---|---|---|---|---|
| ILCE-7M3 | `0x9405` (2300 bytes) | byte 860 | byte 952 | byte 100 |
| ILCE-7M4 | `0x9416` (4416 bytes) | byte 2193 | byte 2326 | byte 79 |

Vignetting, one table per file (knot 0 is always 0, then strictly rising, convex like r^2):

| File | Knots 0 to 15 |
|---|---|
| DSC05009 | 0, 96, 288, 576, 960, 1440, 1984, 2656, 3424, 4256, 5184, 6176, 7200, 8288, 9376, 10496 |
| DSC05025 | 0, 128, 384, 768, 1216, 1728, 2304, 3040, 3840, 4736, 5664, 6656, 7648, 8672, 9728, 10784 |
| 2023-ADR09594 | 0, 160, 384, 640, 928, 1248, 1568, 1856, 2240, 2848, 3680, 4736, 5920, 7232, 8576, 9952 |
| 2025-SST04202 | 0, 32, 192, 448, 800, 1248, 1728, 2144, 2560, 2976, 3520, 4256, 5312, 6752, 8640, 10496 |
| 2026-SST04579 | 0, 192, 352, 576, 896, 1376, 2048, 2912, 3904, 4928, 6016, 7104, 8192, 9280, 10368, 11488 |

CA is a smooth 16-value curve followed by another. DSC05025: -300 rising to +29, then 718 falling to -84. DSC05009: -206
to -24, then 708 to -76. On the A7 IV, 2025-SST04202 reads 550 to 284, then -722 to -152. Distortion tables are in the
size range of the `/16384` scale used for the raw IFD table in [sony-lens-corrections.md](sony-lens-corrections.md)
(DSC05009: -1 to -144, about -0.9 % at the corner; A7 IV: 0 to 418 or more, about 2.5 to 3 %); they are not used here.

### What the measurements could and could not settle

**Vignetting.** The embedded 1616x1080 / 1920x1080 previews are not corrected (green plane against the raw green
plane, one radius-independent mapping fits DSC05025 to 0.003 log units), so they are no reference. With no flat field,
the falloff was estimated from the scene: the radial derivative of log green (half resolution, 8x8 binned, black 512
removed, clipped and dark pixels dropped), averaged over smooth pixels only (gradient below 0.02 or 0.05 log per binned
pixel) and integrated from the axis. Log gain at the corner:

| File | gate 0.05 | gate 0.02 | 1 + v/16384 | 2^(v/16384) | 1 + v/32768 |
|---|---|---|---|---|---|
| DSC05009 | 0.40 | 0.10 | 0.50 | 0.44 | 0.28 |
| DSC05025 | 0.43 | 0.32 | 0.51 | 0.46 | 0.29 |
| 2025-SST04202 | 0.14 | 0.15 | 0.50 | 0.44 | 0.28 |
| 2026-SST04579 | -0.69 | -0.08 | 0.57 | 0.50 | 0.31 |

The two A7 III frames (same lens and setting) agree at gate 0.05 (0.40 and 0.43; tables 10496 and 10784) and sit
between the 1/32768 and 1/16384 readings. The A7 IV frames are dominated by bokeh and a dark concert scene and say
nothing (0.14 to -0.69 depending on the gate). The gate moves the answer by 0.1 to 0.3 log units, as much as the gap
between the candidate scales. The 2023 compressed frame was not decoded. So the tables behave as a vignetting excess
that grows like r^2 (zero on the axis, larger for the macro lens), but **the unit (the 16384 of distortion, a stops
scale or another) and whether the 16 knots span the half diagonal are not established**. The spread across the files is
far from "flat within a few percent".

**Lateral CA.** Red and blue against green on the raw mosaic planes (half resolution, 32x32 patches, a local gain per
patch, a translation and a radial scale per ring, 16 rings, pooled gradients; standard errors 1e-5 or less).
DSC05025: blue +0.0003 to +0.00038 at r 0.1 to 0.4, falling to +0.00007 at the corner; red -0.0001 to -0.0002, to
-0.00006. DSC05009: blue +0.0004 to +0.0003, red -0.0004 to -0.0003. Against the tables, the second half matches blue
at about 4.8e-7 per unit (2^-21): DSC05025 predicts +0.00034 at the axis end, measured +0.0003 to +0.00038; DSC05009
predicts +0.00034, measured +0.0003 to +0.0004. The first half matches red for DSC05025 (-300 predicts -0.00014,
measured -0.0001 to -0.0002) but not for DSC05009 (-206 predicts -0.0001, measured -0.0004, four times too large).
The measured curves also fall with radius more slowly than the tables do over knots spanning the half diagonal (blue
halves by r 0.55, the table by knot 4, r 0.27). The A7 IV frames gave unusable estimates (red and blue swing by 1e-4
to 6e-3 between rings on the dark, bokeh-heavy scenes). The per-file disagreement is as large as the signal, so
neither the unit, the radius spacing nor the channel order is confirmed.

### Decision

Not convincing, so Sony vignetting and CA stay **unapplied**: `camera_correction` still returns nothing for ARW and no
reader or model change was made. A wrong scale would brighten or shift corners incorrectly while compiling and
passing, which is worse than no correction. What would settle it: a flat-field (or defocused sky) frame at several
apertures and focal lengths per lens on each body, which fixes the unit by measuring the corner directly against the
table, and an edge-chart series for CA. With those, the reader is a small addition beside the raw-IFD distortion
reader in `arw_lens.rs`: decipher `0x9405` or `0x9416`, take the offsets above for the body, build a `RadialTable`
over 0 to 1.

## Tests

- `crates/geom/src/camera_lens.rs`: interpolation, flat ends, malformed tables rejected.
- `crates/raw/src/vendor/raf_lens.rs`: a synthetic Fujifilm vignetting table becomes the expected knots; the CA table
  layout decodes to the expected per-channel tables; malformed tables (scale, length, order, range, flat, wrong type,
  empty) are ignored; only validated bodies.
- `crates/raw/src/vendor/raf.rs`: a synthetic RAF through header, crop records and raw IFD; garbage and truncated input.
- `crates/pipeline/src/tests_geometry.rs`: a synthetic vignette is flattened to within two 8-bit levels; off is
  bit-identical (also with the lens profile on); a synthetic CA source is aligned; camera tables yield to what the lens
  profile corrects.
- `crates/develop/src/lib.rs`: off by default, not written, backward compatible; the lens's `camera` field likewise.

Written without a compiler and unverified until CI runs.
