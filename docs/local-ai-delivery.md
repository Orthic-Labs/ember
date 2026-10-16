# Local AI delivery

Status, 2026-10-10: generated CI built source revision 93678dd1b9977d6ee509679be164fe2ec3b44113 on macOS/arm64 & Windows/x86_64; hidden native QA failed, while CPU synthetic denoise receipts were retained as always `UNQUALIFIED`. Follow-up source awaits full generated gates & exact-artifact qualification. No learned-model accuracy, photographic quality, native qualification or 2,000-RAW timing is claimed. No new external dependencies or model weights were added; offline similarity qualification reuses existing color/raster/pipeline workspace crates as native dev-dependencies.

## Work order

| Work | Current evidence | Next gate |
| --- | --- | --- |
| CI restoration | Generated candidate run 38055623316 built source revision 93678dd1b9977d6ee509679be164fe2ec3b44113 on Mac/Windows; receipts were retained; hidden native QA failed | Resolve native QA failures, then rerun exact-artifact hidden Mac/Windows QA; no publication until pass |
| Culling decisions & evaluation | Detached cancellable desktop jobs, paged React review, explicit acceptance/undo, CLI baseline/scorer & synthetic regression source | Generated gates & exact-artifact hidden RightKit review/apply/undo journey; lock human-labelled shoot-level test corpus |
| Face/eye & similarity weights | Exact YuNet, DINOv2 & MediaPipe v1 bytes/SHA pinned; graph/container inventories recorded; offline DINOv2 Candle core, verified loader, RGB8 input & classical comparison; experimental YuNet & MediaPipe raw graphs plus still-image decode/crops/landmark/blendshape composition; no model qualified | Prove graph/preprocessing/geometry reference agreement, calibrate held-out eye evidence, measure larger-crop accuracy/abstention & named-platform latency |
| Cloud BYOK comparison | Strict bounded multi-image OpenRouter contract/transport in source, unknown blur/blink & reviewable outputs | Shared transport qualification, real provider credentials & controlled ambiguous-burst evaluation |
| Deterministic Auto | Closed-loop fit against the real tone stage, continuous scene keys, centre-weighted key, locus-bounded Auto WB; revision `lightcraft.deterministic-auto.v5`; synthetic regressions incl. repeat stability, burst continuity & spatial sensitivity; corpus renders inspected ([auto-tone.md](auto-tone.md)) | Fidelity suite on consented raws + CC0 corpus with blinded pairwise preference (the protocol in [personal-auto-evaluation.md](personal-auto-evaluation.md)); face weighting once YuNet weights are pinned for product use |
| Look targets | `lightcraft-cli look extract` + `develop.applyLook`: measured statistics of sample renders applied by refitting Auto, exposure settled against the real render ([look-targets.md](look-targets.md)); pipeline, engine & CLI regressions | Held-out preference against hand-matched edits; spatial targets; batch `ids` |
| Personal Auto | Deterministic style-residual learner with 28 scalar/spatial fields; explicit-file extract/train/evaluate CLI; private provenance digests, coverage & fallback receipts in source; migration & preservation regression source; baselines bind to Auto revision v3 (older receipts need re-extraction) | Shoot-disjoint rendered/preference comparison against deterministic Auto; numeric distance alone cannot promote a model |
| Noise estimate & Auto NR | `pipeline::noise` signal-dependent model fitted on flat tiles of the original; `develop.autoNoise` sets luminance/colour NR, ISO fallback; calibrated on the 56-raw CC0 corpus up to ISO 1250 ([noise-reduction.md](noise-reduction.md)) | High-ISO calibration points (none above ISO 1250 in the corpus); VST + guided chroma in the render path with GPU parity |
| Denoise (learned) | Original procedural RGB identity/classical NR benchmark source plus generated 128×128 CPU receipts on Mac/Windows with numeric digests, metrics & repeat timing; all reports stay `UNQUALIFIED` | Checkpoint-rights evidence, independent numerical reference & held-out photographic comparison; or the own-training recipe below |
| ChatGPT-plan OAuth | Researched spec retained | Login convenience after useful photo workflows qualify |

## Learned next steps (not started; recipes)

Each builds on a deterministic piece that already exists, stays off by default, and must reproduce the deterministic result bit for bit when absent.

