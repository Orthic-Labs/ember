# Reusable Auto corpus

Full corpus preparation preserves available before/after information across all discovered sources
& finished exports. The two-scalar `median`/`spread` view is one experiment, not its scope boundary.
Preparation does not establish learned quality or authorize training, upload or model promotion.

## Artifacts

- Private JSONL inventory: content identity, original paths, source/export/history relationships,
  all available edit provenance, unresolved identities & explicit reasons for missing labels.
- Immutable catalog snapshot & original export metadata remain authoritative historical evidence.
  Retain full states/history, including nested/local data, rather than only a slider whitelist.
  Imported settings are reference-editor metadata, not equivalent Ember control labels.
- Private proxy cache: lossless f32 images with dimensions, colour interpretation & content hashes.
  Preserve unregistered views even when geometry prevents a paired numeric label. Source-only
  extraction retains unlabelled sources without pretending they are finished treatment examples.
- Derived views: scalar tone, colour diagnostics, heuristic region diagnostics & eventually model
  inputs. Every view references canonical identities & shared grouped splits.

`ai decision extract --manifest PAIRS --out RECORDS --proxy-dir CACHE` retains intermediates.
`--source-manifest SOURCES` replaces `--manifest` for unpaired media; its strict schema is
`ember.ai-source-extract-input.v1`, with `user_consented:true` & 1–64 records containing
`photo_id`, `source_path`, `source_sha256`, `source_is_raw`.

Cache objects contain little-endian u64 width & height, then row-major interleaved f32 samples.
RGB uses three channels; alpha planes use one. Filename includes SHA256 & channel count.
Dimensions & samples participate in SHA256. Atomic publication never replaces an existing object;
every existing object is verified before reuse. Failed writes are errors, not successful corpus rows.
Numeric JSON contains relative object names only; cache paths & originals stay private.

Proxies have at most 512 pixels on their long edge. They support image/colour/region experiments at
that resolution; they do not replace full-resolution RAWs for denoise or detail training. Original
media remain content-addressed by manifests. Before/global Auto/regional Auto are Ember renders,
not recovered historical reference-editor Auto previews. This distinction remains in each view.

## Label meaning

Rendered neutralizing-WB estimates & midtone chromaticities measure image colour. They do not
recover a photographer's intended white point, historical Kelvin, or a unique set of edit controls.
Subject/background/sky planes come from Ember heuristics. They are diagnostics, not annotated
semantic masks. Region statistics use one aligned source population across before/Auto/finished.
Empty populations have null measurements with reasons. Source-only rows have no finished label.

Manual controls, Auto/manual history counts, white-balance modes, nested looks & local tables must
remain available per pair with their provenance. Current-state history is not automatically the
state of a historical export. Preserve uncertainty rather than infer human intent from defaults.
Scene classes, clean/noisy denoise pairs & adjudicated culling labels need their own evidence.

## Model/framework boundary

Unsloth's [official vision documentation](https://unsloth.ai/docs/basics/vision-fine-tuning)
describes image-plus-text supervision & multi-image inputs for vision-language fine-tuning.
A compatible vision-language experiment needs image inputs & justified response labels; two scalar
residuals alone are not that dataset. Image classifiers & continuous treatment regressors are
different consumers. Choice of training framework does not determine corpus preservation scope.

Model-specific adapters must prevent finished targets leaking into inference inputs & use shared
source-family/shoot splits. Numeric ridge is a CPU baseline; no timing or model-quality claim follows
from its algorithm name. GPU/Vast selection belongs to a concrete training recipe & measured need.

## Completion evidence

Account for every inventory item, verify cache hashes & all retained provenance, produce shared
leakage-safe splits, demonstrate loaders replay from retained data, & identify exactly which training
views are ready. A passing numeric loader alone is not full-corpus completion. Existing originals,
catalog snapshot & unresolved examples remain retained. No model is trained by these extractors.
