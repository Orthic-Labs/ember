"""prepare.py reads the sidecars `denoise-train export-mosaics` writes; the schema is the Rust `Sidecar` struct."""

import hashlib
import json

import numpy as np
import pytest

from ember_denoise import manifest, pairs, prepare, splits


def sidecar(sha: str, **kw) -> dict:
    """The JSON the Rust exporter writes (field names from `export_mosaics::Sidecar`)."""
    d = {
        "format_version": 1,
        "sha256": sha,
        "source_name": f"{sha[:6]}.ARW",
        "npy": f"{sha}.npy",
        "width": 64,
        "height": 64,
        "dtype": "<u2",
        "scale": 65535.0,
        "cfa_pattern": "RGGB",
        "original_cfa": "BGGR",
        "crop_offset": [1, 1],
        "black_mean": 512.0,
        "white_level": 16383.0,
        "wb_multipliers": [2.1, 1.0, 1.6],
        "as_shot_neutral": None,
        "iso": 100,
        "make": "Sony",
        "model": "ILCE-7M3",
        "decoder": "test",
    }
    d.update(kw)
    return d


def write(dir_, sha: str, shape=(64, 64), dtype="<u2", **kw) -> None:
    np.save(dir_ / f"{sha}.npy", np.full(shape, 20000, dtype=dtype))
    (dir_ / f"{sha}.json").write_text(json.dumps(sidecar(sha, **kw)), encoding="utf-8")


def sha(i: int) -> str:
    return hashlib.sha256(f"file-{i}".encode()).hexdigest()


def run(tmp_path, **kw):
    return prepare.prepare("ember-corpus-pixls", tmp_path / "m", tmp_path / "out", splits.SplitConfig(), False, require_nonempty=False, **kw)


def test_the_sidecar_schema_round_trips_into_the_index_and_the_split_comes_from_the_hash(tmp_path):
    m = tmp_path / "m"
    m.mkdir()
    for i in range(30):
        write(m, sha(i))
    index = run(tmp_path)
    assert len(index["files"]) == 30 and not index["skipped"]
    for f in index["files"]:
        assert f["split"] == splits.assign(bytes.fromhex(f["sha256"]))
        assert f["licence"] == "CC0-1.0" and f["model"] == "ILCE-7M3" and f["size"] == [64, 64]
    # Rust-compatible splits.json: config + three lists of {path, sha256} + duplicates
    rust = json.loads((tmp_path / "out" / "splits.json").read_text())
    assert set(rust) == {"config", "train", "val", "heldout", "duplicates"}
    assert sum(len(rust[k]) for k in ("train", "val", "heldout")) == 30
    # the written index is what pairs.Corpus reads
    train = [f for f in index["files"] if f["split"] == "train"]
    assert train, "30 files should leave some for training"
    corpus = pairs.Corpus(tmp_path / "out", "train")
    assert len(corpus.entries) == len(train)
    assert corpus.mosaic(0).shape == (64, 64)


def test_unusable_sidecars_are_skipped_with_a_reason_not_fatal(tmp_path):
    m = tmp_path / "m"
    m.mkdir()
    write(m, sha(1))
    write(m, sha(2), iso=3200)
    write(m, sha(3), iso=None)
    write(m, sha(4), cfa_pattern="BGGR")
    write(m, sha(5), dtype="<u4")
    write(m, sha(6), shape=(64, 32), width=64)
    write(m, sha(7), format_version=2)
    (m / f"{sha(8)}.json").write_text(json.dumps(sidecar(sha(8))))  # sidecar without its mosaic
    (m / "skipped.json").write_text("{}")  # the exporter's report is not a sidecar
    (m / "garbage.json").write_text("{not json")
    index = run(tmp_path)
    assert [f["sha256"] for f in index["files"]] == [sha(1)]
    why = " | ".join(s["why"] for s in index["skipped"])
    for needle in ("ISO 3200", "ISO unknown", "RGGB", "uint32", "sidecar says", "format 2", "missing"):
        assert needle in why, needle


def test_unknown_iso_is_allowed_only_on_request(tmp_path):
    m = tmp_path / "m"
    m.mkdir()
    write(m, sha(1), iso=None)
    index = prepare.prepare("ember-corpus-pixls", m, tmp_path / "out", splits.SplitConfig(), True, require_nonempty=False)
    assert len(index["files"]) == 1 and index["files"][0]["iso"] is None


def test_a_duplicate_content_hash_is_listed_once(tmp_path):
    m = tmp_path / "m"
    m.mkdir()
    write(m, sha(1))
    (m / "copy.json").write_text(json.dumps(sidecar(sha(1))))
    index = run(tmp_path)
    assert len(index["files"]) == 1 and len(index["duplicates"]) == 1


def test_a_sidecar_cannot_point_outside_its_directory(tmp_path):
    m = tmp_path / "m"
    m.mkdir()
    np.save(tmp_path / "outside.npy", np.zeros((64, 64), dtype="<u2"))
    (m / "x.json").write_text(json.dumps(sidecar(sha(1), npy="../outside.npy")))
    index = run(tmp_path)
    assert not index["files"] and "beside" in index["skipped"][0]["why"]


def test_a_rejected_source_is_refused_before_any_file_is_read(tmp_path):
    with pytest.raises(manifest.LicenceError):
        prepare.prepare("mit-adobe-fivek", tmp_path / "does-not-exist", tmp_path / "out", splits.SplitConfig(), False)


def test_too_small_a_corpus_stops_before_training(tmp_path):
    m = tmp_path / "m"
    m.mkdir()
    write(m, sha(1))
    with pytest.raises(SystemExit):
        prepare.prepare("ember-corpus-pixls", m, tmp_path / "out", splits.SplitConfig(), False)
