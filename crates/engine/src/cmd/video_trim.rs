//! `video.setTrim` and `video.clearTrim`: the part of a clip that exports keep (issue #74). The in and out points live in the
//! photo's develop settings (`trim_in`, `trim_out`), so they are undoable, travel with versions and history, and the file is
//! never cut. Both are held to the clip's duration with in before out ([`lightcraft_video::check_trim`]).

use lightcraft_catalog::{MediaKind, PhotoId};
use lightcraft_video::Trim;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd};
use crate::{Result, Session};

fn has_video(s: &Session) -> std::result::Result<(), String> {
    match s.active().and_then(|id| s.catalog.photo(id)) {
        Some(p) if p.kind == MediaKind::Video => Ok(()),
        Some(_) => Err("the photo is not a video".into()),
        None => Err("no photo selected".into()),
    }
}

/// The video `{id?}` names (default: the active photo).
fn video_id(s: &Session, c: &str, p: &Value) -> Result<PhotoId> {
    let id = match p.get("id").filter(|v| !v.is_null()) {
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(c, "`id` must be a photo id"))?),
        None => s.active().ok_or_else(|| bad(c, "no active photo"))?,
    };
    let photo = s.catalog.photo(id).ok_or_else(|| bad(c, "no such photo"))?;
    if photo.kind != MediaKind::Video {
        return Err(bad(c, "the photo is not a video"));
    }
    Ok(id)
}

/// A trim point from `p[key]`: absent keeps `current`, `null` clears it, a number sets it.
fn point(c: &str, p: &Value, key: &str, current: Option<f64>) -> Result<Option<f64>> {
    match p.get(key) {
        None => Ok(current),
        Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| bad(c, format!("`{key}` must be a number of seconds, or null to clear it"))),
    }
}

/// Store `trim` as photo `id`'s trim, as one undo step labelled `label`.
fn store(s: &mut Session, c: &str, id: PhotoId, trim: Trim, label: &str) -> Result<Value> {
    let photo = s.catalog.photo(id).ok_or_else(|| bad(c, "no such photo"))?;
    let (_, length) = lightcraft_video::kept_range(trim, photo.duration);
    let report = |changed: bool| json!({"id": id.0, "in": trim.start, "out": trim.end, "length": length, "changed": changed});
    if (photo.develop.trim_in, photo.develop.trim_out) == (trim.start, trim.end) {
        return Ok(report(false));
    }
    let mut develop = (*photo.develop).clone();
    develop.trim_in = trim.start;
    develop.trim_out = trim.end;
    let op = s.develop_op(id, develop, label).ok_or_else(|| bad(c, "no such photo"))?;
    s.commit(label, op)?;
    Ok(report(true))
}

/// `{id?, in?, out?}`: seconds into the clip. A point left out stays as it is, `null` clears it.
fn set_trim(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.setTrim";
    let id = video_id(s, C, p)?;
    if p.get("in").is_none() && p.get("out").is_none() {
        return Err(bad(C, "give `in`, `out` or both (seconds into the clip; null clears a point); video.clearTrim removes the trim"));
    }
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    let (start, end) = (point(C, p, "in", photo.develop.trim_in)?, point(C, p, "out", photo.develop.trim_out)?);
    let trim = lightcraft_video::check_trim(start, end, photo.duration).map_err(|e| bad(C, e))?;
    store(s, C, id, trim, "Set Trim")
}

/// `{id?}`: the whole clip again.
fn clear_trim(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.clearTrim";
    let id = video_id(s, C, p)?;
    store(s, C, id, Trim::default(), "Clear Trim")
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "video.setTrim",
                "Set Trim",
                [],
                None,
                "{id? (default: the active photo, a video), in?: seconds into the clip, out?: seconds into the clip}: the part of a clip that exports keep; a point left out stays as it is, null clears it; held to the clip's duration with in before out; one undo step, the file is never cut → {id, in, out, length (seconds kept, null when the duration is unknown), changed}",
                has_video,
                set_trim
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "video.clearTrim",
                "Clear Trim",
                [],
                None,
                "{id? (default: the active photo, a video)}: keep the whole clip again; one undo step → {id, in, out, length, changed}",
                has_video,
                clear_trim
            )
        },
    ]
}

