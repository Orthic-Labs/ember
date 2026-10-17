"""Demosaic, split and licence-gate behaviour."""

import hashlib

import pytest
import torch

from ember_denoise import manifest, pairs, splits
from ember_denoise.demosaic import mhc_demosaic


def test_a_flat_mosaic_demosaics_to_a_flat_image():
    out = mhc_demosaic(torch.full((1, 1, 16, 20), 0.37))
    assert out.shape == (1, 3, 16, 20)
    assert torch.allclose(out, torch.full_like(out, 0.37), atol=1e-6)


def test_each_channel_keeps_its_own_samples_at_their_own_sites():
    m = torch.rand(1, 1, 12, 12)
    out = mhc_demosaic(m)
    assert torch.equal(out[0, 0, ::2, ::2], m[0, 0, ::2, ::2])  # R sites
    assert torch.equal(out[0, 2, 1::2, 1::2], m[0, 0, 1::2, 1::2])  # B sites
    assert torch.equal(out[0, 1, 0::2, 1::2], m[0, 0, 0::2, 1::2])  # G sites
    assert torch.equal(out[0, 1, 1::2, 0::2], m[0, 0, 1::2, 0::2])


def test_a_gray_ramp_is_reconstructed_in_the_interior():
    # R = G = B = a horizontal ramp, sampled through RGGB: the interior must come back as the ramp in all channels
    ramp = torch.linspace(0.1, 0.9, 24).reshape(1, 1, 1, 24).repeat(1, 1, 24, 1)
    out = mhc_demosaic(ramp)
    inner = (slice(None), slice(None), slice(4, -4), slice(4, -4))
    assert torch.allclose(out[inner], ramp.repeat(1, 3, 1, 1)[inner], atol=1e-5)


def test_demosaic_rejects_odd_or_wrong_shapes():
    with pytest.raises(ValueError):
        mhc_demosaic(torch.zeros(1, 1, 7, 8))
    with pytest.raises(ValueError):
        mhc_demosaic(torch.zeros(1, 3, 8, 8))


def test_dihedral_codes_are_eight_distinct_permutations():
    x = torch.arange(16.0).reshape(1, 4, 4)
    seen = {tuple(pairs.dihedral(x, c).flatten().tolist()) for c in range(8)}
    assert len(seen) == 8
    assert torch.equal(pairs.dihedral(x, 0), x)


def test_make_batch_shapes_ranges_and_reproducibility():
    cfg = pairs.PairConfig(patch=32)
    w = torch.rand(4, 1, cfg.window_side, cfg.window_side) * 0.5 + 0.05
    x1, c1, wt1, (a1, b1) = pairs.make_batch(w, cfg, pairs.step_generator(1, 5, "cpu"))
    x2, c2, _, _ = pairs.make_batch(w, cfg, pairs.step_generator(1, 5, "cpu"))
    x3, _, _, _ = pairs.make_batch(w, cfg, pairs.step_generator(1, 6, "cpu"))
    assert x1.shape == (4, 6, 32, 32) and c1.shape == (4, 3, 32, 32) and wt1.shape == c1.shape
    assert float(c1.min()) >= 0.0 and float(c1.max()) <= 1.0
    assert torch.equal(x1, x2) and torch.equal(c1, c2)
    assert not torch.equal(x1, x3)
    assert (a1 > 0).all() and (b1 >= 0).all()


def test_split_bucket_is_the_documented_function():
    digest = hashlib.sha256(b"abc").digest()
    want = int.from_bytes(hashlib.sha256(b"ember-denoise-split-v1" + digest).digest()[:8], "big") % 1000
    assert splits.bucket(digest) == want
    shares = {"train": 0, "val": 0, "heldout": 0}
    for i in range(4000):
        shares[splits.assign(hashlib.sha256(f"file-{i}".encode()).digest())] += 1
    assert abs(shares["heldout"] / 4000 - 0.15) < 0.03 and abs(shares["val"] / 4000 - 0.10) < 0.03
    # a different salt reshuffles; the same content never moves
    assert splits.assign(digest) == splits.assign(digest)
    with pytest.raises(ValueError):
        splits.SplitConfig(val_permille=600, heldout_permille=400).validate()


def test_shipped_manifest_is_valid_and_accepts_only_permissive_sources():
    m = manifest.load()
    accepted = {s["id"] for s in m["sources"] if s["status"] == "accepted"}
    assert accepted == {"ember-corpus-pixls"}
    for s in m["sources"]:
        if s["id"] in accepted:
            assert s["licence"] in manifest.PERMISSIVE and s["licence_url"].startswith("https://")


@pytest.mark.parametrize("sid", ["mit-adobe-fivek", "raise", "ppr10k", "sidd", "nind", "rawnind", "wikimedia-commons-raws", "nonexistent"])
def test_unaccepted_and_unknown_sources_are_refused(sid):
    with pytest.raises(manifest.LicenceError):
        manifest.require_accepted(sid)
    assert manifest.require_accepted("ember-corpus-pixls")["licence"] == "CC0-1.0"


def test_validation_rejects_a_non_permissive_accepted_entry():
    bad = {"sources": [{"id": "x", "status": "accepted", "licence": "CC-BY-NC-4.0", "licence_url": "https://e", "verified_on": "2026-10-17"}]}
    with pytest.raises(manifest.LicenceError):
        manifest.validate(bad)
    no_date = {"sources": [{"id": "x", "status": "accepted", "licence": "CC0-1.0", "licence_url": "https://e"}]}
    with pytest.raises(manifest.LicenceError):
        manifest.validate(no_date)
