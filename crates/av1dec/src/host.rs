//! The host side of out-of-process decoding: run `ember-av1-worker`, feed it the stream, read the
//! picture back, and turn every way it can go wrong into an [`Error`].
//!
//! rav1d aborts the whole process on some malformed streams (a panic inside its `extern "C"`
//! entry points cannot unwind, and `catch_unwind` cannot stop an abort). Decoding therefore runs
//! in a child process: whatever the stream does to the child, the host sees an exit status or a
//! malformed answer, never its own death.

use crate::wire::{self, Response};
use crate::{Error, MAX_PIXELS, Picture};
use rightkit_child_process::RunOptions;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

/// File name of the worker executable (without the platform suffix).
pub const WORKER_NAME: &str = "ember-av1-worker";
/// Environment variable that overrides where the worker is looked for (tests, unusual installs).
pub const WORKER_ENV: &str = "EMBER_AV1_WORKER";
/// How long one decode may take before the worker is killed.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// `Error::Decoder` code for a worker that did not answer in time.
pub const CODE_TIMEOUT: i32 = -2;
/// `Error::Decoder` code for a worker that could not be started or talked to.
pub const CODE_SPAWN: i32 = -3;

/// Most bytes kept from the worker's stdout: the largest picture the wire reader accepts (a
/// 4:4:4 picture of [`MAX_PIXELS`] in three 16-bit planes, plus its headers), with slack. A longer
/// answer is cut here and then fails to parse, so it is an error, not a memory problem.
fn stdout_cap() -> usize {
    let bytes = 64u64.saturating_add(6u64.saturating_mul(MAX_PIXELS));
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// Where the worker lives, if it can be found: [`WORKER_ENV`], else next to the running
/// executable (or, for a cargo test binary under `target/<profile>/deps`, in `target/<profile>`).
pub fn locate_worker() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(WORKER_ENV) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let file = format!("{WORKER_NAME}{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut dirs = vec![dir];
    if dir.file_name().is_some_and(|n| n == "deps") {
        dirs.extend(dir.parent());
    }
    dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Decode with the worker found by [`locate_worker`].
pub fn decode(data: &[u8]) -> Result<Picture, Error> {
    if data.is_empty() {
        return Err(Error::Empty);
    }
    let worker = locate_worker().ok_or(Error::HelperMissing)?;
    decode_with_worker(&worker, data, TIMEOUT)
}

fn crashed(status: ExitStatus) -> Error {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|s| -s)
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    Error::Decoder(status.code().or(signal).unwrap_or(-1))
}

/// Decode `data` in the given worker executable, killing it after `timeout`.
///
/// The worker is run through `rightkit-child-process`: it is owned (its whole tree is terminated
/// at the deadline), it gets no console window on Windows, and stdin is the framed request.
pub fn decode_with_worker(worker: &Path, data: &[u8], timeout: Duration) -> Result<Picture, Error> {
    if data.is_empty() {
        return Err(Error::Empty);
    }
    let mut request = Vec::with_capacity(data.len().saturating_add(32));
    wire::write_request(&mut request, data).map_err(|_| Error::Malformed("AV1 request could not be framed"))?;
    let options = RunOptions::new(timeout).stdout_cap(stdout_cap()).stderr_cap(0);
    let output = match rightkit_child_process::run_with_stdin(worker, std::iter::empty::<&str>(), &request, &options) {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::HelperMissing),
        Err(_) => return Err(Error::Decoder(CODE_SPAWN)),
    };
    if output.timed_out {
        return Err(Error::Decoder(CODE_TIMEOUT));
    }
    let mut answer = output.stdout.bytes.as_slice();
    let response = wire::read_response(&mut answer);
    match (response, output.status.success()) {
        (Ok(Response::Picture(p)), true) => Ok(p),
        (Ok(Response::Picture(_)), false) => Err(crashed(output.status)),
        (Ok(Response::Failure(e)), _) => Err(e),
        (Err(_), false) => Err(crashed(output.status)),
        (Err(e), true) => Err(e),
    }
}
