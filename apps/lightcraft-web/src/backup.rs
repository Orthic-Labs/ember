//! Library backups for the browser build: a plain zip (entries stored, not compressed — photos
//! don't shrink) of the library files and every stored original, built one entry at a time so
//! only one file is in wasm memory at once, and read back the same way for a restore.
//!
//! Layout of a backup:
//! - `library/catalog.snap`, `library/catalog.log`, `library/presets.json`, … (see
//!   [`crate::files::LIBRARY_FILES`]);
//! - `originals/<content hash>/<file name>`: each imported photo's original bytes, under its own
//!   name so an unzipped backup is browsable;
//! - `README.txt`.
//!
//! Restoring never deletes anything: the library files go to a new library folder in browser
//! storage, originals that aren't stored yet are added (they're named by content, so existing
//! ones are identical; a restored one must match its content hash, see [`verify_original`]),
//! and only then does a small pointer file ([`ACTIVE_LIBRARY`]) switch to the restored library.
//! The library that was there before stays in storage.
//!
//! Backups past 4 GB or 65 535 entries are written as zip64 (the end record and each entry that
//! needs it carry the 64-bit fields) and read back the same way; one entry is still at most 4 GB,
//! since each is held in memory whole. Restores read at most [`MAX_RESTORE_ENTRY_BYTES`] per entry.
//!
//! Cleaning up ([`abandoned_libraries`], [`orphaned_originals`]): a restore leaves the library it
//! replaced in storage, and removing photos leaves their originals. At the next start (one tab, so
//! nothing else is writing) the app keeps the active library and the one before it
//! ([`PREVIOUS_LIBRARY`]), deletes older ones, and deletes stored originals that no kept library's
//! files name.

/// Storage key of the file naming the library folder in use (absent: `library`).
pub const ACTIVE_LIBRARY: &str = "active-library";

/// Storage key of the file naming the library folder the active one replaced (a restore writes it).
pub const PREVIOUS_LIBRARY: &str = "previous-library";

/// Most originals deleted in one start, so a huge clean-up can't hold up opening the library.
pub const MAX_PRUNE_PER_START: usize = 500;

/// The default library folder.
pub const DEFAULT_LIBRARY_DIR: &str = "library";

/// Largest single entry restore reads into memory. Browser restores are deliberately bounded
/// per entry even though the zip format itself allows almost 4 GiB entries.
pub const MAX_RESTORE_ENTRY_BYTES: u64 = 256 << 20;

/// Largest central directory restore reads into memory.
pub const MAX_RESTORE_DIRECTORY_BYTES: u64 = 64 << 20;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const END_SIG: u32 = 0x0605_4b50;
const END64_SIG: u32 = 0x0606_4b50;
const LOC64_SIG: u32 = 0x0706_4b50;
/// Header id of the zip64 extra field.
const ZIP64_EXTRA: u16 = 1;
/// A 32-bit field holding this says "see the zip64 field".
const SAT32: u32 = u32::MAX;
/// A 16-bit count holding this says "see the zip64 field".
const SAT16: u16 = u16::MAX;
/// General-purpose flag bit 11: names are UTF-8.
const UTF8: u16 = 0x0800;
/// 1980-01-01 00:00 in MS-DOS format (date, time).
const DOS_DATE: u16 = (1 << 5) | 1;

/// The readme inside every backup.
pub const README: &str = "Ember library backup (browser version)\n\n\
library/      the catalog (catalog.snap + catalog.log), presets, view state and preferences\n\
originals/    every imported photo, as originals/<content hash>/<file name>\n\n\
Restore it in Ember for the web with File > Restore Library from Backup...\n\
The photos in originals/ are the untouched files you imported; edits live in the catalog.\n";

