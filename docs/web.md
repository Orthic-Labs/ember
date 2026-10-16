# Ember in the browser

`apps/lightcraft-web` runs the same egui UI as the desktop app (`crates/ui-egui`) in the browser,
compiled to WebAssembly and drawn with WebGL2 (eframe's `glow` backend).

## Build and run locally

One-time setup:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the wasm-bindgen version in Cargo.lock> --locked
```

`cargo xtask web` checks the CLI against `Cargo.lock` and prints the exact install command if
it's missing or a different version.

Build, then serve:

```sh
cargo xtask web            # → <target>/web-site/{index.html, worker.js, lightcraft_web.js, lightcraft_web_bg.wasm}
cargo xtask web --serve    # build, then serve on http://127.0.0.1:8080/ (or `--serve 9000`)
cargo xtask web --dev      # unoptimized build with debug info (faster to compile, slow to run)
```

`<target>` is `target/` unless `CARGO_TARGET_DIR` is set. Any static HTTP server works, as long as
it serves `.wasm` as `application/wasm`. The bundle can't be opened from `file://` (module
workers and OPFS need an HTTP(S) origin; `localhost`/`127.0.0.1` count as secure). If `wasm-opt`
(binaryen) is on `PATH`, the release build also runs it over the module.

The release build uses the `web` Cargo profile (see *Bundle size* below) and writes gzip (`-9`)
and brotli (`-11`) precompressed copies next to each file (`*.gz`, `*.br`, pure Rust, printed
as a size table). `--serve` sends them with `Content-Encoding` when the browser accepts them;
configure a production server the same way (e.g. nginx `gzip_static`/`brotli_static`).

## Bundle size

Measured on the full bundle (every codec and raw decoder is in the module), without `wasm-opt`:

| `web` profile                                   | `.wasm` bytes | gzip -9 | brotli -11 | slider job* |
|-------------------------------------------------|--------------:|--------:|-----------:|------------:|
| before: release, thin LTO                       |    15 742 091 | 5 251 664 | 3 473 726 | 3.8 ms |
| fat LTO, 1 CGU, strip, panic=abort, opt 3       |    12 924 849 | 4 779 686 | 3 210 246 | 3.9 ms |
| same, opt-level "s"                             |    13 056 715 | 4 362 536 | 2 959 126 | 6.1 ms |
| same, opt-level "z"                             |    12 648 958 | 4 152 661 | 2 860 173 | 9.0 ms |
| **current:** "s", per-pixel crates at opt 3     |    13 533 126 | 4 583 465 | 3 093 780 | 4.0 ms |

\* median Exposure draft render of the `?bench` loupe in a worker (stage-cached), headless
Chrome. Size-optimizing the pipeline crates costs 50–130 % render time, so `Cargo.toml` keeps
them (pipeline, raster, color, develop, geom, raw, codecs, scenes, preview and the JPEG/PNG
codecs) at opt-level 3 and the rest (egui, eframe, serde, glue) at "s". What goes over the wire
is the brotli column: 3.1 MB, 11 % less than the old build's brotli size and 41 % less than its
gzip size (5.25 MB). `wasm-opt -Oz`, when installed, shrinks it further.

