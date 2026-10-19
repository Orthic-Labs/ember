//! The caller rules Ember puts on top of `rightkit-fs` for files it writes. The writes themselves
//! (temp file, sync order, replace, exclusive publish) are `rightkit-fs`'s.
//!
//! **Write policy** (issue #134): what can't be recreated (originals, XMP sidecars, the catalog,
//! settings, DNGs that replace raws, smart previews, an edit copy that becomes a library photo)
//! is written **durably**. What can always be made again from the originals (exports, renders,
//! screenshots, merge previews) is written **atomically** only (`durable: false`): a half-written
//! file never replaces a good one, but there is no per-file sync (on a USB drive or a NAS that
//! sync dominates a large export).
//!
//! Ember's one rule on top: an `FsError` for which [`rightkit_fs::FsError::target_replaced`] is
//! true means the new content is already in place and only a step after the replace failed (the
//! folder sync). That counts as written; it is logged ([`written`]).
//!
//! Every write here goes through [`rightkit_fs::AtomicWriter`], so the fault-injection and sync
//! counting seams of `rightkit-fs` (feature `test-seams`, dev-dependencies only) see it.

use std::io;
use std::path::{Path, PathBuf};

/// Ember's rule on a `rightkit-fs` result: new content in place with a later step failed
/// (`target_replaced`) counts as written and is logged; every other error is an error.
#[cfg(not(target_arch = "wasm32"))]
pub fn written(path: &Path, result: rightkit_fs::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.target_replaced() => {
            log::warn!("{}: written, but a step after the write failed: {e}", path.display());
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Stream `fill` into an [`rightkit_fs::AtomicWriter`] for `path` and publish it. An error from `fill` aborts: the writer's drop
/// removes the temp file and nothing is published. Returns the length written (the temp file's size once flushed).
#[cfg(not(target_arch = "wasm32"))]
fn stream(path: &Path, options: rightkit_fs::AtomicWriteOptions, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
    use io::Write;
    let mut writer = rightkit_fs::AtomicWriter::create_with(path, options)?;
    {
        let mut out = io::BufWriter::with_capacity(1 << 20, writer.writer());
        fill(&mut out)?;
        out.flush()?;
    }
    let n = writer.file_mut().metadata()?.len();
    written(path, writer.commit())?;
    Ok(n)
}

/// Replace (or create) `path` with `bytes` atomically; `durable` syncs the file and its folder
/// (see the module docs for which files are). The folder must exist.
pub fn replace(path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    replace_with(path, durable, &mut |w| w.write_all(bytes)).map(|_| ())
}

/// [`replace`] with streamed content: `fill` writes it (an error aborts and leaves the old file
/// and no temp file). Returns the length written.
#[cfg(not(target_arch = "wasm32"))]
pub fn replace_with(path: &Path, durable: bool, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
    stream(path, rightkit_fs::AtomicWriteOptions { durable, ..rightkit_fs::AtomicWriteOptions::default() }, fill)
}

/// A browser build has no file system to replace a file in.
#[cfg(target_arch = "wasm32")]
pub fn replace_with(_path: &Path, _durable: bool, _fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "this build has no file system"))
}

/// Create `path` with `bytes` only when nothing is at that name: written to a temp file beside it, flushed when `durable`, then
/// published exclusively, so the file never appears half written and an existing file (or one that appears meanwhile) is never
/// replaced (`AlreadyExists`). One write of the bytes, no read-back (see [`write_new`] for that).
pub fn write_exclusive(path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    write_exclusive_with(path, durable, &mut |w| w.write_all(bytes)).map(|_| ())
}

/// [`write_exclusive`] with streamed content: `fill` writes it (an error aborts: nothing is published and no temp file is left).
/// Returns the length written.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_exclusive_with(path: &Path, durable: bool, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
    let replace = rightkit_fs::ReplaceMode::NoReplace;
    stream(path, rightkit_fs::AtomicWriteOptions { durable, replace, ..rightkit_fs::AtomicWriteOptions::default() }, fill)
}

/// A browser build has no file system to write a file in.
#[cfg(target_arch = "wasm32")]
pub fn write_exclusive_with(_path: &Path, _durable: bool, _fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "this build has no file system"))
}

