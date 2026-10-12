//! Direct, read-only LRCAT Classic catalog import. SQLite pages are read in Rust;
//! LRCAT, Python and a C SQLite runtime are not needed. Original paths stay in place.
//! Raw settings are archived before mutation; mapped edits approximate reference rendering.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::lightroom_sqlite::{Database, LiveTable, Value as SqlValue};
use lightcraft_catalog::{Album, Flag, Op, Photo, PhotoId, Source};
use lightcraft_geom::{Affine, Orientation, Point, Rect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Row = BTreeMap<String, Value>;
const COMMAND: &str = "library.importLightroom";
const MAX_FILE: u64 = 1 << 30;

fn error(why: impl Into<String>) -> crate::EngineError {
    crate::EngineError::BadParams { cmd: COMMAND.into(), msg: why.into() }
}

fn text<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}
fn number(r: &Row, k: &str) -> i64 {
    r.get(k).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(0.0) as i64
}

pub(crate) fn number_for_job(r: &Row, k: &str) -> i64 {
    number(r, k)
}
fn key(path: &str) -> String {
    let p = path.replace('\\', "/");
    if cfg!(windows) { p.to_lowercase() } else { p }
}
fn source_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        _ => None,
    }
}

struct Reader {
    db: Database,
    tables: Vec<LiveTable>,
}
impl Reader {
    fn table(&self, name: &str, required: bool) -> Result<Vec<Row>, String> {
        let Some(t) = self.tables.iter().find(|t| t.name == name) else {
            return if required { Err(format!("unsupported LRCAT catalog: missing {name}")) } else { Ok(Vec::new()) };
        };
        let columns = t.column_names.as_ref().ok_or_else(|| format!("cannot read {name} columns"))?;
        let rows = self.db.read_table(t.rootpage, columns.len()).map_err(|e| format!("{name}: {e:?}"))?;
        if rows.len() > 1_000_000 {
            return Err(format!("{name}: more than one million records"));
        }
        Ok(rows
            .into_iter()
            .map(|r| {
                columns
                    .iter()
                    .cloned()
                    .zip(r.values.into_iter().map(|v| match v {
                        SqlValue::Null => Some(Value::Null),
                        SqlValue::Integer(n) => Some(json!(n)),
                        SqlValue::Real(n) => Some(json!(n)),
                        SqlValue::Text(s) => Some(json!(s)),
                        // Opaque LRCAT payloads never become JSON byte arrays.
                        SqlValue::Blob(_) => None,
                    }))
                    .filter_map(|(column, value)| value.map(|value| (column, value)))
                    .collect()
            })
            .collect())
    }

    fn optional_table(&self, name: &str, warnings: &mut Vec<String>) -> Result<Vec<Row>, String> {
        match self.table(name, false) {
            Ok(rows) => Ok(rows),
            Err(error) => {
                warnings.push(format!("skipping optional LRCAT table {name}: {error}"));
                Ok(Vec::new())
            }
        }
    }

    fn xmp_table(&self) -> Result<(Vec<Row>, Vec<String>), String> {
        let Some(t) = self.tables.iter().find(|t| t.name == "Adobe_AdditionalMetadata") else { return Ok((Vec::new(), Vec::new())) };
        let columns = t.column_names.as_ref().ok_or_else(|| "cannot read Adobe_AdditionalMetadata columns".to_string())?;
        let rows = self.db.read_table(t.rootpage, columns.len()).map_err(|e| format!("Adobe_AdditionalMetadata: {e:?}"))?;
        if rows.len() > 1_000_000 {
            return Err("Adobe_AdditionalMetadata: more than one million records".into());
        }
        let mut warnings = Vec::new();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let image = row
                .values
                .iter()
                .zip(columns)
                .find_map(|(value, column)| {
                    (column == "image").then_some(match value {
                        SqlValue::Integer(n) => *n,
                        _ => 0,
                    })
                })
                .unwrap_or(0);
            let mut mapped = Row::new();
            for (column, value) in columns.iter().zip(row.values) {
                let value = match value {
                    SqlValue::Null => Some(Value::Null),
                    SqlValue::Integer(n) => Some(json!(n)),
                    SqlValue::Real(n) => Some(json!(n)),
                    SqlValue::Text(s) => Some(json!(s)),
                    SqlValue::Blob(bytes) if column == "xmp" => match decode_xmp_bytes(&bytes) {
                        Ok(packet) => Some(Value::String(packet)),
                        Err(error) => {
                            warnings.push(format!("image {image}: {error}"));
                            None
                        }
                    },
                    SqlValue::Blob(_) => None,
                };
                if let Some(value) = value {
                    mapped.insert(column.clone(), value);
                }
            }
            out.push(mapped);
        }
        Ok((out, warnings))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogPhoto {
    pub source_id: i64,
    pub uuid: String,
    pub path: String,
    pub image: Row,
    pub settings: String,
    pub xmp: String,
    pub keywords: Vec<String>,
    pub history: Vec<Row>,
    pub snapshots: Vec<Row>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogImport {
    pub source: String,
    pub photos: Vec<CatalogPhoto>,
    pub collections: Vec<Row>,
    pub members: Vec<Row>,
    pub collection_content: Vec<Row>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct ImportIndex {
    pub(crate) photos: BTreeMap<String, u64>,
    pub(crate) collections: BTreeMap<String, u64>,
}

pub(crate) fn load_index(path: Option<&Path>) -> crate::Result<ImportIndex> {
    match path {
        Some(path) => {
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ImportIndex::default()),
                Err(e) => return Err(error(e.to_string())),
            };
            if file.metadata().map_err(|e| error(e.to_string()))?.len() > 16 * 1024 * 1024 {
                return Err(error("LRCAT import index exceeds 16 MiB"));
            }
            let mut bytes = Vec::new();
            use std::io::Read;
            file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| error(e.to_string()))?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(error("LRCAT import index exceeds 16 MiB"));
            }
            serde_json::from_slice(&bytes).map_err(|e| error(format!("invalid LRCAT import index: {e}")))
        }
        None => Ok(ImportIndex::default()),
    }
}

pub(crate) struct AppliedImport {
    pub(crate) report: Value,
    pub(crate) index: ImportIndex,
}

fn identity(data: &CatalogImport, photo: &CatalogPhoto) -> String {
    if photo.uuid.is_empty() { format!("{}:{}", key(&data.source), photo.source_id) } else { photo.uuid.clone() }
}

fn validate(data: &CatalogImport) -> Result<(), String> {
    let photos: HashMap<_, _> = data.photos.iter().map(|p| (p.source_id, p)).collect();
    if photos.len() != data.photos.len() {
        return Err("duplicate source photo ids".into());
    }
    for photo in &data.photos {
        let master = number(&photo.image, "masterImage");
        if master != 0
            && (master == photo.source_id
                || !photos.contains_key(&master)
                || photos.get(&master).is_some_and(|p| number(&p.image, "masterImage") != 0))
        {
            return Err(format!("invalid virtual copy master for {}", photo.source_id));
        }
    }
    let collections: HashMap<_, _> = data.collections.iter().map(|r| (number(r, "id_local"), r)).collect();
    for id in collections.keys() {
        let mut seen = HashSet::new();
        let mut at = *id;
        while let Some(r) = collections.get(&at) {
            if seen.len() >= 64 || !seen.insert(at) {
                return Err("cyclic or overly deep collection hierarchy".into());
            }
            at = number(r, "parent");
            if at == 0 {
                break;
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_for_job(data: &CatalogImport) -> Result<(), String> {
    validate(data)
}

fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("catalog or WAL exceeds 1 GiB import limit".into());
    }
    let mut out = Vec::new();
    f.take(MAX_FILE + 1).read_to_end(&mut out).map_err(|e| e.to_string())?;
    if out.len() as u64 > MAX_FILE {
        return Err("catalog grew beyond import limit".into());
    }
    Ok(out)
}

fn xmp(v: Option<&Value>) -> Result<String, String> {
    match v {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Array(a)) if !a.is_empty() => {
            let b: Vec<u8> = a.iter().filter_map(|n| n.as_u64().and_then(|n| u8::try_from(n).ok())).collect();
            let size = b.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes).ok_or("truncated XMP header")?;
            if size > 16 << 20 {
                return Err("XMP packet exceeds 16 MiB".into());
            }
            let compressed = b.get(4..).ok_or("truncated XMP")?;
            let decoded = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, 16 << 20).map_err(|_| "invalid compressed XMP")?;
            if decoded.len() != size as usize {
                return Err("XMP packet length mismatch".into());
            }
            String::from_utf8(decoded).map_err(|e| e.to_string())
        }
        _ => Ok(String::new()),
    }
}

