//! AVIF (the AV1 image file format), read-only.
//!
//! The container is parsed here, in safe, bounded code: `ftyp` brands, then `meta` with `hdlr`
//! (`pict`), `pitm`, `iinf`/`infe`, `iloc`, `iref`, `iprp` (`ipco` + `ipma`) and `idat`. The
//! properties used are `ispe`, `colr` (`nclx` and ICC), `irot`, `imir`, `clap` and `auxC`. Items
//! are `av01` images and `grid` images (tiles named by `dimg` references); an alpha plane is an
//! `av01` item with an `auxC` alpha URN and an `auxl` reference to the colour item (`prem`
//! marks premultiplied alpha). Exif (`Exif` item) and XMP (`mime` item) are read when linked by
//! `cdsc`. Image sequences (`avis`) are read through their still image when the file has one.
//!
//! The AV1 pictures are decoded by `lightcraft-av1dec` (rav1d) to 8, 10 or 12-bit planes, which
//! are converted to 16-bit RGB here with the `nclx` matrix and range (the AV1 sequence header's
//! when the file has no `nclx`; BT.709 when neither says), bilinear chroma upsampling, then the
//! container's transforms (`clap`, `irot`, `imir`, in association order) are applied. Like HEIF,
//! AVIF records orientation in the container only, so `orientation` is 1 and Exif's tag is not
//! applied. The samples go through [`crate::convert::finish`] with the file's ICC profile or its
//! `nclx` primaries and transfer (sRGB, BT.709, linear, Display P3 and Rec. 2020 are modelled;
//! PQ and HLG are not and read as sRGB).
//!
//! Hostile input: every box length, count, offset and size is checked, nesting and item, extent,
//! property, reference and tile counts are capped, the bytes handed to the decoder are bounded by
//! the file's size, and the picture size is checked against [`DecodeOptions::max_pixels`] before
//! anything is decoded. Malformed input is an error, never a panic.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;

use lightcraft_av1dec as av1;

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::space::SourceSpace;
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Avif;

/// Deepest box nesting read (`meta` > `iprp` > `ipco` > property is 4).
const MAX_DEPTH: u32 = 6;
/// Most boxes read at one level.
const MAX_BOXES: usize = 1 << 16;
const MAX_ITEMS: usize = 8192;
const MAX_EXTENTS: usize = 64;
const MAX_PROPS: usize = 4096;
const MAX_PROPS_PER_ITEM: usize = 64;
const MAX_REFS: usize = 1 << 16;
const MAX_TILES: usize = 4096;
/// Most bytes one item may hold.
const MAX_ITEM_BYTES: u64 = 256 << 20;
/// Largest Exif or XMP block kept.
const MAX_METADATA_BYTES: usize = 16 << 20;

fn bad(m: &str) -> Error {
    Error::Malformed(F, m.to_string())
}

// ---------------------------------------------------------------------------------------------
// Boxes
// ---------------------------------------------------------------------------------------------

/// A bounds-checked big-endian reader.
struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(d: &'a [u8]) -> Self {
        Rd { d, p: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or_else(|| bad("length overflow"))?;
        let s = self.d.get(self.p..end).ok_or_else(|| bad("truncated box"))?;
        self.p = end;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| bad("truncated box"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(u8::from_be_bytes(self.arr()?))
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.arr()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.arr()?))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.arr()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.arr()?))
    }

    /// An `iloc` field: 0, 4 or 8 bytes.
    fn uint(&mut self, size: usize) -> Result<u64> {
        match size {
            0 => Ok(0),
            4 => Ok(u64::from(self.u32()?)),
            8 => self.u64(),
            _ => Err(bad("unsupported field size")),
        }
    }

    fn rest(&self) -> &'a [u8] {
        self.d.get(self.p..).unwrap_or(&[])
    }

    /// A NUL-terminated string (the rest of the data when there is no terminator).
    fn cstr(&mut self) -> &'a [u8] {
        let rest = self.rest();
        match rest.iter().position(|&b| b == 0) {
            Some(n) => {
                self.p += n + 1;
                rest.get(..n).unwrap_or(&[])
            }
            None => {
                self.p = self.d.len();
                rest
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Bx<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// The boxes laid end to end in `data`. Fewer than 8 trailing bytes are ignored; a box that runs
/// past `data` is an error.
fn children(data: &[u8], depth: u32) -> Result<Vec<Bx<'_>>> {
    if depth > MAX_DEPTH {
        return Err(bad("boxes nested too deeply"));
    }
    let mut out = Vec::new();
    let mut r = Rd::new(data);
    while data.len().saturating_sub(r.p) >= 8 {
        if out.len() >= MAX_BOXES {
            return Err(bad("too many boxes"));
        }
        let size = r.u32()?;
        let kind: [u8; 4] = r.arr()?;
        let body_len = match size {
            0 => data.len().saturating_sub(r.p),
            1 => {
                let big = r.u64()?;
                usize::try_from(big).ok().and_then(|b| b.checked_sub(16)).ok_or_else(|| bad("bad box size"))?
            }
            2..=7 => return Err(bad("bad box size")),
            n => (n as usize).saturating_sub(8),
        };
        let body = r.take(body_len)?;
        out.push(Bx { kind, body });
    }
    Ok(out)
}

fn find<'a>(boxes: &[Bx<'a>], kind: &[u8; 4]) -> Option<Bx<'a>> {
    boxes.iter().find(|b| &b.kind == kind).copied()
}

/// A full box's version, flags and the rest of its body.
fn full(body: &[u8]) -> Result<(u8, u32, &[u8])> {
    let mut r = Rd::new(body);
    let w = r.u32()?;
    Ok(((w >> 24) as u8, w & 0x00FF_FFFF, r.rest()))
}

