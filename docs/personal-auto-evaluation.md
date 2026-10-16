# Personal Auto evaluation

This document defines an offline evaluation for personalized `deterministicAuto`. It does not
change current settings, pipeline, importer, catalog behavior, or rollout status.

## Current contract & limits

Current `develop.auto` reads thumbnail source, source metadata, current develop settings, then
calls `auto_tone`; it writes eight global sliders & does not call Auto WB
([`crates/engine/src/cmd/develop.rs:292`](../crates/engine/src/cmd/develop.rs#L292)). `auto_tone`
fits 512×512 proxy after scene-linear preprocessing, uses sorted EV/chroma percentiles, fixed
low-key/high-key/backlit branches, clamps outputs, & returns finite slider values
([`crates/pipeline/src/auto.rs:33`](../crates/pipeline/src/auto.rs#L33),
[`crates/pipeline/src/auto.rs:63`](../crates/pipeline/src/auto.rs#L63),
[`crates/pipeline/src/auto.rs:68`](../crates/pipeline/src/auto.rs#L68),
[`crates/pipeline/src/auto.rs:108`](../crates/pipeline/src/auto.rs#L108)). Existing regression
coverage is synthetic scenes plus baseline-exposure invariance, not personal preference
([`crates/pipeline/src/auto/auto_regression.rs:147`](../crates/pipeline/src/auto/auto_regression.rs#L147),
[`crates/pipeline/src/auto/auto_regression.rs:207`](../crates/pipeline/src/auto/auto_regression.rs#L207)).

Lightroom import keeps per-photo `settings`, XMP, history, & snapshots
([`crates/engine/src/lightroom_catalog.rs:136`](../crates/engine/src/lightroom_catalog.rs#L136)); it
reads catalog/WAL data without modifying source files
([`crates/engine/src/lightroom_catalog.rs:338`](../crates/engine/src/lightroom_catalog.rs#L338)).
Mapped settings are partial: `crs:` values become Ember controls, while unmapped fields are
reported ([`crates/engine/src/crs.rs:139`](../crates/engine/src/crs.rs#L139),
[`crates/engine/src/crs.rs:372`](../crates/engine/src/crs.rs#L372),
[`crates/engine/src/crs.rs:406`](../crates/engine/src/crs.rs#L406)). Camera profiles carry only an
enable switch because profile corrections are Ember's own file corrections
([`crates/engine/src/crs.rs:304`](../crates/engine/src/crs.rs#L304)). Deferred Lightroom Auto
sentinels are removed & reported, so importer data cannot serve as exact Auto ground truth
([`crates/engine/src/lightroom_catalog.rs:480`](../crates/engine/src/lightroom_catalog.rs#L480),
[`crates/engine/src/lightroom_catalog.rs:884`](../crates/engine/src/lightroom_catalog.rs#L884)).

Therefore Lightroom settings are weak, cold-start relative-style labels only. They are evidence
of observed look after different renderer, process version, camera profile, & possible
local correction; they are not pixel or slider ground truth.

## Dataset & split

Split unit is complete shoot. Assign `shoot_id` before feature extraction from stable
catalog/import identity plus source folder & capture session. Group adjacent captures from one
camera in one folder into one burst when capture timestamps are within 10 seconds; if burst ID or
timestamps are missing, conservatively group the whole folder/session. Never split frames,
virtual copies, exports, or burst members across train, development, or held-out data.

Stratify split assignment by raw/JPEG, camera make/model, process version, profile family, lens,
& capture year. Keep one fixed held-out set of complete shoots, plus second camera-held-out
slice where possible. Record split manifest hashes; an image, XMP packet, Lightroom row, or
derived feature may occur in one split only.

Use three labels:

1. `deterministicbaseline`: current `develop.auto` output from frozen source pixels, source info,
   & current default settings. Evaluate Auto WB separately, since `develop.auto` excludes it.
2. `weaklabelcoldstart`: mapped Lightroom settings & XMP fields, converted to normalized slider
   deltas from baseline. Drop deferred sentinels, unmapped/profile/local fields, malformed rows,
   & records without usable mapped field. Keep confidence per field; do not treat missing as
   zero. This variant has no accepted Ember edits during fitting.
3. `embergroundtruth`: explicitly accepted Ember develop settings after a human edit. Require
   source pixels, baseline settings, final settings, edit timestamp, & provenance. Exclude
   edits created only by Auto, copied/synced settings, imported Lightroom mappings, automatic
   versions, & unresolved render failures. Shoot may contribute one target per photo, but
   all photos in same shoot remain in one split.

## Model & feature boundary

Fit only residuals over frozen baseline output. Scene correction remains current Auto; personal
style predicts a bounded residual, then applies fixed slider clamps:

```text
prediction = clamp(deterministicbaseline + style_residual, control_bounds)
```

Scene residuals include exposure, white balance, highlight/shadow recovery, whites, & blacks.
Style residuals may include contrast, vibrance, saturation, texture, clarity, dehaze, & other
global appearance controls only after enough labels exist. Exclude camera profile, calibration,
lens correction, process version, raw black/baseline exposure, geometry, masks, spots, & crop
from personal style targets. Keep those as fixed renderer inputs or separate quality strata.

Features are deterministic scalar statistics from current 512×512 proxy & source metadata needed
by current pipeline: EV percentiles, luminance/chroma moments, highlight/shadow shares, aspect,
raw/JPEG kind, lens focal range, & exposure/WB metadata. Camera/model/profile identifiers are
strata & nuisance checks, not taste features. Normalize with train-only statistics; fixed feature
order, fixed float64 arithmetic, fixed iteration count, no random seed, & explicit non-finite
fallback make repeated runs byte-stable.

A feasible first model is one ridge linear regressor per residual control, 20–50 scalar features,
with coordinate descent over fixed passes & no new heavy dependency. Store quantized weights,
bounds, feature schema, training manifest hash, baseline receipt contract, & model version. A receipt
from explicit extraction binds decoded thumbnail pixels, `SourceInfo`, baseline settings, Auto revision,
camera-profile cache key, & render-cache version; it is an integrity identity after path stripping, not
proof of source ownership or Lightroom authenticity. Reject model on schema/hash/receipt-contract
mismatch or non-finite output. Fit cost is small CPU work after existing proxy creation; target
budget is <10 ms/photo for prediction on reference CPU, with zero change to current render path
when model is absent or rejected.

## Confounder controls

Report results overall & by camera/model, process version, profile family, raw/JPEG, lens, &
scene-key stratum. Require each comparison to be shoot-disjoint & camera-balanced. Run
camera-held-out report; gain that disappears there is camera/process/profile memorization,
not personal style. Compare residuals after subtracting deterministic baseline, & report
scene-correction controls separately from style controls. Do not let Lightroom profile names,
`CameraProfile`, AI/local fields, or imported unmapped values enter features or targets.

## Metrics & guardrails

For every held-out shoot, render baseline, each candidate, & accepted Ember target at same
size/settings. Log per-photo outcomes plus per-shoot aggregates; photo-level average alone
cannot pass.

* Primary preference: blinded randomized pairwise review of candidate vs baseline, with shoot as
  bootstrap unit. Candidate passes only when preference is at least 55% & the 95% shoot-bootstrap
  lower bound is above 50% overall & on every camera-held-out slice with at least 20 shoots.
* Target edit distance: normalized L1 over eligible style controls. `embergroundtruth` must reduce
  median distance by at least 10% vs baseline; `weaklabelcoldstart` must reduce it by at least 5%
  with a positive paired improvement on at least 60% of held-out shoots.
* Scene safety: candidate must not worsen scene-control distance by more than 2% relative to
  baseline, so style residual cannot undo exposure/WB corrections.
* Render safety: zero non-finite settings, zero new render failures, zero invalid control values,
  & no increase in clipped-pixel share above 1 percentage point on any held-out shoot.
* Regression safety: current synthetic Auto invariants remain unchanged; baseline outputs remain
  identical when model is absent, rejected, or below confidence threshold.
* Cold-start honesty: weak-label result is reported as style transfer only; no claim of improved
  personal preference is allowed until Ember-groundtruth thresholds pass on held-out shoots.

## Source harness

`crates/pipeline/src/personal_auto.rs` implements deterministic float64 ridge residuals for contrast, vibrance & saturation. Feature contract `lightcraft.personal-auto-features.v2` has 28 fixed fields: existing 16 scalar statistics plus 12 spatial statistics. `personal_auto_spatial.rs` divides upright 512px Box/WB-only proxy into 2×2 cells in row-major order (`g00,g01,g10,g11`, row then column). Each cell emits EV mean, EV population standard deviation & mean Oklab chroma. Spatial EV is clamped to `[-32,32]`, chroma to `[0,4]`; pixels with luminance ≤ `1e-6` are excluded, empty cells emit three zeros, & an entirely empty valid population is refused. Source pixels, dimensions, storage & derived values are validated before fitting. These features describe photometric layout, without identifying subjects, faces, sky or intent.

Production `auto_tone` carries revision `lightcraft_pipeline::auto::REVISION` (`lightcraft.deterministic-auto.v5`: closed-loop shaping sliders, continuous scene keys, centre-weighted key, process-version-aware preparation & camera-space Auto WB; receipts from older revisions are rejected & need re-extraction). Spatial features enter only offline residual evaluation; scene controls remain deterministic baseline. Camera identifiers never enter features. Models record schema, train shoots, manifest identity, label sources/counts & experimental status. Missing fields are skipped, settings are bounded, malformed models fall back to baseline. Spatial sensitivity is experimental: held-out preference & safety gates below remain required.

CLI `ai personal extract --manifest FILE --out NEW_MANIFEST` converts an explicit photo manifest into canonical numeric features through same Rust Auto/pipeline path. Input is strict bounded JSON with `version:1`, `userConsented:true`, unique opaque `shoot_id`/`photo_id`, frozen `train|validation|eval|test` shoot split, explicit regular-file `path`, optional supplied opaque shoot `camera` stratum, & supplied `weakLabelColdStart` or `emberGroundTruth` label with provenance/confidence. Accepted weak provenance kinds are `lightroom`, `lightroom-mapped-settings`, `mapped-lightroom-settings`, & `synthetic-weak`; accepted Ember kinds are `human-edit` & `synthetic-human-edit`, always with `accepted:true`. Each listed file uses fresh ephemeral local session so catalog/raw caches do not accumulate; sidecar writes are disabled & imported settings are reset. Extraction never scans folders or writes path/EXIF data. Output labels retain numeric values/deltas & confidence while reducing provenance to normalized kind, acceptance, & source SHA. Each photo also receives a `baselineReceipt` containing digests of decoded thumbnail pixels, canonical emitted features, canonical baseline values, plus source facts/settings, deterministic Auto revision, feature schema, camera-profile cache key, & render-cache version. Evaluation recomputes feature/baseline digests before training, requires current develop-settings schema, & rejects mutations or stale renderer receipts. Extraction fails before output creation on malformed labels, split/ID duplicates, unreadable/decode failures or bounds violations; unpicked photos never become rejects. Resulting path-free manifest is accepted by `ai personal train`/`evaluate`; direct numeric manifests allow only documented root/shoot/photo fields, with paths rejected. Models include per-variant opaque photo/shoot receipt IDs, normalized label kinds, provenance SHA digests, & baseline receipt contract, compared exactly against evaluation manifest:

```text
lightcraft-cli ai personal extract --manifest INPUT.json --out NUMERIC_MANIFEST.json
lightcraft-cli ai personal train --manifest NUMERIC_MANIFEST.json --out NEW_MODEL.json
lightcraft-cli ai personal evaluate --manifest NUMERIC_MANIFEST.json --model NEW_MODEL.json --out NEW_REPORT.json
```

Canonical synthetic example: [`minimal.json`](../tests/fixtures/personal-auto/minimal.json). Separate weak/Ember variants report numeric edit distance by photo, shoot, split & camera. This source has no rendered comparison, preference votes, production Auto replacement or automatic promotion.

Experimental model envelope uses `lightcraft.personal-auto-eval.model.v4`, core model `lightcraft.personal-auto.v2` version 3, baseline receipt `lightcraft.personal-auto.baseline-receipt.v2` & report `lightcraft.personal-auto.report.v2`. Legacy 16-field manifests/receipts/models are rejected explicitly: re-extract source manifests & retrain, never pad old input or reinterpret old weights. Canonical manual fixture contains deliberately authored synthetic spatial values, not photographic measurements.

Source provenance SHA preserves an opaque receipt when private metadata is stripped. Matching receipts establishes consistency with supplied manifest, not independent proof of who edited a photo. Reports expose input/eligible/skipped counts, `modelScored` & `fallbackScored`. Fallbacks remain in aggregate comparisons as unchanged deterministic baselines, preventing selective omission; only actual model predictions can produce numeric-evaluated status. Manual numeric fixtures may omit baseline receipts & are explicitly `manualNumeric`; they cannot establish render parity.

## Rollout gate

Run all three variants offline in shadow mode against immutable manifest. Release candidate
only after thresholds above pass twice from clean manifests, including camera-held-out results;
otherwise retain current deterministicAuto. Initial rollout, if later approved, is opt-in,
confidence-gated, reversible per photo, & suggestion-only until further review. No automatic
catalog mutation, provider call, reference-editor app-bundle read, or model-supply claim is part of this
evaluation.
