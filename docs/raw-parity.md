# RAW parity: cameras, formats, demosaic, colour and lens profiles

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-17 · **Change:** minor (Hasselblad 3FR / FFF checked on four CC0 files, #171 and #172; GoPro GPR (VC-5) decoded, #170; raw-domain denoise wired through the loaders, one decode per settled strength, #201; upstream absorb 2026-10-14: Hasselblad Lunar verified and Leaf MOS recognised, 236 verified and 232 preview; Nikon D4 and D90 image areas; Nikon in-file distortion)

Raw processing is the deepest single dimension of reference editor parity: a photographer whose camera
does not decode, or decodes with the wrong colour, cannot switch. This document measures it model by
model. The numbers feed [target-app-parity.md](target-app-parity.md); individual shortfalls are
ranked in [gaps.md](gaps.md). Details per format: [cr3.md](cr3.md),
[raf-compression.md](raf-compression.md), [sony-lens-corrections.md](sony-lens-corrections.md), [nikon-lens-corrections.md](nikon-lens-corrections.md), [camera-lens-tables.md](camera-lens-tables.md),
[camera-preview-colour.md](camera-preview-colour.md), [denoise.md](denoise.md).

## Headline

| Measure | Value | Kind |
|---|---:|---|
| reference publisher-listed camera models | **1,446** | measured: [raw/adobe-cameras.tsv](raw/adobe-cameras.tsv) |
| Decoded from sensor data and **verified** on a real file | **236** (16%) | measured: [raw/evidence.tsv](raw/evidence.tsv) |
| Decoder path should handle it, **unverified** | **951** (66%) | measured from the format rules (632 of them DNG, mostly phones) |
| **Preview-only** (embedded JPEG) | **232** (16%) | measured |
| **Unsupported** (not imported, refused or decodes wrongly) | **27** (2%) | measured |
| Models added since 2017 (Camera Raw 10+): verified / unverified / preview / unsupported | 82 / 648 / 24 / 0 of 754 | measured |
| **Share of photographers whose camera fully works** (decodes from sensor data), use-weighted | **~85%** | estimated: measured classes × written usage weights × per-format confidence |
| …of which on a body we have verified | **~42%** | estimated, same weights |
| Camera colour: reference publisher profiles vs ours | 1,500 reference publisher Standard profiles, 481 models with Camera Matching profiles vs **4 bundled profiles** + per-file fits + 52 spectral matrices | measured (bundle listing, reference publisher list, `assets/camera-profiles`) |
| Lens profiles: reference publisher vs ours | **3,635 vs 0** (in-file corrections only) | measured |
| **RAW readiness** (decoding 60%, colour 25%, lens 15%) | **~56%** (range 50–60%) | estimated, see [Scores](#scores) |

Re-measure with `python3 docs/raw/coverage.py` (standard library only): it classifies every reference publisher
model from the format rules in the script (which mirror `crates/raw/src`) plus the per-model evidence,
rewrites [raw/coverage.tsv](raw/coverage.tsv) and prints the tables below; `--check` fails when the
committed table is stale.

## How it was measured

- **reference publisher's list**: helpx "Camera Raw supported cameras" (the live page refuses scripted requests, so
  the Wayback Machine capture of 2026-10-03 was parsed): 1,446 camera rows from 40 makers after
  dropping profile-only rows; maker, model, extension(s), Camera Matching profile, minimum Camera Raw
  version. The year a model was added is approximated from that Camera Raw major version.
- **What we decode**: the decoders and their refusals in `crates/raw/src` (`lib.rs` routing, `vendor/*.rs`
  coding variants, `crx.rs` CRX limits, `orf.rs` / `srw.rs` compressed refusals), the import list
  `lightcraft_engine::import::EXTENSIONS`, and `KNOWN_UNSUPPORTED` in `crates/raw/tests/corpus.rs`. Doc
  claims were not used.
- **Verified** means a real file from that body decoded from sensor data in: the corpus test over the
  `RAW_SAMPLES` manifest (`xtask/src/main.rs`, 101 CC0 files; bodies whose sample is in
  `KNOWN_UNSUPPORTED` excluded), `crates/raw/tests/cr3_corpus.rs` (3 bodies exact against an external decoder;
  the EOS R5 Mark II pinned, with a QP-map check of 33,665 private files), the RAF reference
  check ([raf-compression.md](raf-compression.md)), and the raw.pixls.us bulk checks recorded in issue
  #535 (all 100 Sony bodies there; 87 decode) and PR #215 (every Panasonic / Leica RW2 / RWL / RAW body
  there, 178 files from 118 bodies). Bulk-check body lists were matched to reference publisher names through the
  raw.pixls.us folder names. Checks without a body list (≈ 60 NEFs, 35 CR2 bodies for the CFA layout)
  are not counted as verified.
