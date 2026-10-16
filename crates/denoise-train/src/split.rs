//! Deterministic train / validation / held-out splits by file content hash.
//!
//! The unit of the split is the file, identified by the SHA-256 of its bytes (not its path or name), so
//! renaming a file never moves it, byte-identical copies always land together (and are listed once), and
//! adding files to the corpus never reassigns an existing one. A file's bucket is `SHA-256(salt || hash)` mod 1000.
//!
//! Caveat that no hash can fix: two different files of the same scene (a burst) are different bytes and may
//! land in different splits. The CC0 corpus is one frame per camera/variant; do not add bursts without grouping.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Split {
    Train,
    Val,
    Heldout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitConfig {
    /// Changing the salt reshuffles every assignment; it is part of the recorded contract.
    pub salt: String,
    /// Share of files (per mille) going to validation and to held-out; the rest trains.
    pub val_permille: u32,
    pub heldout_permille: u32,
}

impl Default for SplitConfig {
    fn default() -> Self {
        SplitConfig { salt: "ember-denoise-split-v1".into(), val_permille: 100, heldout_permille: 150 }
    }
}

impl SplitConfig {
    fn validate(&self) -> Result<()> {
        if self.val_permille == 0 || self.heldout_permille == 0 || self.val_permille + self.heldout_permille >= 1000 {
            return Err(Error::Split("val and held-out shares must be positive and leave room for training".into()));
        }
        Ok(())
    }
}

pub type Hash = [u8; 32];

pub fn hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bucket (0..1000) of a content hash under `salt`.
pub fn bucket(hash: &Hash, salt: &str) -> u32 {
    let mut d = Sha256::new();
    d.update(salt.as_bytes());
    d.update(hash);
    let out = d.finalize();
    let mut eight = [0u8; 8];
    eight.copy_from_slice(out.get(..8).unwrap_or(&[0u8; 8]));
    (u64::from_be_bytes(eight) % 1000) as u32
}

