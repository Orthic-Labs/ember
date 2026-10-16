//! Video in the library: the probe at import and the poster frame that stands for a clip in the grid, the loupe and exports
//! (issues #73 and #76). The reading is `lightcraft-video`'s (the verified LGPL ffmpeg sidecar, docs/video.md); this is the
//! engine's side of it: probe facts into a [`ProbeInfo`], a poster frame into a working-space source, and a placeholder
//! where the sidecar is not there (a development checkout, CI, Linux, the web), so a video is still catalogued and shown.
//!
//! A video's bytes are never read into memory here: the probe asks `ffprobe`, the frame comes from `ffmpeg`, and neither
//! the content hash nor the decoders of still images see the file.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::UNIX_EPOCH;

use lightcraft_catalog::{MediaKind, Photo, VideoMeta};
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::media::{FileLoader, ProbeInfo};

/// The size a clip is catalogued at when it could not be probed (no sidecar): 16:9, like the placeholder picture.
pub const PLACEHOLDER_SIZE: (u32, u32) = (1280, 720);

/// Whether `path` names a video by its extension.
pub fn is_video(path: &str) -> bool {
    lightcraft_video::is_video_path(Path::new(path))
}

/// A container's creation time (`2026-10-04T08:16:11.000000Z`, or with a space for the `T`) as the ISO 8601 capture time the
/// catalog keeps (`2026-10-04T08:16:11`). Containers state UTC and say nothing of where the clip was shot, so the zone is dropped
/// (the catalog's capture times are local and zoneless).
pub fn captured_from(creation_time: &str) -> Option<String> {
    let head = creation_time.trim().get(..19)?;
    let shape = b"dddd-dd-dd?dd:dd:dd";
    let fits = head.bytes().zip(shape.iter().copied()).all(|(c, s)| match s {
        b'd' => c.is_ascii_digit(),
        b'?' => c == b'T' || c == b' ',
        s => c == s,
    });
    if !fits {
        return None;
    }
    Some(format!("{}T{}", head.get(..10)?, head.get(11..)?))
}

/// What the probe of `path` found, as an import record: the facts of a [`ProbeInfo`] on top of `base`. The size is the size as
/// shown (rotation applied). No content hash: a video is too big to read for one, so duplicates are found by path only.
pub fn info_from(base: ProbeInfo, v: &lightcraft_video::VideoInfo) -> ProbeInfo {
    let (width, height) = v.display_size();
    ProbeInfo {
        width,
        height,
        kind: MediaKind::Video,
        captured: v.creation_time.as_deref().and_then(captured_from),
        duration: v.duration,
        video: Some(VideoMeta { codec: v.codec.clone(), frame_rate: v.frame_rate, rotation: v.rotation }),
        ..base
    }
}

/// Probe a video file for import: `ffprobe`'s facts when the sidecar is there; when it is not, the clip is still catalogued, at
/// [`PLACEHOLDER_SIZE`] with no duration, and its thumbnail is the placeholder picture. A file that is not a readable video (the
/// sidecar is there and says so) is an error, like any unreadable file.
pub fn probe_file(path: &str) -> Result<ProbeInfo, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a file"));
    }
    let stamp = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos());
    let base = ProbeInfo {
        width: PLACEHOLDER_SIZE.0,
        height: PLACEHOLDER_SIZE.1,
        format: Path::new(path).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
        kind: MediaKind::Video,
        file_size: meta.len(),
        // `Some(0)`: a filesystem without a timestamp is never reused from the review cache (as for the image probe)
        source_stamp: Some(stamp.unwrap_or(0)),
        file_modified: crate::files::file_modified(path),
        ..Default::default()
    };
    match lightcraft_video::probe(Path::new(path)) {
        Ok(v) => Ok(info_from(base, &v)),
        Err(e) if e.is_unavailable() => Ok(base),
        Err(e) => Err(format!("{path}: {e}")),
    }
}

