# Auto decision model (plan)

Status: planned, not started. Builds on [`auto-tone.md`](auto-tone.md) (deterministic Auto, revision v3,
region masks) & [`personal-auto-evaluation.md`](personal-auto-evaluation.md) (shoot-disjoint protocol).
Off by default; absent model → bit-identical to deterministic Auto.

## What it decides

Auto has two halves. **Decision**: what this photo should become (subject brighter, sky held, blacks deeper,
how strongly). **Execution**: slider & mask values that get there. Execution is `auto::fit::fit_with`
(`crates/pipeline/src/auto.rs`): closed-loop optimizer over the real tone map, reaching
`Targets { top, bottom, spread, median, clip, w }` per population. Region Auto (`auto/regions.rs`) runs it
per Subject / Sky / Background mask.

Decision is handwritten today: `auto_targets` maps a continuous `SceneKey { low, high, backlit }` to targets
through fixed constants. It cannot tell backlit portrait from intended silhouette from night street; each
needs its own rule, strength & interaction with every other rule.

Decision model replaces only that function. Same inputs, same outputs, learned from examples of edits
people preferred. Optimizer, masks, sliders, undo & receipts stay as they are.

## Inputs (features)

Everything is already measured on the 512 px proxy; nothing new to compute at inference:

- Global: scene EV percentiles (p1 / p5 / p25 / p50 / p75 / p95 / p99), chroma mean, `SceneKey`,
  neutral-render percentiles (`stats` under `tone_map(0,0,0)`), clipped-channel share.
- Per region (Subject, Sky, Background): coverage, median EV, spread, clip, chroma; subject-minus-background
  & sky-minus-subject EV gaps.
- Source: ISO, exposure bias, flash fired, focal length, noise model `(a, b)` from `pipeline::noise`.
- Optional: 384-D DINOv2-S embedding of the 224 px centre crop (`segment::dinov2`), ridge-shrunk; only if it
  beats the scalar model on held-out shoots.

About 35 scalars. Feature schema gets a version string & hash stored in the model file; a mismatch disables
the model (deterministic fallback, warning in `ui.inspect`).

## Outputs (targets)

Same `Targets` the optimizer already consumes, global & per region, plus region strength `w`. Model predicts
a **residual over `auto_targets`**, clamped per field (median ±0.25, top/bottom ±0.1, spread ±0.2, clip
±0.02, w ∈ [0, 3]):

```text
targets = clamp(auto_targets(scene) + model(features), bounds)
```

Zero residual = today's Auto exactly, so an absent, disabled or schema-mismatched model is bit-identical.
Residual form also keeps burst continuity: neighbouring frames share features, so they share residuals.

## Labels: measured output, never sliders

Label is **what the finished photo looks like**, measured with the same statistics as the features: percentiles,
region medians, spread & clip of the *rendered* edit over the *same masks*. Not the editor's slider values.

- Slider values are renderer-specific (process version, profile, tone curve) & the repo already treats mapped
  Lightroom settings as weak cold-start labels only (`personal-auto-evaluation.md` → Current contract).
- Rendered pixels transfer across renderers: "subject median landed at 0.52, sky top at 0.93" is true whoever
  rendered it, & `fit_with` can reach those numbers through Ember's own pipeline.

Measurement path: decode edited JPEG → sRGB → linear → `encode` → same percentile `stats`; masks are
computed on the raw proxy & transferred to the edited render through the edit's crop / orientation
(mapped from `crs:` fields). Edits whose geometry cannot be aligned (perspective, heavy transform, content-
aware) are dropped. Edits with local adjustments are kept: their effect is in the pixels, which is the point.

## Data: an edited Lightroom catalog

A personal catalog with raws, hand edits & exports is the right dataset. Rules:

1. **Hand-edited only.** Keep photos whose develop history (`Adobe_libraryImageDevelopHistoryStep`, read by
   `crates/engine/src/lightroom_catalog.rs`) has slider / mask steps after Auto, or no Auto step at all.
   Drop Auto-only photos: those encode Adobe's policy, not the person's, & distilling them is not clean-room.
   Drop sync / paste-settings-only copies & keep one of each virtual-copy family.
