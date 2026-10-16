//! The ICC profile of the screen showing LightCraft's window (macOS, Windows and Linux).
//!
//! macOS colour-manages windows that say what colour space they draw in. wgpu leaves the window's
//! Metal layer without one for sRGB content, which Core Animation shows without colour matching, so
//! on a wide-gamut (P3) screen the previews looked more saturated than the exported files do in
//! Preview or Photos (issue #787). Windows doesn't colour-manage a window's pixels at all, so there
//! the previews are matched to the monitor's profile the same way (issue #84). While no monitor
//! profile is chosen in Settings ▸ Display, the previews are shown through this profile instead
//! (`Services::screen_profile`, read again about once a second, so a window moved to another
//! screen is followed).
//!
//! macOS asks AppKit for the window's screen; Windows asks which profile the system assigned to
//! the monitor the window is on (`lightcraft_sysmem::window_monitor_profile`, the one unsafe FFI
//! helper) and reads that file. Linux asks colord (the colour daemon GNOME and KDE keep monitor
//! profiles in) over the system D-Bus for the default profile of the device driving the monitor the
//! window is on (winit's `current_monitor` name, matched to colord's `XRANDR_name`; the app layer
//! reports it through `track_window_monitor`), and of the primary display output while that monitor
//! or its device isn't known. A missing, unreadable or invalid profile is `None`: sRGB.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSScreen};

use lightcraft_codecs::display::MAX_PROFILE_BYTES;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// The profile of the screen the window is mostly on (the main screen when it is hidden), or `None`
/// when it isn't known: off the main thread, or a screen without a usable profile.
#[cfg(target_os = "macos")]
pub fn icc() -> Option<Vec<u8>> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let window = app.mainWindow().or_else(|| app.keyWindow()).or_else(|| app.windows().firstObject());
    let screen = window.and_then(|w| w.screen()).or_else(|| NSScreen::mainScreen(mtm))?;
    let icc = screen.colorSpace()?.ICCProfileData()?.to_vec();
    (!icc.is_empty()).then_some(icc)
}

/// The profile Windows assigned to the monitor the window is on, or `None` when there is none
/// that can be read (sRGB then).
#[cfg(windows)]
pub fn icc() -> Option<Vec<u8>> {
    read_cached(&lightcraft_sysmem::window_monitor_profile()?)
}

/// The monitor the window is on, as winit reports it (`Window::current_monitor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    /// The output's name: the RandR output on X11 (`DP-1`), the connector on Wayland (`HDMI-A-1`).
    pub name: Option<String>,
    /// The monitor's top-left corner in the desktop, in physical pixels.
    pub position: Option<(i32, i32)>,
}

/// The monitor the window was last seen on (`None` until the app layer reports one).
static WINDOW_MONITOR: Mutex<Option<MonitorInfo>> = Mutex::new(None);

/// Record the monitor the window is on, for [`icc`] to follow. Cheap; the app layer calls it as the
/// window moves.
pub fn set_window_monitor(info: Option<MonitorInfo>) {
    *WINDOW_MONITOR.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = info;
}

/// The monitor the window is on, read from winit (throttled to a few times a second), and stored for
/// [`icc`]. Does nothing without a window.
#[cfg(target_os = "linux")]
pub fn track_window_monitor(frame: &eframe::Frame) {
    use std::time::{Duration, Instant};
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.is_some_and(|at| at.elapsed() < Duration::from_millis(500)) {
        return;
    }
    *last = Some(Instant::now());
    let Some(window) = frame.winit_window() else { return };
    let info = window.current_monitor().map(|m| MonitorInfo { name: m.name(), position: Some((m.position().x, m.position().y)) });
    set_window_monitor(info);
}

