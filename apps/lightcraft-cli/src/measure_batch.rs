//! Batch dense measurement (`ai measure`): `ember.dense-measure.v1` vectors for many images.
//! Read-only: no library import, development, provider calls or source writes. Output carries
//! record ids and content digests only; manifest paths never leave the manifest.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use lightcraft_codecs::DecodeOptions;
use lightcraft_geom::Orientation;
use lightcraft_photo_ai::digest;
use lightcraft_pipeline::measure::{LEN, MAX_PIXELS, Measurements, RegionKind, SCHEMA, measure};
use lightcraft_raster::{Plane, Rgb32f};
use serde_json::Value;

const USAGE: &str = "ai measure --manifest MANIFEST.jsonl --out NEW_OUT.jsonl";
const INPUT_SCHEMA: &str = "ember.measure-input.v1";
const OUTPUT_SCHEMA: &str = "ember.measure-output.v1";

/// Manifest caps: lines per manifest and bytes per line (terminator excluded).
const MAX_LINES: u64 = 1_000_000;
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_REGIONS: usize = 4;
/// Largest rendered file read for `format: file` (the media cap `ai decision extract` uses).
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// Long edge a rendered file is fitted within, as `look extract` decodes its samples.
const FILE_EDGE: u32 = 1024;
const CACHE_HEADER_BYTES: u64 = 16;

/// Size cap of a cache object with `channels` f32 samples per pixel: header plus the largest image.
const fn cache_cap(channels: u64) -> u64 {
    CACHE_HEADER_BYTES + MAX_PIXELS as u64 * channels * 4
}

/// How a manifest line ended up after bounded reading.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Eof,
    Text,
    TooLong,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    F32Cache,
    File,
}

/// One manifest input: where it is, the digest its bytes must have, how to decode it.
struct Object {
    path: String,
    sha256: String,
    encoding: Encoding,
}

struct Record {
    id: String,
    image: Object,
    regions: Vec<(RegionKind, Object)>,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut manifest = None;
    let mut output = None;
    let mut index = 0usize;
    while let Some(option) = args.get(index) {
        let slot = match option.as_str() {
            "--manifest" => &mut manifest,
            "--out" => &mut output,
            _ => return Err(USAGE.into()),
        };
        if slot.is_some() {
            return Err("duplicate measure option".into());
        }
        index = index.saturating_add(1);
        *slot = Some(args.get(index).ok_or("missing measure option value")?.clone());
        index = index.saturating_add(1);
    }
    let manifest = manifest.ok_or(USAGE)?;
    let output = output.ok_or(USAGE)?;

    let names = Measurements::names();
    if names.len() != LEN {
        return Err("measurement schema length mismatch".into());
    }
    let header = header_line(&names)?;

    // A manifest over the line cap fails before any output exists.
    let mut reader = BufReader::new(open_manifest(Path::new(&manifest))?);
    if count_lines(&mut reader)? > MAX_LINES {
        return Err(format!("manifest exceeds {MAX_LINES} lines"));
    }
    reader.seek(SeekFrom::Start(0)).map_err(|e| format!("manifest rewind: {e}"))?;

    // Existing output (including a link to the manifest) fails before any image is read. A failed
    // new output stays incomplete; only the stdout summary qualifies its bytes.
    let target = OpenOptions::new().write(true).create_new(true).open(&output).map_err(|e| format!("new measure output: {e}"))?;
    let mut out = BufWriter::new(target);
    write_line(&mut out, &header)?;

    let mut buffer = Vec::new();
    let mut number = 0u64;
    let (mut measured, mut failed) = (0u64, 0u64);
    loop {
        let line = next_line(&mut reader, &mut buffer).map_err(|e| format!("manifest read: {e}"))?;
        if line == Line::Eof {
            break;
        }
        number = number.saturating_add(1);
        if number > MAX_LINES {
            return Err(format!("manifest exceeds {MAX_LINES} lines"));
        }
        let (ok, row) = match line {
            Line::TooLong => failure_row(&line_id(number), &format!("manifest line exceeds {MAX_LINE_BYTES} bytes")),
            _ if buffer.iter().all(u8::is_ascii_whitespace) => continue,
            _ => handle_line(&buffer, number),
        };
        if ok {
            measured = measured.saturating_add(1);
        } else {
            failed = failed.saturating_add(1);
        }
        write_line(&mut out, &row)?;
    }
    out.flush().and_then(|()| out.get_ref().sync_all()).map_err(|e| format!("measure output write: {e}"))?;
    println!("{{\"measured\":{measured},\"failed\":{failed}}}");
    Ok(())
}

