//! Downloading a model's files, only when the user asked for it: from an ordered list of mirrors
//! (the next one when a mirror fails), resuming partial files, with connect and stall timeouts,
//! a size cap, progress and cancellation, and checked (exact size, SHA-256 where pinned) before
//! each file is moved into the model folder. Runs on whatever thread calls [`download`] (the
//! engine uses a background thread; the UI thread never waits for the network).
//!
//! The transfer itself (vetted HTTPS through rightkit-http, `<name>.part` plus its resume record,
//! strong-ETag byte-range resume, bounded retries, the SHA-256 check and the atomic rename into
//! place) is the rightkit-models owner's `download_resumable`. This crate adds what the owner
//! leaves to the app: Ember's mirror list and fallback, the size cap, progress across files, the
//! bearer token scoped to one host, and the rule that a model folder never holds a half-written
//! file under a real name. A file that fails its check is deleted, never kept. The `http` module
//! stays for the small bounded requests (map tiles, AI calls) that are not model downloads.
//!
//! What to download and where from belongs to the caller: it passes the [`FileSpec`]s and the
//! default mirrors (see [`mirrors`]). `lightcraft-segment` does so for the SAM 3 weights, which
//! are never part of Ember (SAM License, see docs/ai-masks.md).
//!
//! Native only: on wasm32 this crate is empty (the web build downloads no models).

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod http;

use std::cell::RefCell;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rightkit_models::ErrorCode;
use rightkit_models::download::{DownloadRequest, download_resumable, part_path_for};
use rightkit_models::http::{ModelHttp, ModelHttpOptions};
use sha2::{Digest, Sha256};

use http::Url;

/// One file of the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSpec<'a> {
    pub name: &'a str,
    /// Exact size in bytes, when pinned (then the download resumes and is checked against it).
    pub size: Option<u64>,
    /// SHA-256 (lower-case hex), when pinned.
    pub sha256: Option<&'a str>,
    /// Largest size accepted when `size` is not pinned.
    pub max: u64,
}

/// Lower-case SHA-256 receipt for already-read bytes, without reopening their source.
///
/// Caller bounds input size before reading or hashing it. A digest records identity;
/// authenticity still requires comparison with an independently pinned value.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Verify already-read bytes so callers parse exactly what was hashed, without reopening files.
pub fn verify_bytes(spec: &FileSpec<'_>, bytes: &[u8]) -> Result<bool, String> {
    validate_files(std::slice::from_ref(spec)).map_err(|error| error.to_string())?;
    let size = u64::try_from(bytes.len()).map_err(|_| "byte length exceeds verification limit".to_string())?;
    if size > spec.max || spec.size.is_some_and(|expected| expected != size) {
        return Ok(false);
    }
    Ok(spec.sha256.is_none_or(|expected| sha256_bytes(bytes) == expected))
}

#[cfg(test)]
mod byte_verification_tests {
    use super::{FileSpec, sha256_bytes, verify_bytes};

