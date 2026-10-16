//! Silent playback (issue #73): the player's state transitions, frames shown at their time, the child killed whenever the stream is
//! let go, and errors that leave the clip paused. The streams are in-memory frames and a counting stand-in for the child, so none of
//! it needs the ffmpeg sidecar.

use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_video::{FrameStream, Kill, StreamSpec, VideoError};
use serde_json::{Value, json};
use web_time::Instant;

use crate::Session;
use crate::video_player::{OpenStream, PlayState, StreamRequest};

/// What the fake opener saw: the start of every stream it opened, and how many of its children were killed.
#[derive(Default)]
struct Log {
    starts: Mutex<Vec<f64>>,
    kills: AtomicUsize,
}

impl Log {
    fn starts(&self) -> Vec<f64> {
        self.starts.lock().unwrap().clone()
    }
    fn kills(&self) -> usize {
        self.kills.load(Ordering::SeqCst)
    }
}

struct CountKill(Arc<Log>);

impl Kill for CountKill {
    fn kill(&mut self) {
        self.0.kills.fetch_add(1, Ordering::SeqCst);
    }
}

/// Frames of 2x1 RGBA pixels (8 bytes), every byte the frame's number.
fn frames(n: usize) -> Vec<u8> {
    (0..n).flat_map(|i| [i as u8; 8]).collect()
}

fn spec_at(start: f64) -> StreamSpec {
    StreamSpec { start, fps: 25.0, width: 2, height: 1 }
}

/// A clip of `n` frames at 25 fps from the requested start, all available at once, then its end.
fn short_clip(log: &Arc<Log>, n: usize) -> OpenStream {
    let log = log.clone();
    Arc::new(move |r: &StreamRequest| {
        log.starts.lock().unwrap().push(r.start);
        let (stdout, kill) = (Box::new(Cursor::new(frames(n))), Box::new(CountKill(log.clone())));
        FrameStream::from_parts(stdout, None, kill, spec_at(r.start), 64, Some(r.cancel.clone()))
    })
}

/// A reader that never ends and makes a frame every 20 ms: a clip that is still playing.
struct Running;

impl Read for Running {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(20));
        buf.fill(1);
        Ok(buf.len())
    }
}

fn long_clip(log: &Arc<Log>) -> OpenStream {
    let log = log.clone();
    Arc::new(move |r: &StreamRequest| {
        log.starts.lock().unwrap().push(r.start);
        FrameStream::from_parts(Box::new(Running), None, Box::new(CountKill(log.clone())), spec_at(r.start), 4, Some(r.cancel.clone()))
    })
}

/// A session with one 10 s video, active.
fn session_with_video() -> (Session, PhotoId) {
    let mut s = Session::new();
    let id = add_video(&mut s, "/clips/a.mov");
    s.selection.active = Some(id);
    (s, id)
}

fn add_video(s: &mut Session, path: &str) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: path.into() }, path, "MOV", 1920, 1080, "2026-10-17T10:00:00");
    p.kind = MediaKind::Video;
    p.duration = Some(10.0);
    s.commit("add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn state(v: &Value) -> &str {
    v["state"].as_str().unwrap_or("?")
}

fn rows(s: &Session) -> usize {
    s.activity.list().iter().filter(|t| t.kind == "video").count()
}

#[test]
fn playing_holds_an_activity_row_and_pausing_stops_the_clock_and_kills_the_child() {
    let (mut s, _) = session_with_video();
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    let t0 = Instant::now();
    s.video_play(None, t0).unwrap();
    assert_eq!(s.video.state(), PlayState::Playing);
    assert_eq!(rows(&s), 1, "a running stream is in the activity stack");
    assert_eq!(log.starts(), vec![0.0], "a clip starts at its start");
    until("the first frame", || s.video_tick(t0) && s.video.frame().is_some());
    let (_, first) = s.video.frame().unwrap();
    assert_eq!((first.index, first.pts), (0, 0.0));
    assert!((s.video.position_at(t0 + Duration::from_secs(2)) - 2.0).abs() < 1e-9, "the clock runs from the first frame");

    s.video_pause(t0 + Duration::from_secs(2));
    assert_eq!(s.video.state(), PlayState::Paused);
    assert!((s.video.position_at(t0 + Duration::from_secs(99)) - 2.0).abs() < 1e-9, "paused: the clock stands where it stopped");
    assert_eq!(log.kills(), 1, "pausing kills the child");
    assert_eq!((rows(&s), s.video.is_streaming()), (0, false), "and gives up the row and the stream");
    assert!(s.video.frame().is_some(), "the picture it stopped on stays");
    assert!(!s.video_tick(t0), "nothing to tick without a stream");
    s.video_pause(t0); // pausing a paused clip is nothing
    assert_eq!(log.kills(), 1);
}