2. **Catalog is read only.** The importer already reads `.lrcat` / WAL without writing. Exports are the
   person's own action in their own app. Nothing from the app bundle is read.
3. **Nothing leaves the machine.** Feature / label manifests are numeric only (no paths, EXIF, editor
   fields), as `ai personal extract` already does. No media in the repo, ever.
4. **Shoot-disjoint splits** per `personal-auto-evaluation.md` → Dataset & split: folder + 10 s burst grouping,
   stratified by camera / lens / year, one held-out camera slice. Split manifest hash recorded.

Yield estimate for a 10 k catalog: roughly 40 % hand-edited → ~4 k samples over ~600 shoots. Ample for a
35-input ridge model; thin for the embedding variant without strong shrinkage.

## Model

In order; each stage must beat the previous on held-out shoots or it is not kept:

1. **Ridge regression**, closed form in f64, no seed, fixed row order. Few KB of JSON. First & probably final.
2. **Small MLP** (2 × 64, tanh), trained by deterministic full-batch L-BFGS in pure Rust, only if ridge
   under-fits the interactions (silhouette vs backlit is the test case).
3. **Scene classifier + per-class target tables** (the DINOv2 scene prior in
   [`local-ai-delivery.md`](local-ai-delivery.md)): classes blended by probability, never argmax. A special
   case of 1 with interpretable intermediate; worth it only if someone wants to inspect "why".

Not an LLM, not a fine-tuned vision model, not Unsloth. Those take text or images & emit tokens or class
picks; this takes ~35 numbers & emits ~20 numbers. Weights must be KBs, run on CPU in pure Rust, give the
same answer every run & vanish cleanly when absent. See *Common confusions* below.

Where a large model does help: **offline pseudo-labelling** of scene classes (backlit / silhouette / night /
snow…) on the CC0 corpus with a zero-shot vision-language model, reviewed by hand, then distilled into stage 3.
That runs once, on a workstation, outside the product.

## Work order

1. `lightcraft-cli ai decision manifest --lrcat CATALOG --raw-root DIR --export-root DIR --out M.json`:
   walks catalog read-only, applies the hand-edited filter, pairs raw ↔ export, assigns shoot ids, reports
   counts per drop reason.
2. `ai decision extract --manifest M.json --out NUMERIC.json`: features from the raw proxy, labels from the
   aligned export, masks shared; numeric output only.
3. `ai decision train --manifest NUMERIC.json --out MODEL.json`: ridge over train shoots; model file carries
   feature schema hash, `auto::REVISION`, clamps, split hash.
4. `ai decision evaluate --manifest NUMERIC.json --model MODEL.json --out REPORT.json`: held-out target error
   vs deterministic Auto per field & per region; per-camera slices; burst continuity (max residual jump
   inside a burst); repeat-identical check.
5. `auto::decision` module behind `DevelopSettings`-independent preference `auto.decision_model = PATH`;
   `develop.auto` & region Auto call `targets_with(model)`; receipts record model hash.
6. Gate: `personal-auto-evaluation.md` → Rollout gate (held-out preference against deterministic Auto,
   continuity on bursts). Until passed the preference stays unset & nothing ships.

Steps 1–4 are offline tooling & land first; 5 is small; 6 decides whether a default ever changes.

## Common confusions

- *Classifier or regressor?* Labels here are measurements (continuous), so regression. A classifier is the
  detour through named scenes; take it only for interpretability.
- *Can Unsloth train it?* No. Unsloth fine-tunes LLMs (Qwen / Gemma / Llama) with LoRA over text; its
  "decision model" is an LLM with a classification head choosing among text options. Wrong input type, wrong
  output type, GBs of weights, GPU & Python at inference. Ember's trainer is `lightcraft-cli`, in Rust.
- *Is deterministic the opposite of learned?* No. Learned weights are constants once trained; inference is a
  few dot products & gives the same answer every run.
- *Does it need my slider values?* No. It needs the raw & the finished picture. Crop / orientation are the
  only editor fields read, for alignment.
