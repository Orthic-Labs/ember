# Auto decision model (plan)

Status: read-only `ai decision manifest` discovery is implemented in source; extraction, training,
evaluation & model integration are planned. This is an offline experiment for learning continuous treatment targets from
finished renders. It does not change deterministic Auto or authorize a default change. If enabled
later, an absent, invalid or incompatible model must preserve deterministic Auto's current result.

## Decision boundary

`auto::fit::fit_with` in `crates/pipeline/src/auto.rs` optimizes the existing
`Targets { top, bottom, spread, median, clip, w }` against Ember's renderer. Region Auto in
`crates/pipeline/src/auto/regions.rs` applies that optimizer per Subject, Background and Sky.
Current decision logic supplies global targets through `auto_targets`; subject exposure also has
its own `key_target` calculation and bounded render bisection. A learned policy cannot replace
`auto_targets` alone and claim to control all treatment decisions.

First experiment is deliberately narrow: learn residuals for global `median` and `spread` only,
with all optimizer weights fixed by the existing deterministic policy. Keep `top`, `bottom`,
`clip`, and `w` fixed. Do not train region targets or replace subject exposure bisection in this
stage. Record their outcomes as diagnostics. A later experiment can add a target field only with
an explicit implementation path and a separately held-out benefit. Fixed objective weights are
not labels inferred from finished JPEGs.

The eventual integration must explicitly name each target consumer. Global exposure currently
uses `key_target` outside `auto_targets`; region Auto computes subject exposure separately and
also uses `SUBJECT_EV` bounds and render settling. Until each is deliberately wired and qualified,
the model scope remains the global `median` / `spread` residual experiment above.

## Features and availability

Do not assume all desired features already exist on the current inference path. The current
region path derives proxy pixels and region populations; the following are candidate additions,
not an available contract: ISO, flash state, focal length, noise parameters `(a, b)`, masked
clipping/chroma, and full-resolution noise measurements. Inventory each source, extractor,
missingness behavior, cost, and version before adding it. A missing value must have a defined
numeric encoding plus missingness indicator, or the sample must be dropped with a reason; never
silently substitute a plausible measurement.

Begin with a small, measured set available from the 512 px proxy: global scene EV percentiles and
the existing `SceneKey` inputs. Add per-region coverage, median EV, and spread only after checking
those calculations against the actual masks and treatment path. Any expanded feature set needs a
versioned schema and hash. Schema mismatch disables model use and reports a diagnostic; it never
changes deterministic output.

Optional `segment::dinov2` embeddings are a later, separate experiment. Keep them only if they
produce a measured improvement on untouched shoots after accounting for extraction cost,
availability, and repeatability. Do not infer semantic intent from scalar differences alone.

## Targets and objective

The first model predicts bounded residuals over deterministic global `median` and `spread` targets.
Bounds, normalization, and the exact loss are fixed in the model format and evaluation report.
Optimizer weights remain fixed to deterministic Auto; rendered target measurements do not identify
those weights. Zero residual must take the existing deterministic path exactly.

Rendered labels are measured from the person's finished image using a specified transfer function,
primaries, statistics implementation, mask population, crop/orientation transform, and precision.
Record those definitions and versions with every numeric manifest. Reject non-finite values and
unsupported geometry. Only accept geometry with a verified transform and bounded alignment
residual; perspective, content-aware changes, or other unalignable edits are dropped with explicit
reasons. Compare aligned mask populations before measuring region labels.

## Eligible examples and provenance

Use only positive evidence of human develop edits. The catalog field
`Adobe_libraryImageDevelopHistoryStep` is an implementation identifier for history inspection,
not proof by itself that a photo was personally edited. Require recognized edit evidence and
exclude untouched photos, Auto-only edits, sync/paste-only copies, and uncertain histories with
explicit drop reasons. History labels may vary; unknown history is excluded. Treat edits after an
Auto step as candidate preference evidence only when an actual human change is established; retain
confidence/source strata for evaluation.

Raw/export pairing must bind source identity, virtual-copy identity, export recipe, dimensions,
edit/version time, and geometry. Filename similarity alone is insufficient. The private pairing
manifest necessarily contains local paths and pairing evidence; keep it local and ignored. The
separate extracted numeric manifest contains only versioned numeric features, labels, split IDs,
and non-identifying drop/quality metadata. Keep both artifacts bounded and outside Git.

Catalog access is read-only. Do not change catalog, media, or export state. Exports must already
exist as user-created inputs. No media or identifying paths enter source control or model artifacts.

## Split and evaluation

Assign related material together before model fitting: source/export pairs, virtual-copy families,
bursts, near-duplicates, copies, and photos from the same shoot/session. Use catalog folder/session
evidence and inspect ambiguous groupings; a fixed ten-second gap does not establish independent
shoots. Keep complete groups within one split. Preserve camera/lens/year slices where sample counts
allow, and reserve untouched shoots for final evaluation. Record split inputs and hash.

Fit normalization on training data only; select regularization using validation shoots only; use
final test shoots once. Report target error against deterministic Auto, by-camera and scene slices,
burst continuity, clipping/non-finite/render safety, and repeatability. Include rendered comparisons
and blinded preference evaluation; target error alone cannot establish improved results.

Floating-point f64 and fixed row order do not guarantee bitwise equality across platforms or math
libraries. Specify supported platform/runtime, numerical tolerance, and repeatability procedure;
measure them. Do not claim cross-machine bitwise identity without evidence.

There is no current yield estimate. Inventory the actual catalog and report eligible counts by
drop reason and shoot before deciding whether data supports training. Do not extrapolate a fixed
percentage or shoot count from catalog size.

## Model sequence

1. Start with CPU ridge regression in f64, fixed row order, and a documented deterministic solver.
   Save schema, target fields, fixed objective weights, bounds, normalization, split hash,
   optimizer revision, platform/runtime, and tolerance in the artifact.
2. Consider a small nonlinear model only if ridge misses a defined interaction and the added model
   beats ridge on untouched shoots and blinded preference while meeting runtime and repeatability
   requirements.
3. Consider embeddings or scene-conditioned targets only as independent experiments with measured
   benefit and a valid licensing/availability record for any model artifact.

Do not rule out a training framework by category alone. Assess any proposed framework against this
continuous scalar regression objective, reproducibility, license, offline use, and deployment
boundary. Training may be offline; product inference remains Rust and the existing renderer remains
authoritative.

## Work order

1. Specify exact eligible-history signals, pairing identity, geometry checks, measured feature
   availability/cost/missingness, label statistics, target fields, fixed weights, and grouped split.
2. Inventory a bounded catalog/export set read-only. Produce a private path-bearing pairing
   manifest plus counts; do not extract until provenance and alignment contract is satisfied.
3. Extract versioned numeric records with explicit drop reasons and group assignments.
4. Train ridge on training shoots; select regularization on validation shoots; evaluate once on
   untouched test shoots using the measures above.
5. Only after evidence, design integration for the named target consumers, including the separate
   `key_target` and region exposure paths. Preserve the deterministic fallback and record model
   identity in any decision receipt.
6. Keep model preference unset unless held-out rendered comparison and blinded preference gates
   pass. No promotion follows from training or source changes alone.

## Open questions

- Which catalog history records reliably identify a human adjustment, and what conservative
  unknown-history rule applies?
- Which local exports have complete source/version/geometry provenance?
- Can a crop/orientation transform align raw proxy masks to exported pixels within measured
  residual limits?
- Which candidate features are actually available at inference, and at what cost/missingness?
- Does the narrow median/spread model improve held-out preference, and are region/semantic inputs
  worth a separately measured experiment?
