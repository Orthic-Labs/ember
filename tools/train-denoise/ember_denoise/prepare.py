"""Build the training corpus index from mosaics exported by Ember's own decoder.

    denoise-train export-mosaics --in corpus/raw --out D:\scratch\ember-denoise\mosaics   # Rust, owner builds it
    python -m ember_denoise.prepare --source-id ember-corpus-pixls --mosaic-dir <mosaics> --out <prepared>

No third-party raw decoder is used anywhere in this kit (decision ember-4a): the mosaics come from
``lightcraft-raw``, the decode the product uses, as ``<sha256>.npy`` (uint16 ``[h, w]``, ``round(value * 65535)`` of
the black-subtracted, white-normalised sample, RGGB at (0, 0)) plus a ``<sha256>.json`` sidecar written by
``crates/denoise-train/src/export_mosaics.rs``.

This step reads the sidecars and decides what may be trained on:
* the source must be ``accepted`` in ``datasets.json`` (``manifest.require_accepted``), before any file is read;
* sidecars must be format 1, dtype ``<u2``, scale 65535, pattern RGGB, with an ``.npy`` whose shape matches;
* sources recorded above ISO 800 are skipped as not clean (the Rust trainer's rule); an unknown ISO is skipped
  unless ``--allow-unknown-iso``;
* the split comes from the sidecar's content hash (``splits.py``), identical to the Rust trainer, so
  ``denoise-train eval`` and this trainer agree on what is held out.

Writes ``index.json`` (what ``pairs.Corpus`` reads; ``npy`` is the absolute path of each mosaic, which stays where
the exporter wrote it) and a Rust-compatible ``splits.json``.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

from . import manifest, splits

MAX_SOURCE_ISO = 800
SIDECAR_VERSION = 1
MAX_DIM = 20_000
HEX = set("0123456789abcdef")


def check_sidecar(sc: dict, npy: Path) -> None:
    """Raise ValueError with the reason when a sidecar or its mosaic is not what this trainer can use."""
    if sc.get("format_version") != SIDECAR_VERSION:
        raise ValueError(f"sidecar format {sc.get('format_version')!r} is not {SIDECAR_VERSION}")
    sha = sc.get("sha256", "")
    if len(sha) != 64 or set(sha) - HEX:
        raise ValueError("sidecar sha256 is not 64 lowercase hex characters")
    if sc.get("dtype") != "<u2" or sc.get("scale") != 65535.0:
        raise ValueError(f"mosaic must be <u2 scaled by 65535, sidecar says {sc.get('dtype')!r} x {sc.get('scale')!r}")
    if sc.get("cfa_pattern") != "RGGB":
        raise ValueError(f"mosaic pattern must be RGGB, sidecar says {sc.get('cfa_pattern')!r}")
    w, h = sc.get("width"), sc.get("height")
    if not (isinstance(w, int) and isinstance(h, int) and 64 <= w <= MAX_DIM and 64 <= h <= MAX_DIM and w % 2 == 0 and h % 2 == 0):
        raise ValueError(f"unusable size {w}x{h}")
    if not npy.is_file():
        raise ValueError(f"mosaic file {npy.name} is missing")
    arr = np.load(npy, mmap_mode="r")
    if arr.dtype != np.dtype("<u2") or arr.shape != (h, w):
        raise ValueError(f"{npy.name} is {arr.dtype} {arr.shape}, sidecar says <u2 {(h, w)}")


def prepare(source_id: str, mosaic_dir: Path, out: Path, cfg: splits.SplitConfig, allow_unknown_iso: bool, require_nonempty: bool = True) -> dict:
    src = manifest.require_accepted(source_id)
    sidecars = sorted(mosaic_dir.glob("*.json"))
    sidecars = [p for p in sidecars if p.name != "skipped.json"]
    if not sidecars:
        raise SystemExit(f"no sidecars under {mosaic_dir}: run `denoise-train export-mosaics` first")
    out.mkdir(parents=True, exist_ok=True)
    files, skipped, duplicates = [], [], []
    seen = set()
    for p in sidecars:
        try:
            sc = json.loads(p.read_text(encoding="utf-8"))
            npy = mosaic_dir / sc.get("npy", "")
            if npy.parent != mosaic_dir:
                raise ValueError("sidecar npy must be a plain file name beside it")
            check_sidecar(sc, npy)
        except (ValueError, OSError, json.JSONDecodeError) as e:
            skipped.append({"path": str(p), "why": str(e)})
            continue
        sha = sc["sha256"]
        if sha in seen:
            duplicates.append(str(p))
            continue
        seen.add(sha)
        iso = sc.get("iso")
        if iso is None and not allow_unknown_iso:
            skipped.append({"path": sc["source_name"], "why": "ISO unknown (pass --allow-unknown-iso if the source is known base ISO)"})
            continue
        if iso is not None and iso > MAX_SOURCE_ISO:
            skipped.append({"path": sc["source_name"], "why": f"ISO {iso} > {MAX_SOURCE_ISO}: not a clean source"})
            continue
        files.append(
            {
                "path": sc["source_name"],
                "sha256": sha,
                "npy": str(npy.resolve()),
                "iso": iso,
                "split": splits.assign(bytes.fromhex(sha), cfg),
                "source_id": source_id,
                "licence": src["licence"],
                "licence_url": src["licence_url"],
                "model": sc.get("model"),
                "size": [sc["width"], sc["height"]],
                "original_cfa": sc.get("original_cfa"),
                "black_mean": sc.get("black_mean"),
                "white_level": sc.get("white_level"),
                "wb_multipliers": sc.get("wb_multipliers"),
            }
        )
    by = {s: [f for f in files if f["split"] == s] for s in ("train", "val", "heldout")}
    index = {"format_version": 1, "source_id": source_id, "split_config": cfg.__dict__, "mosaic": "uint16 = round(value*65535), black-subtracted, white=1, RGGB phase, from lightcraft-raw", "files": files, "skipped": skipped, "duplicates": duplicates}
    (out / "index.json").write_text(json.dumps(index, indent=2), encoding="utf-8")
    # same shape as crates/denoise-train SplitManifest, so `denoise-train eval` and this run agree
    rust = {"config": cfg.__dict__, "duplicates": duplicates}
    for s in by:
        rust[s] = sorted(({"path": f["path"], "sha256": f["sha256"]} for f in by[s]), key=lambda e: e["sha256"])
    (out / "splits.json").write_text(json.dumps(rust, indent=2), encoding="utf-8")
    print(f"prepared {len(files)} files ({', '.join(f'{s} {len(v)}' for s, v in by.items())}), skipped {len(skipped)}, duplicates {len(duplicates)}")
    for s in skipped:
        print(f"  skipped {s['path']}: {s['why']}")
    empty = [s for s, v in by.items() if not v]
    if empty and require_nonempty:
        raise SystemExit(f"split(s) {empty} are empty: the corpus is too small for these shares. Add accepted raws before training.")
    return index


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--source-id", required=True, help="an `accepted` id in datasets.json")
    ap.add_argument("--mosaic-dir", required=True, type=Path, help="output of `denoise-train export-mosaics`")
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--salt", default=splits.DEFAULT_SALT)
    ap.add_argument("--val-permille", type=int, default=100)
    ap.add_argument("--heldout-permille", type=int, default=150)
    ap.add_argument("--allow-unknown-iso", action="store_true")
    a = ap.parse_args(argv)
    cfg = splits.SplitConfig(a.salt, a.val_permille, a.heldout_permille)
    cfg.validate()
    try:
        prepare(a.source_id, a.mosaic_dir, a.out, cfg, a.allow_unknown_iso)
    except manifest.LicenceError as e:
        print(f"refused: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
