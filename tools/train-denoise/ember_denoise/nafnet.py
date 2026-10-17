"""NAFNet (Chen, Chu, Zhang, Sun, "Simple Baselines for Image Restoration", arXiv 2204.04676), width-16 class.

Original implementation written from the paper and from ``crates/nafnet/src/model.rs`` (the Candle definition the
product loads); no BasicSR or NAFNet reference code is copied. Parameter names and layouts equal the Candle
loader's, so ``model.state_dict()`` IS the safetensors file the product reads:

  intro.{weight,bias}                         3x3 conv, in_channels -> width
  ending.{weight,bias}                        3x3 conv, width -> out_channels
  encoders.{i}.{j}.<block>   downs.{i}.{weight,bias} (2x2 stride-2 conv, c -> 2c)
  middle_blks.{j}.<block>
  ups.{i}.0.weight                            1x1 conv c -> 2c, NO bias, then PixelShuffle(2)
  decoders.{i}.{j}.<block>
  <block> = norm1/norm2.{weight,bias}, conv1..conv5.{weight,bias}, sca.1.{weight,bias}, beta, gamma ([1,c,1,1])

Differences from the paper, matching the Rust model: 6 input channels (noisy linear RGB + 3 noise-level channels,
see ``noise.model_input``); the global skip adds the first three input channels; the final convolution starts at
zero weights so training starts from the identity.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass

import torch
from torch import nn

IN_CHANNELS = 6
OUT_CHANNELS = 3
LN_EPS = 1e-6


@dataclass(frozen=True)
class NafConfig:
    in_channels: int = IN_CHANNELS
    out_channels: int = OUT_CHANNELS
    width: int = 16
    middle_blocks: int = 3
    enc_blocks: tuple = (1, 1, 1, 2)
    dec_blocks: tuple = (1, 1, 1, 1)

    def validate(self) -> None:
        if not 1 <= self.width <= 256:
            raise ValueError("width must be 1..256")
        if not 1 <= len(self.enc_blocks) <= 6 or len(self.enc_blocks) != len(self.dec_blocks):
            raise ValueError("encoder and decoder need the same number (1..6) of stages")
        if self.in_channels < self.out_channels or self.out_channels < 1:
            raise ValueError("the skip needs in_channels >= out_channels >= 1")

    @property
    def pad_multiple(self) -> int:
        return 1 << len(self.enc_blocks)

    def to_json(self) -> dict:
        """The ``model`` object of contract.json, field names as ``crates/nafnet::NafConfig`` (serde)."""
        d = asdict(self)
        d["enc_blocks"] = list(self.enc_blocks)
        d["dec_blocks"] = list(self.dec_blocks)
        return d

    def expected_params(self) -> int:
        """Closed form: a block with c channels has 7c^2 + 33c parameters (as ``NafConfig::expected_params``)."""

        def block(c: int) -> int:
            return 7 * c * c + 33 * c

        total = self.in_channels * self.width * 9 + self.width
        c = self.width
        for n in self.enc_blocks:
            total += n * block(c) + c * (2 * c) * 4 + 2 * c
            c *= 2
        total += self.middle_blocks * block(c)
        for n in self.dec_blocks:
            total += c * (2 * c)
            c //= 2
            total += n * block(c)
        return total + c * self.out_channels * 9 + self.out_channels

    def expected_tensors(self) -> dict:
        """Every tensor name and shape the Candle loader asks for, derived independently of the module code."""
        t: dict = {}

        def conv(name: str, cin: int, cout: int, k: int, groups: int = 1, bias: bool = True) -> None:
            t[f"{name}.weight"] = (cout, cin // groups, k, k)
            if bias:
                t[f"{name}.bias"] = (cout,)

        def block(prefix: str, c: int) -> None:
            for n in ("norm1", "norm2"):
                t[f"{prefix}.{n}.weight"] = (c,)
                t[f"{prefix}.{n}.bias"] = (c,)
            conv(f"{prefix}.conv1", c, 2 * c, 1)
            conv(f"{prefix}.conv2", 2 * c, 2 * c, 3, groups=2 * c)
            conv(f"{prefix}.conv3", c, c, 1)
            conv(f"{prefix}.sca.1", c, c, 1)
            conv(f"{prefix}.conv4", c, 2 * c, 1)
            conv(f"{prefix}.conv5", c, c, 1)
            t[f"{prefix}.beta"] = (1, c, 1, 1)
            t[f"{prefix}.gamma"] = (1, c, 1, 1)

        conv("intro", self.in_channels, self.width, 3)
        c = self.width
        for i, n in enumerate(self.enc_blocks):
            for j in range(n):
                block(f"encoders.{i}.{j}", c)
            conv(f"downs.{i}", c, 2 * c, 2)
            c *= 2
        for j in range(self.middle_blocks):
            block(f"middle_blks.{j}", c)
        for i, n in enumerate(self.dec_blocks):
            conv(f"ups.{i}.0", c, 2 * c, 1, bias=False)
            c //= 2
            for j in range(n):
                block(f"decoders.{i}.{j}", c)
        conv("ending", c, self.out_channels, 3)
        return t


class LayerNorm2d(nn.Module):
    """Per-pixel normalisation over the channels (not over space), epsilon 1e-6, learnable per-channel affine."""

    def __init__(self, c: int):
        super().__init__()
        self.weight = nn.Parameter(torch.ones(c))
        self.bias = nn.Parameter(torch.zeros(c))

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        mu = x.mean(1, keepdim=True)
        xc = x - mu
        var = xc.pow(2).mean(1, keepdim=True)
        y = xc / torch.sqrt(var + LN_EPS)
        return y * self.weight.view(1, -1, 1, 1) + self.bias.view(1, -1, 1, 1)


def simple_gate(x: torch.Tensor) -> torch.Tensor:
    a, b = x.chunk(2, dim=1)
    return a * b


class NafBlock(nn.Module):
    def __init__(self, c: int):
        super().__init__()
        dw = 2 * c
        self.norm1 = LayerNorm2d(c)
        self.norm2 = LayerNorm2d(c)
        self.conv1 = nn.Conv2d(c, dw, 1)
        self.conv2 = nn.Conv2d(dw, dw, 3, padding=1, groups=dw)
        self.conv3 = nn.Conv2d(dw // 2, c, 1)
        # simplified channel attention: global average pool (no parameters, index 0), then a 1x1 conv (index 1)
        self.sca = nn.Sequential(nn.AdaptiveAvgPool2d(1), nn.Conv2d(dw // 2, dw // 2, 1))
        self.conv4 = nn.Conv2d(c, dw, 1)
        self.conv5 = nn.Conv2d(dw // 2, c, 1)
        self.beta = nn.Parameter(torch.zeros(1, c, 1, 1))
        self.gamma = nn.Parameter(torch.zeros(1, c, 1, 1))

    def forward(self, inp: torch.Tensor) -> torch.Tensor:
        x = self.conv2(self.conv1(self.norm1(inp)))
        x = simple_gate(x)
        x = x * self.sca(x)
        x = self.conv3(x)
        y = inp + x * self.beta
        x = self.conv5(simple_gate(self.conv4(self.norm2(y))))
        return y + x * self.gamma


class NafNet(nn.Module):
    def __init__(self, cfg: NafConfig = NafConfig()):
        super().__init__()
        cfg.validate()
        self.cfg = cfg
        self.intro = nn.Conv2d(cfg.in_channels, cfg.width, 3, padding=1)
        self.encoders = nn.ModuleList()
        self.downs = nn.ModuleList()
        c = cfg.width
        for n in cfg.enc_blocks:
            self.encoders.append(nn.Sequential(*[NafBlock(c) for _ in range(n)]))
            self.downs.append(nn.Conv2d(c, 2 * c, 2, stride=2))
            c *= 2
        self.middle_blks = nn.Sequential(*[NafBlock(c) for _ in range(cfg.middle_blocks)])
        self.ups = nn.ModuleList()
        self.decoders = nn.ModuleList()
        for n in cfg.dec_blocks:
            self.ups.append(nn.Sequential(nn.Conv2d(c, 2 * c, 1, bias=False), nn.PixelShuffle(2)))
            c //= 2
            self.decoders.append(nn.Sequential(*[NafBlock(c) for _ in range(n)]))
        self.ending = nn.Conv2d(c, cfg.out_channels, 3, padding=1)
        nn.init.zeros_(self.ending.weight)  # start from the identity (noisy RGB + a constant)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        h, w = x.shape[-2:]
        m = self.cfg.pad_multiple
        ph, pw = (m - h % m) % m, (m - w % m) % m
        padded = nn.functional.pad(x, (0, pw, 0, ph)) if (ph or pw) else x  # zeros, right and bottom
        y = self.intro(padded)
        skips = []
        for blocks, down in zip(self.encoders, self.downs):
            y = blocks(y)
            skips.append(y)
            y = down(y)
        y = self.middle_blks(y)
        for blocks, up, skip in zip(self.decoders, self.ups, reversed(skips)):
            y = up(y) + skip
            y = blocks(y)
        out = self.ending(y) + padded[:, : self.cfg.out_channels]
        return out[..., :h, :w]


def count_params(model: nn.Module) -> int:
    return sum(p.numel() for p in model.parameters())