/// The time (seconds) photo `p`'s poster frame is cut at: the one chosen with `video.setPoster`, else 10 % into the clip (1 s
/// when its duration is unknown), always inside the clip.
pub fn poster_secs_of(p: &Photo) -> f64 {
    lightcraft_video::poster_secs(p.develop.poster_time, p.duration)
}

/// The picture of a video that could not be read: dark, 16:9, with a play mark, `max_edge` pixels along its long edge (16 to 1280).
pub fn placeholder(max_edge: usize) -> Rgb32f {
    let w = max_edge.clamp(16, PLACEHOLDER_SIZE.0 as usize);
    let h = (w * 9 / 16).max(9);
    // the mark: a triangle pointing right, centred, a fifth of the height tall
    let unit = h as f32 * 0.1;
    let (cx, cy) = (w as f32 / 2.0 - unit / 2.0, h as f32 / 2.0);
    Rgb32f::from_fn(w, h, |x, y| {
        let (u, v) = ((x as f32 + 0.5 - cx) / unit, (y as f32 + 0.5 - cy) / unit);
        let inside = (0.0..=1.0).contains(&u) && v.abs() <= 1.0 - u;
        let level = if inside { 0.40 } else { 0.03 };
        [level, level, level]
    })
}

/// An sRGB frame in the working space (linear Rec.2020), where every source is.
fn to_working(frame: &Rgba8) -> Rgb32f {
    let m = lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING).to_f32();
    frame.to_linear().map(|p| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2]) })
}

/// Load the poster frame of the clip at `path` for a source no larger than `max_edge`: the frame at `poster` (else the default
/// time for a clip of `duration`) when the sidecar gives one, the placeholder when it is not there. `shown` is the size the
/// clip was catalogued at, when it was probed (it saves a process); `cancel` stops the child process.
///
/// Nothing is read into memory but the frame; the memory gate counts what the frame becomes (RGBA bytes, then linear floats).
pub fn load_poster(
    path: &str,
    duration: Option<f64>,
    poster: Option<f64>,
    shown: Option<(u32, u32)>,
    max_edge: usize,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<(Rgb32f, SourceInfo), String> {
    let edge = max_edge.clamp(16, lightcraft_video::MAX_FRAME_EDGE);
    let gate = crate::memory::work_gate();
    let _permit = if crate::memory::is_background() { gate.acquire(edge * edge * 16) } else { gate.acquire_urgent(edge * edge * 16) };
    let t = lightcraft_video::poster_secs(poster, duration);
    let limits = lightcraft_video::Limits { cancel, ..Default::default() };
    let frame = match shown {
        Some(shown) => lightcraft_video::frame_at_sized(Path::new(path), t, shown, edge, &limits),
        None => lightcraft_video::frame_at_with(Path::new(path), t, edge, &limits),
    };
    let r = match frame {
        Ok(frame) => Ok((to_working(&frame), SourceInfo::default())),
        Err(e) if e.is_unavailable() => Ok((placeholder(edge), SourceInfo::default())),
        Err(e) => Err(format!("{path}: {e}")),
    };
    crate::memory::release();
    r
}

/// The loader of photo `p`'s poster frame (a [`FileLoader`] for its source): it knows the chosen poster time and the clip's
/// duration and size, which the file loaders of still images are not told. Anything bigger than a thumbnail shows in the
/// activity stack, with a ✕ that stops the child process.
pub fn poster_loader(p: &Photo, activity: Option<crate::activity::Activity>) -> FileLoader {
    let (duration, poster) = (p.duration, p.develop.poster_time);
    let shown = (p.video.is_some() && p.width > 0 && p.height > 0).then_some((p.width, p.height));
    Arc::new(move |path: &str, max_edge: usize| {
        let task = activity
            .as_ref()
            .filter(|_| max_edge > crate::media::SourceLevel::Thumb.max_edge())
            .map(|a| a.start("video", "Reading Video Frame", crate::activity::Cancel::Yes));
        let cancel = task.as_ref().and_then(|t| t.cancel_flag());
        load_poster(path, duration, poster, shown, max_edge, cancel)
    })
}
