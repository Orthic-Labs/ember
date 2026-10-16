//! Clip export (issue #77): write a clip, cut to its trim, as a new file through the verified ffmpeg sidecar.
//!
//! Two ways to write it. **Stream copy** (the default) keeps the video and audio exactly as they are, so it is fast and loses
//! nothing; a cut can only start on a keyframe, so the file begins at the keyframe at or before the in point (the picture before the
//! in point is the part of a group of pictures the in point falls in) and ends at the first packet past the out point. **Re-encode**
//! decodes and encodes again to start on exactly the in point: H.264 through Media Foundation (`h264_mf`) on Windows and VideoToolbox
//! (`h264_videotoolbox`) on macOS, always with an explicit bit rate (Media Foundation with none writes 1080p at about 88 kbit/s).
//! The approved `decode` build of the sidecar links neither encoder, so a re-encode there fails with a message that says so; copy
//! needs no encoder.
//!
//! The arguments are built by [`clip_args`], which touches no process and takes the platform as a parameter, so what each
//! platform runs is tested everywhere. [`export_clip`] runs them under a deadline and a cancel flag ([`Limits`]) and removes a partial
//! file when the run fails or is cancelled.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::trim::{MIN_TRIM_SECS, Trim, kept_range};
use crate::{EXTENSIONS, Limits, VideoError, VideoInfo};

/// The audio bit rate of a re-encode (AAC, the sidecar's built-in encoder).
pub const AUDIO_KBPS: u32 = 160;
/// The lowest and highest video bit rate (kbit/s) a re-encode is asked for.
pub const MIN_VIDEO_KBPS: u32 = 1_000;
pub const MAX_VIDEO_KBPS: u32 = 100_000;
/// The bit rate of a re-encode of a clip whose size and rate are not known.
pub const FALLBACK_VIDEO_KBPS: u32 = 8_000;
/// Bits per pixel per frame a re-encode spends when no rate is given: 1080p at 30 frames per second comes to about 6 Mbit/s.
const BITS_PER_PIXEL: f64 = 0.1;

/// An H.264 encoder of the sidecar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// `h264_mf`, Windows Media Foundation.
    MediaFoundation,
    /// `h264_videotoolbox`, Apple VideoToolbox.
    VideoToolbox,
}

impl Encoder {
    /// The encoder `os` (`std::env::consts::OS`) has; `None` where the sidecar has none (Linux).
    pub fn for_os(os: &str) -> Option<Encoder> {
        match os {
            "windows" => Some(Encoder::MediaFoundation),
            "macos" => Some(Encoder::VideoToolbox),
            _ => None,
        }
    }

    /// The name ffmpeg knows it by.
    pub fn name(self) -> &'static str {
        match self {
            Encoder::MediaFoundation => "h264_mf",
            Encoder::VideoToolbox => "h264_videotoolbox",
        }
    }
}

/// What to export.
#[derive(Clone, Debug, Default)]
pub struct ClipRequest {
    pub trim: Trim,
    /// The clip's probe: its duration (to resolve the trim), size and rate (to choose a bit rate).
    pub info: Option<VideoInfo>,
    /// Decode and encode again instead of copying the streams.
    pub reencode: bool,
    /// The video bit rate of a re-encode (kbit/s, held to [`MIN_VIDEO_KBPS`]..=[`MAX_VIDEO_KBPS`]); `None` chooses one from the clip.
    pub video_kbps: Option<u32>,
    /// Write no container metadata (the location a phone embeds, the camera, the creation time): the clip's own metadata is copied
    /// otherwise. A clip's metadata is all or nothing here; the finer policies of still exports cannot be applied to it.
    pub strip_metadata: bool,
}

/// How the streams are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    Copy,
    Reencode { encoder: Encoder, video_kbps: u32, audio_kbps: u32 },
}

/// A resolved export: which part of which file, written how, to where.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipPlan {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Where the cut starts (seconds; 0 is the start of the clip).
    pub start: f64,
    /// How much is kept; `None` is everything after `start` (a clip whose duration is not known).
    pub length: Option<f64>,
    pub kind: ClipKind,
    pub strip_metadata: bool,
}

impl ClipPlan {
    pub fn is_copy(&self) -> bool {
        self.kind == ClipKind::Copy
    }
}

/// One ffmpeg argument: text, or a path kept as a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    Text(String),
    Path(PathBuf),
}