// ---------------------------------------------------------------------------------------------
// Items and properties
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Colr<'a> {
    Nclx { primaries: u16, transfer: u16, matrix: u16, full: bool },
    Icc(&'a [u8]),
}

/// A clean aperture: width, height, horizontal and vertical centre offset, each as a fraction.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Clap {
    w: (u32, u32),
    h: (u32, u32),
    x: (i32, u32),
    y: (i32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Prop<'a> {
    Ispe(u32, u32),
    Colr(Colr<'a>),
    /// Anticlockwise quarter turns, 0..=3.
    Irot(u8),
    /// 0: about the vertical axis (left and right swap); 1: about the horizontal axis.
    Imir(u8),
    Clap(Clap),
    AuxC(&'a [u8]),
    /// A property that doesn't change how the image reads.
    Ignored,
    Other([u8; 4]),
}

struct Item<'a> {
    id: u32,
    kind: [u8; 4],
    content_type: &'a [u8],
    /// `(property index (1-based), essential)` in association order.
    props: Vec<(usize, bool)>,
}

struct Loc {
    method: u8,
    base: u64,
    extents: Vec<(u64, u64)>,
}

struct Reference {
    kind: [u8; 4],
    from: u32,
    to: Vec<u32>,
}

fn parse_prop<'a>(b: &Bx<'a>) -> Result<Prop<'a>> {
    Ok(match &b.kind {
        b"ispe" => {
            let (_, _, x) = full(b.body)?;
            let mut r = Rd::new(x);
            let w = r.u32()?;
            let h = r.u32()?;
            Prop::Ispe(w, h)
        }
        b"colr" => {
            let mut r = Rd::new(b.body);
            let t: [u8; 4] = r.arr()?;
            match &t {
                b"nclx" => {
                    let primaries = r.u16()?;
                    let transfer = r.u16()?;
                    let matrix = r.u16()?;
                    let full = r.u8()? & 0x80 != 0;
                    Prop::Colr(Colr::Nclx { primaries, transfer, matrix, full })
                }
                b"rICC" | b"prof" => Prop::Colr(Colr::Icc(r.rest())),
                _ => Prop::Ignored,
            }
        }
        b"irot" => Prop::Irot(Rd::new(b.body).u8()? & 3),
        b"imir" => Prop::Imir(Rd::new(b.body).u8()? & 1),
        b"clap" => {
            let mut r = Rd::new(b.body);
            let w = (r.u32()?, r.u32()?);
            let h = (r.u32()?, r.u32()?);
            let hx = r.i32()?;
            let hd = r.u32()?;
            let vy = r.i32()?;
            let vd = r.u32()?;
            Prop::Clap(Clap { w: (w.0, w.1), h: (h.0, h.1), x: (hx, hd), y: (vy, vd) })
        }
        b"auxC" => {
            let (_, _, x) = full(b.body)?;
            Prop::AuxC(Rd::new(x).cstr())
        }
        b"av1C" | b"pixi" | b"pasp" | b"clli" | b"mdcv" | b"cclv" | b"a1op" | b"a1lx" | b"lsel" | b"altt" | b"cmin" | b"cmex" | b"rloc" => {
            Prop::Ignored
        }
        k => Prop::Other(*k),
    })
}

fn parse_iinf(body: &[u8]) -> Result<Vec<Item<'_>>> {
    let (v, _, b) = full(body)?;
    let mut r = Rd::new(b);
    let count = if v == 0 { usize::from(r.u16()?) } else { r.u32()? as usize };
    if count > MAX_ITEMS {
        return Err(bad("too many items"));
    }
    let mut items = Vec::new();
    for k in children(r.rest(), 2)?.iter().filter(|k| &k.kind == b"infe") {
        if items.len() >= MAX_ITEMS {
            return Err(bad("too many items"));
        }
        let (v, _, b) = full(k.body)?;
        if v < 2 {
            return Err(bad("unsupported infe version"));
        }
        let mut r = Rd::new(b);
        let id = if v == 2 { u32::from(r.u16()?) } else { r.u32()? };
        let _protection = r.u16()?;
        let kind: [u8; 4] = r.arr()?;
        let _name = r.cstr();
        let content_type: &[u8] = if &kind == b"mime" { r.cstr() } else { &[] };
        items.push(Item { id, kind, content_type, props: Vec::new() });
    }
    Ok(items)
}

fn parse_iloc(body: &[u8]) -> Result<Vec<(u32, Loc)>> {
    let (v, _, b) = full(body)?;
    let mut r = Rd::new(b);
    let sizes = r.u8()?;
    let (offset_size, length_size) = (usize::from(sizes >> 4), usize::from(sizes & 15));
    let sizes = r.u8()?;
    let base_size = usize::from(sizes >> 4);
    let versioned = v == 1 || v == 2;
    let index_size = if versioned { usize::from(sizes & 15) } else { 0 };
    let count = if v < 2 { usize::from(r.u16()?) } else { r.u32()? as usize };
    if count > MAX_ITEMS {
        return Err(bad("too many item locations"));
    }
    let mut out = Vec::new();
    for _ in 0..count {
        let id = if v < 2 { u32::from(r.u16()?) } else { r.u32()? };
        let method = if versioned { (r.u16()? & 0xF) as u8 } else { 0 };
        let _data_reference = r.u16()?;
        let base = r.uint(base_size)?;
        let n = usize::from(r.u16()?);
        if n > MAX_EXTENTS {
            return Err(bad("too many item extents"));
        }
        let mut extents = Vec::with_capacity(n);
        for _ in 0..n {
            if index_size > 0 {
                r.uint(index_size)?;
            }
            let offset = r.uint(offset_size)?;
            let length = r.uint(length_size)?;
            extents.push((offset, length));
        }
        out.push((id, Loc { method, base, extents }));
    }
    Ok(out)
}

