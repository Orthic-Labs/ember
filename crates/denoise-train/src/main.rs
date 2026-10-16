//! `denoise-train splits | train | eval`: see `docs/denoise-qualification.md`, "Own-trained denoiser".
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use lightcraft_denoise_train::error::{Error, Result};
use lightcraft_denoise_train::eval::{EvalConfig, evaluate};
use lightcraft_denoise_train::model::{NafConfig, NafNet};
use lightcraft_denoise_train::split::{Split, SplitConfig, SplitManifest, build_manifest};
use lightcraft_denoise_train::train::{DeviceKind, TrainConfig, require_scratch, train};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

const USAGE: &str = "usage:
  denoise-train splits --corpus DIR [--out FILE] [--val-permille N] [--heldout-permille N]
  denoise-train train  --corpus DIR --out DIR [--steps N] [--batch N] [--patch N] [--lr F] [--lr-min F] [--warmup N]
                       [--seed N] [--device cpu|cuda|metal] [--ckpt-every N] [--val-every N] [--log-every N]
                       [--pool-windows N] [--windows-per-file N] [--refresh-every N] [--val-windows-per-file N]
                       [--val-permille N] [--heldout-permille N] [--resume] [--allow-outside-scratch]
  denoise-train eval   --weights FILE --out FILE [--corpus DIR] [--device cpu|cuda|metal] [--patch N]
                       [--windows-per-file N] [--allow-outside-scratch]
Outputs go under D:\\scratch (or /Volumes/D/scratch) unless --allow-outside-scratch is given.";

struct Args {
    values: HashMap<String, String>,
}

const BOOL_FLAGS: [&str; 2] = ["resume", "allow-outside-scratch"];

impl Args {
    fn parse(tokens: &[String]) -> Result<Args> {
        let mut values = HashMap::new();
        let mut i = 0;
        while i < tokens.len() {
            let Some(key) = tokens.get(i).and_then(|t| t.strip_prefix("--")) else {
                return Err(Error::Config(format!("unexpected argument `{}`\n{USAGE}", tokens.get(i).map_or("", String::as_str))));
            };
            if BOOL_FLAGS.contains(&key) {
                values.insert(key.to_string(), "true".to_string());
                i += 1;
            } else {
                let Some(v) = tokens.get(i + 1) else { return Err(Error::Config(format!("--{key} needs a value"))) };
                values.insert(key.to_string(), v.clone());
                i += 2;
            }
        }
        Ok(Args { values })
    }

    fn flag(&mut self, key: &str) -> bool {
        self.values.remove(key).is_some()
    }

    fn string(&mut self, key: &str) -> Option<String> {
        self.values.remove(key)
    }

    fn parsed<T: FromStr>(&mut self, key: &str) -> Result<Option<T>> {
        match self.values.remove(key) {
            None => Ok(None),
            Some(v) => v.parse::<T>().map(Some).map_err(|_| Error::Config(format!("--{key}: cannot read `{v}`"))),
        }
    }

    fn required_path(&mut self, key: &str) -> Result<PathBuf> {
        self.string(key).map(PathBuf::from).ok_or_else(|| Error::Config(format!("--{key} is required\n{USAGE}")))
    }

    fn finish(self) -> Result<()> {
        if let Some(k) = self.values.keys().next() {
            return Err(Error::Config(format!("unknown option --{k}\n{USAGE}")));
        }
        Ok(())
    }
}

fn split_config(a: &mut Args) -> Result<SplitConfig> {
    let mut cfg = SplitConfig::default();
    if let Some(v) = a.parsed::<u32>("val-permille")? {
        cfg.val_permille = v;
    }
    if let Some(v) = a.parsed::<u32>("heldout-permille")? {
        cfg.heldout_permille = v;
    }
    Ok(cfg)
}

fn device_of(a: &mut Args) -> Result<DeviceKind> {
    match a.string("device") {
        None => Ok(DeviceKind::Cpu),
        Some(s) => DeviceKind::parse(&s).ok_or_else(|| Error::Config(format!("--device must be cpu, cuda or metal, not `{s}`"))),
    }
}