/// The profile colord assigns to the monitor the window is on (the primary output's while that
/// isn't known or colord has no such device), or `None` without colord, a display device or a
/// readable profile. Asked when the monitor changes, else at most every few seconds (and rarely
/// again after a failure), so a missing daemon can't stall the UI thread that polls this.
#[cfg(target_os = "linux")]
pub fn icc() -> Option<Vec<u8>> {
    use std::time::{Duration, Instant};
    const REFRESH: Duration = Duration::from_secs(3);
    const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30);
    type Last = (Instant, Option<MonitorInfo>, Option<Vec<u8>>);
    static LAST: Mutex<Option<Last>> = Mutex::new(None);
    let monitor = WINDOW_MONITOR.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, seen, value)) = last.as_ref()
        && *seen == monitor
        && at.elapsed() < if value.is_some() { REFRESH } else { RETRY_AFTER_FAILURE }
    {
        return value.clone();
    }
    let value = ZbusColord::connect().and_then(|colord| profile_path(&colord, monitor.as_ref())).and_then(|path| read_cached(&path));
    *last = Some((Instant::now(), monitor, value.clone()));
    value
}

/// One colord display device, as far as choosing its profile goes.
struct Output {
    /// colord's `OutputPriority` metadata says "primary".
    primary: bool,
    /// The device's `XRANDR_name` metadata: the output it drives.
    output_name: Option<String>,
    /// The file of the device's default (first) profile, when it has one.
    profile: Option<PathBuf>,
}

/// The profile of the primary output, else of the first output that has one.
fn pick_output_profile(outputs: &[Output]) -> Option<&Path> {
    let with_profile = |primary_only: bool| outputs.iter().find(|o| o.profile.is_some() && (o.primary || !primary_only));
    with_profile(true).or_else(|| with_profile(false)).and_then(|o| o.profile.as_deref())
}

/// A colord call failed (no daemon, a bad reply): the caller falls back.
#[derive(Debug, PartialEq, Eq)]
struct DbusError;

/// The colord (`org.freedesktop.ColorManager`) calls the lookup needs, so tests can stand in for the bus.
trait Colord {
    /// `FindDeviceByProperty`: the object path of the device whose metadata `property` is `value`;
    /// `Ok(None)` when colord has no such device.
    fn find_device_by_property(&self, property: &str, value: &str) -> Result<Option<String>, DbusError>;
    /// The file of the device's default (first) profile; `Ok(None)` when it has none.
    fn default_profile(&self, device: &str) -> Result<Option<PathBuf>, DbusError>;
    /// Every display device whose profile could be read (`GetDevicesByKind`).
    fn display_outputs(&self) -> Result<Vec<Output>, DbusError>;
}

/// The names colord may know a winit monitor name by, best first. X11 reports the RandR output
/// (`HDMI-1`), Wayland compositors the kernel connector (`HDMI-A-1`); colord's `XRANDR_name` holds
/// whichever the session's colour daemon saw, so both spellings are tried.
fn output_name_candidates(name: &str) -> Vec<String> {
    let name = name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    let mut names = vec![name.to_string()];
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    // `HDMI-A-1` / `DVI-D-1` <-> `HDMI-1` / `DVI-1`
    let parts: Vec<&str> = name.split('-').collect();
    match parts.as_slice() {
        [kind @ ("HDMI" | "DVI"), variant, index] if variant.len() == 1 && digits(index) => names.push(format!("{kind}-{index}")),
        [kind @ ("HDMI" | "DVI"), index] if digits(index) => names.push(format!("{kind}-{}-{index}", if *kind == "HDMI" { "A" } else { "D" })),
        _ => {}
    }
    names
}

/// The output among `outputs` whose `XRANDR_name` is one of `names` (the first name wins, ignoring case).
fn select_output<'a>(outputs: &'a [Output], names: &[String]) -> Option<&'a Output> {
    names.iter().find_map(|n| outputs.iter().find(|o| o.output_name.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(n))))
}