fn write_line(out: &mut impl Write, line: &str) -> Result<(), String> {
    out.write_all(line.as_bytes()).and_then(|()| out.write_all(b"\n")).map_err(|e| format!("measure output write: {e}"))
}

fn open_manifest(path: &Path) -> Result<File, String> {
    let file = File::open(path).map_err(|e| format!("manifest input: {e}"))?;
    let info = file.metadata().map_err(|e| format!("manifest metadata: {e}"))?;
    if !info.is_file() {
        return Err("manifest must be a regular file".into());
    }
    Ok(file)
}

/// Physical lines of the manifest (a last line without a terminator counts), read in bounded chunks.
fn count_lines(reader: &mut impl BufRead) -> Result<u64, String> {
    let mut lines = 0u64;
    let mut open = false;
    loop {
        let chunk = reader.fill_buf().map_err(|e| format!("manifest read: {e}"))?;
        if chunk.is_empty() {
            break;
        }
        let ends = chunk.iter().filter(|&&b| b == b'\n').count() as u64;
        lines = lines.saturating_add(ends);
        open = chunk.last() != Some(&b'\n');
        let used = chunk.len();
        reader.consume(used);
    }
    Ok(lines.saturating_add(u64::from(open)))
}

/// Reads the next physical line into `buffer` without its terminator, holding at most
/// `MAX_LINE_BYTES + 2` bytes; an over-long line is consumed to its end and reported as such.
fn next_line(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> std::io::Result<Line> {
    buffer.clear();
    let read = reader.by_ref().take(MAX_LINE_BYTES as u64 + 2).read_until(b'\n', buffer)?;
    if read == 0 {
        return Ok(Line::Eof);
    }
    let terminated = buffer.last() == Some(&b'\n');
    if terminated {
        buffer.pop();
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
    }
    if buffer.len() > MAX_LINE_BYTES {
        if !terminated {
            skip_line(reader)?;
        }
        buffer.clear();
        return Ok(Line::TooLong);
    }
    Ok(Line::Text)
}

/// Discards input up to and including the next newline, or to the end of input.
fn skip_line(reader: &mut impl BufRead) -> std::io::Result<()> {
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(());
        }
        let newline = chunk.iter().position(|&b| b == b'\n');
        let used = newline.map_or(chunk.len(), |at| at.saturating_add(1));
        reader.consume(used);
        if newline.is_some() {
            return Ok(());
        }
    }
}

/// One manifest line to its output row: `(measured, JSON line)`.
fn handle_line(bytes: &[u8], number: u64) -> (bool, String) {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return failure_row(&line_id(number), "manifest line is not valid JSON");
    };
    let id = readable_id(&value).unwrap_or_else(|| line_id(number));
    let record = match parse_record(&value) {
        Ok(record) => record,
        Err(reason) => return failure_row(&id, &reason),
    };
    // Last resort: a panic in a decoder or the measurement fails this record, not the batch.
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| measure_record(&record)));
    match guarded {
        Ok(Ok(values)) => match success_row(&record, &values) {
            Ok(row) => (true, row),
            Err(reason) => failure_row(&id, &reason),
        },
        Ok(Err(reason)) => failure_row(&id, &reason),
        Err(_) => failure_row(&id, "measurement panicked"),
    }
}

fn line_id(number: u64) -> String {
    format!("line:{number}")
}

