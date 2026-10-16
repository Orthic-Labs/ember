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
3. Otherwise the existing estimates: a look fitted to the camera JPEG, spectral matrices, the neutral fallback
   (`docs/camera-preview-colour.md`).

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

Known gaps: the white-balance scale of a calibrated model is still the relative one (the catalog decides it from
format and model before profiles are read); a hue/saturation table is not fitted (matrix only); no UI yet.
