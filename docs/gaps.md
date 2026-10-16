# Gaps: where Ember falls short of reference editor

> **Last reviewed:** 2026-10-17 · **Last updated:** 2026-10-17 · **Change:** minor (gap 18: the poster frame is chosen in the player (#76))

Every known shortfall, one entry each, ranked by user impact (how many reference editor users hit it, and
how hard it blocks them), then by effort. **Agents pick work from the top** (the first entry nobody is
working on). When you close a gap, delete or shrink its entry, update its rows in
[parity-checklist.md](parity-checklist.md), and the numbers in [target-app-parity.md](target-app-parity.md)
and [ROADMAP.md](../ROADMAP.md) if they move.

Hours are Opus 5.5 agent wall-clock hours (see [target-app-parity.md](target-app-parity.md#how-hours-are-calibrated)).
`B` marks a gap that blocks **beta**.

## Ranked

**Auto corpus, 2026-10-12:** full-corpus qualification remains open. Reusable extraction now has
source support for retained before/Auto/finished pixels, colour & heuristic region diagnostics;
numeric tone labels alone do not complete corpus preparation. See [format & completion evidence](reusable-auto-corpus.md).

| # | Gap | Evidence | User impact | Hours | Doc |
|---:|---|---|---|---:|---|
| 1 B | **Camera colour is estimated, not measured** (LR-PROF-CAMERACOLOR). Raws start from a per-file fit to the camera JPEG, 4 bundled profiles or 52 spectral matrices; others use a neutral matrix (a black-and-white camera JPEG gives its tone curve only, #577; dark JPEG tones also guide the toe, #1307) [#57](https://github.com/Orthic-Labs/ember/issues/57) | ΔE vs camera JPEG 2.9–10 after fitting (raw-parity.md); 0 chart-calibrated models vs 1,500 reference publisher Standard profiles | every raw shooter sees colour that is close but not reference editor's; some get neutral-matrix colour | 50–90 | [raw-parity](raw-parity.md#colour) |
| 2 B | **CR3 beyond four verified bodies** (LR-IMP-FORMATS): `crx.rs` refuses vertical-tile C-RAW, partial-subband and other plane coding, multi-tile 0x200 and adaptive low-frequency gains, and those files fall back to the preview; CR3 sRAW / mRAW and HEIF CR3 too. Verified: M50 / R100 / R8 / R5 Mark II. EOS R7 C-RAW (odd-height QP map) decodes since a45c2b66 and R6 Mark II C-RAW is reported working on 0.6.0, but neither is verified yet (no corpus test or recorded check) [#176](https://github.com/Orthic-Labs/ember/issues/176) | measured: Canon is the largest bucket (33% of photographers) and its share whose camera fully works is the lowest of the big four, 73%; 18 of 22 popular Canon bodies write CR3, 4 verified (raw-parity.md); R7 rule checked only on paired RAW / C-RAW captures ([cr3.md](cr3.md)) | +6.2 points of photographers whose camera works, the biggest single raw gain | 25–45 | [raw-parity](raw-parity.md#remaining-work-ranked-by-photographers-unlocked-per-hour) |
| 3 B | **Render fidelity unmeasured** (LR-BEHAV-RENDER-FIDELITY): tone, highlights, texture / clarity / dehaze, NR and sharpening are tuned by eye [#90](https://github.com/Orthic-Labs/ember/issues/90) | no side-by-side suite; a retune needs a new process version (only V1 exists) | edits imported from reference editor look different; the look "isn't reference editor's" | 30–60 | [raw-parity](raw-parity.md), [process-versions](process-versions.md) |
| 4 B | **Original-pixel detail at 100%** (LR-VIEW-ZOOM, P0): Detail, Before/After, Compare and Reference are sharp at 1:1–8:1 (zoom windows, #323). Mask overlays and soft proofing render the same zoom window (soft proofing since #45, in code; CI verifies); what remains is photos whose spots or Auto Mask strokes read too far for one window (over 8192 px), which still render the whole frame at the drawn size [#45](https://github.com/Orthic-Labs/ember/issues/45) | checklist row 🟡 | editing a photo over 8192 px with far-reaching spots or Auto Mask strokes at 1:1 renders the whole frame | 2–4 | [ui-parity](ui-parity.md) |
| 5 B | **AI masks: Subject, Sky, People, Background, Landscape** are heuristics (Subject / Background now a classical saliency + region segmenter, unmeasured on real photos); Object / Describe run SAM 3 but its download has no mirror configured [#177](https://github.com/Orthic-Labs/ember/issues/177) | `crates/segment`; ai-masks.md; MASK rows 67% | masking is how most reference editor users edit today | 40–70 + model decision | [target-app-parity](target-app-parity.md#ai) |
| 6 B | **Compressed ORF** (every Olympus / OM body since ~2008) [#178](https://github.com/Orthic-Labs/ember/issues/178) | measured: 66 of 78 Olympus / OM models preview-only; `orf.rs` refuses; OM bucket 3% of photographers, 3% of them fully work | +2.6 points; best gain per hour of any raw gap (~0.2) | 8–15 | [raw-parity](raw-parity.md#compressed-orf-a-clean-room-starting-point) (the two Olympus patents that describe the method) |
| 7 | **Nikon High Efficiency NEF** (HE / HE★ on Z 8, Z 9, Z 6III, Z f, Z50II, Z5II when set to HE) [#179](https://github.com/Orthic-Labs/ember/issues/179) | named and preview only (#193); needs a pure-Rust JPEG XS decoder | +1.1 points (only shots taken in HE; lossless works on the same bodies); low gain per hour (~0.04), so not a beta blocker | 20–40 | [raw-parity](raw-parity.md) |
| 8 B | **No lens profile database** (LR-EDIT-OPTICS-PROFILE): only DNG opcodes, RW2 in-file distortion, Sony ILCE-7RM4A distortion and Nikon Z 6II / Z 8 / Z f distortion (`docs/nikon-lens-corrections.md`) [#53](https://github.com/Orthic-Labs/ember/issues/53) | reference publisher ships 3,635 lens profiles; we never use them | distortion / vignetting correction is a default step for many | 40–80 + calibration shots | [raw-parity](raw-parity.md#lens-profiles) |
| 9 B | **Tests don't run in CI on macOS, Windows or Linux x64**: only FreeBSD runs `cargo test`; the others build packages [#180](https://github.com/Orthic-Labs/ember/issues/180) | `.github/workflows/` (freebsd, packaging-lint, release, windows-arm64); the linux-riscv64 release job added in 0.6.0 (#844) runs only `lightcraft-cli --version` under QEMU | regressions reach users on the main platforms; `cargo run -p xtask -- ci` runs only locally | 6–12 | [target-app-parity](target-app-parity.md#by-dimension) |
| 10 B | **Per-model verification thin, plus cheap per-body decode fixes**: 236 of 1,446 reference publisher models verified (16%), ~42% of photographers on a verified body; Nikon 5 bodies verified, Canon 12. Cheap fixes: Z 6II packed 14-bit NEF (#769, PR #671), Sony A100 / F828 regression (#702), DSLR-A200…A390 / A850 12-bit ARW (#535) [#181](https://github.com/Orthic-Labs/ember/issues/181) | measured: [raw/coverage.tsv](raw/coverage.tsv) | per-body bugs (levels, crops, WB) surface only from users | 25–45 | [raw-parity](raw-parity.md#by-maker-measured) |
| 11 | **Enhance: Raw Details, Super Resolution; AI denoise without shippable weights or X-Trans** [#182](https://github.com/Orthic-Labs/ember/issues/182) | Raw Details demosaic ships for Bayer as an opt-in toggle (#51; X-Trans and the web build keep the usual path); Super Resolution 0%; denoise Bayer only, RawNIND weights GPL and opt-in | Fujifilm users get no AI denoise; nobody gets it out of the box | 30–60 + model decision | [raw-parity](raw-parity.md#demosaic-levels-crops-and-lens-metadata) |
| 12 | **Content-aware / generative remove, detect distractions, reflections removal** [#59](https://github.com/Orthic-Labs/ember/issues/59) | REM rows 75%; patch synthesis shipped for Remove and Fill (#59); generative fill, detect distractions and reflections removal missing | cleanup work goes back to Photoshop | 25–45 (+ generative model) | [target-app-parity](target-app-parity.md#ai) |
| 13 | **reference editor catalog migration is one-way and approximate**: smart collections whose rules map, history and snapshots come over; publish-service and AI (detected face) data are named in the import report, not imported [#220](https://github.com/Orthic-Labs/ember/issues/220) | lightroom-catalog-import.md | switching users lose some looks | 8–12 | [file-format-parity](file-format-parity.md) |
| 14 | **HDR display** (EDR / Windows HDR); the headroom limit and SDR rendition from `crs` XMP (the HDR switch, `HDREditMode`, is read). HLG AVIF export done; View ▸ HDR Display, the macOS EDR headroom query and the SDR/HDR decision are tested; blocked on eframe's 8-bit surface (no hook for the surface format or colour space) and, on Windows, on a safe DXGI HDR query [#46](https://github.com/Orthic-Labs/ember/issues/46) | HDR rows 70%; LR-VIEW-HDR-DISPLAY 🟡 | HDR editing works blind on HDR monitors | 12–25 | [hardware-parity](hardware-parity.md) |
| 15 | **Print module**: OS printer output still missing (refused with an explicit error; no platform print API in egui) [#138](https://github.com/Orthic-Labs/ember/issues/138) | LRC-PRINT-JOB row | Classic users who print straight to a printer | 25–40 | [target-app-parity](target-app-parity.md#features) |
| 16 | **Book, publish services; Map extras** (the Map view itself landed: [#114](https://github.com/Orthic-Labs/ember/issues/114); the Slideshow module landed: [#129](https://github.com/Orthic-Labs/ember/issues/129)–[#137](https://github.com/Orthic-Labs/ember/issues/137)) | LRC-BOOK rows ⬜; LR-MAP-MODULE ✅ ([#83](https://github.com/Orthic-Labs/ember/issues/83)) | Classic users; publish services matter for Flickr / SmugMug workflows | 60–100 | [target-app-parity](target-app-parity.md#features) |
| 17 | **Tethered capture** [#106](https://github.com/Orthic-Labs/ember/issues/106) | ⬜ | studio photographers | 25–45 + cameras | [hardware-parity](hardware-parity.md) |
| 18 | **Video**: audio, the rest of trim (scrubber handles), global edits, re-encoded export (needs an approved sidecar build with an H.264 encoder); silent playback and the poster chooser landed [#73](https://github.com/Orthic-Labs/ember/issues/73) | VID rows: playback 🟡 (silent), trim 🟡 ([#74](https://github.com/Orthic-Labs/ember/issues/74)), export 🟡 ([#77](https://github.com/Orthic-Labs/ember/issues/77)), cover ✅ ([#76](https://github.com/Orthic-Labs/ember/issues/76)), the rest ⬜ | users who mix stills and clips | 20–38 | [target-app-parity](target-app-parity.md#features) |
| 19 | **Plug-in ecosystem**: no reference editor SDK equivalent (export / publish plug-ins, metadata plug-ins) [#183](https://github.com/Orthic-Labs/ember/issues/183) | ⬜ | pro workflows built on plug-ins (LR/Enfuse, Jeffrey's exporters, Negative Lab Pro) | 40–80 + owner decision | [target-app-parity](target-app-parity.md#by-dimension) |
| 20 | **Long-tail raw formats**: compressed SRW, ARQ pixel shift, IIQ, Hasselblad compression, X3F, CRW, MRW, ERF, KDC, DCR, MEF, old FinePix [#184](https://github.com/Orthic-Labs/ember/issues/184) | raw-parity.md | small user share each | 45–85 | [raw-parity](raw-parity.md) |
| 21 | **Localization**: 9 of 15 shipped languages partial (74–99% of catalog messages), lower-layer errors English; Hindi, Arabic, Indonesian, Korean, Vietnamese absent; no RTL or complex shaping [#185](https://github.com/Orthic-Labs/ember/issues/185) | measured from catalogs | non-English users see mixed English | 55–90 (shipped to full); 145–255 (five new) | [localization-parity](localization-parity.md) |
| 22 | **Pen pressure**, OS monitor profiles, follow-window colour [#186](https://github.com/Orthic-Labs/ember/issues/186) | hardware-parity.md | brush users with tablets; multi-monitor setups | 11–22 | [hardware-parity](hardware-parity.md) |
| 23 | **Accessibility** audit with screen readers; canvas tools keyboard-reachable [#92](https://github.com/Orthic-Labs/ember/issues/92) | LR-BEHAV-ACCESS 🟡 | blind and low-vision users | 10–20 | [ui-parity](ui-parity.md) |
| 24 | **Brush live preview and masking feel** (#517, #518 landed); Feather and Refine Edges landed as mask parameters (`mask.feather`, `mask.refine`) and run on the GPU (`feather_refine` in `crates/gpu/src/render.rs`, kernels `xguided_q` / `mask_blend`; the device test `mask_feather_and_refine_match_cpu` checks them against the CPU within 2e-3 and skips without an adapter; CI verifies) (15.5) [#187](https://github.com/Orthic-Labs/ember/issues/187) | issues | masking feels slower than reference editor | 8–12 | [ui-parity](ui-parity.md) |
| 25 | **File writes**: lossy DNG; AVIF import and Render to AVIF are in code and unverified until CI runs, AVIF is still missing on the web build (Render to DNG and the AVIF export format landed) (PSD export landed with Edit in PhotoCraft, #953) [#188](https://github.com/Orthic-Labs/ember/issues/188) | file-format-parity.md | interchange edge cases | 19–34 | [file-format-parity](file-format-parity.md) |
| 26 | **Performance at scale**: 85k-photo library opens in 1.7–4.7 s; 100k grid: a headless test on a 100k-photo catalog asserts that unchanged frames rebuild nothing and visit only the rows near the screen (`a_100k_photo_grid_visits_only_the_visible_rows`), frame-time numbers still to record (`grid_frame_100k`, ignored); colour NR at half resolution landed in code (V3; measure with `LIGHTCRAFT_PROFILE=1`, `nr colour` / `nr_col` stages); the GPU histogram is counted on the device in code (`crates/gpu/src/histogram.rs`, CPU fallback; CI verifies the CPU-oracle test) [#189](https://github.com/Orthic-Labs/ember/issues/189) | ROADMAP log, gpu-pipeline.md | large-catalog users | 15–30 | [target-app-parity](target-app-parity.md#by-dimension) |
| 27 | **Windows / Linux runtime coverage**: Windows installer UI unverified on Windows (#79); GPU path proven on Apple only; the Linux riscv64 tarball (new in 0.6.0, #844) is cross-compiled and never run beyond a QEMU smoke test [#190](https://github.com/Orthic-Labs/ember/issues/190) | ROADMAP, issues, `release.yml` | non-Mac users find bugs first | 15–30 | [hardware-parity](hardware-parity.md) |
| 28 | **Small Classic shortcuts and keys**: flag cycle has no key (`photo.cycleFlag`); desktop app lacks F5 top bar and `\` filter bar; ⇧Tab and T (toolbar) missing [#146](https://github.com/Orthic-Labs/ember/issues/146) | [ui-parity](ui-parity.md#shortcuts) | Classic muscle memory | 4–8 | [ui-parity](ui-parity.md) |
| 29 | **Depth range: embedded depth maps only, unverified on real files**: Depth Range selects on the depth map a Pixel / Android portrait JPEG (Dynamic Depth, GDepth) or an iPhone portrait HEIC embeds; it has been tried on synthetic files only, the HEIC path needs the `heif` feature, no depth is estimated for photos without one, and Lens Blur does not use the map yet [#64](https://github.com/Orthic-Labs/ember/issues/64) | checklist row 🟡 | masking by distance works on phone portrait photos only; other photos get a note instead of a selection | 4–8 | [lens-blur](lens-blur.md) |

Beta-blocking gaps (B) total **~230–430 h**; with the stability and platform work they need around them,
**~340–600 h** to beta (incl. 40–80 h for the ~92 open core-path user issues) (see [target-app-parity.md](target-app-parity.md#stage)).

## Gaps by kind

- **Feature:** 5, 8, 11, 12, 13, 15, 16, 18, 19, 29
- **UI / UX:** 4, 23, 24, 28
- **File format:** 2, 6, 7, 10, 13, 20, 25 (raw ones in [raw-parity.md](raw-parity.md))
- **Hardware:** 14, 17, 22, 27
- **Localization:** 21
- **Quality / fidelity:** 1, 3
- **Stability and platforms:** 9, 26, 27

## Corpus preparation

2026-10-13: bounded grayscale TIFF/JPEG XL CLI extraction preserves integer planes for private
regional corpus preparation. Store identity, raster registration, semantic labels & full-corpus
loader qualification remain separate evidence; AI gaps stay open. See [corpus contract](reusable-auto-corpus.md). [#191](https://github.com/Orthic-Labs/ember/issues/191)

## Closed recently (kept for calibration)

- 2026-10-10: Nikon lossy-after-split NEF (#633), ORF 12.8 bpp (#651), HDR gain map / PQ AVIF / float
  TIFF export (#646), activity stack (#345), JPEG XL export (#247).
- 2026-10-09: EOS R7 C-RAW with an odd-height QP map (a45c2b66; part of gap 2), HEIC in release builds (#609), Samsung uncompressed SRW (#459), contact-sheet PDF (#563),
  Ukrainian UI (#564), Sony ILCE-7CR profile.
- 2026-10-08: CR3 CRX lossless and C-RAW (#279), Fujifilm compressed RAF (#282), AI RAW denoise (#384),
  reference editor catalog import (#218), German, Spanish, Russian UI.
- 2026-10-07: RW2 every format (#215), faces recognition in pure Rust (#174).
- 2026-10-05: compressed NEF (#86). The older history is in [ROADMAP.md](../ROADMAP.md#progress-log).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-17 | minor | Gap 18: the poster frame is chosen in the player (#76) |
| 2026-10-17 | minor | Gap 18 shrinks: silent playback in the loupe (#73), playback held to the trim; audio, scrubber trim handles, edits and re-encoded export remain |
| 2026-10-17 | minor | Gap 18: video export (`app.export {format: video}`: stream copy cut to the trim, re-encode argument building for `h264_mf` / `h264_videotoolbox`, one frame as a still) is in code (#77); a re-encode needs a sidecar build with an H.264 encoder, which the approved `decode` build lacks |
| 2026-10-17 | minor | Gap 18: non-destructive trim (`video.setTrim`, `video.clearTrim`, a Trim row in the loupe) is in code (#74); scrubber handles wait for the player, so playback, edits and export remain |
| 2026-10-17 | minor | Gap 14: LR-VIEW-HDR-DISPLAY is 🟡 (toggle, decision and macOS headroom exist and are tested); still always SDR on screen |
| 2026-10-17 | minor | Gap 25: AVIF import (`crates/codecs/src/avif.rs`, rav1d behind the isolated `crates/av1dec` crate) and `photo.renderToAvif` are in code (#188); unverified until CI runs, web build still without AVIF decode |
| 2026-10-17 | minor | Gap 13: catalog import carries smart collections whose rules map (as smart albums) and develop history and snapshots; publish-service and AI data still not imported, and the import report lists them |
| 2026-10-17 | minor | Gap 15: print guides, user print templates, the job resolution as the export default, the embedded sRGB profile and print progress and cancel are in code; OS printer output remains |
| 2026-10-17 | minor | Gap 16: the Classic Slideshow module (#129-#137): templates, layout and guides, options, overlays, backdrop, titles, music fit, playback and PDF/JPEG export; Book and publish services still open
| 2026-10-17 | minor | Gap 11: Photo ▸ Enhance… (#68, #69, #98) sets AI Denoise and Raw Details on the photo in one undo step; Super Resolution still 0% (shown disabled) |
| 2026-10-16 | minor | Gap 11 narrows: Detail ▸ Raw Details turns on a higher-quality Bayer demosaic (opt-in, off for every existing edit; X-Trans still to do) |
| 2026-10-16 | minor | Gap 14: View ▸ HDR Display and the macOS EDR headroom query land; the display is still SDR until the surface can be HDR |
| 2026-10-13 | minor | Gap 8 narrows: Nikon Z 6II / Z 8 / Z f NEFs use the camera's in-file distortion correction (`docs/nikon-lens-corrections.md`) |
| 2026-10-13 | minor | Bounded grayscale raster extractor added for corpus preparation; no AI quality gap closed |
| 2026-10-12 | minor | Gap 14: HLG AVIF export (`hdrTransfer`) and a tested SDR/HDR display decision |
| 2026-10-12 | minor | Gap 25: Render to DNG lands as an export format; lossy DNG and AVIF import remain |
| 2026-10-12 | minor | Gap 10 shrinks: the Hasselblad Lunar (a Sony NEX-7 ARW under its own Make) decodes and is verified, 236 models (upstream #1275) |
| 2026-10-12 | minor | Camera-look deep shadows follow the JPEG toe (upstream #1307); camera calibration remains partial |
| 2026-10-11 | minor | Reviewed against main after 0.6.0: CR3 entry names the CRX variants `crx.rs` still refuses and the R7 / R6 Mark II C-RAW state (decoding, not verified, so the measured numbers don't move); CI and Windows / Linux entries note the riscv64 release build (#844); R7 fix added to Closed recently. Panel docking (UI.9, #869) closes no listed gap |
| 2026-10-11 | minor | Camera colour entry: raws with a black-and-white camera JPEG take its tone curve (#577); gap not closed |
| 2026-10-16 | minor | Gap 4: soft proofing gets the zoom window (mask overlays had it); only the spot / Auto Mask read limit remains, 4–8 h → 2–4 h (#45) |
| 2026-10-11 | minor | Gap 4: Compare and Reference views get zoom windows per pane (1:1 was a 2,560 px render magnified); overlays and soft proofing remain, 8–15 h → 4–8 h |
| 2026-10-10 | minor | Compressed ORF entry links the two Olympus patents that describe the method |
| 2026-10-11 | minor | Gap 14: `crs:HDREditMode` is read from XMP (#663); the headroom limit and SDR rendition still aren't |
| 2026-10-17 | minor | Gap 26: the 100k-photo grid has a headless test (visible rows only, layout and list built once per view); timings still unrecorded (#189) |
| 2026-10-11 | minor | Gaps 2 and 10 shrink: EOS R5 Mark II CR3 decodes and is verified (fourth CR3 body, Canon 12 verified, 235 models, ~42% on a verified body); CR3 gain +6.5 → +6.2 points |
| 2026-10-10 | minor | RAW gaps re-measured model by model (raw-parity.md): CR3, compressed ORF and verification entries carry measured shares; HE NEF no longer a beta blocker (+1.1 points for 20–40 h); verification entry absorbs the cheap per-body fixes (#769, #702, A200 series) |
| 2026-10-10 | minor | Beta total raised to 340–600 h to include the ~92 open core-path user issues |
| 2026-10-10 | major | Created from the checklist's Top gaps (whose items 1–2 had been duplicated by a merge) and ROADMAP's Where we stand; re-ranked; added evidence, impact, hours and beta flags; added platform-CI, lens-database, HE NEF and localization gaps |
