# File format parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-17 · **Change:** minor (lossy JPEG DNG for Render to DNG, #188)

Every format reference editor reads or writes, with our support, fidelity and tests. Camera raw formats are
deep enough to have their own document: [raw-parity.md](raw-parity.md).

**How measured:** the target's document types come from the installed reference editor 9.6
`Info.plist` (`CFBundleDocumentTypes`: catalog `mcat`, `xmp`, toolkit / module / library bundles, and
the raw-and-image list: jpg, tif, dng, heic and 27 raw extensions) plus reference catalog editor's documented
import and export formats. Ours come from `crates/codecs/src/sniff.rs`, `crates/codecs/src/encode.rs`,
`crates/engine/src/export.rs`, `crates/engine/src/import.rs` and the tests named below.

## Images (non-raw)

| Format | Target reads | Target writes | Ember reads | Ember writes | Notes / tests |
|---|---|---|---|---|---|
| JPEG | ✅ | ✅ | ✅ (parallel decoder, ICC, EXIF orientation) | ✅ quality, size limit, ICC, metadata | `crates/codecs/src/jpeg.rs`, export tests |
| TIFF | ✅ 8/16/32-bit | ✅ 8/16-bit, LZW/ZIP | ✅ 1–64-bit integer, 16–64-bit float, bilevel, CCITT fax, WhiteIsZero, palette (1–8-bit indices through the ColorMap) | ✅ 8/16-bit, 32-bit float linear, none / LZW / ZIP | `crates/tiff`, `crates/codecs/src/tiff_codec.rs` |
| PNG | ✅ | ✅ | ✅ | ✅ | |
| PSD / PSB | ✅ (maximized-compatibility composite) | ✅ (Classic: PSD export, edit-in round trip) | ✅ merged composite | ⬜ (external-editor round trip writes 16-bit TIFF) | `crates/codecs/src/psd.rs` |
| HEIC / HEIF | ✅ | ⬜ (Classic) / HDR only | ✅ in every release build (`crates/heif`, heic-rs) | ⬜ | `heif` feature on by packaging scripts |
| AVIF | ✅ | ✅ (HDR) | ✅ native (rav1d behind the isolated `crates/av1dec` crate: 8/10/12-bit, 4:0:0 to 4:4:4, `nclx` matrix and range, ICC, `irot`/`imir`/`clap`, `grid` images, alpha items, Exif/XMP); ⬜ web, PQ/HLG transfer read as sRGB, overlays, image sequences beyond the still | ✅ 8/10-bit, PQ HDR; `photo.renderToAvif` | `crates/codecs/src/avif.rs` (+ `avif/tests.rs`: hand-built and truncated containers, grids, transforms, YCbCr maths, real AV1 stills), `crates/av1dec`, `crates/codecs/tests/roundtrip.rs` (encode, decode, PSNR), `crates/engine/src/tests_xmp.rs` (`render_to_avif_adds_a_decodable_avif_next_to_the_original`). CI verification pending: nothing was compiled or run when this landed |
| JPEG XL | ✅ | ✅ | ✅ (`jxl-oxide`) incl. DNG 1.7 JXL tiles | ✅ lossy / lossless, 8/16-bit, PQ HDR (`jixel`, native only; ⬜ web) | `crates/engine/src/tests_export.rs` |
| WebP | not in its document types | ⬜ | ✅ | ✅ | beyond target |
| GIF, BMP | ⬜ | ⬜ | ✅ | ⬜ | beyond target |
| DNG (from raw) | ✅ | ✅ incl. lossy, embedded original | ✅ | ✅ uncompressed, LJ92 or deflate; ⬜ lossy raw (CFA) DNG, embedded original | `crates/raw/src/dngwrite.rs`; lossy CFA needs the DNG 1.7 interleaved layout (1-plane JPEG XL tiles of a stacked-field image, `RowInterleaveFactor` / `ColumnInterleaveFactor`), which neither the writer nor the reader has and the spec text could not be checked here |
| Render to DNG (15.5) | — | ✅ | — | ✅ export format "Rendered DNG" and the catalog command `photo.renderToDng` (linear float DNG, edit baked in, added next to the original and stacked); lossy JPEG XL option (`dngCompression: lossy`, DNG 1.7 Compression 52546 tiles via `jixel`, native only, CI verification pending); lossy JPEG option (`dngCompression: lossyJpeg`, DNG 1.4 Compression 34892: 8-bit sRGB-coded baseline JPEG tiles, 4:4:4, restored by a 256-entry `LinearizationTable`, `WhiteLevel` 65535, any platform; `photo.renderToDng` takes `dngCompression` and `quality`; test `lossy_jpeg_dng_has_the_dng_14_tags_and_reads_back_close_to_the_source`, unverified until CI runs) | `crates/raw/src/dngwrite.rs` (`write_rendered_dng_with`), `crates/raw/tests/rendered_dng.rs`, `crates/engine/src/tests_xmp.rs` (`render_to_dng_lossy_jpeg_writes_a_dng_14_that_reads_back`) |
| Original + sidecar | — | ✅ | — | ✅ | |
| HDR gain map JPEG (ISO 21496-1) | ✅ | ✅ | ✅ (gain map read) | ✅ | `crates/codecs/src/gainmap.rs` |
| PQ / HLG HDR | ✅ | ✅ AVIF, JXL, TIFF | — | 🟡 PQ AVIF, PQ JPEG XL, float TIFF; no HLG | |
| Contact sheet / print PDF | — | ✅ (Print module: PDF / JPEG / printer) | — | 🟡 paginated contact-sheet PDF (A4 / Letter, 150 dpi, sRGB) | `crates/engine/src/contact_sheet.rs` |
| Video (MP4, MOV, AVCHD…) | ✅ playback, trim, edit | ✅ export | 🟡 catalogued (kind, duration) | ⬜ | see video gap |

