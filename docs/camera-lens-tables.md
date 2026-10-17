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
| Sony | `VignettingCorrParams` int16s[16], `ChromaticAberrationCorrParams` int16s[32] in the enciphered Tag2010 / Tag9405 families | **Skipped.** The ExifTool tag-name tables give the names, offsets and element counts only; the meaning of the 16 and 32 values (units, radius normalisation, channel order) is not documented, and this checkout has no corpus ARW to derive it from. | [ExifTool Sony tag names](https://exiftool.org/TagNames/Sony.html) |
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
