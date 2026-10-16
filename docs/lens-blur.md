# Lens Blur

Last updated: 2026-10-16. Issue [#54](https://github.com/Orthic-Labs/ember/issues/54), parity row
`LR-EDIT-LENSBLUR`. Code: `crates/pipeline/src/lens_blur.rs`; settings: `LensBlur` in
`crates/develop/src/settings.rs`; controls `lensBlur.*` (`lightcraft-cli controls`).

## Controls

All 0..100, default Amount 0 (off), so existing edits render as before.

| Control | Default | Meaning |
|---|---:|---|
| `lensBlur.amount` | 0 | Blur radius at full distance from the focal band: 2 % of the long edge at 100 |
| `lensBlur.focalNear` | 0 | Near edge of the band of depth that stays sharp |
| `lensBlur.focalFar` | 40 | Far edge of that band |
| `lensBlur.shape` | 0 | 0 = round discs; towards 100, cat's-eye (lemon) discs squeezed along the radius, more at the frame's edges |
| `lensBlur.highlightBoost` | 30 | Extra weight of bright pixels in the blur, so highlights bloom into bright discs |

`lens_blur.enabled` (optional) switches the effect off with `false`; the Lens Blur section's eye
(`disabled_sections` entry `lensBlur`) does too.

## Depth proxy

Ember has no depth model and ships no weights for one. Depth is a two-layer proxy from the Subject
matte (the classical saliency segmenter, a DNG person matte does not apply): the subject is at depth
25, the background at 100, and the matte's soft edge is the transition. The default band (0..40)
keeps the subject sharp and blurs the background; moving `focalNear` above 25 blurs the subject too
(near-field blur: the subject blurs, but its blur does not spread over the sharp background).

The result is only as good as the Subject matte: a frame with no clear subject falls back to a weak
centre ellipse, and the blur then fades towards the frame's edges. A real depth map (and the depth
range mask, #64) replaces `coc_plane` only; the rest of the render is depth-source independent.

## Render

- Runs in scene-linear light after white balance, spots and red eye, before noise reduction
  (`lin_cpu`), so the preview, the export and the GPU path (which falls back to this CPU stage) agree.
- A gather over a Vogel disc (12, 24 or 48 taps by radius) from a box-filtered pyramid, so a large disc
  needs few taps without aliasing. Pixels whose radius is under half a pixel are copied; the cost
  is not measured yet.
- Radius is a share of the output's long edge and the Subject decision is made once on a fixed-size
  proxy of the whole frame, so two output sizes of one photo blur alike (test
  `the_same_picture_at_two_sizes_blurs_alike`). A windowed render grows its window by the blur radius.
- A tap contributes only when its own circle of confusion reaches the pixel being drawn, so the sharp
  subject never smears into the background. Pyramid levels above 0 average across the subject's edge,
  which leaves a faint halo about a third of a radius wide.
- Highlight Boost weights each pixel by `1 + boost × smoothstep(0.6, 2.0, luminance)` in the average.

## Not done

- A panel (the controls are reachable through the control channel, MCP and `lightcraft-cli` only).
- A depth-map source for the blur (the photo's embedded depth plane, loaded for the depth range mask of #64, is not used by it yet), and a GPU kernel.
- Bokeh shapes other than the disc and cat's eye (aperture blades, custom shapes).