## Colour spaces and profiles

| Feature | Target | Ember |
|---|---|---|
| Export colour spaces | sRGB, Adobe RGB, Display P3, ProPhoto, Rec.2020, custom ICC | ✅ sRGB, Display P3, Adobe RGB-compatible, ProPhoto, Rec.2020 with gamut mapping (CPU and GPU), embedded ICC; ⬜ custom ICC output profiles; AVIF stays sRGB |
| Input ICC (JPEG / TIFF / PNG) | ✅ | ✅ matrix / TRC and LUT profiles (moxcms) |
| Soft proofing with printer profiles | ✅ | 🟡 |

## Metadata, sidecars, presets and catalogs

| Format | Target | Ember | Notes |
|---|---|---|---|
| XMP sidecars (`crs:` develop settings, ratings, keywords, regions) | read / write | ✅ read / write, manual (⌘S) and automatic | [xmp-interop.md](xmp-interop.md) |
| EXIF / IPTC / XMP embedded on export | ✅ | ✅ with metadata filters | `crates/meta` |
| Develop presets `.xmp` | ✅ | ✅ import (mapping of supported settings) | |
| Legacy presets `.lrtemplate` | ✅ (converted) | ✅ import | |
| Camera / creative profiles (`.dcp`, `.xmp` profiles) | ✅ | ⬜ deliberately (reference publisher profile formats are never used); `.cube` LUT profiles ✅ | |
| Lens profiles `.lcp` | ✅ | ⬜ deliberately | |
| Other apps' presets | — | ✅ Luminar `.lmp` / `.mplumpack`, `.zip` bundles | beyond target |
| reference catalog editor catalog `.lrcat` | native | 🟡 import (ratings, flags, labels, keywords, collections, virtual copies, supported develop settings; read-only pure-Rust SQLite incl. WAL) | [lightroom-catalog-import.md](lightroom-catalog-import.md) |
| reference editor (cloud) library `.lrlibrary` | native | ⬜ | cloud library format, local cache only |
| Write back to `.lrcat` | native | ⬜ (one-way migration by design) | |
| Keyword lists (text) | import / export | ✅ | |
| GPX track logs | ✅ | ✅ | `crates/meta/src/gpx.rs` |
| Export presets, metadata presets, filename templates | ✅ (`.lrtemplate`) | ✅ own JSON | not interchangeable |
| reference editor plug-ins (`.lrplugin`, `.lrdevplugin`) | ✅ | ⬜ | see ecosystem in [target-app-parity.md](target-app-parity.md) |

## Scores

| Part | Weight | Ready for real work | Kind |
|---|---:|---:|---|
| Standard image import / export | 35% | ~90% | estimated (table above; missing: PSD write, lossy raw DNG, AVIF on the web build) |
| Sidecars, metadata, presets | 25% | ~85% | estimated |
| reference editor catalog migration | 25% | ~50% | estimated (one-way, approximate rendering of unsupported settings) |
| HDR, print, video output | 15% | ~35% | estimated |
| **Non-raw file formats** | 100% | **~71%** | estimated; raw is in [raw-parity.md](raw-parity.md) (~50%) |

Remaining: **18–32 h** (PSD write 4–8 h; lossy raw (CFA) DNG 3–6 h; HLG 3–5 h; catalog import depth 8–12 h).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created from the code and the installed reference editor's document types |
| 2026-10-10 | minor | JPEG XL export (#247): JXL write ✅ (native only), PQ JPEG XL HDR |
| 2026-10-12 | minor | Render to DNG: "Rendered DNG" export format (linear 16-bit float DNG of the rendered photo) |
| 2026-10-16 | minor | Render to DNG: `photo.renderToDng` catalog command; AVIF decode still blocked (no permissive pure-Rust AV1 decoder in the lockfile: rav1e/ravif are encoders); lossy DNG output not started |
| 2026-10-16 | minor | Palette (indexed-colour) TIFF decodes through its ColorMap (1–8-bit indices; `crates/codecs/src/tiff_codec.rs`). Audit: GIF, BMP, WebP and JPEG XL already decode and sniff by magic; no new crate was added. Still missing: AVIF decode, HEIC in default builds |
| 2026-10-16 | minor | Lossy JPEG XL DNG for Render to DNG (`dngCompression: lossy`, DNG 1.7 tags JXLDistance/Effort/DecodeSpeed); raw (CFA) lossy DNG still open |
| 2026-10-17 | minor | DNG export: embedded JPEG preview option (`dngPreview`: none / medium / full), a SubIFD with NewSubFileType 1 |
| 2026-10-17 | minor | Blockers recorded for lossy raw (CFA) DNG and AVIF decode / `photo.renderToAvif` (#79, #188); no behaviour change |
| 2026-10-17 | minor | Lossy JPEG DNG (#188): `DngCompression::JpegLossy`, DNG 1.4 Compression 34892 tiles through a `LinearizationTable`, for Render to DNG (`dngCompression: lossyJpeg`, `quality`); `photo.renderToDng` gains both params. Unverified until CI runs |
| 2026-10-17 | minor | AVIF import (#188): ISOBMFF/HEIF item parser (`crates/codecs/src/avif.rs`), AV1 decode through rav1d 1.1.0 behind the isolated `crates/av1dec` crate, YCbCr to RGB with the `nclx` matrix and range, `irot`/`imir`/`clap`, grids, alpha; `photo.renderToAvif` catalog command. Unverified until CI runs |
