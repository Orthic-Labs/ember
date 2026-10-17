//! Merging a freshly written XMP packet into an existing one (a sidecar another application
//! wrote, e.g. with its develop settings and edit history), keeping everything the writer does not
//! own byte for byte.
//!
//! The existing packet is scanned once (namespace-resolved, with byte positions). Only top-level
//! properties of the `rdf:Description` elements directly under `rdf:RDF` are considered, in
//! attribute form (`xmp:Rating="3"`) and element form (`<dc:subject>…</dc:subject>`):
//! - properties the writer owns ([`MergeRules::owned`]) are removed, and written from the fresh
//!   packet when it has them (so a cleared field is cleared in the file too);
//! - properties owned only when stated ([`MergeRules::owned_if_present`]) are replaced when the
//!   fresh packet has them and kept otherwise;
//! - any other property of the fresh packet (e.g. `xmp:CreatorTool`) is not written: the file
//!   keeps describing itself as its creator left it;
//! - everything else — other namespaces (e.g. `crs:`), `xmpMM:History`, unknown properties,
//!   comments, the packet wrapper and padding — is copied unchanged.
//!
//! The written properties go into one new `rdf:Description` (declaring its own namespaces, so
//! the existing prefix bindings never matter) just before `</rdf:RDF>`. A description left empty
//! by the removals is dropped, so repeated saves don't accumulate. The result is checked by
//! parsing it again: every written property must read back as in the fresh packet.
//!
//! [`merge_ember_regions`] is the one place a packet's `mwg-rs:Regions` is edited in place: it adds, replaces and
//! removes only the region items Ember wrote (they carry `lc:EmberRegion="1"`), and copies every other item, and
//! the rest of the block, byte for byte.

use std::collections::{BTreeSet, HashSet};

use quick_xml::events::Event;

use crate::xmp::{EMBER_REGION_MARK, XmpError, canonical_name, ns_decls, parse_xmp, region_items};
use crate::{Orientation, Rect, Region};

/// Which properties [`merge_xmp`] takes from the fresh packet. Names are canonical
/// `prefix:name` (prefixes as [`crate::parse_xmp`] reports them); `prefix:*` is a whole namespace.
#[derive(Clone, Copy, Debug, Default)]
pub struct MergeRules<'a> {
    /// Always replaced: removed from the existing packet, written from the fresh one if stated.
    pub owned: &'a [&'a str],
    /// Replaced only when the fresh packet states them; otherwise the existing value stays.
    pub owned_if_present: &'a [&'a str],
}

fn listed(list: &[&str], name: &str) -> bool {
    list.iter().any(|p| match p.strip_suffix('*') {
        Some(ns) => name.starts_with(ns),
        None => *p == name,
    })
}

#[derive(Debug)]
struct Attr {
    raw: String,
    name: String,
    /// The value as written (still escaped).
    value: String,
}

#[derive(Debug)]
struct Prop {
    name: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Default)]
struct Desc {
    tag_start: usize,
    tag_end: usize,
    /// The element name as written (`rdf:Description`, or another prefix for the RDF namespace).
    raw_name: String,
    empty: bool,
    attrs: Vec<Attr>,
    props: Vec<Prop>,
    /// The end of `</rdf:Description>` (`tag_end` when self-closing).
    end: usize,
}

/// An element inside a top-level `mwg-rs:Regions` property.
#[derive(Debug)]
struct Elem {
    /// Canonical `prefix:name`.
    name: String,
    /// The name as written.
    raw_name: String,
    /// 1 for a direct child of `mwg-rs:Regions`.
    depth: usize,
    start: usize,
    /// The end of the element (of its closing tag, or of its self-closing tag).
    end: usize,
    empty: bool,
    /// It carries [`EMBER_REGION_MARK`].
    marked: bool,
}

#[derive(Debug, Default)]
struct Scan {
    descs: Vec<Desc>,
    /// Every element inside a top-level `mwg-rs:Regions`, in document order.
    inner: Vec<Elem>,
    /// How many top-level `mwg-rs:Regions` properties the packet has.
    regions_props: usize,
    /// Where the first `</rdf:RDF>` starts.
    rdf_close: Option<usize>,
}

enum Kind {
    Other,
    Rdf,
    Desc(usize),
    Prop {
        desc: usize,
        start: usize,
        name: String,
    },
    /// An element inside `mwg-rs:Regions`: its index in [`Scan::inner`].
    Inner(usize),
}

fn is_meta_attr(name: &str) -> bool {
    name.starts_with("xmlns")
        || matches!(name, "rdf:about" | "rdf:ID" | "rdf:nodeID" | "xml:lang" | "rdf:parseType" | "rdf:resource" | "rdf:datatype")
}

fn pos(r: &quick_xml::Reader<&[u8]>) -> usize {
    usize::try_from(r.buffer_position()).unwrap_or(usize::MAX)
}

