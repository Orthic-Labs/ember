# Camera colour calibration from a chart

Ember's own measured camera colour (gap #1, LR-PROF-CAMERACOLOR, issue #57). Last updated 2026-10-17.

## How a raw gets its colour

1. A DNG uses the colour tags it carries: `ColorMatrix1/2`, `ForwardMatrix1/2`, `CameraCalibration1/2`,
   `AnalogBalance`, `ProfileHueSatMap1/2`, `ProfileLookTable`, `ProfileToneCurve`, interpolated per DNG spec chapter 6
   (`crates/raw/src/color.rs`, `crates/raw/src/profile.rs`): the matrices by inverse correlated colour temperature of
   the as-shot white between the two calibration illuminants (clamped outside them), the white found by the spec's
   iteration from `AsShotNeutral`; a DNG with one matrix pair uses it for every white.
2. A non-DNG raw whose model has a **chart profile** (`<model>_chart.json`) starts from it
   (`camera_preview::starting_colour`, first): the profile's matrices are written into the raw's colour tags and the
   same DNG model renders them.
3. Otherwise the existing estimates: a look fitted to the camera JPEG; then, for an Olympus ORF, the colour matrix
   the file itself carries (below); then spectral matrices, the neutral fallback (`docs/camera-preview-colour.md`).

## The matrix an Olympus ORF carries

Olympus bodies write their own 3 x 3 colour matrix into the maker note: nine 16-bit words, signed 8.8 fixed point
(two's complement, 256 = 1.0), in ImageProcessing `0x0200` (XZ-2, E-M1, E-M10 Mark III) or in the main note `0x1011`
(E-1, E-400). Ember reads it from the user's own file at run time (`lightcraft_raw::maker_colour`,
`crates/raw/src/vendor/orf.rs`); nothing from a manufacturer profile is shipped. It was found by analysing the CC0
raw.pixls.us files for the E-1, E-400, XZ-2, E-M1 and E-M10 Mark III (analysis scripts outside the repository), not
from any GPL source:

- Every row sums to 1.00 (0.96, 0.97 and 0.98 on the E-1), so the matrix takes camera RGB after the as-shot red and
  blue gains (`ImageProcessing 0x0100`, main note `0x1017`/`0x1018`) to a colour space with the same white: white to
  white.
- On the three uncompressed files, the decoded mosaic with the as-shot gains, taken through the matrix, is closer to
  the chromaticities of the embedded JPEG than camera RGB read as linear sRGB (mean abs error of the r/sum and b/sum
  chromaticities over mid-tone cells: XZ-2 0.037 to 0.023, E-1 0.045 to 0.030, E-400 0.031 to 0.022); the transpose is
  far worse (0.084, 0.124, 0.117). So it is camera to sRGB-like primaries, not the other way. A fit of a free matrix to
  the JPEG is more saturated still (the JPEG has its own contrast and saturation), as expected.
- That the target is sRGB primaries with a D65 white is an inference from those two checks (rows summing to 1, the
  direction), not something a source states.

It becomes DNG tags (`ForwardMatrix` = Bradford(D65 to D50) * sRGB-to-XYZ * M with rows normalised to 1, `ColorMatrix` =
the inverse pairing for the as-shot neutral at D65, as `calibrate::fit_chart` does), so the as-shot white renders
through exactly M. The file states no light for it, so the Kelvin scale is the approximate one of one matrix at D65.
It is a step 4a in `camera_preview::starting_colour`: a photo whose JPEG fit is accepted keeps that look (colour and
tone), and the matrix replaces the spectral matrices and the neutral fallback when the fit fails or the file has no
usable JPEG. Compressed ORFs (E-M1, E-M10 Mark III) carry the same tag but are not decoded yet.

## Calibrating a camera

Shoot a 24-patch ColorChecker Classic in even light, unclipped, in raw (no lens correction needed; it is ignored).
Then run `camera.calibrate {id, region | corners, edition?, kelvin?, tint?, save?, force?}` (control channel, CLI,
MCP):

- `region` `[x, y, w, h]` is the upright chart's patch grid; `corners` are its four outer corners
  (patch 1 top left, patch 6 top right, patch 24 bottom right, patch 19 bottom left) for a rotated or upside-down
  chart. Fractions 0 to 1 of the developed, oriented picture.
- `edition`: `after-2014` (default; current Calibrite / X-Rite charts) or `before-2014`.
- `kelvin` (default 5000) and `tint`: the chart light. The `ForwardMatrix` does not depend on it; it only anchors the
  `ColorMatrix` and so the Kelvin scale.
- Each patch is the mean of a window half a patch wide at its centre; clipped patches, non-uniform windows and a grey
  row that does not darken from white to black are refused.
- The fit: camera neutral from the grey row; `ForwardMatrix` by least squares over the 24 patches with camera white
  constrained to D50 white; `ColorMatrix` = inverse of `Bradford(D50 to light) * ForwardMatrix * diag(1 / neutral)`.
  A mean CIE76 dE over 8 needs `force`.
- It saves `<config>/camera-profiles/<model>_chart.json` (`LIGHTCRAFT_CAMERA_PROFILES` overrides the folder) and
  applies to raws of that model opened afterwards.

## Reference data

CIE L*a*b* (D50, 2 degrees) values X-Rite publishes for the ColorChecker Classic, both printings, as tabulated by
BabelColor (Danny Pascale, "The ColorChecker (since 1976!)", babelcolor.com/main_level/ColorChecker.htm) and X-Rite,
"New color specifications for ColorChecker SG and Classic Charts" (2016). Checked against the tables in the BSD-3
`colour-science` project (`colour/characterisation/datasets/colour_checkers/chromaticity_coordinates.py`, fetched
2026-10-17). No proprietary profile, `.dcp` or GPL matrix table is used.

## Status

Code and synthetic tests only (`crates/raw/src/calibrate.rs`, `crates/engine/src/camera_calibration.rs`): a known
camera is recovered from a rendered chart. No real chart photo has been fitted, so no camera model counts as
calibrated, and per-model verification against real files remains.

Known gaps: the Olympus matrix is not measured against a chart or a reference render, only against the camera JPEG's
chromaticities; the white-balance scale of a calibrated model is still the relative one (the catalog decides it from
format and model before profiles are read); a hue/saturation table is not fitted (matrix only); no UI yet.

## DNG 1.7 three-illuminant tags: declared, not read (parked)

`CalibrationIlluminant3`, `CameraCalibration3`, `ColorMatrix3`, `ForwardMatrix3` and `IlluminantData1..3` have tag
numbers in `crates/tiff/src/tags.rs` but nothing reads them: `ColorData` holds two illuminants, so a DNG with a third
calibration set would use only the first two. Implementing them needs the DNG 1.7 specification text (how the
interpolation chooses among three illuminants, how `IlluminantData` gives a custom illuminant's xy, and the tag number
and layout of the third hue/saturation table), which the Adobe PDF (HTTP 403 from this workspace) and the older
DNG SDK header (two illuminants only) do not give. Not guessed; revisit with the spec in hand.
