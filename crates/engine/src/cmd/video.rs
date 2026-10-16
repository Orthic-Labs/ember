//! `video.setPoster`: which frame of a clip stands for it in the grid, the loupe and exports (issue #76). The time is kept
//! in the photo's develop settings (`poster_time`), so it is undoable, travels with versions and history, and changes the
//! thumbnail's cache key; a clip with none shows the frame 10 % into it (1 s when its duration is unknown).

use lightcraft_catalog::{MediaKind, PhotoId};
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

/// `{id?, t}`: `t` seconds into the clip (held inside it), or `null` for the default.
fn set_poster(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.setPoster";
    let id = match p.get("id").filter(|v| !v.is_null()) {
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(C, "`id` must be a photo id"))?),
        None => s.active().ok_or_else(|| bad(C, "no active photo"))?,
    };
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    if photo.kind != MediaKind::Video {
        return Err(bad(C, "the photo is not a video"));
    }
    let duration = photo.duration.filter(|d| d.is_finite() && *d > 0.0);
    let t = match p.get("t").ok_or_else(|| bad(C, "missing `t`: seconds into the clip, or null for the default"))? {
        Value::Null => None,
        v => {
            let t = v.as_f64().ok_or_else(|| bad(C, "`t` must be a number of seconds, or null for the default"))?;
            if !t.is_finite() || t < 0.0 {
                return Err(bad(C, "`t` must be zero or more seconds"));
            }
            Some(duration.map_or(t, |d| t.min(d)))
        }
    };
    let mut develop = (*photo.develop).clone();
    let shown = lightcraft_video::poster_secs(t, duration);
    if develop.poster_time == t {
        return Ok(json!({"id": id.0, "t": t, "poster": shown, "changed": false}));
    }
    develop.poster_time = t;
    let op = s.develop_op(id, develop, "Set Poster Frame").ok_or_else(|| bad(C, "no such photo"))?;
    s.commit("Set Poster Frame", op)?;
    Ok(json!({"id": id.0, "t": t, "poster": shown, "changed": true}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        explicit_targets: true,
        ..cmd!(
            "video.setPoster",
            "Set Poster Frame",
            [],
            None,
            "{id? (default: the active photo, a video), t: seconds into the clip, or null for the default (10 % into the clip, 1 s when its duration is unknown)}: the frame that stands for a video in the grid, the loupe and exports; one undo step → {id, t, poster (seconds used), changed}",
            has_video,
            set_poster
        )
    }]
}