    #[test]
    fn digest_receipt_is_stable_lower_case_hex() {
        assert_eq!(sha256_bytes(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_bytes(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn verifies_exact_bytes_size_hash_and_spec() {
        let spec =
            FileSpec { name: "model.bin", size: Some(3), max: 3, sha256: Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") };
        assert!(verify_bytes(&spec, b"abc").unwrap());
        assert!(!verify_bytes(&spec, b"abd").unwrap());
        assert!(!verify_bytes(&spec, b"abcd").unwrap());
        assert!(verify_bytes(&FileSpec { max: 0, ..spec }, b"abc").is_err());
    }
}

/// Most mirrors used (a hostile list can't make a download loop forever).
const MAX_MIRRORS: usize = 16;
/// Redirects followed per request.
const MAX_REDIRECTS: u32 = 8;

/// The mirrors to try, in order: `env`'s (base URLs separated by commas, spaces or newlines), then
/// the mirrors file's (one base URL per line, `#` comments), then `defaults`; duplicates and
/// unusable URLs dropped, at most 16.
pub fn mirrors(env: Option<&str>, file: Option<&Path>, defaults: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        let s = s.trim().trim_end_matches('/');
        if !s.is_empty() && Url::parse(s).is_ok() && !out.iter().any(|m| m == s) && out.len() < MAX_MIRRORS {
            out.push(s.to_string());
        }
    };
    if let Some(env) = env {
        env.split([',', ' ', '\n', '\t']).for_each(&mut add);
    }
    // a small text file; anything larger isn't a mirror list
    if let Some(text) = file.and_then(|f| std::fs::metadata(f).ok().filter(|m| m.len() < 64 * 1024).and_then(|_| std::fs::read_to_string(f).ok())) {
        text.lines().map(|l| l.split('#').next().unwrap_or_default()).for_each(&mut add);
    }
    defaults.iter().for_each(|m| add(m));
    out
}

/// Download settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Longest wait to connect to a mirror.
    pub connect_timeout: Duration,
    /// Longest wait for the next bytes once connected.
    pub stall_timeout: Duration,
    /// Tries per mirror and file (each resumes where the last one stopped).
    pub attempts: u32,
    /// The environment variable holding a bearer token, sent to each configured mirror's own host (only over https,
    /// never to a host a redirect leads to), so list only mirrors that may see it. `None`: nothing is ever sent, so one model's token cannot leak to another's server.
    pub token_env: Option<&'static str>,
}

impl Default for Options {
    fn default() -> Self {
        Options { connect_timeout: Duration::from_secs(15), stall_timeout: Duration::from_secs(30), attempts: 3, token_env: None }
    }
}

/// Where a download is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub file: String,
    /// Bytes of all files on disk so far, and the total expected (pinned sizes + announced ones).
    pub done: u64,
    pub total: u64,
    /// The mirror in use (without query).
    pub mirror: String,
}

/// Why a download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// No mirror is configured.
    NoMirrors,
    /// Invalid filename, size, hash or aggregate bound supplied by the caller.
    Invalid(String),
    Cancelled,
    /// A local file problem (folder not writable, disk full…).
    Disk(String),
    /// Every mirror failed for `file`; the reason for each.
    AllMirrorsFailed {
        file: String,
        errors: Vec<String>,
    },
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Invalid(e) => write!(f, "invalid download: {e}"),
            DownloadError::NoMirrors => write!(f, "no download location is configured for this model in this build"),
            DownloadError::Cancelled => write!(f, "the download was cancelled"),
            DownloadError::Disk(e) => write!(f, "{e}"),
            DownloadError::AllMirrorsFailed { file, errors } => write!(f, "could not download {file}: {}", errors.join("; ")),
        }
    }
}

/// One mirror's failure for one file.
enum Fail {
    /// Try the next mirror.
    NextMirror(String),
    Cancelled,
}

/// Fetch a single file from its exact URL, preserving any query string. The local filename
/// remains `file.name`; it never comes from redirects or response headers.
pub fn download_url(
    file: &FileSpec<'_>,
    url: &str,
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), DownloadError> {
    download_source(std::slice::from_ref(file), &[url.to_string()], dir, opts, cancel, progress, true)
}