- **Usage weights** ([raw/usage-weights.tsv](raw/usage-weights.tsv)): maker buckets from 2025 market
  data (Nikkei production: Canon ~36%, Sony ~29%, Nikon ~14%, Fujifilm ~12% of mirrorless; BCN retail:
  Sony 29.9%, Canon 27.4%, Nikon 15.1%), adjusted for the DSLR installed base and for the makers named
  in this repo's 325 user issues (Sony 33, Nikon 31, Canon 26, phones 23, Fujifilm 9, Panasonic / Leica
  8, OM / Olympus 6, Pentax / Ricoh 6). Inside each bucket, 80% goes to the popular bodies of the last
  ~8 years (listed in the file), 20% to the bucket's other reference publisher models.
- **Per-format confidence** for unverified bodies (the chance their files decode correctly): DNG, CR2,
  RAF, RW2 0.95; NEF, ARW, PEF 0.9; ORF, SRW, SR2 0.8; **CR3 0.6** (the CRX decoder refuses several
  coding variants and only four bodies are verified; maintainers report the R7 working).

## By maker (measured)

| Maker | reference publisher models | verified | unverified | preview | unsupported | Main reason for preview / unsupported |
|---|---:|---:|---:|---:|---:|---|
| Sony | 149 | 86 | 51 | 9 | 3 | DSLR-A200…A390 and A850 12-bit compressed ARW (preview, #535); A100 and DSC-F828 refused (regression #702); SRF not imported |
| Canon | 134 | 12 | 99 | 21 | 2 | CRW (2000–2004 bodies) preview; EOS-1D / 1Ds TIFF raws not imported |
| Samsung | 128 | 1 | 118 | 9 | 0 | compressed SRW (NX1, NX30, NX300, NX500, NX2000, NX3000, NX3300, NX mini, Galaxy NX) |
| Apple | 113 | 1 | 112 | 0 | 0 | — (DNG / ProRAW) |
| Panasonic | 104 | 93 | 11 | 0 | 0 | — |
| Nikon | 99 | 5 | 94 | 0 | 0 | — (HE / HE★ is a per-shot setting, preview-only) |
| Fujifilm | 97 | 16 | 55 | 26 | 0 | pre-X FinePix RAF without a raw IFD (estimated split) |
| Google | 88 | 1 | 87 | 0 | 0 | — |
| Olympus | 71 | 3 | 9 | 59 | 0 | **compressed ORF** |
| Leica | 61 | 13 | 48 | 0 | 0 | — |
| Xiaomi, Oppo, LG, Huawei, OnePlus, Motorola, Nokia | 139 | 0 | 139 | 0 | 0 | — (DNG) |
| Pentax / Ricoh | 60 | 4 | 56 | 0 | 0 | — |
| Hasselblad | 34 | 1 | 2 | 31 | 0 | Hasselblad-compressed 3FR / FFF |
| Phase One / Leaf / Mamiya | 59 | 0 | 0 | 43 | 16 | IIQ, MOS preview; TIFF raws and MEF not imported |
| Casio | 26 | 0 | 26 | 0 | 0 | — (DNG) |
| DJI | 21 | 0 | 21 | 0 | 0 | — (DNG) |
| OM Digital Solutions | 7 | 0 | 0 | 7 | 0 | **compressed ORF** |
| Kodak, Konica Minolta, Epson, Sigma, GoPro, Contax, others | 56 | 0 | 23 | 27 | 6 | DCR not imported; KDC, MRW, ERF, X3F preview |
| **All** | **1,446** | **236 (16%)** | **951 (66%)** | **232 (16%)** | **27 (2%)** | |

## By use (estimated)

| Bucket | Weight | Camera fully works | On a verified body | Why |
|---|---:|---:|---:|---|
| Canon | 33% | 73% | 19% | 18 of the 22 popular bodies write CR3; 4 of them verified (M50, R8, R100, R5 Mark II), the rest at 0.6 |
| Sony | 27% | 98% | 91% | bulk-checked |
| Nikon | 16% | 91% | 12% | decoders cover every NEF coding but HE; few bodies verified |
| Fujifilm | 10% | 91% | 45% | |
| Panasonic | 4% | 100% | 98% | bulk-checked |
| OM / Olympus | 3% | 3% | 1% | compressed ORF |
| Phones | 3% | 93% | 1% | DNG |
| Pentax / Ricoh | 1.5% | 94% | 14% | |
| Leica | 1% | 95% | 5% | |
| DJI | 1% | 95% | 0% | |
| Medium format and others | 0.5% | 28% | 1% | IIQ, 3FR, X3F |
| **Weighted** | 100% | **85%** | **42%** | |

About 5 in 6 raw photographers' cameras decode, but only about 2 in 5 use a body we have actually
checked, and Canon, the largest bucket, is the weakest of the big four.

## Variant matrix (depth per format)

✅ decoded and checked on real files · 🟡 decoded, partial or unverified · ⬜ preview-only or refused.

| Format | Variant | Status | Evidence |
|---|---|---|---|
| CR2 | lossless JPEG slices | ✅ | 8 corpus bodies; CFA phase from `CR2CFAPattern` (#85, 35 bodies); masked-column black level |
| CR2 | sRAW / mRAW (YCbCr) | ✅ | `vendor/cr2/sraw.rs`, 31 samples from 16 models |
| CR3 | CRX lossless, version 0x100 | ✅ | M50, R100, R8 exact against a reference; R5 Mark II (odd-height planes) pinned |
| CR3 | C-RAW 0x100 (horizontal tiles), 0x200 (single 14-bit tile, adaptive QP incl. the QP-map escape) | ✅ | same 3 bodies exact; R5 Mark II pinned, QP maps of 33,665 files decode exactly, matches the camera JPEG |
| CR3 | other CRX variants (vertical tiles, partial subbands, other plane flags, 0x200 multi-tile) | ⬜ | refused in `crx.rs`, falls back to the preview; which bodies write them is unmeasured [#156](https://github.com/Orthic-Labs/ember/issues/156) |
| CR3 | sRAW / mRAW | ⬜ | 4-plane Bayer only [#157](https://github.com/Orthic-Labs/ember/issues/157) |
| CR3 | HEIF / HDR PQ | ⬜ | HEVC tracks recognised, not decoded [#158](https://github.com/Orthic-Labs/ember/issues/158) |
| CR3 | dual-pixel raw | ⬜ | never substituted [#159](https://github.com/Orthic-Labs/ember/issues/159) |
| CRW | all | ⬜ | preview [#160](https://github.com/Orthic-Labs/ember/issues/160) |
| NEF | uncompressed, lossless, lossy type 1 / 2, lossy after split | ✅ | D5100, D7000, D7500, Z f corpus; ~60 NEFs by hand; #633; D4, D90, D5000, D600 and D610 image area measured against the camera JPEG (white level too for the D4, D90 and D5000) |
| NEF | packed 14-bit stored under a compressed tag (Z 6II uncompressed) | 🟡 | `nef.rs` `stored_plain`: a 34713 strip with no `0x0096` table whose size is exactly the MSB-first packed (12/14/16-bit) or 16-bit-word layout is read as samples; synthetic test only, no real Z 6II file measured (#161) [#161](https://github.com/Orthic-Labs/ember/issues/161) |
| NEF | High Efficiency / HE★ (JPEG XS) | ⬜ | named, preview (#193); needs a JPEG XS decoder [#162](https://github.com/Orthic-Labs/ember/issues/162) |
| ARW | uncompressed, cRAW (ARW2), lossless LJ92 (L / M / S) | ✅ | 86 bodies (#535); M/S black level 1024; the Hasselblad Lunar, a NEX-7 under Make HASSELBLAD, routed by Sony's raw coding (upstream #1275) |
| ARW | packed 12-bit (A900) | ✅ | one CC0 file; A850 unmeasured |
| ARW | ARW6 Compressed / HQ (compression 32766; ILCE-7RM6) | ✅ | 8 CC0 files; 12-bit mode unsupported; `docs/arw6-compression.md` |
| ARW | 12-bit compressed of DSLR-A200…A390 | ⬜ | preview (#535) [#163](https://github.com/Orthic-Labs/ember/issues/163) |
| ARW | A100 ARW; SRF (F828, V3) | ⬜ | refused (#702); SRF not imported [#164](https://github.com/Orthic-Labs/ember/issues/164) |
| ARW | ARQ pixel shift | ⬜ | no code [#165](https://github.com/Orthic-Labs/ember/issues/165) |
| SR2 | DSC-R1 | ✅ | CC0 3221 sample |
| RAF | uncompressed, lossless and lossy compressed; Bayer and X-Trans; GFX 14/16-bit | ✅ | 24 files from 13 bodies exact |
| RAF | pre-X FinePix without a raw IFD | ⬜ | `raf.rs` refuses (26 reference publisher models, estimated split) [#166](https://github.com/Orthic-Labs/ember/issues/166) |
| ORF | uncompressed 16-bit, 12-bit packed, two-field 12-bit, 12.8 bpp | ✅ | E-1, E-400, XZ-2 corpus; #651 |
| ORF | **compressed** (every Olympus / OM body since ~2008) | ⬜ | `orf.rs` refuses; the E-M1 and E-M10 III corpus files are `KNOWN_UNSUPPORTED` |
| RW2 / RWL / RAW | formats 2, 4, 5, 6, 7, 8 and 16-bit words | ✅ | 118 bodies (#215) |
| PEF | uncompressed, Huffman, packed 12-bit | ✅ | K10D, K-3, K-5 IIs |
| PEF / DNG | Pentax pixel shift | ⬜ | frames not combined [#167](https://github.com/Orthic-Labs/ember/issues/167) |
| SRW | uncompressed, 12-bit packed | ✅ | EX1 corpus; NX10, NX20 |
| SRW | compressed 32770 / 32772 / 32773 | ⬜ | refused [#168](https://github.com/Orthic-Labs/ember/issues/168) |
| DNG | uncompressed, LJ92, lossy JPEG / Smart Preview, Deflate incl. float, LinearRaw, tiles / strips, row / column interleave | ✅ | 6 corpus DNGs; 4 ILCE-6400 JXL DNGs |
| DNG | JPEG XL (DNG 1.7, lossless and lossy) | ✅ | synthetic bit-exact; real lossless and lossy files |
| DNG | Apple ProRAW (incl. semantic mattes) | ✅ | iPhone 12 Pro corpus; ProfileGainTableMap read but off (ΔE00 19 against reference editor with it on) |
| DNG | phone DNGs (Pixel, Galaxy, Xiaomi…) | 🟡 | Pixel 2 XL is the only real file. Synthetic tests cover the opcode lists a phone writes (per-Bayer-phase `GainMap`, `FixVignetteRadial`, `WarpRectilinear`/`2`, `MapTable`/`MapPolynomial`, per-row/column deltas and scales), hostile-list rejection (counts, sizes, non-finite floats, fuzzed bytes), `DefaultBlackRender` read and written back, LinearRaw and JPEG XL. `WarpFisheye` and `TrimBounds` in `OpcodeList3` are applied per DNG 1.6.0.0 chapter 7 (pp. 92-94, 97): the fisheye maps through `f = (θ·(k0 + k1θ² + k2θ⁴ + k3θ⁶))/r` with `θ = arctan r` and bilinear resampling, with the spec's increasing-function restriction checked when parsing; `TrimBounds` crops (bottom, right exclusive) and the default crop moves with it. Open: both are still ignored in `OpcodeList1`/`2` (a mosaic trim would also shift the CFA phase and black pattern); the resampling is bilinear where the spec recommends a cubic spline; no real file carries either opcode; `DefaultBlackRender` is carried, not rendered; no Pixel 6+, Galaxy or Xiaomi file to compare; white-level pile-up fix #548 [#169](https://github.com/Orthic-Labs/ember/issues/169) |
| DNG | GoPro GPR (VC-5) | 🟡 | decoded: one VC-5 stream per file, RGGB 14-bit and GBRG 12-bit, ported from GoPro's reference decoder (Apache-2.0 OR MIT, `crates/raw/src/vc5.rs`); synthetic and hostile-input tests, five public GoPro samples pinned in `crates/raw/tests/gpr_corpus.rs` (not CC0, so not in the corpus download), CI verification pending [#170](https://github.com/Orthic-Labs/ember/issues/170) |
| 3FR / FFF | DNG-like layouts | ✅ | a TIFF without a DNG version tag that describes its CFA IFD with DNG's tags (`RawFormat::CfaTiff`, `dng::decode_as`); uncompressed (X1D) and LJ92 strips; `corpus_hasselblad_3fr_fff` checks X1D, H3D, CFV-50c and H4D-50 files against the RGB preview in IFD0 [#171](https://github.com/Orthic-Labs/ember/issues/171) |
| 3FR / FFF | Hasselblad compression | ✅ | lossless JPEG with scan selection value 8 (`ljpeg::decode_hasselblad`; every FFF, H3D, H4D, CFV-50c 3FR): 32-bit little-endian words, sample pairs coded as two categories then two differences; rule measured on 14 CC0 files, then the full frames of H3D, CFV-50c (3FR and FFF) and H4D-50 re-decoded by an independent re-implementation (green rank correlation with the preview 0.97 to 0.99, RGGB colour agreement 0.80 to 0.95, other layouts at most -0.04); pinned in `corpus_hasselblad_3fr_fff`. Per-model counts above not re-measured [#172](https://github.com/Orthic-Labs/ember/issues/172) |
| IIQ, MOS, MRW, ERF, KDC, X3F | all | ⬜ | preview; an uncompressed MOS, whose IFD0 is the mosaic itself with no Make tag, is recognised by Leaf's data block `0x8606` (upstream #1275) [#173](https://github.com/Orthic-Labs/ember/issues/173) |
| IIQ, MOS, MRW, ERF, KDC, X3F | all | ⬜ | preview [#174](https://github.com/Orthic-Labs/ember/issues/174) |
| DCR, MEF, MFW, DXO, TIFF raws | all | ⬜ | not imported [#175](https://github.com/Orthic-Labs/ember/issues/175) |

### Compressed ORF: a clean-room starting point

Two Olympus patents by Takashi Ishikawa describe the *method* of lossless RAW compression in prose; neither names ORF
and neither gives the file layout. See the closed #240 and the open #360 for why a decoder has to be derived from the
files and prose rather than from dcraw-family code.

- [US7333035B2](https://patents.google.com/patent/US7333035): lossless RAW compression for SLR cameras:
  median-edge-detector prediction from the left, upper and upper-left neighbours, a separately coded sign, Golomb-Rice
  codes whose parameter `k` comes from flatness run counters, counters reset per line.
- [US7715639B2](https://patents.google.com/patent/US7715639B2/en): a sibling with predictor variants, `k` derived from
  neighbouring positions, raw low-order bits and a correction filter.

They give the algorithm's shape; constants, the bit layout and the stream header still have to be established from CC0
sample files. A pointer for contributors, not legal advice.

## Demosaic, levels, crops and lens metadata

| Stage | Target | Ember | Status |
|---|---|---|---|
| Bayer demosaic | reference publisher's own, plus AI "Raw Details" | AHD (default), PPG, bilinear (`crates/raw/src/demosaic`) | 🟡 quality unmeasured against reference editor; no Raw Details [#51](https://github.com/Orthic-Labs/ember/issues/51) |
| X-Trans demosaic | ✅ | in-house edge-weighted directional | 🟡 unmeasured |
| Foveon | ✅ (older bodies) | — | ⬜ |
| Monochrome / LinearRaw | ✅ | handled, no demosaic | ✅ |
| Pixel shift (Sony ARQ, Pentax, OM high-res, Nikon) | ✅ (Sony, Pentax, Nikon) | — | ⬜ |
| Black and white levels | per model, calibrated | read from each file (masked columns, maker notes, SR2SubIFD, CMP1) | 🟡 per-body bugs keep surfacing: A450–A700 SR2 black level and M/S ARW pedestal (#535, fixed); 5D Mark IV CR2 renders dark (#455, open); white-level pile-up in phone DNGs (#548, fixed) [#181](https://github.com/Orthic-Labs/ember/issues/181) |
| Crop masks and aspect | ✅ | DefaultCrop, Sony recorded size, Canon AspectInfo, Panasonic in-camera crops, Nikon CropArea, measured Nikon bodies without it (D4, D90, D5000, D600, D610) | 🟡 7RM3 / 7RM4 crop origin 8–32 px off (cosmetic) |
| Lens metadata | ✅ read for profile selection | EXIF lens fields read; used only for the in-file corrections below | 🟡 |
| Highlight reconstruction | ✅ | neutral clip, chromaticity diffusion (`highlight.rs`) | 🟡 tuned by eye |
| AI denoise | ✅ Bayer and X-Trans, model bundled | Bayer only, user-installed weights, pure-Rust CPU / GPU | 🟡 no X-Trans, no shippable weights |
| Raw-domain denoise (classical) | — | Detail ▸ Raw Noise Reduction (`enhance.rawDenoise`, 0–100): per-colour Poisson-Gaussian fit, Anscombe transform, non-local means per mosaic plane, before demosaic (`crates/raw/src/rawdenoise.rs`); Bayer only; preview, export, CLI and MCP share one loader, the decoded-source cache is keyed by the strength, and a slider drag keeps the preview on the last decoded strength so the release is the one decode (an export always decodes at its own strength); a smart preview is built from the denoised decode when the strength is on at build time and is not rebuilt when the strength changes later (rebuild it to match); thumbnails skip it ([#201](https://github.com/Orthic-Labs/ember/issues/201)) | 🟡 not qualified on consented ISO strata, no web build, X-Trans untouched |
| Super Resolution | ✅ | — | ⬜ |
| HDR / panorama merge to raw DNG | ✅ | linear DNG (float16 HDR) with the source's colour tags | ✅ |
| Convert to DNG | ✅ | uncompressed or LJ92 tiles (`dngwrite.rs`) | ✅ (no lossy DNG) |

## Colour

| Item | reference publisher | Ember |
|---|---|---|
| Default profile per model | reference publisher Standard for every model (1,500 profile names in the bundle) | the DNG's own matrices; else a pooled profile (**4 bundled**: ILCE-7CR, ILCE-7M4, X-H2S, X-T4); else a guarded per-file fit to the camera's embedded JPEG; else spectral matrices (**52 models**, rawtoaces-data); else a neutral matrix |
| Camera Matching looks | 481 models (plus phone-maker looks) | none (the per-file JPEG fit imitates the camera's own look) |
| Creative profiles | reference publisher Color, Portrait, Landscape, Vivid, Monochrome… | our own looks (Color, Neutral, Vivid, Landscape, Portrait, Monochrome) |
| DCP / dual-illuminant model | ✅ | dual-illuminant DNG colour (ColorMatrix / ForwardMatrix 1-2, CameraCalibration, AnalogBalance), HueSatMap, LookTable, ProfileToneCurve; external `.dcp` never loaded (clean-room) |
| White balance in Kelvin | ✅ for every model | absolute Kelvin where the camera has colour matrices (its own, or one of the 52 spectral rows), also for files whose look is a per-file fit (#730), or where Sony's white-balance presets in the file give the scale (6 bodies, `kelvin_presets`); other raws keep the relative scale (#296) |

**How far the default rendering is from reference publisher's** (CIE ΔE; measured where stated):

- Against the camera JPEG (what the fit targets): the Sony bulk check (#535) accepted fits on 81 of 87
  bodies, median ΔE 5.1, ΔL\* +0.2; rejected fits fall back to a neutral matrix at median ΔE 18.5,
  ΔL\* −11, chroma ×0.39. RW2 140 of 174 files accepted, median 5.0. NEF 13 of 13, 2.9–7.8. RAF 23 of
  24, pooled X-H2S 1.62 / X-T4 2.34. SRW 2.2–7.6.
- Against reference editor itself (few): Sony DRO median ΔE76 7.5 → 3.3; Olympus E-1 vs reference catalog editor
  ΔE00 11.25 → 8.30; ProRAW with its gain map ΔE00 19 (why the map is off). No measurement against
  reference publisher Standard across models exists.
- User evidence: 33 open issues about raw colour or rendering (wrong colours, dark, flat, green,
  desaturated), the largest group of open core-path issues.

**Colour score ~35%** (estimated): most decoded files start close to the camera's own look (ΔE ~5),
about 10% start far off (ΔE ~18), nothing is calibrated, Kelvin WB is missing for fitted cameras without colour matrices, and
the distance to reference publisher Standard is unmeasured. In the readiness numbers this becomes a colour factor
of ×0.85 for a working photographer (they correct colour by hand, at a cost) and ×0.75 for the
essentials user (the default look is what they keep).

## Lens profiles

| Item | reference publisher | Ember |
|---|---|---|
| Profile database | 3,635 profile files, 56 makers, auto-selected | **0** (reference publisher LCPs never used; no database of our own) |
| In-file corrections | ✅ | DNG opcodes (WarpRectilinear, FixVignetteRadial, GainMap…) applied; Panasonic / Leica RW2 distortion (79 photos, 13 bodies, 11 lenses); Sony tag 0x7037 distortion for the ILCE-7RM4A (2 lenses); Nikon NikonNEFInfo distortion (tag `0xc7d5`, about the sensor-shifted optical centre) for the Z 6II, Z 8 and Z f: F-mount lenses on the FTZ within 0.2 px of the camera JPEG, Z lenses within 0.5–1.5 px except the 24-70's wide end (7 px median; `docs/nikon-lens-corrections.md`) |
| Vignetting / lateral CA from maker notes | ✅ | 🟡 opt-in "Use camera lens corrections" (`optics.camera_corrections`, off by default): Fujifilm vignetting table (`0xf010`, 59 validated bodies) applied through a maker-neutral radial model; Fujifilm lateral CA (`0xf00f`) decoded but not applied (it moved the planes the wrong way after our demosaic); Sony, Panasonic and Olympus skipped (no documented table layout: Sony's int16s[16] / [32] have no published meaning, Panasonic and Olympus store On/Off flags); code and synthetic tests only, CI pending (`docs/camera-lens-tables.md`, #242) |
| Manual distortion, vignetting, CA, defringe | ✅ | ✅ |

**Lens score ~10%** (estimated): automatic correction reaches roughly Panasonic / Leica, phone and
drone DNG users (~9% of photographers by the weights above); reference publisher reaches nearly everyone.

## Scores

| Part | Weight | Score | Basis |
|---|---:|---:|---|
| Decoding | 60% | **77%** | use-weighted 85% × 0.9 (76.5%) for per-body decode bugs still surfacing (levels, crops, WB: #535, #455, #461, #702) |
| Colour | 25% | **35%** | see [Colour](#colour) |
| Lens profiles | 15% | **10%** | see [Lens profiles](#lens-profiles) |
| **RAW readiness** | 100% | **~56%** (range 50–60%) | 45.9 + 8.75 + 1.5; demosaic and highlight quality unmeasured |

The previous estimate (earlier on 2026-10-10) was ~50%, with decoding at ~78% weighted by maker
from the format list alone. Measured model by model, decoding comes out about the same; the change is
the written decomposition, not new decoding.

## Remaining work, ranked by photographers unlocked per hour

Hours are Opus 5.5 agent hours, calibrated on this repo's raw PRs: RW2 every format 6.5 h (#215),
compressed RAF 8.6 h (#282), CR3 CRX 10 h (#279), NEF lossy after split 4.7 h (#633), ORF 12.8 bpp
1.7 h (#651), HEIC 7 h (#609). "Gain" is the change in the use-weighted share whose camera fully
works, from [raw/usage-weights.tsv](raw/usage-weights.tsv).

| # | Camera / variant | Gain (points of photographers) | Hours | Gain per hour | Needs |
|---:|---|---:|---:|---:|---|
| 1 | Compressed ORF (every Olympus / OM body since ~2008) | +2.6 | 8–15 | ~0.20 | CC0 samples (have 2) |
| 2 | CR3: remaining CRX variants, then verify the other 14 popular CR3 bodies | +6.2 | 25–45 | ~0.19 | samples per body (raw.pixls.us has most) |
| 3 | Verify popular Nikon, Fujifilm, Canon CR2 and Pentax bodies (one CC0 file each, fix what breaks) | +2.0 | 8–15 | ~0.17 | public samples |
| 4 | Nikon packed 14-bit under a compressed tag (Z 6II uncompressed, #769) | +0.2 | 1–2 | ~0.13 | PR #671 exists |
| 5 | Sony DSLR-A200…A390 / A850 12-bit compressed ARW | +0.4 | 3–6 | ~0.08 | samples on raw.pixls.us |
| 6 | Pre-X FinePix RAF | +0.6 | 4–8 | ~0.08 | samples |
| 7 | Sony A100 and DSC-F828 regression (#702) | +0.1 | 1–2 | ~0.07 | samples on raw.pixls.us |
| 8 | Canon CRW (2000–2004 bodies) | +1.2 by the uniform tail weight, far less by real use | 6–12 | ≤ 0.1 | samples |
| 9 | Nikon HE / HE★ NEF (Z 8, Z 9, Z 6III, Z f, Z50II, Z5II set to HE) | +1.1 | 20–40 | ~0.04 | a pure-Rust JPEG XS decoder |
| 10 | Compressed SRW, IIQ, X3F, MRW, KDC, ERF, DCR, MEF | +0.6 together | 45–85 | ~0.01 | samples; low use |

Beyond decoding:

| Gap | Hours | Needs |
|---|---:|---|
| Measured colour calibration for the ~60 popular bodies, Kelvin WB for cameras without matrices, render-fidelity suite against reference editor | 80–150 | chart shots per camera (a human with cameras) or licensable measured data |
| Lens profile database of our own (fitting tools, data format, top 100 lenses), maker-note vignetting / CA | 40–80 | calibration shots (a human with lenses) |
| Demosaic and highlight quality measured against reference editor; X-Trans AI denoise | 15–30 | |
| **Total RAW** | **255–480 h** (decoding list above 120–220 h; ~60% parallelizes) | |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-17 | minor | Hasselblad 3FR / FFF rows to ✅ (#171, #172): four CC0 files (X1D uncompressed, H3D and CFV-50c 3FR, H4D-50 FFF) pinned by sha256 in the corpus list and checked against the RGB preview in IFD0; the corpus test accepts an RGB-only preview for them. Per-model counts (Hasselblad row, medium-format bucket) not re-measured |
| 2026-10-17 | minor | Vignetting / lateral CA from maker notes (#242): a maker-neutral radial table model, the opt-in "Use camera lens corrections" develop setting and the Fujifilm vignetting table (CA decoded, not applied; Sony, Panasonic and Olympus skipped: no documented table layout), `docs/camera-lens-tables.md`; lens score unchanged |
| 2026-10-17 | minor | GoPro GPR decoded (#170): a DNG whose raw data is one VC-5 stream (SMPTE ST 2073, compression 9), in pure safe Rust ported from GoPro's reference decoder (Apache-2.0 OR MIT; NOTICE), RGGB 14-bit and GBRG 12-bit; the nine GoPro models move from unsupported to unverified (951 unverified, 27 unsupported, none of the 754 models added since 2017 unsupported); the algorithm was checked in a Python model on the five public GoPro samples (HERO5, 6, 7, 9 and Fusion) and the model's mosaic hashes are pinned in `crates/raw/tests/gpr_corpus.rs`, CI verification pending |
| 2026-10-14 | minor | Nikon D600 and D610 image area (6016 × 4016 at 8, 6) measured on 4 CC0 NEFs of each, and the D90's area and white level applied to the D5000 (same sensor and filler, 2 CC0 NEFs): the filler and noise columns on the right no longer develop as a band, and for these bodies the white level is measured inside the image area, issue upstream #1627 |
| 2026-10-13 | minor | Lens profiles: Nikon in-file distortion (NikonNEFInfo `0xc7d5`) for the Z 6II, Z 8 and Z f, validated on 83 frames from three lenses and the CC0 Z f sample (`docs/nikon-lens-corrections.md`); lens score unchanged |
| 2026-10-13 | minor | Nikon D90 image area (4288 × 2848 at 10, 10) and white level (3830) measured on 10 of 550 NEFs from one body: its 42 filler columns no longer develop as a pink band |
| 2026-10-12 | minor | Hasselblad Lunar verified (a Sony NEX-7 ARW under Make HASSELBLAD, now decoded): 236 verified, 232 preview; an uncompressed Leaf MOS is recognised as a raw (preview only) instead of opening as a greyscale image of its mosaic (upstream #1275) |
| 2026-10-12 | minor | Olympus E-10: as-shot white balance (`0x1017`/`0x1018`) and black level (`0x1012` in `0x102c` ValidBits units) from the main maker note, upstream issue #1273; ΔE76 against its thumbnail 18.5 → 11.5, camera-look fit now accepted; counts unchanged |
| 2026-10-12 | minor | Canon PowerShot G9: as-shot white balance from WBInfo (`0x0029`, the row of the preset in ShotInfo), issue #1273; render unchanged (the camera-look fit already absorbed it), As Shot now the camera's; counts unchanged |
| 2026-10-10 | minor | Compressed ORF: pointer to the two Olympus patents that describe the method, as a clean-room starting point |
| 2026-10-11 | minor | EOS R5 Mark II verified (CRX QP-map first literal and limited-length escape, odd-height Bayer planes; 4 CC0 files pinned, QP maps of 33,665 private files): 235 verified, 942 unverified; use-weighted 85% (42% on verified bodies); decoding 77%, RAW readiness unchanged at ~56% |
| 2026-10-10 (later) | major | Model-by-model measurement against reference publisher's published list (1,446 models: 234 verified, 943 unverified, 233 preview, 36 unsupported); `docs/raw/` data and re-measure script; use-weighted share 84% (40% on verified bodies); variant matrix; colour and lens sections with scores; ranked next cameras; RAW readiness ~56% from written parts |
| 2026-10-10 | major | Created: formats, makers, demosaic, colour and lens checklists; target counts from the installed reference editor 9.6 bundle listing |
