//! `ember-av1-worker`: decodes one AV1 still in its own process, so that a stream that makes rav1d
//! abort (its panics cannot unwind out of `extern "C"`) takes down this process, not the app.
//!
//! Protocol (see `lightcraft_av1dec::wire`): one request on stdin, one picture or error frame on
//! stdout. Exit status: 0 picture sent; 2 bad request; 3 the decoder refused the stream (error
//! frame sent); 4 stdout failed. Anything else (an abort, a signal) is a crash the host reports.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    use lightcraft_av1dec::{decode_in_process, wire};
    use std::io::{BufWriter, Write};
    use std::process::ExitCode;

    let data = match wire::read_request(&mut std::io::stdin().lock()) {
        Ok(d) => d,
        Err(_) => return ExitCode::from(2),
    };
    let mut out = BufWriter::new(std::io::stdout().lock());
    match decode_in_process(&data) {
        Ok(pic) => {
            if wire::write_picture(&mut out, &pic).and_then(|()| out.flush()).is_err() {
                return ExitCode::from(4);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if wire::write_error(&mut out, &e).and_then(|()| out.flush()).is_err() {
                return ExitCode::from(4);
            }
            ExitCode::from(3)
        }
    }
}

/// wasm32 has no processes; the worker is a stub there so the workspace still builds.
#[cfg(target_arch = "wasm32")]
fn main() {}