#[test]
fn a_paused_clip_resumes_where_it_stopped_and_toggle_plays_and_pauses() {
    let (mut s, id) = session_with_video();
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    let r = s.execute("video.togglePlay", &json!({})).unwrap();
    assert_eq!((state(&r), r["photo"].as_u64()), ("playing", Some(id.0)), "{r}");
    let r = s.execute("video.play", &json!({})).unwrap();
    assert_eq!(state(&r), "playing");
    assert_eq!(log.starts().len(), 1, "playing a playing clip opens nothing new");
    let r = s.execute("video.togglePlay", &json!({})).unwrap();
    assert_eq!(state(&r), "paused", "{r}");
    assert_eq!(log.kills(), 1);

    // resume from a chosen point: seek while paused puts the clock there, play goes on from it
    s.execute("video.seek", &json!({"t": 6.0})).unwrap();
    let r = s.execute("video.togglePlay", &json!({})).unwrap();
    assert_eq!(state(&r), "playing");
    assert_eq!(log.starts().last().copied(), Some(6.0), "{:?}", log.starts());
    s.execute("video.pause", &json!({})).unwrap();
    let r = s.execute("video.state", &json!({})).unwrap();
    assert_eq!(state(&r), "paused");
}

#[test]
fn a_frame_is_shown_when_the_clock_reaches_its_time_and_the_end_ends_the_clip() {
    let (mut s, _) = session_with_video();
    let log = Arc::new(Log::default());
    s.video_opener = short_clip(&log, 5); // frames at 0, .04, .08, .12, .16 s
    let t0 = Instant::now();
    s.video_play(None, t0).unwrap();
    until("the stream to be read", || {
        s.video_tick(t0);
        s.video.buffered() == 4
    });
    assert_eq!(s.video.frame().map(|(_, f)| f.index), Some(0), "the clock starts with the first frame, which shows at once");

    let seq = s.video.frame_seq();
    assert!(s.video_tick(t0 + Duration::from_millis(90)));
    assert_eq!(s.video.frame().map(|(_, f)| f.index), Some(2), "the frame whose time has come; the one before it is skipped");
    assert_eq!(s.video.frame_seq(), seq + 1, "one new picture to upload");
    assert!(!s.video_tick(t0 + Duration::from_millis(91)), "nothing new is due yet");

    until("the end", || {
        s.video_tick(t0 + Duration::from_secs(5));
        s.video.state() == PlayState::Ended
    });
    assert_eq!(s.video.frame().map(|(_, f)| f.index), Some(4), "the last frame stays up");
    assert_eq!(s.video.position_at(t0 + Duration::from_secs(5)), 10.0, "ended: at the end of the clip");
    assert_eq!((rows(&s), s.video.is_streaming(), log.kills()), (0, false, 1), "the stream and its child are let go");

    // playing an ended clip plays it again from its start
    s.video_play(None, t0).unwrap();
    assert_eq!((s.video.state(), log.starts()), (PlayState::Playing, vec![0.0, 0.0]));
}

#[test]
fn a_seek_while_paused_shows_the_frame_there_without_playing() {
    let (mut s, _) = session_with_video();
    let log = Arc::new(Log::default());
    s.video_opener = short_clip(&log, 5);
    let t0 = Instant::now();
    s.video_seek(None, 4.0).unwrap();
    assert_eq!((s.video.state(), log.starts()), (PlayState::Paused, vec![4.0]), "paused, and a stream fetches the frame at 4 s");
    until("the frame at 4 s", || {
        s.video_tick(t0);
        s.video.frame().is_some()
    });
    let (_, f) = s.video.frame().unwrap();
    assert_eq!((f.index, f.pts), (0, 4.0));
    assert_eq!((s.video.state(), s.video.is_streaming(), log.kills(), rows(&s)), (PlayState::Paused, false, 1, 0), "one frame, then done");
    assert_eq!(s.video.position_at(t0 + Duration::from_secs(9)), 4.0, "the clock does not run");

    // a seek is held inside the clip, and a bad one is refused
    s.video_seek(None, 500.0).unwrap();
    assert!(log.starts().last().is_some_and(|t| *t < 10.0 && *t > 9.0), "{:?}", log.starts());
    assert!(s.video_seek(None, -1.0).is_err());
    assert!(s.video_seek(None, f64::NAN).is_err());
    assert!(s.execute("video.seek", &json!({})).is_err(), "`t` is required");
}

