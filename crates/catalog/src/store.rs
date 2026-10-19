//! Byte storage for the catalog files (`catalog.snap`, `catalog.log`).
//!
//! [`FsStore`] is the native implementation (a library directory); [`MemStore`] keeps files in
//! memory (tests, ephemeral sessions) and lets tests simulate crashes by editing the bytes. A web
//! host can implement [`Store`] over OPFS.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A flat namespace of named files with the few operations the journal needs.
pub trait Store: Send {
    /// The whole file, or `None` if it doesn't exist.
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Replace a file atomically: after a crash either the old or the new content is visible,
    /// never a mix (native: write temp + fsync + rename + fsync directory).
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()>;
    /// [`Store::write_atomic`] with streamed content: `fill` writes the new content into the
    /// given writer (an error aborts and leaves the old file). Same durability: the file is
    /// replaced only once all of it is written (native: and fsynced). Returns its length.
    ///
    /// The default buffers the content in memory; [`FsStore`] streams it into the temp file, so
    /// a large snapshot is never held in memory as a whole.
    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        let mut buf = Vec::new();
        fill(&mut buf)?;
        self.write_atomic(name, &buf)?;
        Ok(buf.len() as u64)
    }
    /// Append and make durable (fsync) before returning. On an error, part of `data` may have
    /// been written (a full disk, a dropped network share): the journal cuts the file back to its
    /// previous length with [`Store::truncate`] before appending again.
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()>;
    /// Cut a file to `len` bytes (drop a torn tail before appending again). A missing file is
    /// left missing.
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()>;
    /// Human-readable location (diagnostics).
    fn describe(&self) -> String;
    /// Another handle on the same files that a worker thread can write `catalog.snap` through
    /// while this one keeps appending to the log (background compaction). `None` (the default):
    /// snapshots are written synchronously.
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        None
    }
}

/// Files in a directory.
pub struct FsStore {
    dir: PathBuf,
}

impl FsStore {
    /// Use `dir` (created if missing).
    pub fn open(dir: impl AsRef<Path>) -> io::Result<FsStore> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // each folder this creates is made durable; when only that sync fails the library folder is there and usable
            if let Err(e) = rightkit_fs::atomic::create_dir_all_durable(dir.as_ref(), false) {
                if !dir.as_ref().is_dir() {
                    return Err(e.into());
                }
                log::warn!("library folder {}: created, but making it durable failed: {e}", dir.as_ref().display());
            }
            // temp files of a save a crash cut short (only those of writers confirmed dead)
            let _ = rightkit_fs::sweep_stale_temps(dir.as_ref(), std::time::Duration::ZERO);
        }
        #[cfg(target_arch = "wasm32")]
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(FsStore { dir: dir.as_ref().to_path_buf() })
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Store for FsStore {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(name)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.write_atomic_with(name, &mut |w| w.write_all(data)).map(|_| ())
    }

    fn write_atomic_with(&mut self, name: &str, fill: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>) -> io::Result<u64> {
        crate::written::replace_with(&self.path(name), true, fill)
    }

    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        // One durable append (written, flushed, and the folder flushed when the file is created). An error may leave part of
        // `data` in the file; the caller cuts it back ([`Store::truncate`]) before appending again.
        #[cfg(not(target_arch = "wasm32"))]
        {
            rightkit_fs::append_record(&self.path(name), data, rightkit_fs::AppendOptions::default())?;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (name, data);
            Err(io::Error::new(io::ErrorKind::Unsupported, "this build has no file system"))
        }
    }

    // Kept as app code (rightkit-fs leaves torn-tail repair to the app: fs/append.rs): the journal cuts a failed append's partial
    // record before the next append. Proof: tests_torn_append.rs.
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        let f = match std::fs::OpenOptions::new().write(true).open(self.path(name)) {
            Ok(f) => f,
            // nothing to cut (an append that failed before creating the file)
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        f.set_len(len)?;
        f.sync_all()
    }

    fn describe(&self) -> String {
        self.dir.display().to_string()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        Some(Box::new(FsStore { dir: self.dir.clone() }))
    }
}

/// In-memory files. Clones share the same files (so a test can "reopen" or corrupt them).
#[derive(Clone, Default)]
pub struct MemStore {
    pub files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Offer a [`Store::background_writer`] (off by default: snapshots stay synchronous).
    pub background: bool,
}