fn parse_iref(body: &[u8]) -> Result<Vec<Reference>> {
    let (v, _, b) = full(body)?;
    let mut out = Vec::new();
    let mut total = 0usize;
    for k in children(b, 2)? {
        let mut r = Rd::new(k.body);
        let from = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
        let n = usize::from(r.u16()?);
        total += n;
        if total > MAX_REFS {
            return Err(bad("too many item references"));
        }
        let mut to = Vec::with_capacity(n);
        for _ in 0..n {
            to.push(if v == 0 { u32::from(r.u16()?) } else { r.u32()? });
        }
        out.push(Reference { kind: k.kind, from, to });
    }
    Ok(out)
}

fn parse_iprp<'a>(body: &'a [u8], items: &mut [Item<'a>]) -> Result<Vec<Prop<'a>>> {
    let kids = children(body, 2)?;
    let ipco = find(&kids, b"ipco").ok_or_else(|| bad("no ipco box"))?;
    let boxes = children(ipco.body, 3)?;
    if boxes.len() > MAX_PROPS {
        return Err(bad("too many item properties"));
    }
    let props = boxes.iter().map(parse_prop).collect::<Result<Vec<_>>>()?;
    let mut index: HashMap<u32, usize> = HashMap::new();
    for (n, it) in items.iter().enumerate() {
        index.entry(it.id).or_insert(n);
    }
    for ipma in kids.iter().filter(|k| &k.kind == b"ipma") {
        let (v, flags, b) = full(ipma.body)?;
        let mut r = Rd::new(b);
        let entries = r.u32()? as usize;
        if entries > MAX_ITEMS * 2 {
            return Err(bad("too many property associations"));
        }
        for _ in 0..entries {
            let id = if v < 1 { u32::from(r.u16()?) } else { r.u32()? };
            let n = usize::from(r.u8()?);
            let mut list = Vec::with_capacity(n);
            for _ in 0..n {
                let (essential, idx) = if flags & 1 != 0 {
                    let a = r.u16()?;
                    (a & 0x8000 != 0, usize::from(a & 0x7FFF))
                } else {
                    let a = r.u8()?;
                    (a & 0x80 != 0, usize::from(a & 0x7F))
                };
                // index 0 means "no property"
                if idx != 0 {
                    list.push((idx, essential));
                }
            }
            if let Some(&at) = index.get(&id)
                && let Some(it) = items.get_mut(at)
            {
                if it.props.len() + list.len() > MAX_PROPS_PER_ITEM {
                    return Err(bad("too many properties on an item"));
                }
                it.props.extend(list);
            }
        }
    }
    Ok(props)
}

// ---------------------------------------------------------------------------------------------
// The container
// ---------------------------------------------------------------------------------------------

struct Grid {
    rows: usize,
    cols: usize,
    width: u32,
    height: u32,
    tiles: Vec<u32>,
}

/// A container transform, applied in association order.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Xf {
    Crop(Clap),
    Rotate(u8),
    Mirror(u8),
}

struct Container<'a> {
    file: &'a [u8],
    primary: u32,
    items: Vec<Item<'a>>,
    props: Vec<Prop<'a>>,
    locs: Vec<(u32, Loc)>,
    refs: Vec<Reference>,
    idat: &'a [u8],
    /// Item bytes handed out so far, against `budget` (overlapping extents can't multiply work).
    used: Cell<u64>,
    budget: u64,
}

impl<'a> Container<'a> {
    fn parse(file: &'a [u8]) -> Result<Container<'a>> {
        let top = children(file, 0)?;
        let ftyp = find(&top, b"ftyp").ok_or_else(|| bad("no ftyp box"))?;
        let mut r = Rd::new(ftyp.body);
        let major: [u8; 4] = r.arr()?;
        let _minor = r.u32()?;
        let mut brands = vec![major];
        while brands.len() < 64 {
            match r.arr::<4>() {
                Ok(b) => brands.push(b),
                Err(_) => break,
            }
        }
        if !brands.iter().any(|b| b == b"avif" || b == b"avis") {
            return Err(Error::Unsupported(F, "not an AVIF file"));
        }
        let Some(meta) = find(&top, b"meta") else {
            return Err(if brands.contains(b"avis") {
                Error::Unsupported(F, "an AVIF image sequence without a still image")
            } else {
                bad("no meta box")
            });
        };
        let (_, _, meta_body) = full(meta.body)?;
        let mb = children(meta_body, 1)?;

        let hdlr = find(&mb, b"hdlr").ok_or_else(|| bad("no hdlr box"))?;
        let (_, _, hb) = full(hdlr.body)?;
        let mut r = Rd::new(hb);
        let _predefined = r.u32()?;
        let handler: [u8; 4] = r.arr()?;
        if &handler != b"pict" {
            return Err(Error::Unsupported(F, "the file's primary handler isn't a picture"));
        }

        let pitm = find(&mb, b"pitm").ok_or_else(|| bad("no primary item"))?;
        let (v, _, pb) = full(pitm.body)?;
        let mut r = Rd::new(pb);
        let primary = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };

        let iinf = find(&mb, b"iinf").ok_or_else(|| bad("no iinf box"))?;
        let mut items = parse_iinf(iinf.body)?;
        let iloc = find(&mb, b"iloc").ok_or_else(|| bad("no iloc box"))?;
        let locs = parse_iloc(iloc.body)?;
        let refs = match find(&mb, b"iref") {
            Some(b) => parse_iref(b.body)?,
            None => Vec::new(),
        };
        let props = match find(&mb, b"iprp") {
            Some(b) => parse_iprp(b.body, &mut items)?,
            None => Vec::new(),
        };
        let idat = find(&mb, b"idat").map(|b| b.body).unwrap_or(&[]);
        let budget = (file.len() as u64).saturating_mul(2).saturating_add(1 << 20);
        Ok(Container { file, primary, items, props, locs, refs, idat, used: Cell::new(0), budget })
    }

