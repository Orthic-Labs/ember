//! The pipe protocol between the host (`host`) and the decoder worker (`ember-av1-worker`).
//!
//! Everything is little-endian and versioned. The host sends one request on the worker's stdin; the
//! worker answers with one picture or one error frame on stdout and exits (0 = picture, nonzero =
//! error or failure). The reader treats everything it gets as hostile: every dimension is checked
//! against [`MAX_DIMENSION`] / [`MAX_PIXELS`] and the expected plane geometry before a byte of
//! sample data is allocated, samples are range-checked, and a short stream is an error.
//!
//! ```text
//! request : "EAV1" ver:u8 options:u32 (0) len:u64 data[len]
//! picture : "EAVP" ver:u8 width:u32 height:u32 bit_depth:u8 layout:u8 has_colour:u8
//!           primaries:u8 transfer:u8 matrix:u8 full_range:u8 described:u8 planes:u8
//!           { width:u32 height:u32 samples:u16[width*height] } * planes
//! error   : "EAVE" ver:u8 tag:u8 a:u64 b:u64 code:i32 what:u8
//! ```

use crate::{Colour, Error, Layout, MAX_DIMENSION, MAX_PIXELS, Picture, Plane};
use std::io::{self, Read, Write};

/// Protocol version; a mismatch is an error on both sides.
pub const VERSION: u8 = 1;
/// Largest input accepted by the worker (1 GiB).
pub const MAX_INPUT: u64 = 1 << 30;

const REQUEST_MAGIC: [u8; 4] = *b"EAV1";
const PICTURE_MAGIC: [u8; 4] = *b"EAVP";
const ERROR_MAGIC: [u8; 4] = *b"EAVE";
/// Samples converted per read while receiving a plane (bounds the scratch buffer).
const CHUNK_SAMPLES: usize = 32 * 1024;

/// What the worker sent back.
#[derive(Debug)]
pub enum Response {
    /// A decoded picture (still to be trusted only with a zero exit status).
    Picture(Picture),
    /// The decoder refused the stream; the worker exited cleanly with this error.
    Failure(Error),
}

fn short(_: io::Error) -> Error {
    Error::Malformed("AV1 helper output ended early")
}

fn layout_code(l: Layout) -> u8 {
    match l {
        Layout::I400 => 0,
        Layout::I420 => 1,
        Layout::I422 => 2,
        Layout::I444 => 3,
    }
}

fn layout_from(code: u8) -> Option<Layout> {
    Some(match code {
        0 => Layout::I400,
        1 => Layout::I420,
        2 => Layout::I422,
        3 => Layout::I444,
        _ => return None,
    })
}

/// Names the worker can report for [`Error::Unsupported`] (the error carries a `&'static str`).
const UNSUPPORTED: [&str; 3] = ["bit depth", "chroma layout", "AV1 feature"];

fn unsupported_index(why: &str) -> u8 {
    UNSUPPORTED.iter().position(|s| *s == why).map_or(2, |i| i as u8)
}

fn u8_of(r: &mut impl Read) -> Result<u8, Error> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).map_err(short)?;
    Ok(u8::from_le_bytes(b))
}

fn u32_of(r: &mut impl Read) -> Result<u32, Error> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(short)?;
    Ok(u32::from_le_bytes(b))
}

fn u64_of(r: &mut impl Read) -> Result<u64, Error> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(short)?;
    Ok(u64::from_le_bytes(b))
}

fn i32_of(r: &mut impl Read) -> Result<i32, Error> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(short)?;
    Ok(i32::from_le_bytes(b))
}

/// Host side: send one decode request.
pub fn write_request(w: &mut impl Write, data: &[u8]) -> io::Result<()> {
    w.write_all(&REQUEST_MAGIC)?;
    w.write_all(&[VERSION])?;
    w.write_all(&0u32.to_le_bytes())?;
    w.write_all(&(data.len() as u64).to_le_bytes())?;
    w.write_all(data)?;
    w.flush()
}

/// Worker side: read one decode request, bounded by [`MAX_INPUT`].
pub fn read_request(r: &mut impl Read) -> Result<Vec<u8>, Error> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(|_| Error::Malformed("AV1 request ended early"))?;
    if magic != REQUEST_MAGIC {
        return Err(Error::Malformed("not an AV1 helper request"));
    }
    if u8_of(r)? != VERSION {
        return Err(Error::Malformed("AV1 helper protocol version mismatch"));
    }
    let _options = u32_of(r)?;
    let len = u64_of(r)?;
    if len > MAX_INPUT {
        return Err(Error::Malformed("AV1 request too large"));
    }
    let len = usize::try_from(len).map_err(|_| Error::Malformed("AV1 request too large"))?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| Error::Malformed("out of memory for the AV1 request"))?;
    r.take(len as u64).read_to_end(&mut data).map_err(|_| Error::Malformed("AV1 request ended early"))?;
    if data.len() != len {
        return Err(Error::Malformed("AV1 request ended early"));
    }
    Ok(data)
}

