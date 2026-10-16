//! Video through the verified LGPL ffmpeg sidecar (L1): a safe API to probe a clip ([`probe`]) and to cut one frame out of it
//! ([`frame_at`]), the poster frame of a video in the library.
//!
//! Ember decodes no video itself. It runs the `ffmpeg` and `ffprobe` binaries that release bundles ship beside the application,
//! through `rightkit-ffmpeg` (RightKit's owner of the approved-binary catalog and of the process runner): a binary runs only
//! when its content digest matches the approved catalog row and its `-version` banner shows an LGPL build. The binaries are not
//! in this repository (docs/video.md, NOTICE). Where they are not bundled (a development checkout, CI), and on Linux and the
//! web, which ship without video for now, every call returns an error that says so ([`VideoError::is_unavailable`]) and the
//! caller falls back to a placeholder.
//!
//! The media is hostile input. Every child process runs under a deadline and a cancel flag ([`Limits`]) and is killed with its
//! whole process tree when either fires; the input is read through the `file` protocol only (a crafted playlist can name no URL
//! and no other file); the frame is asked for at an exact size, so the output is bounded by [`MAX_FRAME_EDGE`] whatever the
//! clip says; and nothing here panics: every failure is a [`VideoError`].
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lightcraft_raster::Rgba8;
use serde_json::Value;

mod sidecar;

/// File extensions (lower case) Ember treats as video. The engine's import list contains every one of them.
pub const EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "mts", "m2ts"];

/// Whether this build can play or read video at all. `false` on Linux and on the web: calls return [`VideoError::Unsupported`].
pub const SUPPORTED: bool = cfg!(any(target_os = "windows", target_os = "macos"));

/// The largest frame edge [`frame_at`] returns (a bigger `max_size` is cut down to it): 4096 x 4096 RGBA is 64 MiB.
pub const MAX_FRAME_EDGE: usize = 4096;

/// Where a clip's poster frame is when its duration is unknown (seconds).
pub const FALLBACK_POSTER_SECS: f64 = 1.0;
/// Where a clip's poster frame is by default, as a share of its duration.
pub const POSTER_FRACTION: f64 = 0.1;
/// A poster is never closer to the end of the clip than this (seconds): a seek to the very end can find no frame.
const END_MARGIN_SECS: f64 = 0.05;

/// A seek past this many seconds (a hostile number) is held to it.
const MAX_SEEK_SECS: f64 = 1.0e7;

/// The coded width or height of a stream is refused above this (a hostile header could say anything).
const MAX_DIMENSION: u32 = 1 << 16;
/// `ffprobe`'s JSON is refused above this size.
pub(crate) const MAX_PROBE_JSON: usize = 4 << 20;

/// Whether `path` names a video by its extension.
pub fn is_video_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
}

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    /// This build has no video support (Linux, the web).
    #[error("video is not supported on this platform")]
    Unsupported,
    /// The verified ffmpeg sidecar is not bundled (a development checkout, CI).
    #[error("video support not installed (the verified ffmpeg sidecar was not found)")]
    NotInstalled,
    /// A sidecar was found but is not an approved build, or could not be checked.
    #[error("the ffmpeg sidecar was rejected: {0}")]
    Rejected(String),
    /// The file could not be probed.
    #[error("{0}")]
    Probe(String),
    #[error("no video stream in the file")]
    NoVideoStream,
    /// ffmpeg ran and wrote no frame (the clip has none).
    #[error("no frame in the video")]
    NoFrame,
    #[error("ffmpeg timed out after {0:?}")]
    TimedOut(Duration),
    #[error("cancelled")]
    Cancelled,
    /// ffmpeg failed; the end of its error output.
    #[error("ffmpeg failed: {0}")]
    Failed(String),
    #[error("{0}")]
    Invalid(String),
}

impl VideoError {
    /// Video cannot work here at all (no support, no sidecar, a rejected sidecar), as opposed to this one file being unreadable:
    /// callers show a placeholder instead of an error.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, VideoError::Unsupported | VideoError::NotInstalled | VideoError::Rejected(_))
    }
}

