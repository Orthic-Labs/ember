//! Video in an export (issue #77): a clip written as a file (`format: "video"`), and one frame of a clip written as a still
//! (`frameTime`, through the ordinary still pipeline).
//!
//! A clip is never read into memory: ffmpeg (the verified sidecar, `lightcraft_video::clip`) cuts it to its trim into a temporary
//! file ([`StagedFile`]), which the batch then moves to the place [`super::Placer`] chose, through the same atomic write as every
//! other export (a temp file next to the target, renamed over it), so a failure part-way leaves any earlier file as it was and
//! nothing half-written. The run holds a row in the activity stack whose ✕ kills the child process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lightcraft_catalog::{MediaKind, Photo, PhotoId, Source};
use lightcraft_video::Trim;
use lightcraft_video::clip::{ClipKind, ClipRequest, export_clip, export_limits, output_extension, plan_clip};
use serde_json::{Value, json};

use super::{ExportOptions, Exported, MetadataPolicy, PreparedExport, Work, removes_location};
use crate::activity::{Activity, Cancel, Unit};

/// The working memory a clip export asks the export gate for: the child process holds the pictures, not Ember.
const CLIP_WEIGHT: usize = 32 << 20;

/// A finished export file waiting in a temporary place, too big to hold in memory: [`super::run_batch`] moves it to its final path
/// ([`Self::place`]). Dropping it removes the temporary file, so a clip whose placing failed (or was skipped) leaves nothing behind.
pub struct StagedFile {
    path: PathBuf,
    /// Its size in bytes.
    pub size: u64,
    /// What was done, for the export's report (`video` in the entry of the file): `{mode, accurate, start, length, ...}`.
    pub detail: Value,
}