fn decode_xmp_bytes(bytes: &[u8]) -> Result<String, String> {
    let size = bytes.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes).ok_or("truncated XMP header")?;
    if size > 16 << 20 {
        return Err("XMP packet exceeds 16 MiB".into());
    }
    let decoded = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes.get(4..).ok_or("truncated XMP")?, 16 << 20)
        .map_err(|_| "invalid compressed XMP")?;
    if decoded.len() != size as usize {
        return Err("XMP packet length mismatch".into());
    }
    String::from_utf8(decoded).map_err(|e| e.to_string())
}

fn be_word(bytes: &[u8], at: usize) -> Result<u32, String> {
    let word = bytes.get(at..at + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or("truncated WAL")?;
    Ok(u32::from_be_bytes(word))
}

fn wal_checksum(bytes: &[u8], big: bool, mut sum: (u32, u32)) -> (u32, u32) {
    for pair in bytes.as_chunks::<8>().0 {
        let first = [pair[0], pair[1], pair[2], pair[3]];
        let second = [pair[4], pair[5], pair[6], pair[7]];
        let (first, second) =
            if big { (u32::from_be_bytes(first), u32::from_be_bytes(second)) } else { (u32::from_le_bytes(first), u32::from_le_bytes(second)) };
        sum.0 = sum.0.wrapping_add(first).wrapping_add(sum.1);
        sum.1 = sum.1.wrapping_add(second).wrapping_add(sum.0);
    }
    sum
}

// SQLite file format §4: validate the live checksum chain in constant space, then overlay
// only its last committed prefix. The forensic timeline materializes EVERY historical
// database snapshot; it must never be used to validate a large, actively used catalog.
fn committed_wal_len(main: &[u8], wal: &[u8]) -> Result<usize, String> {
    if wal.is_empty() {
        return Ok(0);
    }
    let header = wal.get(..32).ok_or("truncated WAL header")?;
    let magic = be_word(header, 0)?;
    if !matches!(magic, 0x377f0682 | 0x377f0683) || be_word(header, 4)? != 3_007_000 {
        return Err("invalid WAL header".into());
    }
    let size = main.get(16..18).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_be_bytes).ok_or("truncated SQLite header")?;
    let size = if size == 1 { 65536 } else { u32::from(size) };
    if !(512..=65536).contains(&size) || !size.is_power_of_two() || be_word(header, 8)? != size {
        return Err("WAL page size mismatch".into());
    }
    let big = magic == 0x377f0683;
    let mut sum = wal_checksum(&header[..24], big, (0, 0));
    if sum != (be_word(header, 24)?, be_word(header, 28)?) {
        return Err("invalid WAL header checksum".into());
    }
    let salt = (be_word(header, 16)?, be_word(header, 20)?);
    let mut committed = 0;
    let stride = size as usize + 24;
    for (i, frame) in wal[32..].chunks_exact(stride).enumerate() {
        if be_word(frame, 0)? == 0 || salt != (be_word(frame, 8)?, be_word(frame, 12)?) {
            break;
        }
        sum = wal_checksum(&frame[..8], big, sum);
        sum = wal_checksum(&frame[24..], big, sum);
        if sum != (be_word(frame, 16)?, be_word(frame, 20)?) {
            break;
        }
        if be_word(frame, 4)? != 0 {
            committed = 32 + (i + 1) * stride;
        }
    }
    Ok(committed)
}

/// Where a root folder is when its `absolutePath` isn't a folder here (the catalog came from
/// another computer): `relativePathFromCatalog`, which LRCAT records for roots on the
/// catalog's volume, resolved against the catalog's folder, if that is a folder.
fn relocated_root(absolute: &str, relative: &str, catalog: &Path) -> Option<PathBuf> {
    if relative.is_empty() || !Path::new(relative).is_relative() || Path::new(absolute).is_dir() {
        return None;
    }
    let dir = std::path::absolute(catalog).ok()?.parent()?.join(relative);
    let mut out = PathBuf::new();
    for part in dir.components() {
        match part {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            part => out.push(part),
        }
    }
    out.is_dir().then_some(out)
}

/// Inspect `.lrcat` plus committed WAL pages without modifying either source file.
pub fn read(path: &Path) -> Result<CatalogImport, String> {
    let cancel = AtomicBool::new(false);
    let total = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    read_with_progress(path, &cancel, &total, &done)
}

pub fn read_with_progress(path: &Path, cancel: &AtomicBool, total: &AtomicUsize, done: &AtomicUsize) -> Result<CatalogImport, String> {
    // 13 catalog tables plus one validation stage; ImportJob adds one unit per original path.
    total.store(14, Ordering::Relaxed);
    done.store(0, Ordering::Relaxed);
    ensure_read_active(cancel)?;
    let before = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    let wal_before = std::fs::metadata(&wal_path).ok().map(|m| (m.len(), m.modified().ok()));
    let main = bytes(path)?;
    let wal = match std::fs::metadata(&wal_path) {
        Ok(_) => bytes(&wal_path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let wal_len = committed_wal_len(&main, &wal)?;
    ensure_read_active(cancel)?;
    let db = Database::open_with_wal(main, &wal[..wal_len]).map_err(|e| format!("invalid SQLite catalog: {e:?}"))?;
    let after = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let wal_after = std::fs::metadata(&wal_path).ok().map(|m| (m.len(), m.modified().ok()));
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() || wal_before != wal_after {
        return Err("catalog changed during reading; close its source editor and retry".into());
    }
    let reader = Reader { tables: db.live_tables()?, db };
    let mut warnings = Vec::new();
    let table = |name: &str, required: bool| -> Result<Vec<Row>, String> {
        ensure_read_active(cancel)?;
        let rows = reader.table(name, required)?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(rows)
    };
    let optional_table = |name: &str, warnings: &mut Vec<String>| -> Result<Vec<Row>, String> {
        ensure_read_active(cancel)?;
        let rows = reader.optional_table(name, warnings)?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(rows)
    };
    let roots: HashMap<_, _> = table("AgLibraryRootFolder", true)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let folders: HashMap<_, _> = table("AgLibraryFolder", true)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let files: HashMap<_, _> = table("AgLibraryFile", true)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let develops: HashMap<_, _> = optional_table("Adobe_imageDevelopSettings", &mut warnings)?
        .into_iter()
        .map(|r| (number(&r, "image"), text(&r, "text").to_string()))
        .collect();
    ensure_read_active(cancel)?;
    let (metadata_rows, metadata_warnings) = match reader.xmp_table() {
        Ok(result) => result,
        Err(error) => {
            warnings.push(format!("skipping optional LRCAT table Adobe_AdditionalMetadata: {error}"));
            (Vec::new(), Vec::new())
        }
    };
    warnings.extend(metadata_warnings);
    done.fetch_add(1, Ordering::Relaxed);
    let metadata: HashMap<_, _> = metadata_rows.into_iter().map(|r| (number(&r, "image"), r)).collect();
    let keyword_rows: HashMap<_, _> = optional_table("AgLibraryKeyword", &mut warnings)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let mut keywords: HashMap<i64, Vec<String>> = HashMap::new();
    for r in optional_table("AgLibraryKeywordImage", &mut warnings)? {
        ensure_read_active(cancel)?;
        let mut parts = Vec::new();
        let mut at = number(&r, "tag");
        let mut seen = HashSet::new();
        while at != 0 {
            if parts.len() >= 64 || !seen.insert(at) {
                return Err("cyclic or overly deep keyword hierarchy".into());
            }
            let Some(k) = keyword_rows.get(&at) else { break };
            let name = text(k, "name");
            if !name.is_empty() {
                parts.push(name.to_string());
            }
            at = number(k, "parent");
        }
        parts.reverse();
        if !parts.is_empty() {
            keywords.entry(number(&r, "image")).or_default().push(parts.join("|"));
        }
    }
    let group = |rows: Vec<Row>| {
        let mut out: HashMap<i64, Vec<Row>> = HashMap::new();
        for r in rows {
            out.entry(number(&r, "image")).or_default().push(r);
        }
        out
    };
    let mut root_paths = HashMap::new();
    let mut root_ids: Vec<_> = roots.keys().copied().collect();
    root_ids.sort_unstable();
    for (id, root) in root_ids.iter().filter_map(|id| Some((*id, roots.get(id)?))) {
        let absolute = text(root, "absolutePath");
        let resolved = match relocated_root(absolute, text(root, "relativePathFromCatalog"), path) {
            Some(found) => {
                let found = found.to_string_lossy().to_string();
                warnings.push(format!("root folder {}: {absolute} not found; using {found} (relative to the catalog)", text(root, "name")));
                found
            }
            None => absolute.to_string(),
        };
        root_paths.insert(id, resolved);
    }
    let mut history = group(optional_table("Adobe_libraryImageDevelopHistoryStep", &mut warnings)?);
    let mut snapshots = group(optional_table("Adobe_libraryImageDevelopSnapshot", &mut warnings)?);
    let mut out = CatalogImport {
        source: path.to_string_lossy().into(),
        photos: Vec::new(),
        collections: optional_table("AgLibraryCollection", &mut warnings)?,
        members: optional_table("AgLibraryCollectionImage", &mut warnings)?,
        collection_content: optional_table("AgLibraryCollectionContent", &mut warnings)?,
        warnings,
    };
    for image in table("Adobe_images", true)? {
        ensure_read_active(cancel)?;
        let id = number(&image, "id_local");
        let file = files.get(&number(&image, "rootFile")).ok_or_else(|| format!("image {id}: missing file record"))?;
        let folder = folders.get(&number(file, "folder")).ok_or_else(|| format!("image {id}: missing folder record"))?;
        let root = roots.get(&number(folder, "rootFolder")).ok_or_else(|| format!("image {id}: missing root record"))?;
        let root_path = root_paths.get(&number(root, "id_local")).map_or("", String::as_str);
        let folder_path = text(folder, "pathFromRoot");
        let name = text(file, "idx_filename");
        if root_path.is_empty() || name.is_empty() {
            return Err(format!("image {id}: incomplete original path"));
        }
        let path = Path::new(root_path).join(folder_path).join(name).to_string_lossy().to_string();
        let packet = match xmp(metadata.get(&id).and_then(|r| r.get("xmp"))) {
            Ok(s) => s,
            Err(e) => {
                out.warnings.push(format!("image {id}: {e}"));
                String::new()
            }
        };
        out.photos.push(CatalogPhoto {
            source_id: id,
            uuid: text(&image, "id_global").into(),
            path,
            image,
            settings: develops.get(&id).cloned().unwrap_or_default(),
            xmp: packet,
            keywords: keywords.remove(&id).unwrap_or_default(),
            history: history.remove(&id).unwrap_or_default(),
            snapshots: snapshots.remove(&id).unwrap_or_default(),
        });
    }
    ensure_read_active(cancel)?;
    Ok(out)
}

fn ensure_read_active(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("LRCAT import cancelled".into()) } else { Ok(()) }
}

fn mapped_settings(
    text: &str,
    target: crate::crs::Target,
    aspect: f64,
    orientation: Orientation,
    stored_aspect: Option<f64>,
) -> Result<(Value, Vec<String>), String> {
    let mut root = crate::preset_import::parse_lua(text)?;
    let deferred = strip_lua_sentinels(&mut root);
    let (mut props, values) = crate::preset_import::lua_settings_props(&root)?;
    // LRCAT catalog settings carry crop fields without XMP's HasCrop marker. Keep this
    // inference local to catalog import; sidecar/import paths retain their own semantics.
    const CROP: [&str; 5] = ["crs:CropLeft", "crs:CropTop", "crs:CropRight", "crs:CropBottom", "crs:CropAngle"];
    if !props.contains_key("crs:HasCrop") && CROP.iter().any(|key| props.contains_key(*key)) {
        props.insert("crs:HasCrop".into(), vec!["True".into()]);
    }
    let (mut partial, mut unknown) = crate::crs::to_partial_report(&props, Some(&values), target, aspect);
    if !catalog_crop_values_valid(&props) {
        remove_catalog_crop(&mut partial);
        unknown.push("Crop: invalid LRCAT catalog values".into());
    } else if let Some(warning) = catalog_crop(&mut partial, orientation, stored_aspect) {
        remove_catalog_crop(&mut partial);
        unknown.push(warning.into());
    }
    if deferred > 0 {
        unknown.push("Deferred LRCAT adjustments (-999999)".into());
    }
    Ok((partial, unknown))
}

/// LRCAT stores crop corners in the un-oriented source frame. Carry them into the displayed
/// frame, then apply LRCAT's straighten angle around that frame's centre. Invalid catalog
/// dimensions or crop values are left untouched rather than producing non-finite geometry.
fn catalog_crop_values_valid(props: &crate::crs::Props) -> bool {
    let number = |key: &str, default: f64| -> Option<f64> {
        match props.get(key) {
            None => Some(default),
            Some(values) => values.first()?.trim().parse::<f64>().ok().filter(|value| value.is_finite()),
        }
    };
    for (key, default) in [("crs:CropLeft", 0.0), ("crs:CropTop", 0.0), ("crs:CropRight", 1.0), ("crs:CropBottom", 1.0)] {
        let Some(value) = number(key, default) else { return false };
        if props.contains_key(key) && !(0.0..=1.0).contains(&value) {
            return false;
        }
    }
    let Some(left) = number("crs:CropLeft", 0.0) else { return false };
    let Some(top) = number("crs:CropTop", 0.0) else { return false };
    let Some(right) = number("crs:CropRight", 1.0) else { return false };
    let Some(bottom) = number("crs:CropBottom", 1.0) else { return false };
    if right <= left || bottom <= top {
        return false;
    }
    if props.contains_key("crs:CropAngle") {
        let Some(angle) = props
            .get("crs:CropAngle")
            .and_then(|values| values.first())
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite() && value.abs() <= 45.0)
        else {
            return false;
        };
        let _ = angle;
    }
    true
}