/// The profile file for the monitor the window is on. With a monitor name: colord's device for it
/// (`FindDeviceByProperty` on `XRANDR_name`, else a scan of the display devices) and its default
/// profile (a device without one is genuinely unprofiled: `None`, sRGB). Without a name, an
/// unknown device or a D-Bus error on the way: the primary output's profile, as before
/// per-monitor matching.
fn profile_path(colord: &dyn Colord, monitor: Option<&MonitorInfo>) -> Option<PathBuf> {
    let names = monitor.and_then(|m| m.name.as_deref()).map(output_name_candidates).unwrap_or_default();
    if !names.is_empty() {
        for name in &names {
            if let Ok(Some(device)) = colord.find_device_by_property("XRANDR_name", name) {
                match colord.default_profile(&device) {
                    Ok(profile) => return profile,
                    Err(DbusError) => break,
                }
            }
        }
        if let Ok(outputs) = colord.display_outputs()
            && let Some(output) = select_output(&outputs, &names)
        {
            return output.profile.clone();
        }
    }
    let outputs = colord.display_outputs().ok()?;
    pick_output_profile(&outputs).map(Path::to_path_buf)
}

/// colord over the system bus.
#[cfg(target_os = "linux")]
struct ZbusColord {
    conn: zbus::blocking::Connection,
}

#[cfg(target_os = "linux")]
impl ZbusColord {
    const DEST: &'static str = "org.freedesktop.ColorManager";

    fn connect() -> Option<Self> {
        let conn = zbus::blocking::connection::Builder::system().ok()?.method_timeout(std::time::Duration::from_secs(2)).build().ok()?;
        Some(Self { conn })
    }

    fn proxy(&self, path: &str, interface: &'static str) -> Result<zbus::blocking::Proxy<'static>, DbusError> {
        zbus::blocking::Proxy::new(&self.conn, Self::DEST, path.to_string(), interface).map_err(|_| DbusError)
    }

    fn manager(&self) -> Result<zbus::blocking::Proxy<'static>, DbusError> {
        self.proxy("/org/freedesktop/ColorManager", "org.freedesktop.ColorManager")
    }
}

#[cfg(target_os = "linux")]
impl Colord for ZbusColord {
    fn find_device_by_property(&self, property: &str, value: &str) -> Result<Option<String>, DbusError> {
        use zbus::zvariant::OwnedObjectPath;
        // colord answers an unknown device with an error reply, not an empty path
        let found: Result<OwnedObjectPath, _> = self.manager()?.call("FindDeviceByProperty", &(property, value));
        Ok(found.ok().map(|p| p.as_str().to_string()).filter(|p| p.len() > 1))
    }

    fn default_profile(&self, device: &str) -> Result<Option<PathBuf>, DbusError> {
        use zbus::zvariant::OwnedObjectPath;
        let device = self.proxy(device, "org.freedesktop.ColorManager.Device")?;
        let profiles: Vec<OwnedObjectPath> = device.get_property("Profiles").map_err(|_| DbusError)?;
        // colord lists the profile in use first
        let Some(first) = profiles.first() else { return Ok(None) };
        let profile = self.proxy(first.as_str(), "org.freedesktop.ColorManager.Profile")?;
        let file: String = profile.get_property("Filename").map_err(|_| DbusError)?;
        Ok((!file.is_empty()).then(|| PathBuf::from(file)))
    }

    fn display_outputs(&self) -> Result<Vec<Output>, DbusError> {
        use std::collections::HashMap;
        use zbus::zvariant::OwnedObjectPath;
        let devices: Vec<OwnedObjectPath> = self.manager()?.call("GetDevicesByKind", &("display",)).map_err(|_| DbusError)?;
        let mut outputs = Vec::new();
        for path in devices.iter().take(16) {
            let Ok(device) = self.proxy(path.as_str(), "org.freedesktop.ColorManager.Device") else { continue };
            // a device whose profile can't be read is left out, so it never reads as "no profile"
            let Ok(profile) = self.default_profile(path.as_str()) else { continue };
            let metadata: HashMap<String, String> = device.get_property("Metadata").unwrap_or_default();
            let primary = metadata.get("OutputPriority").is_some_and(|p| p == "primary");
            let output_name = metadata.get("XRANDR_name").cloned();
            outputs.push(Output { primary, output_name, profile });
        }
        Ok(outputs)
    }
}

