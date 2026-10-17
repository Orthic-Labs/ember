"""Train NAFNet-w16 on synthetic heteroscedastic noise over prepared clean raws.

    python -m ember_denoise.train --data /workspace/prepared --out /workspace/run1 --steps 200000 --batch 32
    python -m ember_denoise.train --data ... --out ... --resume          # continues from latest.pt

Precision: float32 weights and activations, TF32 matmul/conv allowed (``--precision tf32``, default) or strict
float32. NOT fp16/bf16 autocast: at ISO 100 the noise sigma on a 0.1 signal is ~1e-3, below bf16's resolution
(~4e-4 there) and close to fp16's after the sigma channels, so a mixed-precision run trains without error on a
network that never sees the noise it is meant to remove.

Outputs in ``--out``: ``train.log.jsonl``, ``latest.pt`` (model, EMA, optimiser, step), ``best.pt`` (lowest EMA
validation loss), ``ckpt-XXXXXXXX.pt`` (the last few), ``final.pt``. ``export.py`` turns one into safetensors.
"""

from __future__ import annotations

import argparse
import copy
import json
import math
import os
import time
from pathlib import Path

import numpy as np
import torch

from . import noise
from .demosaic import DEMOSAIC_NAME
from .nafnet import NafConfig, NafNet, count_params
from .pairs import Corpus, PairConfig, make_batch, step_generator


def learning_rate(step: int, steps: int, warmup: int, lr: float, lr_min: float) -> float:
    """Linear warm-up, then cosine decay from ``lr`` to ``lr_min`` at the last step (as the Rust trainer)."""
    if step < warmup:
        return lr * (step + 1) / warmup
    span = max(steps - warmup, 1)
    p = min(max((step - warmup) / span, 0.0), 1.0)
    return lr_min + 0.5 * (lr - lr_min) * (1.0 + math.cos(math.pi * p))


def weighted_l1(pred, clean, weight):
    return ((pred - clean).abs() * weight).mean()


@torch.no_grad()
def ema_update(ema: torch.nn.Module, model: torch.nn.Module, decay: float) -> None:
    for pe, pm in zip(ema.parameters(), model.parameters()):
        pe.mul_(decay).add_(pm.detach(), alpha=1.0 - decay)


def save_atomic(obj, path: Path) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    torch.save(obj, tmp)
    os.replace(tmp, path)


def prune(out: Path, keep: int) -> None:
    ck = sorted(out.glob("ckpt-*.pt"))
    for p in ck[:-keep] if keep > 0 else ck:
        p.unlink()


