//! Video foundation (issues #73, #76): probe facts at import, the poster frame and `video.setPoster`, and import of a clip
//! where the ffmpeg sidecar is not installed.

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use serde_json::{Value, json};

use crate::Session;
use crate::media::ProbeInfo;

fn session_with_clip(duration: Option<f64>) -> (Session, PhotoId) {
    let mut s = Session::new();
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/clips/beach.mov".into() }, "beach.mov", "MOV", 1920, 1080, "2026-10-17T10:00:00");
    p.kind = MediaKind::Video;
    p.duration = duration;
    s.commit("add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    (s, id)
}

fn poster_of(s: &Session, id: PhotoId) -> Option<f64> {
    s.catalog.photo(id).unwrap().develop.poster_time
}

#[test]
fn every_video_extension_is_importable() {
    for e in lightcraft_video::EXTENSIONS {
        assert!(crate::import::EXTENSIONS.contains(e), "{e} is a video extension the import does not list");
        assert!(crate::import::is_supported(std::path::Path::new(&format!("clip.{}", e.to_ascii_uppercase()))), "{e}");
    }
}

#[test]
fn creation_time_becomes_a_capture_time() {
    use crate::video::captured_from;
    assert_eq!(captured_from("2026-10-04T08:16:11.000000Z").as_deref(), Some("2026-10-04T08:16:11"));
    assert_eq!(captured_from("2026-10-04 08:16:11").as_deref(), Some("2026-10-04T08:16:11"));
    assert_eq!(captured_from(" 2026-10-04T08:16:11+02:00 ").as_deref(), Some("2026-10-04T08:16:11"));
    for bad in ["", "yesterday", "2026-10-04", "20261004T081611", "2026-1o-04T08:16:11", "2026-10-04T08:16"] {
        assert_eq!(captured_from(bad), None, "{bad:?}");
    }
    assert_eq!(captured_from("2026-10-04T08:16:1é"), None, "a cut inside a character is no panic");
}

#[test]
fn probe_facts_become_the_import_record() {
    let probed = lightcraft_video::parse_probe_json(
        r#"{"streams": [{"codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 2160, "avg_frame_rate": "60/1",
            "side_data_list": [{"rotation": 90}]}],
            "format": {"duration": "42.5", "tags": {"creation_time": "2026-10-04T08:16:11.000000Z"}}}"#,
    )
    .unwrap();
    let base = ProbeInfo { format: "MP4".into(), file_size: 7, ..Default::default() };
    let info = crate::video::info_from(base, &probed);
    assert_eq!((info.width, info.height), (2160, 3840), "the size as shown, after the rotation");
    assert_eq!(info.kind, MediaKind::Video);
    assert_eq!((info.format.as_str(), info.file_size), ("MP4", 7), "what the file system said is kept");
    assert_eq!(info.duration, Some(42.5));
    assert_eq!(info.captured.as_deref(), Some("2026-10-04T08:16:11"));
    let video = info.video.unwrap();
    assert_eq!((video.codec.as_str(), video.frame_rate, video.rotation), ("hevc", Some(60.0), 90));
    assert!(info.content_hash.is_none(), "a video is never read whole for a hash");
}

#[test]
fn the_poster_defaults_to_a_tenth_into_the_clip_and_is_never_chosen_outside_it() {
    let (mut s, id) = session_with_clip(Some(60.0));
    assert_eq!(crate::video::poster_secs_of(s.catalog.photo(id).unwrap()), 6.0);
    s.execute("video.setPoster", &json!({"id": id.0, "t": 12.0})).unwrap();
    assert_eq!(crate::video::poster_secs_of(s.catalog.photo(id).unwrap()), 12.0);
    let (s2, id2) = session_with_clip(None);
    assert_eq!(crate::video::poster_secs_of(s2.catalog.photo(id2).unwrap()), 1.0, "a second in when the duration is unknown");
    let r = s.execute("video.setPoster", &json!({"id": id.0, "t": 500.0})).unwrap();
    assert_eq!(poster_of(&s, id), Some(60.0), "held to the duration: {r}");
    assert!(crate::video::poster_secs_of(s.catalog.photo(id).unwrap()) < 60.0, "and cut inside the clip");
}

#[test]
fn set_poster_is_one_undo_step_and_changes_the_keys_only_when_set() {
    let (mut s, id) = session_with_clip(Some(20.0));
    let unset = s.catalog.photo(id).unwrap().clone();
    assert_eq!(unset.develop.hash64(), DevelopSettings::default().hash64(), "an unset poster leaves the settings hash as it was");
    assert!(unset.develop.to_json().get("poster_time").is_none(), "and the JSON");
    let before = (s.media.source_key(&unset), s.media.source_identity(&unset));

    let r = s.execute("video.setPoster", &json!({"id": id.0, "t": 7.5})).unwrap();
    assert_eq!((r["changed"].as_bool(), r["poster"].as_f64()), (Some(true), Some(7.5)), "{r}");
    let set = s.catalog.photo(id).unwrap().clone();
    assert_eq!(set.develop.poster_time, Some(7.5));
    assert_ne!(set.develop.hash64(), unset.develop.hash64(), "a poster time invalidates the thumbnails");
    assert_ne!(s.media.source_key(&set), before.0, "and the decoded sources");
    assert_ne!(s.media.source_identity(&set), before.1);
    assert!(set.history.last().is_some_and(|h| h.label == "Set Poster Frame"));
    let undo_depth = s.undo.len();

    // the same time again is nothing to do, not another step
    let r = s.execute("video.setPoster", &json!({"id": id.0, "t": 7.5})).unwrap();
    assert_eq!(r["changed"], false);
    assert_eq!(s.undo.len(), undo_depth);

    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(poster_of(&s, id), None, "undone in one step");
    let back = s.catalog.photo(id).unwrap().clone();
    assert_eq!(back.develop.hash64(), unset.develop.hash64());
    assert_eq!((s.media.source_key(&back), s.media.source_identity(&back)), before);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(poster_of(&s, id), Some(7.5));

    // null goes back to the default; that is undoable too
    s.execute("video.setPoster", &json!({"id": id.0, "t": Value::Null})).unwrap();
    assert_eq!(poster_of(&s, id), None);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(poster_of(&s, id), Some(7.5));
}

