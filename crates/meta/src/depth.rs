//! Depth maps embedded in JPEG photos (Google Pixel and other Android portrait / "lens blur" shots).
//!
//! Two layouts, both described in XMP:
//!
//! - **Dynamic Depth** (Google, current): the main XMP packet lists a `Device:Container` directory of items; the
//!   items' files are appended to the JPEG in listed order (the first, `Primary`, is the file itself, its `Length`
//!   is not counted) and `Item:Length` (plus `Item:Padding`) says how long each is. The `Depth` item is a PNG or
//!   JPEG; its `Camera:DepthMap` element gives the `Depth:Format`, `Depth:Near` and `Depth:Far`.
//! - **GDepth** (Google, older): `GDepth:Data` is the base64 of the depth image, in the extended XMP (the main
//!   packet is limited to 64 KB), with `GDepth:Format`, `GDepth:Near` and `GDepth:Far`.
//!
//! This module only finds the encoded depth image and its range parameters; decoding the image and turning its
//! codes into depths is the caller's. Malformed files are errors, never panics.

use std::collections::BTreeMap;

use crate::container::jpeg_segments;
use crate::xmp::{XmpValue, parse_xmp};

/// How an 8- or 16-bit depth image's codes map to distances (GDepth / Dynamic Depth `Format`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeFormat {
    /// `d = near + v · (far − near)` for a code `v` in 0..1.
    Linear,
    /// `d = far · near / (far − v · (far − near))`: finer steps close to the camera.
    Inverse,
}

/// An embedded depth map, still encoded.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedDepth {
    /// The depth image file (PNG or JPEG).
    pub image: Vec<u8>,
    pub format: RangeFormat,
    pub near: Option<f64>,
    pub far: Option<f64>,
}

/// Why an embedded depth map could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepthError(pub String);

impl std::fmt::Display for DepthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "depth map: {}", self.0)
    }
}

impl std::error::Error for DepthError {}

fn bad<T>(why: impl Into<String>) -> Result<T, DepthError> {
    Err(DepthError(why.into()))
}

type Values = BTreeMap<String, XmpValue>;

/// The encoded depth map a JPEG carries: `Ok(None)` for a file without one (or not a JPEG).
pub fn find_depth(bytes: &[u8]) -> Result<Option<EmbeddedDepth>, DepthError> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Ok(None);
    }
    let seg = jpeg_segments(bytes);
    let mut packets: Vec<Values> = Vec::new();
    for xmp in [seg.xmp.as_deref(), seg.xmp_extended.as_deref()].into_iter().flatten() {
        // (cheap test first: most photos have neither)
        if !(xmp.contains("GDepth:") || xmp.contains("Container:Directory")) {
            continue;
        }
        packets.push(parse_xmp(xmp).map_err(|e| DepthError(e.to_string()))?.values);
    }
    for values in &packets {
        if let Some(d) = dynamic_depth(bytes, values)? {
            return Ok(Some(d));
        }
    }
    for values in &packets {
        if let Some(d) = gdepth(values)? {
            return Ok(Some(d));
        }
    }
    Ok(None)
}

/// The first value named `key` at any depth of `values`.
fn lookup<'a>(values: &'a Values, key: &str) -> Option<&'a XmpValue> {
    values.get(key).or_else(|| values.values().find_map(|v| find(v, key, 0)))
}

fn find<'a>(v: &'a XmpValue, key: &str, depth: usize) -> Option<&'a XmpValue> {
    if depth > 24 {
        return None;
    }
    match v {
        XmpValue::Struct(m) => m.get(key).or_else(|| m.values().find_map(|c| find(c, key, depth + 1))),
        XmpValue::Array(a) => a.iter().find_map(|c| find(c, key, depth + 1)),
        XmpValue::Text(_) => None,
    }
}

fn text<'a>(values: &'a Values, key: &str) -> Option<&'a str> {
    lookup(values, key).and_then(XmpValue::text).filter(|s| !s.is_empty())
}