**Auto decision model.** Planned CPU ridge experiment learns global median/spread residuals from verified proxy measurements & aligned human-edited renders, with optimizer weights fixed. Feature availability, exposure consumers & region policy require explicit qualification. Read-only discovery tooling precedes extraction/training; unknown history & unproven export pairs are excluded. Plan, data rules & work order: [`auto-decision-model.md`](auto-decision-model.md). [#198](https://github.com/Orthic-Labs/ember/issues/198)

**Scene prior for Auto (DINOv2-S head).** Input: the 224 px centre crop through `segment::dinov2` (CPU, f32, fixed op order → repeat-stable embedding). Head: one ridge-regression / softmax layer over ~10 scene classes (portrait, group, landscape, cityscape, night, snow / beach, backlit, indoor, food / product, macro), a few KB of weights, trained in `lightcraft-cli ai personal train` style (f64, no seed). Labels: pseudo-labels from a zero-shot vision–language model on the CC0 corpus + consented shoots, reviewed, then distilled. Use: per-class `auto::fit::Targets` tables blended by class *probability* (never argmax, so neighbouring frames stay continuous); classifier off or absent → `auto_targets` exactly as today. Gate: the same shoot-disjoint preference protocol as Personal Auto, plus a continuity check on bursts. [#199](https://github.com/Orthic-Labs/ember/issues/199)

**Own-trained denoiser.** Data: clean base-ISO CC0 raws (`cargo xtask corpus`) plus consented shoots, developed to scene-linear Rec. 2020; noisy inputs synthesised from the fitted `var(Y) = a·Y + b` model (`pipeline::noise`) across the ISO range (`a` scales with ISO, `b` with read noise), so no paired dataset and no third-party checkpoint rights are involved. Architecture: NAFNet-width16 class (depthwise convolutions, channel attention, PixelShuffle; ~2 M parameters, operator inventory already audited in `models/denoise-candidates.md`), conditioned on `(a, b)` so one network covers all ISOs. Training: Rust on candle (`crates/denoise-train`, not product code), safetensors out; inference in Candle on CPU for bit-stable receipts, Metal within a documented tolerance. Inference backend (source written, unbuilt; CI verifies): `crates/nafnet` holds the one model definition that training and the product share, and `lightcraft-denoise` feature `nafnet` (off by default) adds `naf::NafDenoiser`, which loads a model folder holding `model.safetensors` plus the `contract.json` the trainer writes. It is selected only by what is installed: a folder without that pair yields `None` and the ONNX path is unchanged. Not yet wired: the engine's install path (`denoise.models.install`) still accepts `.onnx` plus `denoise-model.json` only, and the learned-RGB colour contract is still open, so no UI or pipeline stage calls the backend yet. Contract & gates: `denoise-qualification.md` (colour/transfer, tiling, hallucination, latency, held-out preference). Shipping shape: an *Enhance ▸ Denoise* that writes a new derived source, separate from classical noise-reduction sliders. [#200](https://github.com/Orthic-Labs/ember/issues/200)

**Raw-domain classical denoise.** Before demosaic in `crates/raw` (`RawImage::develop`): per CFA plane, generalised Anscombe VST with the measured `(a, b)`, non-local means (7 × 7 patches, 21 × 21 search), inverse VST. Pure Rust, deterministic, no weights; separate raw-cache key; qualified by the procedural benchmark plus consented ISO strata. Wired: `enhance.rawDenoise` (Detail ▸ Raw Noise Reduction) reaches the preview, export, CLI and MCP loaders (`files::load_vec_raw_denoised`), and a photo's decoded sources are keyed by the quantised strength (`media::SourceKey`; strength 0 is the old key); the web build and the ISO-strata qualification are open. [#201](https://github.com/Orthic-Labs/ember/issues/201)

## Current offline DINOv2 source

`crates/segment/src/dinov2.rs` now contains a source-only Candle 0.9.2 DINOv2 ViT-S/14 core: 12 blocks, 384-D embeddings, six heads, 14 px patches, fused QKV, LayerNorm, GELU, LayerScale & residual paths. `DinoV2::forward` accepts already-normalized F32 `[1,3,224,224]` or `[1,3,518,518]` pixels. Native-only `dinov2_artifact` loads exact 88,283,115-byte checkpoint through one bounded read, verifies SHA-256 before allocation, validates all 175 descriptor ranges/shapes/F32 values & creates tensors from owned bytes without ZIP/pickle execution. `dinov2_input` adds bounded antialiased bicubic resize, center crop & ImageNet normalization; 518/592 remains experimental. `dinov2_positions.rs` validates 518 position table, builds bicubic 224 interpolation once & retains both tables for reuse. `DinoV2::cosine` validates finite nonzero `[1,384]` embeddings & computes f64-accumulated cosine clamped to `[-1,1]`.

`DinoV2::from_var_builder` validates architecture shapes & F32 dtype; it accepts caller-supplied tensors without checking artifact identity. Evaluator uses `dinov2_artifact::load_file`, whose immutable-byte identity gate avoids Candle path-based `from_pth` reopening behavior. No inference, model quality, numeric parity, runtime, artifact conversion or platform result is qualified. Source regressions cover hostile identity/layout & preprocessing bounds; they have not run on this revision. See [artifact inventory](models/dinov2-artifact-inventory.md), [input contract](models/dinov2-input-contract.md) & [offline qualification](models/dinov2-qualification.md).

## Reviewable culling commands

`photo.cullSuggest` accepts explicit `ids`, optional finite `rejectBelow` (0..100) & `pickBest`. It reads measurements without writing catalog analysis, flags, undo or command journal. `photo.analyze` with strict boolean `dryRun: true` uses same proposal path. Existing `photo.analyze` without dry-run remains legacy catalog analysis/flagging.

```json
{"command":"photo.cullSuggest","params":{"ids":[1,2],"rejectBelow":50,"pickBest":true}}
```

Result includes per-photo sharpness, clipping, group, unique-best marker, reason codes, qualitative uncertainty, failures & nested `proposal`. Exact sharpness ties receive review status & no pick. Low focus is technical evidence only; intentional blur may deserve keeping. Existing flags receive no replacement proposal.

Pass returned proposal unchanged, with only explicitly accepted suggestions:

```json
{"command":"photo.cullApply","params":{"proposal":"<returned proposal object>","accept":[{"id":1,"flag":"reject"}]}}
```

Replace placeholder string with actual JSON proposal object. Apply verifies catalog revision, source identity, measurements & accepted flags before one undoable batch. Proposal content binding & accepted actions are checked before launching remeasurement; source measurements are still rechecked before commit. Stale/tampered proposals, unknown/duplicate IDs or mismatched flags fail before flag operations are committed. Empty acceptance changes no catalog flags. No original file is deleted.

Desktop `photo.cullSuggest` returns `{taskId,kind,total}` immediately. Detached workers decode without holding Session; `snapshot.status.jobs` reports progress & `completedJobs.result` contains validated proposal. `task.cancel` cancels analysis. React culling review defaults to no selected acceptance, protects existing flags, pages 50 rows at a time & cancels on close. Engine/CLI/MCP direct command dispatch retains synchronous read-only result shape.

Desktop acceptance also returns a `cullApply` task. Fresh measurement rechecks happen off-thread; live revision/source/flags are checked again before one journalled durable batch. UI waits for terminal confirmation before reporting acceptance or cancellation.

## Evaluation

```text
lightcraft-cli cull baseline --manifest FILE_MANIFEST.json --reject-below 50 --out baseline.json
lightcraft-cli cull score --predictions baseline.json --labels LABELS.json --split test --out metrics.json
```

Baseline imports only explicit manifest files into disposable in-memory session; import/decode time is included. Freeze threshold on training shoots. Scorer reports reject precision/recall, false/unknown rejects, decision coverage, abstention & acceptable burst-winner agreement/coverage. Synthetic fixtures verify contracts; they cannot qualify photographic quality. No consented annotations or real path manifest were found in checkout.

Engine timing source separates per-photo origin decode/thumbnail preparation, classical analysis & burst planning.
Missing learned crop/inference stages are explicitly `notApplicable`. Timing metadata is excluded from proposal
binding. CLI baseline validates timing receipts & emits per-photo p50/p95 plus planning totals; status remains
`stage-only`, with hardware/build/timestamps/cache metadata absent. Scorer rejects multiple winners inside one labeled burst, malformed labels & split disagreement; acceptable
winner sets never supply missing keep labels. Regression source remains source-only; generated CI is restored, but follow-up source awaits a full generated test pass.

Evidence protocols:

- [Culling labels, metrics & timing](culling-evaluation.md)
- [Face/eye model candidates](culling-models-faces-eyes.md)
- [Aesthetic/quality/similarity candidates](culling-models-aesthetic.md)
- [Personal Auto evaluation](personal-auto-evaluation.md)
- [Denoise qualification](denoise-qualification.md)
- [CI restoration](ci-restoration.md)
- [Model qualification checklist](models/culling-qualification-checklist.md)

## Personal Auto source harness

```text
lightcraft-cli ai personal extract --manifest EXPLICIT_LABELLED_FILES.json --out NEW_NUMERIC_MANIFEST.json
lightcraft-cli ai personal train --manifest tests/fixtures/personal-auto/minimal.json --out NEW_MODEL.json
lightcraft-cli ai personal evaluate --manifest tests/fixtures/personal-auto/minimal.json --model NEW_MODEL.json --out NEW_REPORT.json
```

Fixture contains synthetic numeric labels only. Explicit-file extraction requires supplied consent & labels, uses disposable local state with sidecar writes disabled, emits canonical numeric features & omits file paths/EXIF/editor fields. Learner fits train shoots, checks frozen split/provenance, skips missing per-control labels & predicts bounded contrast/vibrance/saturation residuals. Lightroom weak labels & accepted Ember labels produce separate experimental variants. Feature-v2 extraction uses 28 fixed pipeline fields: 16 scalar fields including `baseline.exposure`, plus EV mean/stddev & Oklab chroma mean in four spatial cells. Production deterministic Auto remains unchanged. Legacy 16-field manifests/models require re-extraction/retraining; they are never padded. Camera identifiers are excluded from model features, while supplied opaque camera strata may remain in reports. Reports retain label-provenance digests, separate validation/test shoot & camera aggregates, input/eligible/scored/skipped counts & baseline fallback comparisons. No production Auto replacement, render-quality claim or automatic model promotion exists.

`apps/lightcraft-cli/tests/personal_auto.rs` provides source regression coverage for explicit extract → train → evaluate: it checks path-free provenance digests, decoded-proxy baseline receipts, train/test receipt binding, renderer-contract retraining, coverage arithmetic, unclaimed personal-preference status, unchanged source files & XMP sidecars, refusal to overwrite extracted output, & consent rejection. This is source evidence only; current-revision execution remains unclaimed.

## Procedural denoise source harness

`apps/lightcraft-cli/src/denoise_eval.rs` exposes `ai denoise baseline`: five original procedural linear Rec.2020 patterns compare injected-noise identity against exact production guided NR at 0/25/50/75/100. Numeric-only create-new receipts bind clean/noisy/output digests, pixel/gradient error, supplied unverified hardware/revision, first call & 2..10 warm repeats. Finite production output is measured without extra clipping; nonfinite output fails. No photo, catalog, model or provider is read. Generated CI measured 128×128 CPU receipts on Mac/Windows; source analytic/hostile-argument/overwrite regressions remain unrun, so all reports stay `UNQUALIFIED`. See [protocol](denoise-qualification.md).

## Detector qualification source

`crates/segment/examples/yunet_qualify.rs` reads explicit consented P6 RGB8 PPM manifests with normalized face boxes. It records known-negative vs unknown labels, shoot/split/crop slices, threshold configuration, supplied hardware, exact weight hash, model-load timing, first-per-image detection & repeated timings. First pass is not claimed as a cold-process or cold-filesystem benchmark. Metrics remain `UNQUALIFIED` until independent reference parity & held-out quality gates pass.

Run this example only through generated Actions. Required arguments: `--weights`, `--manifest`, `--device cpu|metal`, `--hardware`, `--out`, `--score-threshold`, `--nms-threshold`, `--match-iou-threshold`; optional `--repeats 2..30` & bounded `--source-revision`. Model tensors are cached on selected device; graph execution validates input/weights/final heads without per-layer host readbacks. Receipt records custom 640px preprocessing as experimental, numeric repeat tolerance, all detections vs metric-eligible detections & one-pass shoot aggregation. It does not run eye-state inference, cull photos, change catalogs or publish private input paths.

## Similarity qualification source

`crates/segment/examples/dinov2_qualify.rs` reads strict explicit consented P6 RGB8 manifests & a pinned DINOv2 artifact. Source validates shoot-disjoint splits, duplicate/reversed pair rejection, cross-split identical RGB rejection, optional numerical-reference coverage & input bounds. Receipts bind exact model/manifest/input/reference SHA-256 digests, supplied threshold/source/hardware, preprocessing condition, first/repeated embedding timings & repeat agreement. Pair metrics keep unknown labels as abstentions; precision/recall exclude them. Model/first embeddings are cached for pair scoring; repeats recompute preprocessing & inference. Same RGB inputs also feed production classical signature/similarity after sRGB-to-linear conversion, using a separately supplied `--baseline-threshold`. Zero-norm classical signatures abstain; method/split/shoot reports retain scored/eligible/unknown counts, coverage & false-merge rate. Baseline conversion/signature timing is recorded separately from DINO inference.

Run only through generated native Actions, using separate `small224` (224 crop/256 short edge) & experimental `large518` (518/592) conditions. Receipts always remain `UNQUALIFIED`; schema validation, numerical comparison or matching digests cannot establish photographic quality or approve a model. This example does not change catalog flags, rank burst winners, classify eyes/focus/aesthetics or qualify a 2,000-RAW workflow. Actual classical/DINO comparison results, held-out human labels & named Mac/Windows native qualification remain required. See [offline protocol](models/dinov2-qualification.md).

## MediaPipe raw-graph source

`mediapipe_artifact` performs one bounded owned-byte read, verifies whole bundle identity & four child hashes, then validates static descriptors before loading F32 constants. IEEE Float16 storage is converted once; Int32 controls remain on CPU & exact aliases reuse tensor handles. Corrected schema-driven inventory contains 1,222 tensors & 817 operators: earlier Blendshape `SHAPE`/`UNIQUE` labels were incorrect & replaced with authoritative `SUM`/`SQRT`/`RSQRT`/`SQUARED_DIFFERENCE` facts.

`Bundle::forward_detector`, `forward_landmarks` & `forward_blendshapes` execute raw Candle graphs with NHWC layout, explicit convolution layouts, SAME padding, negative max-pool padding, bounded graph/scratch reservations & finite checks per node. Reservations cover explicit tensor/readback/workspace estimates; driver caches remain outside that accounting. This diagnostic prototype synchronizes device work per node, so it makes no throughput claim.

`examples/mediapipe_qualify.rs` accepts consented strict bounded tensor fixtures & optional independent raw-output references. Receipts bind input/reference/model hashes, exact graph I/O shapes, first/repeated timings & numerical errors without storing tensor values or paths. Always `UNQUALIFIED`: reference agreement, held-out quality & Mac/Windows measurements remain required. See [inventory](models/mediapipe-v1-artifact-inventory.md) & [raw qualification](models/mediapipe-qualification.md).

## MediaPipe still-image source

`mediapipe_input`, `mediapipe_detector`, `mediapipe_blendshapes` & `mediapipe_photo` compose bounded upright RGB8 → detector → weighted NMS → rotated 1.5× face crops → 478 projected landmarks → 146 pixel-coordinate inputs → 52 named coefficients. Exact artifact metadata defines separate detector `[-1,1]` & landmark `[0,1]` normalization. Apache-licensed policy source is pinned at `458200ffced12a9db596ddf1cce3380ea05f71fb`, with license & source hashes retained. Float-bilinear sampling & deterministic score ties are explicit differences from source CPU behavior, requiring independent reference comparison.

`Bundle::analyze_image` accepts explicit detector/NMS/presence thresholds & 1..32 face limit. Strict presence failure leaves landmark/coefficient/eye outputs absent; degenerate crop geometry returns an error rather than publishing partial successful analysis. Raw blink/squint/wide coefficients are uncalibrated expression evidence. This prototype makes no culling decisions & accesses no catalog.

`examples/mediapipe_photo_qualify.rs` accepts one explicit consented P6 RGB8 image, optional strict ordered numeric reference & warm repeats. Receipts bind hashes, pinned input policy, thresholds, output counts, timings & numerical errors while excluding pixels, geometry, landmarks, coefficients & paths. All results remain `UNQUALIFIED`. Source review corrected XYWH/keypoint ordering, aspect-dependent crop projection & wide/tall letterbox placement, with asymmetric regression source; tests remain unrun. See [image contract](models/mediapipe-image-contract.md) & [photo qualification](models/mediapipe-photo-qualification.md).
