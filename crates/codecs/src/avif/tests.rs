use super::*;
use crate::{EncodeImage, EncodeMeta, Samples, encode_avif};

// ---------------------------------------------------------------------------------------------
// Hand-built containers
// ---------------------------------------------------------------------------------------------

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

fn fbx(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = vec![version, (flags >> 16) as u8, (flags >> 8) as u8, flags as u8];
    b.extend_from_slice(body);
    bx(kind, &b)
}

fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut b = w.to_be_bytes().to_vec();
    b.extend_from_slice(&h.to_be_bytes());
    fbx(b"ispe", 0, 0, &b)
}

fn irot(n: u8) -> Vec<u8> {
    bx(b"irot", &[n])
}

fn imir(axis: u8) -> Vec<u8> {
    bx(b"imir", &[axis])
}

fn nclx(primaries: u16, transfer: u16, matrix: u16) -> Vec<u8> {
    let mut b = b"nclx".to_vec();
    for v in [primaries, transfer, matrix] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.push(0x80);
    bx(b"colr", &b)
}

fn aux_alpha() -> Vec<u8> {
    fbx(b"auxC", 0, 0, b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0")
}

struct It {
    id: u32,
    kind: [u8; 4],
    data: Vec<u8>,
    /// Stored in `idat` instead of `mdat`.
    idat: bool,
    /// Property boxes and whether each is essential.
    props: Vec<(Vec<u8>, bool)>,
    /// The `infe` content type of a `mime` item (empty: none).
    ctype: Vec<u8>,
}

fn it(id: u32, kind: &[u8; 4], data: &[u8], props: Vec<Vec<u8>>) -> It {
    It { id, kind: *kind, data: data.to_vec(), idat: false, props: props.into_iter().map(|p| (p, false)).collect(), ctype: Vec::new() }
}

fn mime(id: u32, ctype: &str, data: &[u8]) -> It {
    It { ctype: ctype.as_bytes().to_vec(), ..it(id, b"mime", data, vec![]) }
}

struct B {
    primary: u32,
    brand: [u8; 4],
    items: Vec<It>,
    refs: Vec<([u8; 4], u32, Vec<u32>)>,
}

impl B {
    fn new(primary: u32, items: Vec<It>) -> B {
        B { primary, brand: *b"avif", items, refs: Vec::new() }
    }

    fn build(&self) -> Vec<u8> {
        let (mut idat, mut mdat) = (Vec::new(), Vec::new());
        let mut locs = Vec::new();
        for i in &self.items {
            if i.idat {
                locs.push((i.id, 1u16, idat.len(), i.data.len()));
                idat.extend_from_slice(&i.data);
            } else {
                locs.push((i.id, 0u16, mdat.len(), i.data.len()));
                mdat.extend_from_slice(&i.data);
            }
        }
        let mut ftyp_body = self.brand.to_vec();
        ftyp_body.extend_from_slice(&[0; 4]);
        ftyp_body.extend_from_slice(b"avifmif1");
        let ftyp = bx(b"ftyp", &ftyp_body);
        let meta = |mdat_start: usize| -> Vec<u8> {
            let mut hdlr = vec![0u8; 4];
            hdlr.extend_from_slice(b"pict");
            hdlr.extend_from_slice(&[0u8; 13]);
            let mut parts = vec![fbx(b"hdlr", 0, 0, &hdlr), fbx(b"pitm", 0, 0, &(self.primary as u16).to_be_bytes())];
            let mut iinf = (self.items.len() as u16).to_be_bytes().to_vec();
            for i in &self.items {
                let mut b = (i.id as u16).to_be_bytes().to_vec();
                b.extend_from_slice(&[0, 0]);
                b.extend_from_slice(&i.kind);
                b.push(0);
                if !i.ctype.is_empty() {
                    b.extend_from_slice(&i.ctype);
                    b.push(0);
                }
                iinf.extend(fbx(b"infe", 2, 0, &b));
            }
            parts.push(fbx(b"iinf", 0, 0, &iinf));
            let mut iloc = vec![0x44, 0x00];
            iloc.extend_from_slice(&(locs.len() as u16).to_be_bytes());
            for &(id, method, off, len) in &locs {
                iloc.extend_from_slice(&(id as u16).to_be_bytes());
                iloc.extend_from_slice(&method.to_be_bytes());
                iloc.extend_from_slice(&0u16.to_be_bytes());
                iloc.extend_from_slice(&1u16.to_be_bytes());
                let abs = if method == 0 { mdat_start + off } else { off };
                iloc.extend_from_slice(&(abs as u32).to_be_bytes());
                iloc.extend_from_slice(&(len as u32).to_be_bytes());
            }
            parts.push(fbx(b"iloc", 1, 0, &iloc));
            if !self.refs.is_empty() {
                let mut kids = Vec::new();
                for (k, from, to) in &self.refs {
                    let mut b = (*from as u16).to_be_bytes().to_vec();
                    b.extend_from_slice(&(to.len() as u16).to_be_bytes());
                    for t in to {
                        b.extend_from_slice(&(*t as u16).to_be_bytes());
                    }
                    kids.extend(bx(k, &b));
                }
                parts.push(fbx(b"iref", 0, 0, &kids));
            }
            let (mut ipco, mut ipma, mut n, mut entries) = (Vec::new(), Vec::new(), 0u8, 0u32);
            for i in self.items.iter().filter(|i| !i.props.is_empty()) {
                entries += 1;
                ipma.extend_from_slice(&(i.id as u16).to_be_bytes());
                ipma.push(i.props.len() as u8);
                for (p, essential) in &i.props {
                    ipco.extend_from_slice(p);
                    n += 1;
                    ipma.push(n | if *essential { 0x80 } else { 0 });
                }
            }
            let mut ipma_body = entries.to_be_bytes().to_vec();
            ipma_body.extend(ipma);
            let mut iprp = bx(b"ipco", &ipco);
            iprp.extend(fbx(b"ipma", 0, 0, &ipma_body));
            parts.push(bx(b"iprp", &iprp));
            if !idat.is_empty() {
                parts.push(bx(b"idat", &idat));
            }
            fbx(b"meta", 0, 0, &parts.concat())
        };
        let start = ftyp.len() + meta(0).len() + 8;
        let mut file = ftyp;
        file.extend(meta(start));
        file.extend(bx(b"mdat", &mdat));
        file
    }
}

/// One `av01` image of arbitrary bytes (enough for header reading).
fn fake(w: u32, h: u32, props: Vec<Vec<u8>>) -> Vec<u8> {
    let mut p = vec![ispe(w, h)];
    p.extend(props);
    B::new(1, vec![it(1, b"av01", &[1, 2, 3, 4], p)]).build()
}

fn opts() -> DecodeOptions {
    DecodeOptions::default()
}

#[test]
fn the_header_comes_from_ispe() {
    assert_eq!(header(&fake(64, 48, vec![])).ok(), Some((64, 48, 1)));
}

#[test]
fn rotation_swaps_the_displayed_size_and_a_mirror_does_not() {
    assert_eq!(header(&fake(64, 48, vec![irot(1)])).ok(), Some((48, 64, 1)));
    assert_eq!(header(&fake(64, 48, vec![irot(2)])).ok(), Some((64, 48, 1)));
    assert_eq!(header(&fake(64, 48, vec![irot(3), imir(0)])).ok(), Some((48, 64, 1)));
    assert_eq!(header(&fake(64, 48, vec![imir(1)])).ok(), Some((64, 48, 1)));
}

#[test]
fn a_clean_aperture_crops_the_displayed_size() {
    let mut b = Vec::new();
    for v in [32u32, 1, 16, 1, 0, 1, 0, 1] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    assert_eq!(header(&fake(64, 48, vec![bx(b"clap", &b)])).ok(), Some((32, 16, 1)));
    // an aperture larger than the image is malformed
    let mut big = Vec::new();
    for v in [128u32, 1, 16, 1, 0, 1, 0, 1] {
        big.extend_from_slice(&v.to_be_bytes());
    }
    assert!(header(&fake(64, 48, vec![bx(b"clap", &big)])).is_err());
    let rect = Clap { w: (32, 1), h: (16, 1), x: (0, 1), y: (0, 1) };
    assert_eq!(clap_rect(&rect, 64, 48), Some((16, 16, 32, 16)));
    let off = Clap { x: (-4, 1), ..rect };
    assert_eq!(clap_rect(&off, 64, 48), Some((12, 16, 32, 16)));
    assert_eq!(clap_rect(&Clap { w: (32, 0), ..rect }, 64, 48), None);
}

#[test]
fn an_unknown_essential_property_is_refused_but_an_optional_one_is_not() {
    let weird = bx(b"zzzz", &[1, 2, 3]);
    let optional = B::new(1, vec![it(1, b"av01", &[1, 2, 3, 4], vec![ispe(8, 8), weird.clone()])]).build();
    assert_eq!(header(&optional).ok(), Some((8, 8, 1)));
    let mut item = it(1, b"av01", &[1, 2, 3, 4], vec![ispe(8, 8)]);
    item.props.push((weird, true));
    let essential = B::new(1, vec![item]).build();
    assert!(matches!(header(&essential), Err(Error::Unsupported(Format::Avif, _))));
}

#[test]
fn the_wrong_brand_or_missing_boxes_are_errors() {
    let mut heic = fake(8, 8, vec![]);
    heic[8..12].copy_from_slice(b"heic");
    // the compatible brands still say avif; replace those too
    let at = heic.windows(8).position(|w| w == b"avifmif1").unwrap_or(0);
    heic[at..at + 4].copy_from_slice(b"heic");
    assert!(matches!(header(&heic), Err(Error::Unsupported(Format::Avif, _))));
    assert!(header(&bx(b"ftyp", b"avif\0\0\0\0avif")).is_err(), "no meta box");
    assert!(matches!(header(&bx(b"ftyp", b"avis\0\0\0\0avis")), Err(Error::Unsupported(Format::Avif, _))), "a sequence with no still");
    assert!(header(&[]).is_err());
    assert!(header(b"\0\0\0\x0cftypavif").is_err());
}

#[test]
fn bad_box_sizes_are_errors() {
    assert!(children(&[0, 0, 0, 4, b'f', b't', b'y', b'p'], 0).is_err());
    assert!(children(&[0, 0, 0, 1, b'f', b't', b'y', b'p', 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], 0).is_err());
    assert!(children(&[0, 0, 0, 1, b'f', b't', b'y', b'p', 0, 0, 0, 0, 0, 0, 0, 8], 0).is_err());
    assert!(children(&[0, 0, 1, 0, b'f', b't', b'y', b'p', 1, 2], 0).is_err(), "runs past the data");
    assert_eq!(children(&[0, 0, 0, 0, b'm', b'd', b'a', b't', 1, 2, 3], 0).map(|b| b.len()).ok(), Some(1), "size 0 runs to the end");
    assert!(children(&[], MAX_DEPTH + 1).is_err());
}

#[test]
fn huge_counts_are_capped() {
    // iinf claiming 4 billion items
    let iinf = fbx(b"iinf", 1, 0, &u32::MAX.to_be_bytes());
    assert!(parse_iinf(&iinf[8..]).is_err());
    // iloc (version 2) claiming 4 billion items
    let mut iloc = vec![0x44, 0x00];
    iloc.extend_from_slice(&u32::MAX.to_be_bytes());
    assert!(parse_iloc(&fbx(b"iloc", 2, 0, &iloc)[8..]).is_err());
    // an item with too many extents
    let mut iloc = vec![0x44, 0x00, 0, 1, 0, 1, 0, 0, 0xFF, 0xFF];
    iloc.extend_from_slice(&[0; 64]);
    assert!(parse_iloc(&fbx(b"iloc", 0, 0, &iloc)[8..]).is_err());
    // odd field sizes
    let iloc = vec![0x33, 0x00, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(parse_iloc(&fbx(b"iloc", 0, 0, &iloc)[8..]).is_err());
    // an iref claiming more targets than it holds
    let mut iref = (1u16).to_be_bytes().to_vec();
    iref.extend_from_slice(&u16::MAX.to_be_bytes());
    assert!(parse_iref(&fbx(b"iref", 0, 0, &bx(b"dimg", &iref))[8..]).is_err());
}

#[test]
fn item_data_outside_the_file_is_an_error_not_a_panic() {
    let mut f = fake(8, 8, vec![]);
    // cut the last byte of the mdat payload
    f.pop();
    assert!(header(&f).is_err());
    assert!(decode(&f, &opts()).is_err());
    // an iloc offset past the end, found by corrupting the offset field
    let good = fake(8, 8, vec![]);
    for at in 0..good.len().saturating_sub(4) {
        let mut g = good.clone();
        g[at..at + 4].copy_from_slice(&[0xFF; 4]);
        let _ = header(&g);
    }
}

#[test]
fn every_prefix_and_every_flipped_byte_is_handled() {
    let f = fake(40, 24, vec![irot(1), nclx(1, 13, 6)]);
    for n in 0..f.len() {
        let _ = header(&f[..n]);
        if n % 9 == 0 {
            let _ = decode(&f[..n], &opts());
        }
    }
    for at in 0..f.len() {
        let mut g = f.clone();
        g[at] ^= 0xFF;
        let _ = header(&g);
        if at % 11 == 0 {
            let _ = decode(&g, &opts());
        }
    }
}

#[test]
fn a_declared_size_over_the_limit_is_refused_before_decoding() {
    let f = fake(65_536, 65_536, vec![]);
    assert_eq!(header(&f).ok(), Some((65_536, 65_536, 1)));
    assert!(matches!(decode(&f, &opts()), Err(Error::TooLarge(..))));
    assert!(header(&fake(0, 8, vec![])).is_err());
    assert!(header(&fake(65_537, 8, vec![])).is_err());
    let small = DecodeOptions { max_pixels: 100, ..opts() };
    assert!(matches!(decode(&fake(64, 48, vec![]), &small), Err(Error::TooLarge(..))));
}

#[test]
fn garbage_av1_data_is_a_malformed_error() {
    let f = fake(8, 8, vec![]);
    assert!(matches!(decode(&f, &opts()), Err(Error::Malformed(Format::Avif, _))));
}

#[test]
fn an_unsupported_primary_item_is_refused() {
    let f = B::new(1, vec![it(1, b"hvc1", &[1, 2, 3, 4], vec![ispe(8, 8)])]).build();
    assert!(matches!(header(&f), Err(Error::Unsupported(Format::Avif, _))));
    let missing = B::new(7, vec![it(1, b"av01", &[1, 2, 3, 4], vec![ispe(8, 8)])]).build();
    assert!(header(&missing).is_err());
}

#[test]
fn grid_descriptors_are_validated() {
    let tiles = |n: u32| (2..2 + n).map(|i| it(i, b"av01", &[1, 2, 3, 4], vec![ispe(16, 16)])).collect::<Vec<_>>();
    let grid = |desc: &[u8], tile_count: u32| {
        let mut g = it(1, b"grid", desc, vec![ispe(32, 32)]);
        g.idat = true;
        let mut items = vec![g];
        items.extend(tiles(tile_count));
        let mut b = B::new(1, items);
        b.refs.push((*b"dimg", 1, (2..2 + tile_count).collect()));
        b.build()
    };
    // 2 x 2 grid, 32 x 32 output, 4 tiles
    assert_eq!(header(&grid(&[0, 0, 1, 1, 0, 32, 0, 32], 4)).ok(), Some((32, 32, 1)));
    // the descriptor's size wins over ispe
    assert_eq!(header(&grid(&[0, 0, 1, 1, 0, 30, 0, 28], 4)).ok(), Some((30, 28, 1)));
    // 32-bit fields
    assert_eq!(header(&grid(&[0, 1, 1, 1, 0, 0, 0, 20, 0, 0, 0, 24], 4)).ok(), Some((20, 24, 1)));
    // tile count differs from rows * columns
    assert!(header(&grid(&[0, 0, 1, 1, 0, 32, 0, 32], 3)).is_err());
    // zero size, unknown version, truncated descriptor
    assert!(header(&grid(&[0, 0, 1, 1, 0, 0, 0, 32], 4)).is_err());
    assert!(matches!(header(&grid(&[1, 0, 1, 1, 0, 32, 0, 32], 4)), Err(Error::Unsupported(Format::Avif, _))));
    assert!(header(&grid(&[0, 0, 1, 1, 0, 32], 4)).is_err());
    // 256 x 256 tiles is over the cap
    assert!(header(&grid(&[0, 0, 255, 255, 0, 32, 0, 32], 4)).is_err());
}

// ---------------------------------------------------------------------------------------------
// Pixel maths
// ---------------------------------------------------------------------------------------------

#[test]
fn tiles_are_placed_at_their_cell_and_clipped_at_the_canvas_edge() {
    // 2 x 2 grid of 2 x 2 tiles on a 3 x 3 canvas (one channel): the right and bottom tiles lose a column / row
    let mut out = vec![0u16; 9];
    let tile = |v: u16| vec![v; 4];
    assert!(place_tile(&mut out, 3, 3, 1, &tile(1), 2, 2, 0, 0));
    assert!(place_tile(&mut out, 3, 3, 1, &tile(2), 2, 2, 1, 0));
    assert!(place_tile(&mut out, 3, 3, 1, &tile(3), 2, 2, 0, 1));
    assert!(place_tile(&mut out, 3, 3, 1, &tile(4), 2, 2, 1, 1));
    assert_eq!(out, vec![1, 1, 2, 1, 1, 2, 3, 3, 4]);
    // a tile entirely outside the canvas is skipped, a short tile buffer is refused
    assert!(place_tile(&mut out, 3, 3, 1, &tile(9), 2, 2, 2, 0));
    assert!(!place_tile(&mut out, 3, 3, 1, &[1, 2], 2, 2, 0, 0));
    // three channels
    let mut rgb = vec![0u16; 6];
    assert!(place_tile(&mut rgb, 2, 1, 3, &[1, 2, 3], 1, 1, 0, 0));
    assert!(place_tile(&mut rgb, 2, 1, 3, &[4, 5, 6], 1, 1, 1, 0));
    assert_eq!(rgb, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn rotations_and_mirrors_move_pixels_the_way_the_spec_says() {
    // 3 x 2, one channel:  1 2 3 / 4 5 6
    let src = vec![1u16, 2, 3, 4, 5, 6];
    // anticlockwise quarter turn: 3 4 / 2 5... the top-right pixel goes top-left
    let (r1, w, h) = apply_transforms(src.clone(), 3, 2, 1, &[Xf::Rotate(1)]).unwrap();
    assert_eq!((w, h, r1), (2, 3, vec![3, 6, 2, 5, 1, 4]));
    let (r2, w, h) = apply_transforms(src.clone(), 3, 2, 1, &[Xf::Rotate(2)]).unwrap();
    assert_eq!((w, h, r2), (3, 2, vec![6, 5, 4, 3, 2, 1]));
    let (r3, w, h) = apply_transforms(src.clone(), 3, 2, 1, &[Xf::Rotate(3)]).unwrap();
    assert_eq!((w, h, r3), (2, 3, vec![4, 1, 5, 2, 6, 3]));
    // axis 0: left and right swap; axis 1: top and bottom swap
    let (m0, _, _) = apply_transforms(src.clone(), 3, 2, 1, &[Xf::Mirror(0)]).unwrap();
    assert_eq!(m0, vec![3, 2, 1, 6, 5, 4]);
    let (m1, _, _) = apply_transforms(src.clone(), 3, 2, 1, &[Xf::Mirror(1)]).unwrap();
    assert_eq!(m1, vec![4, 5, 6, 1, 2, 3]);
    // four quarter turns are the identity; transforms apply in order
    let (id, w, h) = apply_transforms(src.clone(), 3, 2, 1, &[Xf::Rotate(1); 4]).unwrap();
    assert_eq!((w, h, id), (3, 2, src.clone()));
    let crop = Clap { w: (2, 1), h: (1, 1), x: (0, 1), y: (0, 1) };
    let (c, w, h) = apply_transforms(src, 3, 2, 1, &[Xf::Crop(crop), Xf::Rotate(1)]).unwrap();
    assert_eq!((w, h), (1, 2));
    assert_eq!(c.len(), 2);
}

fn picture(depth: u8, layout: av1::Layout, planes: Vec<(usize, usize, Vec<u16>)>, colour: Option<av1::Colour>) -> av1::Picture {
    let (width, height) = planes.first().map(|p| (p.0 as u32, p.1 as u32)).unwrap_or((1, 1));
    let planes = planes.into_iter().map(|(width, height, data)| av1::Plane { width, height, data }).collect();
    av1::Picture { width, height, bit_depth: depth, layout, planes, colour }
}

fn px(d: &Dec, i: usize) -> [f32; 3] {
    let s = |c: usize| f32::from(d.data[i * 3 + c]) / 65535.0;
    [s(0), s(1), s(2)]
}

#[test]
fn ycbcr_converts_with_the_matrix_range_and_depth() {
    let nclx = |matrix, full| Some(Nclx { matrix, full });
    // BT.601 full range, 8-bit: grey and pure red
    let pic = picture(8, av1::Layout::I444, vec![(2, 1, vec![128, 76]), (2, 1, vec![128, 85]), (2, 1, vec![128, 255])], None);
    let d = convert(&pic, Want::Rgb, nclx(6, true)).unwrap();
    let grey = px(&d, 0);
    assert!(grey.iter().all(|v| (v - 128.0 / 255.0).abs() < 0.01), "{grey:?}");
    let red = px(&d, 1);
    assert!(red[0] > 0.97 && red[1] < 0.03 && red[2] < 0.03, "{red:?}");
    // limited range: black is 16 and white 235
    let pic = picture(8, av1::Layout::I444, vec![(2, 1, vec![16, 235]), (2, 1, vec![128, 128]), (2, 1, vec![128, 128])], None);
    let d = convert(&pic, Want::Rgb, nclx(1, false)).unwrap();
    assert!(px(&d, 0).iter().all(|&v| v < 0.005) && px(&d, 1).iter().all(|&v| v > 0.995));
    // 10-bit is the same picture at four times the code values
    let pic = picture(10, av1::Layout::I444, vec![(2, 1, vec![64, 940]), (2, 1, vec![512, 512]), (2, 1, vec![512, 512])], None);
    let d = convert(&pic, Want::Rgb, nclx(1, false)).unwrap();
    assert!(px(&d, 0).iter().all(|&v| v < 0.005) && px(&d, 1).iter().all(|&v| v > 0.995));
    assert_eq!(d.depth, 10);
    // without an nclx the sequence header's matrix and range apply
    let colour = av1::Colour { primaries: 1, transfer: 13, matrix: 6, full_range: true, described: true };
    let pic = picture(8, av1::Layout::I444, vec![(1, 1, vec![76]), (1, 1, vec![85]), (1, 1, vec![255])], Some(colour));
    let red = px(&convert(&pic, Want::Rgb, None).unwrap(), 0);
    assert!(red[0] > 0.97 && red[1] < 0.03, "{red:?}");
    // luma only reads as grey; luma as alpha
    let pic = picture(8, av1::Layout::I400, vec![(2, 1, vec![0, 255])], None);
    let d = convert(&pic, Want::Rgb, nclx(6, true)).unwrap();
    assert_eq!((px(&d, 0), px(&d, 1)), ([0.0; 3], [1.0; 3]));
    let a = convert(&pic, Want::Alpha, None).unwrap();
    assert_eq!((a.ch, a.data), (1, vec![0, 65535]));
    // unsupported matrices are refused, not guessed
    let pic = picture(8, av1::Layout::I444, vec![(1, 1, vec![1]), (1, 1, vec![1]), (1, 1, vec![1])], None);
    assert!(matches!(convert(&pic, Want::Rgb, nclx(10, true)), Err(Error::Unsupported(Format::Avif, _))));
    // identity (GBR) needs full-resolution chroma
    let sub = picture(8, av1::Layout::I420, vec![(2, 2, vec![1; 4]), (1, 1, vec![1]), (1, 1, vec![1])], None);
    assert!(matches!(convert(&sub, Want::Rgb, nclx(0, true)), Err(Error::Unsupported(Format::Avif, _))));
}

#[test]
fn chroma_is_upsampled_smoothly_and_odd_sizes_work() {
    // 3 x 3 luma, 2 x 2 chroma (4:2:0), flat luma: Cb ramps left to right
    let pic = picture(8, av1::Layout::I420, vec![(3, 3, vec![128; 9]), (2, 2, vec![100, 160, 100, 160]), (2, 2, vec![128; 4])], None);
    let d = convert(&pic, Want::Rgb, Some(Nclx { matrix: 6, full: true })).unwrap();
    assert_eq!((d.w, d.h, d.data.len()), (3, 3, 27));
    // the blue channel follows Cb: it rises along the row and the middle column lies between the two samples
    let b: Vec<f32> = (0..3).map(|x| px(&d, x)[2]).collect();
    assert!(b[0] < b[1] && b[1] < b[2], "{b:?}");
    // a chroma plane of the wrong size is an error
    let bad = picture(8, av1::Layout::I420, vec![(3, 3, vec![128; 9]), (0, 0, vec![]), (2, 2, vec![128; 4])], None);
    assert!(convert(&bad, Want::Rgb, None).is_err());
    let short = picture(8, av1::Layout::I444, vec![(3, 3, vec![128; 9]), (3, 3, vec![128; 4]), (3, 3, vec![128; 9])], None);
    assert!(convert(&short, Want::Rgb, None).is_err());
}

// ---------------------------------------------------------------------------------------------
// Real AV1 data, made by the encoder Ember exports with
// ---------------------------------------------------------------------------------------------

type Quad = [[u8; 3]; 4];
const RED: [u8; 3] = [220, 30, 30];
const GREEN: [u8; 3] = [30, 200, 40];
const BLUE: [u8; 3] = [30, 40, 220];
const WHITE: [u8; 3] = [235, 235, 235];
/// top-left, top-right, bottom-left, bottom-right
const QUAD: Quad = [RED, GREEN, BLUE, WHITE];

fn quadrants(w: usize, h: usize, q: Quad) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let c = q[usize::from(y >= h / 2) * 2 + usize::from(x >= w / 2)];
            v.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    v
}

/// The AV1 item data of a small AVIF the encoder wrote.
fn av1_payload(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let img = EncodeImage::new(w, h, 4, Samples::U8(rgba));
    let file = encode_avif(&img, 95, 10, &EncodeMeta::default()).unwrap();
    let c = Container::parse(&file).unwrap();
    c.item_data(c.primary).unwrap().into_owned()
}

fn solid(w: u32, h: u32, c: [u8; 3]) -> Vec<u8> {
    av1_payload(&quadrants(w as usize, h as usize, [c; 4]), w, h)
}

fn near(a: [u8; 4], b: [u8; 3]) -> bool {
    (0..3).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= 40)
}

/// The sRGB colour at the centre of each quadrant of a decoded image.
fn quadrant_colours(d: &Decoded) -> [[u8; 4]; 4] {
    let s = d.to_srgb8();
    let at = |fx: usize, fy: usize| s.data[(s.height * fy / 4) * s.width + s.width * fx / 4];
    [at(1, 1), at(3, 1), at(1, 3), at(3, 3)]
}

fn decode_quadrants(props: Vec<Vec<u8>>) -> Decoded {
    // what the encoder writes into the container: sRGB primaries and transfer, BT.601 full range
    let mut p = vec![ispe(32, 32), nclx(1, 13, 6)];
    p.extend(props);
    let f = B::new(1, vec![it(1, b"av01", &av1_payload(&quadrants(32, 32, QUAD), 32, 32), p)]).build();
    decode(&f, &opts()).unwrap()
}

#[test]
fn a_real_still_decodes_to_the_colours_it_was_made_from() {
    let d = decode_quadrants(vec![]);
    assert_eq!((d.width, d.height, d.orientation, d.has_alpha), (32, 32, 1, false));
    let c = quadrant_colours(&d);
    for (got, want) in c.iter().zip(QUAD) {
        assert!(near(*got, want), "{got:?} vs {want:?}");
    }
}

#[test]
fn container_rotation_and_mirroring_turn_the_picture() {
    let expect = |d: &Decoded, want: [[u8; 3]; 4]| {
        let c = quadrant_colours(d);
        for (got, want) in c.iter().zip(want) {
            assert!(near(*got, want), "{c:?} vs {want:?}");
        }
    };
    // anticlockwise quarter turn: top-left goes to bottom-left
    expect(&decode_quadrants(vec![irot(1)]), [GREEN, WHITE, RED, BLUE]);
    expect(&decode_quadrants(vec![irot(2)]), [WHITE, BLUE, GREEN, RED]);
    expect(&decode_quadrants(vec![irot(3)]), [BLUE, RED, WHITE, GREEN]);
    expect(&decode_quadrants(vec![imir(0)]), [GREEN, RED, WHITE, BLUE]);
    expect(&decode_quadrants(vec![imir(1)]), [BLUE, WHITE, RED, GREEN]);
    // a non-square picture swaps its size and agrees with the header
    let f = B::new(1, vec![it(1, b"av01", &av1_payload(&quadrants(48, 16, QUAD), 48, 16), vec![ispe(48, 16), irot(1)])]).build();
    let d = decode(&f, &opts()).unwrap();
    assert_eq!((d.width, d.height), (16, 48));
    assert_eq!(header(&f).ok(), Some((16, 48, 1)));
    assert_eq!((d.source_width, d.source_height), (16, 48));
}

#[test]
fn a_grid_of_tiles_is_assembled_and_clipped() {
    let tiles = [RED, GREEN, BLUE, WHITE];
    let build = |w: u16, h: u16| {
        let mut desc = vec![0, 0, 1, 1];
        desc.extend_from_slice(&w.to_be_bytes());
        desc.extend_from_slice(&h.to_be_bytes());
        let mut grid = it(1, b"grid", &desc, vec![ispe(u32::from(w), u32::from(h))]);
        grid.idat = true;
        let mut items = vec![grid];
        for (n, c) in tiles.iter().enumerate() {
            items.push(it(2 + n as u32, b"av01", &solid(16, 16, *c), vec![ispe(16, 16)]));
        }
        let mut b = B::new(1, items);
        b.refs.push((*b"dimg", 1, vec![2, 3, 4, 5]));
        b.build()
    };
    for (w, h) in [(32u16, 32u16), (30, 27)] {
        let d = decode(&build(w, h), &opts()).unwrap();
        assert_eq!((d.width, d.height), (u32::from(w), u32::from(h)));
        let s = d.to_srgb8();
        let at = |x: usize, y: usize| s.data[y * s.width + x];
        assert!(near(at(5, 5), RED), "{:?}", at(5, 5));
        assert!(near(at(24, 5), GREEN), "{:?}", at(24, 5));
        assert!(near(at(5, 22), BLUE), "{:?}", at(5, 22));
        assert!(near(at(24, 22), WHITE), "{:?}", at(24, 22));
    }
    // tiles that don't cover the output are refused
    assert!(decode(&build(40, 32), &opts()).is_err());
}

#[test]
fn an_alpha_item_becomes_the_alpha_plane() {
    let colour = av1_payload(&quadrants(16, 16, [RED; 4]), 16, 16);
    let alpha = solid(16, 16, [128, 128, 128]);
    let mut b = B::new(1, vec![it(1, b"av01", &colour, vec![ispe(16, 16)]), it(2, b"av01", &alpha, vec![ispe(16, 16), aux_alpha()])]);
    b.refs.push((*b"auxl", 2, vec![1]));
    let f = b.build();
    let d = decode(&f, &opts()).unwrap();
    assert!(d.has_alpha);
    let a = d.alpha.as_ref().unwrap();
    let mean = a.data.iter().sum::<f32>() / a.data.len() as f32;
    assert!((mean - 128.0 / 255.0).abs() < 0.06, "mean alpha {mean}");
    assert_eq!(header(&f).ok(), Some((16, 16, 1)));
    // an alpha plane of another size is malformed
    let mut b = B::new(
        1,
        vec![it(1, b"av01", &colour, vec![ispe(16, 16)]), it(2, b"av01", &solid(32, 32, [128; 3]), vec![ispe(32, 32), aux_alpha()])],
    );
    b.refs.push((*b"auxl", 2, vec![1]));
    assert!(decode(&b.build(), &opts()).is_err());
    // without the alpha URN the auxiliary item is not alpha
    let mut b = B::new(1, vec![it(1, b"av01", &colour, vec![ispe(16, 16)]), it(2, b"av01", &alpha, vec![ispe(16, 16)])]);
    b.refs.push((*b"auxl", 2, vec![1]));
    assert!(!decode(&b.build(), &opts()).unwrap().has_alpha);
}

#[test]
fn colour_description_and_metadata_are_carried() {
    let payload = av1_payload(&quadrants(16, 16, [RED; 4]), 16, 16);
    // nclx primaries / transfer name the source space
    let f = B::new(1, vec![it(1, b"av01", &payload, vec![ispe(16, 16), nclx(12, 13, 6)])]).build();
    let d = decode(&f, &opts()).unwrap();
    assert_eq!(d.space.named, Some(crate::NamedSpace::DisplayP3));
    // an unsupported matrix is an error
    let f = B::new(1, vec![it(1, b"av01", &payload, vec![ispe(16, 16), nclx(1, 13, 10)])]).build();
    assert!(matches!(decode(&f, &opts()), Err(Error::Unsupported(Format::Avif, _))));
    // Exif (offset 0 to the TIFF header) and XMP, linked to the image by cdsc
    let mut exif = vec![0, 0, 0, 0];
    exif.extend_from_slice(b"II*          ");
    let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
    let mut b = B::new(1, vec![it(1, b"av01", &payload, vec![ispe(16, 16)]), it(2, b"Exif", &exif, vec![]), mime(3, "application/rdf+xml", xmp)]);
    b.refs.push((*b"cdsc", 2, vec![1]));
    b.refs.push((*b"cdsc", 3, vec![1]));
    let d = decode(&b.build(), &opts()).unwrap();
    assert!(d.exif.as_deref().is_some_and(|e| e.starts_with(b"II* ")));
    assert!(d.xmp.as_deref().is_some_and(|x| x.contains("xmpmeta")));
    // a mime item of another type is not XMP
    let mut b = B::new(1, vec![it(1, b"av01", &payload, vec![ispe(16, 16)]), mime(3, "text/plain", xmp)]);
    b.refs.push((*b"cdsc", 3, vec![1]));
    assert!(decode(&b.build(), &opts()).unwrap().xmp.is_none());
    assert!(exif_tiff(&[0, 0, 0, 2, b'x', b'x', b'I', b'I', b'*', 0]).is_some());
    assert!(exif_tiff(&[0, 0, 0, 9, 1, 2]).is_none());
    assert!(exif_tiff(&[0, 0]).is_none());
    assert!(exif_tiff(&[0xFF, 0xFF, 0xFF, 0xFF, 1, 2, 3]).is_none());
}

#[test]
fn a_mismatch_between_ispe_and_the_picture_is_malformed() {
    let payload = av1_payload(&quadrants(16, 16, [RED; 4]), 16, 16);
    let f = B::new(1, vec![it(1, b"av01", &payload, vec![ispe(20, 20)])]).build();
    assert!(matches!(decode(&f, &opts()), Err(Error::Malformed(Format::Avif, _))));
}

#[test]
fn item_data_is_bounded_by_the_byte_budget() {
    let f = fake(8, 8, vec![]);
    let c = Container::parse(&f).unwrap();
    assert!(c.item_data(1).is_ok());
    c.used.set(c.budget);
    assert!(c.item_data(1).is_err(), "items can't be handed out more often than the file is large");
}

#[test]
fn without_an_nclx_the_sequence_header_is_used() {
    let f = B::new(1, vec![it(1, b"av01", &av1_payload(&quadrants(16, 16, [RED; 4]), 16, 16), vec![ispe(16, 16)])]).build();
    let d = decode(&f, &opts()).unwrap();
    assert_eq!((d.width, d.height), (16, 16));
}