/// Download `files` into `dir` from `mirrors` (in order; see [`mirrors`]). Verified files
/// are kept. `progress` is called as bytes arrive (often: throttle in it).
pub fn download(
    files: &[FileSpec<'_>],
    mirrors: &[String],
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), DownloadError> {
    download_source(files, mirrors, dir, opts, cancel, progress, false)
}

fn download_source(
    files: &[FileSpec<'_>],
    mirrors: &[String],
    dir: &Path,
    opts: &Options,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    exact_urls: bool,
) -> Result<(), DownloadError> {
    let total = validate_files(files)?;
    if mirrors.len() > MAX_MIRRORS {
        return Err(DownloadError::Invalid("too many mirrors".into()));
    }
    std::fs::create_dir_all(dir).map_err(|e| DownloadError::Disk(format!("can't create {}: {e}", dir.display())))?;
    // what's left to fetch
    let mut todo = Vec::new();
    for f in files {
        if !verified(f, &dir.join(f.name))? {
            todo.push(f);
        }
    }
    if todo.is_empty() {
        return Ok(());
    }
    if mirrors.is_empty() {
        return Err(DownloadError::NoMirrors);
    }
    let mut before: u64 = files.iter().filter(|f| !todo.contains(f)).filter_map(|f| f.size).sum();
    let http = transport(opts);
    for f in todo {
        let mut errors = Vec::new();
        let mut ok = false;
        for m in mirrors {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let url = if exact_urls { m.clone() } else { format!("{m}/{}", f.name) };
            let mut report = |n: u64, announced: u64| {
                let mirror = Url::parse(m).map(|u| u.display()).unwrap_or_default();
                progress(&Progress {
                    file: f.name.to_string(),
                    done: before.saturating_add(n),
                    total: total.max(before.saturating_add(announced)),
                    mirror,
                })
            };
            // a token for the configured host only (the owner client keeps a secret header off a redirect's host)
            let token = Url::parse(m).ok().and_then(|u| {
                let origin = u.host.clone();
                bearer(opts.token_env.and_then(|name| std::env::var(name).ok()).as_deref(), &u, &origin).map(str::to_string)
            });
            match fetch_file(&http, f, &url, dir, token.as_deref(), cancel, &mut report) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(Fail::Cancelled) => return Err(DownloadError::Cancelled),
                Err(Fail::NextMirror(e)) => {
                    log::warn!("download of {}: {e}", short(&url));
                    errors.push(format!("{}: {e}", short(m)));
                }
            }
        }
        if !ok {
            return Err(DownloadError::AllMirrorsFailed { file: f.name.to_string(), errors });
        }
        before = before.saturating_add(std::fs::metadata(dir.join(f.name)).map(|m| m.len()).unwrap_or(0));
    }
    Ok(())
}

/// Validate before touching disk, including names supplied by a user model catalog.
fn validate_files(files: &[FileSpec<'_>]) -> Result<u64, DownloadError> {
    if files.len() > 256 {
        return Err(DownloadError::Invalid("too many files".into()));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for f in files {
        let stem = f.name.split('.').next().unwrap_or_default().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
        if reserved
            || f.name.ends_with(['.', ' '])
            || f.name.ends_with(".part")
            || f.name.is_empty()
            || f.name.len() > 255
            || f.name.starts_with('.')
            || f.name.contains(['/', '\\', ':'])
            || f.name.chars().any(char::is_control)
            || !seen.insert(f.name)
        {
            return Err(DownloadError::Invalid("a filename must be a unique, ordinary filename".into()));
        }
        if f.max == 0
            || f.size.is_some_and(|n| n == 0 || n > f.max)
            || f.sha256.is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err(DownloadError::Invalid("invalid file size or SHA-256".into()));
        }
        total = total.checked_add(f.size.unwrap_or(0)).ok_or_else(|| DownloadError::Invalid("total size overflow".into()))?;
    }
    Ok(total)
}

/// The bearer token to send to `url`: only a non-empty one, only over https, and only to the host the download began
/// at (`origin`), never to the host a redirect leads to.
fn bearer<'a>(token: Option<&'a str>, url: &Url, origin: &str) -> Option<&'a str> {
    token.map(str::trim).filter(|t| !t.is_empty() && url.tls && url.host == origin)
}

/// A mirror for messages (no query string: it may hold a token).
fn short(m: &str) -> String {
    Url::parse(m).map(|u| u.display()).unwrap_or_else(|_| "mirror".into())
}

/// Whether `path` is already the right file (exact size and hash when pinned).
fn verified(f: &FileSpec<'_>, path: &Path) -> Result<bool, DownloadError> {
    verify_file_progress(f, path, |_, _| true).map(|result| result.unwrap_or(false))
}

/// Verify an existing file while reporting streamed hash progress. `None` means callback
/// requested cancellation; no file is changed.
pub fn verify_file_progress<F>(f: &FileSpec<'_>, path: &Path, mut progress: F) -> Result<Option<bool>, DownloadError>
where
    F: FnMut(u64, u64) -> bool,
{
    let Ok(meta) = std::fs::metadata(path) else { return Ok(Some(false)) };
    if !meta.is_file() || meta.len() == 0 {
        return Ok(Some(false));
    }
    if f.size.is_some_and(|s| s != meta.len()) || meta.len() > f.max {
        return Ok(Some(false));
    }
    match f.sha256 {
        Some(want) => Ok(sha256_file_progress(path, &mut progress).map_err(DownloadError::Disk)?.map(|got| got == want)),
        None => Ok(Some(true)),
    }
}

/// Verify an existing file against its bounded manifest entry without loading it into memory.
///
/// This is also used when a user selects a model folder, so manually supplied files receive
/// the same size and pinned SHA-256 checks as downloaded files.
pub fn verify_file(f: &FileSpec<'_>, path: &Path) -> Result<bool, DownloadError> {
    verified(f, path)
}

fn sha256_file_progress<F>(path: &Path, progress: &mut F) -> Result<Option<String>, String>
where
    F: FnMut(u64, u64) -> bool,
{
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let total = file.metadata().map_err(|e| format!("{}: {e}", path.display()))?.len();
    let mut done = 0_u64;
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
        done = done.saturating_add(n as u64);
        if !progress(done, total) {
            return Ok(None);
        }
    }
    Ok(Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect()))
}

fn part_path(dir: &Path, name: &str) -> PathBuf {
    part_path_for(&dir.join(name))
}

