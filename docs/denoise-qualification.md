# Offline denoise qualification

Status, 2026-10-10: procedural CPU baseline measured through generated CI. Production Ember has classical RGB luminance/chroma noise reduction plus a per-photo noise estimate & Auto Noise Reduction ([`noise-reduction.md`](noise-reduction.md)); no learned denoiser is qualified.

## Procedural baseline

```text
lightcraft-cli ai denoise baseline --hardware M6_CPU --source-revision REVISION --out NEW_REPORT.json --size 128 --repeats 3
```

Run through generated RightKit native Actions. `--size` accepts 64..512 & `--repeats` accepts 2..10 warm calls after one first call; defaults are 128 & 3. Hardware/revision labels are supplied metadata, not independently verified machine/source identities. Output is create-new, capped numeric JSON. No input files, personal photos, catalog, model, provider or reference-product assets are accessed.

Original procedural linear Rec.2020 patterns cover flat shadows, color patches/gradient, hard step edges, fine texture & intentionally soft edges. Fixed-seed synthetic signal-dependent/read noise supplies repeatable noisy inputs; its constants do not represent a calibrated camera. Identity output & production `pipeline::local::denoise` at amounts 0, 25, 50, 75 & 100 are compared against each clean pattern. Both luminance & color NR receive the amount; remaining detail controls keep defaults. Source/output longest sides match, so preview scaling is not part of this condition.

Metrics include RGB MSE/RMSE, peak-1 PSNR, signed channel bias & gradient RMSE against clean input. Zero MSE has no finite PSNR & is represented explicitly. Finite production output is measured without extra clipping; out-of-range samples are counted & nonfinite output fails. Identity establishes injected error; clean edges, texture & soft edges expose smoothing or invented-detail distortion. Pixel metrics alone cannot determine photographic preference.

Receipts bind dimensions plus little-endian F32 clean/noisy/output pixels by SHA-256 without storing images. First-call & repeated p50/p95 timing covers NR only; fresh image clones, procedural generation, metrics & hashing are outside that scope. First call is not cold-process/cache timing. Repeated-output maximum difference & tolerance expose numerical drift. All reports remain `UNQUALIFIED`, including numerically perfect or deterministic results.

## Learned RGB candidate gate

[Candidate record](models/denoise-candidates.md) selects full NAFNet-SIDD-width32 as first architecture hypothesis, with pinned source/config & separately unresolved first-party checkpoint permission.

1. Pin first-party architecture/config/checkpoint identity, code licence, checkpoint permission & training-data scope separately. A converted-model card's licence label cannot alone prove first-party checkpoint rights. Record original vs converted precision/operator differences.
2. Establish exact color/transfer, channel order, input range, normalization, pad/crop, tile overlap, global-statistic & output-clamp contracts. Display RGB models cannot silently receive Ember's scene-linear Rec.2020 buffer. Define any color conversion & compare against an independent reference.
3. Compare whole-image vs tiled output, boundaries, bright/shadow color, skin, texture, soft focus & hallucinated structure. Record amount blending separately; intensity is an Ember contract unless model supports it directly.
4. Measure first/warm preview & full-resolution latency, peak memory, cancellation, cache identity & source preservation on named Mac/Windows targets. Operator availability or advertised mobile latency does not qualify Ember performance.
5. Evaluate consented shoot-disjoint camera/ISO strata, including intentional blur, alongside classical NR. Freeze settings on training/validation shoots; use held-out rendered comparison & human preference before any promotion.

An initial RGB candidate is post-demosaic. It cannot establish raw-mosaic reconstruction parity. Future joint raw denoise requires separate sensor/noise/calibration data & camera coverage. Shipping also requires generated CI, exact-artifact hidden native QA & explicit review/acceptance behavior.

## Next evidence

Fork Actions are restored & now produce baseline receipts on candidate builds. Keep source files & catalog authoritative; experimental intermediates belong in disposable cache state.

### CI evidence, run 38055623316