impl MemStore {
    pub fn new() -> MemStore {
        MemStore::default()
    }
    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }
    pub fn set(&self, name: &str, data: Vec<u8>) {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), data);
    }
}

impl Store for MemStore {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.get(name))
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.set(name, data.to_vec());
        Ok(())
    }
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).entry(name.to_string()).or_default().extend_from_slice(data);
        Ok(())
    }
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        if let Some(f) = self.files.lock().unwrap_or_else(|e| e.into_inner()).get_mut(name) {
            f.truncate(len as usize);
        }
        Ok(())
    }
    fn describe(&self) -> String {
        "memory".into()
    }
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        self.background.then(|| Box::new(self.clone()) as Box<dyn Store>)
    }
}

#[cfg(all(test, unix))]
mod tests_staging {
    use super::*;

    /// A link planted at the old staging name (`<name>.tmp`) is never written through: the file it points to keeps its
    /// bytes and the save still lands (issue #1502; the temp name is now `rightkit-fs`'s, created exclusively).
    #[test]
    fn a_link_at_the_staging_name_is_never_written_through() {
        let dir = std::env::temp_dir().join(format!("lc-staging-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("catalog.snap.tmp")).unwrap();
        store.write_atomic("catalog.snap", b"{}").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read(dir.join("catalog.snap")).unwrap(), b"{}");
        std::os::unix::fs::symlink(&victim, dir.join("catalog.log.tmp")).unwrap();
        store.write_atomic_with("catalog.log", &mut |w| w.write_all(b"[1]")).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read(dir.join("catalog.log")).unwrap(), b"[1]");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_replace {
    use super::*;

    #[test]
    fn atomic_writes_replace_the_file_and_leave_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("lc-store-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        store.write_atomic("catalog.snap", b"one").unwrap();
        let n = store.write_atomic_with("catalog.snap", &mut |w| w.write_all(b"two two")).unwrap();
        assert_eq!(n, 7);
        assert_eq!(store.read("catalog.snap").unwrap().as_deref(), Some(&b"two two"[..]));
        let failed = store.write_atomic_with("catalog.snap", &mut |w| {
            w.write_all(b"half")?;
            Err(io::Error::other("encoder failed"))
        });
        assert!(failed.is_err());
        assert_eq!(store.read("catalog.snap").unwrap().as_deref(), Some(&b"two two"[..]), "the old file survives a failed save");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(names, vec!["catalog.snap"], "no temp file left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_append {
    use super::*;

    /// The journal's append on a real folder: each append is flushed (the file, and the folder when the file is created), records
    /// follow each other, and a torn tail is cut back to the previous length before the next append (the app's torn-tail rule,
    /// kept beside `rightkit_fs::append_record`, which leaves it to the app).
    #[test]
    fn appends_are_flushed_and_a_torn_tail_is_cut_back_before_the_next_append() {
        use rightkit_fs::atomic::seams;
        let dir = std::env::temp_dir().join(format!("lc-store-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        seams::reset_sync_counts();
        store.append("catalog.log", b"one\n").unwrap();
        let first = seams::take_sync_counts();
        assert!(first.file_syncs >= 1, "the appended bytes are flushed: {first:?}");
        // rightkit-fs flushes a new file's folder entry on Unix; Windows has no folder sync (try_sync_dir is a no-op).
        if cfg!(unix) {
            assert!(first.dir_syncs >= 1, "the folder entry of a new file is flushed: {first:?}");
        }
        store.append("catalog.log", b"two\n").unwrap();
        assert!(seams::take_sync_counts().file_syncs >= 1);
        // a torn record (a full disk cut it short): the journal cuts the file back to the length it had, then appends again
        let good = std::fs::metadata(dir.join("catalog.log")).unwrap().len();
        store.append("catalog.log", b"thr").unwrap();
        store.truncate("catalog.log", good).unwrap();
        store.append("catalog.log", b"three\n").unwrap();
        assert_eq!(store.read("catalog.log").unwrap().as_deref(), Some(&b"one\ntwo\nthree\n"[..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A link planted at the log's name is never written through.
    #[cfg(unix)]
    #[test]
    fn an_append_never_follows_a_link_at_the_log_name() {
        let dir = std::env::temp_dir().join(format!("lc-store-append-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = FsStore::open(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("catalog.log")).unwrap();
        assert!(store.append("catalog.log", b"x\n").is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