/// `name`, then `stem (1).ext`, `stem (2).ext`, ... up to `count` names (`rightkit_fs::names::numbered_candidates`): the one
/// numbering every "next free name" in Ember uses. A browser build has no file system, so it only offers `name`.
#[cfg(not(target_arch = "wasm32"))]
pub fn numbered(name: &str, count: usize) -> Vec<String> {
    rightkit_fs::names::numbered_candidates(name, count)
}

/// A browser build has no file system to look for a free name in.
#[cfg(target_arch = "wasm32")]
pub fn numbered(name: &str, count: usize) -> Vec<String> {
    std::iter::once(name.to_string()).take(count).collect()
}

/// Write `data` as the new file `path`: never replaces an existing file (`AlreadyExists`), and
/// the file only appears once it is complete, synced and read back identical.
pub fn write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    write_new_unique(std::slice::from_ref(&path.to_path_buf()), data).map(|_| ())
}

/// [`write_new`] under the first free name of `candidates` (all in the same folder; build them with [`numbered`]): the file is written once to a hidden temp file (durable), synced and read back
/// against its SHA-256 before anything is published (`commit_checked`), and then published under the first name that is not
/// taken, without rewriting it ([`rightkit_fs::publish_file_first_free`]). Returns that name; `AlreadyExists` when every
/// candidate is taken, `InvalidInput` for none.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_new_unique(candidates: &[PathBuf], data: &[u8]) -> io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);

    let Some(first) = candidates.first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file name to write"));
    };
    let dir = first.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // The temp name is ours (not `.rktmp-` shaped, so a sweep of the folder leaves it to this call).
    let tmp = dir.join(format!(".lightcraft-{}-{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let r: io::Result<PathBuf> = (|| {
        let mut writer = rightkit_fs::AtomicWriter::create_with(&tmp, rightkit_fs::AtomicWriteOptions::default())?;
        writer.write_all(data)?;
        // Kept (issue #134 point 4): these files replace raws (Convert / Copy as DNG) or become library photos (merges), so
        // the bytes on disk are compared with the bytes encoded before the file gets a real name.
        let want = rightkit_fs::digest::sha256_bytes(data);
        written(
            &tmp,
            writer.commit_checked(|temp| {
                if rightkit_fs::digest::sha256_file(temp)? == want {
                    Ok(())
                } else {
                    Err(io::Error::other("the written file reads back different (failing drive or connection?)"))
                }
            }),
        )?;
        match rightkit_fs::publish_file_first_free(&tmp, candidates, rightkit_fs::AtomicWriteOptions::default()) {
            Ok(winner) => Ok(winner),
            Err(e) if e.target_replaced() => {
                // published (the error names the winning candidate), a later step failed
                log::warn!("{e}");
                match &e {
                    rightkit_fs::FsError::ReplacedButDirSyncFailed { path, .. } | rightkit_fs::FsError::ReplacedButPostStepFailed { path, .. } => {
                        Ok(path.clone())
                    }
                    _ => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    })();
    // after a publish the temp name is gone; after a link-then-unlink fallback or a failure it may remain
    let _ = std::fs::remove_file(&tmp);
    r
}

/// A browser build has no file system to publish a file in.
#[cfg(target_arch = "wasm32")]
pub fn write_new_unique(_candidates: &[PathBuf], _data: &[u8]) -> io::Result<PathBuf> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "this build has no file system"))
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use rightkit_fs::atomic::seams;
    use std::fs;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-written-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn numbered(dir: &Path, name: &str, count: usize) -> Vec<PathBuf> {
        rightkit_fs::names::numbered_candidates(name, count).into_iter().map(|n| dir.join(n)).collect()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn replace_replaces_and_leaves_no_temp() {
        let d = temp("replace");
        let p = d.join("out.jpg");
        replace(&p, b"one", true).unwrap();
        replace(&p, b"two two", true).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["out.jpg"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_mid_write_keeps_the_old_file() {
        let d = temp("midwrite");
        let p = d.join("photo.jpg");
        fs::write(&p, b"the original bytes").unwrap();
        {
            let _f = seams::fail_write_after(4, io::ErrorKind::Other);
            let e = replace(&p, &[7u8; 10_000], true).unwrap_err();
            assert!(e.to_string().contains("injected"), "{e}");
            assert!(write_new(&d.join("new.dng"), &[1u8; 100]).is_err());
        }
        assert_eq!(fs::read(&p).unwrap(), b"the original bytes", "the target is untouched");
        assert_eq!(names(&d), vec!["photo.jpg"], "no temp file and no partial new file");
        // a fill error aborts the same way
        let r = replace_with(&p, true, &mut |w| {
            w.write_all(b"half")?;
            Err(io::Error::other("encoder failed"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read(&p).unwrap(), b"the original bytes");
        assert_eq!(names(&d), vec!["photo.jpg"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn recreatable_outputs_are_atomic_without_a_sync() {
        let d = temp("nosync");
        let p = d.join("export.jpg");
        seams::reset_sync_counts();
        replace(&p, b"one", false).unwrap();
        replace(&p, b"two two", false).unwrap();
        assert_eq!(seams::take_sync_counts(), seams::SyncCounts::default(), "no file or folder sync");
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["export.jpg"], "no temp file left");
        // still atomic: a failure part-way leaves the previous file and no temp file
        {
            let _f = seams::fail_write_after(3, io::ErrorKind::Other);
            assert!(replace(&p, &[9u8; 10_000], false).is_err());
        }
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["export.jpg"]);
        // the durable writers do sync
        seams::reset_sync_counts();
        replace(&p, b"three", true).unwrap();
        assert!(seams::take_sync_counts().file_syncs > 0, "a durable replace flushes the file");
        write_new(&d.join("new.dng"), b"dng").unwrap();
        assert!(seams::take_sync_counts().file_syncs > 0, "a new file is flushed too");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn new_files_never_replace_anything() {
        let d = temp("new");
        let p = d.join("a.dng");
        fs::write(&p, b"keep").unwrap();
        assert_eq!(write_new(&p, b"other").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&p).unwrap(), b"keep");
        assert_eq!(names(&d), vec!["a.dng"], "a refused write leaves no temp file");
        let c = numbered(&d, "a.dng", 10);
        let got = write_new_unique(&c, b"fresh").unwrap();
        assert_eq!(got, d.join("a (1).dng"));
        assert_eq!(fs::read(&got).unwrap(), b"fresh");
        assert_eq!(fs::read(&p).unwrap(), b"keep");
        assert_eq!(names(&d), vec!["a (1).dng", "a.dng"], "the temp file is gone");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn exclusive_writes_never_replace_and_leave_no_temp() {
        let d = temp("exclusive");
        let p = d.join("a.bak");
        write_exclusive(&p, b"first", true).unwrap();
        assert_eq!(write_exclusive(&p, b"second", true).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&p).unwrap(), b"first");
        assert_eq!(names(&d), vec!["a.bak"], "a refused write leaves no temp file");
        {
            let _f = seams::fail_write_after(2, io::ErrorKind::Other);
            assert!(write_exclusive(&d.join("b.bak"), &[1u8; 100], true).is_err());
        }
        assert_eq!(names(&d), vec!["a.bak"], "a failed write leaves neither a partial file nor a temp file");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn many_taken_names_are_skipped_and_all_taken_is_an_error() {
        let d = temp("batches");
        let c = numbered(&d, "n.dng", 400);
        for taken in c.iter().take(300) {
            fs::write(taken, b"x").unwrap();
        }
        let got = write_new_unique(&c, b"fresh").unwrap();
        assert_eq!(got, d.join("n (300).dng"));
        for taken in c.iter().take(300) {
            assert_eq!(fs::read(taken).unwrap(), b"x", "taken names keep their bytes");
        }
        // every candidate taken: refused, nothing replaced, no temp file
        let few = numbered(&d, "n.dng", 3);
        assert_eq!(write_new_unique(&few, b"never").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(write_new_unique(&[], b"never").unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&few[0]).unwrap(), b"x");
        assert!(names(&d).iter().all(|n| n.starts_with('n')), "no temp file: {:?}", names(&d));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_write_of_a_new_file_publishes_nothing() {
        let d = temp("failnew");
        let c = numbered(&d, "r.dng", 3);
        {
            let _f = seams::fail_write_after(2, io::ErrorKind::Other);
            assert!(write_new_unique(&c, &[5u8; 100]).is_err());
        }
        assert!(names(&d).is_empty(), "nothing published, no temp file: {:?}", names(&d));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn written_counts_a_replace_with_a_failed_later_step_as_done() {
        let d = temp("replaced");
        let p = d.join("x");
        let e = rightkit_fs::FsError::ReplacedButDirSyncFailed { path: p.clone(), source: io::Error::other("sync") };
        assert!(written(&p, Err(e)).is_ok());
        assert!(written(&p, Err(rightkit_fs::FsError::AlreadyExists(p.clone()))).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