/// Which split a content hash belongs to.
pub fn assign(hash: &Hash, cfg: &SplitConfig) -> Split {
    let b = bucket(hash, &cfg.salt);
    if b < cfg.heldout_permille {
        Split::Heldout
    } else if b < cfg.heldout_permille + cfg.val_permille {
        Split::Val
    } else {
        Split::Train
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitManifest {
    pub config: SplitConfig,
    pub train: Vec<FileEntry>,
    pub val: Vec<FileEntry>,
    pub heldout: Vec<FileEntry>,
    /// Paths dropped because their bytes duplicate an entry above (same hash, so same split anyway).
    pub duplicates: Vec<PathBuf>,
}

impl SplitManifest {
    pub fn files(&self, split: Split) -> &[FileEntry] {
        match split {
            Split::Train => &self.train,
            Split::Val => &self.val,
            Split::Heldout => &self.heldout,
        }
    }

    /// No content hash in more than one split (or twice in one), and every split non-empty.
    pub fn verify(&self) -> Result<()> {
        let mut seen: HashSet<&str> = HashSet::new();
        for (name, list) in [("train", &self.train), ("val", &self.val), ("heldout", &self.heldout)] {
            if list.is_empty() {
                return Err(Error::Split(format!(
                    "the {name} split is empty: the corpus is too small for these shares (add CC0 raws, or raise the share)"
                )));
            }
            for f in list {
                if !seen.insert(f.sha256.as_str()) {
                    return Err(Error::Split(format!("{} (sha256 {}) appears in more than one split or twice", f.path.display(), f.sha256)));
                }
            }
        }
        Ok(())
    }

    /// Same assignment as `assign`, re-derived from the recorded hashes: catches a hand-edited manifest.
    pub fn verify_assignments(&self) -> Result<()> {
        for (split, list) in [(Split::Train, &self.train), (Split::Val, &self.val), (Split::Heldout, &self.heldout)] {
            for f in list {
                let h = parse_hex(&f.sha256).ok_or_else(|| Error::Split(format!("bad sha256 for {}", f.path.display())))?;
                if assign(&h, &self.config) != split {
                    return Err(Error::Split(format!("{} is in {split:?} but its hash assigns it elsewhere", f.path.display())));
                }
            }
        }
        Ok(())
    }
}

pub fn parse_hex(s: &str) -> Option<Hash> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// SHA-256 of a file's bytes, streamed.
pub fn hash_file(path: &Path) -> Result<Hash> {
    use std::io::Read;
    let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut d = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        d.update(buf.get(..n).unwrap_or(&[]));
    }
    Ok(d.finalize().into())
}

const RAW_EXTENSIONS: [&str; 14] = ["arw", "cr2", "cr3", "dng", "nef", "nrw", "orf", "raf", "rw2", "pef", "srw", "srf", "sr2", "3fr"];

/// Raw files under `dir` (recursive, sorted by path for stable listings).
pub fn scan_raws(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut depth_guard = 0usize;
    while let Some(d) = stack.pop() {
        depth_guard += 1;
        if depth_guard > 10_000 {
            return Err(Error::Data("corpus directory tree is unreasonably large".into()));
        }
        for entry in std::fs::read_dir(&d)? {
            let p = entry?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| RAW_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Split already hashed files. Order of `files` does not matter: entries are sorted by hash.
pub fn build_manifest_from_hashes(files: Vec<(PathBuf, Hash)>, cfg: &SplitConfig) -> Result<SplitManifest> {
    cfg.validate()?;
    let mut files = files;
    // path-sorted first, so which duplicate is kept does not depend on listing order
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut seen: HashSet<Hash> = HashSet::new();
    let mut m = SplitManifest { config: cfg.clone(), train: vec![], val: vec![], heldout: vec![], duplicates: vec![] };
    for (path, h) in files {
        if !seen.insert(h) {
            m.duplicates.push(path);
            continue;
        }
        let entry = FileEntry { path, sha256: hex(&h) };
        match assign(&h, cfg) {
            Split::Train => m.train.push(entry),
            Split::Val => m.val.push(entry),
            Split::Heldout => m.heldout.push(entry),
        }
    }
    for list in [&mut m.train, &mut m.val, &mut m.heldout] {
        list.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    }
    Ok(m)
}

/// Hash and split every raw under `dir`.
pub fn build_manifest(dir: &Path, cfg: &SplitConfig) -> Result<SplitManifest> {
    let paths = scan_raws(dir)?;
    if paths.is_empty() {
        return Err(Error::Split(format!("no raw files under {} (run `cargo xtask corpus --download`)", dir.display())));
    }
    let mut hashed = Vec::with_capacity(paths.len());
    for p in paths {
        let h = hash_file(&p)?;
        hashed.push((p, h));
    }
    build_manifest_from_hashes(hashed, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_hashes(n: usize) -> Vec<(PathBuf, Hash)> {
        (0..n).map(|i| (PathBuf::from(format!("f{i}.arw")), Sha256::digest(format!("file-{i}").as_bytes()).into())).collect()
    }

    #[test]
    fn assignment_is_deterministic_and_independent_of_listing_order() {
        let cfg = SplitConfig::default();
        let files = synthetic_hashes(500);
        let a = build_manifest_from_hashes(files.clone(), &cfg).unwrap();
        let mut rev = files;
        rev.reverse();
        let b = build_manifest_from_hashes(rev, &cfg).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn splits_are_disjoint_and_cover_every_unique_file() {
        let cfg = SplitConfig::default();
        let m = build_manifest_from_hashes(synthetic_hashes(1000), &cfg).unwrap();
        m.verify().unwrap();
        m.verify_assignments().unwrap();
        let mut all: HashSet<&str> = HashSet::new();
        for list in [&m.train, &m.val, &m.heldout] {
            for f in list {
                assert!(all.insert(&f.sha256), "{} in two splits", f.sha256);
            }
        }
        assert_eq!(all.len(), 1000);
    }

    #[test]
    fn shares_follow_the_configured_permille() {
        let cfg = SplitConfig::default();
        let m = build_manifest_from_hashes(synthetic_hashes(10_000), &cfg).unwrap();
        let share = |n: usize| n as f64 / 10_000.0;
        assert!((share(m.val.len()) - 0.10).abs() < 0.02, "val {}", m.val.len());
        assert!((share(m.heldout.len()) - 0.15).abs() < 0.02, "heldout {}", m.heldout.len());
        assert!((share(m.train.len()) - 0.75).abs() < 0.03, "train {}", m.train.len());
    }

    #[test]
    fn an_existing_file_keeps_its_split_when_the_corpus_grows() {
        let cfg = SplitConfig::default();
        let small = build_manifest_from_hashes(synthetic_hashes(300), &cfg).unwrap();
        let large = build_manifest_from_hashes(synthetic_hashes(900), &cfg).unwrap();
        for (split, list) in [(Split::Train, &small.train), (Split::Val, &small.val), (Split::Heldout, &small.heldout)] {
            for f in list {
                assert!(large.files(split).iter().any(|g| g.sha256 == f.sha256), "{} moved", f.sha256);
            }
        }
    }

    #[test]
    fn byte_identical_copies_are_listed_once_and_share_a_split() {
        let cfg = SplitConfig::default();
        let h: Hash = Sha256::digest(b"same bytes").into();
        let mut files = synthetic_hashes(200);
        files.push((PathBuf::from("a-copy.arw"), h));
        files.push((PathBuf::from("b-copy.arw"), h));
        let m = build_manifest_from_hashes(files, &cfg).unwrap();
        assert_eq!(m.duplicates, vec![PathBuf::from("b-copy.arw")]);
        m.verify().unwrap();
    }

    #[test]
    fn the_salt_changes_the_assignment_and_the_path_does_not() {
        let h: Hash = Sha256::digest(b"x").into();
        let moved = (0..50).filter(|i| bucket(&h, &format!("salt-{i}")) != bucket(&h, "salt-0")).count();
        assert!(moved > 0);
        let a = build_manifest_from_hashes(vec![(PathBuf::from("one.arw"), h)], &SplitConfig::default()).unwrap();
        let b = build_manifest_from_hashes(vec![(PathBuf::from("two.arw"), h)], &SplitConfig::default()).unwrap();
        let split_of = |m: &SplitManifest| [m.train.len(), m.val.len(), m.heldout.len()];
        assert_eq!(split_of(&a), split_of(&b));
    }

    #[test]
    fn verify_rejects_an_empty_split_a_shared_file_and_a_hand_edited_assignment() {
        let cfg = SplitConfig::default();
        let m = build_manifest_from_hashes(synthetic_hashes(2), &cfg).unwrap();
        // two files cannot fill three splits
        assert!(m.verify().is_err());

        let mut ok = build_manifest_from_hashes(synthetic_hashes(500), &cfg).unwrap();
        ok.verify().unwrap();
        let moved = ok.train.remove(0);
        ok.val.push(moved.clone());
        assert!(ok.verify_assignments().is_err(), "a train file moved to val must be caught");
        ok.val.pop();
        ok.train.push(moved.clone());
        ok.heldout.push(moved);
        assert!(ok.verify().is_err(), "the same hash in two splits must be caught");
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let h: Hash = Sha256::digest(b"abc").into();
        assert_eq!(parse_hex(&hex(&h)), Some(h));
        assert_eq!(parse_hex("zz"), None);
        assert_eq!(parse_hex(&"é".repeat(32)), None);
    }

    #[test]
    fn shares_must_leave_room_to_train() {
        let bad = SplitConfig { salt: "s".into(), val_permille: 600, heldout_permille: 400 };
        assert!(build_manifest_from_hashes(synthetic_hashes(10), &bad).is_err());
    }
}
