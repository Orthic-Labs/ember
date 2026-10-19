#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Ember mobile host bridge: the UniFFI surface the iOS app calls.
//!
//! One [`EmberSession`] owns one Rust session per open library (`docs/ios-architecture.md`). It wraps
//! [`DesktopHandle`], so every request is served by the same session owner, command registry and
//! catalog journal as the desktop host. Requests and responses are JSON strings, the same shapes as the
//! desktop host's: the bridge stays versioned by [`BRIDGE_VERSION`] rather than by a mirrored type tree.
//! Preview pixels never travel as JSON; [`EmberSession::preview`] returns a descriptor with an opaque
//! handle and [`EmberSession::preview_bytes`] returns the encoded image for it.
//!
//! No panic crosses the ABI: every call runs inside `lightcraft_engine::guard`-backed owner threads, and
//! the bridge itself only maps `Result`s. All methods block; Swift calls them off the main actor.

use std::path::PathBuf;

use lightcraft_desktop_host::{DesktopHandle, HostOptions, PreviewRequest};
use serde_json::Value;

uniffi::setup_scaffolding!();

/// Version of the request/response contract. Bump when a JSON shape the app relies on changes.
pub const BRIDGE_VERSION: u32 = 1;

/// Largest JSON parameter payload accepted from the app.
const MAX_PARAMS_BYTES: usize = 4 * 1024 * 1024;
/// Most photos one view slice may request.
const MAX_SLICE: u32 = 1_000;

/// Errors surfaced to Swift as `EmberError`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EmberError {
    /// The request was malformed: bad JSON, an oversized payload or an out-of-range value.
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    /// The session owner rejected or failed the request.
    #[error("{message}")]
    Session { message: String },
    /// The session has been closed.
    #[error("session is closed")]
    Closed,
}

impl EmberError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest { message: message.into() }
    }
}

impl From<String> for EmberError {
    fn from(message: String) -> Self {
        // A call racing shutdown either finds the channel closed or the owner gone before it replies.
        if message.contains("session owner is closed") || message.contains("session owner stopped") {
            Self::Closed
        } else {
            Self::Session { message }
        }
    }
}

/// How to open a session.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SessionOptions {
    /// Library directory inside the app container. `None` uses the engine's default location.
    pub library_path: Option<String>,
    /// Open the procedural demo library instead of a real one (simulator showcase and tests).
    pub demo: bool,
    /// Number of demo photos when `demo` is set.
    pub demo_count: u32,
}

/// Encoded preview bytes for a handle.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PreviewImage {
    pub bytes: Vec<u8>,
    /// MIME type of `bytes`, e.g. `image/jpeg`.
    pub content_type: String,
}

/// The bridge contract version compiled into this library.
#[uniffi::export]
pub fn bridge_version() -> u32 {
    BRIDGE_VERSION
}

/// One open library.
#[derive(uniffi::Object)]
pub struct EmberSession {
    host: DesktopHandle,
}

#[uniffi::export]
impl EmberSession {
    /// Start the session owner for a library.
    #[uniffi::constructor]
    pub fn open(options: SessionOptions) -> Result<std::sync::Arc<Self>, EmberError> {
        let library_path = match options.library_path {
            Some(path) if path.contains('\0') => return Err(EmberError::invalid("library path contains NUL")),
            Some(path) if path.is_empty() => None,
            Some(path) => Some(PathBuf::from(path)),
            None => None,
        };
        let host = DesktopHandle::spawn(HostOptions {
            library_path,
            demo: options.demo,
            demo_count: usize::try_from(options.demo_count.min(10_000)).unwrap_or(0),
            ..HostOptions::default()
        })?;
        Ok(std::sync::Arc::new(Self { host }))
    }

    /// Dispatch a command by id (`photo.rate`, `develop.set`, `library.import`, `app.export`, …) with
    /// JSON object parameters. Returns the command's JSON result.
    pub fn run(&self, id: String, params_json: String) -> Result<String, EmberError> {
        if id.is_empty() || id.len() > 128 {
            return Err(EmberError::invalid("command id must be 1–128 bytes"));
        }
        let params = parse_params(&params_json)?;
        Ok(self.host.run(id, params)?.to_string())
    }

    /// The session snapshot: active photo, selection, view generation, undo depth, jobs, develop state.
    pub fn snapshot(&self) -> Result<String, EmberError> {
        Ok(self.host.snapshot()?.to_string())
    }