/// Locate the descriptions, their top-level properties and `</rdf:RDF>` in a packet.
fn scan(s: &str) -> Result<Scan, XmpError> {
    let mut reader = quick_xml::Reader::from_reader(s.as_bytes());
    reader.config_mut().check_end_names = true;
    let mut out = Scan::default();
    let mut stack: Vec<Kind> = Vec::new();
    let mut scopes: Vec<Vec<(String, String)>> = vec![vec![("xml".into(), "http://www.w3.org/XML/1998/namespace".into())]];
    let mut count = 0usize;
    // the stack index of the open `mwg-rs:Regions` property
    let mut regions_base: Option<usize> = None;
    loop {
        let before = pos(&reader);
        let ev = reader.read_event().map_err(|e| XmpError::Xml(e.to_string()))?;
        let after = pos(&reader);
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                count += 1;
                if count > 200_000 || stack.len() > 64 {
                    return Err(XmpError::TooComplex);
                }
                let empty = matches!(ev, Event::Empty(_));
                let mut scope = Vec::new();
                let mut raw_attrs = Vec::new();
                // repeated names are refused with a set, in linear time (quick-xml's own check is quadratic)
                let mut seen = HashSet::new();
                for a in e.attributes().with_checks(false) {
                    let a = a.map_err(|e| XmpError::Xml(e.to_string()))?;
                    let k = a.key.0.to_owned();
                    if !seen.insert(k.clone()) {
                        return Err(XmpError::Xml(format!("repeated attribute {k}")));
                    }
                    let v = a.value.to_string();
                    if k == "xmlns" {
                        scope.push((String::new(), v.clone()));
                    } else if let Some(p) = k.strip_prefix("xmlns:") {
                        scope.push((p.to_string(), v.clone()));
                    }
                    raw_attrs.push((k, v));
                }
                scopes.push(scope);
                let raw_name = e.name().0.to_owned();
                let name = canonical_name(&raw_name, &scopes);
                let inside = match stack.last() {
                    Some(Kind::Inner(_)) => true,
                    Some(Kind::Prop { name: parent, .. }) => parent == "mwg-rs:Regions",
                    _ => false,
                };
                let kind = match stack.last() {
                    _ if inside => {
                        let marked = raw_attrs.iter().any(|(k, _)| canonical_name(k, &scopes) == EMBER_REGION_MARK);
                        let depth = stack.len().saturating_sub(regions_base.unwrap_or(0));
                        out.inner.push(Elem { name, raw_name, depth, start: before, end: after, empty, marked });
                        Kind::Inner(out.inner.len() - 1)
                    }
                    Some(Kind::Rdf) if name == "rdf:Description" => {
                        let attrs = raw_attrs.into_iter().map(|(raw, value)| Attr { name: canonical_name(&raw, &scopes), raw, value }).collect();
                        out.descs.push(Desc { tag_start: before, tag_end: after, raw_name, empty, attrs, props: Vec::new(), end: after });
                        Kind::Desc(out.descs.len() - 1)
                    }
                    Some(Kind::Desc(i)) => Kind::Prop { desc: *i, start: before, name },
                    _ if name == "rdf:RDF" && out.rdf_close.is_none() => Kind::Rdf,
                    _ => Kind::Other,
                };
                if let Kind::Prop { name, .. } = &kind
                    && name == "mwg-rs:Regions"
                {
                    out.regions_props += 1;
                    if !empty {
                        regions_base = Some(stack.len());
                    }
                }
                if empty {
                    scopes.pop();
                    if let Kind::Prop { desc, start, name } = kind
                        && let Some(d) = out.descs.get_mut(desc)
                    {
                        d.props.push(Prop { name, start, end: after });
                    }
                } else {
                    stack.push(kind);
                }
            }
            Event::End(_) => {
                scopes.pop();
                match stack.pop() {
                    Some(Kind::Prop { desc, start, name }) => {
                        if name == "mwg-rs:Regions" {
                            regions_base = None;
                        }
                        if let Some(d) = out.descs.get_mut(desc) {
                            d.props.push(Prop { name, start, end: after });
                        }
                    }
                    Some(Kind::Inner(i)) => {
                        if let Some(e) = out.inner.get_mut(i) {
                            e.end = after;
                        }
                    }
                    Some(Kind::Desc(i)) => {
                        if let Some(d) = out.descs.get_mut(i) {
                            d.end = after;
                        }
                    }
                    Some(Kind::Rdf) => out.rdf_close = out.rdf_close.or(Some(before)),
                    Some(Kind::Other) => {}
                    None => return Err(XmpError::Xml("unbalanced end tag".into())),
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(XmpError::Xml("unclosed element".into()));
    }
    Ok(out)
}

/// `start` moved back over the indentation and line break before it (so a removed property
/// takes its line with it).
fn line_start(s: &str, start: usize) -> usize {
    let b = s.as_bytes();
    let mut i = start;
    while i > 0 && matches!(b.get(i - 1), Some(b' ' | b'\t')) {
        i -= 1;
    }
    if i > 0 && b.get(i - 1) == Some(&b'\n') {
        i -= 1;
        if i > 0 && b.get(i - 1) == Some(&b'\r') {
            i -= 1;
        }
    }
    i
}

/// Merge `fresh` (a packet as [`crate::write_xmp_lc`] writes it) into `existing` following
/// `rules` (see the module docs). Fails — and the caller must not overwrite `existing` blindly —
/// when `existing` isn't a well-formed XMP packet.
pub fn merge_xmp(existing: &str, fresh: &str, rules: MergeRules) -> Result<String, XmpError> {
    let (bom, old) = match existing.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", existing),
    };
    parse_xmp(old)?;
    let ex = scan(old)?;
    let rdf_close = ex.rdf_close.ok_or_else(|| XmpError::Xml("no rdf:RDF element".into()))?;
    let new = scan(fresh)?;
    let fresh_desc = new.descs.first().ok_or_else(|| XmpError::Xml("the new packet has no rdf:Description".into()))?;
    let fresh_names: BTreeSet<&str> = fresh_desc.props.iter().map(|p| p.name.as_str()).collect();
    let remove = |name: &str| listed(rules.owned, name) || (listed(rules.owned_if_present, name) && fresh_names.contains(name));
    let write = |name: &str| listed(rules.owned, name) || listed(rules.owned_if_present, name);

    // edits on `old`: (start, end, replacement), applied back to front
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for d in &ex.descs {
        let gone_attrs: Vec<&Attr> = d.attrs.iter().filter(|a| !is_meta_attr(&a.name) && remove(&a.name)).collect();
        let gone_props: Vec<&Prop> = d.props.iter().filter(|p| remove(&p.name)).collect();
        if gone_attrs.is_empty() && gone_props.is_empty() {
            continue;
        }
        let attrs_left = d.attrs.iter().any(|a| !is_meta_attr(&a.name) && !remove(&a.name));
        if !attrs_left && gone_props.len() == d.props.len() {
            // nothing of its own left: drop the whole description
            edits.push((line_start(old, d.tag_start), d.end, String::new()));
            continue;
        }
        if !gone_attrs.is_empty() {
            let mut tag = format!("<{}", d.raw_name);
            for a in d.attrs.iter().filter(|a| is_meta_attr(&a.name) || !remove(&a.name)) {
                tag.push_str(&format!("\n    {}=\"{}\"", a.raw, a.value.replace('"', "&quot;")));
            }
            tag.push_str(if d.empty { "/>" } else { ">" });
            edits.push((d.tag_start, d.tag_end, tag));
        }
        for p in gone_props {
            edits.push((line_start(old, p.start), p.end, String::new()));
        }
    }
    // the new description: its own namespace declarations, the properties to write
    let mut block = String::from("\n  <rdf:Description rdf:about=\"\"\n    xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"");
    for a in fresh_desc.attrs.iter().filter(|a| a.raw.starts_with("xmlns:") && a.raw != "xmlns:rdf") {
        block.push_str(&format!("\n    {}=\"{}\"", a.raw, a.value));
    }
    block.push('>');
    let mut written = 0;
    for p in fresh_desc.props.iter().filter(|p| write(&p.name)) {
        let text = fresh.get(p.start..p.end).ok_or_else(|| XmpError::Xml("bad property span".into()))?;
        block.push_str("\n   ");
        block.push_str(text);
        written += 1;
    }
    block.push_str("\n  </rdf:Description>");
    if written > 0 {
        let at = old.get(..rdf_close).map_or(rdf_close, |h| h.trim_end().len());
        edits.push((at, at, block));
    }
    edits.sort_by_key(|e| (e.0, e.1));
    // overlapping edits can't happen (properties sit inside their description, which is either
    // dropped whole or edited piecewise), but never splice blindly
    let mut out = String::with_capacity(old.len() + 4096);
    out.push_str(bom);
    let mut at = 0;
    for (s, e, r) in &edits {
        if *s < at {
            return Err(XmpError::Xml("overlapping edits".into()));
        }
        out.push_str(old.get(at..*s).ok_or_else(|| XmpError::Xml("bad span".into()))?);
        out.push_str(r);
        at = *e;
    }
    out.push_str(old.get(at..).ok_or_else(|| XmpError::Xml("bad span".into()))?);

    // read back: everything written must come out as written
    let merged = parse_xmp(&out)?;
    let want = parse_xmp(fresh)?;
    for (k, v) in &want.properties {
        let top = k.split('/').next().unwrap_or(k);
        if write(top) && merged.properties.get(k) != Some(v) {
            return Err(XmpError::Xml(format!("{k} did not survive the merge")));
        }
    }
    Ok(out)
}

/// The region items Ember writes into a packet that already has `mwg-rs:Regions` (see [`merge_ember_regions`]).
#[derive(Clone, Debug, Default)]
pub struct EmberRegions {
    /// Every region of the photo, upright (as [`Region::rect`] is stored). Those that sit on the same box as another
    /// application's region are not written: the other application's region stands for that face.
    pub regions: Vec<Region>,
    /// The Exif orientation of the file's stored image, which turns the boxes into the frame other applications read.
    pub orientation: Option<Orientation>,
}

/// How near two boxes' edges are for them to be the same face (normalized units).
const SAME_BOX: f64 = 1.0e-3;

fn same_box(a: &Rect, b: &Rect) -> bool {
    [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1].iter().all(|d| d.abs() < SAME_BOX)
}

/// Where the region items of a packet's `mwg-rs:Regions` sit: the container (`rdf:Bag`, `Seq` or `Alt`) of its
/// `mwg-rs:RegionList` and the indices (into [`Scan::inner`]) of its `rdf:li` items.
fn region_bag(inner: &[Elem]) -> Option<(usize, Vec<usize>)> {
    let list = inner.iter().position(|e| e.name == "mwg-rs:RegionList" && e.depth <= 2)?;
    let (list_end, list_depth) = inner.get(list).map(|e| (e.end, e.depth))?;
    let bag = (list + 1..inner.len())
        .take_while(|i| inner.get(*i).is_some_and(|e| e.start < list_end))
        .find(|i| inner.get(*i).is_some_and(|e| e.depth == list_depth + 1 && matches!(e.name.as_str(), "rdf:Bag" | "rdf:Seq" | "rdf:Alt")))?;
    let (bag_end, bag_depth) = inner.get(bag).map(|e| (e.end, e.depth))?;
    let items = (bag + 1..inner.len())
        .take_while(|i| inner.get(*i).is_some_and(|e| e.start < bag_end))
        .filter(|i| inner.get(*i).is_some_and(|e| e.depth == bag_depth + 1 && e.name == "rdf:li"))
        .collect();
    Some((bag, items))
}

/// `old` with `edits` (start, end, replacement; sorted, not overlapping) applied.
fn apply_edits(old: &str, edits: &[(usize, usize, String)]) -> Result<String, XmpError> {
    let mut out = String::with_capacity(old.len() + 1024);
    let mut at = 0;
    for (s, e, r) in edits {
        if *s < at || *e < *s {
            return Err(XmpError::Xml("overlapping edits".into()));
        }
        out.push_str(old.get(at..*s).ok_or_else(|| XmpError::Xml("bad span".into()))?);
        out.push_str(r);
        at = *e;
    }
    out.push_str(old.get(at..).ok_or_else(|| XmpError::Xml("bad span".into()))?);
    Ok(out)
}

/// Merge Ember's regions into the `mwg-rs:Regions` another application wrote into `existing`.
///
/// Only the items Ember wrote before (those carrying `lc:EmberRegion="1"`) are replaced by `ember.regions`, appended
/// at the end of the same `RegionList` bag; every other item, the rest of the block and the rest of the packet are
/// copied byte for byte, in their order, with their unknown attributes and namespaces. A photo region on the same box as
/// another application's region is not written again. So adding, renaming and removing a face in Ember touches only
/// Ember's own items, and the same call twice gives the same packet.
///
/// The packet is returned unchanged when it has no `mwg-rs:Regions` (the caller adds one then), more than one, or one
/// without a `RegionList` bag it can place items in. It fails when `existing` isn't well-formed XMP, or too deeply
/// nested or large, and the caller then keeps `existing` as it is.
pub fn merge_ember_regions(existing: &str, ember: &EmberRegions) -> Result<String, XmpError> {
    let (bom, old) = match existing.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", existing),
    };
    parse_xmp(old)?;
    let ex = scan(old)?;
    if ex.regions_props != 1 {
        return Ok(existing.to_string());
    }
    let Some((bag_at, item_at)) = region_bag(&ex.inner) else { return Ok(existing.to_string()) };
    let Some(bag) = ex.inner.get(bag_at) else { return Ok(existing.to_string()) };
    // 1. remove Ember's earlier items
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for item in item_at.iter().filter_map(|i| ex.inner.get(*i)).filter(|e| e.marked) {
        edits.push((line_start(old, item.start), item.end, String::new()));
    }
    // 2. the other application's regions, as an application reads them: the packet without Ember's items
    let foreign = parse_xmp(&apply_edits(old, &edits)?)?.metadata.regions;
    let wanted: Vec<Region> = ember.regions.iter().filter(|r| !foreign.iter().any(|f| same_box(&f.rect, &r.rect))).cloned().collect();
    // 3. add Ember's items at the end of the bag. Each carries the namespace declarations it uses, so it reads the same
    // whatever prefixes the rest of the packet binds.
    if !wanted.is_empty() {
        let decls = ns_decls(&["rdf", "lc", "mwg-rs", "stArea"]);
        let mut text = String::new();
        for item in region_items(&wanted, ember.orientation) {
            text.push_str("\n     ");
            text.push_str(&item.replacen("<rdf:li ", &format!("<rdf:li{decls} "), 1));
        }
        if bag.empty {
            let name = &bag.raw_name;
            edits.push((bag.start, bag.end, format!("<{name}>{text}\n    </{name}>")));
        } else {
            let close = old.get(..bag.end).and_then(|head| head.rfind("</")).ok_or_else(|| XmpError::Xml("no closing tag".into()))?;
            let at = old.get(..close).map_or(close, |head| head.trim_end().len());
            edits.push((at, at, text));
        }
    }
    if edits.is_empty() {
        return Ok(existing.to_string());
    }
    edits.sort_by_key(|e| (e.0, e.1));
    let mut out = String::from(bom);
    out.push_str(&apply_edits(old, &edits)?);
    // read back: the packet must still read, and keep every region of the other application
    let merged = parse_xmp(&out)?;
    if merged.metadata.regions.len() < foreign.len() || !merged.values.contains_key("mwg-rs:Regions") {
        return Err(XmpError::Xml("the regions did not survive the merge".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: MergeRules =
        MergeRules { owned: &["xmp:Rating", "xmp:Label", "dc:subject", "dc:title", "lc:*"], owned_if_present: &["exif:DateTimeOriginal"] };

    /// A sidecar as another raw developer writes it: attribute-form simple properties, develop
    /// settings, a structured history, an unknown namespace.
    const FOREIGN: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Some Toolkit 1.0\">
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">
  <rdf:Description rdf:about=\"\"
    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"
    xmlns:xmpMM=\"http://ns.adobe.com/xap/1.0/mm/\"
    xmlns:stEvt=\"http://ns.adobe.com/xap/1.0/sType/ResourceEvent#\"
    xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"
    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"
    xmlns:foo=\"http://example.com/foo/\"
    xmp:Rating=\"2\"
    xmp:CreatorTool=\"Other App\"
    crs:Exposure2012=\"+0.50\"
    crs:Contrast2012=\"-12\"
    foo:Thing=\"a &amp; b\">
   <xmpMM:History>
    <rdf:Seq>
     <rdf:li stEvt:action=\"saved\" stEvt:when=\"2026-01-02T03:04:05\"/>
    </rdf:Seq>
   </xmpMM:History>
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
   <dc:subject>
    <rdf:Bag>
     <rdf:li>old</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
";

    /// With the packet's whitespace padding and trailer.
    fn foreign() -> String {
        format!("{FOREIGN}{}\n<?xpacket end=\"w\"?>", " ".repeat(100))
    }

    fn fresh(rating: i8, keywords: &[&str]) -> String {
        let m = crate::Metadata {
            software: Some("Ember".into()),
            rating: Some(rating),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            ..Default::default()
        };
        crate::write_xmp_lc(&m, &[("settings", "{\"a\":1}"), ("flag", "pick")])
    }

    #[test]
    fn keeps_foreign_properties_and_replaces_owned_ones() {
        let out = merge_xmp(&foreign(), &fresh(4, &["new", "two"]), RULES).unwrap();
        let d = parse_xmp(&out).unwrap();
        let p = |k: &str| d.properties.get(k).cloned().unwrap_or_default();
        assert_eq!(p("xmp:Rating"), vec!["4"]);
        assert_eq!(p("dc:subject"), vec!["new", "two"]);
        assert_eq!(p("lc:settings"), vec!["{\"a\":1}"]);
        assert_eq!(p("lc:flag"), vec!["pick"]);
        assert_eq!(p("xmp:CreatorTool"), vec!["Other App"], "not ours: kept");
        assert_eq!(p("crs:Exposure2012"), vec!["+0.50"]);
        assert_eq!(p("foo:Thing").len(), 1);
        // untouched parts are byte for byte the same
        for part in [
            "   <xmpMM:History>\n    <rdf:Seq>\n     <rdf:li stEvt:action=\"saved\" stEvt:when=\"2026-01-02T03:04:05\"/>\n    </rdf:Seq>\n   </xmpMM:History>",
            "   <crs:ToneCurvePV2012>\n    <rdf:Seq>\n     <rdf:li>0, 0</rdf:li>",
            "crs:Contrast2012=\"-12\"",
            "foo:Thing=\"a &amp; b\"",
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Some Toolkit 1.0\">",
            &format!("</x:xmpmeta>\n{}\n<?xpacket end=\"w\"?>", " ".repeat(100)),
        ] {
            assert!(out.contains(part), "lost {part:?} in\n{out}");
        }
        assert!(!out.contains("xmp:Rating=\"2\""));
        assert!(!out.contains("<rdf:li>old</rdf:li>"));
        // saving again changes only our part, and doesn't pile up descriptions
        let again = merge_xmp(&out, &fresh(1, &[]), RULES).unwrap();
        let d = parse_xmp(&again).unwrap();
        assert_eq!(d.properties.get("xmp:Rating"), Some(&vec!["1".to_string()]));
        assert!(!d.properties.contains_key("dc:subject"), "a cleared field is cleared");
        assert_eq!(again.matches("<rdf:Description").count(), 2, "{again}");
        assert_eq!(merge_xmp(&again, &fresh(1, &[]), RULES).unwrap(), again, "idempotent");
    }

    #[test]
    fn owned_if_present_and_prefix_independence() {
        // the existing packet binds the namespaces to other prefixes, and has a capture time
        let ex = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><r:RDF xmlns:r=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
            <r:Description r:about=\"\" xmlns:a=\"http://ns.adobe.com/xap/1.0/\" xmlns:e=\"http://ns.adobe.com/exif/1.0/\" \
            a:Rating=\"5\" e:DateTimeOriginal=\"2020-01-01T00:00:00\"/></r:RDF></x:xmpmeta>";
        let out = merge_xmp(ex, &fresh(3, &[]), RULES).unwrap();
        let d = parse_xmp(&out).unwrap();
        assert_eq!(d.properties.get("xmp:Rating"), Some(&vec!["3".to_string()]));
        assert_eq!(d.properties.get("exif:DateTimeOriginal"), Some(&vec!["2020-01-01T00:00:00".to_string()]), "fresh has none: kept");
        let mut m = crate::Metadata { rating: Some(3), ..Default::default() };
        m.capture_time = crate::DateTime::parse_iso("2024-05-06T07:08:09");
        let out = merge_xmp(ex, &crate::write_xmp_lc(&m, &[]), RULES).unwrap();
        let d = parse_xmp(&out).unwrap();
        assert_eq!(d.properties.get("exif:DateTimeOriginal"), Some(&vec!["2024-05-06T07:08:09".to_string()]));
    }

    #[test]
    fn many_attributes_scan_in_linear_time() {
        // quick-xml's duplicate-name check compares each attribute with all earlier ones: 200 000 attributes on
        // one description took over 20 s to scan, about 30 ms with the set
        let n = 200_000;
        let mut s = String::from(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             <rdf:Description rdf:about=\"\" xmlns:foo=\"http://example.com/foo/\"",
        );
        for i in 0..n {
            s.push_str(&format!(" foo:a{i}=\"{i}\""));
        }
        s.push_str("/></rdf:RDF></x:xmpmeta>");
        let t = std::time::Instant::now();
        let found = scan(&s).unwrap();
        let took = t.elapsed();
        assert_eq!(found.descs[0].attrs.len(), n + 2, "every attribute is read");
        assert!(took < std::time::Duration::from_secs(5), "{n} attributes took {took:?}");
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        for bad in ["", "not xml at all", "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF", "<a><b></a>", "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>"] {
            assert!(merge_xmp(bad, &fresh(1, &[]), RULES).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn refuses_a_repeated_attribute() {
        // not well-formed XML (strict readers such as expat refuse it): no merge, so the caller keeps a backup.
        // An owned name, a foreign one and one on a nested element (a merge would keep both copies of the last two).
        for (once, twice) in [
            ("xmp:Rating=\"2\"", "xmp:Rating=\"2\" xmp:Rating=\"3\""),
            ("foo:Thing=\"a &amp; b\"", "foo:Thing=\"a &amp; b\" foo:Thing=\"c\""),
            ("stEvt:action=\"saved\"", "stEvt:action=\"saved\" stEvt:action=\"saved\""),
        ] {
            let ex = foreign().replace(once, twice);
            assert!(merge_xmp(&ex, &fresh(1, &[]), RULES).is_err(), "{twice}");
        }
    }

    // ---- merge_ember_regions ----

    /// One of another application's regions: an unknown attribute on the item, and a nested extension in it.
    const FOREIGN_ITEM: &str = "     <rdf:li rdf:parseType=\"Resource\" foo:Score=\"0.93\">
      <mwg-rs:Area rdf:parseType=\"Resource\" stArea:x=\"0.5\" stArea:y=\"0.4\" stArea:w=\"0.2\" stArea:h=\"0.3\" stArea:unit=\"normalized\"/>
      <mwg-rs:Type>Face</mwg-rs:Type>
      <mwg-rs:Name>Jane Doe</mwg-rs:Name>
      <foo:Extension><foo:Engine foo:v=\"2\"/></foo:Extension>
     </rdf:li>";

    fn with_regions(items: &str) -> String {
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Other App\">
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">
  <rdf:Description rdf:about=\"\"
    xmlns:mwg-rs=\"http://www.metadataworkinggroup.com/schemas/regions/\"
    xmlns:stArea=\"http://ns.adobe.com/xmp/sType/Area#\"
    xmlns:stDim=\"http://ns.adobe.com/xap/1.0/sType/Dimensions#\"
    xmlns:foo=\"http://example.com/foo/\"
    foo:Thing=\"keep\">
   <mwg-rs:Regions rdf:parseType=\"Resource\">
    <mwg-rs:AppliedToDimensions rdf:parseType=\"Resource\" stDim:w=\"4000\" stDim:h=\"3000\" stDim:unit=\"pixel\"/>
    <mwg-rs:RegionList>
     <rdf:Bag>
{items}
     </rdf:Bag>
    </mwg-rs:RegionList>
   </mwg-rs:Regions>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end=\"w\"?>"
        )
    }

    fn face(x0: f64, y0: f64, x1: f64, y1: f64, name: &str) -> Region {
        Region { rect: Rect::new(x0, y0, x1, y1), kind: crate::RegionKind::Face, name: Some(name.to_string()), description: None }
    }

    fn ember(regions: Vec<Region>, orientation: Option<Orientation>) -> EmberRegions {
        EmberRegions { regions, orientation }
    }

    fn names(packet: &str) -> Vec<String> {
        parse_xmp(packet).unwrap().metadata.regions.iter().filter_map(|r| r.name.clone()).collect()
    }

    /// The text of `packet` around its `rdf:Bag`'s items: everything but them.
    fn around_bag(packet: &str) -> (&str, &str) {
        let head = packet.find("<rdf:Bag>").map(|i| i + "<rdf:Bag>".len()).unwrap();
        let tail = packet.rfind("\n     </rdf:Bag>").unwrap();
        (&packet[..head], &packet[tail..])
    }

    #[test]
    fn a_foreign_region_survives_an_ember_write_byte_for_byte() {
        let old = with_regions(FOREIGN_ITEM);
        let out = merge_ember_regions(&old, &ember(vec![face(0.5 - 0.1, 0.4 - 0.15, 0.5 + 0.1, 0.4 + 0.15, "Jane Doe")], None)).unwrap();
        assert_eq!(out, old, "Ember's region sits on the foreign one's box: nothing to write");
        // nothing to write and nothing of Ember's to remove: the packet is returned as it was
        let out = merge_ember_regions(&old, &ember(Vec::new(), None)).unwrap();
        assert_eq!(out, old);
    }

    #[test]
    fn an_ember_region_is_added_next_to_the_foreign_one() {
        let old = with_regions(FOREIGN_ITEM);
        let out = merge_ember_regions(&old, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap();
        let (head, tail) = around_bag(&old);
        assert!(out.starts_with(head) && out.ends_with(tail), "{out}");
        assert!(out.contains(FOREIGN_ITEM), "the foreign item is untouched: {out}");
        assert_eq!(out.matches("lc:EmberRegion=\"1\"").count(), 1);
        assert_eq!(names(&out), vec!["Jane Doe", "Ann"]);
        let regions = parse_xmp(&out).unwrap().metadata.regions;
        let r = regions[1].rect;
        assert!((r.x0 - 0.1).abs() < 1e-9 && (r.y0 - 0.1).abs() < 1e-9 && (r.x1 - 0.2).abs() < 1e-9 && (r.y1 - 0.25).abs() < 1e-9, "{r:?}");
        assert_eq!(regions[1].kind, crate::RegionKind::Face);
        // the same call again gives the same packet
        assert_eq!(merge_ember_regions(&out, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap(), out);
    }

    #[test]
    fn renaming_or_deleting_the_ember_face_leaves_the_foreign_one_identical() {
        let old = with_regions(FOREIGN_ITEM);
        let (head, tail) = around_bag(&old);
        let added = merge_ember_regions(&old, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap();
        // rename: the foreign face (as the photo has it, read from the sidecar) is in the list too
        let jane = face(0.4, 0.25, 0.6, 0.55, "Jane Doe");
        let renamed = merge_ember_regions(&added, &ember(vec![jane.clone(), face(0.1, 0.1, 0.2, 0.25, "Anna")], None)).unwrap();
        assert!(renamed.starts_with(head) && renamed.ends_with(tail) && renamed.contains(FOREIGN_ITEM), "{renamed}");
        assert_eq!(names(&renamed), vec!["Jane Doe", "Anna"]);
        assert_eq!(renamed.matches("lc:EmberRegion=\"1\"").count(), 1);
        // delete: back to the packet as it was
        let deleted = merge_ember_regions(&renamed, &ember(vec![jane], None)).unwrap();
        assert_eq!(deleted, old);
    }

    #[test]
    fn a_rename_of_a_foreign_face_does_not_add_a_second_one() {
        let old = with_regions(FOREIGN_ITEM);
        let out = merge_ember_regions(&old, &ember(vec![face(0.4, 0.25, 0.6, 0.55, "Janey")], None)).unwrap();
        assert_eq!(out, old);
    }

    #[test]
    fn foreign_items_after_ours_keep_their_order() {
        let old = with_regions(FOREIGN_ITEM);
        let added = merge_ember_regions(&old, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap();
        // another application appends its own region after Ember's
        let late = FOREIGN_ITEM.replace("Jane Doe", "Late One").replace("stArea:x=\"0.5\"", "stArea:x=\"0.8\"");
        let tail = "\n     </rdf:Bag>";
        let at = added.rfind(tail).unwrap();
        let with_late = format!("{}\n{late}{}", &added[..at], &added[at..]);
        assert_eq!(names(&with_late), vec!["Jane Doe", "Ann", "Late One"]);
        let out = merge_ember_regions(&with_late, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap();
        assert!(out.contains(FOREIGN_ITEM) && out.contains(&late));
        assert_eq!(names(&out), vec!["Jane Doe", "Late One", "Ann"], "ours moves to the end, the foreign order stays");
    }

    #[test]
    fn works_with_other_prefixes_and_an_empty_bag() {
        let old = with_regions(FOREIGN_ITEM).replace("mwg-rs", "m").replace("stArea", "sa");
        let out = merge_ember_regions(&old, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap();
        assert!(out.contains(&FOREIGN_ITEM.replace("mwg-rs", "m").replace("stArea", "sa")));
        assert_eq!(names(&out), vec!["Jane Doe", "Ann"]);
        // an empty bag gets the item inside it
        let empty = with_regions("").replace("<rdf:Bag>\n\n     </rdf:Bag>", "<rdf:Bag/>");
        assert!(empty.contains("<rdf:Bag/>"));
        let out = merge_ember_regions(&empty, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap();
        assert_eq!(names(&out), vec!["Ann"], "{out}");
        assert_eq!(merge_ember_regions(&out, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap(), out);
    }

    #[test]
    fn a_sidecar_without_regions_gets_a_new_bag() {
        let ex = foreign();
        assert_eq!(merge_ember_regions(&ex, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None)).unwrap(), ex, "no bag to merge into");
        let m = crate::Metadata { regions: vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], ..Default::default() };
        let rules = MergeRules { owned: &["lc:*"], owned_if_present: &["mwg-rs:Regions"] };
        let out = merge_xmp(&ex, &crate::write_xmp_lc_with_regions(&m, &[]), rules).unwrap();
        assert_eq!(names(&out), vec!["Ann"], "{out}");
        assert_eq!(out.matches("lc:EmberRegion=\"1\"").count(), 1);
        // and from then on the region merge keeps working on it
        let out2 = merge_ember_regions(&out, &ember(vec![face(0.1, 0.1, 0.2, 0.25, "Anna")], None)).unwrap();
        assert_eq!(names(&out2), vec!["Anna"], "{out2}");
    }

    #[test]
    fn a_turned_files_area_round_trips() {
        // Exif orientation 6: the box is written in the stored frame with a rotation, and reads back upright
        let old = with_regions(FOREIGN_ITEM);
        let r = face(0.1, 0.2, 0.3, 0.5, "Ann");
        let out = merge_ember_regions(&old, &ember(vec![r.clone()], Some(Orientation::Rotate90))).unwrap();
        assert!(out.contains("<mwg-rs:Rotation>"), "{out}");
        let back = parse_xmp(&out).unwrap().metadata.regions;
        let b = back.iter().find(|x| x.name.as_deref() == Some("Ann")).unwrap().rect;
        assert!(
            (b.x0 - r.rect.x0).abs() < 1e-9 && (b.y0 - r.rect.y0).abs() < 1e-9 && (b.x1 - r.rect.x1).abs() < 1e-9 && (b.y1 - r.rect.y1).abs() < 1e-9,
            "{b:?} vs {:?}",
            r.rect
        );
        // rewriting gives the same packet
        assert_eq!(merge_ember_regions(&out, &ember(vec![r], Some(Orientation::Rotate90))).unwrap(), out);
    }

    #[test]
    fn malformed_or_deep_regions_are_an_error_or_left_alone() {
        let one = ember(vec![face(0.1, 0.1, 0.2, 0.25, "Ann")], None);
        let ok = with_regions(FOREIGN_ITEM);
        // not well-formed
        for bad in [ok.replace("</mwg-rs:Regions>", ""), ok.replace("</rdf:Bag>", ""), ok.replace("<rdf:Bag>", "<rdf:Bag><b>"), "<a><b></a>".into()] {
            assert!(merge_ember_regions(&bad, &one).is_err(), "{bad}");
        }
        // nested far too deep
        let deep = with_regions(&format!("{}{}", "<foo:a>".repeat(500), "</foo:a>".repeat(500)));
        match merge_ember_regions(&deep, &one) {
            Err(_) => {}
            Ok(out) => assert_eq!(out, deep),
        }
        // no list, a list without a bag, two Regions: left as they are
        let no_list = ok.replace("<mwg-rs:RegionList>", "<mwg-rs:Other>").replace("</mwg-rs:RegionList>", "</mwg-rs:Other>");
        let no_bag = ok.replace("<rdf:Bag>", "<foo:Bag>").replace("</rdf:Bag>", "</foo:Bag>");
        let empty_list = "<?xpacket begin=\"\u{feff}\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
            <rdf:Description rdf:about=\"\" xmlns:mwg-rs=\"http://www.metadataworkinggroup.com/schemas/regions/\">\
            <mwg-rs:Regions rdf:parseType=\"Resource\"><mwg-rs:RegionList/></mwg-rs:Regions></rdf:Description></rdf:RDF></x:xmpmeta>";
        let two = ok.replace("  </rdf:Description>", "   <mwg-rs:Regions rdf:parseType=\"Resource\"/>\n  </rdf:Description>");
        for same in [no_list, no_bag, empty_list.to_string(), two] {
            assert_eq!(merge_ember_regions(&same, &one).unwrap(), same);
        }
    }
}
