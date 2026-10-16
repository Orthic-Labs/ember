//! Silent playback of a video in the loupe (issue #73): the playback state, and the commands' work.
//!
//! The decoding is `lightcraft-video`'s ([`FrameStream`]: `ffmpeg` as a child process, frames on a bounded queue, the child killed
//! when the stream is dropped). This is the engine's side: which clip, where it is, whether it plays, and which frame is
//! on screen. The commands are `video.play`, `video.pause`, `video.seek`, `video.togglePlay`, `video.stop` and the query
//! `video.state` (`cmd/video.rs`); the desktop UI calls them and, once per UI frame, [`Session::video_tick_now`], which moves the
//! queued frames onto the screen at the time of their pts.
//!
//! * **One stream at a time**, held while the clip plays. Pausing, seeking, switching clips, an error and the end of the clip
//!   all drop it, which kills the child. A running stream holds an activity-stack row ("Playing Video") whose ✕ stops it.
//! * **Time** is the stream's: a frame belongs at `start + index / fps`. The clock starts when the first frame arrives (the
//!   child takes a moment to start), so a slow start does not make the first frames late; after that `position` is the
//!   clock, never the frame on screen. A frame is shown when the clock reaches its time; frames the clock has passed are skipped.
//! * **A seek while paused** opens a stream that stops after its first frame, so the scrubbed-to picture shows without playing.
//! * **Errors** never panic: a stream that fails leaves the clip paused with the reason in [`Player::error`]; a missing sidecar
//!   says "Video support isn't installed".

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lightcraft_catalog::{MediaKind, PhotoId, Source};
use lightcraft_video::{Frame, FrameStream, Limits, Poll, VideoError};
use serde::Serialize;
use serde_json::{Value, json};
use web_time::Instant;

use crate::Session;
use crate::activity::Cancel;

/// The long edge of a played frame (it is never enlarged).
pub const PLAY_EDGE: usize = 1280;
/// The frame rate a clip is played at at most.
pub const PLAY_FPS_CAP: f64 = 30.0;
/// Frames held between the stream's queue and the screen; more is a consumer that is not keeping up, and the oldest go.
const MAX_PENDING: usize = 6;
/// Playback never starts closer to the end than this (seconds): a seek to the very end finds no frame.
const END_MARGIN_SECS: f64 = 0.05;
/// What the user is told when the ffmpeg sidecar is not there.
pub const NOT_INSTALLED: &str = "Video support isn't installed";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayState {
    /// No clip is loaded: the loupe shows the poster.
    #[default]
    Stopped,
    Playing,
    /// A clip is loaded and not running, at `position` (with the frame there, when there is one).
    Paused,
    /// The clip played to its end.
    Ended,
}

/// Why playback could not start or went on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayError {
    pub message: String,
    /// The sidecar is not installed here (the UI says so instead of showing an error).
    pub unavailable: bool,
}

impl PlayError {
    fn of(e: &VideoError, path: &str) -> PlayError {
        if e.is_unavailable() {
            PlayError { message: NOT_INSTALLED.into(), unavailable: true }
        } else {
            PlayError { message: format!("{path}: {e}"), unavailable: false }
        }
    }
}

/// What the engine asks the stream opener for.
#[derive(Clone, Debug)]
pub struct StreamRequest {
    pub path: String,
    pub start: f64,
    /// The clip's size as shown, and its frame rate, as catalogued at import.
    pub shown: (u32, u32),
    pub frame_rate: Option<f64>,
    pub max_size: usize,
    pub fps_cap: Option<f64>,
    /// Set by the activity row's ✕: ends the stream as cancelled.
    pub cancel: Arc<AtomicBool>,
}

/// Opens a [`FrameStream`] for a request. The session's is the real one ([`real_opener`]); tests put their own in
/// [`Session::video_opener`].
pub type OpenStream = Arc<dyn Fn(&StreamRequest) -> Result<FrameStream, VideoError> + Send + Sync>;

/// Whether the ffmpeg sidecar can be run here (tests that need a real clip, or none, ask).
pub fn sidecar_available() -> bool {
    lightcraft_video::available().is_ok()
}

/// The opener that runs the ffmpeg sidecar.
pub fn real_opener() -> OpenStream {
    Arc::new(|r: &StreamRequest| {
        let limits = Limits { cancel: Some(r.cancel.clone()), ..Limits::default() };
        lightcraft_video::stream_frames_sized(Path::new(&r.path), r.start, r.shown, r.frame_rate, r.max_size, r.fps_cap, &limits)
    })
}

