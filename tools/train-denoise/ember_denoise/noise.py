"""Heteroscedastic Poisson-Gaussian sensor noise on the raw (linear) mosaic, in torch.

A sample ``x`` (black-subtracted, white level = 1) is the expected signal. The sensor counts ``K ~ Poisson(x / a)``
quanta of size ``a``, then adds Gaussian read/ADC noise of variance ``b``: ``y = a*K + sqrt(b)*N(0, 1)``.
So ``E[y] = x`` and ``Var[y] = a*x + b``: the ``var(Y) = a*Y + b`` model that ``lightcraft_pipeline::noise`` fits
(there on scene-linear luminance, here per raw sample) and that ``crates/denoise-train/src/noise.rs`` synthesises.

This module mirrors the Rust ``SensorPrior`` (same constants, same formulas) so a Python-trained network and the
Rust trainer see the same noise distribution. The constants are a plausible range, NOT a calibrated camera: issue #200
requires calibrating them on real high-ISO raws before the training run (docs/train-denoise.md, step 0).
"""

from __future__ import annotations

import math
from dataclasses import asdict, dataclass

import torch

SIGMA_SCALE = 16.0  # must equal crates/nafnet SIGMA_SCALE; export.py writes it to contract.json
CONDITIONING_JITTER_LN = 0.15


@dataclass(frozen=True)
class SensorPrior:
    iso_min: float = 100.0
    iso_max: float = 25_600.0
    base_iso: float = 100.0
    full_well_e: tuple[float, float] = (20_000.0, 90_000.0)
    read_e: tuple[float, float] = (1.5, 6.0)
    adc: tuple[float, float] = (5e-5, 3e-4)

    def to_json(self) -> dict:
        d = asdict(self)
        d["calibrated"] = False
        return d


def log_uniform(lo: float, hi: float, n: int, generator: torch.Generator, device=None) -> torch.Tensor:
    """``n`` float64 samples, uniform in log space on ``[lo, hi]``."""
    if not (0.0 < lo <= hi):
        raise ValueError(f"log_uniform needs 0 < lo <= hi, got {lo}, {hi}")
    u = torch.rand(n, generator=generator, dtype=torch.float64, device=device)
    return torch.exp(math.log(lo) + u * (math.log(hi) - math.log(lo)))


def params_at(iso, base_iso, full_well_e, read_e, adc):
    """``(a, b)`` of a camera at ``iso``: ``a = (iso/base)/full_well_e``, ``b = (read_e*a)^2 + adc^2``.

    ``a`` grows linearly with gain (shot noise in normalised units), ``b`` quadratically above the ADC floor.
    All arguments may be tensors of one shape or floats.
    """
    r = iso / base_iso
    if isinstance(r, torch.Tensor):
        r = r.clamp_min(1e-3)
    else:
        r = max(r, 1e-3)
    a = r / full_well_e
    b = (read_e * a) ** 2 + adc**2
    return a, b


def sample_params(prior: SensorPrior, n: int, generator: torch.Generator, device=None):
    """One random (camera, ISO) per sample from the prior; returns float32 ``(a, b)`` tensors of shape ``[n]``."""
    iso = log_uniform(prior.iso_min, prior.iso_max, n, generator, device)
    well = log_uniform(*prior.full_well_e, n, generator, device)
    read = log_uniform(*prior.read_e, n, generator, device)
    adc = log_uniform(*prior.adc, n, generator, device)
    a, b = params_at(iso, prior.base_iso, well, read, adc)
    return a.float(), b.float()


def jitter(a: torch.Tensor, b: torch.Tensor, generator: torch.Generator, sigma_ln: float = CONDITIONING_JITTER_LN):
    """``(a, b)`` scaled by independent log-normal factors: the conditioning the network is told at train time,
    so an estimated (a, b) at inference (``pipeline::noise``) does not hurt. The noise itself is not jittered."""
    fa = torch.exp(sigma_ln * torch.randn(a.shape, generator=generator, dtype=torch.float32, device=a.device))
    fb = torch.exp(sigma_ln * torch.randn(b.shape, generator=generator, dtype=torch.float32, device=b.device))
    return a * fa, b * fb


def _expand(p: torch.Tensor, like: torch.Tensor) -> torch.Tensor:
    return p.reshape(p.shape + (1,) * (like.dim() - p.dim())) if p.dim() < like.dim() else p


def add_noise(clean: torch.Tensor, a: torch.Tensor | float, b: torch.Tensor | float, generator: torch.Generator | None = None):
    """``y = a*Poisson(clean/a) + sqrt(b)*N(0,1)``; ``a``, ``b`` are scalars or ``[n]`` tensors for a ``[n, ...]``
    batch. Negative or non-finite clean samples count as zero signal. Not clipped (the caller clips at white).

    Computed in float32 for the Poisson draw (``torch.poisson`` needs a float rate) and for the Gaussian; at the
    prior's extremes the rate ``x/a`` reaches ~1e5, where float32 is exact for integers (< 2^24)."""
    if not torch.is_tensor(a):
        a = torch.tensor(float(a), device=clean.device)
    if not torch.is_tensor(b):
        b = torch.tensor(float(b), device=clean.device)
    if bool((a <= 0).any()) or bool((b < 0).any()) or not bool(torch.isfinite(a).all() and torch.isfinite(b).all()):
        raise ValueError("noise needs finite a > 0 and b >= 0")
    x = torch.nan_to_num(clean.float(), nan=0.0, posinf=0.0, neginf=0.0).clamp_min(0.0)
    a_ = _expand(a.float(), x)
    b_ = _expand(b.float(), x)
    shot = a_ * torch.poisson(x / a_, generator=generator)
    read = b_.sqrt() * torch.randn(x.shape, generator=generator, dtype=torch.float32, device=x.device)
    return shot + read


def sigma_map(noisy: torch.Tensor, a: torch.Tensor, b: torch.Tensor) -> torch.Tensor:
    """``sqrt(a*max(y, 0) + b) * SIGMA_SCALE`` per sample: the conditioning channels. ``noisy`` is ``[n, c, h, w]``;
    identical to ``crates/nafnet::model_input_chw`` for one image."""
    y = torch.nan_to_num(noisy, nan=0.0, posinf=0.0, neginf=0.0).clamp_min(0.0)
    a_ = _expand(a, y).reshape(-1, 1, 1, 1)
    b_ = _expand(b, y).reshape(-1, 1, 1, 1)
    return (a_ * y + b_).clamp_min(0.0).sqrt() * SIGMA_SCALE


def model_input(noisy_rgb: torch.Tensor, a: torch.Tensor, b: torch.Tensor) -> torch.Tensor:
    """The network input ``[n, 6, h, w]``: noisy linear RGB then the three sigma channels."""
    return torch.cat([noisy_rgb, sigma_map(noisy_rgb, a, b)], dim=1)
