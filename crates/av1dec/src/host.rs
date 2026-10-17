//! The host side of out-of-process decoding: run `ember-av1-worker`, feed it the stream, read the
//! picture back, and turn every way it can go wrong into an [`Error`].
//!
//! rav1d aborts the whole process on some malformed streams (a panic inside its `extern "C"`
//! entry points cannot unwind, and `catch_unwind` cannot stop an abort). Decoding therefore runs
//! in a child process: whatever the stream does to the child, the host sees an exit status or a
//! malformed answer, never its own death.

use crate::wire::{self, Response};
use crate::{Error, Picture};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

/// Kills and reaps the child when dropped, whatever path leaves the decode.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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
pub fn decode_with_worker(worker: &Path, data: &[u8], timeout: Duration) -> Result<Picture, Error> {
    if data.is_empty() {
        return Err(Error::Empty);
    }
    let mut cmd = Command::new(worker);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console window flashes up when a GUI app decodes.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => Reaper(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::HelperMissing),
        Err(_) => return Err(Error::Decoder(CODE_SPAWN)),
    };
    let (Some(mut stdin), Some(stdout)) = (child.0.stdin.take(), child.0.stdout.take()) else {
        return Err(Error::Decoder(CODE_SPAWN));
    };

    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        // Writer and reader run concurrently so neither pipe can fill up and stall the other.
        scope.spawn(move || {
            // A worker that dies early closes the pipe; its status explains it, so ignore this.
            let _ = wire::write_request(&mut stdin, data);
            drop(stdin);
        });
        scope.spawn(move || {
            let _ = tx.send(wire::read_response(&mut BufReader::new(stdout)));
        });

        let answer = rx.recv_timeout(timeout);
        if answer.is_err() {
            // Timed out (or the reader died): killing the child closes both pipes, which ends
            // both threads before the scope returns.
            let _ = child.0.kill();
        }
        // A worker that answered gets a few seconds to exit on its own; any other is killed now.
        let exit_by = Instant::now() + if answer.is_ok() { Duration::from_secs(5) } else { Duration::ZERO };
        let status = loop {
            match child.0.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if Instant::now() < exit_by => std::thread::sleep(Duration::from_millis(1)),
                Ok(None) => {
                    let _ = child.0.kill();
                    break child.0.wait().ok();
                }
                Err(_) => break None,
            }
        };
        match (answer, status) {
            (Err(_), _) => Err(Error::Decoder(CODE_TIMEOUT)),
            (Ok(_), None) => Err(Error::Decoder(CODE_SPAWN)),
            (Ok(Ok(Response::Picture(p))), Some(s)) if s.success() => Ok(p),
            (Ok(Ok(Response::Picture(_))), Some(s)) => Err(crashed(s)),
            (Ok(Ok(Response::Failure(e))), _) => Err(e),
            (Ok(Err(_)), Some(s)) if !s.success() => Err(crashed(s)),
            (Ok(Err(e)), Some(_)) => Err(e),
        }
    })
}
