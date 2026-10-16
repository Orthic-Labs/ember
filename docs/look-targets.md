# Look targets

A preset copies slider values; a look target copies the *result*. Feed Ember finished sample photos (your exports, any camera) & it extracts the statistics they have in common; apply the target to another photo & Auto refits that photo's own eight values until it lands on the same statistics. Every photo gets its own exposure, contrast, highlights, shadows, whites, blacks & vibrance, yet the batch shares one histogram shape. Deterministic & resolution-independent (`crates/pipeline/src/look.rs`).

## Extract

```text
lightcraft-cli look extract sample1.jpg sample2.jpg sample3.jpg -o look.json
```

Samples are rendered files (JPEG / PNG / TIFF…; raws are refused: a raw has no look yet). Each sample is decoded to display-linear Rec. 2020 (at most 1024 px) & measured:

| Field | Meaning |
| --- | --- |
| `luminance` | encoded (gamma 2.2) output luminance at the 1st, 5th, 50th, 95th & 99.5th percentiles |
| `clip` | share of the frame at or over white |
| `chroma` | Oklab chroma at the 50th & 90th percentiles |

Each field is the **median across samples**, so one odd frame does not pull the look. Five to twenty samples of one style work well; a single sample works (`samples: 1`). The file is schema `lightcraft.look-target.v1` & refuses to overwrite without `--force`. `LookTarget::validate` rejects anything else (wrong schema, non-finite, out of range, non-ascending percentiles, unknown fields).

## Apply

```text
lightcraft-cli run --import photo.raf develop.applyLook path=look.json app.export path=out.jpg longEdge=2000
```

`develop.applyLook` (engine command: UI, CLI, MCP & control channel; params `look` inline or `path`, `dryRun`) does, on the photo's 512 px proxy under its current white balance, profile & crop:

1. **Scene population**: the same scene EVs & scene key Auto measures (`auto::scene_population`, `SceneKey`).
2. **Shaping fit**: `auto::fit::fit_with` with the look's statistics as [`Targets`](auto-tone.md) (top = p99.5, bottom = p1, spread = p95 − p5, median = p50, clip). The key weights still soften the contrast term so a night frame is not forced to a daylight spread.
3. **Exposure against the real render**: the proxy is rendered through the actual pipeline (float linear Rec. 2020) & exposure bisected (10 steps, ±2 EV around the tone-map estimate) until the rendered median meets the look's. This is a true closed loop: local stages the fit only models (the edge-aware highlight/shadow base) are in the render.
4. **Vibrance** from the rendered 90th-percentile chroma vs the look's: `40 × log2(want / have)`, clamped ±40.

One undo step. `dryRun: true` returns the values without editing.

**Batch**: `ids: [photoId, …]` (at most 512, duplicates collapse) applies the look to each photo, each refit from its own source & settings, as **one undo step**. The reply is `{dryRun, applied, photos: [{id, values}], errors: [{id, error}]}`: a photo that cannot take the look (unknown id, unreadable source) is listed in `errors` & the others still apply. An empty or oversized list, or a non-integer id, is refused before any edit. A batch holds an activity-stack task (`look`, not cancellable). Without `ids` the active photo is used, as before. Regressions (`look::tests`): a sample rendered a stop brighter & flatter, applied to a frame 1.5 stops darker, lands within 0.03 of the look's median & 0.08 of its highlight percentile; a muted sample pulls vibrance down; hostile targets are refused before any edit; repeat-stable.

## Spatial & face weighting

Extraction also measures a 3 × 3 **grid** over the normalized frame (row-major from the top-left): per cell the encoded luminance median & the Oklab chroma median, each the median across samples (`grid`, `lightcraft.look-target.v1`, schema unchanged). Cells are defined by normalized position, so the same scene at different sizes gives the same grid within a small tolerance (regression: 96 px & 192 px). Targets written before the grid existed have no `grid` & load unchanged.

The grid is used only when the target's `weighting` asks for it (default `{"spatial": false, "faces": false}`, so every existing target & every target you have not edited keeps the frame-wide match, bit for bit). To enable it, edit the target file:

```json
"weighting": {"spatial": true, "faces": true}
```

- `spatial`: the match aims at the mean of the nine cell medians, instead of the frame-wide median.
- `faces`: with `spatial`, each cell is weighted by `1 + 3 × share`, where `share` is how much of that cell the photo's own face boxes cover. A portrait's skin tones therefore pull the fit toward the look's tone where the faces sit. The face boxes are the photo's cached Detect Faces regions (`develop.applyLook` passes them through `apply_with`). With no usable face (under 0.5 % of the frame) the weights are the plain cells' and the fit is the spatial mean.

Face boxes are used only for the match, not for the photo's exposure key. A look with `faces` on but a photo without faces matches exactly as `spatial` alone.

## Limits & next

- Tone & colourfulness only: no hue shifts, split toning, curves or grain are transferred (a preset does those; a look target & a preset combine: apply the preset, then the target).
- The grid is a fixed 3 × 3. Sky & subject masks (region-aware Auto, [#177](https://github.com/Orthic-Labs/ember/issues/177)) are not used for the match yet, so a sky region is weighted only by where it falls in the grid. Acceptance against an accepted reference set of finished shoots is still open for [#203](https://github.com/Orthic-Labs/ember/issues/203).
- Batch: `develop.applyLook` with `ids` is implemented (see Apply). [#202](https://github.com/Orthic-Labs/ember/issues/202)
- Personal style from accepted edits (`personal-auto-evaluation.md`) is the learned counterpart: a look target needs no training & five samples; a residual model needs hundreds of accepted edits & qualifies against held-out shoots.
