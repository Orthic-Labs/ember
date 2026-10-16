# Display (monitor) profiles

Ember can show photos through the monitor's ICC profile, so colours are right on wide-gamut and calibrated
displays. Without one, previews are sRGB, which a wide-gamut panel shows oversaturated.

**The system's monitor profile is used while none is chosen** (Settings ▸ Display ▸ **Use Screen Profile**):
macOS reads the window's screen profile (System Settings ▸ Displays); Windows reads the profile assigned to the monitor
the window is on (`GetICMProfileW`, the Color management settings; `lightcraft_sysmem::window_monitor_profile`); Linux asks
colord over D-Bus for the default profile of the primary display output. A missing, unreadable or invalid profile is sRGB.

**On macOS** the reason is this. macOS doesn't
colour-manage Ember's window (wgpu leaves its Metal layer without a colour space, which Core Animation shows
unmatched), so on a P3 screen previews used to look more saturated than the exported files in Preview or Photos (issue
#787). The profile is read again about once a second, so moving the window to another screen is followed.

## Using it

Settings ▸ Display ▸ **Choose Profile…** picks an `.icc` / `.icm` file (the dialog opens where the system keeps
them: `~/.local/share/icc` on Linux, ColorSync's folders on macOS, `…\spool\drivers\color` on Windows). **Use sRGB**
treats the display as sRGB even where the screen's profile is known; **Use Screen Profile** (macOS, Windows, Linux)
goes back to following it. Precedence: a chosen file, else sRGB, else the screen's profile. The choice is an app setting (`ui.json`, `displayProfile`)
and applies at once; a file that can't be used is reported there and previews stay sRGB. `"srgb"` is the stored value of **Use sRGB**.

Agents and scripts: `app.displayProfile {"path": "/path/to/monitor.icc"}` (`""` or `null`: none, i.e. the screen's profile where known; `"srgb"`: sRGB; no `path`: report
only) returns `{path, description, kind: "matrix" | "lut", primaries: {red, green, blue: [x, y]}}`; the screen's
profile (none chosen) reports `path: null, screen: true`. Hosts supply it as `Services::screen_profile`
(`apps/lightcraft/src/screen_profile.rs`); headless sessions have none and stay sRGB.

## How it works

- **The loupe renders into the display's gamut.** Views (Detail, Before, Compare / Reference, preset and profile
  hover, the second window) ask the pipeline for a render in the display's own primaries
  (`lightcraft_pipeline::DisplaySpace`, `RenderRequest::display`): the per-pixel stage converts from the working space
  to the display's linear RGB, gamut maps into *its* gamut and encodes with the sRGB curve, on the CPU and the GPU
  alike (only `FinishParams::to_out` / `out_luma` change). Colours an sRGB render would clip stay as saturated as the
  display can show. For a display that is exactly a standard space the result equals an 8-bit render into that space.
- **Then to the display's device values** (`lightcraft_codecs::display::DisplayProfile::correct`): for a matrix/TRC
  profile, three 256-entry tables from the sRGB curve to the profile's tone curves (nothing at all when they are the
  sRGB curve, as for Display P3); for a LUT-based profile, a `moxcms` transform through its B2A tables. A LUT-only
  profile (no colorant tags) gets its primaries measured through its A2B tables.
- **Everything made for sRGB is converted** (`lightcraft_engine::display::present`, on the render workers):
  thumbnails, cached view previews, embedded camera JPEGs and merge previews.
- **What stays sRGB:** the histogram (of the same sampled pixels, converted back to sRGB), the rendered-thumbnail and
  view-preview caches (a profile change doesn't invalidate them), exports, and the UI itself.
- **Soft proofing:** the blue "display" gamut warning shows what the monitor can't show, from its profile.
- Conversions are relative colorimetric: the display's white is the image's white.

## Cost

Measured on a 2-core cloud VM, 2560 × 1707 preview: the tone-curve tables take about 5 ms (single-threaded); converting the sampled
histogram pixels a few ms; the sRGB copy for the view-preview cache (full-quality renders only) about 30 ms; a
LUT-based profile's correction about 65 ms. Drafts during slider drags skip the full sRGB copy.

## Not yet

- Linux follows the primary output only: colord can't say which output the window is on, so a window on a secondary
  monitor still gets the primary's profile (X11 `_ICC_PROFILE` and per-output matching are not read).
  [#186](https://github.com/Orthic-Labs/ember/issues/186)
- The Windows and Linux lookups are untested on real multi-monitor hardware here (no build was run); verify by moving
  the window between monitors with different profiles.
