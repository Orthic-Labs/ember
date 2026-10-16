# Culling model qualification checklist

Status: executable acceptance plan only. No face, eye or similarity model is qualified for shipping. No metric below is a current Ember result or a general benchmark claim.

## Frozen evidence protocol

Use manifest version 1 from [`culling-evaluation.md`](../culling-evaluation.md): `shoots[]` are split units; each photo has `ground_truth` `keep`, `reject` or `unknown`; each burst has an `acceptable_winners` set. Keep every burst, related frame & near-duplicate inside one shoot split. Tune only on `train`, select thresholds once on `validation`, keep `test` unread until final scoring. Archive exact manifest, rubric, candidate ID, config, build revision & hardware record.

`unknown` stays outside known-label precision, recall & false-reject denominators. Empty `acceptable_winners` stays outside winner agreement & coverage. A missing or low-confidence model result is `unknown`; it is never converted to `reject`, `closed`, or a selected winner.

Face/eye annotations may live in a consented, access-controlled sidecar keyed by `photo_id` & view. They must not replace culling labels. Use `eye_state`: `open`, `closed`, `occluded`, `unknown`; score eye state only for `open`/`closed`, report occluded/unknown abstention separately. Use one-to-one face matching at IoU ≥ 0.50. Freeze sidecar, rubric & slice thresholds before reading `test`.

Views must be paired for same photo/face:

- `full_frame`: normal culling frame at model input policy;
- `larger_crop`: face-centered crop with documented margin & coordinate transform;
- `small_face`: full-frame face whose shorter bbox side is <64 px after mapping into 640 px detector coordinates;
- `crop_openeye`: larger crop with one visible face whose eye labels are `open` or `closed`.

## Acceptance matrix