/// The record's `id` when it is a valid one; only valid ids are ever echoed.
fn readable_id(value: &Value) -> Option<String> {
    let id = value.get("id")?.as_str()?;
    let valid = (1..=MAX_ID_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    valid.then(|| id.to_owned())
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Strict `ember.measure-input.v1` validation. Messages never echo manifest text.
fn parse_record(value: &Value) -> Result<Record, String> {
    let fields = value.as_object().ok_or("manifest line must be a JSON object")?;
    if fields.keys().any(|key| !["schema", "id", "image", "regions"].contains(&key.as_str())) {
        return Err("unknown field in manifest record".into());
    }
    if fields.get("schema").and_then(Value::as_str) != Some(INPUT_SCHEMA) {
        return Err(format!("record schema must be {INPUT_SCHEMA}"));
    }
    let id = readable_id(value).ok_or("id must be 1..=128 characters of [A-Za-z0-9._:-]")?;
    let image = parse_object(fields.get("image").ok_or("image is missing")?, &["path", "sha256", "format"]).map_err(|e| format!("image: {e}"))?;
    let mut regions: Vec<(RegionKind, Object)> = Vec::new();
    if let Some(list) = fields.get("regions") {
        let list = list.as_array().ok_or("regions must be an array")?;
        if list.len() > MAX_REGIONS {
            return Err(format!("at most {MAX_REGIONS} regions"));
        }
        for (position, entry) in list.iter().enumerate() {
            let object = parse_object(entry, &["kind", "path", "sha256", "format"]).map_err(|e| format!("regions[{position}]: {e}"))?;
            let kind = entry
                .get("kind")
                .and_then(Value::as_str)
                .and_then(|name| RegionKind::ALL.into_iter().find(|kind| kind.name() == name))
                .ok_or_else(|| format!("regions[{position}]: kind must be subject, sky, background or skin"))?;
            if object.encoding != Encoding::F32Cache {
                return Err(format!("regions[{position}]: region objects must use format f32-cache"));
            }
            if regions.iter().any(|(seen, _)| *seen == kind) {
                return Err(format!("duplicate {} region", kind.name()));
            }
            regions.push((kind, object));
        }
    }
    Ok(Record { id, image, regions })
}

fn parse_object(value: &Value, allowed: &[&str]) -> Result<Object, String> {
    let fields = value.as_object().ok_or("expected an object")?;
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown field".into());
    }
    let path = fields
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty() && path.len() <= MAX_PATH_BYTES && !path.contains('\0'))
        .ok_or("path must be a non-empty string of at most 4096 bytes")?;
    let sha256 =
        fields.get("sha256").and_then(Value::as_str).filter(|sha| is_sha256(sha)).ok_or("sha256 must be 64 lowercase hexadecimal characters")?;
    let encoding = match fields.get("format").and_then(Value::as_str) {
        Some("f32-cache") => Encoding::F32Cache,
        Some("file") => Encoding::File,
        _ => return Err("format must be f32-cache or file".into()),
    };
    Ok(Object { path: path.to_owned(), sha256: sha256.to_owned(), encoding })
}

fn measure_record(record: &Record) -> Result<Vec<f32>, String> {
    let image = load_image(&record.image).map_err(|e| format!("image: {e}"))?;
    let mut planes: Vec<(RegionKind, Plane)> = Vec::with_capacity(record.regions.len());
    for (kind, object) in &record.regions {
        let plane = load_plane(object).map_err(|e| format!("{} region: {e}", kind.name()))?;
        if plane.width != image.width || plane.height != image.height {
            return Err(format!("{} region size differs from the image", kind.name()));
        }
        planes.push((*kind, plane));
    }
    let views: Vec<(RegionKind, &Plane)> = planes.iter().map(|(kind, plane)| (*kind, plane)).collect();
    let values = measure(&image, &views).map_err(|e| e.to_string())?.to_vec();
    if values.len() != LEN || values.iter().any(|v| !v.is_finite()) {
        return Err("measurement produced an invalid vector".into());
    }
    Ok(values)
}

fn load_image(object: &Object) -> Result<Rgb32f, String> {
    match object.encoding {
        Encoding::F32Cache => decode_cache_image(&read_verified(object, cache_cap(3))?),
        Encoding::File => decode_file(&read_verified(object, MAX_FILE_BYTES)?),
    }
}

fn load_plane(object: &Object) -> Result<Plane, String> {
    decode_cache_plane(&read_verified(object, cache_cap(1))?)
}

/// The object's bytes, read once, whose SHA-256 must equal the manifest digest.
fn read_verified(object: &Object, limit: u64) -> Result<Vec<u8>, String> {
    let bytes = read_bounded(Path::new(&object.path), limit)?;
    if digest(&bytes) != object.sha256 {
        return Err("SHA256 does not match the manifest digest".into());
    }
    Ok(bytes)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("unreadable ({})", e.kind()))?;
    let info = file.metadata().map_err(|e| format!("metadata unreadable ({})", e.kind()))?;
    if !info.is_file() {
        return Err("not a regular file".into());
    }
    if info.len() > limit {
        return Err(format!("larger than the {limit} byte cap"));
    }
    let length = usize::try_from(info.len()).map_err(|_| "file too large for this platform")?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length.saturating_add(1)).map_err(|_| "out of memory reading the file")?;
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| format!("read failed ({})", e.kind()))?;
    if bytes.len() != length {
        return Err("file changed while it was read".into());
    }
    Ok(bytes)
}