#[cfg(test)]
mod tests {
    use lightcraft_catalog::{Op, Photo, Source};
    use serde_json::{Value, json};

    use crate::Session;

    fn session_with_clip(duration: Option<f64>) -> (Session, lightcraft_catalog::PhotoId) {
        let mut s = Session::new();
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: "/clips/beach.mov".into() }, "beach.mov", "MOV", 1920, 1080, "2026-10-17T10:00:00");
        p.kind = lightcraft_catalog::MediaKind::Video;
        p.duration = duration;
        s.commit("add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        (s, id)
    }

    fn trim_of(s: &Session, id: lightcraft_catalog::PhotoId) -> (Option<f64>, Option<f64>) {
        let d = &s.catalog.photo(id).unwrap().develop;
        (d.trim_in, d.trim_out)
    }

    #[test]
    fn set_trim_stores_the_points_as_one_undo_step() {
        let (mut s, id) = session_with_clip(Some(20.0));
        let unset = s.catalog.photo(id).unwrap().clone();
        let r = s.execute("video.setTrim", &json!({"id": id.0, "in": 2.5, "out": 12.0})).unwrap();
        assert_eq!((r["changed"].as_bool(), r["length"].as_f64()), (Some(true), Some(9.5)), "{r}");
        assert_eq!(trim_of(&s, id), (Some(2.5), Some(12.0)));
        let set = s.catalog.photo(id).unwrap().clone();
        assert!(set.history.last().is_some_and(|h| h.label == "Set Trim"));
        assert!(set.develop.is_unedited(), "a trim is a choice of range, not an edit of the picture");
        assert_ne!(set.develop.hash64(), unset.develop.hash64());
        let undo_depth = s.undo.len();

        // the same points again are nothing to do, not another step
        let r = s.execute("video.setTrim", &json!({"id": id.0, "in": 2.5, "out": 12.0})).unwrap();
        assert_eq!(r["changed"], false);
        assert_eq!(s.undo.len(), undo_depth);

        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(trim_of(&s, id), (None, None), "undone in one step");
        assert_eq!(s.catalog.photo(id).unwrap().develop.hash64(), unset.develop.hash64());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(trim_of(&s, id), (Some(2.5), Some(12.0)));
    }

    #[test]
    fn a_point_left_out_stays_and_null_clears_it() {
        let (mut s, id) = session_with_clip(Some(20.0));
        s.execute("video.setTrim", &json!({"id": id.0, "in": 2.0})).unwrap();
        assert_eq!(trim_of(&s, id), (Some(2.0), None));
        s.execute("video.setTrim", &json!({"id": id.0, "out": 9.0})).unwrap();
        assert_eq!(trim_of(&s, id), (Some(2.0), Some(9.0)), "the in point stayed");
        s.execute("video.setTrim", &json!({"id": id.0, "in": Value::Null})).unwrap();
        assert_eq!(trim_of(&s, id), (None, Some(9.0)), "null cleared the in point");
        // an out point at the end of the clip is no out point
        s.execute("video.setTrim", &json!({"id": id.0, "out": 20.0})).unwrap();
        assert_eq!(trim_of(&s, id), (None, None));
        s.execute("video.setTrim", &json!({"id": id.0, "in": 3.0, "out": 500.0})).unwrap();
        assert_eq!(trim_of(&s, id), (Some(3.0), None), "an out point past the end is held to it");
    }

    #[test]
    fn clear_trim_keeps_the_whole_clip_and_is_undoable() {
        let (mut s, id) = session_with_clip(Some(20.0));
        s.execute("video.setTrim", &json!({"id": id.0, "in": 1.0, "out": 5.0})).unwrap();
        let r = s.execute("video.clearTrim", &json!({"id": id.0})).unwrap();
        assert_eq!((r["changed"].as_bool(), r["length"].as_f64()), (Some(true), Some(20.0)), "{r}");
        assert_eq!(trim_of(&s, id), (None, None));
        assert!(s.catalog.photo(id).unwrap().develop.to_json().get("trim_in").is_none(), "unset is absent from the JSON, so hashes hold");
        assert!(s.catalog.photo(id).unwrap().history.last().is_some_and(|h| h.label == "Clear Trim"));
        let again = s.execute("video.clearTrim", &json!({"id": id.0})).unwrap();
        assert_eq!(again["changed"], false);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(trim_of(&s, id), (Some(1.0), Some(5.0)));
    }

    #[test]
    fn a_trim_is_refused_outside_the_clip_and_changes_nothing() {
        let (mut s, id) = session_with_clip(Some(10.0));
        s.execute("video.setTrim", &json!({"id": id.0, "in": 1.0, "out": 4.0})).unwrap();
        let refused = [
            json!({"id": id.0}),
            json!({"id": id.0, "in": 5.0}),
            json!({"id": id.0, "in": 4.0}),
            json!({"id": id.0, "in": 10.0}),
            json!({"id": id.0, "in": 50.0}),
            json!({"id": id.0, "out": 0.5}),
            json!({"id": id.0, "in": -1.0}),
            json!({"id": id.0, "out": -1.0}),
            json!({"id": id.0, "in": "soon"}),
            json!({"id": id.0, "out": true}),
            json!({"id": "x", "in": 1.0}),
            json!({"id": 999_999, "in": 1.0}),
        ];
        for bad in refused {
            assert!(s.execute("video.setTrim", &bad).is_err(), "{bad}");
        }
        assert_eq!(trim_of(&s, id), (Some(1.0), Some(4.0)), "refused calls change nothing");
        assert!(s.execute("video.clearTrim", &json!({"id": 999_999})).is_err());
    }

    #[test]
    fn a_clip_of_unknown_length_takes_a_trim_checked_only_against_itself() {
        let (mut s, id) = session_with_clip(None);
        let r = s.execute("video.setTrim", &json!({"id": id.0, "in": 2.0, "out": 6.0})).unwrap();
        assert_eq!(r["length"].as_f64(), Some(4.0), "{r}");
        assert!(s.execute("video.setTrim", &json!({"id": id.0, "in": 7.0})).is_err(), "in after the stored out point");
        s.execute("video.setTrim", &json!({"id": id.0, "out": Value::Null})).unwrap();
        let r = s.execute("video.setTrim", &json!({"id": id.0, "in": 7.0})).unwrap();
        assert!(r["length"].is_null(), "no end, no length: {r}");
    }

    #[test]
    fn the_active_video_is_the_default_and_a_still_has_no_trim() {
        let (mut s, id) = session_with_clip(Some(10.0));
        s.selection.active = Some(id);
        s.execute("video.setTrim", &json!({"in": 2.0})).unwrap();
        assert_eq!(trim_of(&s, id), (Some(2.0), None));
        s.execute("video.clearTrim", &json!({})).unwrap();
        assert_eq!(trim_of(&s, id), (None, None));

        let mut demo = Session::with_demo();
        let still = demo.catalog.photos().next().unwrap().id;
        for (cmd, p) in [("video.setTrim", json!({"id": still.0, "in": 1.0})), ("video.clearTrim", json!({"id": still.0}))] {
            let err = demo.execute(cmd, &p).unwrap_err();
            assert!(err.to_string().contains("not a video"), "{cmd}: {err}");
        }
        assert!(demo.catalog.photo(still).unwrap().develop.trim_in.is_none());
    }
}