fn remove_catalog_crop(partial: &mut Value) {
    if let Some(object) = partial.as_object_mut() {
        object.remove("crop");
    }
}

fn catalog_crop(partial: &mut Value, orientation: Orientation, stored_aspect: Option<f64>) -> Option<&'static str> {
    let geometry = partial.pointer_mut("/crop/geometry").and_then(Value::as_object_mut)?;
    let edge = |key: &str| geometry.get("rect").and_then(|rect| rect.get(key)).and_then(Value::as_f64);
    let (Some(x0), Some(y0), Some(x1), Some(y1)) = (edge("x0"), edge("y0"), edge("x1"), edge("y1")) else { return None };
    if [x0, y0, x1, y1].iter().any(|value| !value.is_finite() || !(0.0..=1.0).contains(value)) || x0 >= x1 || y0 >= y1 {
        return Some("Crop: invalid LRCAT catalog geometry");
    }
    let lightroom_angle = geometry.get("angle").and_then(Value::as_f64).unwrap_or(0.0);
    if !lightroom_angle.is_finite() || lightroom_angle.abs() > 360.0 {
        return Some("Crop: invalid LRCAT catalog angle");
    }
    if orientation == Orientation::Normal && lightroom_angle == 0.0 {
        return None;
    }
    if lightroom_angle == 0.0 {
        let rect = orientation.map_norm_rect(Rect::from_points(Point::new(x0, y0), Point::new(x1, y1)));
        if [rect.x0, rect.y0, rect.x1, rect.y1].iter().all(|value| value.is_finite()) {
            geometry.insert("rect".into(), json!({"x0": rect.x0, "y0": rect.y0, "x1": rect.x1, "y1": rect.y1}));
            return None;
        }
        return Some("Crop: invalid LRCAT catalog orientation");
    }
    let Some(stored_aspect) = stored_aspect.filter(|value| value.is_finite() && *value > 0.0) else {
        return Some("Crop: skipped straighten because catalog dimensions are invalid");
    };
    let (ow, oh) = if orientation.swaps_axes() { (1.0, stored_aspect) } else { (stored_aspect, 1.0) };
    if !ow.is_finite() || !oh.is_finite() || ow <= 0.0 || oh <= 0.0 {
        return Some("Crop: invalid LRCAT catalog dimensions");
    }
    // Mirrors reverse the angle sign in LightCraft's oriented frame.
    let angle = if orientation.to_parts().0 { lightroom_angle } else { -lightroom_angle };
    let to_rotated = Affine::rotate_about(angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
    let corner = |x: f64, y: f64| {
        let (u, v) = orientation.map(x * stored_aspect, y, stored_aspect, 1.0);
        to_rotated.apply(Point::new(u, v))
    };
    let rect = Rect::from_points(corner(x0, y0), corner(x1, y1));
    let values = [rect.x0 / ow, rect.y0 / oh, rect.x1 / ow, rect.y1 / oh, angle];
    if values.iter().all(|value| value.is_finite()) {
        geometry.insert("rect".into(), json!({"x0": values[0], "y0": values[1], "x1": values[2], "y1": values[3]}));
        geometry.insert("angle".into(), json!(angle));
        None
    } else {
        Some("Crop: invalid LRCAT catalog dimensions")
    }
}

/// `Adobe_images.orientation` names where stored top-left/top-right corners land in the displayed
/// frame: `AB` upright, `BC` clockwise, `DA` counter-clockwise, with mirrored variants supported.
fn lightroom_orientation(code: &str) -> Option<Orientation> {
    const ALL: [Orientation; 8] = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::Rotate180,
        Orientation::FlipV,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];
    let corner = |orientation: Orientation, x: f64, y: f64| match orientation.map(x, y, 1.0, 1.0) {
        (u, v) if u < 0.5 && v < 0.5 => 'A',
        (_, v) if v < 0.5 => 'B',
        (u, _) if u >= 0.5 => 'C',
        _ => 'D',
    };
    ALL.into_iter().find(|&orientation| code.chars().eq([corner(orientation, 0.0, 0.0), corner(orientation, 1.0, 0.0)]))
}