/// How long a call may run and how to stop it.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Each child process (the probe, the frame) is killed after this long.
    pub timeout: Duration,
    /// Set it to stop the running child process at once.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { timeout: Duration::from_secs(30), cancel: None }
    }
}

/// What `ffprobe` says about a clip's first video stream.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoInfo {
    /// Seconds: the container's, else the stream's.
    pub duration: Option<f64>,
    /// Coded size, before the rotation.
    pub width: u32,
    pub height: u32,
    /// Display rotation in degrees: 0, 90, 180 or 270.
    pub rotation: i32,
    pub codec: String,
    /// Frames per second (the average rate, else the nominal one).
    pub frame_rate: Option<f64>,
    /// As the container writes it (`2026-10-04T08:16:11.000000Z`).
    pub creation_time: Option<String>,
}

impl VideoInfo {
    /// The size as shown: a 90 or 270 degree rotation swaps width and height. [`frame_at`] returns frames of this shape.
    pub fn display_size(&self) -> (u32, u32) {
        if matches!(self.rotation, 90 | 270) { (self.height, self.width) } else { (self.width, self.height) }
    }
}

/// Probe `path` with the default [`Limits`].
pub fn probe(path: &Path) -> Result<VideoInfo, VideoError> {
    probe_with(path, &Limits::default())
}

pub fn probe_with(path: &Path, limits: &Limits) -> Result<VideoInfo, VideoError> {
    let json = sidecar::ffprobe_json(path, limits)?;
    parse_probe_json(&json)
}

/// The frame of `path` at `t` seconds, upright and no larger than `max_size` along its long edge (never enlarged; at most
/// [`MAX_FRAME_EDGE`]), with the default [`Limits`]. A `t` past the end of the clip gives its first frame.
pub fn frame_at(path: &Path, t: f64, max_size: usize) -> Result<Rgba8, VideoError> {
    frame_at_with(path, t, max_size, &Limits::default())
}

pub fn frame_at_with(path: &Path, t: f64, max_size: usize, limits: &Limits) -> Result<Rgba8, VideoError> {
    let shown = probe_with(path, limits)?.display_size();
    frame_at_sized(path, t, shown, max_size, limits)
}

/// [`frame_at_with`] for a caller that already knows the clip's displayed size (what [`VideoInfo::display_size`] said when it
/// probed the file): one child process instead of two. A wrong size gives a stretched frame, never an error.
pub fn frame_at_sized(path: &Path, t: f64, shown: (u32, u32), max_size: usize, limits: &Limits) -> Result<Rgba8, VideoError> {
    let (w, h) = fit_frame(shown.0, shown.1, max_size);
    let t = if t.is_finite() && t >= 0.0 { t.min(MAX_SEEK_SECS) } else { 0.0 };
    let bytes = match sidecar::frame_rgba(path, t, w, h, limits)? {
        Some(bytes) => bytes,
        None if t > 0.0 => sidecar::frame_rgba(path, 0.0, w, h, limits)?.ok_or(VideoError::NoFrame)?,
        None => return Err(VideoError::NoFrame),
    };
    Rgba8::from_bytes(w as usize, h as usize, &bytes).ok_or_else(|| VideoError::Failed("the frame is not the size asked for".into()))
}

/// Whether the verified sidecar can be used here: `Ok` once it is found and checked, else why not
/// ([`VideoError::Unsupported`], [`VideoError::NotInstalled`], [`VideoError::Rejected`]).
pub fn available() -> Result<(), VideoError> {
    sidecar::available()
}