    /// A window of the current filtered/sorted library view. `generation` guards against a view that
    /// changed since the snapshot; pass `None` for the latest.
    pub fn view_slice(&self, generation: Option<u64>, offset: u32, limit: u32) -> Result<String, EmberError> {
        let offset = usize::try_from(offset).map_err(|_| EmberError::invalid("offset out of range"))?;
        let limit = usize::try_from(limit.min(MAX_SLICE)).map_err(|_| EmberError::invalid("limit out of range"))?;
        Ok(self.host.view_slice(generation, offset, limit)?.to_string())
    }

    /// Request a rendered preview. `request_json` is a desktop-host `PreviewRequest` (camelCase:
    /// photoId, slot, viewGeneration, width, height, quality "draft"|"full", before, sequence).
    /// Returns the descriptor JSON; fetch the pixels with [`Self::preview_bytes`].
    pub fn preview(&self, request_json: String) -> Result<String, EmberError> {
        let request = parse_preview(&request_json)?;
        let descriptor = self.host.preview(request)?;
        serde_json::to_string(&descriptor).map_err(|error| EmberError::Session { message: error.to_string() })
    }

    /// Like [`Self::preview`] but answers immediately from the cached thumbnail when one exists.
    pub fn preview_quick(&self, request_json: String) -> Result<String, EmberError> {
        let request = parse_preview(&request_json)?;
        let descriptor = self.host.preview_quick(request)?;
        serde_json::to_string(&descriptor).map_err(|error| EmberError::Session { message: error.to_string() })
    }

    /// Encoded bytes for a preview handle, or `None` once the handle has expired.
    pub fn preview_bytes(&self, handle: String) -> Option<PreviewImage> {
        self.host
            .preview_store()
            .get(&handle)
            .map(|preview| PreviewImage { bytes: preview.bytes.to_vec(), content_type: preview.content_type.to_string() })
    }

    /// Release a preview handle the app no longer displays.
    pub fn release_preview(&self, handle: String) -> bool {
        self.host.preview_store().acknowledge(&handle)
    }

    /// Read preferences, or merge a JSON patch into them and return the result.
    pub fn preferences(&self, patch_json: Option<String>) -> Result<String, EmberError> {
        let patch = patch_json.map(|json| parse_params(&json)).transpose()?;
        Ok(self.host.preferences(patch)?.to_string())
    }

    /// Persist the catalog now (the app calls this when it moves to the background).
    pub fn persist(&self) -> Result<(), EmberError> {
        Ok(self.host.persist()?)
    }

    /// Persist and stop the session owner. Later calls return [`EmberError::Closed`].
    pub fn close(&self) -> Result<(), EmberError> {
        Ok(self.host.shutdown()?)
    }
}

fn parse_params(json: &str) -> Result<Value, EmberError> {
    if json.len() > MAX_PARAMS_BYTES {
        return Err(EmberError::invalid("parameters exceed 4 MiB"));
    }
    if json.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let value: Value = serde_json::from_str(json).map_err(|error| EmberError::invalid(format!("parameters are not JSON: {error}")))?;
    if !value.is_object() {
        return Err(EmberError::invalid("parameters must be a JSON object"));
    }
    Ok(value)
}

