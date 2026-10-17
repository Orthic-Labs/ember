"""Malvar-He-Cutler gradient-corrected linear demosaic for an RGGB mosaic, as fixed 5x5 convolutions on any torch
device (He, Malvar, Cutler, "High-quality linear interpolation for demosaicing of Bayer-patterned color images",
ICASSP 2004; the published coefficients, divided by 8).

Why this and not PPG: the Rust trainer demosaics with PPG on the CPU; per-step PPG in Python would starve a GPU.
What matters for the noise model is that noise is added on the MOSAIC and then demosaiced, so the network sees the
spatially correlated, demosaic-shaped noise a real raw produces (i.i.d. per-channel noise on an already demosaiced
image would train a network that under-denoises real files). The demosaic is pluggable only through this module;
docs/train-denoise.md lists "product demosaic differs from MHC" as a gate item (the contract.json records it).

Pattern: R at (even, even), G at (even, odd) and (odd, even), B at (odd, odd). ``prepare.py`` crops every source to
this phase. Reflection padding by 2 keeps the parity of each index, so the CFA phase survives at the borders.
"""

from __future__ import annotations

import torch
import torch.nn.functional as F

_G_AT_RB = [[0, 0, -1, 0, 0], [0, 0, 2, 0, 0], [-1, 2, 4, 2, -1], [0, 0, 2, 0, 0], [0, 0, -1, 0, 0]]
# chroma (R or B) at a G site whose same-chroma neighbours are left/right
_X_AT_G_ROW = [[0, 0, 0.5, 0, 0], [0, -1, 0, -1, 0], [-1, 4, 5, 4, -1], [0, -1, 0, -1, 0], [0, 0, 0.5, 0, 0]]
# ... up/down: the transpose
_X_AT_G_COL = [list(r) for r in zip(*_X_AT_G_ROW)]
# R at a B site, B at an R site
_X_AT_OPP = [[0, 0, -1.5, 0, 0], [0, 2, 0, 2, 0], [-1.5, 0, 6, 0, -1.5], [0, 2, 0, 2, 0], [0, 0, -1.5, 0, 0]]

DEMOSAIC_NAME = "malvar-he-cutler-5x5, RGGB, reflect padding 2"


def _kernels(device, dtype) -> torch.Tensor:
    k = torch.tensor([_G_AT_RB, _X_AT_G_ROW, _X_AT_G_COL, _X_AT_OPP], dtype=dtype, device=device) / 8.0
    return k.unsqueeze(1)  # [4, 1, 5, 5]


def mhc_demosaic(mosaic: torch.Tensor) -> torch.Tensor:
    """``[n, 1, h, w]`` RGGB mosaic (h, w even, at least 3) to ``[n, 3, h, w]`` RGB. Linear, so it commutes with
    exposure scaling; the output may leave [0, 1] near edges (the caller clips the clean target)."""
    if mosaic.dim() != 4 or mosaic.shape[1] != 1:
        raise ValueError(f"mosaic must be [n, 1, h, w], got {tuple(mosaic.shape)}")
    h, w = mosaic.shape[-2:]
    if h % 2 or w % 2 or h < 3 or w < 3:
        raise ValueError(f"mosaic height and width must be even and >= 4, got {h}x{w}")
    x = mosaic
    f = F.conv2d(F.pad(x, (2, 2, 2, 2), mode="reflect"), _kernels(x.device, x.dtype))
    g_rb, x_row, x_col, x_opp = f[:, 0:1], f[:, 1:2], f[:, 2:3], f[:, 3:4]
    yy = torch.arange(h, device=x.device).reshape(h, 1) % 2
    xx = torch.arange(w, device=x.device).reshape(1, w) % 2
    m_r = ((yy == 0) & (xx == 0)).to(x.dtype)
    m_g1 = ((yy == 0) & (xx == 1)).to(x.dtype)  # in a red row: R left/right, B up/down
    m_g2 = ((yy == 1) & (xx == 0)).to(x.dtype)  # in a blue row: B left/right, R up/down
    m_b = ((yy == 1) & (xx == 1)).to(x.dtype)
    r = x * m_r + x_row * m_g1 + x_col * m_g2 + x_opp * m_b
    g = x * (m_g1 + m_g2) + g_rb * (m_r + m_b)
    b = x * m_b + x_col * m_g1 + x_row * m_g2 + x_opp * m_r
    return torch.cat([r, g, b], dim=1)
