"""Training pairs: clean mosaic windows from prepared files -> (noisy mosaic, clean mosaic) -> demosaic -> network
input, target and loss weights, all on the GPU except the window crop.

Mirrors ``crates/denoise-train/src/data.rs``: random exposure reduction of 0..3 stops, noise on the mosaic clipped at
white, both demosaiced, the clean target clipped to [0, 1], 8 dihedral transforms applied to the final pair, loss
weight ``1 / (sqrt(clamp(clean, 0, 1)) + 0.03)``, ``(a, b)`` jittered log-normally (sigma 0.15) for the conditioning
only. Every batch is a pure function of ``(seed, step)`` so a resumed run continues the same stream.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import torch

from . import noise
from .demosaic import mhc_demosaic

MARGIN = 8  # pixels dropped from every side after demosaicing (as the Rust trainer)


@dataclass(frozen=True)
class PairConfig:
    patch: int = 128
    max_underexpose_ev: float = 3.0
    conditioning_jitter: float = noise.CONDITIONING_JITTER_LN
    prior: noise.SensorPrior = noise.SensorPrior()

    @property
    def window_side(self) -> int:
        return self.patch + 2 * MARGIN


class Corpus:
    """The prepared files of one split (``prepare.py`` output): memory-mapped uint16 mosaics, RGGB phase."""

    def __init__(self, root: Path | str, split: str):
        self.root = Path(root)
        index = json.loads((self.root / "index.json").read_text(encoding="utf-8"))
        self.entries = [e for e in index["files"] if e["split"] == split]
        if not self.entries:
            raise ValueError(f"split {split!r} has no files in {self.root} (corpus too small for the split shares?)")
        self.split = split
        self._maps: dict = {}

    def mosaic(self, i: int) -> np.ndarray:
        m = self._maps.get(i)
        if m is None:
            m = np.load(self.root / self.entries[i]["npy"], mmap_mode="r")
            self._maps[i] = m
        return m

    def windows(self, n: int, side: int, rng: np.random.Generator) -> np.ndarray:
        """``n`` float32 windows ``[n, 1, side, side]`` in [0, 1] at even offsets (so the CFA phase stays RGGB).
        Almost-black (mean < 0.004) or clipped (> 10 % at white) windows are redrawn, at most 12 tries each."""
        out = np.empty((n, 1, side, side), dtype=np.float32)
        for k in range(n):
            for _ in range(12):
                i = int(rng.integers(len(self.entries)))
                m = self.mosaic(i)
                h, w = m.shape
                if h < side or w < side:
                    continue
                y0 = int(rng.integers((h - side) // 2 + 1)) * 2
                x0 = int(rng.integers((w - side) // 2 + 1)) * 2
                win = np.asarray(m[y0 : y0 + side, x0 : x0 + side], dtype=np.float32) / 65535.0
                if win.mean() < 0.004 or (win >= 0.999).mean() > 0.1:
                    continue
                out[k, 0] = win
                break
            else:
                raise RuntimeError("no usable window after 12 tries: corpus is too dark, clipped or small for this patch size")
        return out


def dihedral(x: torch.Tensor, code: int) -> torch.Tensor:
    """One of the 8 dihedral transforms of ``[..., h, w]`` (``code`` 0..7; 0 is the identity)."""
    if code & 4:
        x = x.transpose(-1, -2)
    if code & 1:
        x = x.flip(-1)
    if code & 2:
        x = x.flip(-2)
    return x


def loss_weights(clean: torch.Tensor) -> torch.Tensor:
    return 1.0 / (clean.clamp(0.0, 1.0).sqrt() + 0.03)


def make_batch(windows: torch.Tensor, cfg: PairConfig, generator: torch.Generator, fixed_params=None):
    """``windows``: ``[n, 1, side, side]`` clean mosaics on the target device. Returns ``(x, clean, weight, (a, b))``
    with ``x`` ``[n, 6, p, p]``. ``fixed_params`` = ``(a, b)`` tensors ``[n]`` overrides the prior draw (validation
    strata); gain is still drawn."""
    n = windows.shape[0]
    dev = windows.device
    ev = torch.rand(n, generator=generator, device=dev) * cfg.max_underexpose_ev
    gain = torch.exp2(-ev).reshape(n, 1, 1, 1)
    clean_m = (windows * gain).clamp(0.0, 1.0)
    a, b = fixed_params if fixed_params is not None else noise.sample_params(cfg.prior, n, generator, dev)
    noisy_m = noise.add_noise(clean_m, a, b, generator).clamp(max=1.0)
    crop = slice(MARGIN, MARGIN + cfg.patch)
    clean = mhc_demosaic(clean_m)[..., crop, crop].clamp(0.0, 1.0)
    noisy = mhc_demosaic(noisy_m)[..., crop, crop]
    ta, tb = noise.jitter(a, b, generator, cfg.conditioning_jitter)
    x = noise.model_input(noisy, ta, tb)
    weight = loss_weights(clean)
    codes = torch.randint(0, 8, (n,), generator=generator, device=dev)
    for code in range(1, 8):
        idx = (codes == code).nonzero(as_tuple=True)[0]
        if idx.numel():
            x[idx] = dihedral(x[idx], code)
            clean[idx] = dihedral(clean[idx], code)
            weight[idx] = dihedral(weight[idx], code)
    return x, clean, weight, (a, b)


def step_generator(seed: int, step: int, device) -> torch.Generator:
    """A generator that is a pure function of ``(seed, step)``, so resume reproduces the stream."""
    g = torch.Generator(device=device)
    g.manual_seed((seed * 1_000_003 + step * 7919 + 17) % (2**63 - 1))
    return g