    fn item(&self, id: u32) -> Option<&Item<'a>> {
        self.items.iter().find(|i| i.id == id)
    }

    fn primary_item(&self) -> Result<&Item<'a>> {
        let it = self.item(self.primary).ok_or_else(|| bad("the primary item isn't described"))?;
        if &it.kind != b"av01" && &it.kind != b"grid" {
            return Err(Error::Unsupported(F, "the primary image isn't an AV1 image or a grid of them"));
        }
        Ok(it)
    }

    fn props_of(&self, item: &Item<'a>) -> Vec<(Prop<'a>, bool)> {
        item.props.iter().filter_map(|&(idx, essential)| self.props.get(idx.checked_sub(1)?).map(|p| (*p, essential))).collect()
    }

    /// Refuses an essential property this decoder doesn't know (the file can't be read right without it).
    fn check_props(&self, item: &Item<'a>) -> Result<()> {
        if self.props_of(item).iter().any(|(p, essential)| *essential && matches!(p, Prop::Other(_))) {
            return Err(Error::Unsupported(F, "an essential item property this decoder doesn't know"));
        }
        Ok(())
    }

    fn ispe(&self, item: &Item<'a>) -> Option<(u32, u32)> {
        self.props_of(item).into_iter().find_map(|(p, _)| if let Prop::Ispe(w, h) = p { Some((w, h)) } else { None })
    }

    fn nclx(&self, item: &Item<'a>) -> Option<(u16, u16, u16, bool)> {
        self.props_of(item).into_iter().find_map(|(p, _)| match p {
            Prop::Colr(Colr::Nclx { primaries, transfer, matrix, full }) => Some((primaries, transfer, matrix, full)),
            _ => None,
        })
    }

    fn icc(&self, item: &Item<'a>) -> Option<&'a [u8]> {
        self.props_of(item).into_iter().find_map(|(p, _)| if let Prop::Colr(Colr::Icc(b)) = p { Some(b) } else { None })
    }

    fn transforms(&self, item: &Item<'a>) -> Vec<Xf> {
        self.props_of(item)
            .into_iter()
            .filter_map(|(p, _)| match p {
                Prop::Clap(c) => Some(Xf::Crop(c)),
                Prop::Irot(a) if a != 0 => Some(Xf::Rotate(a)),
                Prop::Imir(m) => Some(Xf::Mirror(m)),
                _ => None,
            })
            .collect()
    }

    /// The slices of the file (or `idat`) that hold an item, checked to lie inside it.
    fn locate(&self, id: u32) -> Result<(Vec<&'a [u8]>, u64)> {
        let loc = self.locs.iter().find(|(i, _)| *i == id).map(|(_, l)| l).ok_or_else(|| bad("an item has no location"))?;
        let src: &'a [u8] = match loc.method {
            0 => self.file,
            1 => self.idat,
            _ => return Err(Error::Unsupported(F, "item data stored by reference to another item")),
        };
        let mut parts = Vec::with_capacity(loc.extents.len());
        let mut total = 0u64;
        for &(offset, length) in &loc.extents {
            let start = loc.base.checked_add(offset).ok_or_else(|| bad("item offset overflow"))?;
            let end = start.checked_add(length).ok_or_else(|| bad("item length overflow"))?;
            let (s, e) = match (usize::try_from(start), usize::try_from(end)) {
                (Ok(s), Ok(e)) => (s, e),
                _ => return Err(bad("item data runs past the end of the file")),
            };
            parts.push(src.get(s..e).ok_or_else(|| bad("item data runs past the end of the file"))?);
            total = total.saturating_add(length);
            if total > MAX_ITEM_BYTES {
                return Err(bad("item too large"));
            }
        }
        Ok((parts, total))
    }

    fn item_data(&self, id: u32) -> Result<Cow<'a, [u8]>> {
        let (parts, total) = self.locate(id)?;
        let used = self.used.get().saturating_add(total);
        if used > self.budget {
            return Err(bad("item data is referenced more often than the file is large"));
        }
        self.used.set(used);
        if let [one] = parts.as_slice() {
            return Ok(Cow::Borrowed(*one));
        }
        let mut v = Vec::new();
        v.try_reserve_exact(usize::try_from(total).map_err(|_| bad("item too large"))?).map_err(|_| bad("out of memory for an item"))?;
        for p in parts {
            v.extend_from_slice(p);
        }
        Ok(Cow::Owned(v))
    }

    fn grid(&self, id: u32) -> Result<Grid> {
        let data = self.item_data(id)?;
        let mut r = Rd::new(&data);
        if r.u8()? != 0 {
            return Err(Error::Unsupported(F, "grid descriptor version"));
        }
        let flags = r.u8()?;
        let rows = usize::from(r.u8()?) + 1;
        let cols = usize::from(r.u8()?) + 1;
        let (width, height) = if flags & 1 != 0 { (r.u32()?, r.u32()?) } else { (u32::from(r.u16()?), u32::from(r.u16()?)) };
        if width == 0 || height == 0 {
            return Err(bad("a grid has no area"));
        }
        if rows * cols > MAX_TILES {
            return Err(bad("too many grid tiles"));
        }
        let tiles: Vec<u32> =
            self.refs.iter().filter(|x| &x.kind == b"dimg" && x.from == id).flat_map(|x| x.to.iter().copied()).take(MAX_TILES + 1).collect();
        if tiles.len() != rows * cols {
            return Err(bad("a grid's tile count doesn't match its rows and columns"));
        }
        Ok(Grid { rows, cols, width, height, tiles })
    }

    /// The coded size of an image item: its `ispe`, or a grid's output size.
    fn coded_size(&self, item: &Item<'a>) -> Result<(u32, u32)> {
        let (w, h) = match &item.kind {
            b"av01" => self.ispe(item).ok_or_else(|| bad("an image has no ispe property"))?,
            b"grid" => {
                let g = self.grid(item.id)?;
                (g.width, g.height)
            }
            _ => return Err(Error::Unsupported(F, "the image isn't AV1")),
        };
        if w == 0 || h == 0 || w > av1::MAX_DIMENSION || h > av1::MAX_DIMENSION {
            return Err(bad("an image has an unusable size"));
        }
        Ok((w, h))
    }

    /// Checks that the data of an image item (and of a grid's tiles) lies inside the file.
    fn validate_data(&self, item: &Item<'a>) -> Result<()> {
        if &item.kind == b"grid" {
            for t in self.grid(item.id)?.tiles {
                self.locate(t)?;
            }
        } else {
            self.locate(item.id)?;
        }
        Ok(())
    }

    fn alpha_of(&self, id: u32) -> Option<u32> {
        self.refs
            .iter()
            .filter(|r| &r.kind == b"auxl" && r.to.contains(&id))
            .map(|r| r.from)
            .find(|&a| self.item(a).is_some_and(|it| self.props_of(it).iter().any(|(p, _)| matches!(p, Prop::AuxC(u) if u.ends_with(b":alpha")))))
    }

    /// Exif (TIFF structure) and XMP linked to the image by `cdsc`; unreadable blocks are skipped.
    fn metadata(&self, id: u32) -> (Option<Vec<u8>>, Option<String>) {
        let (mut exif, mut xmp) = (None, None);
        for r in self.refs.iter().filter(|r| &r.kind == b"cdsc" && r.to.contains(&id)) {
            let Some(it) = self.item(r.from) else { continue };
            let is_exif = &it.kind == b"Exif";
            let is_xmp = &it.kind == b"mime" && it.content_type.starts_with(b"application/rdf+xml");
            if !(is_exif && exif.is_none() || is_xmp && xmp.is_none()) {
                continue;
            }
            let Ok((parts, total)) = self.locate(it.id) else { continue };
            if total > MAX_METADATA_BYTES as u64 || self.used.get().saturating_add(total) > self.budget {
                continue;
            }
            self.used.set(self.used.get() + total);
            let data: Vec<u8> = parts.concat();
            if is_exif {
                exif = exif_tiff(&data);
            } else {
                let text = String::from_utf8_lossy(&data);
                xmp = Some(text.trim_matches('\0').to_string());
            }
        }
        (exif, xmp)
    }
}