/// The screen's current extended-dynamic-range headroom: the brightest component value it can show
/// relative to SDR white (1.0 on an SDR screen or with HDR off, above 1.0 when macOS has the screen
/// in EDR). `None` off the main thread or without a screen.
#[cfg(target_os = "macos")]
pub fn hdr_headroom() -> Option<f32> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let window = app.mainWindow().or_else(|| app.keyWindow()).or_else(|| app.windows().firstObject());
    let screen = window.and_then(|w| w.screen()).or_else(|| NSScreen::mainScreen(mtm))?;
    let v = screen.maximumExtendedDynamicRangeColorComponentValue() as f32;
    v.is_finite().then_some(v)
}

/// Does `bytes` have the shape of an ICC profile: a 128-byte header whose declared size fits and
/// that carries the `acsp` signature? A cheap guard before the CMS parses it.
fn looks_like_icc(bytes: &[u8]) -> bool {
    let declared = bytes.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(|b| u32::from_be_bytes(b) as usize);
    bytes.len() >= 128 && bytes.get(36..40) == Some(b"acsp".as_slice()) && declared.is_some_and(|n| (128..=bytes.len()).contains(&n))
}

/// The ICC profile in the file at `path`; `None` when it is missing, unreadable, too large or not
/// an ICC profile (the display is then treated as sRGB).
fn read_profile_file(path: &Path) -> Option<Vec<u8>> {
    if std::fs::metadata(path).ok()?.len() > MAX_PROFILE_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    looks_like_icc(&bytes).then_some(bytes)
}

