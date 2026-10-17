//! Rendered thumbnails on disk, bounded in bytes. Native builds keep them in a `rightkit-cache`
//! `DiskCache` (the RightKit disk-cache owner): `<dir>/<2 hex>/<64 hex>-<16 hex>.jpg`, written via
//! temp + rename (readers never see partial files) but not fsynced, evicted least recently used
//! first on every write so the folder never stays above its budget, recency kept across restarts.
//! Unreadable entries are deleted and re-rendered.
//!
//! The cache only ever lists, counts or deletes files of its own name shape: the directory may be
//! shared with other files (e.g. a library opened on a photo folder that already has a `thumbs/`
//! folder), and those are never touched (issue #98). A shard directory that is a symlink or junction
//! is not followed (issue #1504).
//!
//! **Older folders.** Before the RightKit cache the files were `<2 hex>/<32 hex>.jpg`. Opening a
//! folder renames each of them (newest first, up to the budget) into the cache's layout, so the
//! thumbnails stay valid; what does not fit the budget is deleted, as the old pruning did.
//!
//! wasm32 has no file system: there the cache is an inert stand-in (every lookup misses).

use lightcraft_raster::Rgba8;

const QUALITY: u8 = 90;

#[cfg(not(target_arch = "wasm32"))]
pub use native::DiskCache;
#[cfg(target_arch = "wasm32")]
pub use stub::DiskCache;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, PoisonError};
    use std::time::SystemTime;

    use lightcraft_raster::Rgba8;
    use rightkit_cache::{Config, Key, PutOutcome};

    use super::{decode_jpeg, encode_jpeg, is_cache_file, is_lower_hex, is_real_dir, is_shard_name};
    use crate::Hash128;

    pub struct DiskCache {
        dir: PathBuf,
        /// Bytes the cache may hold (changed by [`DiskCache::set_budget`]).
        budget: AtomicU64,
        /// The RightKit cache over `dir`, opened on first use: opening indexes the folder and trims
        /// it to the budget, which `new` must not do (it is also used to look at a folder).
        store: Mutex<Option<rightkit_cache::DiskCache>>,
        pub hits: AtomicU64,
        pub misses: AtomicU64,
        pub writes: AtomicU64,
    }

    /// The cache key of a thumbnail: the content key's hex, so the same key always lands in the same slot.
    fn key_of(key: Hash128) -> Key {
        let hex = key.to_string();
        Key::new(&[hex.as_str()])
    }

    impl DiskCache {
        pub fn new(dir: &Path, budget: u64) -> DiskCache {
            DiskCache {
                dir: dir.to_path_buf(),
                budget: AtomicU64::new(budget),
                store: Mutex::new(None),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
                writes: AtomicU64::new(0),
            }
        }

        pub fn dir(&self) -> &Path {
            &self.dir
        }

        /// Bytes the cache may hold.
        pub fn budget(&self) -> u64 {
            self.budget.load(Ordering::Relaxed)
        }

        /// Change the budget: the files stay valid (they are keyed by content), so a resize keeps
        /// them; the cache is reopened under the new cap by the next use, which trims it to the cap.
        pub fn set_budget(&self, bytes: u64) {
            self.budget.store(bytes, Ordering::Relaxed);
            *self.store.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }

        /// The RightKit cache, opened (and older-layout files migrated) when not yet. `create` false
        /// never creates a folder that is gone (a drive that is not connected): that is a miss.
        fn open(&self, create: bool) -> Option<rightkit_cache::DiskCache> {
            let mut slot = self.store.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(store) = slot.as_ref() {
                return Some(store.clone());
            }
            if !create && !self.dir.is_dir() {
                return None;
            }
            match open_migrated(&self.dir, self.budget()) {
                Ok(store) => {
                    *slot = Some(store.clone());
                    Some(store)
                }
                Err(e) => {
                    log::warn!("preview cache: cannot open {}: {e}", self.dir.display());
                    None
                }
            }
        }

        /// The cache for clearing or moving: the open one, else one opened with no cap, so looking
        /// after a folder never trims it. `None` when the folder is not there.
        fn manage(&self) -> Option<rightkit_cache::DiskCache> {
            let slot = self.store.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(store) = slot.as_ref() {
                return Some(store.clone());
            }
            drop(slot);
            if !self.dir.is_dir() {
                return None;
            }
            match open_migrated(&self.dir, u64::MAX) {
                Ok(store) => Some(store),
                Err(e) => {
                    log::warn!("preview cache: cannot open {}: {e}", self.dir.display());
                    None
                }
            }
        }

        pub fn get(&self, key: Hash128) -> Option<Rgba8> {
            let k = key_of(key);
            let Some(bytes) = self.open(false).and_then(|s| s.get(&k)) else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            match decode_jpeg(&bytes) {
                Some(img) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    Some(img)
                }
                None => {
                    log::warn!("preview cache: dropping unreadable entry of {key} in {}", self.dir.display());
                    if let Some(s) = self.open(false) {
                        let _ = s.remove(&k);
                    }
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        }

        pub fn put(&self, key: Hash128, img: &Rgba8) {
            let Some(bytes) = encode_jpeg(img) else { return };
            let Some(store) = self.open(true) else { return };
            match store.put(&key_of(key), &bytes) {
                Ok(PutOutcome::Stored) => {
                    self.writes.fetch_add(1, Ordering::Relaxed);
                }
                // larger than the whole budget: the image stays in memory only
                Ok(_) => {}
                Err(e) => log::warn!("preview cache: could not store {key} in {}: {e}", self.dir.display()),
            }
        }

        /// The stored file of `key` (tests).
        #[doc(hidden)]
        pub fn entry_path(&self, key: Hash128) -> Option<PathBuf> {
            self.open(false)?.get_path(&key_of(key))
        }

        /// Bytes on disk: the cache's entries, and any older-layout files not migrated yet.
        pub fn size(&self) -> u64 {
            let open = self.store.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let held = match open {
                Some(store) => store.stats().bytes,
                // a look at the folder: nothing is created, trimmed or removed
                None => rightkit_cache::DiskCache::open_read_only(&self.dir).map_or(0, |s| s.stats().bytes),
            };
            held.saturating_add(legacy_files(&self.dir).iter().map(|f| f.len).sum())
        }

        pub fn clear(&self) {
            if let Some(store) = self.manage() {
                let _ = store.clear();
            }
            for f in legacy_files(&self.dir) {
                let _ = fs::remove_file(&f.path);
            }
        }

        /// Move this cache's files into `to`, in the same layout: the thumbnails stay valid (they are
        /// keyed by content). Only cache files are touched. Returns how many moved and what failed.
        pub fn move_to(&self, to: &Path) -> (usize, Vec<String>) {
            // older-layout files are brought into the cache's layout first
            if self.manage().is_none() {
                return (0, Vec::new());
            }
            let (mut moved, mut failed) = (0, Vec::new());
            for (p, shard, name) in entry_files(&self.dir) {
                let dir = to.join(&shard);
                let dst = dir.join(&name);
                let done = fs::create_dir_all(&dir).and_then(|()| {
                    if dst.exists() {
                        // the same key is there already (same content): the old copy is redundant
                        fs::remove_file(&p)
                    } else if fs::rename(&p, &dst).is_ok() {
                        Ok(())
                    } else {
                        fs::copy(&p, &dst).and_then(|_| fs::remove_file(&p))
                    }
                });
                match done {
                    Ok(()) => moved += 1,
                    Err(e) => failed.push(format!("{}: {e}", p.display())),
                }
            }
            // the moved files are gone from the index's folder: it is rebuilt on the next use
            *self.store.lock().unwrap_or_else(PoisonError::into_inner) = None;
            (moved, failed)
        }
    }

    /// Open the RightKit cache over `dir` at `budget`, then bring older-layout files into it.
    fn open_migrated(dir: &Path, budget: u64) -> io::Result<rightkit_cache::DiskCache> {
        let store = rightkit_cache::DiskCache::open(dir, Config::new(budget))?;
        migrate_legacy(&store, dir, budget);
        Ok(store)
    }

    /// A file of the layout before the RightKit cache: `<2 hex>/<32 hex>.jpg`, or a `.tmp…` leftover.
    struct Legacy {
        path: PathBuf,
        len: u64,
        modified: SystemTime,
        /// The content key's 32 hex digits.
        hex: String,
        temp: bool,
    }

    /// The older-layout files in `dir` (only names of that shape; shards that are links are skipped).
    fn legacy_files(dir: &Path) -> Vec<Legacy> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(dir) else { return out };
        for sub in rd.flatten() {
            let shard = sub.file_name();
            let Some(shard) = shard.to_str() else { continue };
            if !is_shard_name(shard) || !is_real_dir(&sub.path()) {
                continue;
            }
            let Ok(files) = fs::read_dir(sub.path()) else { continue };
            for f in files.flatten() {
                let name = f.file_name();
                let Some(name) = name.to_str() else { continue };
                if !is_cache_file(shard, name) {
                    continue;
                }
                // `DirEntry::metadata` does not follow symlinks: a link is never a cache file
                let Ok(m) = f.metadata() else { continue };
                if !m.is_file() {
                    continue;
                }
                let Some((hex, ext)) = name.split_once('.') else { continue };
                out.push(Legacy {
                    path: f.path(),
                    len: m.len(),
                    modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    hex: hex.to_string(),
                    temp: ext != "jpg",
                });
            }
        }
        out
    }

    /// Rename each older-layout thumbnail into the cache (newest first, while they fit `budget`); delete
    /// the rest and the temp leftovers. A file that cannot be brought over stays where it is.
    fn migrate_legacy(store: &rightkit_cache::DiskCache, dir: &Path, budget: u64) {
        let mut files = legacy_files(dir);
        if files.is_empty() {
            return;
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.modified));
        let mut kept = 0u64;
        let (mut moved, mut dropped) = (0usize, 0usize);
        for f in files {
            if f.temp || kept.saturating_add(f.len) > budget {
                let _ = fs::remove_file(&f.path);
                dropped += 1;
                continue;
            }
            match migrate_one(store, &f) {
                Ok(()) => {
                    kept += f.len;
                    moved += 1;
                }
                Err(e) => log::warn!("preview cache: could not bring {} into the new layout: {e}", f.path.display()),
            }
        }
        log::info!("preview cache: {} brought into the new layout ({moved} thumbnails kept, {dropped} dropped)", dir.display());
    }

    fn migrate_one(store: &rightkit_cache::DiskCache, f: &Legacy) -> io::Result<()> {
        let reservation = store.reserve_path(&Key::new(&[f.hex.as_str()]), "jpg")?;
        // the reserved path is inside the cache folder, so this is a rename on one volume
        fs::rename(&f.path, reservation.path())?;
        // (`TooLarge`, larger than the whole budget, is not an error: the commit removed the file)
        reservation.commit().map(|_: PutOutcome| ())
    }

    /// The files of the cache's own layout in `dir`: (path, shard folder name, file name). Shards that
    /// are links are skipped.
    fn entry_files(dir: &Path) -> Vec<(PathBuf, String, String)> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(dir) else { return out };
        for sub in rd.flatten() {
            let shard = sub.file_name();
            let Some(shard) = shard.to_str() else { continue };
            if !is_shard_name(shard) || !is_real_dir(&sub.path()) {
                continue;
            }
            let Ok(files) = fs::read_dir(sub.path()) else { continue };
            for f in files.flatten() {
                let name = f.file_name();
                let Some(name) = name.to_str() else { continue };
                if !is_entry_file(shard, name) {
                    continue;
                }
                if f.metadata().is_ok_and(|m| m.is_file()) {
                    out.push((f.path(), shard.to_string(), name.to_string()));
                }
            }
        }
        out
    }

    /// Whether `name` inside shard `shard` is an entry of the `rightkit-cache` layout
    /// (`<64 hex>-<16 hex>[.<1-8 alphanumerics>]`, the hash starting with the shard).
    fn is_entry_file(shard: &str, name: &str) -> bool {
        let (base, ext) = match name.split_once('.') {
            Some((base, ext)) => (base, Some(ext)),
            None => (name, None),
        };
        if ext.is_some_and(|e| e.is_empty() || e.len() > 8 || !e.bytes().all(|b| b.is_ascii_alphanumeric())) {
            return false;
        }
        let Some((hash, group)) = base.split_once('-') else { return false };
        hash.len() == 64 && is_lower_hex(hash) && hash.starts_with(shard) && group.len() == 16 && is_lower_hex(group)
    }
}

