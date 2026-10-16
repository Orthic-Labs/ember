# Video

*Last updated: 2026-10-17 (foundation: probe and poster frame; trim; clip and frame export; silent playback; poster chooser; issues #73, #74, #76, #77).*

Ember catalogues videos, shows a poster frame for each in the grid, the loupe and exports, lets you choose which frame that is, and
plays a clip silently in the loupe. Audio comes next (#73); this page records what exists and how it is built.

## How it works

Ember decodes no video itself. It runs the **FFmpeg and FFprobe programs bundled with the release** as child processes, through
[`rightkit-ffmpeg`](https://github.com/Orthic-Labs/rightkit) (RightKit's owner of the approved-binary catalog and the process
runner), behind the `lightcraft-video` crate (layer L1):

| Call | What it does |
| --- | --- |
| `probe(path) -> VideoInfo` | duration, size, display rotation, codec, frame rate and creation time of the first video stream (`ffprobe` JSON) |
| `frame_at(path, t, max_size) -> Rgba8` | one upright frame at `t` seconds, at most `max_size` along its long edge (never enlarged, never above 4096) |
| `stream_frames(path, start_t, max_size, fps_cap) -> FrameStream` | the clip's frames from `start_t` as raw RGBA at its own rate (at most `fps_cap`), in real time, on a bounded queue; dropping the stream kills the child |
| `available()` | whether the sidecar is found and verified here |

A binary runs only after `rightkit-ffmpeg` has matched its content SHA-256 to the approved catalog row for this platform and
checked that its `-version` banner shows an LGPL build (`--disable-gpl --disable-nonfree --disable-autodetect`). There is no
development fallback: a build that is not in the catalog is refused (`VideoError::Rejected`), never run.

Media files are hostile input:

- every child process has a deadline (30 s by default) and a cancel flag, and is killed with its whole process tree when either fires;
- the input is read through the `file` protocol only (`-protocol_whitelist file`), so a crafted playlist inside a "video" can name no URL
  and no other file; the path is made absolute so it can never be read as an option;
- the frame is requested at an exact size, so the output is bounded by that size whatever the clip claims (plus an output size cap);
  `ffprobe`'s output is refused above 4 MiB, and absurd sizes in a probe are refused;
- nothing panics: every failure is a `VideoError`.

## Platforms

| Platform | Video |
| --- | --- |
| Windows (x86_64) | yes, with the bundled sidecar |
| macOS (Apple silicon, Intel) | yes, with the bundled sidecar |
| **Linux** | **ships without video for now.** The crate compiles to an "unsupported" error (`VideoError::Unsupported`); no sidecar is bundled and none is looked for. |
| Web (wasm32) | no (no processes); same "unsupported" error |

### Where the sidecar is looked for

Release bundles put `ffmpeg` and `ffprobe` (`.exe` on Windows) beside the application. In order: the folder named by
`LIGHTCRAFT_FFMPEG_DIR` (a place to look, not a bypass: the files are verified wherever they are), the application's own folder, an
`ffmpeg` folder there, and the macOS bundle's `Resources` (and `Resources/ffmpeg`). A found pair is verified once per process; a miss is
looked up again on the next call, so installing the sidecar needs no restart.

Where it is not bundled (a development checkout, CI), video features return "video support not installed" and **import still adds the
file**, at 1280x720 with no duration, with a placeholder thumbnail (dark, 16:9, a play mark). Nothing in the build or the tests needs
the sidecar; tests that need a real clip skip without it.

## Import and the poster frame

- Video extensions (`mp4 m4v mov mkv webm avi mts m2ts`) are importable. A video is never read into memory: the probe asks `ffprobe`,
  so there is **no content hash** (duplicates are found by path only).
- The import records the probe: duration (`Photo::duration`), the size **as shown** (rotation applied), the creation time as the capture
  time (zone dropped; the file's date when absent), and codec, frame rate and rotation (`Photo::video`).
- The thumbnail and preview levels come from the **poster frame**. By default that is 10 % into the clip, or 1 s when the duration is
  unknown, always kept inside the clip.
- `video.setPoster {id?, t?}` stores a poster time (seconds, held to the duration; `null` returns to the default; a call with no `t`
  takes where the player is, when the clip is loaded in it) in the photo's develop settings (`poster_time`). It is one undo step (a History entry, "Set Poster Frame"). The field is absent from the JSON while unset,
  so no existing settings hash changes; once set, the thumbnail cache key changes and the decoded sources are dropped.
- The player's **Set Poster Frame** (a button at the right of the transport bar, and Photo > Set Poster Frame) calls `video.setPoster` with
  the time the player is at. It needs the clip loaded in the player (played, or scrubbed to a frame), so the button is disabled until then.
- Frames larger than a thumbnail show as "Reading Video Frame" in the activity stack, with a stop button that kills the child process.

## Silent playback

Space (or the play button) plays a video in the loupe; the poster frame shows until then. There is no audio yet.

- **Stream.** `lightcraft_video::stream_frames(path, start_t, max_size, fps_cap)` (`stream_frames_sized` when the size and rate are known from
  the probe, one process fewer) runs `ffmpeg -re -ss <start> -i <file> -an -vf fps=<n>,scale=<w>:<h> -pix_fmt rgba -f rawvideo pipe:1` through
  `rightkit-ffmpeg`'s `spawn_streaming` (an owned child; stdout and stderr piped, stdin null; the same `file`-only protocol whitelist). `-re`
  paces the child at the clip's own speed, so a frame arrives when it is due.
- **Queue and back-pressure.** A worker thread reads exact `width x height x 4` byte chunks into a queue of 3 frames. It never waits for the
  UI: a frame that finds the queue full is dropped and counted (`FrameStream::dropped`), and a frame's time is its index over the frame
  rate, so a drop does not shift the frames after it. A partial last frame (a child that died mid-frame) is discarded; the last 4 KiB of the
  child's error output turns a failure into a message.
- **Cancellation.** Dropping the `FrameStream`, `FrameStream::stop` or the cancel flag ends it: the child and its whole process tree are
  killed (on a helper thread, so pausing never waits), its pipes close, and the worker ends. No deadline applies to a stream.
- **Frames.** At most 1280 px along the long edge (never enlarged) and 30 frames per second, upright (rotation applied), as the decoder gives
  them: not colour managed, and without the photo's edits.
- **Engine.** `video.play {id?}`, `video.pause`, `video.togglePlay {id?}`, `video.seek {id?, t}` and `video.stop` drive one player per session;
  the query `video.state` reports `{photo, state, position, duration, streaming, dropped, error, unavailable}`. A running stream holds a
  "Playing Video" row in the activity stack, with a stop button. The UI calls `Session::video_tick_now` once per frame, which shows the frame
  whose time has come; the clock starts when the first frame arrives, so the child's start-up time does not make frames late. Pause keeps the
  frame it stopped on; a seek while paused fetches the frame there and stops; a seek while playing restarts the stream there; the end of the
  clip leaves it ended on its last frame (play again starts over). A stream that fails leaves the clip paused with the reason; without the
  sidecar the message is "Video support isn't installed".
- **Trim.** Playback starts at the clip's in point (when the clip is before it, at its end, or after its out point) and stops at its out point.
  Scrubbing is not held to it, so new points can be found outside.
- **Loupe.** A video has a transport bar at the bottom of the loupe (above the Trim row): play or pause, the time and the duration, and a
  scrubber that seeks when it is released. A clip that leaves the loupe (another photo, another view) is stopped, so no child process decodes
  for a picture nobody sees.

## Trim

`video.setTrim {id?, in?, out?}` stores the in and out points (seconds) of the part of a clip that exports keep, in the photo's develop
settings (`trim_in`, `trim_out`); `video.clearTrim {id?}` keeps the whole clip again. The file is never cut.

- A point left out of `setTrim` stays as it is and `null` clears it. Points are held to the clip: an out point at or past the end is no
  out point, an in point at or past the end is refused, and in must be before out with at least 0.05 s between them
  (`lightcraft_video::check_trim`). A clip whose duration is not known (imported without the sidecar) is checked only against itself.
- Each call is one undo step (a History entry, "Set Trim" or "Clear Trim"). The fields are absent from the JSON while unset, so no
  existing settings hash changes; a trim is a choice of range, not an edit of the picture (`is_unedited` ignores it).
- The loupe shows a Trim row at the bottom of a video that has a duration: the points, **Set In** and **Set Out** (at the frame on
  screen: where the player is once the clip is loaded in it, else the poster frame) and **Clear Trim**. Playback is held to the trim
  (it starts at the in point and stops at the out point; scrubbing is free, so new points can be found outside it); draggable handles on
  the player's scrubber are not there yet.

## Capture Frame

`video.captureFrame {id?, t?}` (Photo ▸ Capture Frame, issue #113) saves one frame of a clip as a JPEG beside the clip.

- **Frame.** At `t` seconds (held inside the clip). By default `t` is where the player is when the clip is loaded in it, else the poster
  frame. The frame is decoded at the clip's full size (its longest edge; the sidecar caps it at 4096 px) by `lightcraft_video::frame_at_with`,
  under the activity row "Capturing Frame" with a stop button that kills the child process. It is encoded as a JPEG (quality 95, 4:4:4), not
  colour managed and without the clip's edits, as the stream's frames are.
- **File.** `<clip name> frame <t>s.jpg` in the clip's folder, for example `beach frame 7.500s.jpg`. A name already taken gets ` (2)`,
  ` (3)` and so on before the extension, so a capture never overwrites a file. The file is written durably, as every export is.
- **Catalog.** The JPEG is imported (as Render to DNG imports its copy) and stacked with the clip, the frame on top. It takes the clip's
  keywords and rating, and its capture time is the clip's capture time plus the whole seconds of `t` (rounded down).
- **One undo step.** The import, the metadata and the stack are folded into one History entry, "Capture Frame". Undo removes the frame from
  the catalog and the stack; the JPEG stays on disk, as an imported file does.
- **Errors.** Without the sidecar the command fails with "video support not installed" before anything is written. A clip with no file (a
  generated demo) has nothing to take a frame from. A `t` below zero or a photo that is not a video is refused.
- **Frame source.** The decoder is a parameter (`video_capture::capture_frame`), so the tests (`tests_video_capture.rs`) use a fake frame
  and run without the sidecar; they check the file names, the capture time, the metadata copy, the stack, the single undo step and the
  error without the sidecar.

## Export

`app.export {format: "video"}` (desktop app, `lightcraft-cli run`, MCP `export`, the control channel) writes each selected video as a clip
file, cut to its trim, through the same batch as pictures (names, conflict policy, subfolder, never over an original).

- **Stream copy** (the default): the video and audio streams are copied as they are, so it is fast and lossless, in the clip's own container
  (`mov` stays `mov`). A cut can only begin on a keyframe: the file starts at the keyframe at or before the in point (up to a group of
  pictures longer than asked) and ends at the first packet past the out point; `-avoid_negative_ts make_zero` puts that keyframe at time
  zero. The report says `accurate: false`.
- **Re-encode** (`videoReencode: true`): starts on exactly the in point; H.264 and AAC in MP4. Windows uses `h264_mf` (Media Foundation) and
  macOS `h264_videotoolbox`, each with an explicit `-b:v` (`videoKbps`; by default 0.1 bit per pixel per frame at the clip's size and rate,
  between 1 and 100 Mbit/s): Media Foundation with no rate writes 1080p at about 88 kbit/s. **The approved `decode` build of the sidecar
  links neither encoder** (in the `rightkit-ffmpeg` catalog `--enable-mediafoundation` and `--enable-videotoolbox` belong to the
  `export-ass*` builds), so there a re-encode fails with "this ffmpeg sidecar build has no H.264 encoder"; only stream copy works until an
  export build is approved and bundled.
- Only the first video stream and the audio streams are written; subtitles, data streams and chapters are left out. The clip's container
  metadata is copied, unless the export removes the location (or the photo is in a private place) or the metadata policy is not "all":
  then none is written (a clip's metadata is all or nothing).
- The run holds a row in the activity stack ("Exporting Video", with progress) whose ✕ kills the ffmpeg child and its process tree, and a
  deadline of ten times the kept length. ffmpeg writes a temporary file (`ember-clip-<pid>-<n>.<ext>` in the system temp folder), which
  the batch moves to its path with the atomic write every export uses (a temp file next to the target, renamed over it); a failure or a
  cancel leaves no temporary file and any earlier file as it was.
- The entry of the file in the result gains `video`: `{mode, accurate, start, length, encoder?, videoKbps?, strippedMetadata}`.
- Without the sidecar (a development checkout, CI, Linux, the web) a clip export is refused before anything is written ("video support
  not installed" or "not supported"). `lightcraft-cli render` writes a picture only and refuses the video format.
- A **picture format of a video** exports one frame through the ordinary still pipeline (size, sharpening, colour space, watermark): the
  poster frame, or `frameTime` seconds into the clip (held inside it).

## Licence

FFmpeg is LGPL-2.1-or-later and is **not in this repository**. It is run as a separate program, never linked. Release bundles that include
it owe the source offer stated in `NOTICE` (corresponding source, build recipe and component licences on request for three years, from the
`rightkit-ffmpeg` catalog entry of the shipped build) and carry FFmpeg's licence notice beside the binaries. See `NOTICE` and the
decision in `CLAUDE.md` ("Pure Rust ... except the verified LGPL ffmpeg sidecar").

## Not yet

Audio, draggable trim handles on the player's scrubber, a grid badge and duration label, the Export dialog's Video entry, an export build of the sidecar with an H.264
encoder (so a re-encode works; until then only stream copy does) and the bundling of the sidecar into the installers (release tooling),
and Linux.
