//! Photo > Capture Frame (issue #113): file names, the capture time of the frame, the metadata copy, the stack, one undo step and
//! the error without the sidecar. The frame comes from a fake decoder; the JPEG is written and imported for real.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_raster::Rgba8;
use serde_json::json;

use crate::Session;
use crate::video_capture::{captured_at_frame, frame_file_name};

type Frame = std::result::Result<Rgba8, String>;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-capture-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch folder");
    d
}

/// A clip on disk (the file need not be a real video: the decoder is faked) with a capture time, keywords and a rating.
fn clip(dir: &Path) -> (Session, PhotoId) {
    let mut s = Session::new();
    let id = s.catalog.alloc_photo_id();
    let path = dir.join("beach.mov");
    std::fs::write(&path, b"not really a clip").expect("clip file");
    let mut p = Photo::new(id, Source::File { path: path.to_string_lossy().to_string() }, "beach.mov", "MOV", 1920, 1080, "2026-10-17T10:00:00");
    p.kind = MediaKind::Video;
    p.duration = Some(20.0);
    p.captured = Some("2026-10-17T10:00:00".into());
    p.rating = 4;
    p.meta.keywords = vec!["travel".into(), "sea".into()];
    s.commit("add", Op::AddPhoto { photo: Box::new(p) }).expect("add the clip");
    (s, id)
}

/// A flat 64 x 36 frame, whatever was asked.
fn fake_frame(_: &Path, _: f64, _: usize, _: Option<Arc<AtomicBool>>) -> Frame {
    Rgba8::from_bytes(64, 36, &[120u8; 64 * 36 * 4]).ok_or_else(|| "bad fake frame".to_string())
}

/// What the sidecar says when it is not installed.
fn no_sidecar(_: &Path, _: f64, _: usize, _: Option<Arc<AtomicBool>>) -> Frame {
    Err(lightcraft_video::VideoError::NotInstalled.to_string())
}

#[test]
fn frame_files_are_named_after_the_clip_and_never_overwrite_one() {
    assert_eq!(frame_file_name("beach.mov", 7.5, &|_| false), "beach frame 7.500s.jpg");
    let taken = ["beach frame 7.500s.jpg", "beach frame 7.500s (2).jpg"];
    assert_eq!(frame_file_name("beach.mov", 7.5, &|n| taken.contains(&n)), "beach frame 7.500s (3).jpg");
    assert_eq!(frame_file_name("", 0.0, &|_| false), "clip frame 0.000s.jpg", "a clip with no name still gets a name");
}

#[test]
fn the_frame_is_captured_at_its_own_second_of_the_clip() {
    assert_eq!(captured_at_frame("2026-10-17T10:00:00", 7.5).as_deref(), Some("2026-10-17T10:00:07"));
    assert_eq!(captured_at_frame("2026-10-17T10:00:58", 5.9).as_deref(), Some("2026-10-17T10:01:03"), "crosses the minute");
    assert_eq!(captured_at_frame("not a time", 1.0), None);
}

#[test]
fn capture_writes_a_jpeg_beside_the_clip_and_stacks_it_with_the_clip_as_one_undo_step() {
    let dir = scratch("ok");
    let (mut s, id) = clip(&dir);
    let depth = s.undo.len();

    let r = crate::video_capture::capture_frame(&mut s, id, 7.5, &fake_frame).expect("capture");
    let out = PathBuf::from(r["path"].as_str().expect("path"));
    assert_eq!(out, dir.join("beach frame 7.500s.jpg"));
    assert!(out.exists(), "the JPEG is on disk");
    let new = PhotoId(r["id"].as_u64().expect("id"));

    // metadata copied: capture time moved to the frame, keywords, rating
    let frame = s.catalog.photo(new).expect("the frame is in the library").clone();
    assert_ne!(frame.kind, MediaKind::Video);
    assert_eq!(frame.captured.as_deref(), Some("2026-10-17T10:00:07"));
    assert_eq!(frame.rating, 4);
    assert_eq!(frame.meta.keywords, vec!["travel".to_string(), "sea".to_string()]);

    // stacked with the clip, the frame on top
    let stack = s.catalog.stack_of(new).expect("the frame is stacked").clone();
    assert_eq!(stack.photos.first(), Some(&new));
    assert!(stack.photos.contains(&id));
    assert_eq!(s.catalog.stack_of(id).map(|x| x.id), Some(stack.id));

    // one undo step, which takes the frame and its stack back; the clip stays and the JPEG stays on disk
    assert_eq!(s.undo.len(), depth + 1, "import, metadata and stack are one step");
    assert_eq!(s.undo.last().map(|e| e.label.as_str()), Some("Capture Frame"));
    s.execute("edit.undo", &json!({})).expect("undo");
    assert!(s.catalog.photo(new).is_none());
    assert!(s.catalog.stack_of(id).is_none());
    assert!(s.catalog.photo(id).is_some());
    assert!(out.exists());
}

#[test]
fn a_second_capture_at_the_same_time_gets_its_own_file() {
    let dir = scratch("twice");
    let (mut s, id) = clip(&dir);
    let first = crate::video_capture::capture_frame(&mut s, id, 3.0, &fake_frame).expect("first");
    let second = crate::video_capture::capture_frame(&mut s, id, 3.0, &fake_frame).expect("second");
    assert_eq!(first["path"].as_str().map(|p| p.ends_with("beach frame 3.000s.jpg")), Some(true));
    assert_eq!(second["path"].as_str().map(|p| p.ends_with("beach frame 3.000s (2).jpg")), Some(true));
    assert_ne!(first["id"], second["id"]);
}

#[test]
fn without_the_sidecar_capture_says_so_and_changes_nothing() {
    let dir = scratch("nosidecar");
    let (mut s, id) = clip(&dir);
    let depth = s.undo.len();
    let e = crate::video_capture::capture_frame(&mut s, id, 7.5, &no_sidecar).expect_err("no sidecar");
    assert!(e.to_string().contains("video support not installed"), "{e}");
    assert!(!dir.join("beach frame 7.500s.jpg").exists(), "no file is written");
    assert_eq!(s.undo.len(), depth, "no undo step");
}

#[test]
fn a_time_outside_the_clip_or_a_non_video_is_refused_before_any_work() {
    let dir = scratch("refuse");
    let (mut s, id) = clip(&dir);
    let e = s.execute("video.captureFrame", &json!({"id": id.0, "t": -1})).expect_err("negative time");
    assert!(e.to_string().contains("zero or more seconds"), "{e}");
    let other = s.catalog.alloc_photo_id();
    let still_path = dir.join("still.jpg").to_string_lossy().to_string();
    let still = Photo::new(other, Source::File { path: still_path }, "still.jpg", "JPEG", 10, 10, "2026-10-17T10:00:00");
    s.commit("add", Op::AddPhoto { photo: Box::new(still) }).expect("add a still");
    let e = s.execute("video.captureFrame", &json!({"id": other.0})).expect_err("not a video");
    assert!(e.to_string().contains("not a video"), "{e}");
    assert!(!dir.join("still frame 0.000s.jpg").exists());
}
