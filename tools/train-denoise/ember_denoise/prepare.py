"""Turn licence-cleared raw files into the training corpus: black-subtracted, white-normalised Bayer mosaics cropped
to the RGGB phase, stored as uint16 ``.npy`` (value * 65535) plus ``index.json`` and a Rust-compatible ``splits.json``.

    python -m ember_denoise.prepare --source-id ember-corpus-pixls --raw-dir corpus/raw --out /workspace/prepared

Rules enforced here:
* the source must be ``accepted`` in ``datasets.json`` (``manifest.require_accepted``): a rejected or unverified
  source is refused before any file is read;
* only plain 2x2 Bayer files are kept (X-Trans, 4-colour and linear DNGs are skipped and listed);
* sources recorded above ISO 800 are skipped, as not clean (the Rust trainer's rule); a file whose ISO cannot be
  read is skipped unless ``--allow-unknown-iso``;
* the split comes from the content hash (``splits.py``), identical to the Rust trainer, so ``denoise-train eval``
  and this trainer agree on what is held out.

Decoding uses rawpy (LibRaw bindings) as an external, dev-time decoder on the training machine only: no LibRaw code
is copied into Ember and nothing of it ships. This is a licensing decision for the maintainers to confirm (the
workspace rule is "never copy GPL/LGPL code"; linking a decoder for data preparation is not copying, but it is
flagged in docs/train-denoise.md).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

import numpy as np

from . import manifest, splits

RAW_EXTENSIONS = {".arw", ".cr2", ".cr3", ".dng", ".nef", ".nrw", ".orf", ".raf", ".rw2", ".pef", ".srw", ".srf", ".sr2", ".3fr"}
MAX_SOURCE_ISO = 800
MAX_DIM = 20_000


def read_iso(path: Path):
    try:
        import exifread

        with open(path, "rb") as fh:
            tags = exifread.process_file(fh, details=False, stop_tag="ISOSpeedRatings")
        v = tags.get("EXIF ISOSpeedRatings")
        return int(str(v.values[0] if hasattr(v, "values") and v.values else v)) if v is not None else None
    except Exception:  # unreadable metadata is "unknown", handled by the caller
        return None


def rggb_mosaic(path: Path):
    """``(mosaic float32 in [0,1] with RGGB phase, info)`` or raises ValueError with the reason to skip."""
    import rawpy

    with rawpy.imread(str(path)) as raw:
        if raw.raw_type != rawpy.RawType.Flat:
            raise ValueError("not a flat (single-plane) raw")
        colors = np.asarray(raw.raw_colors_visible)
        img = np.asarray(raw.raw_image_visible)
        h, w = img.shape
        if h > MAX_DIM or w > MAX_DIM or h < 64 or w < 64:
            raise ValueError(f"unusable size {w}x{h}")
        desc = raw.color_desc.decode("ascii", "replace")  # e.g. "RGBG": colour index -> letter
        grid = [[desc[int(colors[y, x])] if int(colors[y, x]) < len(desc) else "?" for x in (0, 1)] for y in (0, 1)]
        flat = sorted(c for row in grid for c in row)
        if flat != ["B", "G", "G", "R"]:
            raise ValueError(f"not a 2x2 Bayer pattern: {grid}")
        # the 2x2 pattern must tile the whole frame (rules out X-Trans and anything aperiodic)
        probe = colors[: min(h, 64), : min(w, 64)]
        tiled = np.tile(colors[:2, :2], (probe.shape[0] // 2 + 1, probe.shape[1] // 2 + 1))[: probe.shape[0], : probe.shape[1]]
        if not np.array_equal(probe, tiled):
            raise ValueError("colour pattern is not a repeating 2x2")
        # offset so that R is at the origin; then G, G, B follow and the phase is RGGB
        oy, ox = next((y, x) for y in (0, 1) for x in (0, 1) if grid[y][x] == "R")
        black = np.asarray(raw.black_level_per_channel, dtype=np.float32)[colors]
        white = float(raw.white_level)
        denom = max(white - float(black.mean()), 1.0)
        norm = np.clip((img.astype(np.float32) - black) / denom, 0.0, 1.0)
        norm = norm[oy:, ox:]
        norm = norm[: norm.shape[0] // 2 * 2, : norm.shape[1] // 2 * 2]
        info = {"pattern": "".join(grid[0] + grid[1]), "crop_offset": [oy, ox], "white_level": white, "black_mean": float(black.mean()), "size": [int(norm.shape[1]), int(norm.shape[0])]}
        return norm, info


def prepare(source_id: str, raw_dir: Path, out: Path, cfg: splits.SplitConfig, allow_unknown_iso: bool) -> dict:
    src = manifest.require_accepted(source_id)
    out.mkdir(parents=True, exist_ok=True)
    files, skipped, duplicates = [], [], []
    seen = set()
    paths = sorted(p for p in raw_dir.rglob("*") if p.is_file() and p.suffix.lower() in RAW_EXTENSIONS)
    if not paths:
        raise SystemExit(f"no raw files under {raw_dir}")
    for p in paths:
        digest = splits.file_digest(str(p))
        hexd = digest.hex()
        if hexd in seen:
            duplicates.append(str(p))
            continue
        seen.add(hexd)
        iso = read_iso(p)
        if iso is None and not allow_unknown_iso:
            skipped.append({"path": str(p), "why": "ISO unreadable (pass --allow-unknown-iso if the source is known base ISO)"})
            continue
        if iso is not None and iso > MAX_SOURCE_ISO:
            skipped.append({"path": str(p), "why": f"ISO {iso} > {MAX_SOURCE_ISO}: not a clean source"})
            continue
        try:
            mosaic, info = rggb_mosaic(p)
        except Exception as e:  # a bad file is skipped and reported, never fatal
            skipped.append({"path": str(p), "why": str(e)})
            continue
        npy = f"{hexd}.npy"
        tmp = out / (npy + ".tmp")
        with open(tmp, "wb") as fh:
            np.save(fh, np.round(mosaic * 65535.0).astype(np.uint16))
        os.replace(tmp, out / npy)
        files.append({"path": str(p), "sha256": hexd, "npy": npy, "iso": iso, "split": splits.assign(digest, cfg), "source_id": source_id, "licence": src["licence"], "licence_url": src["licence_url"], **info})
    by = {s: [f for f in files if f["split"] == s] for s in ("train", "val", "heldout")}
    index = {"format_version": 1, "source_id": source_id, "split_config": cfg.__dict__, "mosaic": "uint16 = round(value*65535), black-subtracted, white=1, RGGB phase", "files": files, "skipped": skipped, "duplicates": duplicates}
    (out / "index.json").write_text(json.dumps(index, indent=2), encoding="utf-8")
    # same shape as crates/denoise-train SplitManifest, so `denoise-train eval --corpus` and this run agree
    rust = {"config": cfg.__dict__, "duplicates": duplicates}
    for s in by:
        rust[s] = sorted(({"path": f["path"], "sha256": f["sha256"]} for f in by[s]), key=lambda e: e["sha256"])
    (out / "splits.json").write_text(json.dumps(rust, indent=2), encoding="utf-8")
    print(f"prepared {len(files)} files ({', '.join(f'{s} {len(v)}' for s, v in by.items())}), skipped {len(skipped)}, duplicates {len(duplicates)}")
    for s in skipped:
        print(f"  skipped {s['path']}: {s['why']}")
    empty = [s for s, v in by.items() if not v]
    if empty:
        raise SystemExit(f"split(s) {empty} are empty: the corpus is too small for these shares. Add accepted raws before training.")
    return index


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--source-id", required=True, help="an `accepted` id in datasets.json")
    ap.add_argument("--raw-dir", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--salt", default=splits.DEFAULT_SALT)
    ap.add_argument("--val-permille", type=int, default=100)
    ap.add_argument("--heldout-permille", type=int, default=150)
    ap.add_argument("--allow-unknown-iso", action="store_true")
    a = ap.parse_args(argv)
    cfg = splits.SplitConfig(a.salt, a.val_permille, a.heldout_permille)
    cfg.validate()
    try:
        prepare(a.source_id, a.raw_dir, a.out, cfg, a.allow_unknown_iso)
    except manifest.LicenceError as e:
        print(f"refused: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
