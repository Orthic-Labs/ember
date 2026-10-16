//! Time one denoise tile on each backend and compare the answers with the CPU's: the pure-Rust CPU runner, the wgpu
//! runner and the opt-in ONNX Runtime backend (DirectML / CoreML), as the engine's agreement check sees them.
//!
//! ```text
//! LIGHTCRAFT_ONNXRUNTIME_DIR=<folder with onnxruntime.dll / libonnxruntime.dylib> \
//!   <run this example in release mode with --features onnx-accel> -- [--model model.onnx] [--tile 512] [--runs 5] [--onnx-cpu]
//! ```
//!
//! Without `--model` a synthetic U-Net (tile 64) stands in, which says nothing about speed on a real model but proves
//! the plumbing. With the installed model pass its `.onnx` file and its tile (512 for the real models). Per backend it
//! prints the time of the first call (kernel / session warm-up included), the best of `--runs` warm calls, which
//! execution provider ran, the worst difference from the CPU as a fraction of the CPU answer's largest value (what the
//! engine gates on) and the SNR against the CPU in dB. `--onnx-cpu` adds ONNX Runtime's own CPU provider as a baseline.
//! No CI runs this; it needs a runtime library on this computer.
use std::path::PathBuf;
use std::time::Instant;

use lightcraft_denoise::manifest::{DenoiserManifest, Domain, Gain};
use lightcraft_denoise::run::TileRunner;
use lightcraft_denoise_onnx::{ExecutionProvider, OnnxRunner};

fn manifest(tile: usize) -> DenoiserManifest {
    DenoiserManifest {
        id: "bench".into(),
        name: "Bench".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: String::new(),
        domain: Domain::BayerToRgb,
        tile: tile as u32,
        overlap: 16,
        gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.05 },
    }
}

/// `(first ms, best warm ms, answer)` of `runs` warm calls after one first call.
fn time(runner: &dyn TileRunner, input: &[f32], runs: usize) -> Result<(f64, f64, Vec<f32>), String> {
    let started = Instant::now();
    let mut answer = runner.run(input).map_err(|e| e.to_string())?;
    let first = started.elapsed().as_secs_f64() * 1e3;
    let mut best = f64::INFINITY;
    for _ in 0..runs.max(1) {
        let started = Instant::now();
        answer = runner.run(input).map_err(|e| e.to_string())?;
        best = best.min(started.elapsed().as_secs_f64() * 1e3);
    }
    Ok((first, best, answer))
}

/// Worst difference as a fraction of `want`'s largest value (the engine's gate), and the SNR in dB.
fn compare(want: &[f32], got: &[f32]) -> (f32, f64) {
    let scale = want.iter().fold(1e-12f32, |m, v| m.max(v.abs()));
    let worst = want.iter().zip(got).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale;
    let (signal, noise) = want.iter().zip(got).fold((0f64, 0f64), |(s, n), (&a, &b)| (s + f64::from(a).powi(2), n + f64::from(a - b).powi(2)));
    (worst, if noise > 0.0 { 10.0 * (signal / noise).log10() } else { f64::INFINITY })
}

fn report(name: &str, provider: &str, result: Result<(f64, f64, Vec<f32>), String>, want: Option<&[f32]>) {
    match result {
        Ok((first, best, got)) => {
            let (worst, snr) = match want {
                Some(w) if w.len() == got.len() => compare(w, &got),
                Some(_) => (f32::NAN, f64::NAN),
                None => (0.0, f64::INFINITY),
            };
            println!("{name:<10} {first:>10.1} {best:>10.1}   {worst:>9.1e} {snr:>8.1}   {provider}");
        }
        Err(why) => println!("{name:<10} {:>10} {:>10}   {:>9} {:>8}   not run: {why}", "-", "-", "-", "-"),
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let runs: usize = flag("--runs").and_then(|v| v.parse().ok()).unwrap_or(5);
    let model = flag("--model").map(PathBuf::from);
    let tile: usize = flag("--tile").and_then(|v| v.parse().ok()).unwrap_or(if model.is_some() { 512 } else { 64 });
    let path = match model {
        Some(p) => p,
        None => {
            let dir = std::env::temp_dir().join(format!("denoise-backends-{}", std::process::id()));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let p = dir.join("model.onnx");
            std::fs::write(&p, lightcraft_denoise::synthetic::unet_onnx(tile as u64, 8, 2, 7)).map_err(|e| e.to_string())?;
            p
        }
    };

    let cpu = lightcraft_denoise::runtime::CpuRunner::load(&path, &manifest(tile)).map_err(|e| e.to_string())?;
    let input = lightcraft_denoise::runtime::check_tile(tile);
    println!("model {} tile {tile} ({} cells in, {} pixels out), best of {runs} warm runs\n", path.display(), input.len(), 3 * 4 * tile * tile);
    println!("{:<10} {:>10} {:>10}   {:>9} {:>8}   provider", "backend", "first ms", "best ms", "worst/max", "SNR dB");

    let cpu_result = time(&cpu, &input, runs.min(2))?;
    let want = cpu_result.2.clone();
    report("cpu", "pure-Rust runner", Ok(cpu_result), None);

    match lightcraft_gpu::nn::runner(cpu.net(), tile) {
        Ok(gpu) => report("gpu", &format!("wgpu: {}", gpu.adapter()), time(&gpu, &input, runs), Some(&want)),
        Err(why) => report("gpu", "wgpu", Err(why), None),
    }

    match lightcraft_denoise_onnx::provider_report() {
        Ok(list) => {
            for (name, usable, detail) in list {
                println!("  ONNX Runtime provider {name}: {} ({detail})", if usable { "usable" } else { "not usable" });
            }
        }
        Err(why) => println!("  ONNX Runtime: {why}"),
    }
    match OnnxRunner::load(&path, tile) {
        Ok(onnx) => report("onnx", onnx.provider_name(), time(&onnx, &input, runs), Some(&want)),
        Err(why) => report("onnx", "accelerator", Err(why.to_string()), None),
    }
    if args.iter().any(|a| a == "--onnx-cpu") {
        match OnnxRunner::load_with(&path, tile, ExecutionProvider::Cpu) {
            Ok(onnx) => report("onnx-cpu", onnx.provider_name(), time(&onnx, &input, runs), Some(&want)),
            Err(why) => report("onnx-cpu", "ONNX Runtime CPU", Err(why.to_string()), None),
        }
    }
    println!("\nThe engine uses a backend only when worst/max is at most 1e-3 (wgpu) or 2e-3 (ONNX Runtime) and it beats the CPU.");
    Ok(())
}
