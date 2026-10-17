# Training Ember's own denoiser on public data (Vast runbook)

Status, 2026-10-17: kit written and **not run**. No build, test, download or training has executed; nothing here is a result. Decision (Adrian, 2026-10-17): train our own NAFNet width-16 on publicly available, permissively licensed data on the Vast 3090 after the current exports finish. No Megvii correspondence and no Megvii weights. Issue [#200](https://github.com/Orthic-Labs/ember/issues/200).

Code: `tools/train-denoise/` (Python, PyTorch; tooling, not product code, never shipped). It complements `crates/denoise-train` (Rust on candle, CPU/Metal, no CUDA build) and feeds the existing Candle loader in `crates/nafnet` (`NafModel::open`) unchanged.

## What the kit is

| Piece | File | Mirrors |
| --- | --- | --- |
| Licence manifest and gate | `datasets.json`, `ember_denoise/manifest.py` | policy: CC0, CC-BY or similar only |
| Raw to RGGB mosaics with Ember's own decoder (Rust) | `denoise-train export-mosaics` (`crates/denoise-train/src/export_mosaics.rs`) | the product's `lightcraft-raw` decode |
| Corpus index from the mosaics, split | `ember_denoise/prepare.py`, `splits.py` | `split.rs` (same salt, same hash rule) |
| Noise `y = a·Poisson(x/a) + sqrt(b)·N(0,1)` on the mosaic | `ember_denoise/noise.py` | `noise.rs`, `pipeline::noise` (`var(Y)=a·Y+b`) |
| Demosaic of noisy and clean mosaic (GPU) | `ember_denoise/demosaic.py` | `data.rs` (Rust uses PPG; see risks) |
| Pairs, exposure, dihedral, loss weights | `ember_denoise/pairs.py` | `data.rs` |
| NAFNet-w16 (original, from arXiv 2204.04676) | `ember_denoise/nafnet.py` | `crates/nafnet/src/model.rs` |
| Training loop | `ember_denoise/train.py` | `denoise-train train` |
| safetensors + `contract.json` export | `ember_denoise/export.py` | `crates/nafnet/src/contract.rs` |
| Tests | `tests/` | |

Conditioning: the network input is 6 channels, noisy linear camera RGB plus `sqrt(a·max(y,0)+b)·16` per channel, with `(a, b)` jittered log-normally (sigma 0.15) at train time so the `(a, b)` estimated by `pipeline::noise` at inference does not hurt. Noise sampling: per patch, ISO log-uniform 100 to 25 600, full well 20k to 90k e-, read noise 1.5 to 6 e-, ADC floor 5e-5 to 3e-4; `a = (ISO/100)/full_well`, `b = (read_e·a)² + adc²`. **These ranges are plausible, not calibrated** (see step 0).

## Datasets (licences read from the source pages on 2026-10-17)

Accepted:

| Source | Licence | Licence page |
| --- | --- | --- |
| Ember corpus: CC0 raws from raw.pixls.us (`xtask corpus`, pinned by SHA-256) | CC0 1.0 | <https://creativecommons.org/publicdomain/zero/1.0/> (site: <https://raw.pixls.us/>) |

Rejected (a model trained on them could not ship):

| Source | Why | Licence page |
| --- | --- | --- |
| MIT-Adobe FiveK | Adobe licences grant rights "solely for your own research purposes" and bar commercial advantage | <https://data.csail.mit.edu/graphics/fivek/legal/LicenseAdobe.txt>, <https://data.csail.mit.edu/graphics/fivek/legal/LicenseAdobeMIT.txt> |
| RAISE | "non-commercial research and educational purposes" | <https://loki.disi.unitn.it/RAISE/download.html> |
| PPR10K | "non-commercial research purposes only" (code is Apache-2.0, data is not) | <https://github.com/csjliang/PPR10K> |

Not accepted, licence not established (blocked in `prepare.py` until a page is read and the entry is edited): SIDD (MIT stated on the project page only, no licence file seen; small phone sensors), NIND on Commons (per-file licences not read; already-developed JPG/PNG, so not a raw mosaic), rawNIND (the Dataverse landing page did not show its licence field; the code is GPL-3.0 per the #200 survey, which says nothing about the data), Wikimedia Commons raws (per-file licences; only CC0 or CC-BY files qualify, never CC-BY-SA; no category listing could be fetched). No specific "signal-processing CC0 raw set" was found in the sources fetched, so none is listed. Details and evidence strings: `datasets.json`.

Consequence: today the only accepted clean source is the pixls CC0 corpus. It has few scenes (about a hundred files across many cameras, Bayer only, recorded ISO up to 800), so after the 75/10/15 split the training set is small and the network will see few distinct images. After `prepare` read the counts it prints; if train has well under about 50 usable files, treat the run as a smoke of the pipeline, not a candidate, and add accepted raws first (a verified CC0/CC-BY set, or individually verified Commons files added to `datasets.json` with licence URL and attribution). Any CC-BY source needs its attribution carried into the model card.

## Step 0, before any paid hour (free, local)

1. **Calibrate the noise prior on real high-ISO raws.** Issue #200 requires this. The corpus tops out near ISO 1250, so the prior's upper half is untested. Fit `(a, b)` per ISO from real raws (rawNIND if its licence page is read and is CC0 or CC-BY, or own raws; `pipeline::noise` fits the same model) and write the ranges to a JSON file of `SensorPrior` fields (`iso_min`, `iso_max`, `base_iso`, `full_well_e`, `read_e`, `adc`). Pass it as `--prior`. Without it `contract.json` says `noise_prior_calibrated: false`, and that must be treated as a gate failure for shipping.
2. **Decide the demosaic contract.** The product's learned-RGB colour contract is still open (`denoise-qualification.md` gate item 2). Training demosaics noisy mosaics with Malvar-He-Cutler on the GPU; the Rust trainer uses PPG; the product feeds whatever its pipeline demosaics. A mismatch still runs and still scores well on synthetic data, and denoises real data worse. Fix which demosaic the product uses before spending, or accept and record the gap.
3. **Export the mosaics with Ember's decoder, no third-party raw decoder** (decision ember-4a: the trainer sees the same decode as the product, and the LGPL question does not arise). The owner builds `denoise-train` through the repo's Rust wrapper (dev profile, owner chat only) and runs `denoise-train export-mosaics --in corpus/raw --out D:\scratch\ember-denoise\mosaics`. Per raw it writes `<sha256>.npy` (uint16 `[h, w]`, black-subtracted, white-normalised, cropped to the RGGB phase) and `<sha256>.json` (CFA pattern before and after the crop, black and white levels, white balance, ISO, make, model); non-Bayer files and undecodable files are listed in `skipped.json`. Do this locally and rsync the mosaics (they are far smaller than building on the instance, which has no cargo); to run it on the instance instead, the binary must be built there by its owner and `--allow-outside-scratch` is needed for an output under `/workspace`.
4. **Local checks, on the owner machine** (these were not run by the author): `cd tools/train-denoise && python -m pytest -q` (CPU torch is enough; the noise test draws 400 000 samples per case).
5. **Pins.** `requirements.txt` pins are unverified against the Vast image; run `pip install --dry-run -r requirements.txt` on the instance first.

## Vast run (3090)

Follow `D:\Claude\docs\rules\paid-compute.md` and `vast-access.md`: the billing clock starts at create and ends at destroy; never end a turn with the instance idle; sync evidence off before destroy. Disk preflight: `latest.pt` + `ckpt` x3 are about 35 MB each (2.19 M parameters; weights, EMA and two optimiser moments), the prepared corpus is about 2 bytes per raw pixel (a 24 MP file is 48 MB), so reserve corpus size + 1 GB.

On the owner machine, ship the exported mosaics of the CC0 corpus (the instance has no cargo and no reason to download):

```text
rsync -a /d/scratch/ember-denoise/mosaics/ <vast>:/workspace/mosaics/    # export-mosaics output, step 0.3
rsync -a tools/train-denoise/ <vast>:/workspace/train-denoise/
rsync -a /d/scratch/ember-denoise/prior.json <vast>:/workspace/prior.json     # step 0 output
```

On the instance, in order:

```text
cd /workspace/train-denoise
pip install -r requirements.txt            # keep the image's CUDA torch
python -m pytest -q
python -m ember_denoise.prepare --source-id ember-corpus-pixls --mosaic-dir /workspace/mosaics --out /workspace/prepared
```

Smoke (Rule 3 of paid-compute: it must cross the changed transition, here resume): the change is a new trainer, and this smoke crosses train, checkpoint, resume, validate and export because it runs 200 steps, stops, resumes to 400, and exports.

```text
python -m ember_denoise.train --data /workspace/prepared --out /workspace/smoke --steps 400 --max-steps-this-run 200 --val-every 100 --ckpt-every 100 --prior /workspace/prior.json
python -m ember_denoise.train --data /workspace/prepared --out /workspace/smoke --steps 400 --val-every 100 --ckpt-every 100 --prior /workspace/prior.json --resume
python -m ember_denoise.export --ckpt /workspace/smoke/final.pt --run /workspace/smoke/run.json --data /workspace/prepared --out /workspace/smoke/model --parity
```

Read `it_per_s` from `train.log.jsonl`, then set the real step count from `steps = hours x 3600 x it_per_s`.

**Exact launch command (owner), after the smoke passes, from `/workspace/train-denoise`:**

```text
nohup python -m ember_denoise.train --data /workspace/prepared --out /workspace/run1 --steps 200000 --batch 32 --patch 128 --ema 0.999 --prior /workspace/prior.json > /workspace/run1.out 2>&1 &
```

If it stops, the same command with `--resume` appended continues from `latest.pt` (the stream of batches is a pure function of seed and step). Watch with a harness-tracked monitor (not a bare loop); a non-finite loss stops the run on purpose, so look at `train.log.jsonl` before resuming. Export the result:

```text
python -m ember_denoise.export --ckpt /workspace/run1/best.pt --run /workspace/run1/run.json --data /workspace/prepared --out /workspace/run1/model --parity
rsync -a <vast>:/workspace/run1/model/ /d/scratch/ember-denoise/run1/model/   # then destroy the instance
```

Expected time (**an estimate, unverified**): about 0.8 GFLOP forward per 128 px patch, so roughly 77 GFLOP per step at batch 32 including backward. A 3090 with these small channel counts is memory-bound, not compute-bound; guess 4 to 8 steps per second in TF32, so 200 000 steps is about 7 to 14 hours, call it 8 to 16 GPU-hours with validation and data loading. Measure `it_per_s` in the smoke and recompute before committing. Check the hourly rate on the instance offer; no price is claimed here. With a corpus this small, 200 000 steps will likely overfit before it finishes; `best.pt` (lowest validation loss) is what exports, so a shorter run is a legitimate outcome.

## What the weights must satisfy (Candle loader spec)

`model.safetensors`: F32, exactly the tensor names and shapes in `NafConfig.expected_tensors()` (`nafnet.py`), which `export.py` checks before writing and after reading back: `intro`, `ending`, `encoders.{i}.{j}.*`, `middle_blks.{j}.*`, `decoders.{i}.{j}.*`, `downs.{i}`, `ups.{i}.0` (1x1 conv, no bias, then pixel shuffle) and per block `norm1/2`, `conv1..5`, `sca.1`, `beta`, `gamma` (`[1, c, 1, 1]`); 2 190 947 parameters at width 16. `contract.json`: `format_version` 1, `model` (the `NafConfig` fields), `sigma_scale` 16, which `crates/nafnet::Contract::validate` requires, plus provenance (sources and licences, checkpoint and weights SHA-256, noise prior, demosaic) and `qualified: false`. Install by placing both files in a folder and opening it with `NafModel::open`; the engine install path still takes ONNX only and no pipeline stage calls the backend (`local-ai-delivery.md`), so this does not by itself put a denoiser in the app.

`--parity` writes `parity-input.npy` and `parity-output.npy`. Not done: a Rust test reading them to compare `NafModel` against the PyTorch forward to about 1e-4. Add it before trusting that the two graphs agree numerically; the tensor-name check proves loading, not equal output.

## Qualification gates after training (nothing is qualified by training)

Run on the owner machine, not on the metered instance:

1. Weights load: `NafModel::open` on the exported folder, and the parity comparison above.
2. Eval on held-out files: `denoise-train eval --weights <run>/model/model.safetensors --corpus corpus/raw --out D:\scratch\ember-denoise\run1\eval.json` through the repo's Rust wrapper (dev profile). The split is the same content-hash split, so held-out files were never trained or validated on (check `splits.json` from `prepare` against the Rust split listing). Status stays `UNQUALIFIED`; its provisional thresholds (never worse than noisy input, at least 2 dB at ISO 3200 and up, bias at most 0.003, chroma shift at most 0.02, tiled difference at most 0.02, no invented structure in flats, no added overshoot, texture energy 0.3 to 1.5) are necessary, not sufficient.
3. The five learned-RGB gate items in `denoise-qualification.md`: checkpoint rights record (ours: trained by us on the accepted sources above, listed in `contract.json`), colour/transfer contract against an independent reference, whole versus tiled output, latency and memory on named Mac and Windows targets, consented shoot-disjoint camera/ISO strata against classical NR (#201) with human preference.
4. Real-noise check the synthetic eval cannot give: real high-ISO raws of cameras not in the training files. A network that passes only on synthetic noise from its own training model has shown nothing about real sensors.
5. Licence re-check at ship time: each accepted source's page read again, CC-BY attributions carried into the model card, and the datasets that were unverified or rejected confirmed absent from `contract.json` `sources`.

## Known risks, stated so a passing run is not over-read

* Synthetic noise from a model the network was trained on scores well by construction. Only gate 4 and the consented strata test real behaviour.
* Clean targets are base-ISO raws that carry their own small native noise, so the network learns a floor it cannot go below.
* The corpus is small (see Datasets); held-out has only a handful of files, so its numbers have wide error.
* Training runs in demosaiced camera RGB with no white balance or colour matrix; the product's scene-linear Rec. 2020 path needs a defined conversion (gate item 2).
* The EMA of the weights (decay 0.999) and AdamW betas (0.9, 0.9) differ from the Rust trainer; weights from the two trainers are not interchangeable evidence.
* `torch.poisson` on CUDA is not bit-reproducible across GPU models; a resumed run on a different GPU follows the same recipe, not the same bits.