/// Worker side: send a picture.
pub fn write_picture(w: &mut impl Write, pic: &Picture) -> io::Result<()> {
    let bad = |m: &'static str| io::Error::new(io::ErrorKind::InvalidData, m);
    w.write_all(&PICTURE_MAGIC)?;
    w.write_all(&[VERSION])?;
    w.write_all(&pic.width.to_le_bytes())?;
    w.write_all(&pic.height.to_le_bytes())?;
    let c = pic.colour.unwrap_or(Colour { primaries: 2, transfer: 2, matrix: 2, full_range: false, described: false });
    let nplanes = u8::try_from(pic.planes.len()).map_err(|_| bad("too many planes"))?;
    w.write_all(&[
        pic.bit_depth,
        layout_code(pic.layout),
        u8::from(pic.colour.is_some()),
        c.primaries,
        c.transfer,
        c.matrix,
        u8::from(c.full_range),
        u8::from(c.described),
        nplanes,
    ])?;
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK_SAMPLES * 2);
    for p in &pic.planes {
        w.write_all(&u32::try_from(p.width).map_err(|_| bad("plane too wide"))?.to_le_bytes())?;
        w.write_all(&u32::try_from(p.height).map_err(|_| bad("plane too tall"))?.to_le_bytes())?;
        for chunk in p.data.chunks(CHUNK_SAMPLES) {
            buf.clear();
            buf.extend(chunk.iter().flat_map(|v| v.to_le_bytes()));
            w.write_all(&buf)?;
        }
    }
    w.flush()
}

/// Worker side: send a decoder error.
pub fn write_error(w: &mut impl Write, e: &Error) -> io::Result<()> {
    let (tag, a, b, code, what) = match e {
        Error::Unavailable => (0u8, 0u64, 0u64, 0i32, 0u8),
        Error::Empty => (1, 0, 0, 0, 0),
        Error::Malformed(_) => (2, 0, 0, 0, 0),
        Error::Unsupported(why) => (3, 0, 0, 0, unsupported_index(why)),
        Error::TooLarge(x, y) => (4, *x, *y, 0, 0),
        Error::Decoder(c) => (5, 0, 0, *c, 0),
        Error::HelperMissing => (6, 0, 0, 0, 0),
    };
    w.write_all(&ERROR_MAGIC)?;
    w.write_all(&[VERSION, tag])?;
    w.write_all(&a.to_le_bytes())?;
    w.write_all(&b.to_le_bytes())?;
    w.write_all(&code.to_le_bytes())?;
    w.write_all(&[what])?;
    w.flush()
}

/// Host side: read the worker's answer. An outer `Err` means the output itself was malformed.
pub fn read_response(r: &mut impl Read) -> Result<Response, Error> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(|_| Error::Malformed("AV1 helper gave no output"))?;
    if u8_of(r)? != VERSION {
        return Err(Error::Malformed("AV1 helper protocol version mismatch"));
    }
    if magic == ERROR_MAGIC {
        return read_failure(r).map(Response::Failure);
    }
    if magic != PICTURE_MAGIC {
        return Err(Error::Malformed("AV1 helper output not recognised"));
    }
    read_picture(r).map(Response::Picture)
}

fn read_failure(r: &mut impl Read) -> Result<Error, Error> {
    let tag = u8_of(r)?;
    let a = u64_of(r)?;
    let b = u64_of(r)?;
    let code = i32_of(r)?;
    let what = u8_of(r)?;
    Ok(match tag {
        0 => Error::Unavailable,
        1 => Error::Empty,
        2 => Error::Malformed("the AV1 stream is malformed"),
        3 => Error::Unsupported(UNSUPPORTED.get(usize::from(what)).copied().unwrap_or("AV1 feature")),
        4 => Error::TooLarge(a, b),
        5 => Error::Decoder(code),
        6 => Error::HelperMissing,
        _ => return Err(Error::Malformed("AV1 helper error not recognised")),
    })
}