**Fonts.** The browser has no system fonts to fall back on, so CJK text comes from
[craft-fonts](https://github.com/storytold/craft-fonts), the optional `CRAFT_FONTS_DIR` build
input (`CRAFT_FONTS_DIR=../craft-fonts cargo xtask web`; release builds always set it). On wasm32
`crates/engine/build.rs` embeds only BIZ UDPGothic Regular (UI and watermark fallback). The web
bundle also ships Noto Sans CJK SC Regular as `lightcraft_zh_hans.otf` and loads it before the UI
starts. Keeping the 16 MB Chinese face separate leaves the module under Cloudflare's 25 MiB
per-file limit (the older Japanese-only module measured 21.8 MB). Without `CRAFT_FONTS_DIR`, the
web build still works, but Chinese and Japanese text lack glyphs. **Korean (Hangul).** BIZ UDPGothic has no Hangul, so on the web Hangul falls through to the Noto Sans CJK SC face that the bundle loads for Chinese, appended after the UI faces. Whether that face carries the Hangul syllables is not verified in this repository (no craft-fonts checkout here) Noto CJK for Korean is allowed (maintainer decision, 2026-10-16).

## Deploying: headers

`cargo xtask web --serve` sends these on every response. A production server may send them too;
today they are optional:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
Cross-Origin-Resource-Policy: same-origin
```

COOP + COEP make the page *cross-origin isolated* (`crossOriginIsolated === true`). That will be
**required** for `SharedArrayBuffer`, i.e. for a future wasm-threads build (`+atomics`, nightly
`build-std`), and gives full-resolution `performance.now()`. The current render workers (below)
don't share memory, so the app runs without them; with COEP on, every subresource must be
same-origin or send CORP/CORS headers (the bundle has no third-party resources). The file names
are the same in every version, so serve them with `Cache-Control: no-cache` (not `immutable`);
`packaging/web/README.md` (shipped as `HOSTING.md`) has the hosting details.

## What works

- **A persistent library.** The library lives in the browser's storage for the page's origin:
  [OPFS](https://developer.mozilla.org/docs/Web/API/File_System_API/Origin_private_file_system)
  when the main thread can write it (`FileSystemFileHandle.createWritable`: Chrome, Edge,
  Firefox, recent Safari), otherwise IndexedDB. Both hold the same layout:
  - `library/catalog.snap`, `library/catalog.log`: the same crash-safe journal as the desktop app
    (`lightcraft-catalog`), plus `presets.json`, `view.json`, `prefs.json` and `ui.json` (panel
    layout). The catalog `Store` is a memory mirror loaded at start-up; every change is flushed in
    the background within a frame or two, each file replaced atomically, in modification order
    (`apps/lightcraft-web/src/files.rs`). View state and UI prefs are saved every second when they
    change (a tab can close without notice).
  - Known limitation: the browser storage has no file locks, so two tabs of the same origin open
    the same library and the last one to write a snapshot wins (the desktop app and the CLI lock
    a library folder: `catalog.lock`, issue #99). Use one tab at a time; a guard through the Web
    Locks API (`navigator.locks`) is still to do.
  - `originals/<content hash>`: the bytes of every imported photo. The catalog refers to them as
    `web/<hash>/<file name>`; the main thread keeps recently used originals in memory (≤ 768 MB)
    and loads the rest on demand (the active photo is prefetched).
  - `thumbs/<key>.jpg` + `thumbs/index.json`: the rendered-thumbnail cache, with the desktop's
    budget (2 GB, least recently used pruned to 80 %).

  A new library starts with the procedural demo photos. URL options: `?store=idb` forces
  IndexedDB, `?store=memory` keeps nothing, `?reset` deletes the stored library first (after a
  confirmation: it deletes every imported photo too), `?safe` opens the library without decoding
  any photo (see *Keeping the library safe*).
- **Keeping the library safe** (experimental: the library lives only in this browser):
  - The app asks for persistent storage (`navigator.storage.persist()`); when the browser doesn't
    grant it, a notice says the library may be evicted under storage pressure.
  - **File ▸ Back Up Library…** downloads a zip of the library files and every stored original
    (`originals/<hash>/<file name>`, so an unzipped backup is browsable; entries are stored, not
    compressed; zip64 past 4 GB or 65 535 files, one original at most 4 GB). **File ▸ Restore Library from Backup…** reads such a zip into a
    new library folder (`library-restored-<time>/`; originals already stored are skipped, every
    entry's checksum is verified, and each restored original must match its content hash) and
    only then switches to it (`active-library` names the folder in use) and reloads: the previous
    library stays in storage (see *Storage tidy-up*). Both are web-only (`file.backupLibrary`, `file.restoreLibrary`; the
    desktop library is a folder).
  - **`?safe` start** (add it to the address): the library opens with the renderer paused (no
    thumbnail, loupe render, embedded-preview stand-in or prefetch starts, and no render workers
    start), storage is not tidied, and a notice says so. This is the way out of a photo whose
    decoder panics: on wasm32 a panic aborts the module and can't be caught, so the page shows
    the error (with a hint to use `?safe`) and the stored library stays intact; in the safe start
    the photo can be removed, or the library backed up, without decoding anything. Thumbnails
    stay blank there; reload without `?safe` to see photos again.
  - **Storage tidy-up**, when the library opens (only in the tab holding the library lock, never
    in a `?safe` start): a restore records the library it replaced (`previous-library`), and the
    next start keeps the active library and that one and deletes older `library-*` folders,
    including a half-written restore. Library folders from before restores recorded this one are
    never deleted. Then stored originals that no kept library names are deleted (a plain scan of
    the kept libraries' files for `web/<hash>/` paths, so a damaged catalog still protects its
    photos; at most 500 per start; nothing is deleted unless the active library has catalog
    files and every kept library's files could be read). A removed photo's original therefore
    goes at the next start once the catalog no longer names it (the catalog's log is compacted
    into its snapshot from time to time), not at once: Undo and the other tabs' state stay safe.
    A notice says what was freed.
  - A failed save (quota exceeded, storage cleared) is not silent: the catalog then refuses new
    writes, so commands report `saved in memory but not written`, the top bar shows the unsaved
    warning (as on the desktop, see `docs/control-protocol.md`), and saving is retried every
    2 s until it works (`web.stats` → `saveError`).
  - A picked or dropped photo whose bytes can't be stored is not added (it would be gone after
    a reload); a notice says why.
  - One tab at a time: the page holds a Web Lock (`navigator.locks`, `lightcraft-library`); a
    second tab or window shows "Ember is already open in another tab" instead of loading
    its own copy (two copies would overwrite each other's saves). Browsers without Web Locks, or
    that refuse the lock request, aren't protected: the app starts without the guard.
  - If the stored library can't be opened, a notice says the session is temporary and nothing
    is saved; the stored library is left as it was.
  - A panic (wasm is built with `panic = "abort"`) replaces the page with a message saying the
    stored library is kept and to reload, instead of a frozen canvas.
- **Importing photos with no filesystem.** *File ▸ Import Photos…* (<kbd>⌘⇧I</kbd>) opens the
  browser's file picker. You can also drop files anywhere on the page. The bytes are written to
  storage, then imported and decoded by the same engine code as the desktop app
  (`lightcraft_engine::files::{probe_bytes, load_bytes}`): JPEG/PNG/TIFF/WebP and the supported
  raw formats. "Copy into library" is the same as "Add" here.
- **Rendering in Web Workers.** Renders don't run on the main thread: up to four dedicated
  workers (`hardwareConcurrency − 1`, `?workers=N` to override, `?workers=0` for the old inline
  path) each run a second instance of the same wasm module. `index.html` compiles the module once
  and posts the compiled `WebAssembly.Module` to `worker.js`. A job crosses as JSON (the photo's
  source, develop settings, request); the worker reads the original or the cached thumbnail from
  storage itself, keeps decoded sources and the loupe's stage cache, renders, and transfers the
  RGBA bytes back. Jobs for a photo go back to the worker that decoded it. This needs no wasm
  threads or `SharedArrayBuffer`. If no worker starts, rendering falls back to the main thread.
- **Every develop control** (sliders, curves, mixer, grading, masking, crop…) works as it does
  on the desktop, since it's the same crate.
- **Export downloads the file.** *Export…* (<kbd>⌘⇧E</kbd>) runs the same `app.export` path as
  the desktop app (`lightcraft_engine::export`: JPEG/PNG/TIFF/WebP, sizing, naming). The host's
  `write` service hands each file to the browser as a download instead of writing it to disk.
- **Exports run in the render workers** ([#192](https://github.com/Orthic-Labs/ember/issues/192)).
  The main thread prepares each photo (`prepare_batch`), posts a `WireExport` (the render job
  sized for the output, develop settings, export options, metadata, file name) to a worker, and
  the worker reads the original from storage (the page sends the bytes only with `?store=memory`),
  decodes, renders, encodes and transfers the file back. The main thread then places it with the
  engine's `BatchPlacer` (naming, conflicts) and starts the download, which only it can do. One
  photo is in flight at a time, so a batch holds one decoded original; with several workers the
  first is left to the views. The batch has its row in the activity stack, and its ✕ takes effect
  at once (a photo already in a worker finishes there and its file is dropped). `app.export`
  returns `{"background": true, "total": n}`; the result arrives as the toast and in `ui.inspect`
  → `export.last`. If no worker starts (or `?workers=0`), exports run on the main thread as
  before, and a worker that fails mid-photo hands that photo back to the main thread. Photos a
  worker can't make also run on the main thread, one per timer tick: *Original* copies and DNGs
  (they only read the file) and photos with a Denoise edit.
- **Auto Settings run in the render workers** ([#192](https://github.com/Orthic-Labs/ember/issues/192)).
  Auto is two phases ([`auto_measure`](../crates/engine/src/auto_measure.rs)): a pure measure
  (`measure_auto`: the decoded thumbnail-level source, its facts and the photo's settings in, the
  adjustment values out; no `Session`) and an apply command, `develop.applyAutoValues {values,
  basis, id?, control?}`, that writes the values as one undo step. The basis is a hash of the
  photo's settings, face regions and source; the apply is refused with an error when it no longer
  matches, so values measured for an older photo are never written. On the desktop and native
  builds `develop.auto*` composes the two in place and behaves as before. On the web,
  `develop.auto`, `develop.autoTone` and `develop.autoControl` hand the measure to a worker (a
  `WireAuto` message: the origin, settings and face boxes; the worker reads the original from storage
  itself) and answer `{"background": true}` at once (as `app.export` does: `command()` resolves before the
  values are applied); an "Auto Settings" row shows in the activity
  stack until the values are back, and `auto_task::poll` then applies them. A second Auto on the
  same photo while one is away is refused ("already measuring"); one that finds every worker busy
  waits up to 30 s for a free one. If no worker is alive, or an original is not stored, the command
  runs on the main thread as before. Edit the photo while the measure is away and the apply is
  refused with the basis error; run Auto again. The decoded source is the one the worker keeps for
  the photo, so a repeat measure does not decode again. Native unit tests cover measure + apply
  equal to `develop.auto`, the stale basis, one undo step, and the host handoff; the browser check
  is not run yet. (A raw file's as-shot white balance, which the main thread's own decode records
  in the catalog, is not recorded by a worker's decode.)
- **Preset files.** *File ▸ Import Profiles & Presets…* opens the browser's file picker for
  `.lcpreset`, `.xmp`, `.lrtemplate`, `.zip`, `.dng`, Luminar `.lmp` / `.mplumpack`; the bytes are
  read and imported by the same reader as the desktop (`Session::import_preset_bytes`), and a
  notice reports what was added, what was already there and what could not be read. *Export
  Presets…* (or the Presets panel's export) hands the `.lcpreset` to the browser as a download
  (`preset.export` with `content`). `.cube` LUT profiles and point-curve presets aren't wired to
  the browser picker yet. [#195](https://github.com/Orthic-Labs/ember/issues/195)
- **Automation.** `await lightcraft.command("library.info", "{}")` runs any engine or UI command
  by id on the next frame and resolves to the JSON result once the command's writes are in browser
  storage (or rejects if storage refused them; no storage, `?store=memory`, resolves at once).
  `web.stats` reports storage, workers and the render queue. This is how the headless-Chrome
  checks drive the page.

## Not yet

- **Some auto-adjustments still compute on the main thread** ([#192](https://github.com/Orthic-Labs/ember/issues/192)
  stays open for them). Auto Settings, Auto Tone and Auto Slider measure in the render workers (*Auto
  Settings run in the render workers*). Still in the engine's synchronous command, on the main
  thread: `develop.autoBatch` (Auto Settings to Selection), `develop.autoBwMix`, `develop.autoNoise`,
  White Balance ▸ Auto, the Auto step of a preset (`preset.apply`), and `develop.auto` with
  `dryRun` (it has to answer with the values). Each measures one photo from its own
  thumbnail-level source, so they can take the same route (a measure function that needs no
  session, then an apply command); batches need one `applyAutoValues` that takes several photos as
  one undo step.
  Separately, **the "still loading" failure after a reload** is fixed for every `develop.auto*`
  command: the browser host parks a `develop.auto*` command that fails because the original is still coming from storage
  (`apps/lightcraft-web/src/retry.rs`) and runs it again every 50 ms until it works, fails for
  another reason, or 60 s pass; the failed read has already queued the load. Only `command()` calls
  are parked (`web.stats` → `commandsWaitingForOriginals`); a key press in the UI right after a
  reload still reports it, though the active photo's original is prefetched from the first frame.
  The *Original*/DNG and Denoise export cases above also still read the original on the main thread
  and do not wait. The park/retry state is covered by native unit tests; the browser check (first
  command after reload succeeds) is not run yet.
- **Durability is "a frame later" for user actions, by design** ([#193](https://github.com/Orthic-Labs/ember/issues/193)):
  the catalog's `Store` is synchronous and browser storage is not (OPFS writable streams,
  IndexedDB), so a click or key command can't be written before it returns. It is written by the
  flush that starts in the same frame, a few milliseconds later; a crash or tab kill inside that
  window loses it. What the page does about it:
  - `web.stats` → `durability` is `durable` (everything written is in storage), `pending` (written in
    memory, a flush is queued or running) or `failed` (storage refused it; see `saveError`).
    `Files::durability` (`apps/lightcraft-web/src/files.rs`) is the state machine, covered by a
    native unit test.
  - The automation API is durable before it returns: `command()` (see *Automation*) resolves only once
    its writes reached storage, and rejects, saying the change is not saved, if storage refused them.
  - On `beforeunload`, `pagehide` and when the tab becomes hidden, the page starts the flush at once
    (not on the next frame, which a hidden tab may never run). While writes are pending or refused,
    `beforeunload` also asks the browser to confirm leaving, which gives the in-flight write time to
    finish. No handler can wait for an asynchronous write, so a forced kill (task manager, crash) in
    the window is still lost. This is the documented weaker guarantee.
  - Not run yet: a browser kill check (headless Chrome: run a command, kill the tab, reload,
    compare). Only the state machine is covered by tests.
- **Data safety: what is left** (issue #107): a panic in a decoder still ends the page, since a
  wasm32 panic aborts and can't be caught (a photo that panics while its thumbnail renders can do
  so again after a reload: use the `?safe` start above). Nothing else is open. [#194](https://github.com/Orthic-Labs/ember/issues/194)
- **Control channel / MCP.** These are desktop-only, because they need a TCP socket.
- **AVIF export** is native only: its encoder (`ravif`, `rav1e`) is compiled out on wasm32, so the browser
  build doesn't offer the format in the export dialog (issue #196). [#196](https://github.com/Orthic-Labs/ember/issues/196)
- **JPEG XL export** is native only: the encoder (`jixel`) starts threads of its own, which wasm32 has not got; the
  web build reports "not available in this build". JPEG XL import works. [#197](https://github.com/Orthic-Labs/ember/issues/197)

## Measuring

Open `http://127.0.0.1:8080/?bench` to run a scripted measurement:

1. Wait for the grid thumbnails.
2. Open the first photo in Detail.
3. Drag Exposure through 8 steps, exactly as a slider drag does (begin interaction → `develop.set`
   ×8 → end).

The page then logs one console line:

```
lightcraft-bench {"first_frame_ms":…,"thumbs_done_ms":…,"slider_draft_ms":[…],"slider_draft_median_ms":…,"slider_job_ms":[…],"release_full_ms":…}
```

- `slider_draft_ms`: time from the `develop.set` command until the new loupe texture is ready.
- `slider_job_ms`: the pipeline's share of that time.
- `release_full_ms`: the full-quality render after the drag ends.
