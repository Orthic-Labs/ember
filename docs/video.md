# Video

*Last updated: 2026-10-17 (foundation: probe and poster frame; trim; clip and frame export; issues #73, #74, #76, #77).*

Ember catalogues videos, shows a poster frame for each in the grid, the loupe and exports, and lets you choose which frame that is.
Playback comes next (silent first, #73); this page records what exists and how it is built.

## How it works

Ember decodes no video itself. It runs the **FFmpeg and FFprobe programs bundled with the release** as child processes, through
[`rightkit-ffmpeg`](https://github.com/Orthic-Labs/rightkit) (RightKit's owner of the approved-binary catalog and the process
runner), behind the `lightcraft-video` crate (layer L1):

| Call | What it does |
| --- | --- |
| `probe(path) -> VideoInfo` | duration, size, display rotation, codec, frame rate and creation time of the first video stream (`ffprobe` JSON) |
| `frame_at(path, t, max_size) -> Rgba8` | one upright frame at `t` seconds, at most `max_size` along its long edge (never enlarged, never above 4096) |
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
- `video.setPoster {id?, t}` stores a poster time (seconds, held to the duration; `null` returns to the default) in the photo's develop
  settings (`poster_time`). It is one undo step (a History entry, "Set Poster Frame"). The field is absent from the JSON while unset,
  so no existing settings hash changes; once set, the thumbnail cache key changes and the decoded sources are dropped.
- Frames larger than a thumbnail show as "Reading Video Frame" in the activity stack, with a stop button that kills the child process.

## Trim

`video.setTrim {id?, in?, out?}` stores the in and out points (seconds) of the part of a clip that exports keep, in the photo's develop
settings (`trim_in`, `trim_out`); `video.clearTrim {id?}` keeps the whole clip again. The file is never cut.

- A point left out of `setTrim` stays as it is and `null` clears it. Points are held to the clip: an out point at or past the end is no
  out point, an in point at or past the end is refused, and in must be before out with at least 0.05 s between them
  (`lightcraft_video::check_trim`). A clip whose duration is not known (imported without the sidecar) is checked only against itself.
- Each call is one undo step (a History entry, "Set Trim" or "Clear Trim"). The fields are absent from the JSON while unset, so no
  existing settings hash changes; a trim is a choice of range, not an edit of the picture (`is_unedited` ignores it).
- The loupe shows a Trim row at the bottom of a video that has a duration: the points, **Set In** and **Set Out** (at the frame on
  screen, which is the poster frame until the player exists) and **Clear Trim**. The player's scrubber gets draggable handles with it.

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

Silent playback (play/pause, seek, a player in the loupe) with trim handles on its scrubber and playback held to the trimmed range, audio,
a grid badge and duration label, a poster chooser in the UI, the Export dialog's Video entry, an export build of the sidecar with an H.264
encoder (so a re-encode works; until then only stream copy does) and the bundling of the sidecar into the installers (release tooling),
and Linux.
