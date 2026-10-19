//! Output files of the CLI's evaluation and extraction tools, published whole through `rightkit-fs`
//! (a crash leaves no half-written file at the final name).

use std::io::Write;
use std::path::Path;

use rightkit_fs::{AtomicWriteOptions, FsError, ReplaceMode};

/// Options for a file that must not exist yet: the publish is refused with
/// [`FsError::AlreadyExists`] when the name is taken, also when it was taken meanwhile.
pub fn new_file_options() -> AtomicWriteOptions {
    AtomicWriteOptions { replace: ReplaceMode::NoReplace, ..AtomicWriteOptions::default() }
}

/// True when something (a file, a directory or a link, a dangling one included) already has this
/// name. Tools check it before slow work so an existing output fails early; the publish itself
/// still refuses to replace it.
pub fn name_taken(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Write `bytes` as the new file `path`, never replacing an existing one. The new file counts as
/// written when only a step after the publish failed.
pub fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), FsError> {
    match rightkit_fs::atomic_write_with_options(path, new_file_options(), |file| file.write_all(bytes)) {
        Err(e) if e.target_replaced() => Ok(()),
        other => other,
    }
}

/// Replace (or create) `path` with `bytes`. The new file counts as written when only a step after
/// the replace failed.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match rightkit_fs::atomic_write(path, bytes) {
        Err(e) if e.target_replaced() => Ok(()),
        other => other.map_err(std::io::Error::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ember-output-file-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_new_output_is_published_whole_and_never_replaces_a_file() {
        let dir = scratch("new");
        let path = dir.join("report.json");
        publish_new(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert!(matches!(publish_new(&path, b"second"), Err(FsError::AlreadyExists(_))));
        assert_eq!(std::fs::read(&path).unwrap(), b"first", "the existing file is untouched");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no temp file is left behind: {names:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_output_refuses_a_dangling_link_at_its_name() {
        let dir = scratch("link");
        let path = dir.join("report.json");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("nowhere"), &path).unwrap();
            assert!(name_taken(&path), "a dangling link still holds the name");
            assert!(publish_new(&path, b"x").is_err());
            assert!(!dir.join("nowhere").exists(), "the link is not written through");
        }
        #[cfg(not(unix))]
        assert!(!name_taken(&path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replace_overwrites_atomically() {
        let dir = scratch("replace");
        let path = dir.join("out.txt");
        std::fs::write(&path, b"old").unwrap();
        replace(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(replace(&dir.join("missing").join("out.txt"), b"x").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
