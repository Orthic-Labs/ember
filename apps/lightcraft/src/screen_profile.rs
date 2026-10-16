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
//! profiles in) over the system D-Bus for the default profile of the primary display output; it
//! can't tell which output the window is on, so a window on a secondary monitor still gets the
//! primary one's profile. A missing, unreadable or invalid profile is `None`: sRGB.
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

/// The default profile colord assigns to the primary display output, or `None` without colord, a
/// display device or a readable profile. Asked at most every few seconds (and rarely again after a
/// failure), so a missing daemon can't stall the UI thread that polls this.
#[cfg(target_os = "linux")]
pub fn icc() -> Option<Vec<u8>> {
    use std::time::{Duration, Instant};
    const REFRESH: Duration = Duration::from_secs(3);
    const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30);
    static LAST: Mutex<Option<(Instant, Option<Vec<u8>>)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, value)) = last.as_ref()
        && at.elapsed() < if value.is_some() { REFRESH } else { RETRY_AFTER_FAILURE }
    {
        return value.clone();
    }
    let value = colord_profile_path().and_then(|path| read_cached(&path));
    *last = Some((Instant::now(), value.clone()));
    value
}

/// One colord display device, as far as choosing its profile goes.
struct Output {
    /// colord's `OutputPriority` metadata says "primary".
    primary: bool,
    /// The file of the device's default (first) profile, when it has one.
    profile: Option<PathBuf>,
}

/// The profile of the primary output, else of the first output that has one.
fn pick_output_profile(outputs: &[Output]) -> Option<&Path> {
    let with_profile = |primary_only: bool| outputs.iter().find(|o| o.profile.is_some() && (o.primary || !primary_only));
    with_profile(true).or_else(|| with_profile(false)).and_then(|o| o.profile.as_deref())
}

/// Ask colord (`org.freedesktop.ColorManager`, system bus) for the display devices and their
/// default profiles' files.
#[cfg(target_os = "linux")]
fn colord_profile_path() -> Option<PathBuf> {
    use std::collections::HashMap;
    use zbus::blocking::{Proxy, connection::Builder};
    use zbus::zvariant::OwnedObjectPath;
    const DEST: &str = "org.freedesktop.ColorManager";
    let conn = Builder::system().ok()?.method_timeout(std::time::Duration::from_secs(2)).build().ok()?;
    let manager = Proxy::new(&conn, DEST, "/org/freedesktop/ColorManager", "org.freedesktop.ColorManager").ok()?;
    let devices: Vec<OwnedObjectPath> = manager.call("GetDevicesByKind", &("display",)).ok()?;
    let mut outputs = Vec::new();
    for device in devices.iter().take(16) {
        let Ok(device) = Proxy::new(&conn, DEST, device.as_str().to_string(), "org.freedesktop.ColorManager.Device") else { continue };
        let metadata: HashMap<String, String> = device.get_property("Metadata").unwrap_or_default();
        let primary = metadata.get("OutputPriority").is_some_and(|p| p == "primary");
        let profiles: Vec<OwnedObjectPath> = device.get_property("Profiles").unwrap_or_default();
        // colord lists the profile in use first
        let profile = profiles.first().and_then(|path| {
            let profile = Proxy::new(&conn, DEST, path.as_str().to_string(), "org.freedesktop.ColorManager.Profile").ok()?;
            let file: String = profile.get_property("Filename").ok()?;
            (!file.is_empty()).then(|| PathBuf::from(file))
        });
        outputs.push(Output { primary, profile });
    }
    pick_output_profile(&outputs).map(Path::to_path_buf)
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
        let out = |primary: bool, file: Option<&str>| Output { primary, profile: file.map(PathBuf::from) };
        let pick = |outputs: &[Output]| pick_output_profile(outputs).map(|p| p.to_string_lossy().to_string());
        assert_eq!(pick(&[]), None);
        assert_eq!(pick(&[out(true, None), out(false, None)]), None);
        assert_eq!(pick(&[out(false, Some("a.icc")), out(true, Some("b.icc"))]), Some("b.icc".into()));
        // a primary output without a profile: the first output that has one
        assert_eq!(pick(&[out(false, None), out(true, None), out(false, Some("c.icc"))]), Some("c.icc".into()));
        assert_eq!(pick(&[out(false, Some("a.icc")), out(false, Some("b.icc"))]), Some("a.icc".into()));
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