/// `Format`, `Near` and `Far` under `ns` (`Depth` for Dynamic Depth, `GDepth` for the older layout).
fn range_params(values: &Values, ns: &str) -> Result<(RangeFormat, Option<f64>, Option<f64>), DepthError> {
    let format = match text(values, &format!("{ns}:Format")).map(str::to_ascii_lowercase).as_deref() {
        None => RangeFormat::Linear,
        Some("rangelinear") => RangeFormat::Linear,
        Some("rangeinverse") => RangeFormat::Inverse,
        Some(other) => return bad(format!("unsupported depth format `{other}`")),
    };
    let num = |k: &str| text(values, &format!("{ns}:{k}")).and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite());
    Ok((format, num("Near"), num("Far")))
}

/// Whether `data` starts like a PNG or a JPEG.
fn is_image(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG\r\n\x1a\n") || data.starts_with(&[0xff, 0xd8, 0xff])
}

struct Item {
    semantic: String,
    length: u64,
    padding: u64,
}

fn item_of(li: &XmpValue) -> Item {
    // `<rdf:li><Container:Item …/></rdf:li>`, or the item's fields on the `rdf:li` itself
    let it = li.field("Container:Item").unwrap_or(li);
    let field = |k: &str| it.field(k).and_then(XmpValue::text);
    let number = |k: &str| field(k).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    Item { semantic: field("Item:Semantic").unwrap_or_default().to_string(), length: number("Item:Length"), padding: number("Item:Padding") }
}

/// The Dynamic Depth container's `Depth` item, found by the lengths of the items appended to the file.
fn dynamic_depth(bytes: &[u8], values: &Values) -> Result<Option<EmbeddedDepth>, DepthError> {
    let Some(dir) = lookup(values, "Container:Directory") else { return Ok(None) };
    let items: Vec<Item> = dir.items().iter().map(item_of).collect();
    // the first item is the file's own image: the rest follow it, so they are counted from the end
    let tail = items.iter().skip(1).fold(0u64, |t, i| t.saturating_add(i.length).saturating_add(i.padding));
    let len = bytes.len() as u64;
    if tail > len {
        return bad("the container's items are longer than the file");
    }
    let mut at = len - tail;
    for item in items.iter().skip(1) {
        let start = at;
        at = at.saturating_add(item.length).saturating_add(item.padding);
        if !item.semantic.eq_ignore_ascii_case("Depth") {
            continue;
        }
        let end = start.saturating_add(item.length);
        let range = usize::try_from(start).ok().zip(usize::try_from(end).ok());
        let Some(data) = range.and_then(|(s, e)| bytes.get(s..e)) else { return bad("the depth item lies outside the file") };
        if !is_image(data) {
            return bad("the container's depth item is not a PNG or JPEG image");
        }
        let (format, near, far) = range_params(values, "Depth")?;
        return Ok(Some(EmbeddedDepth { image: data.to_vec(), format, near, far }));
    }
    Ok(None)
}

/// The older layout: the depth image's base64 in `GDepth:Data`.
fn gdepth(values: &Values) -> Result<Option<EmbeddedDepth>, DepthError> {
    let Some(data) = text(values, "GDepth:Data") else { return Ok(None) };
    let Some(image) = base64_decode(data) else { return bad("GDepth:Data is not valid base64") };
    if !is_image(&image) {
        return bad("GDepth:Data is not a PNG or JPEG image");
    }
    let (format, near, far) = range_params(values, "GDepth")?;
    Ok(Some(EmbeddedDepth { image, format, near, far }))
}

