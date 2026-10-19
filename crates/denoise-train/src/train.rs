//! The training loop: batches from [`TrainPool`] (train split only), AdamW, cosine schedule with warm-up,
//! periodic validation on the fixed validation set (val split only), checkpoints under the output directory.
//!
//! Output (all under `out_dir`, which the CLI requires to be below the scratch root):
//! `contract.json` (what the network expects and what it was trained on), `splits.json`, `ckpt-XXXXXXXX.safetensors`
//! (the last few), `latest.safetensors`, `best.safetensors` (lowest validation loss), `state.json` (resume point)
//! and `log.jsonl`. The optimiser's moment estimates are not checkpointed: a resumed run restarts them, which
//! costs a short warm-up, not correctness.

use crate::data::{Example, FixedSet, PairConfig, TrainPool};
use crate::error::{Error, Result, config};
use crate::model::{IN_CHANNELS, NafConfig, NafNet, OUT_CHANNELS, SIGMA_SCALE, zero_init_ending};
use crate::split::{Split, SplitManifest};
use candle_core::{DType, Device, Tensor};
use candle_nn::optim::{AdamW, Optimizer, ParamsAdamW};
use candle_nn::{VarBuilder, VarMap};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
    Cuda,
    Metal,
}

impl DeviceKind {
    pub fn parse(s: &str) -> Option<DeviceKind> {
        match s {
            "cpu" => Some(DeviceKind::Cpu),
            "cuda" => Some(DeviceKind::Cuda),
            "metal" => Some(DeviceKind::Metal),
            _ => None,
        }
    }

    pub fn open(self) -> Result<Device> {
        match self {
            DeviceKind::Cpu => Ok(Device::Cpu),
            DeviceKind::Cuda => open_cuda(),
            DeviceKind::Metal => open_metal(),
        }
    }
}

fn open_cuda() -> Result<Device> {
    config(
        "CUDA is not built in: the workspace lock carries no CUDA stack. Use --device cpu or metal, or add a local cuda feature on the training machine",
    )
}
#[cfg(feature = "metal")]
fn open_metal() -> Result<Device> {
    Ok(Device::new_metal(0)?)
}
#[cfg(not(feature = "metal"))]
fn open_metal() -> Result<Device> {
    config("this build has no Metal support; rebuild with `--features metal` on macOS")
}

/// Training output must go under the scratch root (workspace rule: never the workspace or the internal disk).
pub fn require_scratch(path: &Path, allow_outside: bool) -> Result<()> {
    if allow_outside {
        return Ok(());
    }
    let norm = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    if norm.split('/').any(|c| c == "..") {
        return config(format!("{} contains `..`", path.display()));
    }
    let ok = ["d:/scratch", "/volumes/d/scratch"].iter().any(|root| norm == *root || norm.starts_with(&format!("{root}/")));
    if ok {
        Ok(())
    } else {
        config(format!("{} is not under D:\\scratch or /Volumes/D/scratch (pass --allow-outside-scratch to override)", path.display()))
    }
}

#[derive(Clone, Debug)]
pub struct TrainConfig {
    pub steps: u64,
    pub batch: usize,
    pub lr: f64,
    pub lr_min: f64,
    pub warmup: u64,
    pub seed: u64,
    pub ckpt_every: u64,
    pub val_every: u64,
    pub log_every: u64,
    pub pool_windows: usize,
    pub windows_per_file: usize,
    /// A new file is decoded into the pool every this many steps.
    pub refresh_every: u64,
    pub val_windows_per_file: usize,
    pub keep_ckpts: usize,
    pub pair: PairConfig,
    pub model: NafConfig,
    pub device: DeviceKind,
    pub out_dir: PathBuf,
    pub resume: bool,
}

impl TrainConfig {
    pub fn new(out_dir: PathBuf) -> TrainConfig {
        TrainConfig {
            steps: 200_000,
            batch: 8,
            lr: 1e-3,
            lr_min: 1e-6,
            warmup: 500,
            seed: 1,
            ckpt_every: 1000,
            val_every: 1000,
            log_every: 50,
            pool_windows: 1024,
            windows_per_file: 32,
            refresh_every: 20,
            val_windows_per_file: 4,
            keep_ckpts: 3,
            pair: PairConfig::default(),
            model: NafConfig::width16(),
            device: DeviceKind::Cpu,
            out_dir,
            resume: false,
        }
    }

