"""Export a trained checkpoint to the folder the product loads: ``model.safetensors`` + ``contract.json``.

    python -m ember_denoise.export --ckpt run1/best.pt --run run1/run.json --data prepared --out run1/model

The tensor names, shapes and dtype are checked against ``NafConfig.expected_tensors()`` (derived independently of the
module code from ``crates/nafnet/src/model.rs``) BEFORE writing, and the written file is read back and checked again.
``contract.json`` has the fields ``crates/nafnet::Contract`` requires (``format_version`` 1, ``model``,
``sigma_scale`` 16) plus provenance. It always says ``qualified: false``: only the gates in docs/train-denoise.md can
change that, and not in this file.

``--parity`` also writes ``parity-input.npy`` / ``parity-output.npy`` (a deterministic [1,6,40,56] input and this
model's output) so a Rust test can compare ``NafModel::open`` against the PyTorch forward to ~1e-4.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import torch
from safetensors import safe_open
from safetensors.torch import save_file

from . import manifest
from .nafnet import NafConfig, NafNet, count_params
from .noise import SIGMA_SCALE

FORMAT_VERSION = 1


def check_tensors(tensors: dict, cfg: NafConfig) -> None:
    """Raise ValueError unless ``tensors`` has exactly the expected names, shapes and float32 dtype."""
    want = cfg.expected_tensors()
    got = {k: tuple(v.shape) for k, v in tensors.items()}
    missing = sorted(set(want) - set(got))
    extra = sorted(set(got) - set(want))
    wrong = sorted(k for k in want if k in got and want[k] != got[k])
    if missing or extra or wrong:
        raise ValueError(f"tensor mismatch: missing {missing[:5]}, extra {extra[:5]}, wrong shape {[(k, got[k], want[k]) for k in wrong[:5]]}")
    bad = sorted(k for k, v in tensors.items() if v.dtype != torch.float32)
    if bad:
        raise ValueError(f"tensors must be float32: {bad[:5]}")
    nonfinite = sorted(k for k, v in tensors.items() if not bool(torch.isfinite(v).all()))
    if nonfinite:
        raise ValueError(f"non-finite weights: {nonfinite[:5]}")


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def export(ckpt: Path, out: Path, run_json: Path | None, data: Path | None, parity: bool) -> Path:
    ck = torch.load(ckpt, map_location="cpu", weights_only=False)
    c = ck["cfg"]
    cfg = NafConfig(c["in_channels"], c["out_channels"], c["width"], c["middle_blocks"], tuple(c["enc_blocks"]), tuple(c["dec_blocks"]))
    cfg.validate()
    state = {k: v.detach().to(torch.float32).contiguous().clone() for k, v in ck["model"].items()}
    check_tensors(state, cfg)
    model = NafNet(cfg)
    model.load_state_dict(state, strict=True)
    if count_params(model) != cfg.expected_params():
        raise ValueError("parameter count differs from the closed form")
    out.mkdir(parents=True, exist_ok=True)
    weights = out / "model.safetensors"
    save_file(state, str(weights), metadata={"format": "pt"})
    with safe_open(str(weights), framework="pt") as f:  # read back what was written
        back = {k: f.get_tensor(k) for k in f.keys()}
    check_tensors(back, cfg)
    for k in state:
        if not torch.equal(state[k], back[k]):
            raise ValueError(f"{k} changed on the round trip")

    run = json.loads(run_json.read_text(encoding="utf-8")) if run_json else {}
    sources = {}
    if data is not None:
        idx = json.loads((data / "index.json").read_text(encoding="utf-8"))
        m = manifest.load()
        sid = idx["source_id"]
        s = manifest.require_accepted(sid, m)  # refuse to export a model trained on a source that is not accepted
        sources = {sid: {"licence": s["licence"], "licence_url": s["licence_url"], "files": len(idx["files"]), "train_files": sum(f["split"] == "train" for f in idx["files"])}}
    contract = {
        "format_version": FORMAT_VERSION,
        "model": cfg.to_json(),
        "weights_format": "safetensors, F32, PyTorch NAFNet parameter names and layouts (crates/nafnet model.rs)",
        "input": f"[6, h, w]: channels 0..3 noisy linear camera RGB (white level 1, no white balance, no colour matrix); channels 3..6 sqrt(a*max(y,0)+b) per channel times {SIGMA_SCALE:g}",
        "output": "[3, h, w] = input RGB + network residual; zero-pad right/bottom to a multiple of 2^stages, crop after",
        "sigma_scale": SIGMA_SCALE,
        "loss": "mean over samples of |pred - clean| / (sqrt(clamp(clean, 0, 1)) + 0.03)",
        "domain": "demosaiced camera RGB (not the product scene-linear Rec.2020: an unresolved item of the learned-RGB gate)",
        "trainer": "tools/train-denoise (PyTorch)",
        "training": run,
        "checkpoint": {"file": ckpt.name, "sha256": sha256_file(ckpt), "step": ck.get("step")},
        "weights_sha256": sha256_file(weights),
        "sources": sources,
        "noise_prior_calibrated": bool(run.get("prior", {}).get("calibrated", False)),
        "qualified": False,
    }
    (out / "contract.json").write_text(json.dumps(contract, indent=2), encoding="utf-8")
    if parity:
        g = torch.Generator().manual_seed(7)
        x = torch.rand(1, cfg.in_channels, 40, 56, generator=g) * 0.5
        model.eval()
        with torch.no_grad():
            y = model(x)
        np.save(out / "parity-input.npy", x.numpy())
        np.save(out / "parity-output.npy", y.numpy())
    print(f"exported {len(state)} tensors, {count_params(model)} parameters to {out}")
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--ckpt", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--run", type=Path, default=None, help="run.json written by train.py")
    ap.add_argument("--data", type=Path, default=None, help="prepare.py output (records the training sources and refuses unaccepted ones)")
    ap.add_argument("--parity", action="store_true")
    a = ap.parse_args(argv)
    export(a.ckpt, a.out, a.run, a.data, a.parity)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