#[test]
fn set_poster_uses_the_active_video_and_refuses_what_it_cannot_set() {
    let (mut s, id) = session_with_clip(Some(10.0));
    s.selection.active = Some(id);
    s.execute("video.setPoster", &json!({"t": 2.0})).unwrap();
    assert_eq!(poster_of(&s, id), Some(2.0));
    let refused = [
        json!({"id": id.0}),
        json!({"id": id.0, "t": -1.0}),
        json!({"id": id.0, "t": "soon"}),
        json!({"id": "x", "t": 1.0}),
        json!({"id": 999_999, "t": 1.0}),
    ];
    for bad in refused {
        assert!(s.execute("video.setPoster", &bad).is_err(), "{bad}");
    }
    assert_eq!(poster_of(&s, id), Some(2.0), "refused calls change nothing");

    // a still photo has no poster
    let mut demo = Session::with_demo();
    let still = demo.catalog.photos().next().unwrap().id;
    let err = demo.execute("video.setPoster", &json!({"id": still.0, "t": 1.0})).unwrap_err();
    assert!(err.to_string().contains("not a video"), "{err}");
    assert!(demo.catalog.photo(still).unwrap().develop.poster_time.is_none());
}

#[test]
fn the_placeholder_is_widescreen_and_sized_by_the_level() {
    let small = crate::video::placeholder(64);
    assert_eq!((small.width, small.height), (64, 36));
    let big = crate::video::placeholder(usize::MAX);
    assert_eq!((big.width, big.height), (1280, 720), "never larger than the nominal size");
    let tiny = crate::video::placeholder(0);
    assert!(tiny.width >= 16 && tiny.height >= 9);
    let (c, edge) = (small.get(32, 18), small.get(1, 1));
    assert!(c[0] > edge[0], "the play mark is lighter than the ground: {c:?} {edge:?}");
}

/// Importing a clip where the sidecar is not installed (a development checkout, CI) adds it at the placeholder size with a
/// placeholder thumbnail; it is never an import failure. (Skipped where the sidecar is installed: the file is not a video.)
#[test]
fn a_video_imports_without_the_sidecar_and_shows_a_placeholder() {
    if lightcraft_video::available().is_ok() {
        eprintln!("SKIP: the ffmpeg sidecar is installed here");
        return;
    }
    let dir = std::env::temp_dir().join(format!("lc-video-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("clip.MP4"), b"not really a video, only bytes").unwrap();
    std::fs::write(dir.join("notes.txt"), b"not a photo").unwrap();

    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(0), "{r}");
    let p = s.catalog.photos().next().unwrap().clone();
    assert_eq!((p.kind, p.format.as_str()), (MediaKind::Video, "MP4"));
    assert_eq!((p.width, p.height), crate::video::PLACEHOLDER_SIZE, "catalogued at the placeholder size");
    assert!(p.duration.is_none() && p.video.is_none(), "nothing probed");
    assert_eq!(p.file_size, 30);
    assert!(p.content_hash.is_none(), "the bytes of a video are not hashed");
    assert!(p.captured.is_some(), "the file's date stands in for the capture time");

    // its thumbnail and its preview are the placeholder, not an error
    let thumb = s.render_now(p.id, 64, 64).unwrap();
    assert!(thumb.image.width > thumb.image.height && thumb.image.width <= 64, "{}x{}", thumb.image.width, thumb.image.height);
    let view = s.source_now(p.id, crate::SourceLevel::Preview).unwrap();
    assert!(view.width > view.height, "a 16:9 placeholder");

    // the file is probed again and imported once: by path, it is a duplicate
    let again = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(again["imported"].as_array().map(Vec::len), Some(0), "{again}");

    // an unreadable path is an error, not a placeholder
    assert!(crate::video::probe_file(&dir.join("missing.mp4").to_string_lossy()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_poster_loader_gives_a_placeholder_without_the_sidecar_and_never_reads_the_clip() {
    if lightcraft_video::available().is_ok() {
        eprintln!("SKIP: the ffmpeg sidecar is installed here");
        return;
    }
    // a path that does not exist: the placeholder is made without the file (the sidecar is what would read it)
    let (img, _) = crate::video::load_poster("/no/such/clip.mp4", Some(10.0), None, Some((1920, 1080)), 256, None).unwrap();
    assert_eq!((img.width, img.height), (256, 144));
    // the still-image loader does not try to decode a clip either
    let (loader, _) = crate::files::fs_hooks();
    let (img, _) = loader("/no/such/clip.mov", 128).unwrap();
    assert_eq!((img.width, img.height), (128, 72));
}