#[cfg(target_arch = "wasm32")]
mod stub {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use lightcraft_raster::Rgba8;

    use crate::Hash128;

    /// A browser build has no file system: every lookup misses and nothing is stored.
    pub struct DiskCache {
        dir: std::path::PathBuf,
        budget: AtomicU64,
        pub hits: AtomicU64,
        pub misses: AtomicU64,
        pub writes: AtomicU64,
    }

    impl DiskCache {
        pub fn new(dir: &Path, budget: u64) -> DiskCache {
            DiskCache {
                dir: dir.to_path_buf(),
                budget: AtomicU64::new(budget),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
                writes: AtomicU64::new(0),
            }
        }
        pub fn dir(&self) -> &Path {
            &self.dir
        }
        pub fn budget(&self) -> u64 {
            self.budget.load(Ordering::Relaxed)
        }
        pub fn set_budget(&self, bytes: u64) {
            self.budget.store(bytes, Ordering::Relaxed);
        }
        pub fn get(&self, _key: Hash128) -> Option<Rgba8> {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
        pub fn put(&self, _key: Hash128, _img: &Rgba8) {}
        pub fn size(&self) -> u64 {
            0
        }
        pub fn clear(&self) {}
        pub fn move_to(&self, _to: &Path) -> (usize, Vec<String>) {
            (0, Vec::new())
        }
    }
}

/// Whether `p` is a directory itself, not a symlink or Windows reparse point to one.
#[cfg(not(target_arch = "wasm32"))]
fn is_real_dir(p: &std::path::Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A shard directory name: two lowercase hex digits.
fn is_shard_name(s: &str) -> bool {
    s.len() == 2 && is_lower_hex(s)
}

/// Whether `name` inside shard directory `shard` is a file of the layout this cache had before the
/// RightKit cache: `<32 hex>.jpg` (whose first two digits are the shard) or a
/// `<32 hex>.tmp<alphanumeric>` temp file. Such files are brought into the new layout (or deleted)
/// when a folder is opened; anything else in the folder belongs to someone else.
pub fn is_cache_file(shard: &str, name: &str) -> bool {
    if !is_shard_name(shard) {
        return false;
    }
    let Some((stem, ext)) = name.split_once('.') else { return false };
    if stem.len() != 32 || !is_lower_hex(stem) || !stem.starts_with(shard) {
        return false;
    }
    ext == "jpg" || ext.strip_prefix("tmp").is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Encode a thumbnail as the cache stores it (JPEG, RGB, 4:2:0).
pub fn encode_jpeg(img: &Rgba8) -> Option<Vec<u8>> {
    if img.width == 0 || img.height == 0 || img.width > u16::MAX as usize || img.height > u16::MAX as usize {
        return None;
    }
    let mut rgb = Vec::with_capacity(img.width * img.height * 3);
    for p in &img.data {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut out, QUALITY);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    enc.encode(&rgb, img.width as u16, img.height as u16, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(out)
}

/// Decode a cached thumbnail (see [`encode_jpeg`]).
pub fn decode_jpeg(bytes: &[u8]) -> Option<Rgba8> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let px = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if px.len() < w * h * 3 {
        return None;
    }
    let data = px.as_chunks::<3>().0.iter().take(w * h).map(|c| [c[0], c[1], c[2], 255]).collect();
    Some(Rgba8 { width: w, height: h, data })
}