/// The extension an export of `source` is written with: a copy keeps the clip's container (its streams may not fit another), a
/// re-encode is MP4.
pub fn output_extension(source: &Path, reencode: bool) -> String {
    let ext = source.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext {
        Some(e) if !reencode && EXTENSIONS.contains(&e.as_str()) => e,
        _ => "mp4".into(),
    }
}

/// The video bit rate (kbit/s) a re-encode of `info` is asked for when none is given: [`BITS_PER_PIXEL`] bits per pixel per frame at
/// the clip's own size and rate, within [`MIN_VIDEO_KBPS`]..=[`MAX_VIDEO_KBPS`]; [`FALLBACK_VIDEO_KBPS`] when the clip was not probed.
pub fn suggested_video_kbps(info: Option<&VideoInfo>) -> u32 {
    let Some(info) = info else { return FALLBACK_VIDEO_KBPS };
    let (w, h) = info.display_size();
    let fps = info.frame_rate.filter(|f| f.is_finite() && *f > 0.0).unwrap_or(30.0).min(240.0);
    let kbps = f64::from(w) * f64::from(h) * fps * BITS_PER_PIXEL / 1000.0;
    (kbps.round() as u32).clamp(MIN_VIDEO_KBPS, MAX_VIDEO_KBPS)
}

/// Resolve `req` for the file `input` written to `output` on `os`. A trim that keeps nothing is refused, and so is a re-encode
/// where the sidecar has no encoder (Linux).
pub fn plan_clip(input: &Path, output: &Path, req: &ClipRequest, os: &str) -> Result<ClipPlan, VideoError> {
    let duration = req.info.as_ref().and_then(|i| i.duration);
    let (start, length) = kept_range(req.trim, duration);
    if let Some(length) = length
        && length < MIN_TRIM_SECS
    {
        return Err(VideoError::Invalid("the trim keeps nothing of the clip".into()));
    }
    let kind = if req.reencode {
        let encoder = Encoder::for_os(os).ok_or(VideoError::Unsupported)?;
        let video_kbps = req.video_kbps.map_or_else(|| suggested_video_kbps(req.info.as_ref()), |k| k.clamp(MIN_VIDEO_KBPS, MAX_VIDEO_KBPS));
        ClipKind::Reencode { encoder, video_kbps, audio_kbps: AUDIO_KBPS }
    } else {
        ClipKind::Copy
    };
    Ok(ClipPlan { input: input.to_path_buf(), output: output.to_path_buf(), start, length, kind, strip_metadata: req.strip_metadata })
}

fn texts(args: &mut Vec<Arg>, items: &[&str]) {
    args.extend(items.iter().map(|i| Arg::Text((*i).to_string())));
}

/// The ffmpeg arguments of `plan`.
///
/// `-ss` comes before `-i` (a seek in the input: to the keyframe at or before the point for a copy, to the exact frame for a
/// re-encode, which decodes up to it) and `-t` after it (how much to write). Only the first video stream and the audio streams are
/// mapped; subtitles, data streams and chapters (which no longer fit) are left out, the clip's metadata stays unless the plan strips
/// it (`-map_metadata -1`: no location, camera or creation time in the file). The input is read
/// through the `file` protocol only. `-progress pipe:1` reports the position for the activity row.
pub fn clip_args(plan: &ClipPlan) -> Vec<Arg> {
    let mut a = Vec::new();
    texts(&mut a, &["-hide_banner", "-nostdin", "-y", "-v", "error", "-nostats", "-protocol_whitelist", "file"]);
    if plan.start > 0.0 {
        texts(&mut a, &["-ss"]);
        a.push(Arg::Text(format!("{:.3}", plan.start)));
    }
    texts(&mut a, &["-i"]);
    a.push(Arg::Path(plan.input.clone()));
    if let Some(length) = plan.length {
        texts(&mut a, &["-t"]);
        a.push(Arg::Text(format!("{length:.3}")));
    }
    texts(&mut a, &["-map", "0:v:0", "-map", "0:a?", "-map_metadata", if plan.strip_metadata { "-1" } else { "0" }, "-map_chapters", "-1"]);
    match plan.kind {
        // avoid_negative_ts: the keyframe before the in point is written at time zero, so every player starts at the file's start
        ClipKind::Copy => texts(&mut a, &["-c", "copy", "-avoid_negative_ts", "make_zero"]),
        ClipKind::Reencode { encoder, video_kbps, audio_kbps } => {
            // even sides: the encoders refuse an odd width or height in 4:2:0
            texts(&mut a, &["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-c:v", encoder.name(), "-b:v"]);
            a.push(Arg::Text(format!("{video_kbps}k")));
            texts(&mut a, &["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a"]);
            a.push(Arg::Text(format!("{audio_kbps}k")));
        }
    }
    let ext = plan.output.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "mp4" | "m4v" | "mov") {
        texts(&mut a, &["-movflags", "+faststart"]);
    }
    texts(&mut a, &["-progress", "pipe:1", "-stats_period", "0.25"]);
    a.push(Arg::Path(plan.output.clone()));
    a
}