fn run(tokens: &[String]) -> Result<()> {
    let Some((cmd, rest)) = tokens.split_first() else { return Err(Error::Config(USAGE.into())) };
    let mut a = Args::parse(rest)?;
    match cmd.as_str() {
        "splits" => {
            let corpus = a.required_path("corpus")?;
            let out = a.string("out").map(PathBuf::from);
            let cfg = split_config(&mut a)?;
            a.finish()?;
            let m = build_manifest(&corpus, &cfg)?;
            println!(
                "train {} / val {} / heldout {} files ({} byte-identical duplicates dropped)",
                m.train.len(),
                m.val.len(),
                m.heldout.len(),
                m.duplicates.len()
            );
            m.verify_assignments()?;
            if let Err(e) = m.verify() {
                println!("WARNING: {e}");
            }
            let json = serde_json::to_vec_pretty(&m)?;
            match out {
                Some(p) => std::fs::write(p, json)?,
                None => println!("{}", String::from_utf8_lossy(&json)),
            }
            Ok(())
        }
        "train" => {
            let corpus = a.required_path("corpus")?;
            let out = a.required_path("out")?;
            require_scratch(&out, a.values.contains_key("allow-outside-scratch"))?;
            let split_cfg = split_config(&mut a)?;
            let mut cfg = TrainConfig::new(out);
            macro_rules! set {
                ($key:literal, $field:expr) => {
                    if let Some(v) = a.parsed($key)? {
                        $field = v;
                    }
                };
            }
            set!("steps", cfg.steps);
            set!("batch", cfg.batch);
            set!("patch", cfg.pair.patch);
            set!("lr", cfg.lr);
            set!("lr-min", cfg.lr_min);
            set!("warmup", cfg.warmup);
            set!("seed", cfg.seed);
            set!("ckpt-every", cfg.ckpt_every);
            set!("val-every", cfg.val_every);
            set!("log-every", cfg.log_every);
            set!("pool-windows", cfg.pool_windows);
            set!("windows-per-file", cfg.windows_per_file);
            set!("refresh-every", cfg.refresh_every);
            set!("val-windows-per-file", cfg.val_windows_per_file);
            cfg.device = device_of(&mut a)?;
            cfg.resume = a.flag("resume");
            a.flag("allow-outside-scratch");
            a.finish()?;
            // a resumed run keeps its recorded split: the corpus may have grown, and the salt is part of the contract
            let recorded = cfg.out_dir.join("splits.json");
            let manifest = if cfg.resume && recorded.exists() {
                serde_json::from_slice::<SplitManifest>(&std::fs::read(&recorded)?)?
            } else {
                build_manifest(&corpus, &split_cfg)?
            };
            manifest.verify()?;
            println!("splits: train {} / val {} / heldout {} files", manifest.train.len(), manifest.val.len(), manifest.heldout.len());
            let summary = train(&manifest, &cfg)?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            Ok(())
        }
        "eval" => {
            let weights = a.required_path("weights")?;
            let out = a.required_path("out")?;
            require_scratch(&out, a.values.contains_key("allow-outside-scratch"))?;
            let corpus = a.string("corpus").map(PathBuf::from);
            let device_kind = device_of(&mut a)?;
            let mut ecfg = EvalConfig::default();
            if let Some(v) = a.parsed("patch")? {
                ecfg.pair.patch = v;
            }
            if let Some(v) = a.parsed("windows-per-file")? {
                ecfg.windows_per_file = v;
            }
            a.flag("allow-outside-scratch");
            a.finish()?;

            let dir = weights.parent().map(Path::to_path_buf).unwrap_or_default();
            let model_cfg = read_model_config(&dir.join("contract.json"))?;
            let manifest = match std::fs::read(dir.join("splits.json")) {
                Ok(bytes) => serde_json::from_slice::<SplitManifest>(&bytes)?,
                Err(_) => {
                    let corpus = corpus.ok_or_else(|| Error::Config("no splits.json beside the weights: pass --corpus".into()))?;
                    build_manifest(&corpus, &SplitConfig::default())?
                }
            };
            manifest.verify()?;
            manifest.verify_assignments()?;
            let device = device_kind.open()?;
            let net = NafNet::load(&model_cfg, &weights, &device)?;
            let report = evaluate(&net, manifest.files(Split::Heldout), &ecfg, &device)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
            println!("status: {} (see `checks` in {})", report.status, out.display());
            for c in &report.checks {
                println!("  [{}] {}: {:.5} (threshold {})", if c.pass { "pass" } else { "FAIL" }, c.name, c.value, c.threshold);
            }
            Ok(())
        }
        other => Err(Error::Config(format!("unknown command `{other}`\n{USAGE}"))),
    }
}

fn read_model_config(path: &Path) -> Result<NafConfig> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let model = v.get("model").ok_or_else(|| Error::Config(format!("{} has no `model` entry", path.display())))?;
    let cfg: NafConfig = serde_json::from_value(model.clone())?;
    cfg.validate()?;
    Ok(cfg)
}

fn main() -> ExitCode {
    let tokens: Vec<String> = std::env::args().skip(1).collect();
    match run(&tokens) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