    fn validate(&self) -> Result<()> {
        self.model.validate()?;
        if self.steps == 0 || self.batch == 0 || self.ckpt_every == 0 || self.val_every == 0 || self.log_every == 0 || self.refresh_every == 0 {
            return config("steps, batch and the *_every intervals must be positive");
        }
        if !(self.lr.is_finite() && self.lr > 0.0 && self.lr_min >= 0.0 && self.lr_min <= self.lr) {
            return config("need 0 <= lr_min <= lr and lr > 0");
        }
        let m = self.model.pad_multiple();
        if self.pair.patch == 0 || !self.pair.patch.is_multiple_of(m) {
            return config(format!("patch {} must be a positive multiple of {m}", self.pair.patch));
        }
        Ok(())
    }
}

/// Learning rate at `step`: linear warm-up, then cosine decay from `lr` to `lr_min` at the last step.
pub fn learning_rate(step: u64, steps: u64, warmup: u64, lr: f64, lr_min: f64) -> f64 {
    if step < warmup {
        return lr * (step + 1) as f64 / warmup as f64;
    }
    let span = steps.saturating_sub(warmup).max(1) as f64;
    let p = ((step - warmup) as f64 / span).clamp(0.0, 1.0);
    lr_min + 0.5 * (lr - lr_min) * (1.0 + (std::f64::consts::PI * p).cos())
}

#[derive(Serialize)]
struct Contract<'a> {
    format_version: u32,
    model: &'a NafConfig,
    weights_format: &'static str,
    input: String,
    output: &'static str,
    sigma_scale: f32,
    loss: &'static str,
    domain: &'static str,
    clean_source: String,
    noise: serde_json::Value,
    patch: usize,
    demosaic: String,
    max_underexpose_ev: f64,
    conditioning_jitter_ln: f64,
    seed: u64,
    splits: serde_json::Value,
}

fn write_contract(cfg: &TrainConfig, manifest: &SplitManifest) -> Result<()> {
    let contract = Contract {
        format_version: 1,
        model: &cfg.model,
        weights_format: "safetensors, F32, PyTorch NAFNet parameter names and layouts (see model.rs)",
        input: format!(
            "[{IN_CHANNELS}, h, w]: channels 0..3 noisy linear camera RGB (white level 1, no white balance, no colour matrix); channels 3..6 sqrt(a*max(y,0)+b) per channel times {SIGMA_SCALE}"
        ),
        output: "[3, h, w] = input RGB + network residual; zero-pad right/bottom to a multiple of 2^stages, crop after",
        sigma_scale: SIGMA_SCALE,
        loss: "mean over samples of |pred - clean| / (sqrt(clamp(clean, 0, 1)) + 0.03)",
        domain: "demosaiced camera RGB (not the product scene-linear Rec.2020: an unresolved item of the learned-RGB gate)",
        clean_source: format!(
            "CC0 raws recorded at ISO <= {} (cargo run -p xtask -- corpus --download), train split only",
            crate::data::MAX_SOURCE_ISO
        ),
        noise: serde_json::json!({
            "model": "y = a*Poisson(x/a) + sqrt(b)*N(0,1) on the raw mosaic, clipped at white; var = a*x + b",
            "a": "base_iso-relative gain / full_well_e",
            "b": "(read_e*a)^2 + adc^2",
            "prior": {
                "iso": [cfg.pair.prior.iso_min, cfg.pair.prior.iso_max],
                "base_iso": cfg.pair.prior.base_iso,
                "full_well_e": [cfg.pair.prior.full_well_e.0, cfg.pair.prior.full_well_e.1],
                "read_e": [cfg.pair.prior.read_e.0, cfg.pair.prior.read_e.1],
                "adc": [cfg.pair.prior.adc.0, cfg.pair.prior.adc.1],
            },
            "calibrated": false,
        }),
        patch: cfg.pair.patch,
        demosaic: format!("{:?}", cfg.pair.demosaic),
        max_underexpose_ev: cfg.pair.max_underexpose_ev,
        conditioning_jitter_ln: cfg.pair.conditioning_jitter,
        seed: cfg.seed,
        splits: serde_json::json!({
            "salt": manifest.config.salt,
            "train_files": manifest.train.len(),
            "val_files": manifest.val.len(),
            "heldout_files": manifest.heldout.len(),
        }),
    };
    rightkit_fs::atomic_write(&cfg.out_dir.join("contract.json"), &serde_json::to_vec_pretty(&contract)?)?;
    rightkit_fs::atomic_write(&cfg.out_dir.join("splits.json"), &serde_json::to_vec_pretty(manifest)?)?;
    Ok(())
}

