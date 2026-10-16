//! Last-resort panic guard (never-crash standard, `AGENTS.md`): a panic that escapes a command
//! becomes that command's error and the session (catalog, library, undo history) stays usable.
//! It's a safety net, not a way to report errors: commands return `Err` themselves.
//!
//! Crash reports belong to RightKit (`rightkit-crash`, capability `crash-reporting`): the app
//! installs its panic hook through [`install_hook`], and a panic [`catch`] contained is also
//! recorded as a `guard_caught` report that names what was running. Native builds only; on wasm32
//! (built with `panic = "abort"`) there is no hook and nothing to catch.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

/// The crash-report settings [`install_hook`] installed; `catch` records through them.
#[cfg(not(target_arch = "wasm32"))]
static CRASH: OnceLock<rightkit_crash::CrashConfig> = OnceLock::new();

/// The text of a panic payload (`panic!("…")` gives a `&str` or a `String`).
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    match (payload.downcast_ref::<&str>(), payload.downcast_ref::<String>()) {
        (Some(s), _) => (*s).to_string(),
        (_, Some(s)) => s.clone(),
        _ => "unknown panic".into(),
    }
}

/// Run `f`; a panic inside it becomes `Err("<what> failed unexpectedly: <message>")`. On wasm32
/// (built with `panic = "abort"`) nothing can be caught and this just runs `f`.
pub fn catch<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        let msg = format!("{what} failed unexpectedly: {}", panic_message(p.as_ref()));
        log::error!("{msg}");
        record_caught(&msg);
        msg
    })
}

/// Record a contained panic through `rightkit-crash` (best effort), when [`install_hook`] ran.
#[cfg(not(target_arch = "wasm32"))]
fn record_caught(msg: &str) {
    if let Some(config) = CRASH.get() {
        let thread = std::thread::current();
        let _ = rightkit_crash::record_report(config, "guard_caught", msg, None, thread.name().unwrap_or("unnamed"), "");
    }
}

#[cfg(target_arch = "wasm32")]
fn record_caught(_msg: &str) {}

/// Install `rightkit-crash`'s panic hook: it keeps the default report (stderr) and writes a
/// redacted report for every panic into `crash_dir` (best effort), so a panic in a GUI session
/// leaves a trace. Call once, early in `main`; a second call does nothing.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_hook(crash_dir: PathBuf) {
    let config = rightkit_crash::CrashConfig::new("Ember", env!("CARGO_PKG_VERSION"), crash_dir).process_kind(rightkit_crash::ProcessKind::Engine);
    if CRASH.set(config.clone()).is_ok() {
        rightkit_crash::install_process_hook(rightkit_crash::ProcessKind::Engine, config);
    }
}

/// The web build has no panic hook to install (`panic = "abort"`).
#[cfg(target_arch = "wasm32")]
pub fn install_hook(_crash_dir: PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_an_error() {
        assert_eq!(catch("ok", || 7), Ok(7));
        let e = catch("`demo.cmd`", || -> u32 { panic!("boom {}", 1) }).unwrap_err();
        assert_eq!(e, "`demo.cmd` failed unexpectedly: boom 1");
        assert!(catch("x", || std::panic::panic_any(5u8)).unwrap_err().ends_with("unknown panic"));
    }
}