/// Remove the partial download of `name` in `dir` and its resume record, whatever state they are in. A caller that
/// cancels a download it will not resume (the engine's model staging) calls this so nothing is left behind.
pub fn discard_partial(dir: &Path, name: &str) {
    rightkit_models::download::discard_partial(&part_path(dir, name));
}

/// The owner's transport for one download call: its vetted client (public-web address policy, per-hop redirect
/// checks, the OS trust store, environment proxies), with Ember's connect and stall limits, bounded retries per
/// mirror (`attempts` tries in all, each resuming where the last stopped) and a user agent naming Ember.
fn transport(opts: &Options) -> ModelHttp {
    let stall = opts.stall_timeout;
    let options = ModelHttpOptions {
        connect_timeout: Some(opts.connect_timeout),
        header_timeout: Some(stall),
        read_timeout: Some(stall),
        max_redirects: MAX_REDIRECTS,
        retries: opts.attempts.clamp(1, 16).saturating_sub(1),
        app: Some(("Ember".to_string(), env!("CARGO_PKG_VERSION").to_string())),
        ..base_options()
    };
    ModelHttp::new(options)
}

#[cfg(not(any(test, feature = "testing")))]
fn base_options() -> ModelHttpOptions {
    ModelHttpOptions::default()
}

/// Tests talk to a loopback server; the owner's public-web address policy would refuse it. Compiled only for this
/// crate's tests and for a build that asks for the `testing` feature (the engine's tests, as a dev-dependency).
#[cfg(any(test, feature = "testing"))]
fn base_options() -> ModelHttpOptions {
    ModelHttpOptions {
        address_policy: rightkit_http::testing::loopback_config().address_policy,
        proxy: rightkit_models::http::ProxyMode::None,
        ..ModelHttpOptions::default()
    }
}

/// Remove anything at the partial download's names that is not a plain file (a link, which the owner's `File::create`
/// would write through): the link itself goes, never its target (issue #1502).
fn clear_links(part: &Path) {
    let mut record = part.as_os_str().to_os_string();
    record.push(".meta");
    let mut staged = record.clone();
    staged.push(".meta");
    for path in [part.to_path_buf(), PathBuf::from(record), PathBuf::from(staged)] {
        if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Fetch one file from one mirror into `<name>.part` (resuming it when the owner can prove it is the same file),
/// check it and move it into place.
fn fetch_file(
    http: &ModelHttp,
    f: &FileSpec<'_>,
    url: &str,
    dir: &Path,
    token: Option<&str>,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(u64, u64),
) -> Result<(), Fail> {
    let dest = dir.join(f.name);
    let part = part_path(dir, f.name);
    let limit = f.size.unwrap_or(f.max);
    clear_links(&part);
    // set when the server's file cannot be the expected one: the transfer is stopped through the cancel hook
    let refused: RefCell<Option<String>> = RefCell::new(None);
    let stop = || cancel.load(Ordering::Relaxed) || refused.borrow().is_some();
    let progress = |received: u64, announced: u64| {
        if refused.borrow().is_none() {
            if announced > limit || (announced != 0 && f.size.is_some_and(|s| s != announced)) {
                *refused.borrow_mut() = Some(format!("the server's file has the wrong size ({announced} bytes)"));
            } else if received > limit {
                *refused.borrow_mut() = Some(format!("the server sent more than the expected {limit} bytes"));
            }
        }
        report(received, f.size.unwrap_or(announced));
    };
    let authorization = token.map(|t| format!("Bearer {t}"));
    let request = DownloadRequest {
        url,
        part_path: &part,
        dest_path: &dest,
        expected_sha256: f.sha256,
        authorization: authorization.as_deref(),
        cancelled: &stop,
    };
    let outcome = download_resumable(http, request, progress);
    let why = refused.borrow_mut().take();
    if let Some(why) = why {
        rightkit_models::download::discard_partial(&part);
        return Err(Fail::NextMirror(why));
    }
    match outcome {
        Ok(_) => {}
        Err(e) if e.code == ErrorCode::Cancelled => return Err(Fail::Cancelled),
        Err(e) if e.code == ErrorCode::HashMismatch => {
            return Err(Fail::NextMirror(format!("the file is damaged or not the official one (SHA-256 check failed: {})", e.message)));
        }
        Err(e) => return Err(Fail::NextMirror(e.message)),
    }
    // the owner checked the hash; the size is ours to check (an unpinned file has only the cap)
    let len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    if len == 0 || f.size.is_some_and(|s| s != len) || len > limit {
        let _ = std::fs::remove_file(&dest);
        return Err(Fail::NextMirror(if len == 0 {
            "the server sent an empty file".to_string()
        } else {
            format!("the file has the wrong size ({len} bytes)")
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