| Area | Frozen slice & input | Measurement | Acceptance gate | Required receipt |
|---|---|---|---|---|
| Manifest integrity | All consented shoots | IDs, split isolation, burst membership, winner subsets, provenance | 100% validation; no burst crosses shoot/split; no duplicate IDs | Archived manifest, rubric version, consent/retention record & digest |
| Face detection | `full_frame`, known face labels | One-to-one IoU ≥0.50 precision & recall | Report overall, `small_face`, non-small; target precision ≥0.99 & recall ≥0.90 on each known slice; otherwise no qualification | TP/FP/FN by slice, abstentions, unknown/occluded counts & confusion table |
| Face crop transfer | Paired `larger_crop` view | Same face identity retained after crop coordinate mapping; precision & recall | Target precision ≥0.99 & recall ≥0.90 on crop slice; no silent face loss; out-of-frame/occluded => `unknown` | Per-view boxes, source-to-crop transform, matched IDs, abstention reasons |
| Face safety | Known `keep` photos with faces, including blur/occlusion | False reject attributable to model face signal | False-reject rate ≤0.01 on explicit keeps; no reject from missing/low-quality face output | Per-photo decision, face status, reject reason & baseline comparison |
| Eye state | `crop_openeye` only | Precision, recall & abstention for `open`/`closed`; occluded/unknown excluded from known denominator | **BLOCKED ON QUALITY:** MediaPipe v1 bytes, SHA-256 & model-card license record are pinned; conversion, port parity, labels, held-out eye threshold & platform measurements remain pending. Do not report pass, eye accuracy or open-eye product claim. | Eye confusion matrix, unknown/occluded abstention, threshold/config receipt [#206](https://github.com/Orthic-Labs/ember/issues/206) |
| YuNet role boundary | YuNet 2023mar output | Detector boxes/keypoints only | Keypoints may establish crop geometry; they cannot classify eye-open state or blink; no eye gate may consume YuNet alone | Candidate output schema explicitly marks eye state unavailable |
| DINO similarity | Held-out paired photos within same shoot/burst window | Cosine score after frozen 224 & 518 preprocessing; false merge at validation-selected threshold | Report score spread & abstention; false-merge rate ≤0.01 on known keep/reject cross-burst pairs, with `unknown` excluded; must not reject photos | Pair list, threshold, preprocessing, false-merge numerator/denominator & luma-signature baseline |
| DINO burst quality | Adjudicated bursts with non-empty `acceptable_winners` | `selected_winner_id ∈ acceptable_winners`; winner coverage | Winner agreement ≥0.90 & coverage ≥0.90; no promotion from synthetic evidence; tie sets are accepted | Per-burst selected ID, acceptable set, score margin, tie/abstention reason |
| Intentional blur/style | Explicit keeps labelled in accepted culling manifest; style notes remain reviewer evidence | Kept blurred/soft-focus frame retained; model disagreement rate | No hard reject; report blur-preservation rate separately; low margin or disagreement keeps baseline order & review state | Per-photo `keep`/`unknown`, baseline order, model score & reviewer label |
| Culling regression | Same held-out test manifest for baseline & candidate | Reject precision/recall, false rejects, decision coverage | Reject precision ≥0.99, reject recall ≥0.90, false-reject rate ≤0.01; no safety-metric regression vs frozen classical baseline | `lightcraft.cull-eval.v1` report, raw predictions & baseline report |
| Mac Metal timing | Exactly 2,000 RAW files on one named Mac with Metal device | Cold & warm `decode`, `crops`, `inference`, `total`; p50/p95; peak memory | Complete records required; no numeric target is asserted here; missing stage, sample count, timestamp, machine or cache state = `incomplete` | Machine model, OS, CPU, GPU, RAM, storage, build revision, cache state, timestamps, peak RSS/device memory [#207](https://github.com/Orthic-Labs/ember/issues/207) |
| Windows CPU timing | Same ordered 2,000 RAW files on one named Windows CPU machine | Cold & warm `decode`, `crops`, `inference`, `total`; p50/p95; peak memory | Complete records required; no numeric target is asserted here; missing stage, sample count, timestamp, machine or cache state = `incomplete` | Machine model, OS, CPU, RAM, storage, build revision, cache state, timestamps, peak RSS [#207](https://github.com/Orthic-Labs/ember/issues/207) |
| Artifact identity | Every candidate artifact | URL/version, source revision, byte length, SHA-256, retrieval date, graph/tensor inventory | Exact bytes & immutable digest required before use; ETag/Last-Modified alone fail; code, weight & data terms recorded separately | Attribution record, license notices, SHA-256 receipt & conversion manifest |
| Port parity | Pure Rust Candle implementation | Reference tensor/output parity on frozen fixtures; preprocessing & coordinate transform parity | No C++ or ONNX Runtime; no GPL/reference-product assets; every tensor shape/name/operator checked; mismatch blocks qualification | Fixture hashes, operator inventory, tolerances, output diffs & device used |
| Unknown handling | No face, partial/occluded face, malformed crop, low margin, disagreement | Abstention coverage & downstream decision | Unknown remains review/abstain; never synthesized as reject, closed eye, similarity merge or winner | Reason-coded unknown rows & denominator policy |

## Artifact gates

| Candidate | Required identity & licensing record | Current decision |
|---|---|---|
| YuNet 2023mar | OpenCV Zoo commit `f12e12798e8314f7c074a6656816c048dcc95b7a`; 232,589 bytes; LFS SHA-256 `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4`; model-directory MIT notice; WIDER Face data rights remain a release gate. See [`yunet-2023mar-graph.json`](yunet-2023mar-graph.json). | Detector-only research candidate; no eye-state claim. |
| MediaPipe Face Landmarker float16/1 | Exact versioned task URL; [`mediapipe-face-landmarker-v1-qualification.json`](mediapipe-face-landmarker-v1-qualification.json); 3,758,596 bytes; SHA-256 `64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff`; bundle graph/tensor & image-metadata inventories; Apache-2.0 model-card notices; data/redistribution terms reviewed separately. | Pinned identity & license evidence complete; verified loader, raw graphs & experimental still-image decode/crops/mapping source present; pending reference parity, held-out eye threshold & Mac/Windows measurements; no `crop_openeye` result. |
| DINOv2 ViT-S/14 | Pinned repo/model revision, `.pth` byte length, captured SHA-256, Apache-2.0 model/repo notices & separate training-data terms. See [`dinov2-small-qualification.json`](dinov2-small-qualification.json). | Offline core is useful for similarity-only experiment; **unqualified** for aesthetic, face, blink, focus, quality or culling claims. |

## Static port review flags

`crates/segment/src/yunet.rs` must not be treated as qualified solely because it loads. Current production path verifies size & SHA-256 in `YuNet::load` before calling private `from_verified_bytes`; preserve that invariant. `Graph::validate` checks structural counts, dependencies & output names, but not an exact node-by-node digest, so its “fingerprint” is defense-in-depth rather than artifact identity; the verified SHA remains authoritative. Review this distinction before artifact qualification.

Graph metadata does not define preprocessing, decode or NMS. Freeze RGB/scale/mean, resize/letterbox placement, 640-coordinate mapping, stride decode, score fusion & NMS against one reference before measuring. Current output keypoints are not eye-state labels. No Rust edit is part of this checklist.

## Timing receipt shape

Use culling timing protocol: fresh process with no warmed decode/crop/inference cache for cold; same process after one documented priming pass for warm; identical file order & config. Store:

```json
{
  "candidate": "<id>",
  "split": "test",
  "sample_count": 2000,
  "hardware": {"machine": "<named>", "os": "<version>", "cpu": "<model>", "gpu": "<model-or-none>", "ram_gb": 0, "storage": "<type>"},
  "cache_state": "cold|warm",
  "stages_ms": {"decode": {"p50": 0, "p95": 0}, "crops": {"p50": 0, "p95": 0}, "inference": {"p50": 0, "p95": 0}, "total": {"p50": 0, "p95": 0}},
  "peak_memory_mb": 0,
  "status": "complete|incomplete"
}
```

Paper/model-card metrics, source-paper FLOPs, synthetic fixtures & unlabelled timing are evidence boundaries only; none is Ember accuracy, burst quality, face recall or platform performance.
