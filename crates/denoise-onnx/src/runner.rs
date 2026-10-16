//! ONNX Runtime loading and the tile runner (feature `onnx-accel`, native only).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use lightcraft_denoise::run::{Error, TileRunner};
pub use rightkit_onnx::ExecutionProvider;
use rightkit_onnx::ort::session::Session;
use rightkit_onnx::ort::value::Tensor;
use rightkit_onnx::{CandidateSource, EnvironmentInitOptions, RuntimeCandidate, RuntimeError, RuntimeSelection, SessionOptions};

use crate::{NOT_FOUND, RUNTIME_DIR_ENV, TileShape, check_input, check_output, tile_shape};

/// The runtime library inside `dir` (the value of [`RUNTIME_DIR_ENV`]), or why there is none. Nothing is loaded or
/// changed: the path is only checked (absolute, the platform's file name, a regular file, not Windows' own copy).
pub fn locate_runtime(dir: Option<&Path>) -> Result<RuntimeSelection, String> {
    let name = rightkit_onnx::runtime_filename();
    let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty()) else {
        return Err(format!("{NOT_FOUND}: set {RUNTIME_DIR_ENV} to the folder that holds {name}"));
    };
    if !dir.is_absolute() {
        return Err(format!("{NOT_FOUND}: {RUNTIME_DIR_ENV} must be an absolute folder, not {}", dir.display()));
    }
    rightkit_onnx::resolve_runtime([RuntimeCandidate::new(dir.join(name), CandidateSource::AppData)]).map_err(|e| match e {
        RuntimeError::NotFound { .. } => format!("{NOT_FOUND}: {} has no {name}", dir.display()),
        other => format!("{NOT_FOUND}: {other}"),
    })
}

fn runtime_dir_from_env() -> Option<PathBuf> {
    std::env::var_os(RUNTIME_DIR_ENV).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Bind and load ONNX Runtime from [`RUNTIME_DIR_ENV`], once per process; later calls give the first outcome. The
/// answer is the library's path. The library is checked as it loads (exports the API, new enough for the pinned
/// `ort`), so a wrong file is an error. Binding sets `ORT_DYLIB_PATH` and puts the folder first on `PATH` (for
/// `DirectML.dll` beside it), so the first call should come before other threads read the environment.
pub fn ensure_runtime() -> Result<&'static Path, String> {
    static RUNTIME: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let found = locate_runtime(runtime_dir_from_env().as_deref())?;
            let selection = rightkit_onnx::configure_runtime([RuntimeCandidate::new(found.path.clone(), found.source)])
                .map_err(|e| format!("{NOT_FOUND}: {e}"))?;
            rightkit_onnx::prepend_runtime_dir_to_path(&selection.path);
            rightkit_onnx::init_environment_with_options(&selection, &EnvironmentInitOptions::default())
                .map_err(|e| format!("ONNX Runtime at {} could not be loaded: {e}", selection.path.display()))?;
            Ok(selection.path)
        })
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}

/// Which execution providers the loaded runtime offers here: `(name, usable, detail)`. An error when there is no
/// runtime.
pub fn provider_report() -> Result<Vec<(String, bool, String)>, String> {
    ensure_runtime()?;
    Ok(rightkit_onnx::probe_providers().into_iter().map(|p| (format!("{:?}", p.provider), p.available, p.detail)).collect())
}

/// What a provider is called in reports.
pub fn provider_name(p: ExecutionProvider) -> &'static str {
    match p {
        ExecutionProvider::Cpu => "ONNX Runtime CPU",
        ExecutionProvider::DirectMl => "DirectML",
        ExecutionProvider::CoreMl => "CoreML",
    }
}

/// The denoise model on ONNX Runtime. One session serves every call (ONNX Runtime wants exclusive use of a session to
/// run it, so tiles take turns here; an accelerator is one device anyway, and the engine overlaps the packing and
/// blending of the tiles around it).
pub struct OnnxRunner {
    session: Mutex<Session>,
    shape: TileShape,
    provider: ExecutionProvider,
}

impl OnnxRunner {
    /// The model at `path` on this computer's accelerator (DirectML on Windows, CoreML on Apple). An error where there
    /// is none, where the runtime is missing or cannot register the provider, or where it cannot load the model: there
    /// is no fallback, because the engine falls back to its own runners.
    pub fn load(path: &Path, tile: usize) -> Result<Self, Error> {
        let provider = ExecutionProvider::platform_accelerator();
        if provider == ExecutionProvider::Cpu {
            return Err(Error::Load("this operating system has no ONNX Runtime accelerator here".into()));
        }
        Self::load_with(path, tile, provider)
    }