/// The limits an export of `plan` runs under: a deadline of ten times what it keeps (ten minutes at least, twelve hours at most;
/// an hour's worth when the length is not known) and `cancel`, which kills the child and its process tree.
pub fn export_limits(plan: &ClipPlan, cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Limits {
    let kept = plan.length.filter(|l| l.is_finite() && *l > 0.0).unwrap_or(3600.0);
    Limits { timeout: Duration::from_secs_f64((kept * 10.0).clamp(600.0, 43_200.0)), cancel }
}

/// What a failed run says: the end of ffmpeg's error output, and for an encoder the sidecar build does not have, why and what to do.
pub(crate) fn failure_message(kind: ClipKind, stderr: &str) -> String {
    if let ClipKind::Reencode { encoder, .. } = kind
        && (stderr.contains("Unknown encoder") || stderr.contains("Encoder not found") || stderr.contains("Unrecognized option"))
    {
        return format!(
            "this ffmpeg sidecar build has no H.264 encoder ({}), so the clip can't be re-encoded; export it as a stream copy instead",
            encoder.name()
        );
    }
    const MAX: usize = 600;
    let text = stderr.trim();
    if text.len() <= MAX {
        return text.to_string();
    }
    let mut start = text.len() - MAX;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.get(start..).unwrap_or_default().to_string()
}

/// Run `plan`: write `plan.output`. `progress` is told the fraction done (0 to 1) as ffmpeg reports its position, when the length is
/// known. A run that fails, times out or is cancelled leaves no file; one that ends without writing one is an error.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn export_clip(plan: &ClipPlan, limits: &Limits, progress: &mut dyn FnMut(f64)) -> Result<(), VideoError> {
    use rightkit_ffmpeg::runner::{ProgressEvent, run_with_progress};
    use rightkit_ffmpeg::{CancellationToken, FfmpegCommand, RunError, RunOptions};

    let ffmpeg = crate::sidecar::ffmpeg()?;
    // absolute, so neither path can be read as an option
    let abs = |p: &Path| std::path::absolute(p).map_err(|e| VideoError::Invalid(format!("{}: {e}", p.display())));
    let plan = ClipPlan { input: abs(&plan.input)?, output: abs(&plan.output)?, ..plan.clone() };
    let mut cmd = FfmpegCommand::new();
    for arg in clip_args(&plan) {
        match arg {
            Arg::Text(t) => cmd.text(t),
            Arg::Path(p) => cmd.path(p),
        };
    }
    let total_ms = plan.length.map(|l| l * 1000.0).filter(|t| *t > 0.0);
    let mut on_progress = |e: &ProgressEvent| {
        if let Some(total) = total_ms {
            progress((e.out_time_ms as f64 / total).clamp(0.0, 1.0));
        }
    };
    let token = limits.cancel.as_ref().map_or_else(CancellationToken::new, |flag| CancellationToken::from_flag(flag.clone()));
    let opts = RunOptions::new(limits.timeout, token).with_progress(&mut on_progress);
    let result = run_with_progress(&ffmpeg, &cmd, opts).map_err(|e| match e {
        RunError::TimedOut(after) => VideoError::TimedOut(after),
        RunError::Cancelled => VideoError::Cancelled,
        RunError::Failed { stderr_tail, .. } => VideoError::Failed(failure_message(plan.kind, &stderr_tail)),
        other => VideoError::Failed(failure_message(plan.kind, &other.to_string())),
    });
    let written = std::fs::metadata(&plan.output).map(|m| m.len()).unwrap_or(0);
    match result {
        Ok(_) if written > 0 => Ok(()),
        Ok(_) => {
            let _ = std::fs::remove_file(&plan.output);
            Err(VideoError::Failed("ffmpeg wrote no file".into()))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&plan.output);
            Err(e)
        }
    }
}