/// The size [`frame_at`] returns for a clip shown at `width` x `height`: fitted into `max_edge` (at most [`MAX_FRAME_EDGE`]),
/// keeping the shape, never enlarged, and at least one pixel each way.
pub fn fit_frame(width: u32, height: u32, max_edge: usize) -> (u32, u32) {
    let max = max_edge.clamp(1, MAX_FRAME_EDGE) as u64;
    let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
    let long = w.max(h);
    if long <= max {
        return (width.max(1), height.max(1));
    }
    let scale = |x: u64| u32::try_from(((x * max + long / 2) / long).max(1)).unwrap_or(u32::MAX);
    (scale(w), scale(h))
}

/// Where the poster frame of a clip of `duration` seconds is when nobody chose: 10 % into the clip, or 1 s when the duration is
/// not known.
pub fn default_poster_secs(duration: Option<f64>) -> f64 {
    match positive(duration) {
        Some(d) => d * POSTER_FRACTION,
        None => FALLBACK_POSTER_SECS,
    }
}

/// The time to cut a poster at: the one chosen (`set`, seconds) or [`default_poster_secs`], kept inside the clip.
pub fn poster_secs(set: Option<f64>, duration: Option<f64>) -> f64 {
    let t = set.filter(|t| t.is_finite() && *t >= 0.0).unwrap_or_else(|| default_poster_secs(duration));
    match positive(duration) {
        Some(d) => t.min((d - END_MARGIN_SECS).max(0.0)),
        None => t,
    }
}

fn positive(v: Option<f64>) -> Option<f64> {
    v.filter(|d| d.is_finite() && *d > 0.0)
}

// ---------- ffprobe JSON

/// A JSON number, or a number ffprobe printed as a string.
fn number(v: Option<&Value>) -> Option<f64> {
    let n = match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    n.filter(|x| x.is_finite())
}

fn dimension(v: Option<&Value>) -> Option<u32> {
    let x = number(v)?;
    (1.0..=f64::from(MAX_DIMENSION)).contains(&x).then_some(x as u32)
}