#[test]
fn a_seek_while_playing_restarts_the_stream_there_and_keeps_playing() {
    let (mut s, _) = session_with_video();
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    let t0 = Instant::now();
    s.video_play(None, t0).unwrap();
    s.video_seek(None, 7.0).unwrap();
    assert_eq!(s.video.state(), PlayState::Playing);
    assert_eq!(log.starts(), vec![0.0, 7.0]);
    assert_eq!(log.kills(), 1, "the stream before the seek is killed");
    assert_eq!(rows(&s), 1, "and one row remains");
    until("a frame from 7 s", || s.video_tick(t0) && s.video.frame().is_some());
    assert_eq!(s.video.frame().map(|(_, f)| f.pts), Some(7.0));
    assert!((s.video.position_at(t0) - 7.0).abs() < 1e-9);
    s.video_stop();
    assert_eq!((log.kills(), rows(&s)), (2, 0), "stopping lets go of the stream");
}

#[test]
fn switching_clips_kills_the_old_stream_and_stop_forgets_the_clip() {
    let (mut s, a) = session_with_video();
    let b = add_video(&mut s, "/clips/b.mov");
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    s.execute("video.play", &json!({"id": a.0})).unwrap();
    let r = s.execute("video.play", &json!({"id": b.0})).unwrap();
    assert_eq!((state(&r), r["photo"].as_u64()), ("playing", Some(b.0)), "{r}");
    assert_eq!((log.kills(), rows(&s)), (1, 1), "the first clip's child is killed");
    assert_eq!(log.starts(), vec![0.0, 0.0], "the other clip starts at its start");

    let r = s.execute("video.stop", &json!({})).unwrap();
    assert_eq!((state(&r), r["photo"].is_null()), ("stopped", true), "{r}");
    assert_eq!((log.kills(), rows(&s)), (2, 0));
    assert!(s.video.frame().is_none(), "the loupe shows the poster again");
}

#[test]
fn without_the_sidecar_play_says_so_and_leaves_nothing_running() {
    let (mut s, _) = session_with_video();
    s.video_opener = Arc::new(|_: &StreamRequest| Err(VideoError::NotInstalled));
    let err = s.execute("video.play", &json!({})).unwrap_err();
    assert!(err.to_string().contains("Video support isn't installed"), "{err}");
    let r = s.execute("video.state", &json!({})).unwrap();
    assert_eq!((state(&r), r["unavailable"].as_bool(), r["streaming"].as_bool()), ("paused", Some(true), Some(false)), "{r}");
    assert_eq!(r["error"].as_str(), Some("Video support isn't installed"));
    assert_eq!(rows(&s), 0);
    assert_eq!(s.video.error().map(|e| e.unavailable), Some(true));
    // a seek says the same, and a working opener afterwards clears the error
    assert!(s.execute("video.seek", &json!({"t": 1.0})).is_err());
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    s.execute("video.play", &json!({})).unwrap();
    assert!(s.video.error().is_none());
    s.video_stop();
}

#[test]
fn a_stream_that_fails_leaves_the_clip_paused_with_the_reason() {
    let (mut s, _) = session_with_video();
    s.video_opener = Arc::new(|r: &StreamRequest| {
        let stderr: Option<Box<dyn Read + Send>> = Some(Box::new(Cursor::new(b"Invalid data found when processing input".to_vec())));
        FrameStream::from_parts(Box::new(Cursor::new(Vec::new())), stderr, Box::new(CountKill(Arc::default())), spec_at(r.start), 4, None)
    });
    let t0 = Instant::now();
    s.video_play(None, t0).unwrap();
    until("the failure", || {
        s.video_tick(t0);
        s.video.state() != PlayState::Playing
    });
    assert_eq!(s.video.state(), PlayState::Paused);
    let e = s.video.error().unwrap();
    assert!(e.message.contains("Invalid data") && !e.unavailable, "{e:?}");
    assert_eq!((rows(&s), s.video.is_streaming()), (0, false));
}