impl StagedFile {
    /// Replace (or create) `dest` with this file: its folder is made if needed, the content is written to a temp file next to `dest`
    /// and renamed into place ([`lightcraft_catalog::safe_file::write_atomic_with`]).
    pub(super) fn place(&self, dest: &Path) -> Result<(), String> {
        if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut src = std::fs::File::open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        lightcraft_catalog::safe_file::write_atomic_with(dest, &mut |w| std::io::copy(&mut src, w).map(|_| ()))
            .map_err(|e| format!("{}: {e}", dest.display()))?;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What a clip export does, set up from the session ([`prepare`]) so it can run without it.
pub(super) struct ClipWork {
    /// The clip's original (a catalog path).
    path: String,
    /// The photo's name, for messages.
    label: String,
    request: ClipRequest,
    /// The extension the staged file is written with (the container it is written in).
    ext: String,
    /// The size as shown, reported for the file.
    shown: (usize, usize),
    activity: Activity,
}

/// A fresh name for a staged file in the temporary folder.
fn staging_path(ext: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ember-clip-{}-{n}.{ext}", std::process::id()))
}

/// Whether a clip's container metadata is dropped from its export: when the export removes the location (or the photo is in a
/// private place), and for any metadata policy but "all". A clip's metadata is all or nothing, so the finer policies of still
/// exports lean to leaving out.
fn strips_metadata(o: &ExportOptions, removes_location: bool) -> bool {
    removes_location || !matches!(o.metadata, MetadataPolicy::All)
}

/// The probe facts of `p` as the clip export uses them: its length, its size as shown and its frame rate.
fn info_of(p: &Photo) -> lightcraft_video::VideoInfo {
    lightcraft_video::VideoInfo {
        duration: p.duration,
        width: p.width,
        height: p.height,
        rotation: 0,
        codec: p.video.as_ref().map(|v| v.codec.clone()).unwrap_or_default(),
        frame_rate: p.video.as_ref().and_then(|v| v.frame_rate),
        creation_time: None,
    }
}

/// Set up the export of video `id` as a clip (`format: "video"`). Refused before anything is made when the photo is not a video,
/// has no original file, or the sidecar is not installed.
pub(super) fn prepare(
    session: &mut crate::Session,
    id: PhotoId,
    o: &ExportOptions,
    seq: usize,
    guard: Arc<crate::originals::OriginalGuard>,
) -> Result<PreparedExport, String> {
    let p = session.catalog.photo(id).ok_or("no such photo")?;
    if p.kind != MediaKind::Video {
        return Err(format!("{} is not a video: choose a picture format", p.file_name));
    }
    let Source::File { path } = &p.source else {
        return Err(format!("{} is a generated demo photo: it has no original file to export", p.file_name));
    };
    let label = p.file_name.clone();
    lightcraft_video::available().map_err(|e| format!("{label}: {e}"))?;
    let request = ClipRequest {
        trim: Trim { start: p.develop.trim_in, end: p.develop.trim_out },
        info: Some(info_of(p)),
        reencode: o.video_reencode,
        video_kbps: o.video_kbps,
        strip_metadata: strips_metadata(o, removes_location(p, &session.catalog, o)),
    };
    let work = ClipWork {
        path: path.clone(),
        label,
        request,
        ext: output_extension(Path::new(&p.file_name), o.video_reencode),
        shown: (p.width as usize, p.height as usize),
        activity: session.activity.clone(),
    };
    Ok(PreparedExport { photo: id, file_name: o.file_name_for(p, seq), work: Work::Clip(Box::new(work)), guard, weight: CLIP_WEIGHT })
}

/// Make the clip: ffmpeg writes it to a staged file under the task row in the activity stack (its ✕ kills the child process).
pub(super) fn run(file_name: String, w: ClipWork) -> Result<Exported, String> {
    let staged = staging_path(&w.ext);
    let plan = plan_clip(Path::new(&w.path), &staged, &w.request, std::env::consts::OS).map_err(|e| format!("{}: {e}", w.label))?;
    let task = w.activity.start("video", "Exporting Video", Cancel::Yes);
    task.set_unit(Unit::Percent);
    let limits = export_limits(&plan, task.cancel_flag());
    export_clip(&plan, &limits, &mut |done| task.progress((done * 100.0).round() as u64, 100)).map_err(|e| format!("{}: {e}", w.label))?;
    drop(task);
    let size = std::fs::metadata(&staged).map(|m| m.len()).map_err(|e| format!("{}: {e}", staged.display()))?;
    // from here the file is owned by `StagedFile`: it is removed when the batch has placed it, or has failed to
    let mut detail = match plan.kind {
        ClipKind::Copy => json!({"mode": "copy", "accurate": false}),
        ClipKind::Reencode { encoder, video_kbps, .. } => {
            json!({"mode": "reencode", "accurate": true, "encoder": encoder.name(), "videoKbps": video_kbps})
        }
    };
    detail["start"] = json!(plan.start);
    detail["length"] = json!(plan.length);
    detail["strippedMetadata"] = json!(plan.strip_metadata);
    Ok(Exported {
        file_name,
        bytes: Vec::new(),
        width: w.shown.0,
        height: w.shown.1,
        sidecars: Vec::new(),
        gpu_fallback: None,
        staged: Some(StagedFile { path: staged, size, detail }),
    })
}

/// `p` with its poster frame at `t` seconds: what the loader of the frame at `t` is made from.
fn with_frame_time(p: &Photo, t: f64) -> Photo {
    let mut q = p.clone();
    let mut develop = (*q.develop).clone();
    develop.poster_time = Some(t);
    q.develop = Arc::new(develop);
    q
}

/// The loader of the frame of clip `p` at `t` seconds (held inside the clip): the poster loader of `p` with its poster at `t`.
pub(super) fn frame_loader(p: &Photo, t: f64, activity: Option<Activity>) -> crate::media::FileLoader {
    crate::video::poster_loader(&with_frame_time(p, t), activity)
}

/// Make `job` (the render of video `p` for a still export) read the frame at `t` seconds instead of the poster frame.
pub(super) fn at_frame(session: &mut crate::Session, job: &mut crate::media::RenderJob, p: &Photo, t: f64) {
    let mut source = session.media.source_ref_uncached(p, job.level);
    if let crate::media::SourceRef::File { loader, .. } = &mut source {
        *loader = Some(frame_loader(p, t, Some(session.activity.clone())));
        job.source = source;
        // the render's identity follows the frame, never the poster's
        job.key ^= t.to_bits().wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use crate::export::{Conflict, Destination, ExportFormat, Placer, Resize, prepare_export};

    fn session_with_clip(path: &str, duration: Option<f64>) -> (Session, PhotoId) {
        let mut s = Session::new();
        let id = s.catalog.alloc_photo_id();
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut p = Photo::new(id, Source::File { path: path.into() }, &name, "MOV", 1920, 1080, "2026-10-17T10:00:00");
        p.kind = MediaKind::Video;
        p.duration = duration;
        s.commit("add", lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        (s, id)
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-clip-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn staged(bytes: &[u8], detail: Value) -> (StagedFile, PathBuf) {
        let path = staging_path("mp4");
        std::fs::write(&path, bytes).unwrap();
        (StagedFile { path: path.clone(), size: bytes.len() as u64, detail }, path)
    }

    fn exported(name: &str, staged: StagedFile) -> Exported {
        Exported {
            file_name: name.into(),
            bytes: Vec::new(),
            width: 1920,
            height: 1080,
            sidecars: Vec::new(),
            gpu_fallback: None,
            staged: Some(staged),
        }
    }

    fn video_options() -> ExportOptions {
        ExportOptions { format: ExportFormat::Video, ..Default::default() }
    }

    #[test]
    fn the_video_format_and_its_options_parse_validate_and_round_trip() {
        let o = ExportOptions::from_params(&json!({"format": "video", "videoReencode": true, "videoKbps": 5000, "frameTime": 2.5})).unwrap();
        assert_eq!(o.format, ExportFormat::Video);
        assert!(o.video_reencode);
        assert_eq!((o.video_kbps, o.frame_time), (Some(5000), Some(2.5)));
        assert_eq!(ExportOptions::from_json(&o.to_json()), o);
        let plain = ExportOptions::default().to_json();
        for key in ["videoReencode", "videoKbps", "frameTime"] {
            assert!(plain.get(key).is_none(), "{key} is absent while unset, so saved presets and prefs do not change");
        }
        for bad in [
            json!({"videoKbps": 0}),
            json!({"videoKbps": "fast"}),
            json!({"videoKbps": 1.5}),
            json!({"frameTime": -1.0}),
            json!({"frameTime": "now"}),
            json!({"videoReencode": "yes"}),
            json!({"format": "mov"}),
        ] {
            assert!(ExportOptions::from_params(&bad).is_err(), "{bad}");
        }
        assert!(!ExportFormat::Video.is_rendered(), "a clip is not rendered to pixels");
    }

    #[test]
    fn a_clip_keeps_its_container_unless_it_is_re_encoded() {
        let (s, id) = session_with_clip("/clips/Beach.MOV", Some(20.0));
        let p = s.catalog.photo(id).unwrap();
        assert_eq!(video_options().file_name_for(p, 1), "Beach.mov");
        let named = ExportOptions { naming: "{name}-cut".into(), ..video_options() };
        assert_eq!(named.file_name_for(p, 1), "Beach-cut.mov");
        let again = ExportOptions { video_reencode: true, ..video_options() };
        assert_eq!(again.file_name_for(p, 1), "Beach.mp4");
    }

    #[test]
    fn a_clips_metadata_is_dropped_with_the_location_or_any_policy_but_all() {
        let o = video_options();
        assert!(!strips_metadata(&o, false));
        assert!(strips_metadata(&o, true), "a location that is removed, or private, is never copied into the clip");
        for metadata in [MetadataPolicy::AllExceptCamera, MetadataPolicy::Copyright, MetadataPolicy::None] {
            assert!(strips_metadata(&ExportOptions { metadata, ..video_options() }, false), "{metadata:?}");
        }
    }

    #[test]
    fn only_a_video_is_exported_as_a_clip() {
        let mut demo = Session::with_demo();
        let still = demo.catalog.photos().next().unwrap().id;
        let err = match prepare_export(&mut demo, still, &video_options(), 1) {
            Err(e) => e,
            Ok(_) => panic!("a still is not a clip"),
        };
        assert!(err.contains("not a video"), "{err}");
    }

    #[test]
    fn a_clip_export_without_the_sidecar_is_an_error_before_anything_is_made() {
        if lightcraft_video::available().is_ok() {
            eprintln!("SKIP: the ffmpeg sidecar is installed here");
            return;
        }
        let (mut s, id) = session_with_clip("/clips/beach.mov", Some(20.0));
        for reencode in [false, true] {
            let o = ExportOptions { video_reencode: reencode, ..video_options() };
            let err = match prepare_export(&mut s, id, &o, 1) {
                Err(e) => e,
                Ok(_) => panic!("there is no sidecar to cut the clip"),
            };
            assert!(err.contains("beach.mov") && (err.contains("not installed") || err.contains("not supported")), "{err}");
        }
    }

    #[test]
    fn a_staged_clip_is_placed_atomically_with_a_free_name_and_leaves_no_temp() {
        let dir = scratch("place");
        let original = dir.join("beach.mov");
        std::fs::write(&original, b"the original").unwrap();
        let (s, id) = session_with_clip(&original.to_string_lossy(), Some(20.0));
        let guard = s.original_guard();
        let out = dir.join("out");
        let to = Destination { dir: out.to_string_lossy().to_string(), exact: None };
        let o = ExportOptions { conflict: Conflict::Unique, ..video_options() };
        let wrote = std::cell::Cell::new(0usize);
        let mut write = |_: &str, _: &[u8]| -> Result<(), String> {
            wrote.set(wrote.get() + 1);
            Ok(())
        };
        let exists = |p: &str| Path::new(p).exists();
        let mut placer = Placer::new(&o, &to, &mut write, &exists, false);
        let detail = json!({"mode": "copy", "accurate": false, "start": 2.5, "length": 6.0});
        let (first, first_temp) = staged(b"clip bytes", detail.clone());
        assert!(first_temp.exists());
        placer.place(id, "beach.mov".into(), &guard, Ok(exported("beach.mov", first))).unwrap();
        assert!(!first_temp.exists(), "the staged file is gone once it is placed");
        let (second, second_temp) = staged(b"more bytes", detail.clone());
        placer.place(id, "beach.mov".into(), &guard, Ok(exported("beach.mov", second))).unwrap();
        assert!(!second_temp.exists());
        let results = std::mem::take(&mut placer.out);
        drop(placer);

        assert_eq!(wrote.get(), 0, "a clip is moved on disk, never pushed through the byte writer");
        assert_eq!(std::fs::read(out.join("beach.mov")).unwrap(), b"clip bytes");
        assert_eq!(std::fs::read(out.join("beach-2.mov")).unwrap(), b"more bytes", "a taken name gets the next free one");
        assert_eq!(results.len(), 2, "{results:?}");
        assert_eq!(results[0]["bytes"], 10);
        assert_eq!(results[0]["video"]["mode"], "copy");
        assert_eq!(results[0]["video"]["start"], 2.5);
        assert_eq!(std::fs::read(&original).unwrap(), b"the original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staged_clip_never_replaces_an_original_and_is_cleaned_up_when_it_is_not_placed() {
        let dir = scratch("guard");
        let original = dir.join("beach.mov");
        std::fs::write(&original, b"the original").unwrap();
        let (s, id) = session_with_clip(&original.to_string_lossy(), Some(20.0));
        let guard = s.original_guard();
        let to = Destination { dir: dir.to_string_lossy().to_string(), exact: None };
        let o = ExportOptions { conflict: Conflict::Overwrite, ..video_options() };
        let mut write = |_: &str, _: &[u8]| -> Result<(), String> { Ok(()) };
        let exists = |p: &str| Path::new(p).exists();
        let mut placer = Placer::new(&o, &to, &mut write, &exists, false);
        let (clip, temp) = staged(b"clip bytes", json!({"mode": "copy"}));
        placer.place(id, "beach.mov".into(), &guard, Ok(exported("beach.mov", clip))).unwrap();
        let results = std::mem::take(&mut placer.out);
        assert!(results[0]["error"].as_str().is_some_and(|e| e.contains("original")), "{results:?}");
        assert_eq!(std::fs::read(&original).unwrap(), b"the original", "the original is untouched, whatever the conflict policy");
        assert!(!temp.exists(), "a clip that could not be placed leaves no temporary file");

        // a clip skipped by the conflict policy is cleaned up too
        let skip = ExportOptions { conflict: Conflict::Skip, ..video_options() };
        let mut placer = Placer::new(&skip, &to, &mut write, &exists, false);
        let (clip, temp) = staged(b"clip bytes", json!({"mode": "copy"}));
        placer.place(id, "beach.mov".into(), &guard, Ok(exported("beach.mov", clip))).unwrap();
        assert!(placer.out[0].get("skipped").is_some(), "{:?}", placer.out);
        assert!(!temp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_frame_of_a_still_export_is_the_one_asked_for_inside_the_clip() {
        let (s, id) = session_with_clip("/clips/beach.mov", Some(20.0));
        let p = s.catalog.photo(id).unwrap();
        assert_eq!(crate::video::poster_secs_of(p), 2.0, "the poster is a tenth in");
        let at = with_frame_time(p, 7.5);
        assert_eq!(at.develop.poster_time, Some(7.5));
        assert_eq!(crate::video::poster_secs_of(&at), 7.5);
        assert_eq!(p.develop.poster_time, None, "the photo itself is not changed");
        assert!(crate::video::poster_secs_of(&with_frame_time(p, 500.0)) < 20.0, "a time past the end is held inside the clip");
    }

    #[test]
    fn a_frame_of_a_clip_exports_as_a_still_through_the_ordinary_pipeline() {
        if lightcraft_video::available().is_ok() {
            eprintln!("SKIP: the ffmpeg sidecar is installed here (this checks the frame the pipeline gets without it)");
            return;
        }
        let (mut s, id) = session_with_clip("/clips/beach.mov", Some(20.0));
        for frame_time in [None, Some(5.0), Some(500.0)] {
            let o = ExportOptions { format: ExportFormat::Png, resize: Some(Resize::long_edge(64)), frame_time, ..Default::default() };
            let e = prepare_export(&mut s, id, &o, 1).unwrap().run().unwrap();
            assert!(e.bytes.starts_with(b"\x89PNG"), "{frame_time:?}");
            assert!(e.staged.is_none(), "a still is written from memory like any other");
            assert_eq!((e.width, e.height), (64, 36), "{frame_time:?}");
            assert_eq!(e.file_name, "beach.png");
        }
    }
}