/// Stack examples into `(input, clean, weight)` tensors.
fn batch_tensors(ex: &[Example], patch: usize, device: &Device) -> Result<(Tensor, Tensor, Tensor)> {
    let b = ex.len();
    let mut input = Vec::with_capacity(b * IN_CHANNELS * patch * patch);
    let mut clean = Vec::with_capacity(b * OUT_CHANNELS * patch * patch);
    let mut weight = Vec::with_capacity(clean.capacity());
    for e in ex {
        if e.input.len() != IN_CHANNELS * patch * patch || e.clean.len() != OUT_CHANNELS * patch * patch {
            return Err(Error::Data("an example does not have the patch size".into()));
        }
        input.extend_from_slice(&e.input);
        clean.extend_from_slice(&e.clean);
        weight.extend_from_slice(&e.weight);
    }
    Ok((
        Tensor::from_vec(input, (b, IN_CHANNELS, patch, patch), device)?,
        Tensor::from_vec(clean, (b, OUT_CHANNELS, patch, patch), device)?,
        Tensor::from_vec(weight, (b, OUT_CHANNELS, patch, patch), device)?,
    ))
}

fn weighted_l1(pred: &Tensor, clean: &Tensor, weight: &Tensor) -> Result<Tensor> {
    let d = (pred - clean)?.abs()?;
    Ok((&d * weight)?.mean_all()?)
}

