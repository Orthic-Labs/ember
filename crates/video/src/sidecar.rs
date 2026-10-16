//! The verified ffmpeg/ffprobe sidecar: where it is looked for, how it is checked, and the two commands run through it.
//!
//! Windows and macOS only (`rightkit-ffmpeg` has approved catalog rows for no other platform). Elsewhere every call is
//! [`VideoError::Unsupported`].

pub(crate) use native::{available, ffprobe_json, frame_rgba};
#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
pub(crate) use native::resolve_in;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod native {
    use std::path::Path;

    use crate::{Limits, VideoError};

    pub(crate) fn available() -> Result<(), VideoError> {
        Err(VideoError::Unsupported)
    }

    pub(crate) fn ffprobe_json(_path: &Path, _limits: &Limits) -> Result<String, VideoError> {
        Err(VideoError::Unsupported)
    }

    pub(crate) fn frame_rgba(_path: &Path, _t: f64, _w: u32, _h: u32, _limits: &Limits) -> Result<Option<Vec<u8>>, VideoError> {
        Err(VideoError::Unsupported)
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod native {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use rightkit_ffmpeg::runner::run_capture_with;
    use rightkit_ffmpeg::{
        CancellationToken, DECODE_VARIANT, FfmpegCommand, MediaError, RunError, RunOptions, Tool, VerifiedFfmpeg, VerifiedFfprobe,
        verify_bundled_ffmpeg, verify_bundled_ffprobe,
    };

    use crate::{Limits, MAX_PROBE_JSON, VideoError};

    /// Names a directory holding the sidecar (`ffmpeg` and `ffprobe`), ahead of the places a release bundle puts it. The binaries are
    /// verified against the approved catalog wherever they are found, so this only says where to look.
    pub(crate) const DIR_ENV: &str = "LIGHTCRAFT_FFMPEG_DIR";

    /// How long checking a binary (its `-version` banner) may take.
    const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
    /// The part of ffmpeg's error output a [`VideoError::Failed`] keeps.
    const MAX_TAIL: usize = 600;

    /// The verified binaries, kept once they have passed.
    pub(crate) struct Tools {
        ffmpeg: VerifiedFfmpeg,
        ffprobe: VerifiedFfprobe,
    }

    static TOOLS: Mutex<Option<Arc<Tools>>> = Mutex::new(None);

    /// Where a release bundle keeps the sidecar, most explicit first: the environment override, then beside the executable
    /// (Windows install folder, a macOS `MacOS` folder), in an `ffmpeg` folder there, and in the macOS bundle's `Resources`.
    fn search_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(dir) = std::env::var_os(DIR_ENV).filter(|d| !d.is_empty()) {
            dirs.push(PathBuf::from(dir));
        }
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
            dirs.push(exe_dir.clone());
            dirs.push(exe_dir.join("ffmpeg"));
            if let Some(contents) = exe_dir.parent() {
                dirs.push(contents.join("Resources"));
                dirs.push(contents.join("Resources").join("ffmpeg"));
            }
        }
        dirs
    }

    fn map_media_error(e: MediaError) -> VideoError {
        match e {
            MediaError::ArtifactNotFound | MediaError::OverrideMissing { .. } => VideoError::NotInstalled,
            MediaError::NoApprovedArtifact { .. } | MediaError::ArtifactPending { .. } => VideoError::Unsupported,
            // a digest that is not the approved build, a banner that is not LGPL, a probe that failed: never worked around
            other => VideoError::Rejected(other.to_string()),
        }
    }

    /// Verify the first directory of `dirs` that holds both programs. No program is run before its content digest matches the
    /// approved catalog row.
    pub(crate) fn resolve_in(dirs: &[PathBuf]) -> Result<Tools, VideoError> {
        let (ffmpeg_name, ffprobe_name) = (Tool::Ffmpeg.file_name(std::env::consts::OS), Tool::Ffprobe.file_name(std::env::consts::OS));
        let dir = dirs.iter().find(|d| d.join(&ffmpeg_name).is_file() && d.join(&ffprobe_name).is_file()).ok_or(VideoError::NotInstalled)?;
        let ffmpeg = verify_bundled_ffmpeg(&dir.join(ffmpeg_name), DECODE_VARIANT, VERIFY_TIMEOUT).map_err(map_media_error)?;
        let ffprobe = verify_bundled_ffprobe(&dir.join(ffprobe_name), DECODE_VARIANT, VERIFY_TIMEOUT).map_err(map_media_error)?;
        Ok(Tools { ffmpeg, ffprobe })
    }

    /// The verified binaries. A pass is kept for the process; a miss is looked up again next time (the sidecar may have been
    /// installed meanwhile), which costs a few `stat` calls.
    fn tools() -> Result<Arc<Tools>, VideoError> {
        let mut kept = TOOLS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tools) = kept.as_ref() {
            return Ok(tools.clone());
        }
        let tools = Arc::new(resolve_in(&search_dirs())?);
        *kept = Some(tools.clone());
        Ok(tools)
    }

    pub(crate) fn available() -> Result<(), VideoError> {
        tools().map(|_| ())
    }

    fn token(limits: &Limits) -> CancellationToken {
        limits.cancel.as_ref().map_or_else(CancellationToken::new, |flag| CancellationToken::from_flag(flag.clone()))
    }

    fn map_run_error(e: RunError) -> VideoError {
        match e {
            RunError::TimedOut(after) => VideoError::TimedOut(after),
            RunError::Cancelled => VideoError::Cancelled,
            RunError::Failed { stderr_tail, .. } => VideoError::Failed(tail(&stderr_tail)),
            other => VideoError::Failed(tail(&other.to_string())),
        }
    }

    /// The end of `text`, cut at a character boundary.
    fn tail(text: &str) -> String {
        let text = text.trim();
        if text.len() <= MAX_TAIL {
            return text.to_string();
        }
        let mut start = text.len() - MAX_TAIL;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text.get(start..).unwrap_or_default().to_string()
    }

    /// `path` as an absolute path, so it can never be read as an option (`-i` style) or a protocol name by the program.
    fn input_of(path: &Path) -> Result<PathBuf, VideoError> {
        std::path::absolute(path).map_err(|e| VideoError::Invalid(format!("{}: {e}", path.display())))
    }

    /// `ffprobe`'s JSON for `path`: format and streams.
    pub(crate) fn ffprobe_json(path: &Path, limits: &Limits) -> Result<String, VideoError> {
        let tools = tools()?;
        let input = input_of(path)?;
        let mut cmd = FfmpegCommand::new();
        // `file` only: a crafted playlist or concat file inside a "video" can name no URL and no other protocol
        cmd.texts(["-v", "error", "-protocol_whitelist", "file", "-print_format", "json", "-show_format", "-show_streams"]).path(&input);
        let out = run_capture_with(&tools.ffprobe, &cmd, RunOptions::new(limits.timeout, token(limits))).map_err(map_run_error)?;
        if !out.status.success() {
            return Err(VideoError::Probe(format!("{}: {}", path.display(), tail(&out.stderr))));
        }
        if out.stdout.len() > MAX_PROBE_JSON {
            return Err(VideoError::Probe("the probe output is too large".into()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// One upright frame of `path` at `t` seconds as `w` x `h` RGBA bytes: `Some` when ffmpeg wrote exactly that, `None` when it
    /// wrote no frame (a seek past the end), an error for anything else. The output is bounded by what is asked for.
    pub(crate) fn frame_rgba(path: &Path, t: f64, w: u32, h: u32, limits: &Limits) -> Result<Option<Vec<u8>>, VideoError> {
        let tools = tools()?;
        let input = input_of(path)?;
        let expected = u64::from(w) * u64::from(h) * 4;
        let mut cmd = FfmpegCommand::new();
        cmd.texts(["-hide_banner", "-nostdin", "-v", "error", "-protocol_whitelist", "file", "-ss"]).text(format!("{t:.3}")).input(&input);
        // the frame is scaled to the exact size asked for (after the automatic rotation), so the output is that many bytes;
        // `-fs` is a second bound on the output file, a page above it
        cmd.texts(["-an", "-sn", "-dn", "-frames:v", "1", "-vf"]).text(format!("scale={w}:{h}:flags=bicubic"));
        cmd.texts(["-pix_fmt", "rgba", "-f", "rawvideo", "-fs"]);
        cmd.text((expected + 4096).to_string()).text("pipe:1");
        let out = run_capture_with(&tools.ffmpeg, &cmd, RunOptions::new(limits.timeout, token(limits))).map_err(map_run_error)?;
        if !out.status.success() {
            return Err(VideoError::Failed(tail(&out.stderr)));
        }
        match u64::try_from(out.stdout.len()) {
            Ok(0) => Ok(None),
            Ok(n) if n == expected => Ok(Some(out.stdout)),
            _ => Err(VideoError::Failed(format!("ffmpeg wrote {} bytes for a {w}x{h} frame", out.stdout.len()))),
        }
    }
}