/// Is `name` a safe library folder name (`library` or `library-<letters, digits, ->`)?
pub fn valid_library_dir(name: &str) -> bool {
    name == DEFAULT_LIBRARY_DIR
        || name.strip_prefix("library-").is_some_and(|s| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Is `h` a plausible content hash (used as a storage key segment)?
pub fn valid_hash(h: &str) -> bool {
    !h.is_empty() && h.len() <= 128 && h.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Zip entry name for an original.
pub fn original_entry(hash: &str, file_name: &str) -> String {
    let name: String = file_name.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    let name = if name.is_empty() || name == "." || name == ".." { "original".to_string() } else { name };
    format!("originals/{hash}/{name}")
}

/// Where a backup entry goes when restoring into library folder `dir`: `Some(storage key)`, or
/// `None` to skip it (README, thumbnails, anything unknown or unsafe).
pub fn restore_key(entry: &str, dir: &str) -> Option<String> {
    if let Some(name) = entry.strip_prefix("library/") {
        return crate::files::LIBRARY_FILES.contains(&name).then(|| format!("{dir}/{name}"));
    }
    let rest = entry.strip_prefix("originals/")?;
    let (hash, _) = rest.split_once('/').unwrap_or((rest, ""));
    valid_hash(hash).then(|| format!("originals/{hash}"))
}

fn u16le(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u64le(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

struct Central {
    name: String,
    crc: u32,
    size: u32,
    offset: u64,
}

/// Writes a stored (uncompressed) zip piece by piece: [`ZipWriter::entry`] returns the local
/// header to emit before the entry's bytes, [`ZipWriter::finish`] the central directory.
#[derive(Default)]
pub struct ZipWriter {
    entries: Vec<Central>,
    offset: u64,
}

impl ZipWriter {
    /// The local header for `name` holding `data` (emit it, then `data`).
    pub fn entry(&mut self, name: &str, data: &[u8]) -> Result<Vec<u8>, String> {
        let size = u32::try_from(data.len()).map_err(|_| format!("{name}: larger than 4 GB"))?;
        let name_len = u16::try_from(name.len()).map_err(|_| format!("{name}: name too long"))?;
        let header_len = 30 + u64::from(name_len);
        let end = self.offset.checked_add(header_len + u64::from(size)).ok_or("the library is too large for one backup file")?;
        let offset = self.offset;
        let crc = crc32fast::hash(data);
        let mut h = Vec::with_capacity(header_len as usize);
        u32le(&mut h, LOCAL_SIG);
        u16le(&mut h, 20); // version needed
        u16le(&mut h, UTF8);
        u16le(&mut h, 0); // stored
        u16le(&mut h, 0); // time
        u16le(&mut h, DOS_DATE);
        u32le(&mut h, crc);
        u32le(&mut h, size);
        u32le(&mut h, size);
        u16le(&mut h, name_len);
        u16le(&mut h, 0); // extra
        h.extend_from_slice(name.as_bytes());
        self.offset = end;
        self.entries.push(Central { name: name.to_string(), crc, size, offset });
        Ok(h)
    }

    /// The central directory and end record (emit last). Entries past 4 GB, and a directory that
    /// needs it, get zip64 fields.
    pub fn finish(self) -> Result<Vec<u8>, String> {
        let mut cd = Vec::new();
        for e in &self.entries {
            let big = e.offset >= u64::from(SAT32);
            u32le(&mut cd, CENTRAL_SIG);
            u16le(&mut cd, if big { 45 } else { 20 }); // made by
            u16le(&mut cd, if big { 45 } else { 20 }); // needed
            u16le(&mut cd, UTF8);
            u16le(&mut cd, 0);
            u16le(&mut cd, 0);
            u16le(&mut cd, DOS_DATE);
            u32le(&mut cd, e.crc);
            u32le(&mut cd, e.size);
            u32le(&mut cd, e.size);
            u16le(&mut cd, u16::try_from(e.name.len()).map_err(|_| format!("{}: name too long", e.name))?);
            u16le(&mut cd, if big { 12 } else { 0 }); // extra
            u16le(&mut cd, 0); // comment
            u16le(&mut cd, 0); // disk
            u16le(&mut cd, 0); // internal attrs
            u32le(&mut cd, 0); // external attrs
            u32le(&mut cd, if big { SAT32 } else { u32::try_from(e.offset).unwrap_or(SAT32) });
            cd.extend_from_slice(e.name.as_bytes());
            if big {
                // only the offset overflowed: the extra field holds just that
                u16le(&mut cd, ZIP64_EXTRA);
                u16le(&mut cd, 8);
                u64le(&mut cd, e.offset);
            }
        }
        let cd_offset = self.offset;
        let cd_size = cd.len() as u64;
        let n = self.entries.len() as u64;
        let zip64 = cd_offset >= u64::from(SAT32) || cd_size >= u64::from(SAT32) || n >= u64::from(SAT16);
        let mut out = cd;
        if zip64 {
            let end64_at = cd_offset + cd_size;
            u32le(&mut out, END64_SIG);
            u64le(&mut out, 44); // size of the rest of this record
            u16le(&mut out, 45);
            u16le(&mut out, 45);
            u32le(&mut out, 0); // this disk
            u32le(&mut out, 0); // disk with the directory
            u64le(&mut out, n);
            u64le(&mut out, n);
            u64le(&mut out, cd_size);
            u64le(&mut out, cd_offset);
            u32le(&mut out, LOC64_SIG);
            u32le(&mut out, 0); // disk with the zip64 end record
            u64le(&mut out, end64_at);
            u32le(&mut out, 1); // disks
        }
        u32le(&mut out, END_SIG);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        if zip64 {
            u16le(&mut out, SAT16);
            u16le(&mut out, SAT16);
            u32le(&mut out, SAT32);
            u32le(&mut out, SAT32);
        } else {
            u16le(&mut out, u16::try_from(n).unwrap_or(SAT16));
            u16le(&mut out, u16::try_from(n).unwrap_or(SAT16));
            u32le(&mut out, u32::try_from(cd_size).unwrap_or(SAT32));
            u32le(&mut out, u32::try_from(cd_offset).unwrap_or(SAT32));
        }
        u16le(&mut out, 0); // comment
        Ok(out)
    }
}

/// One entry of a zip's central directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub crc: u32,
    pub size: u64,
    /// Offset of the entry's local header.
    pub header: u64,
    /// 0 = stored (the only method a restore reads).
    pub method: u16,
}

fn rd16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn rd32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn rd64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// How many bytes from the end of a zip to read to find its end record.
pub fn tail_len(file_len: u64) -> u64 {
    // the end record with its longest comment, plus the zip64 locator and end record before it
    file_len.min(22 + 65_535 + 20 + 56)
}

/// The central directory's (offset, size) from the last [`tail_len`] bytes of a zip (zip64 too).
pub fn find_central(tail: &[u8]) -> Result<(u64, u64), String> {
    let at = (0..tail.len().saturating_sub(21)).rev().find(|&i| rd32(tail, i) == Some(END_SIG)).ok_or("not a zip file (no end record)")?;
    let entries = rd16(tail, at + 10).ok_or("truncated zip")?;
    let size = rd32(tail, at + 12).ok_or("truncated zip")?;
    let offset = rd32(tail, at + 16).ok_or("truncated zip")?;
    let saturated = size == SAT32 || offset == SAT32 || entries == SAT16;
    let locator = at.checked_sub(20).filter(|&l| rd32(tail, l) == Some(LOC64_SIG));
    match (saturated, locator) {
        (true, Some(loc)) => {
            // the zip64 end record sits right before its locator (extensible data isn't supported)
            let rec = loc
                .checked_sub(56)
                .filter(|&r| rd32(tail, r) == Some(END64_SIG) && rd64(tail, r + 4) == Some(44))
                .ok_or("this zip64 backup has a layout that isn't supported")?;
            let size = rd64(tail, rec + 40).ok_or("truncated zip")?;
            let offset = rd64(tail, rec + 48).ok_or("truncated zip")?;
            Ok((offset, size))
        }
        (true, None) if size == SAT32 || offset == SAT32 => Err("damaged zip: the zip64 end record is missing".into()),
        _ => Ok((u64::from(offset), u64::from(size))),
    }
}

/// Parse a central directory.
pub fn parse_central(cd: &[u8]) -> Result<Vec<ZipEntry>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 46 <= cd.len() {
        if rd32(cd, i) != Some(CENTRAL_SIG) {
            return Err("damaged zip directory".into());
        }
        let bad = || "damaged zip directory".to_string();
        let method = rd16(cd, i + 10).ok_or_else(bad)?;
        let crc = rd32(cd, i + 16).ok_or_else(bad)?;
        let size32 = rd32(cd, i + 24).ok_or_else(bad)?;
        let n = usize::from(rd16(cd, i + 28).ok_or_else(bad)?);
        let extra = usize::from(rd16(cd, i + 30).ok_or_else(bad)?);
        let comment = usize::from(rd16(cd, i + 32).ok_or_else(bad)?);
        let header32 = rd32(cd, i + 42).ok_or_else(bad)?;
        let name = cd.get(i + 46..i + 46 + n).ok_or_else(bad)?;
        let extra_bytes = cd.get(i + 46 + n..i + 46 + n + extra).ok_or_else(bad)?;
        let (size, header) = zip64_fields(extra_bytes, size32, rd32(cd, i + 20).ok_or_else(bad)?, header32).ok_or_else(bad)?;
        out.push(ZipEntry { name: String::from_utf8_lossy(name).into_owned(), crc, size, header, method });
        i += 46 + n + extra + comment;
    }
    Ok(out)
}

/// The entry's real (size, header offset): the 32-bit fields, or the zip64 extra field's values for
/// the ones that hold the saturated marker (in the order uncompressed size, compressed size, offset).
/// `None` when a marker has no value behind it.
fn zip64_fields(extra: &[u8], size32: u32, csize32: u32, header32: u32) -> Option<(u64, u64)> {
    if size32 != SAT32 && csize32 != SAT32 && header32 != SAT32 {
        return Some((u64::from(size32), u64::from(header32)));
    }
    let mut i = 0;
    while i + 4 <= extra.len() {
        let id = rd16(extra, i)?;
        let len = usize::from(rd16(extra, i + 2)?);
        let body = extra.get(i + 4..i + 4 + len)?;
        if id == ZIP64_EXTRA {
            let mut at = 0;
            let mut next = |wanted: bool| -> Option<Option<u64>> {
                if !wanted {
                    return Some(None);
                }
                let v = rd64(body, at)?;
                at += 8;
                Some(Some(v))
            };
            let size = next(size32 == SAT32)?;
            let csize = next(csize32 == SAT32)?;
            let header = next(header32 == SAT32)?;
            let size = size.unwrap_or(u64::from(size32));
            // stored entries only: both sizes must agree
            if csize.is_some_and(|c| c != size) {
                return None;
            }
            return Some((size, header.unwrap_or(u64::from(header32))));
        }
        i += 4 + len;
    }
    None
}

/// Offset of an entry's data from its local header's first 30 bytes.
pub fn data_offset(entry: &ZipEntry, local: &[u8]) -> Result<u64, String> {
    if rd32(local, 0) != Some(LOCAL_SIG) {
        return Err(format!("{}: damaged entry", entry.name));
    }
    let n = rd16(local, 26).ok_or("truncated entry")?;
    let extra = rd16(local, 28).ok_or("truncated entry")?;
    Ok(entry.header + 30 + u64::from(n) + u64::from(extra))
}

/// Check an entry's bytes.
pub fn verify(entry: &ZipEntry, data: &[u8]) -> Result<(), String> {
    if entry.method != 0 {
        return Err(format!("{}: compressed entries are not supported (not an Ember backup?)", entry.name));
    }
    if data.len() as u64 != entry.size || crc32fast::hash(data) != entry.crc {
        return Err(format!("{}: damaged (checksum mismatch)", entry.name));
    }
    Ok(())
}

/// Check that stored original bytes match their content-addressed storage key.
pub fn verify_original_hash(hash: &str, data: &[u8]) -> Result<(), String> {
    if crate::store::content_hash(data) == hash { Ok(()) } else { Err(format!("originals/{hash}: content hash mismatch")) }
}

/// Validate one accepted entry during restore preflight. Returns whether its storage key needs a
/// write: library files always do, while a matching existing original can be reused safely.
pub fn preflight_restore_entry(entry: &ZipEntry, data: &[u8], original_hash: Option<&str>, existing_original: Option<&[u8]>) -> Result<bool, String> {
    verify(entry, data)?;
    let Some(hash) = original_hash else { return Ok(true) };
    verify_original_hash(hash, data)?;
    Ok(!existing_original.is_some_and(|bytes| bytes.len() as u64 <= MAX_RESTORE_ENTRY_BYTES && crate::store::content_hash(bytes) == hash))
}

/// Check that an original's bytes are the photo its storage key names: originals are stored
/// under their content hash, and a restore that kept bytes under another photo's hash would
/// give that photo's catalog record a different image (the zip checksum can't tell).
pub fn verify_original(entry: &ZipEntry, hash: &str, data: &[u8]) -> Result<(), String> {
    if crate::store::content_hash(data) != hash {
        return Err(format!("{}: the file doesn't match its content hash (damaged or altered backup)", entry.name));
    }
    Ok(())
}

/// Content hashes that `web/<hash>/<file name>` paths in `bytes` (a library file) name. A plain
/// scan, so it also works on a damaged catalog and errs towards keeping originals.
pub fn referenced_hashes(bytes: &[u8], out: &mut std::collections::HashSet<String>) {
    const PREFIX: &[u8] = b"web/";
    let mut i = 0;
    while let Some(at) = bytes.get(i..).and_then(|rest| rest.windows(PREFIX.len()).position(|w| w == PREFIX)) {
        let start = i + at + PREFIX.len();
        let hash: Vec<u8> = bytes.get(start..).unwrap_or_default().iter().copied().take_while(u8::is_ascii_alphanumeric).collect();
        if bytes.get(start + hash.len()) == Some(&b'/')
            && let Ok(h) = String::from_utf8(hash)
            && valid_hash(&h)
        {
            out.insert(h);
        }
        i = start;
    }
}

/// Library folders (of `all`) to delete: every valid one except the active library and the one it
/// replaced. Sorted, for stable logs.
pub fn abandoned_libraries(all: &[String], active: &str, previous: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> =
        all.iter().filter(|d| valid_library_dir(d) && d.as_str() != active && Some(d.as_str()) != previous).cloned().collect();
    out.sort();
    out.dedup();
    out
}

/// Stored originals (`stored`: names in `originals/`) that no kept library names, at most `cap`.
pub fn orphaned_originals(stored: &[String], referenced: &std::collections::HashSet<String>, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = stored.iter().filter(|h| valid_hash(h) && !referenced.contains(h.as_str())).cloned().collect();
    out.sort();
    out.truncate(cap);
    out
}

/// A name for the restored library's folder.
pub fn restored_dir_name(now_ms: f64) -> String {
    format!("library-restored-{}", now_ms.max(0.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = ZipWriter::default();
        let mut out = Vec::new();
        for (n, d) in files {
            out.extend(w.entry(n, d).unwrap());
            out.extend_from_slice(d);
        }
        out.extend(w.finish().unwrap());
        out
    }

    #[test]
    fn written_backups_read_back() {
        let files: [(&str, &[u8]); 3] =
            [("README.txt", README.as_bytes()), ("library/catalog.log", b"{\"seq\":1}\n"), ("originals/abc123/IMG 1.jpg", &[0, 1, 2, 3, 255])];
        let zip = build(&files);
        let tail = &zip[zip.len() - tail_len(zip.len() as u64) as usize..];
        let (off, size) = find_central(tail).unwrap();
        let entries = parse_central(&zip[off as usize..(off + size) as usize]).unwrap();
        assert_eq!(entries.len(), 3);
        for (e, (name, data)) in entries.iter().zip(files) {
            assert_eq!(e.name, name);
            let start = data_offset(e, &zip[e.header as usize..e.header as usize + 30]).unwrap() as usize;
            let got = &zip[start..start + e.size as usize];
            assert_eq!(got, data);
            verify(e, got).unwrap();
        }
        // damage is caught
        let e = &entries[2];
        assert!(verify(e, &[0, 1, 2, 3, 254]).is_err());
        assert!(find_central(b"not a zip at all, just some text").is_err());
    }

    #[test]
    fn originals_must_match_their_content_hash() {
        let red: &[u8] = b"red corner photo";
        let blue: &[u8] = b"blue corner photo";
        let hash = crate::store::content_hash(red);
        let name = original_entry(&hash, "IMG 1.png");
        let key = restore_key(&name, "d").unwrap();
        let key_hash = key.strip_prefix("originals/").unwrap();
        assert_eq!(key_hash, hash);
        // the entry's own bytes pass; other bytes under the same name (valid zip CRC) don't
        let zip = build(&[(name.as_str(), blue)]);
        let tail = &zip[zip.len() - tail_len(zip.len() as u64) as usize..];
        let (off, size) = find_central(tail).unwrap();
        let entries = parse_central(&zip[off as usize..(off + size) as usize]).unwrap();
        let entry = &entries[0];
        verify(entry, blue).unwrap();
        let err = verify_original(entry, key_hash, blue).unwrap_err();
        assert!(err.contains(&name), "{err}");
        verify_original(entry, key_hash, red).unwrap();
    }

    #[test]
    fn restore_keys_are_safe() {
        assert_eq!(restore_key("library/catalog.snap", "library-restored-1").as_deref(), Some("library-restored-1/catalog.snap"));
        assert_eq!(restore_key("library/../../x", "library-r"), None);
        assert_eq!(restore_key("library/evil.bin", "library-r"), None);
        assert_eq!(restore_key("originals/abc123/IMG.jpg", "d").as_deref(), Some("originals/abc123"));
        assert_eq!(restore_key("originals/../library/catalog.log", "d"), None);
        assert_eq!(restore_key("README.txt", "d"), None);
        assert_eq!(restore_key("thumbs/x.jpg", "d"), None);
        assert_eq!(original_entry("abc", "a/b\\c.jpg"), "originals/abc/a_b_c.jpg");
        assert_eq!(original_entry("abc", ".."), "originals/abc/original");
        assert!(valid_library_dir("library") && valid_library_dir("library-restored-17"));
        assert!(!valid_library_dir("library-../x") && !valid_library_dir("originals") && !valid_library_dir("library-"));
    }

    #[test]
    fn size_limits_are_errors_not_panics() {
        let mut w = ZipWriter { offset: u64::MAX - 10, ..Default::default() };
        assert!(w.entry("x", &[0; 64]).is_err());
        assert!(ZipWriter::default().entry(&"n".repeat(70_000), b"x").is_err());
    }

    fn central_of(zip_tail: &[u8]) -> Vec<ZipEntry> {
        let (off, size) = find_central(zip_tail).unwrap();
        let _ = off;
        parse_central(&zip_tail[..size as usize]).unwrap()
    }

    #[test]
    fn backups_past_4_gb_are_zip64_and_read_back() {
        // as if 5 GB of originals were already written
        let mut w = ZipWriter { offset: 5 << 30, ..Default::default() };
        let _ = w.entry("library/catalog.snap", b"{}").unwrap();
        let _ = w.entry("originals/abc/IMG 1.jpg", &[7; 100]).unwrap();
        let tail = w.finish().unwrap();
        let (off, size) = find_central(&tail).unwrap();
        assert!(off > (5 << 30), "directory offset past 4 GB: {off}");
        assert_eq!(size as usize + 20 + 56 + 22, tail.len());
        let entries = parse_central(&tail[..size as usize]).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "library/catalog.snap");
        assert_eq!(entries[0].header, 5 << 30);
        assert_eq!(entries[0].size, 2);
        assert_eq!(entries[1].header, (5 << 30) + 30 + "library/catalog.snap".len() as u64 + 2);
        assert_eq!(entries[1].size, 100);
        // a damaged zip64 end record is an error, not a wrong directory
        let mut broken = tail.clone();
        let at = tail.len() - 22 - 20 - 56;
        broken[at] ^= 0xFF;
        assert!(find_central(&broken).is_err());
        // a saturated field with nothing behind it is rejected too
        let mut no_extra = tail.clone();
        let cd = &mut no_extra[..size as usize];
        cd[30..32].copy_from_slice(&0u16.to_le_bytes()); // first entry: extra length 0
        assert!(parse_central(cd).is_err());
    }

    #[test]
    fn small_backups_stay_plain_zip() {
        let mut w = ZipWriter::default();
        let _ = w.entry("a", b"x").unwrap();
        let tail = w.finish().unwrap();
        assert!(!tail.windows(4).any(|x| x == LOC64_SIG.to_le_bytes()), "no zip64 records");
        assert_eq!(central_of(&tail).len(), 1);
    }

    #[test]
    fn more_than_65535_entries_are_zip64() {
        let mut w = ZipWriter::default();
        let mut zip = Vec::new();
        for i in 0..70_000u32 {
            let name = format!("originals/h{i}/p.jpg");
            zip.extend(w.entry(&name, b"x").unwrap());
            zip.push(b'x');
        }
        zip.extend(w.finish().unwrap());
        let tail = &zip[zip.len() - tail_len(zip.len() as u64) as usize..];
        let (off, size) = find_central(tail).unwrap();
        let entries = parse_central(&zip[off as usize..(off + size) as usize]).unwrap();
        assert_eq!(entries.len(), 70_000);
        assert_eq!(entries[69_999].name, "originals/h69999/p.jpg");
        let e = &entries[69_999];
        let start = data_offset(e, &zip[e.header as usize..e.header as usize + 30]).unwrap() as usize;
        verify(e, &zip[start..start + e.size as usize]).unwrap();
    }

    #[test]
    fn references_are_found_in_catalog_text() {
        let mut refs = std::collections::HashSet::new();
        let log = br#"{"op":"add","source":{"type":"file","path":"web/abc123/IMG 1.jpg"}}
{"source":{"type":"file","path":"web/DEF456/a/b.png"}} web/nohash web//x web/bad-hash/y "web/zzz9/""#;
        referenced_hashes(log, &mut refs);
        let mut got: Vec<_> = refs.into_iter().collect();
        got.sort();
        assert_eq!(got, ["DEF456", "abc123", "zzz9"]);
        let mut none = std::collections::HashSet::new();
        referenced_hashes(b"", &mut none);
        referenced_hashes(b"web/", &mut none);
        referenced_hashes(b"web/abc", &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn cleanup_keeps_the_active_and_previous_library_and_what_they_name() {
        let all: Vec<String> = ["library", "library-restored-1", "library-restored-2", "library-restored-3", "originals", "library-../x"]
            .map(String::from)
            .to_vec();
        assert_eq!(abandoned_libraries(&all, "library-restored-3", Some("library-restored-2")), ["library", "library-restored-1"]);
        assert_eq!(abandoned_libraries(&all, "library", None), ["library-restored-1", "library-restored-2", "library-restored-3"]);
        let referenced: std::collections::HashSet<String> = ["a1".to_string()].into();
        let stored: Vec<String> = ["a1", "b2", "c3", "../d4"].map(String::from).to_vec();
        assert_eq!(orphaned_originals(&stored, &referenced, 10), ["b2", "c3"]);
        assert_eq!(orphaned_originals(&stored, &referenced, 1), ["b2"]);
        assert!(orphaned_originals(&stored, &["a1", "b2", "c3"].map(String::from).into(), 10).is_empty());
    }

    #[test]
    fn original_hash_matches_bytes_even_when_zip_crc_is_valid() {
        let original = b"original image";
        let replacement = b"replacement image";
        let hash = crate::store::content_hash(original);
        let name = original_entry(&hash, "photo.png");
        let zip = build(&[(name.as_str(), replacement)]);
        let tail = &zip[zip.len() - tail_len(zip.len() as u64) as usize..];
        let (offset, size) = find_central(tail).unwrap();
        let entries = parse_central(&zip[offset as usize..(offset + size) as usize]).unwrap();
        let entry = &entries[0];
        let start = data_offset(entry, &zip[entry.header as usize..entry.header as usize + 30]).unwrap() as usize;
        verify(entry, &zip[start..start + replacement.len()]).unwrap();
        assert!(verify_original_hash(&hash, replacement).is_err());
        assert!(verify_original_hash(&hash, original).is_ok());
    }

    #[test]
    fn preflight_repairs_corrupt_original_but_reuses_matching_key() {
        let original = b"original image";
        let hash = crate::store::content_hash(original);
        let entry =
            ZipEntry { name: original_entry(&hash, "photo.png"), crc: crc32fast::hash(original), size: original.len() as u64, header: 0, method: 0 };
        assert!(preflight_restore_entry(&entry, original, Some(&hash), Some(b"blue image")).unwrap());
        assert!(!preflight_restore_entry(&entry, original, Some(&hash), Some(original)).unwrap());
        assert!(preflight_restore_entry(&entry, original, Some(&hash), None).unwrap());
    }

    #[test]
    fn late_invalid_entry_keeps_preflight_write_phase_empty() {
        let original = b"original image";
        let hash = crate::store::content_hash(original);
        let original_entry = ZipEntry {
            name: super::original_entry(&hash, "photo.png"),
            crc: crc32fast::hash(original),
            size: original.len() as u64,
            header: 0,
            method: 0,
        };
        let catalog_entry = ZipEntry { name: "library/catalog.log".into(), crc: crc32fast::hash(b"catalog"), size: 7, header: 0, method: 0 };
        let checks = [(&original_entry, &original[..], Some(hash.as_str()), Some(&b"corrupt"[..])), (&catalog_entry, &b"tampered"[..], None, None)];
        let mut plan = Vec::new();
        let result = checks
            .iter()
            .try_for_each(|(entry, data, hash, existing)| preflight_restore_entry(entry, data, *hash, *existing).map(|write| plan.push(write)));
        let active_before = "library";
        let active_after = result.as_ref().ok().map(|_| "library-restored").unwrap_or(active_before);
        let writes = result.ok().map(|_| plan).unwrap_or_default();
        assert!(writes.is_empty());
        assert_eq!(active_after, active_before);
    }

    #[test]
    fn valid_retry_repairs_existing_corrupt_key_then_reuses_it() {
        let original = b"original image";
        let hash = crate::store::content_hash(original);
        let entry = ZipEntry {
            name: super::original_entry(&hash, "photo.png"),
            crc: crc32fast::hash(original),
            size: original.len() as u64,
            header: 0,
            method: 0,
        };
        let corrupt = b"blue image";
        let should_repair = preflight_restore_entry(&entry, original, Some(&hash), Some(corrupt)).unwrap();
        assert!(should_repair);
        let mut stored = corrupt.to_vec();
        if should_repair {
            stored = original.to_vec();
        }
        assert_eq!(stored, original);
        assert!(!preflight_restore_entry(&entry, original, Some(&hash), Some(&stored)).unwrap());
    }
}