/// Base64 (standard or URL-safe alphabet, padding and white space optional); `None` for anything else.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n depth pixels";

    fn b64(data: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = c.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
            for k in 0..=c.len() {
                s.push(A[((n >> (18 - 6 * k)) & 63) as usize] as char);
            }
            s.extend(std::iter::repeat_n('=', 3 - c.len()));
        }
        s
    }

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0xff, marker];
        v.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn packet(description: &str) -> String {
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:GDepth=\"http://ns.google.com/photos/1.0/depthmap/\" \
             xmlns:Device=\"http://ns.google.com/photos/dd/1.0/device/\" \
             xmlns:Container=\"http://ns.google.com/photos/1.0/container/\" \
             xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\" \
             xmlns:Camera=\"http://ns.google.com/photos/dd/1.0/camera/\" \
             xmlns:Depth=\"http://ns.google.com/photos/dd/1.0/depthmap/\" {description}</rdf:Description></rdf:RDF></x:xmpmeta>"
        )
    }

    fn main_xmp(xmp: &str) -> Vec<u8> {
        segment(0xe1, &[b"http://ns.adobe.com/xap/1.0/\0".as_slice(), xmp.as_bytes()].concat())
    }

    fn extended_xmp(xmp: &str) -> Vec<u8> {
        let mut p = b"http://ns.adobe.com/xmp/extension/\0".to_vec();
        p.extend_from_slice(&[b'0'; 32]);
        p.extend_from_slice(&(xmp.len() as u32).to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(xmp.as_bytes());
        segment(0xe1, &p)
    }

    fn jpeg(segments: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0xff, 0xd8];
        for s in segments {
            v.extend_from_slice(s);
        }
        v.extend_from_slice(&[0xff, 0xd9]);
        v
    }

    fn gdepth_file(format: &str, near: &str, far: &str) -> Vec<u8> {
        let ext = packet(&format!(
            "GDepth:Format=\"{format}\" GDepth:Near=\"{near}\" GDepth:Far=\"{far}\" GDepth:Mime=\"image/png\" GDepth:Data=\"{}\">",
            b64(PNG)
        ));
        jpeg(&[main_xmp(&packet(">")), extended_xmp(&ext)])
    }

    #[test]
    fn base64_reads_both_alphabets_and_ignores_padding_and_white_space() {
        assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode("aGVs\nbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode("-_-_").as_deref(), base64_decode("+/+/").as_deref());
        assert_eq!(base64_decode("a$b"), None);
        assert_eq!(base64_decode(""), None);
        assert_eq!(base64_decode(&b64(PNG)).as_deref(), Some(PNG));
    }

    #[test]
    fn gdepth_in_the_extended_xmp_is_found_with_its_range() {
        let d = find_depth(&gdepth_file("RangeInverse", "0.5", "5")).unwrap().unwrap();
        assert_eq!((d.image.as_slice(), d.format, d.near, d.far), (PNG, RangeFormat::Inverse, Some(0.5), Some(5.0)));
        let d = find_depth(&gdepth_file("RangeLinear", "1", "9.5")).unwrap().unwrap();
        assert_eq!((d.format, d.near, d.far), (RangeFormat::Linear, Some(1.0), Some(9.5)));
    }

    #[test]
    fn gdepth_in_the_main_xmp_is_found() {
        let xmp = packet(&format!("GDepth:Format=\"RangeLinear\" GDepth:Data=\"{}\">", b64(PNG)));
        let d = find_depth(&jpeg(&[main_xmp(&xmp)])).unwrap().unwrap();
        assert_eq!((d.image.as_slice(), d.near, d.far), (PNG, None, None));
    }

    /// A Dynamic Depth file: the primary JPEG, then the confidence map and the depth map, appended.
    fn dynamic_file(items: &[(&str, &[u8], u64)], format: &str) -> Vec<u8> {
        let mut dir = String::from("><Device:Container><Container:Directory><rdf:Seq>");
        dir.push_str("<rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Mime=\"image/jpeg\" Item:Semantic=\"Primary\" Item:Length=\"0\"/></rdf:li>");
        for (semantic, data, padding) in items {
            dir.push_str(&format!(
                "<rdf:li rdf:parseType=\"Resource\"><Container:Item Item:Mime=\"image/png\" Item:Semantic=\"{semantic}\" Item:Length=\"{}\" Item:Padding=\"{padding}\"/></rdf:li>",
                data.len()
            ));
        }
        dir.push_str("</rdf:Seq></Container:Directory></Device:Container>");
        dir.push_str(&format!(
            "<Device:Cameras><rdf:Seq><rdf:li rdf:parseType=\"Resource\"><Camera:DepthMap rdf:parseType=\"Resource\">\
             <Depth:Format>{format}</Depth:Format><Depth:Near>0.25</Depth:Near><Depth:Far>8</Depth:Far></Camera:DepthMap></rdf:li></rdf:Seq></Device:Cameras>"
        ));
        let mut file = jpeg(&[main_xmp(&packet(&dir))]);
        for (_, data, padding) in items {
            file.extend_from_slice(data);
            file.extend(std::iter::repeat_n(0u8, *padding as usize));
        }
        file
    }

    #[test]
    fn dynamic_depth_container_items_are_cut_from_the_end_of_the_file() {
        let confidence = b"\x89PNG\r\n\x1a\nCONF";
        let depth = b"\x89PNG\r\n\x1a\nDEPTH!";
        let file = dynamic_file(&[("GainMap", b"\xff\xd8\xff gain", 0), ("Depth", depth, 3), ("Confidence", confidence, 0)], "RangeInverse");
        let d = find_depth(&file).unwrap().unwrap();
        assert_eq!((d.image.as_slice(), d.format, d.near, d.far), (&depth[..], RangeFormat::Inverse, Some(0.25), Some(8.0)));
        let file = dynamic_file(&[("Depth", PNG, 0)], "RangeLinear");
        let d = find_depth(&file).unwrap().unwrap();
        assert_eq!((d.image.as_slice(), d.format), (PNG, RangeFormat::Linear));
    }

    #[test]
    fn dynamic_depth_without_a_depth_item_has_none() {
        assert_eq!(find_depth(&dynamic_file(&[("Confidence", PNG, 0)], "RangeLinear")), Ok(None));
    }

    #[test]
    fn photos_without_depth_have_none() {
        assert_eq!(find_depth(&jpeg(&[])), Ok(None));
        assert_eq!(find_depth(&jpeg(&[main_xmp(&packet(">"))])), Ok(None));
        assert_eq!(find_depth(b"\x89PNG\r\n\x1a\n"), Ok(None));
        assert_eq!(find_depth(b""), Ok(None));
    }

    #[test]
    fn damaged_depth_is_an_error() {
        // container lengths longer than the file
        let mut file = dynamic_file(&[("Depth", PNG, 0)], "RangeLinear");
        file.truncate(file.len() - PNG.len() - 40);
        assert!(find_depth(&file).is_err());
        // the depth item does not start an image
        let mut file = dynamic_file(&[("Depth", PNG, 0)], "RangeLinear");
        let n = file.len();
        file[n - PNG.len()] = b'X';
        assert!(find_depth(&file).is_err());
        // not base64, not an image, an unknown format, unreadable XML
        let bad_data = |d: &str, f: &str| jpeg(&[main_xmp(&packet(&format!("GDepth:Format=\"{f}\" GDepth:Data=\"{d}\">")))]);
        assert!(find_depth(&bad_data("a$$$", "RangeLinear")).is_err());
        assert!(find_depth(&bad_data(&b64(b"just text"), "RangeLinear")).is_err());
        assert!(find_depth(&bad_data(&b64(PNG), "Stepwise")).is_err());
        let broken = jpeg(&[main_xmp("<x:xmpmeta GDepth:Data=\"<<<")]);
        assert!(!matches!(find_depth(&broken), Ok(Some(_))));
    }

    #[test]
    fn hostile_lengths_and_cut_files_never_panic() {
        let huge = jpeg(&[main_xmp(&packet(
            "><Device:Container><Container:Directory><rdf:Seq><rdf:li><Container:Item Item:Semantic=\"Primary\"/></rdf:li>\
             <rdf:li><Container:Item Item:Semantic=\"Depth\" Item:Length=\"18446744073709551615\" Item:Padding=\"18446744073709551615\"/></rdf:li>\
             </rdf:Seq></Container:Directory></Device:Container>",
        ))]);
        assert!(find_depth(&huge).is_err());
        for file in [gdepth_file("RangeInverse", "0.5", "5"), dynamic_file(&[("Depth", PNG, 2)], "RangeInverse")] {
            for cut in 0..file.len() {
                let _ = find_depth(&file[..cut]);
            }
        }
    }
}