/// Validates a cache object (little-endian u64 width and height, then row-major f32 samples)
/// against the `channels` its role needs; returns width, height and the sample bytes.
fn cache_layout(bytes: &[u8], channels: u64) -> Result<(usize, usize, &[u8]), String> {
    let (width, rest) = bytes.split_first_chunk::<8>().ok_or("cache object is shorter than its 16-byte header")?;
    let (height, payload) = rest.split_first_chunk::<8>().ok_or("cache object is shorter than its 16-byte header")?;
    let (width, height) = (u64::from_le_bytes(*width), u64::from_le_bytes(*height));
    if width == 0 || height == 0 {
        return Err("cache object has a zero dimension".into());
    }
    let pixels = width.checked_mul(height).filter(|&pixels| pixels <= MAX_PIXELS as u64).ok_or("cache object exceeds the pixel limit")?;
    let payload_bytes = |channels: u64| pixels.checked_mul(channels).and_then(|samples| samples.checked_mul(4));
    if payload_bytes(channels) != Some(payload.len() as u64) {
        let other = if channels == 3 { 1 } else { 3 };
        return Err(if payload_bytes(other) == Some(payload.len() as u64) {
            format!("expected a {channels}-channel cache object, found {other} channels")
        } else {
            "cache object length does not match its header dimensions (truncated or extended)".into()
        });
    }
    let width = usize::try_from(width).map_err(|_| "cache object width exceeds this platform")?;
    let height = usize::try_from(height).map_err(|_| "cache object height exceeds this platform")?;
    Ok((width, height, payload))
}

fn samples(payload: &[u8]) -> impl Iterator<Item = f32> + '_ {
    payload.chunks_exact(4).filter_map(|chunk| <[u8; 4]>::try_from(chunk).ok()).map(f32::from_le_bytes)
}

fn decode_cache_image(bytes: &[u8]) -> Result<Rgb32f, String> {
    let (width, height, payload) = cache_layout(bytes, 3)?;
    let pixels = width.checked_mul(height).ok_or("cache object pixel count overflows")?;
    let mut data: Vec<[f32; 3]> = Vec::new();
    data.try_reserve_exact(pixels).map_err(|_| "out of memory decoding the image object")?;
    let mut values = samples(payload);
    while let (Some(r), Some(g), Some(b)) = (values.next(), values.next(), values.next()) {
        data.push([r, g, b]);
    }
    if data.len() != pixels {
        return Err("cache object samples do not fill its dimensions".into());
    }
    Ok(Rgb32f { width, height, data })
}

fn decode_cache_plane(bytes: &[u8]) -> Result<Plane, String> {
    let (width, height, payload) = cache_layout(bytes, 1)?;
    let pixels = width.checked_mul(height).ok_or("cache object pixel count overflows")?;
    let mut data: Vec<f32> = Vec::new();
    data.try_reserve_exact(pixels).map_err(|_| "out of memory decoding the region object")?;
    data.extend(samples(payload));
    if data.len() != pixels {
        return Err("cache object samples do not fill its dimensions".into());
    }
    Ok(Plane { width, height, data })
}

/// A rendered file to display-linear Rec. 2020, as `look extract` reads its samples (EXIF
/// orientation applied, as `ai decision extract` does for finished exports).
fn decode_file(bytes: &[u8]) -> Result<Rgb32f, String> {
    if lightcraft_codecs::sniff(bytes).is_some_and(lightcraft_codecs::Format::is_raw) {
        return Err("raw files have no rendered look; give a rendered JPEG, PNG or TIFF".into());
    }
    let options = DecodeOptions { max_size: Some((FILE_EDGE, FILE_EDGE)), max_pixels: MAX_PIXELS as u64 };
    let decoded = lightcraft_codecs::decode(bytes, options).map_err(|e| format!("decode failed: {e}"))?;
    Ok(decoded.to_working().into_oriented(Orientation::from_exif(decoded.orientation)))
}

fn header_line(names: &[String]) -> Result<String, String> {
    let names = serde_json::to_string(names).map_err(|_| "names serialization failed")?;
    Ok(format!("{{\"schema\":\"{OUTPUT_SCHEMA}\",\"measure_schema\":\"{SCHEMA}\",\"names\":{names}}}"))
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "null".into())
}

