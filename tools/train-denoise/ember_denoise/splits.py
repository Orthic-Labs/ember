"""Content-hash train/val/held-out split, bit-identical to ``crates/denoise-train/src/split.rs``:
bucket = u64_be(first 8 bytes of SHA-256(salt || SHA-256(file bytes))) mod 1000; < heldout_permille is held-out,
then val, else train. Renames never move a file; byte-identical copies share a split; adding files never
reassigns existing ones. So the Python trainer and ``denoise-train eval`` agree on which files are held out."""

from __future__ import annotations

import hashlib
from dataclasses import dataclass

DEFAULT_SALT = "ember-denoise-split-v1"


@dataclass(frozen=True)
class SplitConfig:
    salt: str = DEFAULT_SALT
    val_permille: int = 100
    heldout_permille: int = 150

    def validate(self) -> None:
        if self.val_permille <= 0 or self.heldout_permille <= 0 or self.val_permille + self.heldout_permille >= 1000:
            raise ValueError("val and held-out shares must be positive and leave room for training")


def file_digest(path: str, chunk: int = 1 << 20) -> bytes:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            buf = fh.read(chunk)
            if not buf:
                break
            h.update(buf)
    return h.digest()


def bucket(digest: bytes, salt: str = DEFAULT_SALT) -> int:
    if len(digest) != 32:
        raise ValueError("digest must be 32 bytes")
    out = hashlib.sha256(salt.encode("utf-8") + digest).digest()
    return int.from_bytes(out[:8], "big") % 1000


def assign(digest: bytes, cfg: SplitConfig = SplitConfig()) -> str:
    b = bucket(digest, cfg.salt)
    if b < cfg.heldout_permille:
        return "heldout"
    if b < cfg.heldout_permille + cfg.val_permille:
        return "val"
    return "train"