/// One optimiser step on `examples`; returns the loss.
pub fn train_step(net: &NafNet, opt: &mut AdamW, examples: &[Example], patch: usize, device: &Device) -> Result<f32> {
    let (x, y, w) = batch_tensors(examples, patch, device)?;
    let pred = net.forward(&x)?;
    let loss = weighted_l1(&pred, &y, &w)?;
    opt.backward_step(&loss)?;
    Ok(loss.to_scalar::<f32>()?)
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ValStats {
    pub loss: f64,
    pub mse: f64,
}

/// Loss and MSE of `net` over the fixed validation set (no gradients are taken).
pub fn validate(net: &NafNet, set: &FixedSet, device: &Device, batch: usize) -> Result<ValStats> {
    let n = set.patch;
    let (mut loss, mut mse, mut count) = (0.0f64, 0.0f64, 0usize);
    for chunk in set.examples.chunks(batch.max(1)) {
        let ex: Vec<Example> = chunk.iter().map(|(_, e)| e.clone()).collect();
        let (x, y, w) = batch_tensors(&ex, n, device)?;
        let pred = net.forward(&x)?;
        loss += weighted_l1(&pred, &y, &w)?.to_scalar::<f32>()? as f64 * ex.len() as f64;
        mse += (&pred - &y)?.sqr()?.mean_all()?.to_scalar::<f32>()? as f64 * ex.len() as f64;
        count += ex.len();
    }
    let c = count.max(1) as f64;
    Ok(ValStats { loss: loss / c, mse: mse / c })
}

#[derive(Serialize, Deserialize)]
struct State {
    step: u64,
    best_val_loss: f64,
}

fn read_state(dir: &Path) -> Option<State> {
    serde_json::from_slice(&std::fs::read(dir.join("state.json")).ok()?).ok()
}

/// Save atomically: write beside, then rename over.
///
/// The temp file is named here (not by `rightkit_fs::AtomicWriter`) because candle's `VarMap::save` writes the file itself,
/// by path, and offers no writer to stream into. The finished file is then published with `rightkit_fs::publish_file`
/// (synced, replace, folder synced), so the checkpoint name never holds a partial file. A save that fails or is killed leaves
/// only this `.tmp` beside it, which the next save of the same checkpoint overwrites.
fn save_atomic(varmap: &VarMap, path: &Path) -> Result<()> {
    let tmp = path.with_extension("safetensors.tmp");
    varmap.save(&tmp)?;
    rightkit_fs::publish_file(&tmp, path)?;
    Ok(())
}

fn prune_checkpoints(dir: &Path, keep: usize) -> Result<()> {
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("ckpt-") && n.ends_with(".safetensors"))
        .collect();
    names.sort();
    let drop = names.len().saturating_sub(keep.max(1));
    for n in names.iter().take(drop) {
        std::fs::remove_file(dir.join(n)).ok();
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct TrainSummary {
    pub steps_run: u64,
    pub final_step: u64,
    pub best_val_loss: f64,
    pub last_val_loss: Option<f64>,
    pub out_dir: PathBuf,
}

/// Train. Only `manifest.train` feeds the network, only `manifest.val` is validated on; `manifest.heldout` is
/// never opened here.
pub fn train(manifest: &SplitManifest, cfg: &TrainConfig) -> Result<TrainSummary> {
    cfg.validate()?;
    manifest.verify()?;
    manifest.verify_assignments()?;
    std::fs::create_dir_all(&cfg.out_dir)?;
    write_contract(cfg, manifest)?;
    let device = cfg.device.open()?;

    #[allow(unused_mut)] // `VarMap::load` takes `&mut self`
    let mut varmap = VarMap::new();
    let net = NafNet::new(&cfg.model, VarBuilder::from_varmap(&varmap, DType::F32, &device))?;
    let mut start = 0u64;
    let mut best = f64::INFINITY;
    match (cfg.resume, read_state(&cfg.out_dir)) {
        (true, Some(state)) => {
            varmap.load(cfg.out_dir.join("latest.safetensors"))?;
            start = state.step;
            best = state.best_val_loss;
            println!("resumed at step {start} (best val loss {best:.6})");
        }
        (true, None) => return config(format!("--resume but {} has no state.json", cfg.out_dir.display())),
        _ => zero_init_ending(&varmap)?,
    }
    if start >= cfg.steps {
        return config(format!("already at step {start} of {}", cfg.steps));
    }
    let mut opt = AdamW::new(varmap.all_vars(), ParamsAdamW { lr: cfg.lr, beta1: 0.9, beta2: 0.9, eps: 1e-8, weight_decay: 0.0 })?;

    let mut pool = TrainPool::new(manifest.files(Split::Train), cfg.pair.clone(), cfg.pool_windows, cfg.windows_per_file, cfg.seed)?;
    for (p, why) in &pool.skipped {
        eprintln!("skipped training file {p}: {why}");
    }
    let val = FixedSet::build(manifest.files(Split::Val), &cfg.pair, cfg.val_windows_per_file, &[], cfg.seed ^ 0x7a11d)?;
    println!("pool: {} windows from {} training files; validation: {} examples", pool.len(), manifest.train.len(), val.examples.len());

    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(cfg.out_dir.join("log.jsonl"))?;
    let t0 = Instant::now();
    let mut last_val = None;
    let mut running = 0.0f64;
    let mut running_n = 0u64;
    for step in start..cfg.steps {
        opt.set_learning_rate(learning_rate(step, cfg.steps, cfg.warmup, cfg.lr, cfg.lr_min));
        if step > start && step.is_multiple_of(cfg.refresh_every) {
            pool.visit_next();
        }
        let examples = pool.batch(step, cfg.batch);
        if examples.is_empty() {
            return Err(Error::Data("the pool produced an empty batch".into()));
        }
        let loss = train_step(&net, &mut opt, &examples, cfg.pair.patch, &device)?;
        if !loss.is_finite() {
            return Err(Error::Diverged(format!(
                "loss {loss} at step {step}; the last checkpoint in {} is the last good state",
                cfg.out_dir.display()
            )));
        }
        running += loss as f64;
        running_n += 1;
        let done = step + 1;
        if done.is_multiple_of(cfg.log_every) {
            let line = serde_json::json!({
                "step": done, "loss": running / running_n as f64,
                "lr": learning_rate(step, cfg.steps, cfg.warmup, cfg.lr, cfg.lr_min),
                "elapsed_s": t0.elapsed().as_secs_f64(),
            });
            println!("{line}");
            writeln!(log, "{line}")?;
            running = 0.0;
            running_n = 0;
        }
        let mut improved = false;
        if done.is_multiple_of(cfg.val_every) || done == cfg.steps {
            let v = validate(&net, &val, &device, cfg.batch)?;
            let line = serde_json::json!({"step": done, "val_loss": v.loss, "val_mse": v.mse});
            println!("{line}");
            writeln!(log, "{line}")?;
            last_val = Some(v.loss);
            if v.loss < best {
                best = v.loss;
                improved = true;
            }
        }
        if improved {
            save_atomic(&varmap, &cfg.out_dir.join("best.safetensors"))?;
        }
        if done.is_multiple_of(cfg.ckpt_every) || done == cfg.steps {
            let ck = cfg.out_dir.join(format!("ckpt-{done:08}.safetensors"));
            save_atomic(&varmap, &ck)?;
            save_atomic(&varmap, &cfg.out_dir.join("latest.safetensors"))?;
            rightkit_fs::atomic_write(&cfg.out_dir.join("state.json"), &serde_json::to_vec(&State { step: done, best_val_loss: best })?)?;
            prune_checkpoints(&cfg.out_dir, cfg.keep_ckpts)?;
        }
    }
    save_atomic(&varmap, &cfg.out_dir.join("final.safetensors"))?;
    Ok(TrainSummary {
        steps_run: cfg.steps - start,
        final_step: cfg.steps,
        best_val_loss: best,
        last_val_loss: last_val,
        out_dir: cfg.out_dir.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::NoiseParams;

    fn fake_example(patch: usize, seed: usize) -> Example {
        let n = patch * patch;
        let clean: Vec<f32> = (0..3 * n).map(|i| 0.2 + 0.1 * (((i + seed) % 17) as f32 / 17.0)).collect();
        let noisy: Vec<f32> = clean.iter().enumerate().map(|(i, c)| c + 0.05 * (((i * 7 + seed) % 11) as f32 / 11.0 - 0.5)).collect();
        let p = NoiseParams { a: 1e-2, b: 1e-4 };
        Example { input: crate::model::model_input_chw(&noisy, p), weight: crate::data::loss_weights(&clean), clean, params: p }
    }

    #[test]
    fn the_schedule_warms_up_then_decays_to_the_floor() {
        let lr = |s| learning_rate(s, 1000, 100, 1e-3, 1e-6);
        assert!(lr(0) < lr(50) && lr(50) < lr(99));
        assert!((lr(100) - 1e-3).abs() < 1e-9);
        assert!(lr(500) < lr(100) && lr(999) < lr(500));
        assert!((lr(1000) - 1e-6).abs() < 1e-9);
        assert!(lr(2000) >= 1e-6 - 1e-12, "past the end it stays at the floor");
    }

    #[test]
    fn output_must_be_under_the_scratch_root() {
        assert!(require_scratch(Path::new("D:\\scratch\\ember\\run1"), false).is_ok());
        assert!(require_scratch(Path::new("d:/scratch"), false).is_ok());
        assert!(require_scratch(Path::new("/Volumes/D/scratch/x"), false).is_ok());
        assert!(require_scratch(Path::new("D:\\Claude\\ember\\run"), false).is_err());
        assert!(require_scratch(Path::new("D:\\scratch\\..\\Claude"), false).is_err());
        assert!(require_scratch(Path::new("D:\\scratchy\\x"), false).is_err());
        assert!(require_scratch(Path::new("C:\\temp"), false).is_err());
        assert!(require_scratch(Path::new("C:\\temp"), true).is_ok());
    }

    #[test]
    fn a_training_step_runs_changes_the_weights_and_keeps_the_loss_finite() {
        let cfg = NafConfig::tiny();
        let varmap = VarMap::new();
        let net = NafNet::new(&cfg, VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu)).unwrap();
        zero_init_ending(&varmap).unwrap();
        let mut opt = AdamW::new(varmap.all_vars(), ParamsAdamW { lr: 1e-2, beta1: 0.9, beta2: 0.9, eps: 1e-8, weight_decay: 0.0 }).unwrap();
        let patch = 16;
        let ex: Vec<Example> = (0..2).map(|i| fake_example(patch, i)).collect();
        let before = {
            let data = varmap.data().lock().unwrap();
            data.get("ending.weight").unwrap().as_tensor().abs().unwrap().sum_all().unwrap().to_scalar::<f32>().unwrap()
        };
        assert_eq!(before, 0.0);
        let mut losses = Vec::new();
        for _ in 0..3 {
            losses.push(train_step(&net, &mut opt, &ex, patch, &Device::Cpu).unwrap());
        }
        assert!(losses.iter().all(|l| l.is_finite() && *l > 0.0), "{losses:?}");
        let after = {
            let data = varmap.data().lock().unwrap();
            data.get("ending.weight").unwrap().as_tensor().abs().unwrap().sum_all().unwrap().to_scalar::<f32>().unwrap()
        };
        assert!(after > 0.0, "the optimiser moved the zero-initialised ending");
    }

    #[test]
    fn a_batch_of_the_wrong_patch_size_is_an_error() {
        let ex = vec![fake_example(8, 0)];
        assert!(batch_tensors(&ex, 16, &Device::Cpu).is_err());
    }

    #[test]
    fn the_config_rejects_a_patch_the_network_cannot_tile() {
        let mut c = TrainConfig::new(PathBuf::from("D:\\scratch\\x"));
        assert!(c.validate().is_ok());
        c.pair.patch = 130;
        assert!(c.validate().is_err());
        c.pair.patch = 128;
        c.lr_min = 1.0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn checkpoints_are_pruned_to_the_newest() {
        let dir = std::env::temp_dir().join(format!("ember-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for s in [1u64, 2, 3, 4, 5] {
            std::fs::write(dir.join(format!("ckpt-{s:08}.safetensors")), b"x").unwrap();
        }
        std::fs::write(dir.join("best.safetensors"), b"x").unwrap();
        prune_checkpoints(&dir, 2).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(left, vec!["best.safetensors", "ckpt-00000004.safetensors", "ckpt-00000005.safetensors"]);
    }
}
