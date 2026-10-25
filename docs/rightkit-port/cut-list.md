# Ember on RightKit: cut list

> **Last updated:** 2026-10-24. Revision: first version (port lane, Orthic-Labs/ember#240).

Ember is a fork of storytold/lightcraft moved onto RightKit, not rewritten. This is the second step of Adrian's order (foundation, **cut list**, add-ons, new packages): every crate under `crates/`, every app under `apps/`, and the support trees, with a verdict, its size and the reason.

No code is deleted for a `drop` row. Each one is an issue for Adrian's call (linked in the table). The few deletions this branch does make are `replace` rows whose RightKit owner already exists; they are listed under "What this branch changed".

## Verdicts

| Verdict | Meaning |
|---|---|
| **keep** | Ember's own product code. `(upstream-synced)`: a survivor that keeps its `lightcraft-*` name and keeps absorbing upstream fixes on `main`. |
| **replace** | A RightKit owner exists (named). The Ember copy goes when the port is done; until then it sits in `.rightkit/ownership-exceptions.json` with a RightKit issue and an expiry. |
| **drop** | Not necessary for a photo library plus RAW editor, or upstream breadth. Adrian decides (issue linked). |
| **later** | The own-model area (denoise, culling, auto-edit). Training stays product tooling; distribution and verified load go through `rightkit-models` and the model-artifact lane (bogusyogi/rightkit#1161). |

## Method

- Sizes are `wc -l` of tracked files at this branch: **src** is `crates/<c>/src/**/*.rs`, **other** is the same crate's tests, examples, benches and `build.rs`. Rows that start "slice of" are parts of a parent row and are never added into the totals.
- The verdict rows below partition the Rust tree exactly: 331,748 src and 20,679 other lines across `crates/`, `apps/` and `xtask/` (checked by summing the table).
- Sources: the unit's own `Cargo.toml` and README, `docs/gaps.md`, `ROADMAP.md`, `NOTICE`, the 2026-10-24 Ember audit (`ember-apple-audit.json`) and port order, and the published RightKit crates (read as the `.crate` files from crates.io, not from the RightKit working tree).
- Nothing here was compiled or run (cloud lane); the hosts compile through the broker.

## The table

### keep (product)

| Unit | Lines (src / other) | Verdict | Reason | Evidence |
|---|---|---|---|---|
| `apps/lightcraft-desktop` | 2,509 / 3,808 rs; 7,229 ts/tsx/css | keep | The shipped host on macOS and Windows: Tauri 2 and React over the engine session. Window host, settings, device identity, logs and updates already sit on RightKit owners; what stays is listed in `.rightkit/ownership-exceptions.json`. | `docs/desktop-migration.md`; `apps/lightcraft-desktop/Cargo.toml:15-27` |
| `crates/desktop-host` | 6,172 / 0 | keep | Owner-thread session host: `tasks.rs` (2,080 lines) drives import, merge and cull jobs over engine activity rows, plus render, preview, snapshot and auto-import. Host orchestration, not a registry. | `crates/desktop-host/src/tasks.rs:1-40` |
| `apps/lightcraft-cli` (render, commands, snapshot, MCP launch) | 1,252 / 2,008 | keep | Headless render, command listing and the MCP entry; signed and shipped in the bundle; it is the agent and QA tool. | `right-release.config.mjs:49,65`; `scripts/desktop/finalize-candidate.mjs:32` |
| `xtask` | 2,873 / 0 | keep | CI gates: fmt, clippy, tests, parity, layers, assets, wasm. `web.rs` (352) goes with the web row. | `AGENTS.md` quality gates |
| `crates/sysmem` | 338 / 0 | keep | Allocator pressure relief, physical memory, window monitor profile: one of the three places with `unsafe`. No catalogued owner; proposed as `rightkit-sysmem` (N10, bogusyogi/rightkit#1160). | `AGENTS.md` "Never crash" |
| `crates/segment`: mask-model modules (mobilesam, vit, clip, tokenizer, detector, tracker, remote) | 3,332 / 260 | keep | AI masks (`docs/gaps.md` gap 5): a product feature behind engine feature `sam`, on in the desktop host; weights are downloaded on request. `remote.rs` frame code is a replace row below. | `crates/engine/Cargo.toml:46,63` |

### keep (upstream-synced)

| Unit | Lines (src / other) | Verdict | Reason | Evidence |
|---|---|---|---|---|
| `crates/engine` | 86,434 / 1,054 | keep | Session, command registry, history, import/export, Lightroom import, culling, Personal Auto. Carries the replace and drop slices below (not added into totals). | `crates/engine/Cargo.toml:13-50` |
| `crates/raw` | 31,568 / 3,736 | keep | RAW decoders, demosaic, DNG colour model. Gaps 1, 2, 6, 7 and 10 live here, so it takes the most upstream fixes. No RightKit owner: N3, decision D1 (bogusyogi/rightkit#1158). | `docs/gaps.md` |
| `crates/pipeline` | 20,282 / 37 | keep | The develop pipeline (CPU reference). Hard dependency on `inpaint` (`spots.rs:352`). | `crates/pipeline/Cargo.toml:17` |
| `crates/catalog` | 12,207 / 216 | keep | Library model: ops, rules, keywords, albums, stacks, journal (format VERSION 10). Take catalog changes per feature, never by cherry-pick (upstream reuses format numbers). The journal core (1,143 lines) is candidate N8. | `crates/catalog/src/journal.rs:38-80` |
| `crates/codecs` | 8,892 / 2,022 | keep | Sniff, decode to linear light with ICC, encode with metadata. Default features: parallel, jxl, avif (avif pulls `av1dec`). | `crates/codecs/Cargo.toml:15,36,41,51` |
| `crates/meta` | 5,577 / 53 | keep | EXIF, XMP read and write, IPTC (N2). | `crates/meta/README.md` |
| `crates/gpu` | 5,359 / 1,178 | keep | wgpu compute twin of the CPU pipeline. Its device context, backend policy, crash sentinel, exit gate and memory budget (1,976 lines) are a separable block (N7, bogusyogi/rightkit#1159). | `crates/gpu/src/backend.rs:1-8` |
| `crates/develop` | 3,363 / 0 | keep | The non-destructive edit model: settings and control specs. Product domain. | `crates/develop/src/lib.rs:1-5` |
| `crates/heif` | 2,246 / 0 | keep | HEIC/HEIF decode; optional feature, off by default (N6). | `crates/codecs/Cargo.toml:15,45-48` |
| `crates/preview` | 1,930 / 0 | keep | Memory LRU, prioritised job pool, disk thumbnail cache. The disk store is `rightkit-cache` (pin moved to 0.2.3 in this branch). | `crates/preview/Cargo.toml:21` |
| `crates/geom` | 1,705 / 0 | keep | Points, transforms, the crop model; 15 crates depend on it. Product domain. | `crates/geom/src/lib.rs:1-5` |
| `crates/tiff` | 1,597 / 429 | keep | TIFF/IFD reader and writer (N1). | `crates/*/Cargo.toml` |
| `crates/lensdb` | 1,059 / 0 | keep | Lens distortion, TCA and vignetting profiles from a lensfun-format directory the user supplies (N5). | `crates/lensdb/src/lib.rs:1-12` |
| `crates/raster` | 1,020 / 42 | keep | Image buffers, resampling, blur, histogram; 20 crates depend on it (N11). | `crates/raster/src/lib.rs:1-9` |
| `crates/color` | 889 / 0 | keep | Colour spaces, Bradford adaptation, CCT/tint, transfer functions, OkLab, splines (N4). The 28 `theme.math.relative-luminance` scanner hits in the tree are Rec.709 luma weights, not UI colour maths. | `crates/color/src/lib.rs:1-10` |

### replace (a RightKit owner exists)

| Unit | Lines (src / other) | Verdict | Owner and state | Reason | Evidence and issue |
|---|---|---|---|---|---|
| `crates/mcp` | 2,751 / 318 | replace (partial) | `rightkit-mcp` `ServerHost` 0.7.0; `rightkit-crypto` 0.1.0 | **Done here:** `base64.rs` (60 lines) and the local `ct_eq` (10) are deleted for `rightkit_crypto::{base64_encode, base64_decode, ct_eq}`. **Not done:** the server loop, `server.rs` (262) and `progress.rs` (165). Ember's `Server` is synchronous (`handle_line`, `handle`), driven by the crate's unit tests and by `tests/e2e.rs`, and implements batches, three protocol revisions, `_meta.progressToken` progress and cancel suppression; the owner host is concurrent with its own tool-provider, batch-policy and validation model, so the swap needs the e2e run on a host. Tool schemas, backends, the headless session and control auth (about 1,800 lines) stay. | `crates/mcp/src/server.rs:1-140`; bogusyogi/rightkit#283 (ledger entries for `server.rs` and `progress.rs`) |
| `crates/fetch` | 1,509 / 0 | replace (adapter shrinks) | `rightkit-models` 0.5.1; `rightkit-http` 0.3.8 | `lib.rs` (508) already runs the transfer on the owner's `download_resumable`; what it adds is the mirror loop and cap (the owner has `FileSpec::mirrors`, `download_resumable_capped`, `ErrorCode::AllMirrorsFailed` since 0.4.4), the bearer token scoped to one host and progress across files (owner gaps). `http.rs` (506) keeps Ember's `Url`/`Limits`/`get`/`post_json` surface over `rightkit-http`; its callers are map tiles and photo-ai, so it shrinks with those rows. | bogusyogi/rightkit#1164 (models); Orthic-Labs/ember#256 (http) |
| slice of `crates/engine`: `src/activity.rs` | 438 / 0 | replace | `rightkit-job-queue` `InlineCancelRegistry` 0.5.1 | The activity registry. Ember's rows add `done`/`total` counts, a unit, a kind, `set_cancellable` and numeric ids; the owner row has label, message, a 0..1 fraction and string ids. `desktop-host/src/tasks.rs` stays. | `crates/engine/src/activity.rs:21-60,234-252`; bogusyogi/rightkit#1162 |
| slice of `apps/lightcraft-desktop`: `src/services.rs` `open_url` | 23 | replace | `rightkit-child-process` `open_url` 0.5.6 | **Done here:** the hand-rolled opener, its bounded-wait helper and constants are deleted; the https-only rule stays at the call site (it is Ember's policy). On Windows the owner uses `rundll32 url.dll,FileProtocolHandler` where Ember used `explorer`; the egui host already called the owner. | `apps/lightcraft/src/main.rs:475-481` |
| slice of `apps/lightcraft-desktop`: QA viewport helper in `src/native.rs` | 74 | replace | `rightkit-qa` `embedded::set_viewport` 0.9.0 | **Done here:** `qa_viewport` and the `lc_qa_viewport` command (qa-native only) had no caller; the native journeys call `Control::set_viewport` and read geometry back through the shell's `windows::readback`. Retires the M1 section 2 exception. | `apps/lightcraft-desktop/tests/native_qualification.rs:643` |
| slice of `apps/lightcraft-desktop`: `src/rk_logs.rs` | 131 | replace | `rightkit-tauri-shell` `logs` feature 0.6.0 | Ember copies the shell's three `rightkit_logs_*` commands: it opens its own `JsonlStore`, which its `log` bridge also writes, while the shell's `LogsRuntime` builds a store of its own (no injection point). The move needs the bridge to append through `LogsRuntime::store()` and a record path before the runtime is installed (#1055); the egui host's `RotatingTextWriter` is a different file. | bogusyogi/rightkit#1027, #1055 |
| slice of `crates/segment`: `src/remote.rs` frames | about 20 | replace (blocked) | `rightkit-sidecar` `length_prefixed` 0.3.0 | Same 4-byte big-endian frame, but the owner refuses a zero-length frame and Ember's click-only SAM request sends one (`width` 0). Read, not ported. | `crates/segment/src/remote.rs:58-72,161`; bogusyogi/rightkit#1163 |
| slice of `crates/develop`: `src/segmask.rs` base64 | 36 | replace | `rightkit-crypto` 0.1.0 | The mask payload is persisted; the owner's decoder is strict (padded only) where Ember's accepts both forms, so the swap waits for a stored-form check. | `crates/develop/src/segmask.rs:86-121`; Orthic-Labs/ember#257 |
| slice of `crates/video`: `src/sidecar.rs` | 254 | replace (deferred) | `rightkit-ffmpeg` 0.2.10 `resolve_ffmpeg` with `ResolutionRequest.bundled_paths` | Not an owner gap: the verification already runs on `verify_bundled_ffmpeg` and `verify_bundled_ffprobe`, and what stays is Ember's bundle-layout directory search. The owner closed the `bundle_dir` request (bogusyogi/rightkit#1015) as not planned because the published API takes caller-named paths. Only matters if the video row is kept. | `crates/video/src/sidecar.rs:73-118`; Orthic-Labs/ember#267 |
| slice of `crates/denoise-onnx`: `src/runner.rs` `ensure_runtime` | about 70 of 178 | replace (blocked) | `rightkit-onnx` `bind_installed_runtime` (planned) | Find, verify and bind the ONNX Runtime library once per process. Optional feature `onnx-accel`, off by default. | `crates/denoise-onnx/src/runner.rs:36-58`; bogusyogi/rightkit#1165 |
| slice of `crates/photo-ai`: `src/network.rs`, `src/scaledown.rs` | 155 + 79 | replace | `rightkit-llm` 0.3.1 | OpenRouter transport; an own-model ("later") crate, scheduled with the cloud-comparison work. | Orthic-Labs/ember#258 |
| slice of `scripts/desktop`: `static-check.mjs` CSP and capability assertions | 156 | replace (blocked) | `@rightkit/release` doctor rule | Hand-written Tauri CSP and capability assertions; the shared drift check is requested. | bogusyogi/rightkit#1136 |
| slice of `crates/ui-egui`: `i18n.rs`, `build.rs`, 24 locale files, `theme.rs` | 1,432 + 208 + 687 | replace (blocked) | `rightkit-i18n` and theme tokens (not published) | The egui locale table, its codegen and the token palette. Moot if ui-egui is dropped. | bogusyogi/rightkit#960; Orthic-Labs/ember#269 |
| slice of `ios/`: Swift helpers (A15 to A19) | about 150 | replace (conditional) | RightKitSwift diagnostics, store, bookmarks, import queue (0.9.0 tag) | Only if the iOS row is kept. | Orthic-Labs/ember#260 |

The editor settings screen (`emberSettingsBackend.ts` and `EmberSettings.tsx`, 651 lines) is deliberately not a replace row: `emberSettingsBackend.ts` implements the `SettingsBackend` port of `@rightkit/settings-ui` 0.7.0 and `EmberSettings.tsx` is Ember's own settings presentation. Whether `settings-ui`'s shell adapter over the shell's `rightkit_settings_*` host commands fits Ember's preference schema is not verified.

### later (own-model area)

| Unit | Lines (src / other) | Verdict | Reason | Evidence |
|---|---|---|---|---|
| `crates/denoise` | 3,196 / 250 | later | Denoise manifests, Bayer tiling, CPU runner, `.lcdn` cache. The Tauri app does not enable engine feature `denoise`; the download catalog is empty. Runtime package N9. | `crates/engine/Cargo.toml:26,64` |
| `crates/denoise-core` | 2,281 / 0 | later | Model metadata, archive and cache formats, Bayer packing; the engine depends on it directly. | `crates/engine/Cargo.toml:25` |
| `crates/denoise-onnx` | 315 / 0 | later | Optional ONNX Runtime tile runner behind `onnx-accel`; already on `rightkit-onnx` (0.2.7; the rc.14 move needed no code change, Orthic-Labs/ember#230). | `crates/denoise-onnx/Cargo.toml:15,23` |
| `crates/nafnet` | 830 / 0 | later | The network definition shared by the product denoiser and the trainer. | `crates/nafnet/src/lib.rs:1-6` |
| `crates/denoise-train` | 3,096 / 0 | later | Training tooling, not product code; nothing depends on it. Stays in Ember. | `crates/denoise-train/src/lib.rs:1-7` |
| `crates/photo-ai` | 1,648 / 0 | later | Typed read-only cloud photo assessment experiments (culling, search, schemas). | `crates/photo-ai/README.md` |
| `crates/segment`: culling-model modules (yunet, dinov2, mediapipe) | 14,430 / 3,685 | later | Experimental local culling models in candle: 81% of the crate's source, called only by four `examples/*_qualify.rs` files. Candidate for N9 or a fork-only crate; no upstream sync. | `crates/segment/src/lib.rs:44-99` |
| `apps/lightcraft-cli`: evaluation tooling | 6,976 / 0 | later | The measurement harness for own models and Personal Auto (`cull_eval`, `personal_auto_eval`, `decision_*`, `denoise_eval`). `decision_extract/geometry.rs` imports `lightcraft_merge` (see the merge row). | `apps/lightcraft-cli/src/*.rs` |

### drop (Adrian decides)

| Unit | Lines (src / other) | Verdict | Reason | Evidence and issue |
|---|---|---|---|---|
| `crates/ui-egui` | 69,137 / 886 | drop | The upstream egui interface; the shipped product does not link it. The largest single cut. A drop ends upstream UI fixes, the Linux and FreeBSD host, the egui driving workflow and `lightcraft-cli snapshot`. | `apps/lightcraft-desktop/Cargo.toml:21-27`; Orthic-Labs/ember#269 |
| `apps/lightcraft` (egui host) | 4,694 / 73 | drop (with ui-egui) | The egui desktop app with the loopback control server behind the M4/M6/M7 not-applicable rulings. | `apps/lightcraft/src/control_server.rs:46-53`; Orthic-Labs/ember#269 |
| `apps/lightcraft-web` | 5,874 / 0 (+15 js) | drop | WASM build of the egui UI; nothing depends on it. `xtask/src/web.rs` and the wasm gate go with it. | `apps/lightcraft-web/Cargo.toml:18`; Orthic-Labs/ember#261 |
| `crates/faces` | 5,511 / 0 | drop | Face detection and recognition (People). Engine slice: `faces_*`, `face_download`, `cmd/face_*` = 3,638 lines plus 1,426 tests. | `crates/engine/Cargo.toml:31`; Orthic-Labs/ember#263 |
| `crates/merge` | 3,482 / 410 | drop (keep registration primitives) | HDR merge and panorama. Not a pure drop: the CLI's Personal Auto extraction imports `align`, `features`, `ransac` (about 993 lines). Engine and host slice: 908 lines plus 322 tests. | `apps/lightcraft-cli/src/decision_extract/geometry.rs:3`; Orthic-Labs/ember#266 |
| `crates/video` | 1,923 / 0 | drop (cheap to keep) | Probe and poster frames through `rightkit-ffmpeg`. Engine slice: 1,188 lines plus 750 tests. | `crates/engine/Cargo.toml:35`; Orthic-Labs/ember#267 |
| `crates/scenes` | 1,002 / 29 | drop (keep as dev fixtures) | Procedural demo scenes; product use is only `--demo`. Referenced from 20 files outside the crate, mostly tests. | `crates/engine/src/demo.rs`; Orthic-Labs/ember#265 |
| `crates/av1dec` | 936 / 108 | drop | AVIF import through rav1d in a signed child worker; import is unverified (gap 25). A drop removes a signed binary, an `unsafe` FFI crate and the one fact against the M3 ruling. | `right-release.config.mjs:49,65`; Orthic-Labs/ember#259 |
| `crates/inpaint` | 845 / 0 | drop | PatchMatch content-aware fill for Remove (gap 12); one call site. | `crates/pipeline/src/spots.rs:352`; Orthic-Labs/ember#264 |
| `crates/geocode` | 396 / 0 | drop | Offline reverse geocoding from a GeoNames dump. With the map and places engine slice: 2,686 lines. | `crates/engine/Cargo.toml:34`; Orthic-Labs/ember#262 |
| `crates/mobile` with `ios/` | 312 / 77 rs; 1,192 swift + 111 yml; 47 sh | drop (as a pair) | UniFFI bridge and SwiftUI app, built as a V1 slice on 2026-10-19. | `docs/ios-architecture.md:7-16`; Orthic-Labs/ember#260 |
| slice of `crates/engine`: Classic output modules (print, slideshow, book, contact sheet) | 4,252 / 0 | drop | Beyond library plus RAW editor (Classic about 30%). | `crates/engine/src/print.rs`; Orthic-Labs/ember#268 |
| `packaging/` (Linux, FreeBSD, web, macOS, Windows scripts) | about 2,700 | drop (in part) | RightKit owns signing and installers; two signing scripts (145 lines) are referenced by no workflow, script or config. | `packaging/macos/import-cert.sh:1-8`; Orthic-Labs/ember#270 |

## Totals

| Verdict | Units | src | other |
|---|---|---|---|
| keep (product) | 6 | 16,476 | 6,076 |
| keep (upstream-synced) | 15 | 184,128 | 8,767 |
| replace (whole crates) | 2 | 4,260 | 318 |
| later | 8 | 32,772 | 3,935 |
| drop | 11 | 94,112 | 1,583 |
| **Rust tree** | 42 | **331,748** | **20,679** |

Non-Rust: keep 7,229 ts/tsx/css (desktop web), 1,537 mjs and 4 sh (`scripts/desktop`), 3,151 py/sh/ps1/txt (`tools/`); drop candidates 1,192 Swift and 111 YAML (`ios/`), 47 sh (`scripts/ios`), about 2,700 (`packaging/`), 15 js (`apps/lightcraft-web`). Engine slices a set of drops would remove from `crates/engine`: faces 3,638, merge and desktop-host 908, map and places 2,686, Classic output modules 4,252, video 1,188, demo library 684 (not added into totals).

## What this branch changed

| Row | Change | Evidence |
|---|---|---|
| open_url | deleted the local opener for `rightkit_child_process::open_url` | `apps/lightcraft-desktop/src/services.rs` |
| QA viewport | deleted `qa_viewport` and `lc_qa_viewport` (no caller) | `apps/lightcraft-desktop/src/native.rs` |
| mcp base64, `ct_eq` | deleted `crates/mcp/src/base64.rs` and the local `ct_eq`; the crate re-exports the owner's | `crates/mcp/src/lib.rs`, `control_auth.rs`, `Cargo.toml` |
| every RightKit pin | moved to the registry version (Rust and npm); `cargo update --precise` and `pnpm install --lockfile-only` regenerate the lockfiles | `Cargo.lock`, `pnpm-lock.yaml` |
| release switches (M21/M23) | added `switches.json` (109 reads, 12 files with a non-literal read) and `releaseSwitches.nonShipped` for the eight directories no release target builds; 17 class S1 to S4 reads are not compiled out and are tracked in Orthic-Labs/ember#271 | `switches.json`, `right-release.config.mjs` |
| QA env reads | `RIGHTKIT_QA_HIDDEN` and `RIGHTKIT_QA_BACKGROUND` were read outside `qa-native` (the value was then ignored); they now go through two helpers that read nothing without the feature | `apps/lightcraft-desktop/src/native.rs` |
| release config | `distribution.updaterPublicKey` read from `tauri.conf.json` `plugins.updater.pubkey`; `packageManager` is the tool name `pnpm` (the version lives in `package.json`) | `right-release.config.mjs` |

## Not verified

- Nothing was compiled, tested or run here. Every deletion and call-site change above needs the host build and the unit tests of the touched crates (`crates/mcp`, `apps/lightcraft-desktop`).
- Whether the egui or web hosts have users is Adrian's knowledge, not the tree's.
- The CutRight figures in the RightKit issues (N7) come from the 2026-10-24 port order and were not re-read.