/// Clip export needs the sidecar, which Linux and the web ship without.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn export_clip(_plan: &ClipPlan, _limits: &Limits, _progress: &mut dyn FnMut(f64)) -> Result<(), VideoError> {
    Err(VideoError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: u32, h: u32, fps: Option<f64>, duration: Option<f64>) -> VideoInfo {
        VideoInfo { duration, width: w, height: h, rotation: 0, codec: "h264".into(), frame_rate: fps, creation_time: None }
    }

    fn text(args: &[Arg]) -> Vec<String> {
        args.iter()
            .map(|a| match a {
                Arg::Text(t) => t.clone(),
                Arg::Path(p) => format!("<{}>", p.display()),
            })
            .collect()
    }

    fn at(args: &[String], flag: &str) -> Option<usize> {
        args.iter().position(|a| a == flag)
    }

    fn request(start: Option<f64>, end: Option<f64>, reencode: bool) -> ClipRequest {
        ClipRequest {
            trim: Trim { start, end },
            info: Some(info(1920, 1080, Some(30.0), Some(60.0))),
            reencode,
            video_kbps: None,
            strip_metadata: false,
        }
    }

    fn plan(req: &ClipRequest, os: &str) -> ClipPlan {
        plan_clip(Path::new("in.mov"), Path::new("out.mp4"), req, os).unwrap()
    }

    #[test]
    fn a_copy_seeks_before_the_input_limits_after_it_and_changes_no_stream() {
        let p = plan(&request(Some(2.5), Some(8.5), false), "windows");
        assert_eq!((p.start, p.length), (2.5, Some(6.0)));
        assert!(p.is_copy());
        let a = text(&clip_args(&p));
        let (ss, i, t, c) = (at(&a, "-ss").unwrap(), at(&a, "-i").unwrap(), at(&a, "-t").unwrap(), at(&a, "-c").unwrap());
        assert!(ss < i && i < t, "-ss is an input option, -t an output option: {a:?}");
        assert_eq!((a[ss + 1].as_str(), a[t + 1].as_str()), ("2.500", "6.000"));
        assert_eq!(a[i + 1], "<in.mov>");
        assert_eq!(a[c + 1], "copy");
        assert!(at(&a, "-c:v").is_none() && at(&a, "-b:v").is_none(), "nothing is encoded: {a:?}");
        assert_eq!(a[at(&a, "-avoid_negative_ts").unwrap() + 1], "make_zero");
        assert_eq!(a.last().map(String::as_str), Some("<out.mp4>"), "the output is the last argument");
        assert_eq!(a[at(&a, "-protocol_whitelist").unwrap() + 1], "file", "the input is read through the file protocol only");
    }

    #[test]
    fn an_untrimmed_copy_has_no_seek_and_no_limit() {
        let p = plan(&request(None, None, false), "macos");
        assert_eq!((p.start, p.length), (0.0, Some(60.0)), "the whole clip");
        let a = text(&clip_args(&p));
        assert!(at(&a, "-ss").is_none(), "{a:?}");
        // the length is still stated: a copy of the whole clip is bounded the same way
        assert_eq!(a[at(&a, "-t").unwrap() + 1], "60.000");
        let unknown = ClipRequest { info: None, ..request(None, None, false) };
        let a = text(&clip_args(&plan(&unknown, "macos")));
        assert!(at(&a, "-ss").is_none() && at(&a, "-t").is_none(), "no duration and no trim: nothing to cut: {a:?}");
    }

    #[test]
    fn an_in_point_alone_runs_to_the_end_and_an_out_point_alone_from_the_start() {
        let p = plan(&request(Some(10.0), None, false), "windows");
        assert_eq!((p.start, p.length), (10.0, Some(50.0)));
        let p = plan(&request(None, Some(4.0), false), "windows");
        assert_eq!((p.start, p.length), (0.0, Some(4.0)));
        let a = text(&clip_args(&p));
        assert!(at(&a, "-ss").is_none() && a[at(&a, "-t").unwrap() + 1] == "4.000", "{a:?}");
    }

    #[test]
    fn a_reencode_on_windows_is_media_foundation_with_an_explicit_rate() {
        let p = plan(&request(Some(1.0), Some(3.0), true), "windows");
        assert_eq!(p.kind, ClipKind::Reencode { encoder: Encoder::MediaFoundation, video_kbps: 6221, audio_kbps: AUDIO_KBPS });
        let a = text(&clip_args(&p));
        assert_eq!(a[at(&a, "-c:v").unwrap() + 1], "h264_mf");
        assert_eq!(a[at(&a, "-b:v").unwrap() + 1], "6221k", "Media Foundation with no rate writes about 88 kbit/s");
        assert_eq!(a[at(&a, "-pix_fmt").unwrap() + 1], "yuv420p");
        assert_eq!((a[at(&a, "-c:a").unwrap() + 1].as_str(), a[at(&a, "-b:a").unwrap() + 1].as_str()), ("aac", "160k"));
        assert!(at(&a, "-c").is_none() && at(&a, "-avoid_negative_ts").is_none(), "no stream copy: {a:?}");
        assert_eq!(a[at(&a, "-vf").unwrap() + 1], "scale=trunc(iw/2)*2:trunc(ih/2)*2", "even sides");
        assert!(at(&a, "-ss").unwrap() < at(&a, "-i").unwrap(), "the seek still comes first (it decodes up to the exact frame)");
    }

    #[test]
    fn a_reencode_on_macos_is_videotoolbox() {
        let req = ClipRequest { video_kbps: Some(12_000), ..request(None, Some(5.0), true) };
        let p = plan(&req, "macos");
        assert_eq!(p.kind, ClipKind::Reencode { encoder: Encoder::VideoToolbox, video_kbps: 12_000, audio_kbps: AUDIO_KBPS });
        let a = text(&clip_args(&p));
        assert_eq!((a[at(&a, "-c:v").unwrap() + 1].as_str(), a[at(&a, "-b:v").unwrap() + 1].as_str()), ("h264_videotoolbox", "12000k"));
    }

    #[test]
    fn a_reencode_where_the_sidecar_has_no_encoder_is_refused() {
        for os in ["linux", "freebsd", ""] {
            let err = plan_clip(Path::new("in.mov"), Path::new("out.mp4"), &request(None, None, true), os).unwrap_err();
            assert!(matches!(err, VideoError::Unsupported), "{os}: {err:?}");
        }
        assert!(plan_clip(Path::new("in.mov"), Path::new("out.mov"), &request(None, None, false), "linux").is_ok(), "a copy needs no encoder");
    }

    #[test]
    fn the_rate_follows_the_clip_and_is_held_to_its_bounds() {
        assert_eq!(suggested_video_kbps(Some(&info(1920, 1080, Some(30.0), None))), 6221);
        assert_eq!(suggested_video_kbps(Some(&info(3840, 2160, Some(60.0), None))), 49_766);
        assert_eq!(suggested_video_kbps(Some(&info(320, 240, Some(30.0), None))), MIN_VIDEO_KBPS);
        assert_eq!(suggested_video_kbps(Some(&info(8192, 8192, Some(240.0), None))), MAX_VIDEO_KBPS);
        assert_eq!(suggested_video_kbps(Some(&info(1920, 1080, None, None))), 6221, "30 frames per second when the rate is not known");
        assert_eq!(suggested_video_kbps(None), FALLBACK_VIDEO_KBPS);
        let rotated = VideoInfo { rotation: 90, ..info(1080, 1920, Some(30.0), None) };
        assert_eq!(suggested_video_kbps(Some(&rotated)), 6221);
        let low = ClipRequest { video_kbps: Some(1), ..request(None, None, true) };
        let high = ClipRequest { video_kbps: Some(u32::MAX), ..request(None, None, true) };
        let kbps = |r: &ClipRequest| match plan(r, "windows").kind {
            ClipKind::Reencode { video_kbps, .. } => video_kbps,
            ClipKind::Copy => 0,
        };
        assert_eq!((kbps(&low), kbps(&high)), (MIN_VIDEO_KBPS, MAX_VIDEO_KBPS));
    }

    #[test]
    fn a_trim_that_keeps_nothing_is_refused() {
        let req = request(Some(30.0), Some(30.01), false);
        let err = plan_clip(Path::new("in.mov"), Path::new("out.mov"), &req, "windows").unwrap_err();
        assert!(matches!(err, VideoError::Invalid(_)), "{err:?}");
        let gone = request(Some(59.99), None, false);
        assert!(plan_clip(Path::new("in.mov"), Path::new("out.mov"), &gone, "windows").is_err(), "less than the least before the end");
    }

    #[test]
    fn the_container_is_kept_for_a_copy_and_mp4_for_a_reencode() {
        assert_eq!(output_extension(Path::new("/a/Clip.MOV"), false), "mov");
        assert_eq!(output_extension(Path::new("a.mkv"), false), "mkv");
        assert_eq!(output_extension(Path::new("a.m2ts"), false), "m2ts");
        assert_eq!(output_extension(Path::new("a.mkv"), true), "mp4");
        assert_eq!(output_extension(Path::new("a.txt"), false), "mp4", "not a video extension");
        assert_eq!(output_extension(Path::new("noext"), false), "mp4");
    }

    #[test]
    fn mp4_and_mov_outputs_get_the_index_up_front_others_do_not() {
        let with = |out: &str| {
            let p = ClipPlan { output: PathBuf::from(out), ..plan(&request(None, None, false), "windows") };
            text(&clip_args(&p))
        };
        for out in ["a.mp4", "a.MOV", "a.m4v"] {
            assert!(at(&with(out), "-movflags").is_some(), "{out}");
        }
        for out in ["a.mkv", "a.webm", "a.avi", "a.m2ts"] {
            assert!(at(&with(out), "-movflags").is_none(), "{out}");
        }
    }

    #[test]
    fn only_the_first_video_and_the_audio_are_mapped_and_progress_is_reported() {
        let a = text(&clip_args(&plan(&request(Some(1.0), Some(3.0), false), "macos")));
        let maps: Vec<&str> = a.windows(2).filter(|w| w[0] == "-map").map(|w| w[1].as_str()).collect();
        assert_eq!(maps, ["0:v:0", "0:a?"], "no subtitles or data streams; audio only when there is some");
        assert_eq!(a[at(&a, "-map_chapters").unwrap() + 1], "-1", "chapters of the whole clip do not fit a part of it");
        assert_eq!(a[at(&a, "-progress").unwrap() + 1], "pipe:1");
        assert!(at(&a, "-nostdin").is_some() && at(&a, "-y").is_some());
    }

    #[test]
    fn a_clip_keeps_its_metadata_unless_asked_to_drop_it() {
        for reencode in [false, true] {
            let kept = text(&clip_args(&plan(&request(None, None, reencode), "windows")));
            assert_eq!(kept[at(&kept, "-map_metadata").unwrap() + 1], "0", "reencode {reencode}");
            let req = ClipRequest { strip_metadata: true, ..request(None, None, reencode) };
            let stripped = text(&clip_args(&plan(&req, "windows")));
            assert_eq!(stripped[at(&stripped, "-map_metadata").unwrap() + 1], "-1", "no location or camera survives: reencode {reencode}");
        }
    }

    #[test]
    fn a_missing_encoder_is_explained_and_other_failures_keep_their_tail() {
        let kind = ClipKind::Reencode { encoder: Encoder::MediaFoundation, video_kbps: 6000, audio_kbps: AUDIO_KBPS };
        let m = failure_message(kind, "Unknown encoder 'h264_mf'\n");
        assert!(m.contains("no H.264 encoder (h264_mf)") && m.contains("stream copy"), "{m}");
        // a copy never asks for an encoder: the same words from ffmpeg are just its error
        assert!(!failure_message(ClipKind::Copy, "Unknown encoder 'x'").contains("sidecar build"));
        assert_eq!(failure_message(ClipKind::Copy, "  Invalid data found when processing input\n"), "Invalid data found when processing input");
        let long = format!("{}é{}", "x".repeat(700), "tail");
        let m = failure_message(ClipKind::Copy, &long);
        assert!(m.len() <= 601 && m.ends_with("tail"), "{}", m.len());
    }

    #[test]
    fn the_deadline_scales_with_what_is_kept_and_has_bounds() {
        let p = |length: Option<f64>| ClipPlan { length, ..plan(&request(None, None, false), "windows") };
        assert_eq!(export_limits(&p(Some(5.0)), None).timeout, Duration::from_secs(600));
        assert_eq!(export_limits(&p(Some(300.0)), None).timeout, Duration::from_secs(3000));
        assert_eq!(export_limits(&p(Some(1.0e9)), None).timeout, Duration::from_secs(43_200));
        assert_eq!(export_limits(&p(None), None).timeout, Duration::from_secs(36_000), "an hour's worth when the length is unknown");
        assert!(export_limits(&p(Some(5.0)), None).cancel.is_none());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn export_is_unsupported_off_windows_and_macos() {
        let p = plan(&request(None, None, false), "linux");
        let err = export_clip(&p, &export_limits(&p, None), &mut |_| {}).unwrap_err();
        assert!(matches!(err, VideoError::Unsupported), "{err:?}");
    }
}