fn stored_aspect(row: &Row) -> Option<f64> {
    let width = row.get("fileWidth").and_then(Value::as_f64)?;
    let height = row.get("fileHeight").and_then(Value::as_f64)?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let aspect = width / height;
    aspect.is_finite().then_some(aspect)
}

fn strip_lua_sentinels(value: &mut crate::preset_import::Lua) -> usize {
    use crate::preset_import::Lua;
    match value {
        Lua::Num(n) if *n == -999999.0 => {
            *value = Lua::Nil;
            1
        }
        Lua::Table(array, fields) => {
            let mut removed = array.iter_mut().map(strip_lua_sentinels).sum::<usize>();
            fields.retain_mut(|(_, value)| {
                removed += strip_lua_sentinels(value);
                !matches!(value, Lua::Nil)
            });
            removed
        }
        _ => 0,
    }
}

fn strip_xmp_sentinels(value: &mut Value) {
    match value {
        Value::Object(fields) => fields.retain(|_, value| {
            if value.as_f64() == Some(-999999.0) {
                return false;
            }
            strip_xmp_sentinels(value);
            true
        }),
        Value::Array(values) => {
            if values.iter().any(|v| v.as_f64() == Some(-999999.0)) {
                values.clear();
            } else {
                for v in values {
                    strip_xmp_sentinels(v);
                }
            }
        }
        _ => {}
    }
}

/// Import into the current library. Existing edited records are preserved unless explicitly
/// requested; missing originals remain catalogued for later relinking. The source catalog is read-only.
pub fn import(s: &mut crate::Session, path: &Path, update_existing: bool) -> crate::Result<Value> {
    let mut job = crate::lightroom_job::LightroomJob::new(s, path.to_path_buf(), update_existing)?;
    let cancel = AtomicBool::new(false);
    let prepared = job.prepare(&cancel)?;
    let completion = crate::lightroom_job::commit_prepared(s, prepared)?;
    if s.library.as_ref().is_some_and(|library| library.on_disk) {
        s.persist()?;
    }
    completion.finalization.finish()?;
    Ok(completion.report)
}

#[cfg(test)]
fn apply(s: &mut crate::Session, data: CatalogImport, update_existing: bool) -> crate::Result<Value> {
    let archive_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Interop"));
    let index_path = archive_dir.as_ref().map(|p| p.join("lightroom-index.json"));
    let mut index = load_index(index_path.as_deref())?;
    let archive_path = archive_dir.as_ref().map(|dir| crate::lightroom_archive::store(dir, &data).map_err(error)).transpose()?;
    let undo0 = s.undo.len();
    let existing: HashMap<_, _> =
        s.catalog.photos().filter(|p| p.copy_of.is_none()).filter_map(|p| source_path(p).map(|path| (key(path), p.id))).collect();
    let paths: Vec<_> = data
        .photos
        .iter()
        .filter(|p| number(&p.image, "masterImage") == 0 && !existing.contains_key(&key(&p.path)) && Path::new(&p.path).is_file())
        .map(|p| p.path.clone())
        .collect();
    let missing_sources: HashSet<String> =
        data.photos.iter().filter(|p| number(&p.image, "masterImage") == 0).filter(|p| !Path::new(&p.path).is_file()).map(|p| key(&p.path)).collect();
    let files = crate::import::import(s, &paths, crate::import::ImportMode::Add)?;
    let applied =
        apply_prepared(s, data, ApplyPrepared { update_existing, files, index: &mut index, archive_path, missing_sources: &missing_sources, undo0 })?;
    if let Some(path) = index_path {
        s.persist()?;
        let bytes = serde_json::to_vec(&applied.index).map_err(|e| error(e.to_string()))?;
        lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| error(e.to_string()))?;
    }
    Ok(applied.report)
}

/// Apply prepared normal-file results and LRCAT metadata in one owner-thread operation.
/// This function does no filesystem work; callers finish its returned index separately.
pub(crate) struct ApplyPrepared<'a> {
    pub(crate) update_existing: bool,
    pub(crate) files: crate::import::ImportReport,
    pub(crate) index: &'a mut ImportIndex,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) missing_sources: &'a HashSet<String>,
    pub(crate) undo0: usize,
}