/// The TIFF structure inside an `Exif` item (a 4-byte offset to the TIFF header, then the data).
fn exif_tiff(data: &[u8]) -> Option<Vec<u8>> {
    let offset = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
    let tiff = data.get(4usize.checked_add(offset)?..)?;
    let tiff = tiff.strip_prefix(b"Exif\0\0").unwrap_or(tiff);
    (tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*")).then(|| tiff.to_vec())
}

// ---------------------------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------------------------

/// The crop rectangle `(left, top, width, height)` of a clean aperture on a `w` × `h` image.
fn clap_rect(c: &Clap, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    if c.w.1 == 0 || c.h.1 == 0 || c.x.1 == 0 || c.y.1 == 0 {
        return None;
    }
    let cw = (f64::from(c.w.0) / f64::from(c.w.1)).round();
    let ch = (f64::from(c.h.0) / f64::from(c.h.1)).round();
    let ox = f64::from(c.x.0) / f64::from(c.x.1);
    let oy = f64::from(c.y.0) / f64::from(c.y.1);
    let left = ((w as f64 - cw) / 2.0 + ox).round();
    let top = ((h as f64 - ch) / 2.0 + oy).round();
    if !(cw >= 1.0 && ch >= 1.0 && left >= 0.0 && top >= 0.0 && left + cw <= w as f64 && top + ch <= h as f64) {
        return None;
    }
    Some((left as usize, top as usize, cw as usize, ch as usize))
}

/// The size after the transforms.
fn displayed_size(ops: &[Xf], mut w: u32, mut h: u32) -> Result<(u32, u32)> {
    for op in ops {
        match op {
            Xf::Crop(c) => {
                let (_, _, cw, ch) = clap_rect(c, w as usize, h as usize).ok_or_else(|| bad("an unusable clean aperture"))?;
                (w, h) = (cw as u32, ch as u32);
            }
            Xf::Rotate(a) if a % 2 == 1 => std::mem::swap(&mut w, &mut h),
            Xf::Rotate(_) | Xf::Mirror(_) => {}
        }
    }
    Ok((w, h))
}

/// `w * h * ch` samples, or an error when that overflows.
fn sample_count(w: usize, h: usize, ch: usize) -> Result<usize> {
    w.checked_mul(h).and_then(|n| n.checked_mul(ch)).ok_or_else(|| bad("picture too large"))
}

fn alloc_u16(n: usize) -> Result<Vec<u16>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| bad("out of memory for the picture"))?;
    v.resize(n, 0);
    Ok(v)
}