fn parse_preview(json: &str) -> Result<PreviewRequest, EmberError> {
    if json.len() > MAX_PARAMS_BYTES {
        return Err(EmberError::invalid("preview request exceeds 4 MiB"));
    }
    serde_json::from_str(json).map_err(|error| EmberError::invalid(format!("preview request: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> std::sync::Arc<EmberSession> {
        EmberSession::open(SessionOptions { library_path: None, demo: true, demo_count: 3 }).expect("demo session opens")
    }

    #[test]
    fn rejects_malformed_requests_without_panicking() {
        let session = demo();
        assert!(matches!(session.run(String::new(), "{}".into()), Err(EmberError::InvalidRequest { .. })));
        assert!(matches!(session.run("photo.rate".into(), "[1,2]".into()), Err(EmberError::InvalidRequest { .. })));
        assert!(matches!(session.run("photo.rate".into(), "{not json".into()), Err(EmberError::InvalidRequest { .. })));
        assert!(matches!(session.preview("{}".into()), Err(EmberError::InvalidRequest { .. })));
        assert!(session.preview_bytes("../../etc/passwd".into()).is_none());
        assert!(session.run("no.such.command".into(), "{}".into()).is_err());
        assert!(EmberSession::open(SessionOptions { library_path: Some("a\0b".into()), demo: false, demo_count: 0 }).is_err());
        session.close().expect("close");
    }

    #[test]
    fn demo_session_slices_rates_edits_and_renders() {
        let session = demo();
        let snapshot: Value = serde_json::from_str(&session.snapshot().expect("snapshot")).expect("json");
        assert_eq!(snapshot["total"].as_u64(), Some(3));
        let generation = snapshot["viewGeneration"].as_u64().expect("generation");
        let slice: Value = serde_json::from_str(&session.view_slice(Some(generation), 0, 10).expect("slice")).expect("json");
        let id = slice["photos"][0]["id"].as_u64().expect("photo id");

        session.run("library.select".into(), format!(r#"{{"ids":[{id}],"active":{id}}}"#)).expect("select");
        session.run("photo.rate".into(), format!(r#"{{"rating":4,"ids":[{id}]}}"#)).expect("rate");
        session.run("develop.set".into(), r#"{"control":"light.exposure","value":0.5}"#.into()).expect("develop");

        // Edits can change the view (a rating filter, sort order), so previews use the latest generation.
        let generation =
            serde_json::from_str::<Value>(&session.snapshot().expect("snapshot")).expect("json")["viewGeneration"].as_u64().expect("generation");
        let request = format!(
            r#"{{"photoId":{id},"slot":"main","viewGeneration":{generation},"width":320,"height":240,"quality":"draft","before":false,"sequence":1}}"#
        );
        let descriptor: Value = serde_json::from_str(&session.preview(request).expect("preview")).expect("json");
        let handle = descriptor["handle"].as_str().expect("handle").to_string();
        let image = session.preview_bytes(handle.clone()).expect("bytes for live handle");
        assert!(!image.bytes.is_empty());
        assert!(image.content_type.starts_with("image/"));
        session.release_preview(handle);

        session.close().expect("close");
        assert!(matches!(session.snapshot(), Err(EmberError::Closed)));
    }

    /// The app's slider sequence (select, begin, set, draft, end, full) must change the rendered pixels.
    #[test]
    fn slider_gesture_changes_the_full_render() {
        let session = demo();
        let snapshot: Value = serde_json::from_str(&session.snapshot().expect("snapshot")).expect("json");
        let generation = snapshot["viewGeneration"].as_u64().expect("generation");
        let slice: Value = serde_json::from_str(&session.view_slice(Some(generation), 0, 1).expect("slice")).expect("json");
        let id = slice["photos"][0]["id"].as_u64().expect("photo id");
        session.run("library.select".into(), format!(r#"{{"ids":[{id}],"active":{id}}}"#)).expect("select");
        let render = |quality: &str, sequence: u64| {
            let generation =
                serde_json::from_str::<Value>(&session.snapshot().expect("snapshot")).expect("json")["viewGeneration"].as_u64().expect("generation");
            let request = format!(
                r#"{{"photoId":{id},"slot":"main","viewGeneration":{generation},"width":400,"height":300,"quality":"{quality}","before":false,"sequence":{sequence}}}"#
            );
            let descriptor: Value = serde_json::from_str(&session.preview(request).expect("preview")).expect("json");
            session.preview_bytes(descriptor["handle"].as_str().expect("handle").into()).expect("bytes").bytes
        };
        let before = render("full", 1);
        session.run("develop.beginInteraction".into(), r#"{"label":"Exposure"}"#.into()).expect("begin");
        session.run("develop.set".into(), r#"{"control":"light.exposure","value":1.5}"#.into()).expect("set");
        let draft = render("draft", 2);
        session.run("develop.endInteraction".into(), "{}".into()).expect("end");
        let after = render("full", 3);
        let (before, draft, after) = (mean_luma(&before), mean_luma(&draft), mean_luma(&after));
        assert!(draft > before + 10.0, "draft render shows +1.5 EV: before {before:.1}, draft {draft:.1}");
        assert!(after > before + 10.0, "full render after the gesture shows +1.5 EV: before {before:.1}, after {after:.1}");
        session.close().expect("close");
    }

    fn mean_luma(png_bytes: &[u8]) -> f64 {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes)).read_info().expect("png header");
        let mut pixels = vec![0; reader.output_buffer_size().expect("buffer size")];
        let info = reader.next_frame(&mut pixels).expect("png frame");
        let channels = info.color_type.samples();
        let pixels = &pixels[..info.buffer_size()];
        let sum: f64 = pixels.chunks(channels).map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])).sum();
        sum / (3.0 * (pixels.len() / channels) as f64)
    }

    #[test]
    fn bridge_version_is_exported() {
        assert_eq!(bridge_version(), BRIDGE_VERSION);
    }
}