fn read_picture(r: &mut impl Read) -> Result<Picture, Error> {
    let width = u32_of(r)?;
    let height = u32_of(r)?;
    if width == 0 || height == 0 {
        return Err(Error::Malformed("empty picture from the AV1 helper"));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::TooLarge(u64::from(width), u64::from(height)));
    }
    let bit_depth = u8_of(r)?;
    if !matches!(bit_depth, 8 | 10 | 12) {
        return Err(Error::Malformed("AV1 helper bit depth"));
    }
    let layout = layout_from(u8_of(r)?).ok_or(Error::Malformed("AV1 helper chroma layout"))?;
    let has_colour = u8_of(r)?;
    let primaries = u8_of(r)?;
    let transfer = u8_of(r)?;
    let matrix = u8_of(r)?;
    let full_range = u8_of(r)?;
    let described = u8_of(r)?;
    if has_colour > 1 || full_range > 1 || described > 1 {
        return Err(Error::Malformed("AV1 helper colour flags"));
    }
    let colour = (has_colour == 1).then_some(Colour { primaries, transfer, matrix, full_range: full_range == 1, described: described == 1 });
    let nplanes = usize::from(u8_of(r)?);
    let shifts = layout.shifts();
    if nplanes != if shifts.is_some() { 3 } else { 1 } {
        return Err(Error::Malformed("AV1 helper plane count"));
    }
    let limit = 1u32 << bit_depth;
    let mut planes = Vec::with_capacity(nplanes);
    for i in 0..nplanes {
        let (sx, sy) = if i == 0 { (0, 0) } else { shifts.unwrap_or((0, 0)) };
        let want_w = (width as usize + sx as usize) >> sx;
        let want_h = (height as usize + sy as usize) >> sy;
        let (w, h) = (u32_of(r)? as usize, u32_of(r)? as usize);
        if (w, h) != (want_w, want_h) {
            return Err(Error::Malformed("AV1 helper plane size"));
        }
        let count = w.checked_mul(h).ok_or(Error::Malformed("AV1 helper plane size"))?;
        let mut data: Vec<u16> = Vec::new();
        data.try_reserve_exact(count).map_err(|_| Error::Malformed("out of memory for a picture plane"))?;
        let mut buf = vec![0u8; CHUNK_SAMPLES * 2];
        let mut left = count;
        while left > 0 {
            let n = left.min(CHUNK_SAMPLES);
            let bytes = buf.get_mut(..n * 2).ok_or(Error::Malformed("AV1 helper chunk"))?;
            r.read_exact(bytes).map_err(short)?;
            for c in bytes.as_chunks::<2>().0 {
                let v = u16::from_le_bytes(*c);
                if u32::from(v) >= limit {
                    return Err(Error::Malformed("AV1 helper sample out of range"));
                }
                data.push(v);
            }
            left -= n;
        }
        planes.push(Plane { width: w, height: h, data });
    }
    Ok(Picture { width, height, bit_depth, layout, planes, colour })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Picture {
        let plane = |w: usize, h: usize, base: u16| Plane { width: w, height: h, data: (0..w * h).map(|i| (base + i as u16) % 1024).collect() };
        Picture {
            width: 5,
            height: 3,
            bit_depth: 10,
            layout: Layout::I420,
            planes: vec![plane(5, 3, 1), plane(3, 2, 100), plane(3, 2, 200)],
            colour: Some(Colour { primaries: 1, transfer: 13, matrix: 6, full_range: true, described: true }),
        }
    }

    fn sent(p: &Picture) -> Vec<u8> {
        let mut v = Vec::new();
        write_picture(&mut v, p).expect("write");
        v
    }

    #[test]
    fn a_picture_round_trips() {
        let p = picture();
        let Response::Picture(q) = read_response(&mut sent(&p).as_slice()).expect("read") else { panic!("not a picture") };
        assert_eq!((q.width, q.height, q.bit_depth, q.layout, q.colour), (p.width, p.height, p.bit_depth, p.layout, p.colour));
        assert_eq!(q.planes.len(), 3);
        for (a, b) in p.planes.iter().zip(&q.planes) {
            assert_eq!((a.width, a.height, &a.data), (b.width, b.height, &b.data));
        }
    }

    #[test]
    fn errors_round_trip() {
        for e in [Error::Empty, Error::TooLarge(9, 8), Error::Decoder(-22), Error::Unsupported("chroma layout"), Error::HelperMissing] {
            let mut v = Vec::new();
            write_error(&mut v, &e).expect("write");
            let Response::Failure(back) = read_response(&mut v.as_slice()).expect("read") else { panic!("not a failure") };
            assert_eq!(back.to_string(), e.to_string());
        }
    }

    #[test]
    fn a_request_round_trips_and_is_bounded() {
        let mut v = Vec::new();
        write_request(&mut v, &[1, 2, 3]).expect("write");
        assert_eq!(read_request(&mut v.as_slice()).expect("read"), vec![1, 2, 3]);
        let mut huge = Vec::new();
        huge.extend_from_slice(b"EAV1");
        huge.push(VERSION);
        huge.extend_from_slice(&0u32.to_le_bytes());
        huge.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(read_request(&mut huge.as_slice()).is_err());
        // a declared length longer than what follows
        v.truncate(v.len() - 1);
        assert!(read_request(&mut v.as_slice()).is_err());
    }

    #[test]
    fn hostile_output_is_an_error_not_a_panic() {
        let good = sent(&picture());
        // every truncation
        for n in 0..good.len() {
            assert!(read_response(&mut &good[..n]).is_err(), "{n}");
        }
        // a flipped version, magic, dimension, depth, layout, sample
        for (at, val) in [(4usize, 9u8), (0, b'X'), (8, 0xFF), (13, 7), (14, 9), (good.len() - 1, 0xFF)] {
            let mut b = good.clone();
            if let Some(x) = b.get_mut(at) {
                *x = val;
            }
            assert!(read_response(&mut b.as_slice()).is_err(), "{at}");
        }
        // an enormous declared size must not allocate
        let mut b = good.clone();
        for x in b.iter_mut().skip(5).take(8) {
            *x = 0xFF;
        }
        assert!(matches!(read_response(&mut b.as_slice()), Err(Error::TooLarge(..))));
        assert!(read_response(&mut [].as_slice()).is_err());
    }
}