/// The profile file read last, so polling once a second doesn't read it again until it changes.
struct Cached {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    bytes: Vec<u8>,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// [`read_profile_file`], kept while the file's path, size and modification time stay the same.
fn read_cached(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    let (modified, len) = (meta.modified().ok(), meta.len());
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = cache.as_ref()
        && c.path == path
        && c.modified == modified
        && c.len == len
    {
        return Some(c.bytes.clone());
    }
    let Some(bytes) = read_profile_file(path) else {
        *cache = None;
        return None;
    };
    *cache = Some(Cached { path: path.to_path_buf(), modified, len, bytes: bytes.clone() });
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_codecs::{NamedSpace, icc::write_named};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightcraft-screen-profile-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_off_the_main_thread() {
        // AppKit is main-thread only; a test thread gets nothing rather than undefined behaviour
        assert_eq!(std::thread::spawn(super::icc).join().unwrap(), None);
        assert_eq!(std::thread::spawn(super::hdr_headroom).join().unwrap(), None);
    }

    #[test]
    fn a_missing_or_invalid_profile_falls_back_to_srgb() {
        let dir = scratch("invalid");
        assert_eq!(read_profile_file(&dir.join("missing.icc")), None);
        assert_eq!(read_cached(&dir.join("missing.icc")), None);

        let garbage = dir.join("garbage.icc");
        std::fs::write(&garbage, vec![b'x'; 300]).unwrap();
        assert_eq!(read_profile_file(&garbage), None);
        assert_eq!(read_cached(&garbage), None);

        // a directory, an empty file, a profile cut short
        assert_eq!(read_profile_file(&dir), None);
        let empty = dir.join("empty.icc");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(read_profile_file(&empty), None);
        let mut short = write_named(NamedSpace::DisplayP3);
        short.truncate(short.len() / 2);
        let cut = dir.join("cut.icc");
        std::fs::write(&cut, &short).unwrap();
        assert_eq!(read_profile_file(&cut), None, "its header declares more bytes than there are");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_valid_profile_is_read_and_follows_changes() {
        let dir = scratch("valid");
        let path = dir.join("monitor.icc");
        let p3 = write_named(NamedSpace::DisplayP3);
        std::fs::write(&path, &p3).unwrap();
        assert_eq!(read_profile_file(&path), Some(p3.clone()));
        assert_eq!(read_cached(&path), Some(p3.clone()));
        assert_eq!(read_cached(&path), Some(p3.clone()), "the second read comes from the cache");

        // the monitor gets another profile (a different size, so the cache can't be fooled)
        let mut adobe = write_named(NamedSpace::AdobeRgb);
        adobe.extend_from_slice(&[0; 16]);
        std::fs::write(&path, &adobe).unwrap();
        assert_eq!(read_cached(&path), Some(adobe));
        // ... and then a broken one
        std::fs::write(&path, b"broken").unwrap();
        assert_eq!(read_cached(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_primary_output_wins_and_outputs_without_a_profile_are_skipped() {
        let out = |primary: bool, file: Option<&str>| Output { primary, output_name: None, profile: file.map(PathBuf::from) };
        let pick = |outputs: &[Output]| pick_output_profile(outputs).map(|p| p.to_string_lossy().to_string());
        assert_eq!(pick(&[]), None);
        assert_eq!(pick(&[out(true, None), out(false, None)]), None);
        assert_eq!(pick(&[out(false, Some("a.icc")), out(true, Some("b.icc"))]), Some("b.icc".into()));
        // a primary output without a profile: the first output that has one
        assert_eq!(pick(&[out(false, None), out(true, None), out(false, Some("c.icc"))]), Some("c.icc".into()));
        assert_eq!(pick(&[out(false, Some("a.icc")), out(false, Some("b.icc"))]), Some("a.icc".into()));
    }

    /// A canned colord: devices by output name, with switches for failures.
    struct FakeColord {
        outputs: Vec<Output>,
        /// `FindDeviceByProperty` knows these (output name -> device path)
        by_name: Vec<(&'static str, &'static str)>,
        /// device path -> default profile (a device missing here has none)
        profiles: Vec<(&'static str, &'static str)>,
        /// every call fails (no daemon)
        down: bool,
        /// reading a device's profile fails
        profile_error: bool,
    }

    impl FakeColord {
        fn new() -> Self {
            let out = |primary: bool, name: &str, file: Option<&str>| Output {
                primary,
                output_name: Some(name.to_string()),
                profile: file.map(PathBuf::from),
            };
            Self {
                outputs: vec![out(true, "DP-1", Some("/icc/dp1.icc")), out(false, "HDMI-A-1", Some("/icc/hdmi.icc")), out(false, "eDP-1", None)],
                by_name: vec![("DP-1", "/dev/dp1"), ("HDMI-A-1", "/dev/hdmi"), ("eDP-1", "/dev/edp")],
                profiles: vec![("/dev/dp1", "/icc/dp1.icc"), ("/dev/hdmi", "/icc/hdmi.icc")],
                down: false,
                profile_error: false,
            }
        }
    }

    impl Colord for FakeColord {
        fn find_device_by_property(&self, property: &str, value: &str) -> Result<Option<String>, DbusError> {
            if self.down {
                return Err(DbusError);
            }
            assert_eq!(property, "XRANDR_name");
            Ok(self.by_name.iter().find(|(n, _)| *n == value).map(|(_, d)| d.to_string()))
        }
        fn default_profile(&self, device: &str) -> Result<Option<PathBuf>, DbusError> {
            if self.down || self.profile_error {
                return Err(DbusError);
            }
            Ok(self.profiles.iter().find(|(d, _)| *d == device).map(|(_, p)| PathBuf::from(p)))
        }
        fn display_outputs(&self) -> Result<Vec<Output>, DbusError> {
            if self.down {
                return Err(DbusError);
            }
            Ok(self.outputs.iter().map(|o| Output { primary: o.primary, output_name: o.output_name.clone(), profile: o.profile.clone() }).collect())
        }
    }

    fn on(name: &str) -> MonitorInfo {
        MonitorInfo { name: Some(name.to_string()), position: Some((0, 0)) }
    }

    fn path_for(colord: &FakeColord, monitor: Option<&MonitorInfo>) -> Option<String> {
        profile_path(colord, monitor).map(|p| p.to_string_lossy().to_string())
    }

    #[test]
    fn monitor_names_map_to_colord_output_names() {
        assert_eq!(output_name_candidates("DP-1"), ["DP-1"]);
        assert_eq!(output_name_candidates("  eDP-1 "), ["eDP-1"]);
        assert_eq!(output_name_candidates("HDMI-A-1"), ["HDMI-A-1", "HDMI-1"]);
        assert_eq!(output_name_candidates("HDMI-2"), ["HDMI-2", "HDMI-A-2"]);
        assert_eq!(output_name_candidates("DVI-D-1"), ["DVI-D-1", "DVI-1"]);
        assert_eq!(output_name_candidates("DVI-1"), ["DVI-1", "DVI-D-1"]);
        // not a connector spelling we rewrite
        assert_eq!(output_name_candidates("HDMI-A-x"), ["HDMI-A-x"]);
        assert_eq!(output_name_candidates("Dell U2720Q"), ["Dell U2720Q"]);
        assert!(output_name_candidates("").is_empty());
        assert!(output_name_candidates("   ").is_empty());
    }

    #[test]
    fn the_device_of_the_window_monitor_is_chosen_among_several() {
        let colord = FakeColord::new();
        assert_eq!(path_for(&colord, Some(&on("DP-1"))), Some("/icc/dp1.icc".into()));
        assert_eq!(path_for(&colord, Some(&on("HDMI-A-1"))), Some("/icc/hdmi.icc".into()));
        // X11 spells the same output HDMI-1
        assert_eq!(path_for(&colord, Some(&on("HDMI-1"))), Some("/icc/hdmi.icc".into()));
        // a monitor without a profile is sRGB, not the primary's profile
        assert_eq!(path_for(&colord, Some(&on("eDP-1"))), None);
    }

    #[test]
    fn select_output_matches_names_ignoring_case_and_in_order() {
        let colord = FakeColord::new();
        let pick = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            select_output(&colord.outputs, &names).and_then(|o| o.output_name.clone())
        };
        assert_eq!(pick(&["hdmi-a-1"]), Some("HDMI-A-1".into()));
        assert_eq!(pick(&["VGA-1", "DP-1"]), Some("DP-1".into()));
        assert_eq!(pick(&["VGA-1"]), None);
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn a_device_found_only_by_scanning_is_used() {
        // FindDeviceByProperty knows nothing, but the device list carries the output name
        let mut colord = FakeColord::new();
        colord.by_name.clear();
        assert_eq!(path_for(&colord, Some(&on("HDMI-A-1"))), Some("/icc/hdmi.icc".into()));
    }

    #[test]
    fn unknown_monitors_fall_back_to_the_primary_output() {
        let colord = FakeColord::new();
        // no monitor reported, a monitor without a name, an empty name, a name colord has no device for
        assert_eq!(path_for(&colord, None), Some("/icc/dp1.icc".into()));
        assert_eq!(path_for(&colord, Some(&MonitorInfo { name: None, position: Some((1920, 0)) })), Some("/icc/dp1.icc".into()));
        assert_eq!(path_for(&colord, Some(&on(""))), Some("/icc/dp1.icc".into()));
        assert_eq!(path_for(&colord, Some(&on("DP-9"))), Some("/icc/dp1.icc".into()));
    }

    #[test]
    fn dbus_errors_fall_back_and_a_missing_colord_is_srgb() {
        // reading the monitor's device profile fails (the real scan leaves such a device out): the
        // primary output's profile is the answer, as before per-monitor matching
        let mut colord = FakeColord::new();
        colord.profile_error = true;
        colord.outputs.retain(|o| o.primary);
        assert_eq!(path_for(&colord, Some(&on("HDMI-A-1"))), Some("/icc/dp1.icc".into()));
        // no daemon at all: nothing, so sRGB
        let mut down = FakeColord::new();
        down.down = true;
        assert_eq!(path_for(&down, Some(&on("DP-1"))), None);
        assert_eq!(path_for(&down, None), None);
        // no display devices
        let mut empty = FakeColord::new();
        empty.outputs.clear();
        empty.by_name.clear();
        assert_eq!(path_for(&empty, Some(&on("DP-1"))), None);
    }

    #[test]
    fn the_header_check_needs_signature_and_size() {
        let good = write_named(NamedSpace::Srgb);
        assert!(looks_like_icc(&good));
        let mut no_sig = good.clone();
        no_sig[36] = b'x';
        assert!(!looks_like_icc(&no_sig));
        assert!(!looks_like_icc(&good[..127]));
        assert!(!looks_like_icc(&[]));
    }
}