/// The playback state of the session's one player.
#[derive(Debug, Default)]
pub struct Player {
    photo: Option<PhotoId>,
    state: PlayState,
    duration: Option<f64>,
    /// Seconds: where the clip is while it is not running, and where the running clock started.
    position: f64,
    /// The clock of a running stream: the instant its first frame arrived and that frame's time.
    anchor: Option<(Instant, f64)>,
    stream: Option<FrameStream>,
    task: Option<crate::activity::TaskGuard>,
    /// Frames taken off the stream and not yet due, oldest first.
    pending: VecDeque<Frame>,
    /// How the running stream ended, until it is acted on (once its frames are shown).
    ended: Option<Result<(), VideoError>>,
    frame: Option<Arc<Frame>>,
    frame_photo: Option<PhotoId>,
    frame_seq: u64,
    /// The stream stops after its first frame (a scrub while paused).
    still: bool,
    /// Where a playing stream stops: the out point of the clip's trim, when it has one.
    stop_at: Option<f64>,
    error: Option<PlayError>,
    fps: f64,
}

impl Player {
    /// The clip loaded (playing, paused or ended), if any.
    pub fn photo(&self) -> Option<PhotoId> {
        self.photo
    }

    pub fn state(&self) -> PlayState {
        self.state
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub fn error(&self) -> Option<&PlayError> {
        self.error.as_ref()
    }

    /// Whether a stream is running (playing, or fetching the frame of a scrub).
    pub fn is_streaming(&self) -> bool {
        self.stream.is_some()
    }

    /// The frame on screen and the photo it belongs to; it changes whenever [`Player::frame_seq`] does.
    pub fn frame(&self) -> Option<(PhotoId, &Arc<Frame>)> {
        Some((self.frame_photo?, self.frame.as_ref()?))
    }

    /// Counts the frames put on screen: a new number means a new picture to upload.
    pub fn frame_seq(&self) -> u64 {
        self.frame_seq
    }

    /// Frames the stream had to drop because the consumer was behind.
    pub fn dropped(&self) -> u64 {
        self.stream.as_ref().map_or(0, FrameStream::dropped)
    }

    /// Frames taken off the stream and waiting for their time.
    #[cfg(test)]
    pub(crate) fn buffered(&self) -> usize {
        self.pending.len()
    }

    /// The frame rate of the running stream (or the last one).
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Where the clip is at `now`, in seconds, kept inside it.
    pub fn position_at(&self, now: Instant) -> f64 {
        let t = match (self.state, self.anchor) {
            (PlayState::Playing, Some((at, pts))) => pts + now.saturating_duration_since(at).as_secs_f64(),
            _ => self.position,
        };
        let t = match (self.state, self.stop_at) {
            (PlayState::Playing, Some(out)) => t.min(out),
            _ => t,
        };
        self.clamp(t)
    }

    fn clamp(&self, t: f64) -> f64 {
        let t = if t.is_finite() && t > 0.0 { t } else { 0.0 };
        match self.duration {
            Some(d) => t.min(d),
            None => t,
        }
    }

    /// Where playback may start: inside the clip, short of its end.
    fn start_point(&self, t: f64) -> f64 {
        let t = self.clamp(t);
        match self.duration {
            Some(d) => t.min((d - END_MARGIN_SECS).max(0.0)),
            None => t,
        }
    }

    /// Drop the running stream (which kills the child), its activity row and what it had queued.
    fn close_stream(&mut self) {
        self.stream = None;
        self.task = None;
        self.anchor = None;
        self.pending.clear();
        self.ended = None;
        self.still = false;
        self.stop_at = None;
    }

    fn present(&mut self, frame: Frame) {
        self.frame = Some(Arc::new(frame));
        self.frame_photo = self.photo;
        self.frame_seq = self.frame_seq.wrapping_add(1);
    }
}

fn state_json(p: &Player, now: Instant) -> Value {
    json!({
        "photo": p.photo.map(|id| id.0),
        "state": p.state,
        "position": p.position_at(now),
        "duration": p.duration,
        "streaming": p.is_streaming(),
        "dropped": p.dropped(),
        "error": p.error.as_ref().map(|e| e.message.clone()),
        "unavailable": p.error.as_ref().is_some_and(|e| e.unavailable),
    })
}

impl Session {
    /// The video `id` (default: the active photo): it must be a video.
    fn video_target(&self, id: Option<PhotoId>) -> Result<PhotoId, String> {
        let id = id.or_else(|| self.active()).ok_or("no photo selected")?;
        match self.catalog.photo(id) {
            Some(p) if p.kind == MediaKind::Video => Ok(id),
            Some(_) => Err("the photo is not a video".into()),
            None => Err("no such photo".into()),
        }
    }