@torch.no_grad()
def validate(model, windows: torch.Tensor, pc: PairConfig, seed: int, device, batch: int) -> dict:
    """A fixed validation set: the same windows, gains, noise and (a, b) every time (generator seeded by ``seed``)."""
    model.eval()
    g = step_generator(seed, 10**9, device)
    tot_loss = tot_mse = n_tot = 0.0
    for i in range(0, windows.shape[0], batch):
        w = windows[i : i + batch]
        x, clean, weight, _ = make_batch(w, pc, g)
        pred = model(x)
        tot_loss += float(weighted_l1(pred, clean, weight)) * w.shape[0]
        tot_mse += float(((pred.clamp(0, 1) - clean) ** 2).mean()) * w.shape[0]
        n_tot += w.shape[0]
    model.train()
    return {"loss": tot_loss / n_tot, "mse": tot_mse / n_tot}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data", required=True, type=Path, help="prepare.py output")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--steps", type=int, default=200_000)
    ap.add_argument("--batch", type=int, default=32)
    ap.add_argument("--patch", type=int, default=128)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--lr-min", type=float, default=1e-6)
    ap.add_argument("--warmup", type=int, default=500)
    ap.add_argument("--ema", type=float, default=0.999, help="EMA decay of the exported weights; 0 disables")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--val-windows", type=int, default=256)
    ap.add_argument("--val-every", type=int, default=1000)
    ap.add_argument("--ckpt-every", type=int, default=2000)
    ap.add_argument("--log-every", type=int, default=50)
    ap.add_argument("--keep-ckpts", type=int, default=3)
    ap.add_argument("--precision", choices=["tf32", "fp32"], default="tf32")
    ap.add_argument("--prior", type=Path, default=None, help="JSON overriding SensorPrior fields (the calibrated prior)")
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--max-steps-this-run", type=int, default=0, help="stop after this many steps (smoke tests); 0 = all")
    a = ap.parse_args(argv)

    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    if device.type == "cpu":
        print("WARNING: no CUDA device; this will be very slow (smoke tests only)")
    tf32 = a.precision == "tf32" and device.type == "cuda"
    torch.backends.cuda.matmul.allow_tf32 = tf32
    torch.backends.cudnn.allow_tf32 = tf32
    torch.backends.cudnn.benchmark = True

    cfg = NafConfig()
    pc = PairConfig(patch=a.patch)
    if a.prior is not None:
        raw = json.loads(a.prior.read_text(encoding="utf-8"))
        raw.pop("calibrated", None)
        for k in ("full_well_e", "read_e", "adc"):
            if k in raw:
                raw[k] = tuple(raw[k])
        pc = PairConfig(patch=a.patch, prior=noise.SensorPrior(**raw))
    if a.patch % cfg.pad_multiple:
        raise SystemExit(f"--patch must be a multiple of {cfg.pad_multiple}")
    a.out.mkdir(parents=True, exist_ok=True)

    train_set, val_set = Corpus(a.data, "train"), Corpus(a.data, "val")
    val_windows = torch.from_numpy(val_set.windows(a.val_windows, pc.window_side, np.random.default_rng(a.seed + 12345))).to(device)

    model = NafNet(cfg).to(device)
    ema = copy.deepcopy(model).eval() if a.ema > 0 else None
    opt = torch.optim.AdamW(model.parameters(), lr=a.lr, betas=(0.9, 0.9), weight_decay=0.0)
    step, best = 0, float("inf")
    latest = a.out / "latest.pt"
    if a.resume:
        if not latest.is_file():
            raise SystemExit(f"--resume but {latest} does not exist")
        ck = torch.load(latest, map_location=device, weights_only=False)
        if ck["cfg"] != cfg.to_json():
            raise SystemExit("checkpoint architecture differs from this build's NafConfig")
        model.load_state_dict(ck["model"])
        if ema is not None and ck.get("ema") is not None:
            ema.load_state_dict(ck["ema"])
        opt.load_state_dict(ck["opt"])
        step, best = int(ck["step"]), float(ck["best"])
        print(f"resumed at step {step}")
    print(f"NafNet params {count_params(model)} (closed form {cfg.expected_params()}), device {device}, precision {a.precision}")
    if count_params(model) != cfg.expected_params():
        raise SystemExit("parameter count differs from the closed form: the architecture is wrong")

    meta = {"seed": a.seed, "batch": a.batch, "patch": a.patch, "steps": a.steps, "demosaic": DEMOSAIC_NAME, "prior": pc.prior.to_json(), "max_underexpose_ev": pc.max_underexpose_ev, "conditioning_jitter_ln": pc.conditioning_jitter, "ema": a.ema, "precision": a.precision, "train_files": len(train_set.entries), "val_files": len(val_set.entries)}
    (a.out / "run.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
    log = open(a.out / "train.log.jsonl", "a", encoding="utf-8")
    t0, done_this_run = time.time(), 0
    model.train()
    while step < a.steps:
        for gparam in opt.param_groups:
            gparam["lr"] = learning_rate(step, a.steps, a.warmup, a.lr, a.lr_min)
        rng = np.random.default_rng([a.seed, step])
        w = torch.from_numpy(train_set.windows(a.batch, pc.window_side, rng)).to(device, non_blocking=True)
        g = step_generator(a.seed, step, device)
        x, clean, weight, _ = make_batch(w, pc, g)
        loss = weighted_l1(model(x), clean, weight)
        if not torch.isfinite(loss):
            raise SystemExit(f"non-finite loss at step {step}: stop, do not resume from this checkpoint's successor (see docs/train-denoise.md)")
        opt.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        opt.step()
        if ema is not None:
            ema_update(ema, model, min(a.ema, (1 + step) / (10 + step)))
        step += 1
        done_this_run += 1
        if step % a.log_every == 0:
            rec = {"step": step, "loss": float(loss), "lr": opt.param_groups[0]["lr"], "it_per_s": done_this_run / (time.time() - t0)}
            log.write(json.dumps(rec) + "\n")
            log.flush()
            print(rec)
        final = step == a.steps or (a.max_steps_this_run and done_this_run >= a.max_steps_this_run)
        if step % a.val_every == 0 or final:
            v = validate(ema or model, val_windows, pc, a.seed, device, 32)
            log.write(json.dumps({"step": step, "val": v}) + "\n")
            log.flush()
            print({"step": step, "val": v})
            if v["loss"] < best:
                best = v["loss"]
                save_atomic({"cfg": cfg.to_json(), "model": (ema or model).state_dict(), "step": step, "val": v}, a.out / "best.pt")
        if step % a.ckpt_every == 0 or final:
            state = {"cfg": cfg.to_json(), "model": model.state_dict(), "ema": ema.state_dict() if ema is not None else None, "opt": opt.state_dict(), "step": step, "best": best}
            save_atomic(state, latest)
            save_atomic(state, a.out / f"ckpt-{step:08d}.pt")
            prune(a.out, a.keep_ckpts)
        if final:
            break
    save_atomic({"cfg": cfg.to_json(), "model": (ema or model).state_dict(), "step": step}, a.out / "final.pt")
    print(f"done at step {step}; best validation loss {best:.6g}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