/// Build an `ow` × `oh` image whose pixel `(x, y)` is source pixel `map(x, y)`.
fn remap(src: &[u16], w: usize, h: usize, ch: usize, ow: usize, oh: usize, map: impl Fn(usize, usize) -> (usize, usize)) -> Result<Vec<u16>> {
    let mut out = alloc_u16(sample_count(ow, oh, ch)?)?;
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = map(x, y);
            if sx >= w || sy >= h {
                return Err(bad("a transform maps outside the picture"));
            }
            let s = src.get((sy * w + sx) * ch..(sy * w + sx + 1) * ch).ok_or_else(|| bad("a transform reads outside the picture"))?;
            let at = (y * ow + x) * ch;
            out.get_mut(at..at + ch).ok_or_else(|| bad("a transform writes outside the picture"))?.copy_from_slice(s);
        }
    }
    Ok(out)
}

/// Apply the container transforms to interleaved 16-bit samples.
fn apply_transforms(mut data: Vec<u16>, mut w: usize, mut h: usize, ch: usize, ops: &[Xf]) -> Result<(Vec<u16>, usize, usize)> {
    for op in ops {
        match *op {
            Xf::Crop(c) => {
                let (l, t, cw, chh) = clap_rect(&c, w, h).ok_or_else(|| bad("an unusable clean aperture"))?;
                data = remap(&data, w, h, ch, cw, chh, |x, y| (x + l, y + t))?;
                (w, h) = (cw, chh);
            }
            Xf::Rotate(1) => {
                // anticlockwise quarter turn: output (x, y) comes from source (w - 1 - y, x)
                data = remap(&data, w, h, ch, h, w, |x, y| (w.wrapping_sub(1).wrapping_sub(y), x))?;
                (w, h) = (h, w);
            }
            Xf::Rotate(2) => data = remap(&data, w, h, ch, w, h, |x, y| (w.wrapping_sub(1).wrapping_sub(x), h.wrapping_sub(1).wrapping_sub(y)))?,
            Xf::Rotate(3) => {
                data = remap(&data, w, h, ch, h, w, |x, y| (y, h.wrapping_sub(1).wrapping_sub(x)))?;
                (w, h) = (h, w);
            }
            Xf::Rotate(_) => {}
            Xf::Mirror(0) => data = remap(&data, w, h, ch, w, h, |x, y| (w.wrapping_sub(1).wrapping_sub(x), y))?,
            Xf::Mirror(_) => data = remap(&data, w, h, ch, w, h, |x, y| (x, h.wrapping_sub(1).wrapping_sub(y)))?,
        }
    }
    Ok((data, w, h))
}

// ---------------------------------------------------------------------------------------------
// Pictures
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Want {
    Rgb,
    Alpha,
}

/// The colour matrix and range from the container's `nclx`, when it has one.
#[derive(Clone, Copy)]
struct Nclx {
    matrix: u16,
    full: bool,
}

/// A decoded image, 16-bit full scale, `ch` interleaved channels (3 for RGB, 1 for alpha).
struct Dec {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<u16>,
    depth: u8,
    colour: Option<av1::Colour>,
}

fn av1_err(e: av1::Error) -> Error {
    match e {
        av1::Error::Unsupported(why) => Error::Unsupported(F, why),
        av1::Error::Unavailable => Error::Unsupported(F, "AV1 decoding isn't available in this build of Ember"),
        av1::Error::TooLarge(w, h) => Error::TooLarge(w, h),
        other => Error::Malformed(F, other.to_string()),
    }
}

/// Luma coefficients `(Kr, Kb)` of an H.273 matrix; `None` for the identity (GBR) matrix.
fn coefficients(matrix: u16) -> Result<Option<(f32, f32)>> {
    Ok(match matrix {
        0 => None,
        1 | 2 => Some((0.2126, 0.0722)),
        4 => Some((0.30, 0.11)),
        5 | 6 => Some((0.299, 0.114)),
        7 => Some((0.212, 0.087)),
        9 => Some((0.2627, 0.0593)),
        _ => return Err(Error::Unsupported(F, "matrix coefficients (YCgCo, constant luminance or ICtCp)")),
    })
}

fn to16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

/// One chroma row, upsampled to `out.len()` luma columns by bilinear interpolation with
/// centred chroma samples, as raw sample values.
fn chroma_row(p: &av1::Plane, y: usize, sx: u32, sy: u32, out: &mut [f32]) -> Option<()> {
    let (cw, chh) = (p.width, p.height);
    if cw == 0 || chh == 0 {
        return None;
    }
    let fy = if sy == 0 { y as f32 } else { ((y as f32 + 0.5) * 0.5 - 0.5).max(0.0) };
    let y0 = (fy as usize).min(chh - 1);
    let y1 = (y0 + 1).min(chh - 1);
    let wy = (fy - y0 as f32).clamp(0.0, 1.0);
    let r0 = p.data.get(y0 * cw..(y0 + 1) * cw)?;
    let r1 = p.data.get(y1 * cw..(y1 + 1) * cw)?;
    for (x, o) in out.iter_mut().enumerate() {
        let fx = if sx == 0 { x as f32 } else { ((x as f32 + 0.5) * 0.5 - 0.5).max(0.0) };
        let x0 = (fx as usize).min(cw - 1);
        let x1 = (x0 + 1).min(cw - 1);
        let wx = (fx - x0 as f32).clamp(0.0, 1.0);
        let at = |r: &[u16], i: usize| r.get(i).map(|&v| f32::from(v));
        let a = at(r0, x0)? * (1.0 - wx) + at(r0, x1)? * wx;
        let b = at(r1, x0)? * (1.0 - wx) + at(r1, x1)? * wx;
        *o = a * (1.0 - wy) + b * wy;
    }
    Some(())
}