[Mac & Windows candidate receipts](https://github.com/Orthic-Labs/lightcraft/actions/runs/38055623316) completed from source revision `93678dd1b9977d6ee509679be164fe2ec3b44113`. Both used 128×128 procedural input, one first call plus three warm calls, CPU-only processing, zero repeatability errors & zero out-of-range channels. Receipts remain alwaysUNQUALIFIED.

| Case | Identity MSE / gradient / PSNR | Amount 100 MSE / gradient / PSNR | Best sampled amount by MSE |
| --- | --- | --- | --- |
| Flat shadows | 1.734e-5 / 5.717e-3 / 47.61 dB | 1.718e-6 / 1.558e-3 / 57.65 dB | 100 |
| Color patches/gradient | 1.225e-4 / 1.564e-2 / 39.12 dB | 6.916e-3 / 3.154e-2 / 21.60 dB | 0 |
| Hard step edge | 1.618e-4 / 1.796e-2 / 37.91 dB | 2.615e-4 / 8.072e-3 / 35.83 dB | 50 |
| Fine texture | 1.530e-4 / 1.744e-2 / 38.15 dB | 1.108e-3 / 4.708e-2 / 29.56 dB | 25 |
| Intentional soft edge | 1.492e-4 / 1.723e-2 / 38.26 dB | 7.502e-5 / 4.395e-3 / 41.25 dB | 75 |

Mac host metadata was Apple M1 (Virtual), 3 CPUs & 7,516,196,864 bytes memory; nonzero-NR warm p50 ranged 0.391–0.482 ms & p95 0.392–0.784 ms. Windows metadata was AMD EPYC 7763 64-Core Processor, 4 CPUs & 17,174,360,064 bytes; warm p50 ranged 1.842–2.319 ms & p95 1.955–2.436 ms. Mac & Windows CLI hashes were fbce434888b0892eb8c8cbb5b90a0625d5246d350bf9dc8c404fcaeb7170e2c3 & 30b3e43909d95f0320c92f671e23da40bc114e3180ec05f841c3c2533ea54915.

Cross-platform pixel SHA-256 values are not bit-identical, including noisy/output buffers & one soft-edge clean buffer, so receipts bind each platform independently. Results describe synthetic RGB guided-NR behavior only; they provide no photographic, learned-model or Metal evidence.

Next compare a rights-cleared RGB candidate against identical procedural & consented photographic conditions.

## Own-trained denoiser: training and eval tooling (#200)

Status, 2026-10-16: tooling written and unit-tested in source; **nothing has been trained and no result exists** (no build, test or training has run yet; CI verifies the source). Scope is decision A only: the CC0 raw corpus, no consented shoots, no paid compute, hardware we own. Everything below lives in `crates/denoise-train` (`lightcraft-denoise-train`), a tool crate registered `Exempt` in `xtask layers`; no product crate depends on it and its outputs are never committed (`*.safetensors` is gitignored).

### Run it

```text
cargo run -p xtask -- corpus --download                                  # CC0 raws into corpus/raw (gitignored)
denoise-train splits --corpus corpus/raw                       # show the hash split; warns on an empty split
denoise-train train  --corpus corpus/raw --out D:\scratch\ember-denoise\run1 --steps 200000 --device cpu
denoise-train train  --corpus corpus/raw --out D:\scratch\ember-denoise\run1 --resume --steps 200000
denoise-train eval   --weights D:\scratch\ember-denoise\run1\best.safetensors --out D:\scratch\ember-denoise\run1\eval.json
```

Build and run the binary (`-p lightcraft-denoise-train`, bin `denoise-train`) through the repo's Rust wrapper in the dev profile; on a CPU that is slow, so try a small `--steps` first. `--device metal` needs `--features metal` on macOS; CUDA is not built in (it would add a CUDA stack to the workspace lock). Output paths must be under `D:\scratch` or `/Volumes/D/scratch`.

### Data

* **Splits** (`split.rs`): per file, by `SHA-256(salt || SHA-256(file bytes)) mod 1000`: 15% held-out, 10% validation, 75% train (`--heldout-permille`, `--val-permille`). The unit is content, so renames never move a file, byte-identical copies share a split and are listed once, and adding files never reassigns existing ones. `verify()` rejects an empty split or a hash in two splits; `verify_assignments()` re-derives every assignment. Training reads `train` only, validation reads `val` only, `eval` reads `heldout` only (taken from the run's `splits.json`). Caveat: two different files of one scene (a burst) can land in different splits; keep the corpus one frame per scene. The corpus is small, so a tiny corpus can leave a split empty and `splits` says so.
* **Pairs** (`data.rs`): clean = demosaic (PPG) of a black-subtracted, white-normalised CC0 Bayer crop (sources recorded above ISO 800, X-Trans and linear DNGs are skipped); noisy = demosaic of the same mosaic after a Poisson-Gaussian noise draw, clipped at white. Random exposure reduction of 0 to 3 stops, the 8 flips/rotations, 128 px patches, fresh noise on every draw, every example seeded from `(seed, step, index)`.
* **Noise** (`noise.rs`): `y = a·Poisson(x/a) + sqrt(b)·N(0,1)`, so `E[y] = x`, `var(y) = a·x + b`. Per ISO: `a = (ISO/base)/full_well_e`, `b = (read_e·a)² + adc²`, with camera and ISO drawn log-uniformly from `SensorPrior` (ISO 100 to 25 600, full well 20k to 90k e-, read noise 1.5 to 6 e-). These ranges are plausible, **not calibrated** to any camera.
* **Conditioning**: the network gets noisy linear camera RGB plus `sqrt(a·max(y,0)+b)·16` per channel (6 input channels); `(a, b)` are jittered log-normally (sigma 0.15) at train time so an estimated `(a, b)` from `pipeline::noise` at inference does not hurt.

### Model and weights

`model.rs` is NAFNet on candle with the reference parameter names and PyTorch layouts: `intro`, `ending`, `encoders.{i}.{j}`, `middle_blks.{j}`, `decoders.{i}.{j}`, `downs.{i}`, `ups.{i}.0`, per block `norm1/2`, `conv1..5`, `sca.1`, `beta`, `gamma`. Width 16, encoder `[1,1,1,2]`, 3 middle, decoder `[1,1,1,1]`: 2 190 947 parameters (a test checks this against the closed form `7c²+33c` per block). Differences from the reference: 6 input channels (the global skip adds the first three), and the final convolution starts at zero weights so training starts from the identity. Weights are plain F32 safetensors with a `contract.json` beside them (architecture config, input/output contract, noise prior, split counts).

**Correction to the delivery plan:** `crates/denoise`'s ONNX path loads plain U-Nets (no depthwise convolution, channel attention or LayerNorm), so it cannot run this network. Training is Rust on candle, not PyTorch as `local-ai-delivery.md` first sketched. 2026-10-17 addendum: because the candle trainer has no CUDA build, the GPU run on the Vast 3090 uses a PyTorch trainer, `tools/train-denoise`, that writes the same tensor names and `contract.json`; see [`train-denoise.md`](train-denoise.md) for the licensed-data manifest, the runbook and the gates.

### Inference backend (#200, second half)

The model definition moved out of the training crate into `crates/nafnet` (`lightcraft-nafnet`, registered Layer 1, ordered before `denoise`); `denoise-train::model` re-exports it, so the trained graph and the product graph are one piece of code (same parameter names and layouts, depthwise conv, SimpleGate, simplified channel attention, LayerNorm2d, `model_input_chw` noise map). `lightcraft-denoise` feature `nafnet` (off by default) adds `naf::NafDenoiser::open_installed(dir)`: `None` for a folder without `contract.json` beside `model.safetensors` (ONNX behaviour unchanged), `Err` for a malformed contract (bad JSON, unsupported `format_version`, channel counts or `sigma_scale` that are not this build's, invalid architecture) or a missing or wrongly shaped tensor. Files are size-capped (contract 1 MiB, weights 512 MiB). `denoise_rgb` takes noisy linear camera RGB planes and `(a, b)` and runs whole-image or tiled (overlap blended with linear ramps).

Source tests (unrun until CI): random-init weights saved and reloaded give outputs identical to the training-side forward; tiled equals whole within 1e-5 when the residual is tile-local (zeroed final convolution); a missing tensor and each malformed-contract case return `Err`. **Not established:** tiled-versus-whole difference for a trained network (channel attention sees only the tile; the eval's 128/32 tiled check measures it), and Metal tolerance. **Not wired:** the engine install path still takes `.onnx` plus `denoise-model.json`, and no pipeline stage calls the backend; a `Domain` for demosaiced RGB and the installer change are the next step, gated on the colour contract (item 2).

### Eval report (`eval.json`)

On held-out files only, per ISO stratum (800 to 25 600 on a fixed reference camera): MSE, PSNR, display-encoded PSNR, gradient RMSE, signed per-channel bias and R/G, B/G shift, each for the noisy input and the network. Plus whole-image vs tiled difference (128 px tiles, 32 px overlap), procedural flats, steps, soft edge and texture at ISO 3200 and 12 800 (structure left in flats, edge overshoot, gradient energy kept), and warm p50/p95 latency of one 512 px whole-image call on the chosen device.

`status` is always `UNQUALIFIED`. The `checks` thresholds are provisional values proposed by the tool (never worse than the noisy input, at least 2 dB at ISO 3200 and up, bias at most 0.003, chroma shift at most 0.02, tiled difference at most 0.02, no invented structure in flats, no added overshoot, texture energy 0.3 to 1.5); they are not ratified here, and passing them is necessary evidence only.

Not measured: human preference and the classical-NR comparison (gate item 5), peak memory and named Mac/Windows latency (item 4), the product colour contract (item 2: training runs in demosaiced camera RGB, not scene-linear Rec.2020, until that contract is fixed), and consented camera/ISO strata.