/// `30000/1001` or `25` as frames per second; `0/0` (unknown) and anything not positive is `None`.
fn frame_rate_of(text: &str) -> Option<f64> {
    let rate = match text.trim().split_once('/') {
        Some((n, d)) => {
            let (n, d) = (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?);
            if d == 0.0 {
                return None;
            }
            n / d
        }
        None => text.trim().parse::<f64>().ok()?,
    };
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

/// Display rotation of a stream, normalised to 0, 90, 180 or 270: the display matrix of newer files, else the `rotate` tag of
/// older ones.
fn rotation_of(stream: &Value) -> i32 {
    let from_matrix = stream.get("side_data_list").and_then(Value::as_array).and_then(|list| list.iter().find_map(|e| number(e.get("rotation"))));
    let degrees = from_matrix.or_else(|| number(stream.get("tags").and_then(|t| t.get("rotate")))).unwrap_or(0.0);
    let quarter_turns = (degrees / 90.0).round() as i64;
    (quarter_turns.rem_euclid(4) * 90) as i32
}

/// A tag, whatever the case of its key.
fn tag(container: &Value, key: &str) -> Option<String> {
    let tags = container.get("tags")?.as_object()?;
    tags.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).and_then(|(_, v)| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Read the JSON `ffprobe -print_format json -show_format -show_streams` prints: its first video stream that is not cover art.
pub fn parse_probe_json(json: &str) -> Result<VideoInfo, VideoError> {
    if json.len() > MAX_PROBE_JSON {
        return Err(VideoError::Probe("the probe output is too large".into()));
    }
    let root: Value = serde_json::from_str(json).map_err(|e| VideoError::Probe(format!("ffprobe printed invalid JSON: {e}")))?;
    let streams = root.get("streams").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let stream = streams
        .iter()
        .find(|s| {
            let is_video = s.get("codec_type").and_then(Value::as_str) == Some("video");
            let cover_art = number(s.get("disposition").and_then(|d| d.get("attached_pic"))).unwrap_or(0.0) != 0.0;
            is_video && !cover_art
        })
        .ok_or(VideoError::NoVideoStream)?;
    let width = dimension(stream.get("width")).ok_or_else(|| VideoError::Probe("the video stream has no usable width".into()))?;
    let height = dimension(stream.get("height")).ok_or_else(|| VideoError::Probe("the video stream has no usable height".into()))?;
    let format = root.get("format").unwrap_or(&Value::Null);
    let duration = positive(number(format.get("duration"))).or_else(|| positive(number(stream.get("duration"))));
    let frame_rate = ["avg_frame_rate", "r_frame_rate"].iter().find_map(|k| stream.get(*k).and_then(Value::as_str).and_then(frame_rate_of));
    Ok(VideoInfo {
        duration,
        width,
        height,
        rotation: rotation_of(stream),
        codec: stream.get("codec_name").and_then(Value::as_str).unwrap_or_default().to_string(),
        frame_rate,
        creation_time: tag(format, "creation_time").or_else(|| tag(stream, "creation_time")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE_CLIP: &str = r#"{
        "streams": [
            {"index": 0, "codec_name": "hevc", "codec_type": "video", "width": 1920, "height": 1080,
             "r_frame_rate": "30000/1001", "avg_frame_rate": "30000/1001", "duration": "12.512000",
             "disposition": {"default": 1, "attached_pic": 0},
             "tags": {"creation_time": "2026-10-04T08:16:11.000000Z"},
             "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}]},
            {"index": 1, "codec_name": "aac", "codec_type": "audio", "sample_rate": "48000", "channels": 2}
        ],
        "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "12.545000", "tags": {"creation_time": "2026-10-04T08:16:11.000000Z"}}
    }"#;

    #[test]
    fn probe_json_gives_size_rotation_codec_rate_duration_and_time() {
        let info = parse_probe_json(PHONE_CLIP).unwrap();
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.rotation, 270, "-90 degrees is three quarter turns");
        assert_eq!(info.display_size(), (1080, 1920), "a quarter turn swaps the sides");
        assert_eq!(info.codec, "hevc");
        assert!((info.frame_rate.unwrap() - 29.97).abs() < 0.001);
        assert_eq!(info.duration, Some(12.545), "the container's duration wins over the stream's");
        assert_eq!(info.creation_time.as_deref(), Some("2026-10-04T08:16:11.000000Z"));
    }

    #[test]
    fn probe_json_skips_cover_art_and_audio_and_falls_back_on_the_stream() {
        let json = r#"{"streams": [
            {"codec_type": "video", "codec_name": "mjpeg", "width": 600, "height": 600, "disposition": {"attached_pic": 1}},
            {"codec_type": "audio", "codec_name": "aac"},
            {"codec_type": "video", "codec_name": "h264", "width": "1280", "height": "720", "avg_frame_rate": "0/0", "r_frame_rate": "25/1",
             "duration": "3.5", "tags": {"rotate": "90", "CREATION_TIME": "2020-01-02T03:04:05Z"}}
        ], "format": {}}"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!((info.codec.as_str(), info.width, info.height), ("h264", 1280, 720));
        assert_eq!(info.frame_rate, Some(25.0), "an unknown average rate falls back to the nominal one");
        assert_eq!(info.duration, Some(3.5));
        assert_eq!(info.rotation, 90, "the old rotate tag still counts");
        assert_eq!(info.creation_time.as_deref(), Some("2020-01-02T03:04:05Z"), "tag keys are matched in any case");
    }

    #[test]
    fn probe_json_refuses_what_is_not_a_video_without_panicking() {
        assert!(matches!(parse_probe_json(r#"{"streams": [{"codec_type": "audio"}]}"#), Err(VideoError::NoVideoStream)));
        assert!(matches!(parse_probe_json("{}"), Err(VideoError::NoVideoStream)));
        assert!(matches!(parse_probe_json("not json"), Err(VideoError::Probe(_))));
        assert!(matches!(parse_probe_json(r#"{"streams": [{"codec_type": "video", "width": 0, "height": 10}]}"#), Err(VideoError::Probe(_))));
        let huge = r#"{"streams": [{"codec_type": "video", "width": 4000000000, "height": 10}]}"#;
        assert!(matches!(parse_probe_json(huge), Err(VideoError::Probe(_))), "a hostile size is refused");
        let negative = r#"{"streams": [{"codec_type": "video", "width": -5, "height": 10, "duration": "-3", "avg_frame_rate": "5/0"}]}"#;
        assert!(parse_probe_json(negative).is_err());
        let odd = r#"{"streams": [{"codec_type": "video", "width": 10, "height": 10, "duration": "-3",
            "avg_frame_rate": "5/0", "r_frame_rate": "x/y"}]}"#;
        let info = parse_probe_json(odd).unwrap();
        assert_eq!((info.duration, info.frame_rate, info.rotation), (None, None, 0));
        assert!(matches!(parse_probe_json(&" ".repeat(MAX_PROBE_JSON + 1)), Err(VideoError::Probe(_))));
    }

    #[test]
    fn the_poster_is_a_tenth_into_the_clip_or_a_second_without_a_duration() {
        assert_eq!(default_poster_secs(Some(60.0)), 6.0);
        assert_eq!(default_poster_secs(None), FALLBACK_POSTER_SECS);
        assert_eq!(default_poster_secs(Some(0.0)), FALLBACK_POSTER_SECS, "a zero duration is no duration");
        assert_eq!(default_poster_secs(Some(f64::NAN)), FALLBACK_POSTER_SECS);
        assert_eq!(poster_secs(None, Some(20.0)), 2.0);
        assert_eq!(poster_secs(Some(7.5), Some(20.0)), 7.5, "a chosen time wins");
        assert!(poster_secs(Some(99.0), Some(20.0)) < 20.0, "a chosen time past the end is held inside the clip");
        assert_eq!(poster_secs(Some(-1.0), None), FALLBACK_POSTER_SECS, "a negative time is not a time");
        assert_eq!(poster_secs(Some(f64::INFINITY), None), FALLBACK_POSTER_SECS);
        assert_eq!(poster_secs(None, Some(0.01)), 0.0, "a clip shorter than the end margin is cut at its start");
    }

    #[test]
    fn frames_fit_their_box_keep_their_shape_and_are_not_enlarged() {
        assert_eq!(fit_frame(1920, 1080, 512), (512, 288));
        assert_eq!(fit_frame(1080, 1920, 512), (288, 512));
        assert_eq!(fit_frame(320, 240, 512), (320, 240));
        assert_eq!(fit_frame(10_000, 10, 64), (64, 1), "a thin strip keeps a pixel");
        assert_eq!(fit_frame(0, 0, 0), (1, 1));
        let (w, h) = fit_frame(65_536, 65_536, usize::MAX);
        assert_eq!((w as usize, h as usize), (MAX_FRAME_EDGE, MAX_FRAME_EDGE), "the output is capped");
    }

    #[test]
    fn video_extensions_are_recognised_in_any_case() {
        assert!(is_video_path(Path::new("/a/b/CLIP.MOV")));
        assert!(is_video_path(Path::new("c.mp4")));
        assert!(!is_video_path(Path::new("c.jpg")));
        assert!(!is_video_path(Path::new("mp4")));
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn a_missing_sidecar_is_reported_as_not_installed() {
        let nowhere = std::env::temp_dir().join("ember-video-test-no-such-ffmpeg-directory");
        let err = sidecar::resolve_in(&[nowhere]).err();
        assert!(matches!(err, Some(VideoError::NotInstalled)), "{err:?}");
        assert!(VideoError::NotInstalled.is_unavailable());
        assert!(VideoError::NotInstalled.to_string().contains("not installed"));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn video_is_unsupported_off_windows_and_macos() {
        assert!(!SUPPORTED);
        let err = probe(Path::new("clip.mp4")).err();
        assert!(matches!(err, Some(VideoError::Unsupported)), "{err:?}");
        assert!(matches!(frame_at(Path::new("clip.mp4"), 1.0, 256).err(), Some(VideoError::Unsupported)));
    }
}