#[test]
fn the_activity_rows_stop_button_pauses_without_an_error() {
    let (mut s, _) = session_with_video();
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    let t0 = Instant::now();
    s.video_play(None, t0).unwrap();
    let row = s.activity.list().into_iter().find(|t| t.kind == "video").unwrap();
    assert!(row.cancellable, "the row has a stop button");
    s.activity.cancel(row.id).unwrap();
    until("the stream to end as cancelled", || {
        s.video_tick(t0);
        s.video.state() == PlayState::Paused
    });
    assert!(s.video.error().is_none(), "a stop is not a failure");
    assert_eq!((rows(&s), s.video.is_streaming(), log.kills()), (0, false, 1));
}

#[test]
fn only_a_video_plays_and_a_photo_is_needed() {
    let mut s = Session::new();
    assert!(s.execute("video.play", &json!({})).is_err(), "no photo");
    let mut demo = Session::with_demo();
    let still = demo.catalog.photos().next().unwrap().id;
    let err = demo.execute("video.play", &json!({"id": still.0})).unwrap_err();
    assert!(err.to_string().contains("not a video"), "{err}");
    assert!(demo.execute("video.play", &json!({"id": "x"})).is_err());
    assert!(demo.execute("video.play", &json!({"id": 999_999})).is_err());
    assert_eq!(demo.video.state(), PlayState::Stopped, "nothing was loaded");
    assert!(demo.execute("video.seek", &json!({"t": 1.0})).is_err(), "no clip to seek");
}

#[test]
fn playback_starts_inside_the_trim_and_stops_at_its_out_point() {
    let (mut s, _) = session_with_video();
    s.execute("video.setTrim", &json!({"in": 2.0, "out": 4.0})).unwrap();
    let log = Arc::new(Log::default());
    s.video_opener = short_clip(&log, 5);
    let t0 = Instant::now();
    s.video_play(None, t0).unwrap();
    assert_eq!(log.starts(), vec![2.0], "a trimmed clip plays from its in point");
    until("the stream to be read", || {
        s.video_tick(t0);
        s.video.buffered() == 4
    });
    assert_eq!(s.video.state(), PlayState::Playing);
    // 3 s on, the clock is past the out point at 4 s: the clip ends there and the child is let go
    assert!(s.video_tick(t0 + Duration::from_secs(3)));
    assert_eq!(s.video.state(), PlayState::Ended);
    assert_eq!(s.video.position_at(t0 + Duration::from_secs(9)), 4.0, "held at the out point");
    assert_eq!((s.video.is_streaming(), log.kills(), rows(&s)), (false, 1, 0));
    // playing it again starts from the in point
    s.video_play(None, t0).unwrap();
    assert_eq!(log.starts(), vec![2.0, 2.0]);
}

#[test]
fn a_seek_goes_outside_the_trim_but_play_comes_back_inside_it() {
    let (mut s, _) = session_with_video();
    s.execute("video.setTrim", &json!({"in": 2.0, "out": 4.0})).unwrap();
    let log = Arc::new(Log::default());
    s.video_opener = long_clip(&log);
    s.video_seek(None, 7.0).unwrap();
    s.video_seek(None, 0.5).unwrap();
    assert_eq!(log.starts(), vec![7.0, 0.5], "scrubbing is free, so a new in or out point can be found outside the trim");
    s.video_play(None, Instant::now()).unwrap();
    assert_eq!(log.starts().last().copied(), Some(2.0), "play from before the in point starts at it");
    s.video_pause(Instant::now());
    s.video_seek(None, 3.0).unwrap();
    s.video_play(None, Instant::now()).unwrap();
    assert_eq!(log.starts().last().copied(), Some(3.0), "inside the trim it goes on from where it is");
    s.video_pause(Instant::now());
    s.video_seek(None, 9.0).unwrap();
    s.video_play(None, Instant::now()).unwrap();
    assert_eq!(log.starts().last().copied(), Some(2.0), "past the out point it starts at the in point");
    s.video_stop();
}