pub(crate) fn apply_prepared(s: &mut crate::Session, data: CatalogImport, context: ApplyPrepared<'_>) -> crate::Result<AppliedImport> {
    let ApplyPrepared { update_existing, files, index, archive_path, missing_sources, undo0 } = context;
    validate(&data).map_err(error)?;
    let mut warnings = data.warnings.clone();
    let duplicates: HashMap<_, _> = files.duplicates.iter().filter_map(|d| d.existing.map(|id| (key(&d.path), PhotoId(id)))).collect();
    let mut by_path: HashMap<_, _> =
        s.catalog.photos().filter(|p| p.copy_of.is_none()).filter_map(|p| source_path(p).map(|path| (key(path), p.id))).collect();
    let mut ids = HashMap::new();
    let mut ops = Vec::new();
    let now = (s.clock)();
    let mut missing = Vec::new();
    let mut unmapped = BTreeMap::new();
    let mut preserved = 0;
    // Masters precede their virtual copies.
    let mut photos: Vec<_> = data.photos.iter().collect();
    photos.sort_by_key(|p| number(&p.image, "masterImage") != 0);
    let mut planned: HashMap<PhotoId, Photo> = HashMap::new();
    for src in photos {
        let master = number(&src.image, "masterImage");
        let indexed = index
            .photos
            .get(&identity(&data, src))
            .copied()
            .map(PhotoId)
            .filter(|id| s.catalog.photo(*id).is_some_and(|p| source_path(p).is_some_and(|path| key(path) == key(&src.path))));
        let existing_id = indexed.or_else(|| if master == 0 { by_path.get(&key(&src.path)).copied() } else { None });
        let mut p = if let Some(id) = existing_id.and_then(|id| s.catalog.photo(id)) {
            (**id).clone()
        } else if master != 0 {
            let master_id = ids.get(&master).copied().ok_or_else(|| error(format!("virtual copy {}: missing master", src.source_id)))?;
            let mut p = planned
                .get(&master_id)
                .cloned()
                .or_else(|| s.catalog.photo(master_id).map(|p| (**p).clone()))
                .ok_or_else(|| error("virtual copy master is unavailable"))?;
            p.id = s.catalog.alloc_photo_id();
            p.copy_of = Some(master_id);
            p.copy_name = Some(text(&src.image, "copyName").to_string());
            p
        } else if let Some(duplicate) = duplicates.get(&key(&src.path)).and_then(|id| s.catalog.photo(*id)) {
            // Catalogs can contain distinct originals with identical bytes and independent edits.
            let mut p = (**duplicate).clone();
            p.id = s.catalog.alloc_photo_id();
            p.source = Source::File { path: src.path.clone() };
            p.file_name = Path::new(&src.path).file_name().map(|n| n.to_string_lossy().into()).unwrap_or_else(|| src.path.clone());
            p
        } else {
            let id = s.catalog.alloc_photo_id();
            let name = Path::new(&src.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| src.path.clone());
            if missing_sources.contains(&key(&src.path)) {
                missing.push(src.path.clone());
            }
            let mut p = Photo::new(
                id,
                Source::File { path: src.path.clone() },
                &name,
                text(&src.image, "fileFormat"),
                number(&src.image, "fileWidth").max(0) as u32,
                number(&src.image, "fileHeight").max(0) as u32,
                &now,
            );
            if ["RAW", "DNG", "ARW", "CR2", "CR3", "NEF", "NRW", "RAF", "ORF", "RW2", "PEF"].contains(&p.format.to_uppercase().as_str()) {
                p.kind = lightcraft_catalog::MediaKind::Raw;
            }
            p
        };
        ids.insert(src.source_id, p.id);
        index.photos.insert(identity(&data, src), p.id.0);
        let keep_develop = existing_id.is_some() && p.edited.is_some() && !update_existing;
        let previous_develop = (p.develop.clone(), p.edited.clone());
        if keep_develop {
            preserved += 1;
        }
        if !src.xmp.is_empty() {
            match crate::sidecar::parse_sidecar(&src.xmp, crate::crs::Target::for_photo(&p)) {
                Ok(mut sc) => {
                    if let Some(crate::sidecar::DevelopPatch::Partial(partial)) = &mut sc.develop {
                        strip_xmp_sentinels(partial);
                    }
                    crate::sidecar::merge_into(&mut p, &sc, &now);
                }
                Err(e) => warnings.push(format!("image {} XMP: {e}", src.source_id)),
            }
        }
        p.rating = number(&src.image, "rating").clamp(0, 5) as u8;
        p.flag = match number(&src.image, "pick") {
            1 => Flag::Pick,
            -1 => Flag::Reject,
            _ => Flag::None,
        };
        p.label = s.catalog.label_from_name(text(&src.image, "colorLabels"));
        if !text(&src.image, "captureTime").is_empty() {
            p.captured = Some(text(&src.image, "captureTime").into());
        }
        if !src.keywords.is_empty() {
            p.meta.keywords = src.keywords.clone();
        }
        if !src.settings.is_empty() && !keep_develop {
            let orientation = lightroom_orientation(text(&src.image, "orientation")).unwrap_or_default();
            match mapped_settings(
                &src.settings,
                crate::crs::Target::for_photo(&p),
                p.width.max(1) as f64 / p.height.max(1) as f64,
                orientation,
                stored_aspect(&src.image),
            ) {
                Ok((partial, unknown)) => {
                    p.develop = Arc::new(lightcraft_develop::apply_partial(&p.develop, &partial, 1.0));
                    p.edited = Some(now.clone());
                    if !unknown.is_empty() {
                        unmapped.insert(src.source_id, unknown);
                    }
                }
                Err(e) => warnings.push(format!("image {} settings: {e}", src.source_id)),
            }
        }
        if keep_develop {
            (p.develop, p.edited) = previous_develop;
        }
        if s.catalog.photo(p.id).is_some() {
            ops.extend([
                Op::SetRating { id: p.id, rating: p.rating },
                Op::SetFlag { id: p.id, flag: p.flag },
                Op::SetLabel { id: p.id, label: p.label },
                Op::SetMeta { id: p.id, meta: Box::new(p.meta.clone()) },
                Op::SetCaptured { id: p.id, captured: p.captured.clone() },
            ]);
            ops.push(Op::SetDevelop { id: p.id, settings: p.develop.clone(), label: "LRCAT catalog".into(), edited: p.edited.clone() });
        } else {
            ops.push(Op::AddPhoto { photo: Box::new(p.clone()) });
        }
        by_path.entry(key(&src.path)).or_insert(p.id);
        planned.insert(p.id, p);
    }
    let mut albums = HashMap::new();
    for collection in &data.collections {
        if number(collection, "systemOnly") != 0 {
            continue;
        }
        let collection_key = format!("{}:{}", key(&data.source), number(collection, "id_local"));
        let id = index
            .collections
            .get(&collection_key)
            .copied()
            .map(lightcraft_catalog::AlbumId)
            .filter(|id| s.catalog.album(*id).is_some())
            .unwrap_or_else(|| s.catalog.alloc_album_id());
        index.collections.insert(collection_key, id.0);
        albums.insert(number(collection, "id_local"), id);
    }
    let parents: HashMap<_, _> = data.collections.iter().map(|r| (number(r, "id_local"), number(r, "parent"))).collect();
    let mut grouped_members: HashMap<i64, Vec<&Row>> = HashMap::new();
    for member in &data.members {
        grouped_members.entry(number(member, "collection")).or_default().push(member);
    }
    for members in grouped_members.values_mut() {
        members.sort_by(|a, b| text(a, "positionInCollection").cmp(text(b, "positionInCollection")));
    }
    let mut ordered: Vec<_> = data.collections.iter().collect();
    ordered.sort_by_key(|r| {
        let mut depth = 0;
        let mut at = number(r, "parent");
        while at != 0 && depth < 64 {
            depth += 1;
            at = parents.get(&at).copied().unwrap_or(0);
        }
        depth
    });
    for collection in ordered {
        let Some(id) = albums.get(&number(collection, "id_local")).copied() else { continue };
        let mut album = Album::new(id, text(collection, "name"));
        album.parent = albums.get(&number(collection, "parent")).copied();
        album.folder = text(collection, "creationId").contains("group");
        if text(collection, "creationId").contains("smart") {
            warnings.push(format!("{}: smart rules preserved in archive; imported current membership", album.name));
        }
        if !album.folder {
            album.photos = grouped_members
                .get(&number(collection, "id_local"))
                .into_iter()
                .flatten()
                .filter_map(|r| ids.get(&number(r, "image")).copied())
                .collect();
        }
        if s.catalog.album(id).is_some() {
            // Reimport preserves the user's album name/hierarchy, merges only new membership.
            if let Some(old) = s.catalog.album(id).filter(|a| !a.folder && !a.is_smart()) {
                let mut photos = old.photos.clone();
                let mut seen: HashSet<_> = photos.iter().copied().collect();
                for photo in album.photos {
                    if seen.insert(photo) {
                        photos.push(photo);
                    }
                }
                ops.push(Op::SetAlbumPhotos { id, photos });
            }
        } else {
            ops.push(Op::AddAlbum { album });
        }
    }
    if !ops.is_empty() {
        s.commit("Import LRCAT Catalog", Op::Batch { ops })?;
    }
    s.merge_undo(s.undo.len().saturating_sub(undo0), "Import LRCAT Catalog");
    Ok(AppliedImport {
        report: json!({"source":data.source,"photos":ids.len(),"imported":files.imported.len(),"collections":albums.len(),"preservedExistingEdits":preserved,"missing":missing,"failed":files.failed,"warnings":warnings,"unmapped":unmapped,"archive":archive_path,"mapping":ids}),
        index: index.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::CropGeometry;
    fn row(v: Value) -> Row {
        serde_json::from_value(v).unwrap()
    }
    fn sample() -> CatalogImport {
        let mut photo = CatalogPhoto {
            source_id: 1,
            uuid: "test-master".into(),
            path: "/missing/test.ARW".into(),
            image: row(json!({"id_local":1,"rating":5,"pick":1,"fileFormat":"ARW","fileWidth":20,"fileHeight":10})),
            settings: "s = { Exposure2012 = 1.25, Contrast2012 = 17, CameraProfile = \"Example\" }".into(),
            xmp: String::new(),
            keywords: vec!["Travel|Goa".into()],
            history: vec![row(json!({"text":"s = { Exposure2012 = 0.5 }"}))],
            snapshots: Vec::new(),
        };
        let mut copy = photo.clone();
        copy.source_id = 2;
        copy.uuid = "test-copy".into();
        copy.image = row(json!({"id_local":2,"masterImage":1,"copyName":"Mono","rating":2,"pick":-1}));
        copy.settings = "s = { ConvertToGrayscale = true, Exposure2012 = -0.5 }".into();
        photo.image.insert("captureTime".into(), json!("2024-01-16T12:34:56"));
        CatalogImport {
            source: "test.lrcat".into(),
            photos: vec![copy, photo],
            collections: vec![
                row(json!({"id_local":9,"name":"Travel","creationId":"collection.group"})),
                row(json!({"id_local":10,"parent":9,"name":"Keepers","creationId":"collection"})),
            ],
            members: vec![
                row(json!({"collection":10,"image":2,"positionInCollection":"a"})),
                row(json!({"collection":10,"image":1,"positionInCollection":"b"})),
            ],
            collection_content: Vec::new(),
            warnings: Vec::new(),
        }
    }
    #[test]
    fn imports_copies_collections_ratings_and_mapped_edits_in_one_undo_step() {
        let mut s = crate::Session::new();
        let data = sample();
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert_eq!(r["photos"], 2);
        assert_eq!(r["missing"].as_array().unwrap().len(), 1);
        let master = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
        assert_eq!(master.rating, 5);
        assert_eq!(master.flag, Flag::Pick);
        assert_eq!(master.meta.keywords, vec!["Travel|Goa"]);
        assert_eq!(master.develop.light.exposure, 1.25);
        let copy = s.catalog.photos().find(|p| p.copy_of.is_some()).unwrap();
        assert_eq!(copy.copy_of, Some(master.id));
        assert_eq!(copy.rating, 2);
        assert_eq!(copy.flag, Flag::Reject);
        assert_eq!(copy.develop.light.exposure, -0.5);
        let album = s.catalog.albums().find(|a| a.name == "Keepers").unwrap();
        assert_eq!(album.photos, vec![copy.id, master.id]);
        assert!(album.parent.is_some());
        assert!(r["unmapped"]["1"].as_array().unwrap().iter().any(|v| v.as_str() == Some("CameraProfile")));
        assert_eq!(s.undo.len(), 1);
        s.undo_step().unwrap();
        assert!(s.catalog.is_empty());
        assert!(s.catalog.albums().next().is_none());
        assert!(data.photos[1].settings.contains("CameraProfile")); // unmapped source stays intact
    }
    #[test]
    fn preserves_existing_edits_and_rejects_invalid_structure_before_mutation() {
        let mut s = crate::Session::new();
        let data = sample();
        apply(&mut s, data.clone(), false).unwrap();
        let id = s.catalog.photos().find(|p| p.copy_of.is_none()).unwrap().id;
        s.set_develop(id, lightcraft_develop::DevelopSettings::default(), "Personal edit").unwrap();
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert!(r["preservedExistingEdits"].as_u64().unwrap() > 0);
        assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 0.0);
        let before = s.catalog.to_snapshot();
        let mut cyclic = data;
        cyclic.collections[0].insert("parent".into(), json!(10));
        assert!(apply(&mut s, cyclic, false).is_err());
        assert_eq!(s.catalog.to_snapshot(), before);
        assert!(
            mapped_settings(
                &format!("{}0{}", "{".repeat(1000), "}".repeat(1000)),
                crate::crs::Target::RawAbsolute,
                1.0,
                Orientation::Normal,
                Some(1.5)
            )
            .is_err()
        );
    }
    #[test]
    fn compressed_xmp_is_bounded_and_settings_are_data_only() {
        let packet = b"<x:xmpmeta>example</x:xmpmeta>";
        let mut encoded = (packet.len() as u32).to_be_bytes().to_vec();
        encoded.extend(miniz_oxide::deflate::compress_to_vec_zlib(packet, 6));
        assert_eq!(xmp(Some(&json!(encoded))).unwrap(), String::from_utf8_lossy(packet));
        encoded[0] = 0xff;
        assert!(xmp(Some(&json!(encoded))).is_err());
        let (partial, _) = mapped_settings(
            "s = { Exposure2012 = 0.75, ToneCurvePV2012 = {0,0,128,150,255,255} }",
            crate::crs::Target::RawAbsolute,
            1.5,
            Orientation::Normal,
            Some(1.5),
        )
        .unwrap();
        assert_eq!(partial["light"]["exposure"], 0.75);
    }

    #[test]
    fn custom_white_balance_on_relative_wb_raws_stays_as_shot_and_is_reported() {
        let settings =
            "s = { WhiteBalance = \"Custom\", Temperature = 3578, Tint = -5, CustomTemperature = 3578.95, CustomTint = -5, Exposure2012 = 0.5 }";
        let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawRelative, 1.5, Orientation::Normal, Some(1.5)).unwrap();
        assert_eq!(partial["wb"], json!({"mode": "asShot"}), "{partial}");
        assert_eq!(partial["light"]["exposure"], 0.5);
        assert!(unknown.iter().any(|field| field.starts_with("Temperature, Tint")), "{unknown:?}");

        let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, Some(1.5)).unwrap();
        assert_eq!(
            (partial["wb"]["mode"].as_str(), partial["wb"]["temp"].as_f64(), partial["wb"]["tint"].as_f64()),
            (Some("custom"), Some(3578.0), Some(-5.0))
        );
        assert!(!unknown.iter().any(|field| field.starts_with("Temperature")), "{unknown:?}");
    }

    #[test]
    fn catalog_crop_fields_follow_orientation_and_reject_bad_dimensions() {
        let settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }";
        let crop = |orientation| {
            let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, orientation, Some(1.5)).unwrap();
            let rect = &partial["crop"]["geometry"]["rect"];
            assert!(!unknown.iter().any(|key| key.starts_with("Crop")), "{unknown:?}");
            [rect["x0"].as_f64(), rect["y0"].as_f64(), rect["x1"].as_f64(), rect["y1"].as_f64()].map(|value| (value.unwrap() * 1e9).round() / 1e9)
        };
        for (code, expected) in [
            ("AB", [0.1, 0.2, 0.6, 0.9]),
            ("BA", [0.4, 0.2, 0.9, 0.9]),
            ("CD", [0.4, 0.1, 0.9, 0.8]),
            ("DC", [0.1, 0.1, 0.6, 0.8]),
            ("AD", [0.2, 0.1, 0.9, 0.6]),
            ("BC", [0.1, 0.1, 0.8, 0.6]),
            ("CB", [0.1, 0.4, 0.8, 0.9]),
            ("DA", [0.2, 0.4, 0.9, 0.9]),
        ] {
            assert_eq!(crop(lightroom_orientation(code).unwrap()), expected, "{code}");
        }

        let (partial, _) = mapped_settings("s = { CropAngle = 2 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Rotate90, Some(1.5)).unwrap();
        assert_eq!(partial["crop"]["geometry"]["angle"], -2.0);
        let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, None).unwrap();
        assert_eq!(
            [
                partial["crop"]["geometry"]["rect"]["x0"].as_f64(),
                partial["crop"]["geometry"]["rect"]["y0"].as_f64(),
                partial["crop"]["geometry"]["rect"]["x1"].as_f64(),
                partial["crop"]["geometry"]["rect"]["y1"].as_f64(),
            ]
            .map(|value| value.unwrap()),
            [0.1, 0.2, 0.6, 0.9]
        );
        let (partial, unknown) = mapped_settings(
            "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9, CropAngle = 2 }",
            crate::crs::Target::RawAbsolute,
            1.5,
            Orientation::Rotate90,
            None,
        )
        .unwrap();
        assert!(partial.get("crop").is_none());
        assert!(unknown.iter().any(|warning| warning.contains("dimensions")));
        let (partial, unknown) =
            mapped_settings("s = { CropLeft = \"nope\", CropRight = 0.6 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, Some(1.5))
                .unwrap();
        assert!(partial.get("crop").is_none());
        assert!(unknown.iter().any(|warning| warning.contains("invalid")));
        assert!(stored_aspect(&row(json!({"fileWidth": 0, "fileHeight": 100}))).is_none());
        assert!(stored_aspect(&row(json!({"fileWidth": "bad", "fileHeight": 100}))).is_none());
    }

    #[test]
    fn straightened_catalog_crops_preserve_aspect_and_mirror_sign() {
        let cases = [
            (
                "s = { CropAngle = -9.34444, CropBottom = 0.802034, CropLeft = 0.05499, CropRight = 0.94501, CropTop = 0.197966 }",
                Orientation::Normal,
                [0.0935892619821715, 0.09359009062947415, 0.9064107380178285, 0.9064099093705258],
                9.34444,
            ),
            (
                "s = { CropAngle = -1.48964, CropBottom = 0.962457, CropLeft = 0.010429, CropRight = 0.989571, CropTop = 0.037543 }",
                Orientation::Rotate90,
                [0.018608817271099032, 0.018609202353514116, 0.981391182728901, 0.981390797646486],
                1.48964,
            ),
        ];
        for (settings, orientation, expected, angle) in cases {
            let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, orientation, Some(9504.0 / 6336.0)).unwrap();
            let geometry = &partial["crop"]["geometry"];
            let rect = &geometry["rect"];
            let got = [rect["x0"].as_f64(), rect["y0"].as_f64(), rect["x1"].as_f64(), rect["y1"].as_f64()].map(|value| value.unwrap());
            assert!(got.iter().zip(expected.iter()).all(|(got, expected)| (*got - *expected).abs() < 1e-12), "{got:?} != {expected:?}");
            assert!((geometry["angle"].as_f64().unwrap() - angle).abs() < 1e-12);
        }
        let (partial, _) = mapped_settings(
            "s = { CropAngle = 2, CropLeft = 0.1, CropRight = 0.9, CropTop = 0.1, CropBottom = 0.9 }",
            crate::crs::Target::RawAbsolute,
            1.5,
            Orientation::FlipH,
            Some(1.5),
        )
        .unwrap();
        assert_eq!(partial["crop"]["geometry"]["angle"], 2.0);
    }

    #[test]
    fn imported_catalog_crop_survives_apply_and_undo() {
        let mut data = sample();
        data.photos.retain(|photo| photo.source_id == 1);
        data.collections.clear();
        data.members.clear();
        let photo = data.photos.first_mut().unwrap();
        photo.image.insert("orientation".into(), json!("BC"));
        photo.settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }".into();
        let mut s = crate::Session::new();
        let report = apply(&mut s, data, false).unwrap();
        let id = PhotoId(report["mapping"]["1"].as_u64().unwrap());
        let geometry = &s.catalog.photo(id).unwrap().develop.crop.geometry;
        let actual = [geometry.rect.x0, geometry.rect.y0, geometry.rect.x1, geometry.rect.y1];
        let expected = [0.1, 0.1, 0.8, 0.6];
        assert!(actual.iter().zip(expected).all(|(actual, expected)| (*actual - expected).abs() < 1e-12), "{actual:?}");
        assert_eq!(s.undo.len(), 1);
        s.undo_step().unwrap();
        assert!(s.catalog.is_empty());
    }

    #[test]
    fn deferred_auto_tone_sentinels_never_become_real_slider_values() {
        let mut data = sample();
        data.photos.truncate(1);
        data.photos[0].image.remove("masterImage");
        data.photos[0].settings =
            "s = { AutoTone = true, Exposure2012 = -999999, Contrast2012 = -999999, Shadows2012 = 65, Temperature = 6150 }".into();
        data.photos[0].xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="-999999" crs:Contrast2012="-999999" crs:Blacks2012="-22"/></rdf:RDF></x:xmpmeta>"#.into();
        let mut s = crate::Session::new();
        let report = apply(&mut s, data, false).unwrap();
        let p = s.catalog.photos().next().unwrap();
        assert_eq!(p.develop.light.exposure, 0.0);
        assert_eq!(p.develop.light.contrast, 0.0);
        assert_eq!(p.develop.light.shadows, 65.0);
        assert_eq!(p.develop.light.blacks, -22.0);
        assert!(report["unmapped"]["2"].as_array().unwrap().iter().any(|v| v.as_str().is_some_and(|v| v.contains("Deferred"))));
        let (partial, _) =
            mapped_settings("s = { Exposure2012 = -5, Contrast2012 = -100 }", crate::crs::Target::RawAbsolute, 1.0, Orientation::Normal, Some(1.5))
                .unwrap();
        assert_eq!(partial["light"]["exposure"], -5.0);
        assert_eq!(partial["light"]["contrast"], -100.0);
    }

    #[test]
    fn wal_checksums_cover_commits_without_materializing_historical_databases() {
        let mut main = vec![0; 512];
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        assert_eq!(wal_checksum(&[1, 0, 0, 0, 2, 0, 0, 0], false, (0, 0)), (1, 3));
        for big in [false, true] {
            let mut wal = vec![0; 32];
            wal[0..4].copy_from_slice(&(if big { 0x377f0683u32 } else { 0x377f0682u32 }).to_be_bytes());
            wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
            wal[8..12].copy_from_slice(&512u32.to_be_bytes());
            wal[16..20].copy_from_slice(&123u32.to_be_bytes());
            let mut sum = wal_checksum(&wal[..24], big, (0, 0));
            wal[24..28].copy_from_slice(&sum.0.to_be_bytes());
            wal[28..32].copy_from_slice(&sum.1.to_be_bytes());
            for _ in 0..128 {
                let mut frame = vec![0; 536];
                frame[0..4].copy_from_slice(&1u32.to_be_bytes());
                frame[4..8].copy_from_slice(&1u32.to_be_bytes());
                frame[8..12].copy_from_slice(&123u32.to_be_bytes());
                sum = wal_checksum(&frame[..8], big, sum);
                sum = wal_checksum(&frame[24..], big, sum);
                frame[16..20].copy_from_slice(&sum.0.to_be_bytes());
                frame[20..24].copy_from_slice(&sum.1.to_be_bytes());
                wal.extend(frame);
            }
            assert_eq!(committed_wal_len(&main, &wal).unwrap(), wal.len());
            let end = wal.len();
            wal[end - 1] ^= 1;
            assert_eq!(committed_wal_len(&main, &wal).unwrap(), end - 536);
            wal[24] ^= 1;
            assert!(committed_wal_len(&main, &wal).is_err());
            main[17] = 0;
            assert!(committed_wal_len(&main, &wal).is_err());
            main[16..18].copy_from_slice(&512u16.to_be_bytes());
        }
    }

    #[test]
    fn persistent_reimport_keeps_copy_and_collection_ids_and_capture_time() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-reimport-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        let mut data = sample();
        data.collections.reverse(); // Parent must be committed before its child regardless of row order.
        let first = apply(&mut s, data.clone(), false).unwrap();
        s.close_library().unwrap();
        s.open_library(&dir, false).unwrap();
        let id = PhotoId(first["mapping"]["1"].as_u64().unwrap());
        s.commit("Changed time", Op::SetCaptured { id, captured: None }).unwrap();
        let second = apply(&mut s, data, true).unwrap();
        assert_eq!(first["mapping"], second["mapping"]);
        assert_eq!(s.catalog.len(), 2);
        assert_eq!(s.catalog.albums().count(), 2);
        assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("2024-01-16T12:34:56"));
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Construct a small SQLite fixture from public file-format records, never private media.
    fn varint(mut n: u64) -> Vec<u8> {
        let mut out = vec![(n & 127) as u8];
        n >>= 7;
        while n > 0 {
            out.push((n & 127) as u8 | 128);
            n >>= 7;
        }
        out.reverse();
        out
    }
    fn record(values: &[SqlValue]) -> Vec<u8> {
        let mut header = Vec::new();
        let mut body = Vec::new();
        for value in values {
            let serial = match value {
                SqlValue::Null => 0,
                SqlValue::Integer(n) => {
                    body.extend(n.to_be_bytes());
                    6
                }
                SqlValue::Text(s) => {
                    body.extend(s.as_bytes());
                    13 + 2 * s.len() as u64
                }
                _ => unreachable!(),
            };
            header.extend(varint(serial));
        }
        let mut out = varint(1 + header.len() as u64);
        out.extend(header);
        out.extend(body);
        out
    }
    fn leaf(page: &mut [u8], offset: usize, rows: &[Vec<SqlValue>]) {
        page[offset] = 13;
        page[offset + 3..offset + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
        let mut end = page.len();
        for (i, values) in rows.iter().enumerate() {
            let payload = record(values);
            let mut cell = varint(payload.len() as u64);
            cell.extend(varint(i as u64 + 1));
            cell.extend(payload);
            end -= cell.len();
            page[end..end + cell.len()].copy_from_slice(&cell);
            page[offset + 8 + i * 2..offset + 10 + i * 2].copy_from_slice(&(end as u16).to_be_bytes());
        }
        page[offset + 5..offset + 7].copy_from_slice(&(end as u16).to_be_bytes());
    }
    /// A one-row-per-table SQLite file: `(name, columns, row)` each.
    fn native_catalog(tables: &[(&str, &str, Vec<SqlValue>)]) -> Vec<u8> {
        use SqlValue::{Integer as I, Text as T};
        let mut bytes = vec![0u8; 5 * 2048];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        bytes[16..18].copy_from_slice(&2048u16.to_be_bytes());
        bytes[18] = 1;
        bytes[19] = 1;
        bytes[21] = 64;
        bytes[22] = 32;
        bytes[23] = 32;
        bytes[28..32].copy_from_slice(&5u32.to_be_bytes());
        bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
        bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
        let mut schema = Vec::new();
        for (i, (name, columns, row)) in tables.iter().enumerate() {
            schema.push(vec![
                T("table".into()),
                T((*name).into()),
                T((*name).into()),
                I(i as i64 + 2),
                T(format!("CREATE TABLE {name} ({columns})")),
            ]);
            leaf(&mut bytes[(i + 1) * 2048..(i + 2) * 2048], 0, std::slice::from_ref(row));
        }
        leaf(&mut bytes[..2048], 100, &schema);
        bytes
    }

    fn photo_tables(root: (&str, &str)) -> Vec<(&'static str, &'static str, Vec<SqlValue>)> {
        use SqlValue::{Integer as I, Null as N, Text as T};
        vec![
            (
                "AgLibraryRootFolder",
                "id_local INTEGER PRIMARY KEY, absolutePath, name, relativePathFromCatalog",
                vec![N, T(root.0.into()), T("Photos".into()), T(root.1.into())],
            ),
            ("AgLibraryFolder", "id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot", vec![N, I(1), T("Photos/".into())]),
            ("AgLibraryFile", "id_local INTEGER PRIMARY KEY, folder, idx_filename", vec![N, I(1), T("test.ARW".into())]),
            (
                "Adobe_images",
                "id_local INTEGER PRIMARY KEY, id_global, rootFile, fileFormat, fileWidth, fileHeight, rating, pick",
                vec![N, T("fixture-image".into()), I(1), T("ARW".into()), I(20), I(10), I(5), I(1)],
            ),
        ]
    }

    #[test]
    fn reads_native_sqlite_tables_without_lightroom_or_external_processes() {
        let bytes = native_catalog(&photo_tables(("/catalog/", "")));
        let path = std::env::temp_dir().join(format!("lightcraft-native-catalog-{}.lrcat", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos.len(), 1);
        assert_eq!(catalog.photos[0].uuid, "fixture-image");
        assert_eq!(number(&catalog.photos[0].image, "rating"), 5);
        assert!(catalog.photos[0].path.ends_with("test.ARW"));
        std::fs::write(&path, b"bad catalog").unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_root_missing_here_is_found_relative_to_the_catalog() {
        let dir = std::env::temp_dir().join(format!("lightcraft-relocated-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("LRCAT")).unwrap();
        std::fs::create_dir_all(dir.join("RAW")).unwrap();
        let path = dir.join("LRCAT").join("LRCAT.lrcat");
        std::fs::write(&path, native_catalog(&photo_tables(("C:/Users/me/Project/RAW/", "../RAW/")))).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(Path::new(&catalog.photos[0].path), dir.join("RAW").join("Photos").join("test.ARW"));
        assert!(catalog.warnings.iter().any(|w| w.contains("relative to the catalog")), "{:?}", catalog.warnings);
        // a relative path that leads nowhere keeps the recorded one
        std::fs::write(&path, native_catalog(&photo_tables(("C:/Users/me/Project/RAW/", "../Elsewhere/")))).unwrap();
        let catalog = read(&path).unwrap();
        assert!(catalog.photos[0].path.starts_with("C:/Users/me/Project/RAW/"), "{}", catalog.photos[0].path);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_optional_table_warns_and_is_skipped() {
        let mut main = vec![0u8; 2 * 512];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21] = 64;
        main[22] = 32;
        main[23] = 32;
        main[28..32].copy_from_slice(&2u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        main[100] = 0x0d;
        let page = &mut main[512..];
        page[0] = 0x0d;
        page[3..5].copy_from_slice(&1u16.to_be_bytes());
        let mut cell = varint((64 * 1024 * 1024 + 1) as u64);
        cell.extend(varint(1));
        let start = 500 - cell.len();
        page[start..start + cell.len()].copy_from_slice(&cell);
        page[8..10].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        page[5..7].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        let db = Database::open_with_wal(main, &[]).unwrap();
        let reader =
            Reader { db, tables: vec![LiveTable { name: "optional_history".into(), rootpage: 2, column_names: Some(vec!["payload".into()]) }] };
        let mut warnings = Vec::new();
        let rows = reader.optional_table("optional_history", &mut warnings).unwrap();
        assert!(rows.is_empty());
        assert!(warnings.iter().any(|warning| warning.contains("optional_history")));
    }
    #[test]
    fn catalog_crops_without_has_crop_are_imported() {
        // LRCAT Classic catalogs store crop edges and angle but never `HasCrop`
        let (partial, unknown) = mapped_settings(
            "s = { CropConstrainAspectRatio = true, CropLeft = 0.042702, CropRight = 0.957298, CropTop = 0.1 }",
            crate::crs::Target::RawAbsolute,
            1.5,
            Orientation::Normal,
            Some(1.5),
        )
        .unwrap();
        let crop = &partial["crop"]["geometry"];
        assert_eq!(crop["rect"]["x0"], 0.042702);
        assert_eq!(crop["rect"]["y0"], 0.1);
        assert_eq!(crop["rect"]["x1"], 0.957298);
        assert_eq!(crop["rect"]["y1"], 1.0);
        assert_eq!(crop["angle"], 0.0);
        assert!(!unknown.iter().any(|k| k.starts_with("CropLeft")), "{unknown:?}");
        // an angle alone still means a crop (LRCAT's angle turns the other way to LightCraft's)
        let (partial, unknown) =
            mapped_settings("s = { CropAngle = -1.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, Some(1.5)).unwrap();
        assert_eq!(partial["crop"]["geometry"]["angle"], 1.5);
        assert!(!unknown.iter().any(|k| k.starts_with("CropAngle")), "{unknown:?}");
        // no crop fields: the crop stays untouched
        let (partial, _) =
            mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, Some(1.5)).unwrap();
        assert!(partial.get("crop").is_none());
    }

    #[test]
    fn lightroom_orientation_codes() {
        // the three seen in a real catalog, checked against each file's EXIF orientation (1, 6, 8)
        assert_eq!(lightroom_orientation("AB"), Some(Orientation::Normal));
        assert_eq!(lightroom_orientation("BC"), Some(Orientation::Rotate90));
        assert_eq!(lightroom_orientation("DA"), Some(Orientation::Rotate270));
        assert_eq!(lightroom_orientation("CD"), Some(Orientation::Rotate180));
        assert_eq!(lightroom_orientation("BA"), Some(Orientation::FlipH));
        assert_eq!(lightroom_orientation("DC"), Some(Orientation::FlipV));
        assert_eq!(lightroom_orientation("AD"), Some(Orientation::Transpose));
        assert_eq!(lightroom_orientation("CB"), Some(Orientation::Transverse));
        for code in ["", "A", "ABC", "AC", "ab", "XY"] {
            assert_eq!(lightroom_orientation(code), None, "{code:?}");
        }
    }

    #[test]
    fn catalog_crops_follow_the_photo_orientation() {
        let crop = |o: Orientation| {
            let settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }";
            let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, Some(1.5)).unwrap();
            let g = &partial["crop"]["geometry"];
            let r = &g["rect"];
            [r["x0"].as_f64(), r["y0"].as_f64(), r["x1"].as_f64(), r["y1"].as_f64(), g["angle"].as_f64()].map(|v| (v.unwrap() * 1e9).round() / 1e9)
        };
        // (x, y) of the stored image → (1 − y, x) a quarter turn clockwise, (y, 1 − x) counter-clockwise
        assert_eq!(crop(Orientation::Normal), [0.1, 0.2, 0.6, 0.9, 0.0]);
        assert_eq!(crop(Orientation::Rotate90), [0.1, 0.1, 0.8, 0.6, 0.0]);
        assert_eq!(crop(Orientation::Rotate270), [0.2, 0.4, 0.9, 0.9, 0.0]);
        assert_eq!(crop(Orientation::Rotate180), [0.4, 0.1, 0.9, 0.8, 0.0]);
        assert_eq!(crop(Orientation::FlipH), [0.4, 0.2, 0.9, 0.9, 0.0]);
        // a mirror turns the straighten the other way
        let angle = |o: Orientation| {
            let (partial, _) = mapped_settings("s = { CropAngle = 2 }", crate::crs::Target::RawAbsolute, 1.5, o, Some(1.5)).unwrap();
            partial["crop"]["geometry"]["angle"].as_f64().unwrap()
        };
        assert_eq!(angle(Orientation::Rotate90), -2.0);
        assert_eq!(angle(Orientation::FlipH), 2.0);
        // no crop: nothing to orient
        let (partial, _) =
            mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Rotate90, Some(1.5)).unwrap();
        assert!(partial.get("crop").is_none());
    }

    /// The `crs` corners and angle LRCAT would store for LightCraft crop `g` on a stored image
    /// of aspect `w` (height 1) shown with orientation `o`.
    fn lightroom_crop(g: CropGeometry, o: Orientation, w: f64) -> String {
        let (ow, oh) = if o.swaps_axes() { (1.0, w) } else { (w, 1.0) };
        let to_source = Affine::rotate_about(-g.angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
        let stored = |x: f64, y: f64| {
            let p = to_source.apply(Point::new(x * ow, y * oh));
            o.inverse().map(p.x, p.y, ow, oh)
        };
        // the rectangle's corners that land top-left and bottom-right on the stored image
        let corners = [stored(g.rect.x0, g.rect.y0), stored(g.rect.x1, g.rect.y0), stored(g.rect.x1, g.rect.y1), stored(g.rect.x0, g.rect.y1)];
        let (tl, br) = (0..4).map(|i| (corners[i], corners[(i + 2) % 4])).find(|(a, b)| a.0 < b.0 && a.1 < b.1).unwrap();
        let angle = if o.to_parts().0 { g.angle } else { -g.angle };
        format!("s = {{ CropLeft = {}, CropTop = {}, CropRight = {}, CropBottom = {}, CropAngle = {angle} }}", tl.0 / w, tl.1, br.0 / w, br.1)
    }

    #[test]
    fn straightened_catalog_crops_round_trip() {
        let g = CropGeometry { rect: Rect::new(0.2, 0.15, 0.7, 0.8), angle: 6.5 };
        for o in [Orientation::Normal, Orientation::Rotate90, Orientation::Rotate270, Orientation::Rotate180, Orientation::FlipH] {
            for angle in [6.5, -6.5] {
                let g = CropGeometry { angle, ..g };
                let (partial, _) = mapped_settings(&lightroom_crop(g, o, Some(1.5)), crate::crs::Target::RawAbsolute, 1.5, o, Some(1.5)).unwrap();
                let got: CropGeometry = serde_json::from_value(partial["crop"]["geometry"].clone()).unwrap();
                let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
                assert!(
                    close(got.angle, g.angle)
                        && [(got.rect.x0, g.rect.x0), (got.rect.y0, g.rect.y0), (got.rect.x1, g.rect.x1), (got.rect.y1, g.rect.y1)]
                            .iter()
                            .all(|&(a, b)| close(a, b)),
                    "{o:?} {angle}: {got:?}"
                );
            }
        }
    }

    #[test]
    fn straightened_catalog_crops_match_lightroom_exports() {
        // Settings and pixel sizes from a real LRCAT Classic catalog (9504 × 6336 stored) and the
        // JPEGs LRCAT exported from it at full size.
        for (settings, o, export) in [
            (
                "s = { CropAngle = -9.34444, CropBottom = 0.802034, CropLeft = 0.05499, CropRight = 0.94501, CropTop = 0.197966 }",
                Orientation::Normal,
                (7725.0, 5150.0),
            ),
            (
                "s = { CropAngle = -1.48964, CropBottom = 0.962457, CropLeft = 0.010429, CropRight = 0.989571, CropTop = 0.037543 }",
                Orientation::Rotate90,
                (6100.0, 9150.0),
            ),
        ] {
            let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, Some(9504.0 / 6336.0)).unwrap();
            let r: Rect = serde_json::from_value(partial["crop"]["geometry"]["rect"].clone()).unwrap();
            let (ow, oh) = if o.swaps_axes() { (6336.0, 9504.0) } else { (9504.0, 6336.0) };
            let (w, h) = (r.width() * ow, r.height() * oh);
            assert!((w - export.0).abs() <= 3.0 && (h - export.1).abs() <= 3.0, "{o:?}: {w:.1} × {h:.1}, LRCAT exported {export:?}");
        }
    }
}
