# Ember on RightKit: new packages

> **Last updated:** 2026-10-24. Revision: first version (port lane, Orthic-Labs/ember#240).

Step four of Adrian's order (foundation, cut list, add-ons, **new packages**): the survivors of the cut list that no catalogued RightKit owner covers. `rightkit-ownership.json` (104 capabilities, read from `@rightkit/release` 0.6.4) has none for RAW, colour science, lens profiles, GPU contexts or a journal, so none of these is an ownership exception today. All are proposed as **add-ons** under the placement rule (RightKit-owned from the first consumer); Adrian's alternative for any of them is to keep it product-specific, which needs no RightKit work.

Nothing moves until the decision issue D1 is answered. The code stays under its `lightcraft-*` names in the meantime so upstream fixes keep landing on `main`.

## Decisions and issues

| # | Question | Issue |
|---|---|---|
| D1 | Who owns the source of record for N1 to N6 and N11 while the lightcraft upstream sync continues: RightKit or Ember's `lightcraft-*` paths? | bogusyogi/rightkit#1158 |
| N7 | One wgpu pin for Ember (wgpu 30) and CutRight (wgpu 29.0.1) before a shared crate | bogusyogi/rightkit#1159 |
| N8 | Torn-tail JSONL journal in `rightkit-fs` (Ember is the third copy) | bogusyogi/rightkit#1155 |
| N10 | Three copies of the same unsafe FFI | bogusyogi/rightkit#1160 |
| A14 | Own-model distribution lane (`ember` app key, image-model kind, public delivery) | bogusyogi/rightkit#1161 |

## Proposed packages

Sizes are `wc -l` of `src/**/*.rs` at this branch. "Sync" is whether upstream fixes keep arriving for the code.

| # | Proposed crate | Ember code now (lines) | Public surface | First consumer; recurs in | Class | Sync continues? | Issue now? |
|---|---|---|---|---|---|---|---|
| N1 | `rightkit-tiff` | `crates/tiff` 1,597 (+429 tests) | TIFF/IFD reader and writer; the dependency of raw, meta and the desktop tests. | Ember only. ScrapeRight takes the third-party `image` tiff feature, not hand-rolled code. | add-on | yes (upstream crate) | D1 |
| N2 | `rightkit-photo-meta` | `crates/meta` 5,577 | EXIF, XMP (read and write, `lc:settings` round trip), IPTC-IIM, and JPEG/PNG/WebP/TIFF container walkers. | Ember only: no other app tree declares an EXIF or XMP reader. | add-on | yes | D1 |
| N3 | `rightkit-raw` | `crates/raw` 31,568 (+3,736 tests) | `probe`, `decode` to a RawImage (CFA, levels, DNG colour tags, opcodes), `embedded_preview`, `demosaic`, `develop`, camera colour table, semantic masks. | Ember only: no other app tree (nine checked) declares a RAW, EXIF or HEIF decoder. | add-on | yes, continuously (`docs/gaps.md` gaps 1, 2, 6, 7, 10); keep the `lightcraft-raw` name and path while syncing | D1 (anchor) |
| N4 | `rightkit-color` | `crates/color` 889 | RGB colour spaces from primaries, Bradford adaptation, CCT/tint, sRGB/gamma/PQ/HLG transfer, OkLab/OkLCh, tone-curve splines. | Ember only. `@rightkit/theme` has CSS colour maths, a different domain; if catalogued, add a scanner exemption for the Rec.709 luma weights (`theme.math.relative-luminance` fires 28 times on Ember today). | add-on | yes (small) | D1 |
| N5 | `rightkit-lens` | `crates/lensdb` 1,059 | Lensfun-format profile loader from a user directory, camera/lens identification, distortion/TCA/vignetting models converted to DNG opcode polynomials. | Ember only. | add-on | yes | D1 |
| N6 | `rightkit-image-codecs` | `crates/codecs` 8,892 + `crates/heif` 2,246 | Format sniffing, decode to linear light with ICC, encoders with metadata embedding (JPEG, PNG, TIFF, WebP, JPEG XL, AVIF), optional HEIC decode. | Ember. GenRight, MailRight, ScrapeRight and HeardRight take the third-party `image` crate with four different feature sets (allowlisted, not hand-rolled), so a shared owner is optional. | add-on | yes | D1 |
| N7 | `rightkit-gpu` | `crates/gpu` context, backend, exit, budget 1,976 of 5,359 | wgpu device context: per-OS backend policy, crash sentinel around device creation, adapter pick and limits, device-memory budget (halve on out-of-memory, grow on success), exit gate. Kernels stay in Ember. | Ember and CutRight (`cutright-compositor-gpu`, per the 2026-10-24 port order; not re-read here). | add-on | probably: `budget.rs` cites upstream issue #1764, `backend.rs` and `exit.rs` cite #136 and #620 (origin not verified) | #1159 |
| N8 | `rightkit_fs::journal::JsonlJournal`, then `rightkit-journal` | `crates/catalog` journal.rs + store.rs + lock.rs 1,143 (+1,520 tests) | `JsonlJournal`: open(path, validate) scans under the file lock, truncates to the last valid record and fsyncs; append via `append_record`. Ember's layer on top (CRC per op, atomic snapshot, compaction, damaged-log quarantine, format-version gate, writer lock) is generic over Op and State, which needs `journal.rs:72-73` to stop importing `Catalog` and `Op` first. | Ember (`store.rs`, the third torn-tail copy), CodeRight `journal_repair.rs`, `rightkit-workflow` `journal.rs`. | add-on | partly: the generic core would be Ember-owned (format VERSION 10; upstream commits reuse format numbers, so no cherry-picks); the Op/rules/keywords domain keeps syncing | #1155 |
| N9 | `rightkit-infer` | `denoise-core` + `denoise` + `nafnet` + `denoise-onnx` + `gpu/nn.rs` about 7,700 lines; `faces` 5,511 and the segment culling modules 14,430 if kept | Model manifest validation and hostile-file probe, bounded ONNX-subset interpreter, a `TileRunner` trait with CPU, wgpu and ONNX Runtime runners behind an agreement check, Bayer tiling and blending, `.lcdn` cache. | Ember only. HeardRight and CodeRight run ONNX Runtime through `rightkit-onnx` for speech and embeddings, a different shape. Status "later": no learned accuracy is claimed and the desktop app does not enable `denoise`. | add-on | `denoise-core`, `denoise`, `faces`: yes; segment culling modules: no (fork-only); training stays product tooling | runtime: no, wait for the first own model; distribution lane: #1161 |
| N10 | `rightkit-sysmem` | `crates/sysmem` 338 | `release_free_memory` (allocator pressure relief), `total_physical_memory`, `window_monitor_profile`. | Ember, Pulse (`engine/src/disk_index.rs:795-804`) and `rightkit-fsindex` (`src/index.rs:446-449`) each carry the same `malloc_zone_pressure_relief` FFI. | add-on | yes (small, upstream crate) | #1160 |
| N11 | `rightkit-raster` | `crates/raster` 1,020 | `Image<T>` buffers (Rgb32f, Rgba8, Plane), Lanczos-3/Mitchell/box resampling, Gaussian blur, histogram. | Ember only (20 crates depend on it). | add-on | yes | D1 |

Not proposed (they stay product): `develop`, `pipeline`, the catalog domain (ops, rules, keywords), `engine`, the preview job pool, `desktop-host`, `geom`, the mask-model modules of `segment`.

## Notes that decide the order

- **Dependency weight.** Taking `rightkit-crypto` in `crates/mcp` (this branch) added `blake3` and `constant_time_eq` to the lock. `blake3` compiles C and assembly by default through `cc` (its `build.rs`), which meets Ember's "no C/C++ dependencies" rule only because `ring` already sits in the lock through the owners. A `pure`-style feature on the owner would avoid it; not filed, because the weight is small next to the TLS stack.
- **N3 first.** RAW is the product's centre of gravity and takes most upstream fixes; moving it first would settle D1 in practice. Moving it last keeps upstream sync free for as long as possible.
- **N8 does not need D1.** It is a RightKit-side change to `rightkit-fs` that Ember adopts afterwards.
- **A14 waits for the first model.** Ember does no work until a trained model exists; the issue only unblocks the lane.