/// Convert a decoded AV1 picture to 16-bit RGB (or its luma as alpha).
fn convert(pic: &av1::Picture, want: Want, nclx: Option<Nclx>) -> Result<Dec> {
    let (w, h) = (pic.width as usize, pic.height as usize);
    let depth = pic.bit_depth;
    let luma = pic.planes.first().ok_or_else(|| bad("a picture has no luma plane"))?;
    if luma.width != w || luma.height != h || luma.data.len() != w * h {
        return Err(bad("a picture's luma plane has the wrong size"));
    }
    let colour = pic.colour;
    let (matrix, full) = match (want, nclx) {
        (Want::Rgb, Some(n)) => (n.matrix, n.full),
        _ => colour.map(|c| (u16::from(c.matrix), c.full_range)).unwrap_or((2, true)),
    };
    // Normalising: luma to 0..1, chroma to -0.5..0.5.
    let maxv = ((1u32 << depth) - 1) as f32;
    let (y_off, y_scale, c_off, c_scale) = if full {
        (0.0, 1.0 / maxv, (1u32 << (depth - 1)) as f32, 1.0 / maxv)
    } else {
        let s = (1u32 << (depth - 8)) as f32;
        (16.0 * s, 1.0 / (219.0 * s), 128.0 * s, 1.0 / (224.0 * s))
    };
    let ny = |v: f32| (v - y_off) * y_scale;
    let nc = |v: f32| (v - c_off) * c_scale;

    if want == Want::Alpha {
        let mut data = alloc_u16(w * h)?;
        for (o, &v) in data.iter_mut().zip(&luma.data) {
            *o = to16(ny(f32::from(v)));
        }
        return Ok(Dec { w, h, ch: 1, data, depth, colour });
    }

    let mut data = alloc_u16(sample_count(w, h, 3)?)?;
    let Some((sx, sy)) = pic.layout.shifts() else {
        // luma only: grey
        for (o, &v) in data.as_chunks_mut::<3>().0.iter_mut().zip(&luma.data) {
            let g = to16(ny(f32::from(v)));
            o.fill(g);
        }
        return Ok(Dec { w, h, ch: 3, data, depth, colour });
    };
    let (cb, cr) = match (pic.planes.get(1), pic.planes.get(2)) {
        (Some(cb), Some(cr)) => (cb, cr),
        _ => return Err(bad("a picture has no chroma planes")),
    };
    let coeffs = coefficients(matrix)?;
    if coeffs.is_none() && pic.layout != av1::Layout::I444 {
        return Err(Error::Unsupported(F, "identity matrix with subsampled chroma"));
    }
    let mut cbr = vec![0.0f32; w];
    let mut crr = vec![0.0f32; w];
    for (y, out) in data.chunks_exact_mut(w * 3).enumerate() {
        let yrow = luma.data.get(y * w..(y + 1) * w).ok_or_else(|| bad("a luma row is missing"))?;
        chroma_row(cb, y, sx, sy, &mut cbr).ok_or_else(|| bad("a chroma plane is the wrong size"))?;
        chroma_row(cr, y, sx, sy, &mut crr).ok_or_else(|| bad("a chroma plane is the wrong size"))?;
        for (x, px) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let yv = f32::from(yrow.get(x).copied().unwrap_or(0));
            let (cbv, crv) = (cbr.get(x).copied().unwrap_or(0.0), crr.get(x).copied().unwrap_or(0.0));
            let [r, g, b] = match coeffs {
                // GBR: Y is green, Cb blue, Cr red, all scaled as luma
                None => [ny(crv), ny(yv), ny(cbv)],
                Some((kr, kb)) => {
                    let (yn, cbn, crn) = (ny(yv), nc(cbv), nc(crv));
                    let r = yn + 2.0 * (1.0 - kr) * crn;
                    let b = yn + 2.0 * (1.0 - kb) * cbn;
                    let g = (yn - kr * r - kb * b) / (1.0 - kr - kb);
                    [r, g, b]
                }
            };
            px.copy_from_slice(&[to16(r), to16(g), to16(b)]);
        }
    }
    Ok(Dec { w, h, ch: 3, data, depth, colour })
}

/// Copy a tile into the grid canvas at `(col, row)`; the part past the canvas edge is dropped.
/// `false` when the sizes don't fit the buffers.
#[allow(clippy::too_many_arguments)]
fn place_tile(out: &mut [u16], out_w: usize, out_h: usize, ch: usize, tile: &[u16], tw: usize, th: usize, col: usize, row: usize) -> bool {
    let (x0, y0) = (col * tw, row * th);
    if x0 >= out_w || y0 >= out_h {
        return true;
    }
    let cw = tw.min(out_w - x0);
    let rows = th.min(out_h - y0);
    for y in 0..rows {
        let src = tile.get(y * tw * ch..y * tw * ch + cw * ch);
        let at = ((y0 + y) * out_w + x0) * ch;
        let dst = out.get_mut(at..at + cw * ch);
        match (src, dst) {
            (Some(s), Some(d)) => d.copy_from_slice(s),
            _ => return false,
        }
    }
    true
}