    /// Where the trim of clip `id` starts and ends (seconds): the clip's own ends where it has no trim point, the end `None`
    /// when that is not known.
    fn video_kept_range(&self, id: PhotoId) -> (f64, Option<f64>) {
        let Some(p) = self.catalog.photo(id) else { return (0.0, None) };
        let trim = lightcraft_video::Trim { start: p.develop.trim_in, end: p.develop.trim_out };
        let (from, len) = lightcraft_video::kept_range(trim, p.duration);
        (from, len.map(|l| from + l))
    }

    /// Load `id` as the player's clip if it is not already (stopping what plays), at its start.
    fn video_load(&mut self, id: PhotoId) {
        if self.video.photo == Some(id) {
            return;
        }
        let duration = self.catalog.photo(id).and_then(|p| p.duration).filter(|d| d.is_finite() && *d > 0.0);
        self.video.close_stream();
        self.video = Player { photo: Some(id), state: PlayState::Paused, duration, ..Player::default() };
    }

    /// Start a stream of the loaded clip at `start`; `still` makes it stop after its first frame. On failure the clip is left
    /// paused at `start` with the reason kept (and returned).
    fn video_open(&mut self, start: f64, still: bool) -> Result<(), String> {
        let id = self.video.photo.ok_or("no video is loaded")?;
        let (path, shown, frame_rate, stop_at) = match self.catalog.photo(id) {
            Some(p) => match &p.source {
                Source::File { path } => (path.clone(), (p.width, p.height), p.video.as_ref().and_then(|v| v.frame_rate), p.develop.trim_out),
                Source::Demo { .. } => return Err("the video has no file".into()),
            },
            None => return Err("no such photo".into()),
        };
        let start = self.video.start_point(start);
        self.video.close_stream();
        self.video.position = start;
        let cancel = Arc::new(AtomicBool::new(false));
        let request =
            StreamRequest { path: path.clone(), start, shown, frame_rate, max_size: PLAY_EDGE, fps_cap: Some(PLAY_FPS_CAP), cancel: cancel.clone() };
        match (self.video_opener)(&request) {
            Ok(stream) => {
                let p = &mut self.video;
                p.fps = stream.spec().fps;
                p.stream = Some(stream);
                p.task = Some(self.activity.start("video", "Playing Video", Cancel::Flag(cancel)));
                p.still = still;
                p.stop_at = if still { None } else { stop_at };
                p.error = None;
                p.state = if still { PlayState::Paused } else { PlayState::Playing };
                Ok(())
            }
            Err(e) => {
                let error = PlayError::of(&e, &path);
                let message = error.message.clone();
                self.video.error = Some(error);
                self.video.state = PlayState::Paused;
                Err(message)
            }
        }
    }

    /// `video.play`: play `id` (default: the active photo) from where it is, or from its start when it is another clip or ended.
    pub fn video_play(&mut self, id: Option<PhotoId>, now: Instant) -> Result<(), String> {
        let id = self.video_target(id)?;
        self.video_load(id);
        if self.video.state == PlayState::Playing && self.video.stream.is_some() {
            return Ok(());
        }
        // playback is held to the clip's trim: from the in point when the clip is before it, at its end, or ended
        let (from, out) = self.video_kept_range(id);
        let p = &self.video;
        let at_end = p.state == PlayState::Ended || out.is_some_and(|end| p.position >= end - END_MARGIN_SECS);
        let start = if at_end || p.position < from { from } else { p.position_at(now) };
        self.video_open(start, false)
    }

    /// `video.pause`: stop where the clip is. Nothing to do when it is not playing.
    pub fn video_pause(&mut self, now: Instant) {
        let p = &mut self.video;
        if p.state != PlayState::Playing {
            // a scrub still fetching its frame is let go; the clip stays where it was put
            if p.still {
                p.close_stream();
            }
            return;
        }
        p.position = p.position_at(now);
        p.close_stream();
        p.state = PlayState::Paused;
    }

