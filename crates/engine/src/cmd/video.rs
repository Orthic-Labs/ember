//! `video.setPoster`: which frame of a clip stands for it in the grid, the loupe and exports (issue #76). The time is kept
//! in the photo's develop settings (`poster_time`), so it is undoable, travels with versions and history, and changes the
//! thumbnail's cache key; a clip with none shows the frame 10 % into it (1 s when its duration is unknown).
//!
//! `video.play`, `video.pause`, `video.togglePlay`, `video.seek`, `video.stop` and `video.state`: silent playback in the loupe
//! (issue #73, [`crate::video_player`]). They change the session's player, never the library, so they are not journalled.

use lightcraft_catalog::{MediaKind, PhotoId};
use serde_json::{Value, json};
use web_time::Instant;

use super::{CommandSpec, always, bad, cmd};
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
    let t = match p.get("t") {
        // no time named: the one the player is at (the menu item, which cannot name one), when this clip is loaded in it
        None if s.video.photo() == Some(id) => {
            let at = s.video_position_now();
            Some(duration.map_or(at, |d| at.min(d)))
        }
        None => return Err(bad(C, "missing `t`: seconds into the clip, or null for the default (or play or scrub the clip to a frame first)")),
        Some(Value::Null) => None,
        Some(v) => {
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

/// `id` when the call names a video, else `None` (the active photo, or the loaded clip).
fn opt_id(p: &Value, c: &str) -> Result<Option<PhotoId>> {
    match p.get("id").filter(|v| !v.is_null()) {
        Some(v) => Ok(Some(PhotoId(v.as_u64().ok_or_else(|| bad(c, "`id` must be a photo id"))?))),
        None => Ok(None),
    }
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.play";
    let id = opt_id(p, C)?;
    s.video_play(id, Instant::now()).map_err(|e| bad(C, e))?;
    Ok(s.video_state_json(Instant::now()))
}

fn pause(s: &mut Session, _: &Value) -> Result<Value> {
    s.video_pause(Instant::now());
    Ok(s.video_state_json(Instant::now()))
}

fn toggle_play(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.togglePlay";
    let id = opt_id(p, C)?;
    let loaded = s.video.photo();
    let playing_this = s.video.state() == crate::video_player::PlayState::Playing && (id.is_none() || id == loaded);
    if playing_this {
        s.video_pause(Instant::now());
    } else {
        s.video_play(id, Instant::now()).map_err(|e| bad(C, e))?;
    }
    Ok(s.video_state_json(Instant::now()))
}

fn seek(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.seek";
    let id = opt_id(p, C)?;
    let t = p.get("t").and_then(Value::as_f64).ok_or_else(|| bad(C, "missing `t`: seconds into the clip"))?;
    s.video_seek(id, t).map_err(|e| bad(C, e))?;
    Ok(s.video_state_json(Instant::now()))
}

fn stop(s: &mut Session, _: &Value) -> Result<Value> {
    s.video_stop();
    Ok(s.video_state_json(Instant::now()))
}

fn state(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.video_state_json(Instant::now()))
}

pub fn specs() -> Vec<CommandSpec> {
    let named = |spec: CommandSpec| CommandSpec { explicit_targets: true, ..spec };
    vec![
        named(cmd!(
            "video.setPoster",
            "Set Poster Frame",
            ["Photo"],
            None,
            "{id? (default: the active photo, a video), t? (default: where the clip is in the player): seconds into the clip, or null for the default (10 % into the clip, 1 s when its duration is unknown)}: the frame that stands for a video in the grid, the loupe and exports; one undo step → {id, t, poster (seconds used), changed}",
            has_video,
            set_poster
        )),
        named(cmd!(
            query "video.play",
            "Play Video",
            [],
            None,
            "{id? (default: the active photo, a video)}: play the clip silently in the loupe, from where it was paused (from the start when it is another clip or ended); \"Video support isn't installed\" without the ffmpeg sidecar → the state, as `video.state`",
            has_video,
            play
        )),
        cmd!(query "video.pause", "Pause Video", [], None, "{}: stop the playing clip where it is → the state", always, pause),
        named(cmd!(
            query "video.togglePlay",
            "Play/Pause Video",
            [],
            None,
            "{id? (default: the active photo, a video)}: pause the playing clip, else play it → the state",
            has_video,
            toggle_play
        )),
        cmd!(
            query "video.seek",
            "Seek Video",
            [],
            None,
            "{id? (default: the loaded clip, else the active photo), t: seconds into the clip}: move to `t` (held inside the clip); playing goes on from there, paused shows the frame at `t` → the state",
            always,
            seek
        ),
        cmd!(
            query "video.stop",
            "Stop Video",
            [],
            None,
            "{}: stop and forget the clip; the loupe shows its poster again → the state",
            always,
            stop
        ),
        cmd!(
            query "video.state",
            "Video State",
            [],
            None,
            "{}: → {photo, state (stopped|playing|paused|ended), position, duration, streaming, dropped (frames lost to a slow display), error, unavailable}",
            always,
            state
        ),
    ]
}