    /// Like [`OnnxRunner::load`] on a named provider, ONNX Runtime's own CPU one included (for comparisons).
    pub fn load_with(path: &Path, tile: usize, provider: ExecutionProvider) -> Result<Self, Error> {
        let shape = tile_shape(tile).map_err(|e| Error::Load(e.to_string()))?;
        ensure_runtime().map_err(Error::Load)?;
        let built = SessionOptions::new(provider)
            .with_cpu_fallback(false)
            .build_from_file_reported(path)
            .map_err(|e| Error::Load(format!("{}: {e}", provider_name(provider))))?;
        if built.provider != provider || built.fallback.is_some() {
            return Err(Error::Load(format!("{} was not used: {}", provider_name(provider), built.fallback.unwrap_or_default())));
        }
        let (inputs, outputs) = (built.session.inputs().len(), built.session.outputs().len());
        if inputs != 1 || outputs != 1 {
            return Err(Error::Load(format!("the model has {inputs} inputs and {outputs} outputs, not one of each")));
        }
        Ok(Self { session: Mutex::new(built.session), shape, provider })
    }

    /// The provider that serves the model.
    pub fn provider(&self) -> ExecutionProvider {
        self.provider
    }

    /// What the provider is called in reports.
    pub fn provider_name(&self) -> &'static str {
        provider_name(self.provider)
    }

    pub fn tile(&self) -> usize {
        self.shape.tile
    }
}

impl TileRunner for OnnxRunner {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        check_input(&self.shape, input)?;
        let runtime = |what: &str, e: &dyn std::fmt::Display| Error::Runtime(format!("{what}: {e}"));
        let tensor = Tensor::from_array((self.shape.input_dims(), input.to_vec())).map_err(|e| runtime("the input tensor", &e))?;
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session.run(rightkit_onnx::ort::inputs![tensor]).map_err(|e| runtime(self.provider_name(), &e))?;
        if outputs.len() == 0 {
            return Err(Error::Model("no output".into()));
        }
        let (dims, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| runtime("the output tensor", &e))?;
        check_output(&self.shape, dims, data.len())?;
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_runtime_is_an_error_not_a_panic() {
        let e = locate_runtime(None).unwrap_err();
        assert!(e.starts_with(NOT_FOUND) && e.contains(RUNTIME_DIR_ENV), "{e}");
        let e = locate_runtime(Some(Path::new(""))).unwrap_err();
        assert!(e.starts_with(NOT_FOUND), "{e}");
        let e = locate_runtime(Some(Path::new("relative/dir"))).unwrap_err();
        assert!(e.starts_with(NOT_FOUND) && e.contains("absolute"), "{e}");
        let none = std::env::temp_dir().join("lightcraft-no-such-onnxruntime-folder");
        let e = locate_runtime(Some(&none)).unwrap_err();
        assert!(e.starts_with(NOT_FOUND), "{e}");
    }

    #[test]
    fn a_tile_outside_the_supported_sizes_does_not_load() {
        // checked before any runtime is needed
        let e = OnnxRunner::load_with(Path::new("model.onnx"), 0, ExecutionProvider::Cpu).err();
        assert!(matches!(e, Some(Error::Load(_))));
    }

    /// Needs ONNX Runtime in `LIGHTCRAFT_ONNXRUNTIME_DIR` and a denoise model in `LIGHTCRAFT_DENOISE_ONNX` (the
    /// `.onnx` file), with its tile in `LIGHTCRAFT_DENOISE_TILE` (default 512).
    #[test]
    #[ignore = "needs the ONNX Runtime library and a denoise .onnx file on this computer"]
    fn the_model_runs_on_the_platform_accelerator() {
        let model = std::env::var_os("LIGHTCRAFT_DENOISE_ONNX").map(PathBuf::from).expect("LIGHTCRAFT_DENOISE_ONNX");
        let tile = std::env::var("LIGHTCRAFT_DENOISE_TILE").ok().and_then(|v| v.parse().ok()).unwrap_or(512);
        let runner = OnnxRunner::load(&model, tile).expect("accelerated session");
        let shape = tile_shape(tile).expect("tile");
        let out = runner.run(&vec![0.25; shape.input_len]).expect("one tile");
        assert_eq!(out.len(), shape.output_len);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