/// Numbers are written by the f32 serializer (shortest text that parses back to the same f32).
fn success_row(record: &Record, values: &[f32]) -> Result<String, String> {
    let values = serde_json::to_string(values).map_err(|_| "values serialization failed")?;
    Ok(format!("{{\"id\":{},\"image_sha256\":\"{}\",\"values\":{values}}}", quote(&record.id), record.image.sha256))
}

fn failure_row(id: &str, reason: &str) -> (bool, String) {
    (false, format!("{{\"id\":{},\"error\":{}}}", quote(id), quote(reason)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(width: u64, height: u64, samples: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        for value in samples {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn cache_layout_accepts_only_the_exact_channel_count() {
        let rgb = cache(2, 1, &[0.0; 6]);
        assert_eq!(cache_layout(&rgb, 3).map(|(w, h, p)| (w, h, p.len())), Ok((2, 1, 24)));
        assert!(cache_layout(&rgb, 1).unwrap_err().contains("found 3 channels"));
        let alpha = cache(2, 1, &[0.0; 2]);
        assert_eq!(cache_layout(&alpha, 1).map(|(w, h, p)| (w, h, p.len())), Ok((2, 1, 8)));
        assert!(cache_layout(&alpha, 3).unwrap_err().contains("found 1 channels"));
    }

    #[test]
    fn cache_layout_rejects_hostile_headers() {
        assert!(cache_layout(&[0; 15], 3).is_err());
        assert!(cache_layout(&cache(0, 4, &[]), 3).is_err());
        assert!(cache_layout(&cache(u64::MAX, u64::MAX, &[]), 3).is_err());
        assert!(cache_layout(&cache(u64::MAX, 2, &[]), 1).is_err());
        assert!(cache_layout(&cache(MAX_PIXELS as u64 + 1, 1, &[]), 1).is_err());
        let mut truncated = cache(2, 1, &[0.0; 6]);
        truncated.pop();
        assert!(cache_layout(&truncated, 3).unwrap_err().contains("does not match"));
    }

    #[test]
    fn lines_are_bounded_and_numbered_physically() {
        let mut text = b"first\r\n\nsecond".to_vec();
        text.push(b'\n');
        text.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES + 1));
        text.extend_from_slice(b"\nlast");
        let mut reader = std::io::Cursor::new(text);
        assert_eq!(count_lines(&mut reader), Ok(5));
        reader.set_position(0);
        let mut buffer = Vec::new();
        let mut seen = Vec::new();
        loop {
            match next_line(&mut reader, &mut buffer).unwrap() {
                Line::Eof => break,
                Line::TooLong => seen.push("too long".to_string()),
                Line::Text => seen.push(String::from_utf8(buffer.clone()).unwrap()),
            }
        }
        assert_eq!(seen, ["first", "", "second", "too long", "last"]);
    }

    #[test]
    fn records_are_validated_without_echoing_manifest_text() {
        let sha = "a".repeat(64);
        let object = serde_json::json!({"path": "p", "sha256": sha, "format": "f32-cache"});
        let record = |extra: Value| {
            let mut value = serde_json::json!({"schema": INPUT_SCHEMA, "id": "a.b:c-d_1", "image": object});
            if let (Some(fields), Some(more)) = (value.as_object_mut(), extra.as_object()) {
                fields.extend(more.clone());
            }
            value
        };
        assert!(parse_record(&record(serde_json::json!({}))).is_ok());
        let sky = serde_json::json!({"kind": "sky", "path": "p", "sha256": sha, "format": "f32-cache"});
        assert_eq!(parse_record(&record(serde_json::json!({"regions": [sky.clone()]}))).map(|r| r.regions.len()), Ok(1));
        let duplicate = parse_record(&record(serde_json::json!({"regions": [sky.clone(), sky.clone()]})));
        assert_eq!(duplicate.err().as_deref(), Some("duplicate sky region"));
        let secret = parse_record(&record(serde_json::json!({"secret/path": 1})));
        assert_eq!(secret.err().as_deref(), Some("unknown field in manifest record"));
        let bad_id = serde_json::json!({"schema": INPUT_SCHEMA, "id": "bad id", "image": object});
        assert!(parse_record(&bad_id).is_err());
        assert_eq!(readable_id(&bad_id), None);
    }
}
