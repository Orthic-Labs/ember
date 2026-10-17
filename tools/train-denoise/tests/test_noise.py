"""The synthetic noise follows var(Y) = a*Y + b with E[Y] = x, and the prior only makes valid parameters."""

import math

import pytest
import torch

from ember_denoise import noise


def _gen(seed: int) -> torch.Generator:
    return torch.Generator().manual_seed(seed)


@pytest.mark.parametrize(
    "x,a,b",
    [(0.001, 2e-3, 1e-5), (0.02, 1e-3, 4e-6), (0.18, 5e-4, 2e-5), (0.5, 1e-4, 1e-6), (0.0, 1e-3, 3e-5), (0.9, 1.2e-2, 1e-4)],
)
def test_mean_is_the_signal_and_variance_follows_a_x_plus_b(x, a, b):
    n = 400_000
    clean = torch.full((n,), x)
    y = noise.add_noise(clean, a, b, _gen(11)).double()
    mean, var = y.mean().item(), y.var(unbiased=True).item()
    want_var = a * x + b
    assert abs(mean - x) < 6.0 * math.sqrt(want_var / n) + 1e-7, (mean, x)
    assert abs(var / want_var - 1.0) < 0.03, (var, want_var)


def test_per_sample_parameters_in_one_batch():
    # two (a, b) pairs in one batch of [n, 1, h, w]: each row follows its own a*x + b
    n, hw = 2, 300_000
    clean = torch.full((n, 1, 1, hw), 0.1)
    a = torch.tensor([1e-3, 8e-3])
    b = torch.tensor([1e-6, 5e-5])
    y = noise.add_noise(clean, a, b, _gen(3)).double().reshape(n, hw)
    for i in range(n):
        want = float(a[i]) * 0.1 + float(b[i])
        assert abs(y[i].var().item() / want - 1.0) < 0.03


def test_pure_shot_noise_variance_is_proportional_to_the_signal():
    n = 300_000
    v1 = noise.add_noise(torch.full((n,), 0.1), 1e-3, 0.0, _gen(3)).double().var().item()
    v4 = noise.add_noise(torch.full((n,), 0.4), 1e-3, 0.0, _gen(4)).double().var().item()
    assert abs(v4 / v1 / 4.0 - 1.0) < 0.05


def test_signal_dependence_beats_a_constant_variance_model():
    # a flat-variance model would give equal variance in the dark and the bright patch; this must not
    n = 200_000
    dark = noise.add_noise(torch.full((n,), 0.01), 5e-3, 1e-6, _gen(1)).double().var().item()
    bright = noise.add_noise(torch.full((n,), 0.8), 5e-3, 1e-6, _gen(2)).double().var().item()
    assert bright / dark > 20.0


def test_invalid_parameters_are_an_error_not_silent_garbage():
    for a, b in [(0.0, 1e-5), (-1e-3, 1e-5), (float("nan"), 1e-5), (1e-3, -1.0)]:
        with pytest.raises(ValueError):
            noise.add_noise(torch.full((4,), 0.1), a, b)


def test_negative_and_nonfinite_clean_samples_count_as_zero_signal():
    clean = torch.tensor([-0.5, float("nan"), float("inf")] * 50_000)
    y = noise.add_noise(clean, 1e-3, 1e-6, _gen(5))
    assert torch.isfinite(y).all()
    assert abs(y.double().mean().item()) < 1e-4  # only read noise, zero mean


def test_noise_is_reproducible_from_the_generator():
    clean = torch.full((1000,), 0.2)
    assert torch.equal(noise.add_noise(clean, 1e-3, 1e-5, _gen(9)), noise.add_noise(clean, 1e-3, 1e-5, _gen(9)))
    assert not torch.equal(noise.add_noise(clean, 1e-3, 1e-5, _gen(9)), noise.add_noise(clean, 1e-3, 1e-5, _gen(10)))


def test_iso_scales_a_linearly_and_b_quadratically_above_the_adc_floor():
    a1, b1 = noise.params_at(800.0, 100.0, 40_000.0, 3.0, 0.0)
    a2, b2 = noise.params_at(1600.0, 100.0, 40_000.0, 3.0, 0.0)
    assert abs(a2 / a1 - 2.0) < 1e-9
    assert abs(b2 / b1 - 4.0) < 1e-9


def test_the_prior_only_makes_valid_params_inside_its_ranges():
    prior = noise.SensorPrior()
    a, b = noise.sample_params(prior, 5000, _gen(5))
    assert torch.isfinite(a).all() and torch.isfinite(b).all()
    assert (a > 0).all() and (b >= 0).all()
    # extremes of the closed form: lowest gain / biggest well, highest gain / smallest well
    a_lo = 1.0 / prior.full_well_e[1]
    a_hi = (prior.iso_max / prior.base_iso) / prior.full_well_e[0]
    assert a.min().item() >= a_lo * 0.999 and a.max().item() <= a_hi * 1.001


def test_jitter_perturbs_the_told_parameters_not_their_scale():
    a = torch.full((100_000,), 1e-3)
    b = torch.full((100_000,), 1e-5)
    ja, jb = noise.jitter(a, b, _gen(2), 0.15)
    la = torch.log(ja / a)
    assert abs(la.mean().item()) < 0.01 and abs(la.std().item() - 0.15) < 0.01
    assert (jb > 0).all()


def test_model_input_matches_the_rust_conditioning_formula():
    y = torch.tensor([[[[0.25, -0.1], [0.0, 1.0]]]]).repeat(1, 3, 1, 1)
    a, b = torch.tensor([2e-3]), torch.tensor([1e-5])
    x = noise.model_input(y, a, b)
    assert x.shape == (1, 6, 2, 2)
    assert torch.equal(x[:, :3], y)
    want = torch.sqrt(2e-3 * y.clamp_min(0) + 1e-5) * noise.SIGMA_SCALE
    assert torch.allclose(x[:, 3:], want, atol=1e-6)
    assert noise.SIGMA_SCALE == 16.0  # crates/nafnet SIGMA_SCALE; contract.json is rejected on mismatch
