# AI masks (Object and Describe)

Ember selects objects with **MobileSAM** (Zhang et al. 2023: Segment Anything's prompt encoder and
mask decoder behind a 5M-parameter TinyViT image encoder), run in pure Rust by `crates/segment` on
[candle](https://github.com/huggingface/candle): on the GPU through Metal on macOS, on the CPU
elsewhere (for now). **SAM 3** (Segment Anything with Concepts, Meta 2025) stays an optional
upgrade: if its files are in the model folder it is used instead, and it is the only model with text
prompts today.

**The model is optional.** It is not part of Ember, and nothing requires it: without it,
Object and Describe offer to download it, every other feature (and every existing mask, see
below) works, and no render, export or command ever waits for it.

## The models

| | **MobileSAM** (default) | SAM 3 (optional) |
|---|---|---|
| Download | one file, **40.6 MB** (40,613,024 bytes) | 3.44 GB (+ 2 tokenizer files) |
| Gated? | **No**: public Hugging Face repository, no account, no token | **Yes**: manual approval by Meta |
| Licence | **Apache License 2.0** (code and weights) | SAM License (Meta; custom, not OSI) |
| Prompts | clicks (include / leave out) and a box | clicks and text |
| Source | `lmz/candle-sam` revision `8b4cb7c7`, `mobile_sam-tiny-vitt.safetensors` | `facebook/sam3` |
| SHA-256 | `5ad43a841b57cc798e2837e251c15aa9f602868a1780d195459383d117784cf7` | `6d06f0a5…cc14a` (`model.safetensors`) |

**MobileSAM facts, with evidence** (checked 2026-10-17):

- Weights: <https://huggingface.co/lmz/candle-sam/resolve/8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97/mobile_sam-tiny-vitt.safetensors>,
  a float32 safetensors conversion, by the candle maintainers, of the official `weights/mobile_sam.pt`
  of <https://github.com/ChaoningZhang/MobileSAM>. The Hugging Face API reports `gated: false` for the
  repository; the size and SHA-256 above were computed over a fresh download of exactly that URL
  (`crates/segment/src/fetch/mod.rs`, `MOBILESAM`, pins both and the revision).
- Licence: the MobileSAM repository's `LICENSE` is the Apache License 2.0
  (<https://github.com/ChaoningZhang/MobileSAM/blob/master/LICENSE>); the Segment Anything code it builds
  on is Apache-2.0 too; the network code Ember runs, `candle-transformers`, is MIT OR Apache-2.0. The
  conversion repository has **no model card or licence file of its own**, so the licence of those weights
  is the original repository's; if you redistribute them, ship that licence with them.
- Training data: MobileSAM was distilled from SAM on a subset of SA-1B (Meta's dataset); the repository
  states Apache-2.0 for the weights regardless. This is the one thing to re-check before commercial
  redistribution (Ember does not redistribute them: the app downloads them from Hugging Face on request).

**Expected quality** (upstream numbers, from the MobileSAM README, not measured by Ember):

| | Original SAM (ViT-H) | MobileSAM |
|---|---|---|
| Image encoder parameters | 611M | 5M |
| Whole pipeline parameters | 615M | 9.66M |
| Image encoder time (one GPU, upstream) | 452 ms | 8 ms |
| Whole pipeline time (one GPU, upstream) | 456 ms | 12 ms |

Agreement with the original SAM's masks for two-point prompts (mIoU, higher is better; MobileSAM
README, points 100 to 500 pixels apart): **0.73, 0.71, 0.74, 0.73, 0.73** (FastSAM: 0.27 to 0.41). The
upstream claim is that it "performs on par with the original SAM (at least visually)" with the same
pipeline and a smaller encoder: expect clean edges on clear subjects, and weaker results than SAM 3 on
cluttered scenes, thin structures and low contrast. **Ember has not yet measured MobileSAM on real
photos** (no GPU or CPU timing, no IoU against SAM 3 or hand-made masks): that evaluation (about 50 CC0
photos, per [#221](https://github.com/Orthic-Labs/ember/issues/221)) is open, and these numbers are the
upstream's, on a GPU.

## Using them

In the Masking panel (M):

- **Object**: click the thing you want; every click refines the selection. ⌥-click (Alt) a part
  to leave it out. Clicks show as green (include) and red (leave out) dots.
- **Describe**: type what to select — `sky`, `trees`, `the red car` — and press Return. Every
  instance the model finds (score above 0.5) joins the mask; if nothing matches, nothing is
  created and you are told so. **Describe needs SAM 3** (see below) until the grounding step of
  [#221](https://github.com/Orthic-Labs/ember/issues/221) is built: with MobileSAM alone it says so at once.
- Several things at once: `car, road` (comma-separated) selects each and merges them.
- **+ / −** under the selected mask (and the Add / Subtract / Intersect menus) combine
  selections: Describe `car`, click the mask, + ▸ Describe… `road`; − ▸ Describe… `people` takes
  the people out of a sky mask; − ▸ Object then clicks remove one object.
- **Edge** (per selection, −100…100): below 0 a crisper border, above 0 a feathered one (up to 2 %
  of the long edge, so previews and exports match).
- **Detail:** about a second after the last click (or right after a description), the photo
  around the selection is analyzed again, zoomed in, from a sharper source (~5 s on an M4 Pro,
  in the background); the result is kept as a high-resolution patch (at most 4 per selection),
  so small objects get 5–10× finer edges than one pass over the whole photo gives.

The first Object or Describe on a photo analyzes it (the image encoder runs once per photo and
look; the timings in this paragraph were measured with SAM 3: ~3 s on an Apple M4 Pro, plus a
one-time ~6 s for compiling GPU kernels and loading the model after launch; MobileSAM's encoder is
about 100 times smaller, and is not timed in Ember yet). That starts in the background as soon as you pick Object; after it, each
click takes ~35 ms and each description ~0.6 s. Everything the model does runs on its own worker
thread: the window stays responsive, a click shows its dot at once and the selection follows
when it's computed ("Selecting…" / "Analyzing the photo…" under the mask tiles). The model is
unloaded after 10 minutes without use (it takes several GB of memory) and loaded again when
needed.

The result is stored with the mask: the model's 288 × 288 mask logits over the uncropped photo
(quantized, compressed, ~10–30 KB). Renders and exports sample them at any resolution and never
need the model, so masks survive moving the library to a machine without it, and the web build
renders them. Editing the photo's look later does not recompute a mask (click again to update).

## Remote Metal inference

Native builds with `sam` can keep the editor, RAW files, library, and exports local while
a Mac runs SAM 3. Build both ends from the same revision. On the Mac, install the model
as described below, then run:

```sh
cargo build --release --locked -p lightcraft-segment --bin lightcraft-sam3-worker
target/release/lightcraft-sam3-worker "$HOME/Library/Application Support/LightCraft/models/sam3" 127.0.0.1:8793
```

On the editing computer, start an SSH tunnel in one terminal and Ember in another:

```sh
ssh -N -T -o ExitOnForwardFailure=yes -L 127.0.0.1:8793:127.0.0.1:8793 user@your-mac
LIGHTCRAFT_SAM3_REMOTE=127.0.0.1:8793 lightcraft
```

Object clicks, Describe prompts, and detail refinement use the remote worker. Only the
rendered model input (long edge at most 1008 pixels) and prompts cross the tunnel. The
worker caches the current photo; detail crops do not replace that cache. Returned masks
stay in the local library, so later adjustments and exports work without the connection.

The worker requires Metal and accepts one editing connection at a time. It loads weights
on demand and releases them when the client disconnects or remains idle for ten minutes.
Both endpoints require loopback addresses: SSH provides authentication and encryption.
The listener has no independent authentication; do not expose it through a public proxy. Any
process of any user on the worker Mac can connect to it, and on the editing computer a local
process listening on the port while the tunnel is down receives the rendered model input, so use
this only on machines whose local users you trust.

No local weights are required in remote mode. An unavailable worker produces an explicit
error, not a silent CPU fallback. Restore the tunnel and retry. Unset
`LIGHTCRAFT_SAM3_REMOTE` to use the local model again. This offloads segmentation only,
not RAW development, denoising, or upscaling. The model licence below still applies.

## Getting the model

### In the app (recommended)

The first time you pick Object or Describe without the model, Ember asks:

> **Download the AI mask model?** Object masks use MobileSAM, a small segmentation model.
> It isn't part of Ember, and everything else works without it. A one-time download of about
> 40.6 MB, saved in *folder*. Licence: Apache License 2.0 — The model's terms, not Ember's.
> Downloading it means accepting them. [Read the model's licence] — **Not Now** / **Download**

Nothing is downloaded until you choose **Download**. The download runs in the background (close
the dialog and keep working; the Masking panel shows its progress), can be cancelled, resumes
where it stopped if interrupted (also after quitting), and tries the next download location when
one fails or stalls. Each file is written as `<name>.part` and moved into the model folder only
when complete and verified — `mobile_sam-tiny-vitt.safetensors` must have the exact size and SHA-256
pinned above; a file that doesn't match is deleted, never used. If no location works, the
dialog says why and Ember carries on without the model.

Agents and scripts use the same commands (control channel, `lightcraft-cli mcp --connect`):
`segment.model.status`, `segment.model.download {"acknowledged": true}` (refused without the
explicit `acknowledged`: pass it only after the user agreed to the download and the licence),
`segment.model.cancel`.

### Download locations

The model is fetched from an ordered list of mirrors: base URLs where
`<base>/mobile_sam-tiny-vitt.safetensors` lives. The list is:

1. `LIGHTCRAFT_SAM3_MIRRORS` (environment variable; URLs separated by commas or spaces), then
2. the file `models/sam3-mirrors.txt` in Ember's settings folder (one URL per line, `#`
   comments) — `~/Library/Application Support/Ember/` (macOS), `%APPDATA%\Ember\`
   (Windows), `~/.config/lightcraft/` (Linux) (both names date from SAM 3 and are kept so existing
   setups work);
3. **only when you list none**: the built-in location, `DEFAULT_MIRRORS` in
   `crates/segment/src/fetch/mod.rs`: the pinned Hugging Face revision of `lmz/candle-sam`
   (public, no token). A list of your own replaces it.

Whatever the location, the file must have the pinned size and SHA-256.

> **Maintainers:** when the file is mirrored on Ember's own storage (R2), add that base URL first in
> `DEFAULT_MIRRORS` (the file must be byte-identical to the pinned one: same size and SHA-256) and
> keep Hugging Face as the fallback. [#177](https://github.com/Orthic-Labs/ember/issues/177)

**SAM 3** (optional) is never fetched from a built-in location: Hugging Face's `facebook/sam3`
is **gated** (each person must accept the licence there, wait for approval and download with
their own token), and Ember has no right to host it yet. Put its three files in the model folder
by hand (below), or someone with access can use it as a mirror once MobileSAM is not wanted:
the in-app download fetches MobileSAM's file only.

(`LIGHTCRAFT_SAM3_TOKEN` is sent as a bearer token only over https, and only to the mirror's own
host — not to the storage host it redirects to.)

The download is plain HTTP/1.1 over TLS in pure Rust (rustls with the RustCrypto provider and
the Mozilla root certificates; no OpenSSL, `ring` or `aws-lc`, nothing compiled from C), with a
15 s connect timeout and a 30 s stall timeout; it follows redirects but doesn't use HTTP proxies.

### By hand

MobileSAM: put `mobile_sam-tiny-vitt.safetensors` (the file pinned above) in the model folder.
SAM 3: put `model.safetensors`, `vocab.json` and `merges.txt` from `facebook/sam3` there; if both
are there, SAM 3 is used (it can do everything MobileSAM does, and text). The model folder is:
`models/sam3/` in the settings folder above, or the folder `LIGHTCRAFT_SAM3_DIR` names. Developers
can use the installer scripts, which download from Hugging Face with your token and verify the
weights:

```sh
HF_TOKEN=hf_... tools/install-sam3.sh            # macOS, Linux
tools/install-sam3.sh --check                     # verify an installation
```

```powershell
$env:HF_TOKEN = "hf_..."; .\tools\install-sam3.ps1   # Windows
```

## Licence of the model

Ember's code is MIT OR Apache-2.0; `crates/segment` (a port of Apache-2.0 code) is
Apache-2.0. **The default model's weights (MobileSAM) are Apache-2.0** too, see above: they are not
part of Ember, and Ember downloads them only when asked. **The SAM 3 weights are neither**: they are Meta's, released under the **SAM
License** (<https://github.com/facebookresearch/sam3/blob/main/LICENSE>), a custom licence that
is not an OSI open-source licence. It grants a non-exclusive, worldwide, non-transferable,
royalty-free licence to use, reproduce, distribute, copy, modify and create derivative works of
the "SAM Materials", subject to its terms, including: anyone redistributing them must provide a
copy of the licence with them, and derivative works are distributed under its terms; uses
subject to ITAR, military or warfare purposes, nuclear industries, espionage, guns or illegal
weapons, and reverse engineering are prohibited, and trade controls (sanctions, export rules)
apply; the materials come "as is", without warranty; Meta may terminate the licence on a breach
(then the materials must be deleted); California law governs. Read the licence itself — this
summary is not legal advice.

Therefore:

- **The weights are never part of this repository, its releases or installers**, and must not be
  committed.
- They reach a user's computer only when that user asks for them, after the dialog names the
  licence (or when they install them by hand).
- Whoever hosts a mirror redistributes the SAM Materials and must follow the licence (ship it
  next to the files).

## Commands (control channel, MCP)

| Command | Params | |
|---|---|---|
| `mask.add` | `{kind: "object", points?: [[x,y],…], exclude?: [[x,y],…], seg?}` | an Object mask (empty until clicked) |
| `mask.add` | `{kind: "prompt", text, seg?}` | a Describe mask |
| `mask.addComponent` | `{op: add\|subtract\|intersect, kind: "object"\|"prompt", …}` | the same as a component |
| `mask.objectPoint` | `{x, y, exclude?: bool, id?}` | one click on the selected mask's Object selection (≤ 64 clicks) |
| `mask.refineDetail` | `{id?, component?}` | the zoomed-in detail pass → `{started}` |
| `segment.prepare` | `{}` | load the model and analyze the active photo → `{busy}` |
| `segment.model.status` | `{}` | `{available, installed, model ("MobileSAM" \| "SAM 3" \| null), describe, remote, dir, loaded, busy, analyzing, sizeBytes, license, licenseUrl, mirrors, download}` (`sizeBytes`, `license`: the default download) |
| `segment.model.download` | `{acknowledged: true}` | start the download in the background → `{started}` |
| `segment.model.cancel` | `{}` | stop it (it resumes next time) → `{cancelled}` |

`remote` reports the configured endpoint, not its health. `installed` describes local weights only.

Coordinates are normalized to the uncropped, oriented photo, like every mask shape. `seg` passes
a stored segmentation (as `mask.list` / the develop settings hold it): the mask is made without
the model. Without the model, the AI kinds fail with an error starting "The AI mask model is not
installed" that says how to get it. `describe` in the status is false (and a Describe request fails at
once, naming #221) while only MobileSAM is installed.

Box prompts exist in the engine (`Session::segment_box`, `Model::segment_box`: MobileSAM only) but have
no command or tool yet: an Object selection stores clicks, and storing a box is a document-format
change that is still to be decided.

In the desktop app the commands don't wait for the model: `mask.objectPoint` returns
`{pending: true}` and the selection is applied when it's computed (one undo step per settled
selection); `mask.add {kind: "prompt"}` returns `{pending: true}` and the mask appears when
something is found (or a message says nothing was). `lightcraft-cli` and the headless MCP server
wait and return the result (when built with the `sam` feature; the default CLI has no AI masks).

## Implementation notes

- **MobileSAM** (`mobilesam.rs`, `mobilesam_prep.rs`, `model.rs`): the TinyViT encoder, mask decoder
  and prompt-encoder tables are `candle-transformers`' `segment_anything` (pure Rust); the weights are
  read with the same positional-read backend as SAM 3. Ember computes the prompt embeddings itself
  (`mobilesam_prep::sparse_embeddings`, tested against the reference formulas), because
  `candle-transformers` does not support boxes. Input: long edge to 1024 with Pillow-style bilinear,
  normalized with mean `[123.675, 116.28, 103.53]` and std `[58.395, 57.12, 57.375]`, zero-padded
  bottom and right; one click returns the best of SAM's three masks by predicted IoU, several
  clicks or a box the single mask; the decoder's 256² low-resolution logits are resampled over the
  photo (the padding never leaks) and stored as 256² logits like SAM 3's 288². If a GPU op fails the
  model moves to the CPU and retries once.
- `crates/segment` ports the Hugging Face `transformers` implementation (Apache-2.0; see
  `NOTICE` — the ported files say so and that they were modified): the 32-layer ViT backbone
  with 2-D RoPE and windowed attention, the feature pyramids, the SAM 2-style prompt encoder and
  two-way mask decoder for clicks, and the CLIP text encoder, DETR encoder/decoder (box
  relative-position bias, presence token) and pixel decoder for text. The CLIP tokenizer is a
  small BPE in `tokenizer.rs`. The downloader (original code) is the `lightcraft-fetch` crate, shared by any model that is downloaded; `fetch/` here says what SAM 3 needs.
- candle is pinned at 0.9.2: later releases make `candle-core` depend on `tokenizers` with the
  Oniguruma C library, and the product is pure Rust. Weights are read with positional reads (no
  memory map: `unsafe` stays in `lightcraft-sysmem`), only the tensors a path needs, and every
  tensor's byte range is checked against the file's length when it is opened (a truncated or
  damaged file is an error, never a crash).
- The engine (`crates/engine/src/segment/`) owns one worker thread with the model and the last
  encoded photo; the session sends it jobs and receives results over channels, never holding a
  lock the model holds. Each job runs under `catch_unwind`: a panic inside candle becomes an
  error and the model is reloaded on the next request; busy counters are reset by drop guards.
- Hostile documents: a selection keeps at most 4 detail patches (when read and when rendered),
  each at most 1024² logits; more than 64 clicks are never sent to the model.
- Accuracy: `crates/segment/tests/reference.rs` compares every stage against the Python
  reference (`tools/sam3_reference.py`); on Metal the backbone matches to 7e-5 relative error
  and masks to ~5e-5. f16 was tried and rejected (overflow, ~7 % faster).
- The engine feature `sam` (on in the desktop app, off for web and CLI) gates the model; without
  it the commands report that the build has no AI masks.

**Developers:** the accuracy test against the Python reference needs `torch` and
`transformers` (`tools/requirements-sam3-reference.txt`):

```sh
python3 -m venv .venv-sam3 && .venv-sam3/bin/pip install -r tools/requirements-sam3-reference.txt
.venv-sam3/bin/python tools/sam3_reference.py photo.jpg ref.safetensors
LIGHTCRAFT_SAM3_DIR=<model dir> LIGHTCRAFT_SAM3_REF=ref.safetensors cargo test -p lightcraft-segment --release -- --nocapture
```
