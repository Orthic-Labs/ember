//! Where the thumbnail cache is kept: in the library's own `thumbs` folder, or in a folder the user
//! chose. The cache is keyed by content, so its files can be moved or dropped and stay valid. A
//! chosen folder never sits among the library's originals, and while a chosen folder is not there
//! (a drive that is not connected) the library's own folder is used, so the choice comes back with
//! the drive.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The folder the cache uses now: `chosen` when it exists and lies outside `originals`, else
/// `default`. Looks at the folder only; [`at_open`] also checks that it can be written.
pub fn in_use(default: &Path, chosen: Option<&Path>, originals: &Path) -> PathBuf {
    chosen.filter(|d| d.is_dir() && check_outside_originals(d, originals).is_ok()).map_or_else(|| default.to_path_buf(), Path::to_path_buf)
}

/// [`in_use`], then the writability of a chosen folder (a read-only drive): the library's own folder
/// when it cannot be written, with a log line. Used when the library opens and when the cache is
/// attached.
pub fn at_open(default: &Path, chosen: Option<&Path>, originals: &Path) -> PathBuf {
    let dir = in_use(default, chosen, originals);
    if dir.as_path() != default
        && let Err(why) = crate::smart::check_writable(&dir)
    {
        log::warn!("thumbnail cache: {why}; using the library's folder");
        return default.to_path_buf();
    }
    dir
}

/// Refuse a cache folder that is, lies inside, or holds the library's originals: the cache never
/// shares a folder with the user's photos.
pub fn check_outside_originals(dir: &Path, originals: &Path) -> Result<(), String> {
    let (d, o) = (comparable(dir), comparable(originals));
    if d.starts_with(&o) || o.starts_with(&d) {
        return Err(format!("{} is inside the library's originals ({}): choose another folder", dir.display(), originals.display()));
    }
    Ok(())
}

/// Whether `p` is absolute and has no `..` part (what a chosen folder must be).
pub fn plain_absolute(p: &Path) -> bool {
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// `p` with its existing part resolved (symlinks, Windows short names and verbatim prefixes), so
/// both sides of a comparison are spelled the same way; the part that does not exist yet is kept.
fn comparable(p: &Path) -> PathBuf {
    let mut tail: Vec<OsString> = Vec::new();
    let mut cur = p;
    loop {
        if let Ok(resolved) = std::fs::canonicalize(cur) {
            return tail.iter().rev().fold(resolved, |acc, part| acc.join(part));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-cache-location-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_folder_falls_back_to_the_library_folder() {
        let lib = scratch("missing");
        let (default, originals) = (lib.join("thumbs"), lib.join("Originals"));
        let gone = lib.join("gone-drive").join("cache");
        assert_eq!(in_use(&default, Some(&gone), &originals), default);
        assert_eq!(in_use(&default, None, &originals), default);
        assert_eq!(at_open(&default, Some(&gone), &originals), default);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn a_usable_folder_is_used_and_one_inside_the_originals_is_refused() {
        let lib = scratch("usable");
        let (default, originals) = (lib.join("thumbs"), lib.join("Originals"));
        std::fs::create_dir_all(&originals).unwrap();
        let other = scratch("elsewhere");
        assert_eq!(in_use(&default, Some(&other), &originals), other);
        assert_eq!(at_open(&default, Some(&other), &originals), other);
        let inside = originals.join("cache");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(check_outside_originals(&inside, &originals).is_err());
        assert!(check_outside_originals(&originals, &originals).is_err());
        assert!(check_outside_originals(&lib, &originals).is_err(), "a folder that holds the originals is refused too");
        assert!(check_outside_originals(&other, &originals).is_ok());
        assert_eq!(in_use(&default, Some(&inside), &originals), default);
        let _ = std::fs::remove_dir_all(&lib);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn a_folder_under_originals_that_do_not_exist_yet_is_refused() {
        let lib = scratch("not-yet");
        let originals = lib.join("Originals");
        assert!(check_outside_originals(&originals.join("a").join("b"), &originals).is_err());
        assert!(check_outside_originals(&lib.join("cache"), &originals).is_ok());
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn only_plain_absolute_folders_are_chosen() {
        let abs = std::env::temp_dir();
        assert!(plain_absolute(&abs));
        assert!(!plain_absolute(Path::new("relative/cache")));
        assert!(!plain_absolute(&abs.join("..").join("cache")));
    }
}
