"""The PyTorch NAFNet matches the Candle definition's tensor names, shapes and parameter count, and exports cleanly."""

import json

import pytest
import torch
from safetensors.torch import load_file

from ember_denoise import export
from ember_denoise.nafnet import IN_CHANNELS, NafConfig, NafNet, count_params


def tiny() -> NafConfig:
    return NafConfig(width=4, middle_blocks=1, enc_blocks=(1, 1), dec_blocks=(1, 1))


def test_width16_has_the_candle_parameter_count():
    assert NafConfig().expected_params() == 2_190_947  # crates/nafnet width16(); asserted by its own test
    assert count_params(NafNet(NafConfig())) == 2_190_947


@pytest.mark.parametrize("cfg", [NafConfig(), tiny()])
def test_state_dict_names_and_shapes_are_exactly_what_the_loader_asks_for(cfg):
    got = {k: tuple(v.shape) for k, v in NafNet(cfg).state_dict().items()}
    assert got == cfg.expected_tensors()


def test_expected_names_include_the_special_cases_of_the_candle_loader():
    names = NafConfig().expected_tensors()
    assert "ups.0.0.weight" in names and "ups.0.0.bias" not in names  # 1x1 conv, no bias, then pixel shuffle
    assert "encoders.3.1.sca.1.weight" in names  # the pooled attention conv sits at index 1
    assert names["encoders.0.0.beta"] == (1, 16, 1, 1)
    assert "middle_blks.2.gamma" in names and "decoders.3.0.conv5.bias" in names


def test_forward_keeps_the_size_for_any_h_w_and_starts_as_the_identity():
    net = NafNet(tiny()).eval()
    x = torch.rand(2, IN_CHANNELS, 18, 22)  # not a multiple of 4: padded right/bottom, cropped back
    with torch.no_grad():
        y = net(x)
    assert y.shape == (2, 3, 18, 22)
    # zero-initialised final conv: output = noisy RGB + the (random) bias constant per channel
    diff = (y - x[:, :3]).amax(dim=(2, 3)) - (y - x[:, :3]).amin(dim=(2, 3))
    assert float(diff.max()) < 1e-6


def test_a_training_step_changes_weights_and_keeps_the_loss_finite():
    net = NafNet(tiny())
    opt = torch.optim.AdamW(net.parameters(), lr=1e-2)
    x = torch.rand(2, IN_CHANNELS, 16, 16)
    target = x[:, :3] * 0.5
    before = net.ending.weight.detach().clone()
    for _ in range(3):
        loss = (net(x) - target).abs().mean()
        opt.zero_grad()
        loss.backward()
        opt.step()
    assert torch.isfinite(loss)
    assert not torch.equal(before, net.ending.weight)


def test_export_writes_a_folder_the_candle_loader_accepts(tmp_path):
    cfg = tiny()
    net = NafNet(cfg)
    with torch.no_grad():  # make beta/gamma nonzero so the round trip is not trivially zeros
        for n, p in net.named_parameters():
            if n.endswith("beta") or n.endswith("gamma"):
                p.uniform_(-0.5, 0.5)
    ck = tmp_path / "x.pt"
    torch.save({"cfg": cfg.to_json(), "model": net.state_dict(), "step": 5}, ck)
    out = export.export(ck, tmp_path / "model", None, None, parity=True)
    tensors = load_file(str(out / "model.safetensors"))
    export.check_tensors(tensors, cfg)
    contract = json.loads((out / "contract.json").read_text())
    # the fields crates/nafnet::Contract::validate requires
    assert contract["format_version"] == 1 and contract["sigma_scale"] == 16.0
    assert contract["model"]["in_channels"] == 6 and contract["model"]["out_channels"] == 3
    assert contract["model"]["enc_blocks"] == [1, 1] and contract["qualified"] is False
    assert (out / "parity-input.npy").is_file() and (out / "parity-output.npy").is_file()


def test_export_rejects_a_missing_extra_or_wrong_tensor():
    cfg = tiny()
    good = {k: v.clone() for k, v in NafNet(cfg).state_dict().items()}
    export.check_tensors(good, cfg)
    missing = dict(good)
    missing.pop("ending.bias")
    with pytest.raises(ValueError):
        export.check_tensors(missing, cfg)
    extra = dict(good)
    extra["ups.0.0.bias"] = torch.zeros(8)
    with pytest.raises(ValueError):
        export.check_tensors(extra, cfg)
    wrong = dict(good)
    wrong["intro.weight"] = torch.zeros(4, 3, 3, 3)
    with pytest.raises(ValueError):
        export.check_tensors(wrong, cfg)
    nan = dict(good)
    nan["ending.bias"] = torch.full_like(good["ending.bias"], float("nan"))
    with pytest.raises(ValueError):
        export.check_tensors(nan, cfg)