    /// `video.seek`: move the clip to `t` seconds (held inside it). Playing continues from there; paused, the frame at `t` is
    /// fetched and shown. Seeking a clip other than the loaded one loads it (the active photo, when `id` is none).
    pub fn video_seek(&mut self, id: Option<PhotoId>, t: f64) -> Result<(), String> {
        if !t.is_finite() || t < 0.0 {
            return Err("`t` must be zero or more seconds".into());
        }
        let id = match (id, self.video.photo) {
            (None, Some(loaded)) => loaded,
            (id, _) => self.video_target(id)?,
        };
        self.video_load(id);
        let playing = self.video.state == PlayState::Playing;
        let t = self.video.start_point(t);
        self.video.position = t;
        self.video_open(t, !playing)
    }

    /// `video.stop`: forget the clip; the loupe shows its poster again.
    pub fn video_stop(&mut self) {
        self.video.close_stream();
        self.video = Player::default();
    }

    /// The state as the commands report it.
    pub fn video_state_json(&self, now: Instant) -> Value {
        state_json(&self.video, now)
    }

    /// Where the loaded clip is now, in seconds (0 when none is).
    pub fn video_position_now(&self) -> f64 {
        self.video.position_at(Instant::now())
    }

    /// [`Session::video_tick`] at the present time.
    pub fn video_tick_now(&mut self) -> bool {
        self.video_tick(Instant::now())
    }

    /// Move what the stream has produced onto the screen as the clock reaches it, and act on its end. Call once per UI frame while
    /// [`Player::is_streaming`]. Returns whether the picture or the state changed.
    pub fn video_tick(&mut self, now: Instant) -> bool {
        let p = &mut self.video;
        let Some(stream) = p.stream.as_mut() else { return false };
        let before = (p.frame_seq, p.state);
        loop {
            match stream.poll() {
                Poll::Frame(f) => {
                    if p.pending.len() >= MAX_PENDING {
                        p.pending.pop_front();
                    }
                    p.pending.push_back(f);
                }
                Poll::Empty => break,
                Poll::Ended(r) => {
                    if p.ended.is_none() {
                        p.ended = Some(r);
                    }
                    break;
                }
            }
        }
        if p.still {
            if let Some(f) = p.pending.pop_front() {
                p.position = f.pts;
                p.present(f);
                p.close_stream();
                return (p.frame_seq, p.state) != before;
            }
        } else {
            if p.anchor.is_none()
                && let Some(first) = p.pending.front()
            {
                // the clock starts with the first picture, not with the request: the child took a while to start
                p.anchor = Some((now, first.pts));
            }
            if p.anchor.is_some() {
                let due = p.position_at(now) + 0.5 / p.fps.max(1.0);
                let mut latest = None;
                while p.pending.front().is_some_and(|f| f.pts <= due) {
                    latest = p.pending.pop_front();
                }
                if let Some(f) = latest {
                    p.present(f);
                }
            }
        }
        // a trimmed clip stops at its out point
        if !p.still
            && p.state == PlayState::Playing
            && let Some(out) = p.stop_at
            && p.position_at(now) >= out
        {
            p.close_stream();
            p.position = out;
            p.state = PlayState::Ended;
            return (p.frame_seq, p.state) != before;
        }
        // the end of the clip waits for its last frames to be shown; a failure or a stop does not
        let end_now = match &p.ended {
            Some(Ok(())) | Some(Err(VideoError::NoFrame)) => p.pending.is_empty(),
            Some(Err(_)) => true,
            None => false,
        };
        if end_now && let Some(end) = p.ended.take() {
            let at = p.position_at(now);
            let was_still = p.still;
            p.close_stream();
            match end {
                Err(VideoError::Cancelled) => {
                    p.position = at;
                    p.state = PlayState::Paused;
                }
                Ok(()) | Err(VideoError::NoFrame) if was_still => p.state = PlayState::Paused,
                Ok(()) | Err(VideoError::NoFrame) => {
                    p.position = p.duration.unwrap_or(at);
                    p.state = PlayState::Ended;
                }
                Err(e) => {
                    let name = p.photo.and_then(|id| self.catalog.photo(id)).map(|ph| ph.file_name.clone()).unwrap_or_default();
                    let p = &mut self.video;
                    p.position = at;
                    p.state = PlayState::Paused;
                    p.error = Some(PlayError::of(&e, &name));
                }
            }
        }
        (self.video.frame_seq, self.video.state) != before
    }
}