impl<'a> Container<'a> {
    fn decode_av01(&self, id: u32, want: Want, nclx: Option<Nclx>) -> Result<Dec> {
        let item = self.item(id).ok_or_else(|| bad("an image item isn't described"))?;
        self.check_props(item)?;
        let data = self.item_data(id)?;
        let pic = av1::decode(&data).map_err(av1_err)?;
        convert(&pic, want, nclx)
    }

    fn decode_item(&self, id: u32, want: Want, nclx: Option<Nclx>, opts: &DecodeOptions) -> Result<Dec> {
        let item = self.item(id).ok_or_else(|| bad("an image item isn't described"))?;
        match &item.kind {
            b"av01" => self.decode_av01(id, want, nclx),
            b"grid" => {
                self.check_props(item)?;
                let g = self.grid(id)?;
                check_size(F, u64::from(g.width), u64::from(g.height), opts)?;
                let (ow, oh) = (g.width as usize, g.height as usize);
                let mut canvas: Option<(Vec<u16>, usize, usize, usize, u8, Option<av1::Colour>)> = None;
                for (n, tile_id) in g.tiles.iter().enumerate() {
                    let tile_item = self.item(*tile_id).ok_or_else(|| bad("a grid tile isn't described"))?;
                    if &tile_item.kind != b"av01" {
                        return Err(Error::Unsupported(F, "a grid of anything but AV1 tiles"));
                    }
                    let t = self.decode_av01(*tile_id, want, nclx)?;
                    if canvas.is_none() {
                        let (tw, th) = (t.w, t.h);
                        let area = (tw as u64).saturating_mul(th as u64).saturating_mul(g.tiles.len() as u64);
                        if g.cols * tw < ow || g.rows * th < oh || area > opts.max_pixels.saturating_mul(2) {
                            return Err(bad("grid tiles don't cover the grid, or are too large"));
                        }
                        canvas = Some((alloc_u16(sample_count(ow, oh, t.ch)?)?, tw, th, t.ch, t.depth, t.colour));
                    }
                    let Some((buf, tw, th, ch, _, _)) = canvas.as_mut() else { return Err(bad("grid canvas missing")) };
                    if (t.w, t.h, t.ch) != (*tw, *th, *ch) {
                        return Err(bad("grid tiles differ in size"));
                    }
                    if !place_tile(buf, ow, oh, *ch, &t.data, *tw, *th, n % g.cols, n / g.cols) {
                        return Err(bad("a grid tile doesn't fit"));
                    }
                }
                let (data, _, _, ch, depth, colour) = canvas.ok_or_else(|| bad("a grid has no tiles"))?;
                Ok(Dec { w: ow, h: oh, ch, data, depth, colour })
            }
            _ => Err(Error::Unsupported(F, "the image isn't AV1")),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// The displayed size (container transforms applied, so orientation 1), from the container
/// alone: no picture is decoded. Refuses a file whose image data runs past its end.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let c = Container::parse(bytes)?;
    let prim = c.primary_item()?;
    c.check_props(prim)?;
    let (w, h) = c.coded_size(prim)?;
    c.validate_data(prim)?;
    let (w, h) = displayed_size(&c.transforms(prim), w, h)?;
    Ok((w, h, 1))
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let c = Container::parse(bytes)?;
    let prim = c.primary_item()?;
    c.check_props(prim)?;
    let (cw, ch) = c.coded_size(prim)?;
    // The declared size is checked before any pixel is decoded.
    check_size(F, u64::from(cw), u64::from(ch), opts)?;
    let ops = c.transforms(prim);
    let (dw, dh) = displayed_size(&ops, cw, ch)?;

    let nclx = c.nclx(prim);
    let color = c.decode_item(prim.id, Want::Rgb, nclx.map(|(_, _, matrix, full)| Nclx { matrix, full }), opts)?;
    if (color.w, color.h) != (cw as usize, ch as usize) {
        return Err(bad("the decoded size differs from the declared size"));
    }
    let depth = color.depth;
    let colour = color.colour;
    let mut data = color.data;
    let mut channels = 3;
    let mut premultiplied = false;
    if let Some(alpha_id) = c.alpha_of(prim.id) {
        let alpha = c.decode_item(alpha_id, Want::Alpha, None, opts)?;
        if (alpha.w, alpha.h) != (color.w, color.h) {
            return Err(bad("the alpha plane's size differs from the picture's"));
        }
        let mut rgba = alloc_u16(sample_count(color.w, color.h, 4)?)?;
        for ((o, c3), a) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(data.as_chunks::<3>().0).zip(&alpha.data) {
            o[..3].copy_from_slice(c3);
            o[3] = *a;
        }
        data = rgba;
        channels = 4;
        premultiplied = c.refs.iter().any(|r| &r.kind == b"prem" && r.from == prim.id && r.to.contains(&alpha_id));
    }
    let (data, w, h) = apply_transforms(data, color.w, color.h, channels, &ops)?;

    let (exif, xmp) = c.metadata(prim.id);
    let icc = c.icc(prim).map(<[u8]>::to_vec);
    let hint = match nclx {
        Some((primaries, transfer, _, _)) => SourceSpace::from_cicp(primaries, transfer),
        None => colour.filter(|k| k.described).and_then(|k| SourceSpace::from_cicp(u16::from(k.primaries), u16::from(k.transfer))),
    };
    let raw = Raw { width: w, height: h, model: Model::Rgb, alpha: channels == 4, premultiplied, buf: Buf::U16(data), bit_depth: depth };
    let meta = Meta { icc, exif, xmp, orientation: Some(1), hint, ..Default::default() };
    finish(F, raw, meta, (dw, dh), opts)
}

#[cfg(test)]
mod tests;
