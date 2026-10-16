//! Photo > Capture Frame (issue #113): one frame of a clip, at full size, saved as a JPEG next to the clip, imported and stacked
//! with it (as Render to DNG does for a picture), with the clip's keywords and rating and its capture time moved to the frame.
//! The whole thing is one undo step. The frame comes from the verified ffmpeg sidecar (`lightcraft_video`, docs/video.md); the
//! decoder is a parameter, so the tests run without it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_catalog::{Op, PhotoId, Source};
use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta};
use lightcraft_raster::Rgba8;
use serde_json::{Value, json};

use crate::activity::Cancel;
use crate::cmd::bad;
use crate::{Result, Session};

/// Decodes one frame: the clip, the time in seconds, the longest edge in pixels, and the cancel flag of the task.
pub(crate) type Decode<'a> = &'a dyn Fn(&Path, f64, usize, Option<Arc<AtomicBool>>) -> std::result::Result<Rgba8, String>;

/// The frame through the ffmpeg sidecar. Without it the error is "video support not installed" (or "not supported").
pub(crate) fn decode_sidecar(path: &Path, t: f64, max_edge: usize, cancel: Option<Arc<AtomicBool>>) -> std::result::Result<Rgba8, String> {
    let limits = lightcraft_video::Limits { cancel, ..Default::default() };
    lightcraft_video::frame_at_with(path, t, max_edge, &limits).map_err(|e| e.to_string())
}

/// The JPEG's file name for a frame at `t` seconds of the clip called `clip_name`: `<clip stem> frame <t>s.jpg`, with
/// ` (2)`, ` (3)`… before the extension while `exists` says a file of that name is there.
pub(crate) fn frame_file_name(clip_name: &str, t: f64, exists: &dyn Fn(&str) -> bool) -> String {
    let stem = Path::new(clip_name).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("clip");
    let base = format!("{stem} frame {t:.3}s");
    let mut name = format!("{base}.jpg");
    let mut n = 2;
    while exists(&name) {
        name = format!("{base} ({n}).jpg");
        n += 1;
    }
    name
}

/// The capture time of a frame `t` seconds into a clip captured at `captured` (ISO 8601 local time, as the catalog keeps it):
/// the whole second the frame is shown in. `None` when the time cannot be moved.
pub(crate) fn captured_at_frame(captured: &str, t: f64) -> Option<String> {
    lightcraft_catalog::dates::shift_iso(captured, t.floor() as i64)
}

/// Capture the frame of video `id` at `t` seconds (already held inside the clip). The new JPEG is written durably next to the
/// clip, imported, stacked with the clip (the frame on top, as Render to DNG stacks its copy), and given the clip's keywords and
/// rating and the frame's capture time. Those steps are folded into one undo step, "Capture Frame".
pub(crate) fn capture_frame(s: &mut Session, id: PhotoId, t: f64, decode: Decode<'_>) -> Result<Value> {
    const C: &str = "video.captureFrame";
    let clip = s.catalog.photo(id).cloned().ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
    let Source::File { path } = &clip.source else {
        return Err(bad(C, "a generated demo video has no file to take a frame from"));
    };
    let clip_path = Path::new(path);
    let dir = clip_path.parent().unwrap_or(Path::new(""));
    // full size: the clip's own longest edge (the sidecar caps it)
    let max_edge = clip.width.max(clip.height).max(1) as usize;

    let task = s.activity.start("capture", "Capturing Frame", Cancel::Yes);
    let frame = decode(clip_path, t, max_edge, task.cancel_flag()).map_err(|e| bad(C, e))?;
    let jpeg = lightcraft_codecs::encode_jpeg(&EncodeImage::rgba8(&frame), 95, ChromaSubsampling::S444, &EncodeMeta::default())
        .map_err(|e| bad(C, format!("the frame could not be encoded: {e}")))?;
    drop(task);

    let name = frame_file_name(&clip.file_name, t, &|n| dir.join(n).exists());
    let out = dir.join(&name).to_string_lossy().to_string();
    crate::export::write_file_durable(&out, &jpeg).map_err(|e| bad(C, e))?;

    let before = s.commits();
    let imported = match s.execute("library.import", &json!({"paths": [&out]})) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            return Err(e);
        }
    };
    let Some(new) = imported["imported"].get(0).and_then(Value::as_u64).map(PhotoId) else {
        let _ = std::fs::remove_file(&out);
        return Err(bad(C, "the frame could not be added to the library"));
    };
    let Some(frame_photo) = s.catalog.photo(new).cloned() else {
        return Err(bad(C, "the frame could not be added to the library"));
    };

    // the clip's keywords, rating and capture time (moved to the frame)
    let mut ops = Vec::new();
    let captured = clip.captured.as_deref().and_then(|c| captured_at_frame(c, t));
    if captured.is_some() && captured != frame_photo.captured {
        ops.push(Op::SetCaptured { id: new, captured });
    }
    if clip.rating != frame_photo.rating {
        ops.push(Op::SetRating { id: new, rating: clip.rating });
    }
    let mut meta = frame_photo.meta.clone();
    meta.keywords = clip.meta.keywords.clone();
    if meta != frame_photo.meta {
        ops.push(Op::SetMeta { id: new, meta: Box::new(meta) });
    }
    let copied = if ops.is_empty() { Ok(()) } else { s.commit("Capture Frame", Op::Batch { ops }) };

    let stacked = s.execute("stack.group", &json!({"ids": [new.0, id.0], "top": new.0, "collapsed": false}));
    // fold the import, the copy and the stack into one step (merge_undo leaves a step alone that has folder or file parts)
    let steps = usize::try_from(s.commits().wrapping_sub(before)).unwrap_or(0);
    s.merge_undo(steps, "Capture Frame");
    copied?;
    stacked?;
    Ok(json!({"path": out, "id": new.0, "clip": id.0, "t": t}))
}
