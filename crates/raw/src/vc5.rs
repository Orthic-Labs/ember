//! GoPro GPR: a DNG whose raw data is one VC-5 stream (SMPTE ST 2073, TIFF compression 9), decoded in safe Rust.
//!
//! The stream is a 32-bit-aligned sequence of tag/value segments. The header names the image (`ImageWidth`,
//! `ImageHeight`), four channels (the Bayer components GS, RG, BG, GD) and the Part 3 Bayer format; then each channel
//! carries ten subbands (the lowpass band and three highpass bands of the coarsest wavelet, then three highpass bands each
//! of the next two), every one in a codeblock chunk. The lowpass band is raw `LowpassPrecision`-bit words; a highpass band
//! is run-length coded with the fixed codebook 17 (runs of zeros and companded magnitudes with a sign bit) and ends in a
//! marker. Decoding inverts three levels of the 2/6 spatial wavelet (dequantising each highpass band, undoing the cubic
//! companding), which leaves four component planes at half the image size in each direction; the Bayer inverse
//! component transform and the inverse ProTune log curve turn them into the CFA mosaic.
//!
//! Source: ported from GoPro's reference implementation of GPR, <https://github.com/gopro/gpr> (Copyright 2018 GoPro Inc.,
//! licensed under the Apache License 2.0 or the MIT licence, at the user's option; both texts are in that repository as
//! `LICENSE-APACHE` and `LICENSE-MIT`). The routines follow `source/lib/vc5_decoder` (`decoder.c`, `component.c`, `inverse.c`,
//! `wavelet.c`, `dequantize.c`, `vlc.c`, `raw.c`) and `source/lib/vc5_common` (`wavelet.c`, `codec.c`, `companding.c`,
//! `logcurve.c`); [`TABLE`] is codebook 17 from `table17.inc`. Modified by the Ember contributors (2026): translated from C to
//! safe Rust, with every size, offset, run length and index from the stream checked, one codeblock decoded from its own
//! payload slice, and the decoded image returned as the mosaic in the DNG's own CFA order. See NOTICE.
//!
//! Hostile input: the dimensions come from 16-bit header values bounded by [`MAX_PIXELS`], each band is sized by them and
//! refused when its payload cannot hold it, runs are checked against the band, and every read returns an error at the end
//! of its payload rather than past it; no input panics.

use crate::{RawError, Result};

const START_MARKER: &[u8; 4] = b"VC-5";
/// The four Bayer components GS (green sum), RG, BG (red and blue minus green) and GD (green difference).
const CHANNELS: usize = 4;
/// Wavelet levels per channel.
const LEVELS: usize = 3;
const SUBBANDS: usize = 10;
/// Wavelet level of each subband, in the order the stream carries them: the coarsest level first.
const SUBBAND_LEVEL: [usize; SUBBANDS] = [2, 2, 2, 2, 1, 1, 1, 0, 0, 0];
/// Band of its wavelet (0 lowpass, 1 lowpass-vertical/highpass-horizontal, 2 the reverse, 3 highpass both) of each subband.
const SUBBAND_BAND: [usize; SUBBANDS] = [0, 1, 2, 3, 1, 2, 3, 1, 2, 3];
/// Most pixels (width times height) a stream may declare: the largest GoPro raw is about 23 megapixels.
const MAX_PIXELS: usize = 1 << 28;
/// `ImageFormat` of a VC-5 Part 3 Bayer raw.
const IMAGE_FORMAT_RAW: i16 = 4;
/// Marks a codebook trie entry as a leaf holding an index into [`TABLE`].
const LEAF: u16 = 0x8000;
/// Midpoint of the colour-difference components RG, BG and GD.
const MIDPOINT: i32 = 2048;

/// Header and band tags (VC-5 `codec.h`); the sign bit of a tag only marks it optional.
mod tag {
    pub const CHANNEL_COUNT: u32 = 12;
    pub const SUBBAND_COUNT: u32 = 14;
    pub const IMAGE_WIDTH: u32 = 20;
    pub const IMAGE_HEIGHT: u32 = 21;
    pub const LOWPASS_PRECISION: u32 = 35;
    pub const SUBBAND_NUMBER: u32 = 48;
    pub const QUANTIZATION: u32 = 53;
    pub const CHANNEL_NUMBER: u32 = 62;
    pub const IMAGE_FORMAT: u32 = 84;
    pub const BITS_PER_COMPONENT: u32 = 101;
    pub const MAX_BITS_PER_COMPONENT: u32 = 102;
    pub const CHANNEL_WIDTH: u32 = 104;
    pub const CHANNEL_HEIGHT: u32 = 105;
    pub const PATTERN_WIDTH: u32 = 106;
    pub const PATTERN_HEIGHT: u32 = 107;
    pub const COMPONENTS_PER_SAMPLE: u32 = 108;
    pub const PRESCALE_SHIFT: u32 = 109;
    /// The 16-bit inverse component transform: the one small chunk that changes the decoded values and is not supported.
    pub const INVERSE_TRANSFORM_16: u32 = 0x4003;
}

/// The order of the colour filters in the decoded mosaic, as the DNG's `CFAPattern` states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mosaic {
    /// R G / G B, 14-bit samples (GoPro's own mapping for RGGB files).
    Rggb,
    /// G B / R G, 12-bit samples.
    Gbrg,
}

fn bad(why: impl std::fmt::Display) -> RawError {
    RawError::Corrupt(format!("GoPro VC-5 stream: {why}"))
}

fn truncated() -> RawError {
    bad("a coded block ends early")
}

/// Decode one VC-5 stream (the whole raw strip of a GPR) into the `width × height` mosaic in `mosaic` order. The stream's
/// own dimensions must be those of the DNG.
pub(crate) fn decode(src: &[u8], width: usize, height: usize, mosaic: Mosaic) -> Result<Vec<u16>> {
    if src.get(..START_MARKER.len()) != Some(&START_MARKER[..]) {
        return Err(bad("it does not start with the VC-5 marker"));
    }
    let book = Codebook::build()?;
    let mut st = State::new();
    let mut pos = START_MARKER.len();
    while !st.complete() {
        let seg: [u8; 4] = src.get(pos..pos + 4).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("it ends before the image is complete"))?;
        pos += 4;
        let raw_tag = i16::from_be_bytes([seg[0], seg[1]]);
        let value = i16::from_be_bytes([seg[2], seg[3]]);
        let optional = raw_tag < 0;
        let id = i32::from(raw_tag).unsigned_abs();
        match id {
            tag::CHANNEL_COUNT => st.channel_count = usize::from(value as u16),
            tag::IMAGE_WIDTH => st.width = usize::from(value as u16),
            tag::IMAGE_HEIGHT => st.height = usize::from(value as u16),
            tag::SUBBAND_NUMBER => st.subband = usize::from(value as u16),
            tag::QUANTIZATION => st.quant = i32::from(value as u16),
            tag::LOWPASS_PRECISION => {
                if !(8..=32).contains(&value) {
                    return Err(bad("the lowpass precision is outside 8 to 32 bits"));
                }
                st.precision = u32::from(value as u16);
            }
            tag::CHANNEL_NUMBER => st.channel = usize::from(value as u16),
            tag::PRESCALE_SHIFT => {
                for (i, p) in st.prescale.iter_mut().enumerate() {
                    *p = ((value >> (14 - 2 * i)) & 3) as u8;
                }
            }
            tag::IMAGE_FORMAT => st.format = value,
            tag::PATTERN_WIDTH => st.pattern.0 = value,
            tag::PATTERN_HEIGHT => st.pattern.1 = value,
            tag::SUBBAND_COUNT
            | tag::BITS_PER_COMPONENT
            | tag::MAX_BITS_PER_COMPONENT
            | tag::COMPONENTS_PER_SAMPLE
            | tag::CHANNEL_WIDTH
            | tag::CHANNEL_HEIGHT => {}
            // a codeblock: a large chunk whose 24-bit size in segments is the value and the low byte of the tag
            _ if id & 0xFF00 == 0x6000 => {
                let segments = usize::from(value as u16) | (((id & 0xFF) as usize) << 16);
                let end = pos.checked_add(segments * 4).ok_or_else(truncated)?;
                let payload = src.get(pos..end).ok_or_else(truncated)?;
                pos = end;
                st.codeblock(payload, &book, width, height)?;
            }
            // any other large chunk (metadata) carries a payload of the same kind of size
            _ if id & 0x6000 == 0x6000 => {
                let segments = usize::from(value as u16) | (((id & 0xFF) as usize) << 16);
                pos = skip(src, pos, segments)?;
            }
            // a small chunk (vendor data, the inverse component permutation and transform of Part 3, an image identifier): the
            // value is its payload size in segments; the Bayer inverse transform is fixed, so only the 16-bit form is refused
            _ if id & 0x4000 != 0 => {
                if id == tag::INVERSE_TRANSFORM_16 && !optional {
                    return Err(RawError::Unsupported("VC-5 stream with a 16-bit inverse component transform".into()));
                }
                pos = skip(src, pos, usize::from(value as u16))?;
            }
            // section markers (a large chunk tag with no payload of its own) only delimit what follows
            _ if id & 0x2000 != 0 => {}
            _ if optional => {}
            _ => return Err(bad(format!("a required element (tag {id}) is not understood"))),
        }
    }
    let (cw, chh) = (width / 2, height / 2);
    let comps: Vec<&[i16]> = st.channels.iter().filter_map(|c| c.out.as_deref()).collect();
    pack(&comps, cw, chh, mosaic)
}

/// The position after a chunk payload of `segments` 32-bit segments.
fn skip(src: &[u8], pos: usize, segments: usize) -> Result<usize> {
    let end = pos.checked_add(segments * 4).filter(|&e| e <= src.len()).ok_or_else(truncated)?;
    Ok(end)
}

/// One wavelet level of one channel: its four bands as they arrive, their quantisation and which bands are present.
struct Level {
    bands: [Vec<i16>; 4],
    quant: [i32; 4],
    have: u8,
}

impl Level {
    fn new() -> Self {
        Level { bands: Default::default(), quant: [1; 4], have: 0 }
    }
}

struct Channel {
    levels: [Level; LEVELS],
    /// The component plane once the finest level is inverted.
    out: Option<Vec<i16>>,
}

/// What the tags seen so far say.
struct State {
    width: usize,
    height: usize,
    channel_count: usize,
    format: i16,
    pattern: (i16, i16),
    channel: usize,
    subband: usize,
    precision: u32,
    quant: i32,
    prescale: [u8; LEVELS],
    channels: Vec<Channel>,
}

impl State {
    fn new() -> Self {
        State {
            width: 0,
            height: 0,
            channel_count: 0,
            format: 0,
            pattern: (0, 0),
            channel: 0,
            subband: 0,
            precision: 16,
            quant: 0,
            prescale: [0; LEVELS],
            channels: Vec::new(),
        }
    }

    fn complete(&self) -> bool {
        self.channels.len() == CHANNELS && self.channels.iter().all(|c| c.out.is_some())
    }

    /// The component size, once the header describes a mosaic this decoder can read and the DNG agrees with it.
    fn component_size(&self, width: usize, height: usize) -> Result<(usize, usize)> {
        if self.channel_count != CHANNELS {
            return Err(RawError::Unsupported(format!("VC-5 stream with {} components instead of the 4 of a Bayer mosaic", self.channel_count)));
        }
        if self.format != IMAGE_FORMAT_RAW || self.pattern != (2, 2) {
            return Err(RawError::Unsupported("VC-5 stream that is not a 2x2 Bayer raw".into()));
        }
        if self.width != width || self.height != height {
            return Err(bad(format!("it is {}x{} but the DNG says {width}x{height}", self.width, self.height)));
        }
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(RawError::Unsupported(format!("VC-5 mosaic of {width}x{height} pixels (the Bayer pattern needs even sizes)")));
        }
        if width.checked_mul(height).is_none_or(|n| n > MAX_PIXELS) {
            return Err(RawError::Limit("VC-5 image too large"));
        }
        let (cw, chh) = (width / 2, height / 2);
        let coarsest = level_dims(cw, chh)[LEVELS - 1];
        if coarsest.0 < 3 || coarsest.1 < 3 {
            return Err(RawError::Unsupported(format!("VC-5 image of {width}x{height} pixels is too small for its wavelets")));
        }
        Ok((cw, chh))
    }

    /// Decode the codeblock `payload` as the current channel's current subband, then invert every wavelet it completes.
    fn codeblock(&mut self, payload: &[u8], book: &Codebook, width: usize, height: usize) -> Result<()> {
        let (cw, chh) = self.component_size(width, height)?;
        if self.channels.is_empty() {
            self.channels = (0..CHANNELS).map(|_| Channel { levels: std::array::from_fn(|_| Level::new()), out: None }).collect();
        }
        let (c, sub) = (self.channel, self.subband);
        let (Some(&level), Some(&band)) = (SUBBAND_LEVEL.get(sub), SUBBAND_BAND.get(sub)) else {
            return Err(bad("a subband number is out of range"));
        };
        let dims = level_dims(cw, chh);
        let (w, h) = dims[level];
        let (precision, quant) = (self.precision, self.quant);
        let channel = self.channels.get_mut(c).ok_or_else(|| bad("there are more channels than the four components"))?;
        let lv = &mut channel.levels[level];
        if lv.have & (1 << band) != 0 {
            return Err(bad("a subband is coded twice"));
        }
        lv.bands[band] = if sub == 0 { read_lowpass(payload, w, h, precision)? } else { read_highpass(payload, w, h, book)? };
        lv.have |= 1 << band;
        if sub != 0 {
            lv.quant[band] = quant;
        }
        // the next codeblock is the next subband unless a tag says otherwise
        self.subband += 1;
        if self.subband == SUBBANDS {
            self.subband = 0;
            self.channel += 1;
        }
        self.reconstruct(c, level, (cw, chh), &dims)
    }

    /// Invert the wavelet at `level` of channel `c` if all four of its bands are there, and so on down to the component
    /// plane: each inverse is the lowpass band of the next finer level.
    fn reconstruct(&mut self, c: usize, mut level: usize, size: (usize, usize), dims: &[(usize, usize); LEVELS]) -> Result<()> {
        let prescale = self.prescale;
        let channel = self.channels.get_mut(c).ok_or_else(|| bad("there are more channels than the four components"))?;
        loop {
            let lv = &mut channel.levels[level];
            if lv.have != 0xF {
                return Ok(());
            }
            let (w, h) = dims[level];
            let (ow, oh) = if level == 0 { size } else { dims[level - 1] };
            let bands = std::mem::take(&mut lv.bands);
            let out = inverse(bands, lv.quant, (w, h), (ow, oh), prescale[level])?;
            if level == 0 {
                channel.out = Some(out);
                return Ok(());
            }
            level -= 1;
            let next = &mut channel.levels[level];
            if next.have & 1 != 0 {
                return Err(bad("a lowpass band is coded twice"));
            }
            next.bands[0] = out;
            next.have |= 1;
        }
    }
}

/// Sizes of the lowpass band of each level: the component size halved (rounding up) once per level.
fn level_dims(cw: usize, chh: usize) -> [(usize, usize); LEVELS] {
    let (mut w, mut h) = (cw, chh);
    let mut dims = [(0, 0); LEVELS];
    for d in dims.iter_mut() {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        *d = (w, h);
    }
    dims
}

/// Reads a block's bits most significant first.
struct Bits<'a> {
    data: &'a [u8],
    next: usize,
    acc: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, next: 0, acc: 0, count: 0 }
    }

    /// The next `n` (at most 32) bits, or `None` when the block ends first.
    fn get(&mut self, n: u32) -> Option<u32> {
        while self.count < n {
            let byte = *self.data.get(self.next)?;
            self.next += 1;
            self.acc = (self.acc << 8) | u64::from(byte);
            self.count += 8;
        }
        self.count -= n;
        let v = (self.acc >> self.count) & ((1u64 << n) - 1);
        self.acc &= (1u64 << self.count) - 1;
        Some(v as u32)
    }
}

/// Codebook 17 as a binary trie: each node holds the child for a 0 and a 1 bit; 0 is no child, [`LEAF`] marks an index into
/// [`TABLE`].
struct Codebook {
    nodes: Vec<[u16; 2]>,
}

fn child(nodes: &mut [[u16; 2]], node: usize, bit: usize) -> Result<&mut u16> {
    nodes.get_mut(node).and_then(|n| n.get_mut(bit)).ok_or_else(|| bad("the codebook is inconsistent"))
}

impl Codebook {
    fn build() -> Result<Self> {
        let mut nodes = vec![[0u16; 2]];
        for (index, &(size, bits, _, _)) in TABLE.iter().enumerate() {
            let mut node = 0usize;
            for i in (0..u32::from(size)).rev() {
                let bit = ((bits >> i) & 1) as usize;
                let slot = *child(&mut nodes, node, bit)?;
                if i == 0 {
                    if slot != 0 {
                        return Err(bad("the codebook repeats a code"));
                    }
                    *child(&mut nodes, node, bit)? = LEAF | index as u16;
                } else if slot == 0 {
                    let next = nodes.len();
                    if next >= usize::from(LEAF) {
                        return Err(bad("the codebook is too large"));
                    }
                    nodes.push([0, 0]);
                    *child(&mut nodes, node, bit)? = next as u16;
                    node = next;
                } else if slot & LEAF != 0 {
                    return Err(bad("the codebook is not prefix free"));
                } else {
                    node = usize::from(slot);
                }
            }
        }
        Ok(Codebook { nodes })
    }

    /// The next codeword: the run length and the unsigned value (a run length of 0 is a special marker).
    fn read(&self, bits: &mut Bits) -> Result<(u16, u16)> {
        let mut node = 0usize;
        loop {
            let bit = bits.get(1).ok_or_else(truncated)? as usize;
            let next = self.nodes.get(node).and_then(|n| n.get(bit)).copied().unwrap_or(0);
            if next == 0 {
                return Err(bad("a codeword is not in the codebook"));
            }
            if next & LEAF != 0 {
                let &(_, _, count, value) = TABLE.get(usize::from(next & !LEAF)).ok_or_else(|| bad("the codebook is inconsistent"))?;
                return Ok((count, value));
            }
            node = usize::from(next);
        }
    }
}

/// The lowpass band: `w × h` words of `precision` bits (read as signed 16-bit coefficients).
fn read_lowpass(payload: &[u8], w: usize, h: usize, precision: u32) -> Result<Vec<i16>> {
    let total = w * h;
    // every word takes `precision` bits: a short block is refused before the band is allocated
    if (total as u64 * u64::from(precision)).div_ceil(8) > payload.len() as u64 {
        return Err(truncated());
    }
    let mut bits = Bits::new(payload);
    let mut band = Vec::with_capacity(total);
    for _ in 0..total {
        band.push(bits.get(precision).ok_or_else(truncated)? as u16 as i16);
    }
    Ok(band)
}

/// A highpass band: runs of one value (zero runs, or a single companded magnitude followed by its sign bit) until `w × h`
/// coefficients are set, then the band-end marker.
fn read_highpass(payload: &[u8], w: usize, h: usize, book: &Codebook) -> Result<Vec<i16>> {
    let total = w * h;
    let mut band = vec![0i16; total];
    let mut bits = Bits::new(payload);
    let mut pos = 0usize;
    while pos < total {
        let (count, value) = book.read(&mut bits)?;
        if count == 0 {
            return Err(bad("a band ends before all its coefficients are coded"));
        }
        let mut v = value as i16;
        if value != 0 && bits.get(1).ok_or_else(truncated)? == 1 {
            v = -v;
        }
        let end = pos + usize::from(count);
        let Some(run) = band.get_mut(pos..end) else {
            return Err(bad("a run passes the end of its band"));
        };
        run.fill(v);
        pos = end;
    }
    let (count, value) = book.read(&mut bits)?;
    if count != 0 || value != 1 {
        return Err(bad("a band lacks its end marker"));
    }
    Ok(band)
}

fn clamp16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Undo the cubic companding `m + 768 m³ / 255³` of each magnitude and the band's quantisation.
fn dequantize(band: &[i16], quant: i32) -> Vec<i16> {
    band.iter()
        .map(|&v| {
            let m = i64::from(v).abs();
            let scaled = (m + m * m * m * 768 / 16_581_375) * i64::from(quant);
            let signed = if v < 0 { -scaled } else { scaled };
            signed.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
        })
        .collect()
}

/// The vertical inverse of a lowpass and a highpass plane (both `w × h`): the even and odd output rows, as planes of the
/// input size. Interior rows use the 3-tap filter, the first and last rows the border filters.
fn vertical(low: &[i16], high: &[i16], w: usize, h: usize) -> (Vec<i16>, Vec<i16>) {
    let mut even = vec![0i16; w * h];
    let mut odd = vec![0i16; w * h];
    let l = |r: usize, x: usize| low.get(r * w + x).map_or(0, |&v| i32::from(v));
    for r in 0..h {
        for x in 0..w {
            let (e, o) = if r == 0 {
                ((11 * l(0, x) - 4 * l(1, x) + l(2, x) + 4) >> 3, (5 * l(0, x) + 4 * l(1, x) - l(2, x) + 4) >> 3)
            } else if r + 1 == h {
                ((5 * l(r, x) + 4 * l(r - 1, x) - l(r - 2, x) + 4) >> 3, (11 * l(r, x) - 4 * l(r - 1, x) + l(r - 2, x) + 4) >> 3)
            } else {
                (((l(r - 1, x) - l(r + 1, x) + 4) >> 3) + l(r, x), ((l(r + 1, x) - l(r - 1, x) + 4) >> 3) + l(r, x))
            };
            let k = r * w + x;
            let hv = high.get(k).map_or(0, |&v| i32::from(v));
            if let (Some(a), Some(b)) = (even.get_mut(k), odd.get_mut(k)) {
                *a = clamp16((e + hv) >> 1);
                *b = clamp16((o - hv) >> 1);
            }
        }
    }
    (even, odd)
}

/// The horizontal inverse of one row of a lowpass and a highpass plane into `out`, which is `2 w` samples or one fewer.
/// Without a prescale the result is halved and held to 14 bits; with a prescale of 2 or 3 the halving is replaced by the
/// descale (a left shift of one for 2) and the result is held to 16 bits signed.
fn horizontal(low: &[i16], high: &[i16], out: &mut [i16], prescale: u8) {
    let w = low.len();
    let descale = prescale > 1;
    let shift = u32::from(prescale == 2);
    let at = |x: usize| low.get(x).map_or(0, |&v| i32::from(v));
    let store = |sum: i32| if descale { clamp16(sum << shift) } else { (sum >> 1).clamp(0, 16383) as i16 };
    for x in 0..w {
        let (e, o) = if x == 0 {
            ((11 * at(0) - 4 * at(1) + at(2) + 4) >> 3, (5 * at(0) + 4 * at(1) - at(2) + 4) >> 3)
        } else if x + 1 == w {
            ((5 * at(x) + 4 * at(x - 1) - at(x - 2) + 4) >> 3, (11 * at(x) - 4 * at(x - 1) + at(x - 2) + 4) >> 3)
        } else {
            (((at(x - 1) - at(x + 1) + 4) >> 3) + at(x), ((at(x + 1) - at(x - 1) + 4) >> 3) + at(x))
        };
        let hv = high.get(x).map_or(0, |&v| i32::from(v));
        if let Some(d) = out.get_mut(2 * x) {
            *d = store(e + hv);
        }
        if let Some(d) = out.get_mut(2 * x + 1) {
            *d = store(o - hv);
        }
    }
}

/// The inverse wavelet of one level: the four `w × h` bands (lowpass, then the three highpass bands, dequantised with
/// `quant`) become the `ow × oh` lowpass band (or component plane) of the next finer level, where `ow` is `2w` or `2w - 1`
/// and `oh` is `2h` or `2h - 1`.
fn inverse(bands: [Vec<i16>; 4], quant: [i32; 4], (w, h): (usize, usize), (ow, oh): (usize, usize), prescale: u8) -> Result<Vec<i16>> {
    let n = w * h;
    if w < 3 || h < 3 || !(2 * w - 1..=2 * w).contains(&ow) || !(2 * h - 1..=2 * h).contains(&oh) || bands.iter().any(|b| b.len() != n) {
        return Err(bad("a wavelet's bands do not fit its size"));
    }
    let [ll, lh, hl, hh] = &bands;
    let lh = dequantize(lh, quant[1]);
    let hl = dequantize(hl, quant[2]);
    let hh = dequantize(hh, quant[3]);
    let (even_lo, odd_lo) = vertical(ll, &hl, w, h);
    let (even_hi, odd_hi) = vertical(&lh, &hh, w, h);
    let mut out = vec![0i16; ow * oh];
    for r in 0..h {
        let row = r * w..(r + 1) * w;
        if let Some(dst) = out.get_mut(2 * r * ow..(2 * r + 1) * ow) {
            horizontal(even_lo.get(row.clone()).unwrap_or_default(), even_hi.get(row.clone()).unwrap_or_default(), dst, prescale);
        }
        if let Some(dst) = out.get_mut((2 * r + 1) * ow..(2 * r + 2) * ow) {
            horizontal(odd_lo.get(row.clone()).unwrap_or_default(), odd_hi.get(row).unwrap_or_default(), dst, prescale);
        }
    }
    Ok(out)
}

/// The inverse ProTune log curve: 12-bit coded value to 16-bit linear.
fn log_curve() -> Vec<u16> {
    (0..4096)
        .map(|i| {
            let v = 65535.0 * (113f64.powf(f64::from(i) / 4095.0) - 1.0) / 112.0;
            (v as f32 as i32).min(65535) as u16
        })
        .collect()
}

/// The component planes GS, RG, BG, GD (each `cw × chh`) to the mosaic: red and blue are twice their difference from the
/// green sum above it, the two greens are the green sum plus and minus the green difference; each goes through the log curve and is
/// reduced to 14 bits (RGGB) or 12 bits (GBRG) and placed in its 2x2 cell.
fn pack(comps: &[&[i16]], cw: usize, chh: usize, mosaic: Mosaic) -> Result<Vec<u16>> {
    let [gs, rg, bg, gd] = comps else {
        return Err(bad("it has fewer than four components"));
    };
    let n = cw * chh;
    if comps.iter().any(|c| c.len() != n) {
        return Err(bad("a component plane has the wrong size"));
    }
    let ow = cw * 2;
    let log = log_curve();
    let (shift, rggb) = match mosaic {
        Mosaic::Rggb => (2u32, true),
        Mosaic::Gbrg => (4u32, false),
    };
    let lookup = |v: i32| log.get(v.clamp(0, 4095) as usize).map_or(0, |&l| l >> shift);
    let mut out = vec![0u16; n * 4];
    for y in 0..chh {
        for x in 0..cw {
            let i = y * cw + x;
            let at = |c: &[i16]| c.get(i).map_or(0, |&v| i32::from(v as u16));
            let (s, d_rg, d_bg, d_gd) = (at(*gs), at(*rg) - MIDPOINT, at(*bg) - MIDPOINT, at(*gd) - MIDPOINT);
            let r = lookup((d_rg << 1) + s);
            let b = lookup((d_bg << 1) + s);
            let g1 = lookup(s + d_gd);
            let g2 = lookup(s - d_gd);
            let cell = if rggb { [r, g1, g2, b] } else { [g1, b, r, g2] };
            let base = 2 * y * ow + 2 * x;
            for (k, v) in cell.into_iter().enumerate() {
                if let Some(o) = out.get_mut(base + (k / 2) * ow + k % 2) {
                    *o = v;
                }
            }
        }
    }
    Ok(out)
}

/// Codebook 17 (`table17.inc` of GoPro's GPR): codeword length in bits, the codeword, the run length and the value. A run
/// length of 0 is a special marker (value 1 ends a band); every other entry is a run of that many copies of the value, which
/// is zero or a single unsigned magnitude.
#[rustfmt::skip]
const TABLE: [(u8, u32, u16, u16); 264] = [
    (1, 0x00000000, 1, 0),
    (2, 0x00000002, 1, 1),
    (3, 0x00000007, 1, 2),
    (5, 0x00000019, 1, 3),
    (6, 0x00000030, 1, 4),
    (6, 0x00000036, 1, 5),
    (7, 0x0000006F, 1, 8),
    (7, 0x00000063, 1, 6),
    (7, 0x00000069, 12, 0),
    (7, 0x0000006B, 1, 7),
    (8, 0x000000D1, 20, 0),
    (8, 0x000000D4, 1, 9),
    (8, 0x000000DC, 1, 10),
    (9, 0x00000189, 1, 11),
    (9, 0x0000018A, 32, 0),
    (9, 0x000001A0, 1, 12),
    (9, 0x000001AB, 1, 13),
    (10, 0x00000377, 1, 18),
    (10, 0x00000310, 1, 14),
    (10, 0x00000316, 1, 15),
    (10, 0x00000343, 60, 0),
    (10, 0x00000354, 1, 16),
    (10, 0x00000375, 1, 17),
    (11, 0x00000623, 1, 19),
    (11, 0x00000684, 1, 20),
    (11, 0x00000685, 100, 0),
    (11, 0x000006AB, 1, 21),
    (11, 0x000006EC, 1, 22),
    (12, 0x00000DDB, 1, 29),
    (12, 0x00000C5C, 1, 24),
    (12, 0x00000C5E, 1, 25),
    (12, 0x00000C44, 1, 23),
    (12, 0x00000D55, 1, 26),
    (12, 0x00000DD1, 1, 27),
    (12, 0x00000DD3, 1, 28),
    (13, 0x00001BB5, 1, 35),
    (13, 0x0000188B, 1, 30),
    (13, 0x000018BB, 1, 31),
    (13, 0x000018BF, 180, 0),
    (13, 0x00001AA8, 1, 32),
    (13, 0x00001BA0, 1, 33),
    (13, 0x00001BA5, 320, 0),
    (13, 0x00001BA4, 1, 34),
    (14, 0x00003115, 1, 36),
    (14, 0x00003175, 1, 37),
    (14, 0x0000317D, 1, 38),
    (14, 0x00003553, 1, 39),
    (14, 0x00003768, 1, 40),
    (15, 0x00006E87, 1, 46),
    (15, 0x00006ED3, 1, 47),
    (15, 0x000062E8, 1, 42),
    (15, 0x000062F8, 1, 43),
    (15, 0x00006228, 1, 41),
    (15, 0x00006AA4, 1, 44),
    (15, 0x00006E85, 1, 45),
    (16, 0x0000C453, 1, 48),
    (16, 0x0000C5D3, 1, 49),
    (16, 0x0000C5F3, 1, 50),
    (16, 0x0000DDA4, 1, 53),
    (16, 0x0000DD08, 1, 51),
    (16, 0x0000DD0C, 1, 52),
    (17, 0x0001BB4B, 1, 61),
    (17, 0x0001BB4A, 1, 60),
    (17, 0x00018BA5, 1, 55),
    (17, 0x00018BE5, 1, 56),
    (17, 0x0001AA95, 1, 57),
    (17, 0x0001AA97, 1, 58),
    (17, 0x000188A4, 1, 54),
    (17, 0x0001BA13, 1, 59),
    (18, 0x00031748, 1, 62),
    (18, 0x000317C8, 1, 63),
    (18, 0x00035528, 1, 64),
    (18, 0x0003552C, 1, 65),
    (18, 0x00037424, 1, 66),
    (18, 0x00037434, 1, 67),
    (18, 0x00037436, 1, 68),
    (19, 0x00062294, 1, 69),
    (19, 0x00062E92, 1, 70),
    (19, 0x00062F92, 1, 71),
    (19, 0x0006AA52, 1, 72),
    (19, 0x0006AA5A, 1, 73),
    (19, 0x0006E86A, 1, 75),
    (19, 0x0006E86E, 1, 76),
    (19, 0x0006E84A, 1, 74),
    (20, 0x000C452A, 1, 77),
    (20, 0x000C5D27, 1, 78),
    (20, 0x000C5F26, 1, 79),
    (20, 0x000D54A6, 1, 80),
    (20, 0x000D54B6, 1, 81),
    (20, 0x000DD096, 1, 82),
    (20, 0x000DD0D6, 1, 83),
    (20, 0x000DD0DE, 1, 84),
    (21, 0x00188A56, 1, 85),
    (21, 0x0018BA4D, 1, 86),
    (21, 0x0018BE4E, 1, 87),
    (21, 0x0018BE4F, 1, 88),
    (21, 0x001AA96E, 1, 89),
    (21, 0x001BA12E, 1, 90),
    (21, 0x001BA12F, 1, 91),
    (21, 0x001BA1AF, 1, 92),
    (21, 0x001BA1BF, 1, 93),
    (22, 0x0037435D, 1, 99),
    (22, 0x0037437D, 1, 100),
    (22, 0x00317498, 1, 94),
    (22, 0x0035529C, 1, 95),
    (22, 0x0035529D, 1, 96),
    (22, 0x003552DE, 1, 97),
    (22, 0x003552DF, 1, 98),
    (23, 0x0062E933, 1, 102),
    (23, 0x0062295D, 1, 101),
    (23, 0x006AA53D, 1, 103),
    (23, 0x006AA53F, 1, 105),
    (23, 0x006AA53E, 1, 104),
    (23, 0x006E86B9, 1, 106),
    (23, 0x006E86F8, 1, 107),
    (24, 0x00D54A79, 1, 111),
    (24, 0x00C5D265, 1, 109),
    (24, 0x00C452B8, 1, 108),
    (24, 0x00DD0D71, 1, 113),
    (24, 0x00D54A78, 1, 110),
    (24, 0x00DD0D70, 1, 112),
    (24, 0x00DD0DF2, 1, 114),
    (24, 0x00DD0DF3, 1, 115),
    (25, 0x0188A5F6, 1, 225),
    (25, 0x0188A5F5, 1, 189),
    (25, 0x0188A5F4, 1, 188),
    (25, 0x0188A5F3, 1, 203),
    (25, 0x0188A5F2, 1, 202),
    (25, 0x0188A5F1, 1, 197),
    (25, 0x0188A5F0, 1, 207),
    (25, 0x0188A5EF, 1, 169),
    (25, 0x0188A5EE, 1, 223),
    (25, 0x0188A5ED, 1, 159),
    (25, 0x0188A5AA, 1, 235),
    (25, 0x0188A5E3, 1, 152),
    (25, 0x0188A5DF, 1, 192),
    (25, 0x0188A589, 1, 179),
    (25, 0x0188A5DD, 1, 201),
    (25, 0x0188A578, 1, 172),
    (25, 0x0188A5E0, 1, 149),
    (25, 0x0188A588, 1, 178),
    (25, 0x0188A5D6, 1, 120),
    (25, 0x0188A5DB, 1, 219),
    (25, 0x0188A5E1, 1, 150),
    (25, 0x0188A587, 1, 127),
    (25, 0x0188A59A, 1, 211),
    (25, 0x0188A5C4, 1, 125),
    (25, 0x0188A5EC, 1, 158),
    (25, 0x0188A586, 1, 247),
    (25, 0x0188A573, 1, 238),
    (25, 0x0188A59C, 1, 163),
    (25, 0x0188A5C8, 1, 228),
    (25, 0x0188A5FB, 1, 183),
    (25, 0x0188A5A1, 1, 217),
    (25, 0x0188A5EB, 1, 168),
    (25, 0x0188A5A8, 1, 122),
    (25, 0x0188A584, 1, 128),
    (25, 0x0188A5D2, 1, 249),
    (25, 0x0188A599, 1, 187),
    (25, 0x0188A598, 1, 186),
    (25, 0x0188A583, 1, 136),
    (25, 0x018BA4C9, 1, 181),
    (25, 0x0188A5D0, 1, 255),
    (25, 0x0188A594, 1, 230),
    (25, 0x0188A582, 1, 135),
    (25, 0x0188A5CB, 1, 233),
    (25, 0x0188A5D8, 1, 222),
    (25, 0x0188A5E7, 1, 145),
    (25, 0x0188A581, 1, 134),
    (25, 0x0188A5EA, 1, 167),
    (25, 0x0188A5A9, 1, 248),
    (25, 0x0188A5A6, 1, 209),
    (25, 0x0188A580, 1, 243),
    (25, 0x0188A5A0, 1, 216),
    (25, 0x0188A59D, 1, 164),
    (25, 0x0188A5C3, 1, 140),
    (25, 0x0188A57F, 1, 157),
    (25, 0x0188A5C0, 1, 239),
    (25, 0x0188A5DE, 1, 191),
    (25, 0x0188A5D4, 1, 251),
    (25, 0x0188A57E, 1, 156),
    (25, 0x0188A5C2, 1, 139),
    (25, 0x0188A592, 1, 242),
    (25, 0x0188A5CD, 1, 133),
    (25, 0x0188A57D, 1, 162),
    (25, 0x0188A5A3, 1, 213),
    (25, 0x0188A5E8, 1, 165),
    (25, 0x0188A5A2, 1, 212),
    (25, 0x0188A57C, 1, 227),
    (25, 0x0188A58E, 1, 198),
    (25, 0x0188A5B3, 1, 236),
    (25, 0x0188A5B2, 1, 234),
    (25, 0x0188A5B1, 1, 117),
    (25, 0x0188A5B0, 1, 215),
    (25, 0x0188A5AF, 1, 124),
    (25, 0x0188A5AE, 1, 123),
    (25, 0x0188A5AD, 1, 254),
    (25, 0x0188A5AC, 1, 253),
    (25, 0x0188A5AB, 1, 148),
    (25, 0x0188A5DA, 1, 218),
    (25, 0x0188A5E4, 1, 146),
    (25, 0x0188A5E5, 1, 147),
    (25, 0x0188A5D9, 1, 224),
    (25, 0x0188A5B5, 1, 143),
    (25, 0x0188A5BC, 1, 184),
    (25, 0x0188A5BD, 1, 185),
    (25, 0x0188A5E9, 1, 166),
    (25, 0x0188A5CC, 1, 132),
    (25, 0x0188A585, 1, 129),
    (25, 0x0188A5D3, 1, 250),
    (25, 0x0188A5E2, 1, 151),
    (25, 0x0188A595, 1, 119),
    (25, 0x0188A596, 1, 193),
    (25, 0x0188A5B8, 1, 176),
    (25, 0x0188A590, 1, 245),
    (25, 0x0188A5C9, 1, 229),
    (25, 0x0188A5A4, 1, 206),
    (25, 0x0188A5E6, 1, 144),
    (25, 0x0188A5A5, 1, 208),
    (25, 0x0188A5CE, 1, 137),
    (25, 0x0188A5BF, 1, 241),
    (25, 0x0188A572, 1, 237),
    (25, 0x0188A59B, 1, 190),
    (25, 0x0188A5BE, 1, 240),
    (25, 0x0188A5C7, 1, 131),
    (25, 0x0188A5CA, 1, 232),
    (25, 0x0188A5D5, 1, 252),
    (25, 0x0188A57B, 1, 171),
    (25, 0x0188A58D, 1, 205),
    (25, 0x0188A58C, 1, 204),
    (25, 0x0188A58B, 1, 118),
    (25, 0x0188A58A, 1, 214),
    (25, 0x018BA4C8, 1, 180),
    (25, 0x0188A5C5, 1, 126),
    (25, 0x0188A5FA, 1, 182),
    (25, 0x0188A5BB, 1, 175),
    (25, 0x0188A5C1, 1, 141),
    (25, 0x0188A5CF, 1, 138),
    (25, 0x0188A5B9, 1, 177),
    (25, 0x0188A5B6, 1, 153),
    (25, 0x0188A597, 1, 194),
    (25, 0x0188A5FE, 1, 160),
    (25, 0x0188A5D7, 1, 121),
    (25, 0x0188A5BA, 1, 174),
    (25, 0x0188A591, 1, 246),
    (25, 0x0188A5C6, 1, 130),
    (25, 0x0188A5DC, 1, 200),
    (25, 0x0188A57A, 1, 170),
    (25, 0x0188A59F, 1, 221),
    (25, 0x0188A5F9, 1, 196),
    (25, 0x0188A5B4, 1, 142),
    (25, 0x0188A5A7, 1, 210),
    (25, 0x0188A58F, 1, 199),
    (25, 0x0188A5FD, 1, 155),
    (25, 0x0188A5B7, 1, 154),
    (25, 0x0188A593, 1, 244),
    (25, 0x0188A59E, 1, 220),
    (25, 0x0188A5F8, 1, 195),
    (25, 0x0188A5FF, 1, 161),
    (25, 0x0188A5FC, 1, 231),
    (25, 0x0188A579, 1, 173),
    (25, 0x0188A5F7, 1, 226),
    (26, 0x03114BA2, 1, 116),
    (26, 0x03114BA3, 0, 1),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes bits most significant first, as the stream is read.
    #[derive(Default)]
    struct BitWriter {
        bits: Vec<bool>,
    }

    impl BitWriter {
        fn put(&mut self, value: u64, n: u32) {
            for i in (0..n).rev() {
                self.bits.push((value >> i) & 1 == 1);
            }
        }

        /// Pad with zero bits to a whole number of 32-bit segments.
        fn align(&mut self) {
            while !self.bits.len().is_multiple_of(32) {
                self.bits.push(false);
            }
        }

        fn bytes(&self) -> Vec<u8> {
            self.bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |b, (i, &bit)| b | (u8::from(bit) << (7 - i)))).collect()
        }
    }

    /// The codeword of the table entry with this run length and value.
    fn code(count: u16, value: u16) -> (u32, u64) {
        let &(size, bits, _, _) = TABLE.iter().find(|e| e.2 == count && e.3 == value).expect("entry in the codebook");
        (u32::from(size), u64::from(bits))
    }

    fn put_code(w: &mut BitWriter, count: u16, value: u16) {
        let (size, bits) = code(count, value);
        w.put(bits, size);
    }

    /// A run of `n` zeros as the codebook's zero runs, longest first.
    fn put_zeros(w: &mut BitWriter, mut n: usize) {
        let mut runs: Vec<u16> = TABLE.iter().filter(|e| e.3 == 0 && e.2 > 0).map(|e| e.2).collect();
        runs.sort_unstable_by(|a, b| b.cmp(a));
        while n > 0 {
            let run = *runs.iter().find(|&&r| usize::from(r) <= n).expect("a run of one exists");
            put_code(w, run, 0);
            n -= usize::from(run);
        }
    }

    /// A highpass band as the stream codes it: runs of zeros, single signed magnitudes and the end marker.
    fn band_bits(values: &[i16]) -> BitWriter {
        let mut w = BitWriter::default();
        let mut zeros = 0usize;
        for &v in values {
            if v == 0 {
                zeros += 1;
                continue;
            }
            put_zeros(&mut w, zeros);
            zeros = 0;
            put_code(&mut w, 1, v.unsigned_abs());
            w.put(u64::from(v < 0), 1);
        }
        put_zeros(&mut w, zeros);
        put_code(&mut w, 0, 1);
        w
    }

    /// [`band_bits`] padded to a whole segment.
    fn encode_band(values: &[i16]) -> Vec<u8> {
        let mut w = band_bits(values);
        w.align();
        w.bytes()
    }

    fn encode_lowpass(values: &[i16], precision: u32) -> Vec<u8> {
        let mut w = BitWriter::default();
        for &v in values {
            w.put(u64::from(v as u16), precision);
        }
        w.align();
        w.bytes()
    }

    #[test]
    fn the_codebook_is_a_prefix_code_and_every_entry_decodes_to_itself() {
        let book = Codebook::build().unwrap();
        assert_eq!(TABLE.len(), 264);
        for &(size, bits, count, value) in TABLE.iter() {
            let mut w = BitWriter::default();
            w.put(u64::from(bits), u32::from(size));
            let bytes = w.bytes();
            let mut r = Bits::new(&bytes);
            assert_eq!(book.read(&mut r).unwrap(), (count, value), "{size} bits {bits:#x}");
        }
    }

    #[test]
    fn known_codewords_decode() {
        let book = Codebook::build().unwrap();
        // (bits, length, run, value) from codebook 17: m0, m1, m2, z12, z20 and the band-end marker c256
        let known = [
            (0x0u64, 1, 1, 0),
            (0x2, 2, 1, 1),
            (0x7, 3, 1, 2),
            (0x69, 7, 12, 0),
            (0xD1, 8, 20, 0),
            (0x03114BA3, 26, 0, 1),
        ];
        for (bits, size, run, value) in known {
            let mut w = BitWriter::default();
            w.put(bits, size);
            let bytes = w.bytes();
            assert_eq!(book.read(&mut Bits::new(&bytes)).unwrap(), (run, value), "{bits:#x}");
        }
    }

    #[test]
    fn a_highpass_band_decodes_runs_signs_and_the_end_marker() {
        let book = Codebook::build().unwrap();
        // 4 x 3, with zeros running across the end of the first row and a long run at the end
        let band = [0, 0, 0, 0, 0, 5, -3, 0, 255, -255, 0, 0];
        assert_eq!(read_highpass(&encode_band(&band), 4, 3, &book).unwrap(), band);
        let zeros = vec![0i16; 600];
        assert_eq!(read_highpass(&encode_band(&zeros), 30, 20, &book).unwrap(), zeros);
    }

    #[test]
    fn malformed_highpass_bands_are_errors() {
        let book = Codebook::build().unwrap();
        let values = [0, 1, -1, 0, 0, 0, 0, 0, 2, 0, 0, 0];
        let good = encode_band(&values);
        assert!(read_highpass(&good, 4, 3, &book).is_ok());
        // truncated anywhere before the end of the last codeword (the padding after it is not needed)
        for n in 0..band_bits(&values).bits.len().div_ceil(8) {
            assert!(read_highpass(&good[..n], 4, 3, &book).is_err(), "{n} bytes");
        }
        // a run of 12 zeros passes the end of a band of 6
        assert!(read_highpass(&encode_band(&[0; 12]), 3, 2, &book).is_err());
        // the end marker before the band is full
        let mut w = BitWriter::default();
        put_code(&mut w, 1, 3);
        w.put(0, 1);
        put_code(&mut w, 0, 1);
        w.align();
        assert!(read_highpass(&w.bytes(), 4, 3, &book).is_err());
        // a band with no marker after its last coefficient
        let mut w = BitWriter::default();
        put_zeros(&mut w, 6);
        w.align();
        assert!(read_highpass(&w.bytes(), 3, 2, &book).is_err());
        assert!(read_highpass(&[], 3, 2, &book).is_err());
    }

    #[test]
    fn the_lowpass_band_reads_words_and_a_short_block_is_refused() {
        let values = [0i16, 1, -1, 32767, -32768, 1234, 4321, 7];
        assert_eq!(read_lowpass(&encode_lowpass(&values, 16), 4, 2, 16).unwrap(), values);
        // 12-bit words
        let small = [0i16, 1, 4095, 2048, 100, 7];
        assert_eq!(read_lowpass(&encode_lowpass(&small, 12), 3, 2, 12).unwrap(), small);
        assert!(read_lowpass(&encode_lowpass(&values, 16)[..15], 4, 2, 16).is_err());
        // an absurd band is refused from its size, without allocating it
        assert!(read_lowpass(&[0; 8], 1 << 14, 1 << 14, 32).is_err());
    }

    fn bands(ll: &[i16], lh: &[i16], hl: &[i16], hh: &[i16]) -> [Vec<i16>; 4] {
        [ll.to_vec(), lh.to_vec(), hl.to_vec(), hh.to_vec()]
    }

    // 4 x 3 bands, as the stream codes them: the highpass values are companded and quantised by 3, 5 and 7 (the bands are
    // LH, HL and HH)
    const LL: [i16; 12] = [1000, 1100, 1200, 1300, 1010, 1120, 1230, 1340, 990, 1000, 1500, 1100];
    const LH: [i16; 12] = [0, 5, -3, 0, 40, 0, 0, -7, 0, 1, 0, 2];
    const HL: [i16; 12] = [2, 0, 0, -1, 0, 0, 60, 0, -4, 0, 0, 9];
    const HH: [i16; 12] = [0, 0, 0, 0, 1, -1, 0, 0, 0, 0, 100, -100];
    const QUANT: [i32; 4] = [1, 3, 5, 7];

    #[test]
    fn a_constant_lowpass_band_inverts_to_a_quarter_without_prescale_and_to_itself_with_prescale_two() {
        let flat = [1000i16; 12];
        let zero = [0i16; 12];
        for (ow, oh) in [(8, 6), (7, 5), (7, 6), (8, 5)] {
            let out = inverse(bands(&flat, &zero, &zero, &zero), [1; 4], (4, 3), (ow, oh), 0).unwrap();
            assert_eq!(out, vec![250i16; ow * oh], "{ow}x{oh}");
            let out = inverse(bands(&flat, &zero, &zero, &zero), [1; 4], (4, 3), (ow, oh), 2).unwrap();
            assert_eq!(out, vec![1000i16; ow * oh], "descaled {ow}x{oh}");
        }
        // without prescale the result is held to 14 bits, with it to 16 bits signed
        let hot = [32000i16; 12];
        assert_eq!(inverse(bands(&hot, &zero, &zero, &zero), [1; 4], (4, 3), (8, 6), 0).unwrap(), vec![8000i16; 48]);
        let low = [-4000i16; 12];
        assert_eq!(inverse(bands(&low, &zero, &zero, &zero), [1; 4], (4, 3), (8, 6), 0).unwrap(), vec![0i16; 48]);
        assert_eq!(inverse(bands(&low, &zero, &zero, &zero), [1; 4], (4, 3), (8, 6), 2).unwrap(), vec![-4000i16; 48]);
    }

    /// The expected arrays come from an independent model of the reference decoder's `InvertSpatialQuant16s` /
    /// `InvertSpatialQuantDescale16s` (checked first against real GPR files of three cameras); the first sample is also worked
    /// by hand. Column 0, row 0: the vertical top-row filter gives the left pair `(11·1000 − 4·1010 + 990 + 4) >> 3 = 994` plus
    /// the dequantised HL `2·5 = 10`, halved: 502; the right pair `(11·0 − 4·(42·3) + 0 + 4) >> 3 = −63` plus HH 0, halved: −32;
    /// likewise 539 and 10 at column 1, 611 and −6 at column 2; the horizontal left-border filter gives
    /// `(11·502 − 4·539 + 611 + 4) >> 3 = 497`, plus −32, halved: 232.
    #[test]
    fn the_inverse_wavelet_matches_hand_computed_arrays() {
        let run = |ow, oh, prescale| inverse(bands(&LL, &LH, &HL, &HH), QUANT, (4, 3), (ow, oh), prescale).unwrap();
        let p0: Vec<Vec<i16>> = vec![
            vec![232, 269, 267, 271, 297, 314, 317, 308],
            vec![254, 243, 277, 284, 286, 303, 320, 355],
            vec![287, 218, 265, 301, 378, 393, 365, 317],
            vec![266, 238, 281, 272, 223, 236, 280, 376],
            vec![264, 224, 249, 275, 600, 101, 79, 541],
            vec![258, 241, 221, 255, 143, 656, 580, 0],
        ];
        let flat = |rows: &[Vec<i16>], w: usize, h: usize| rows.iter().take(h).flat_map(|r| r.iter().take(w).copied()).collect::<Vec<i16>>();
        assert_eq!(run(8, 6, 0), flat(p0.as_slice(), 8, 6));
        // an odd output is the same arrays without the last column and row
        assert_eq!(run(7, 5, 0), flat(p0.as_slice(), 7, 5));
        let p2: Vec<Vec<i16>> = vec![
            vec![930, 1078, 1070, 1086, 1188, 1256, 1268, 1232],
            vec![1018, 974, 1108, 1136, 1144, 1212, 1280, 1420],
            vec![1148, 872, 1060, 1204, 1512, 1574, 1460, 1268],
            vec![1064, 952, 1126, 1090, 892, 944, 1122, 1506],
            vec![1058, 898, 996, 1102, 2400, 404, 316, 2166],
            vec![1034, 966, 884, 1020, 574, 2624, 2320, -400],
        ];
        assert_eq!(run(8, 6, 2), flat(p2.as_slice(), 8, 6));
        assert_eq!(run(7, 5, 2), flat(p2.as_slice(), 7, 5));
        // a prescale of 3 shifts by nothing (only 2 is defined, as in the reference)
        assert_ne!(run(8, 6, 3), run(8, 6, 2));
    }

    #[test]
    fn the_inverse_wavelet_refuses_inconsistent_sizes() {
        let b = || bands(&LL, &LH, &HL, &HH);
        assert!(inverse(b(), QUANT, (4, 3), (8, 6), 0).is_ok());
        // output sizes must be 2n or 2n - 1
        assert!(inverse(b(), QUANT, (4, 3), (6, 6), 0).is_err());
        assert!(inverse(b(), QUANT, (4, 3), (9, 6), 0).is_err());
        assert!(inverse(b(), QUANT, (4, 3), (8, 4), 0).is_err());
        // too small for the three-tap filter, or bands of another size
        assert!(inverse(b(), QUANT, (2, 6), (4, 12), 0).is_err());
        assert!(inverse(b(), QUANT, (6, 2), (12, 4), 0).is_err());
        assert!(inverse(b(), QUANT, (0, 0), (0, 0), 0).is_err());
        assert!(inverse(bands(&LL[..11], &LH, &HL, &HH), QUANT, (4, 3), (8, 6), 0).is_err());
    }

    #[test]
    fn dequantizing_undoes_the_cubic_companding_and_the_quantisation() {
        // m + 768 m³ / 255³ (truncated), times the quantisation, with the sign kept
        assert_eq!(dequantize(&[0, 1, 2, 40, 255, -1, -40, -255], 1), [0, 1, 2, 42, 1023, -1, -42, -1023]);
        assert_eq!(dequantize(&[0, 1, 40, -40, 255], 3), [0, 3, 126, -126, 3069]);
        // held to the 16-bit range
        assert_eq!(dequantize(&[255, -255], 65535), [32767, -32768]);
        assert_eq!(dequantize(&[7, -7], 0), [0, 0]);
    }

    #[test]
    fn the_log_curve_is_the_inverse_protune_curve() {
        let log = log_curve();
        assert_eq!(log.len(), 4096);
        // pinned from the reference formula 65535 (113^(i/4095) − 1) / 112, truncated through a float
        let pins = [(0, 0), (1, 0), (100, 71), (2048, 5638), (2800, 14242), (2950, 17045), (3000, 18093), (3050, 19203), (3200, 22944)];
        for (i, want) in pins.into_iter().chain([(4094, 65458), (4095, 65535)]) {
            assert_eq!(log[i], want, "{i}");
        }
        assert!(log.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn components_are_placed_in_the_cfa_cell_by_the_pattern() {
        // GS 3000, RG +100, BG −100, GD +50: R = 3200, B = 2800, G1 = 3050, G2 = 2950
        let comps: [&[i16]; 4] = [&[3000], &[2148], &[1948], &[2098]];
        // RGGB keeps 14 bits: R G1 / G2 B
        assert_eq!(pack(&comps, 1, 1, Mosaic::Rggb).unwrap(), [22944 >> 2, 19203 >> 2, 17045 >> 2, 14242 >> 2]);
        // GBRG keeps 12 bits: G1 B / R G2
        assert_eq!(pack(&comps, 1, 1, Mosaic::Gbrg).unwrap(), [19203 >> 4, 14242 >> 4, 22944 >> 4, 17045 >> 4]);
        // values outside the 12-bit range of the curve are held to its ends
        let hot: [&[i16]; 4] = [&[4095], &[2148], &[0], &[2048]];
        assert_eq!(pack(&hot, 1, 1, Mosaic::Rggb).unwrap(), [16383, 16383, 16383, 0]);
        // two components across and one down: each pair of values is its own cell, in raster order
        let wide: [&[i16]; 4] = [&[3000, 2000], &[2048, 2048], &[2048, 2048], &[2048, 2048]];
        let out = pack(&wide, 2, 1, Mosaic::Rggb).unwrap();
        let (a, b) = (log_curve()[3000] >> 2, log_curve()[2000] >> 2);
        assert_eq!(out, [a, a, b, b, a, a, b, b]);
        assert!(pack(&comps[..3], 1, 1, Mosaic::Rggb).is_err());
        assert!(pack(&comps, 2, 1, Mosaic::Rggb).is_err());
    }

    fn seg(tag: i16, value: i16) -> Vec<u8> {
        let mut v = tag.to_be_bytes().to_vec();
        v.extend(value.to_be_bytes());
        v
    }

    /// A conforming stream of an `iw × ih` mosaic whose four components are constant (`comps`, even values): every highpass
    /// band is empty (all zeros), every level has the descaling prescale 2, so each inverse level returns its lowpass value.
    /// `subband_tag` is the SubbandNumber written before the codeblock of channel `c`, subband `s`; `extra` follows the header.
    fn stream(iw: usize, ih: usize, comps: [i16; 4], extra: &[u8], subband_tag: impl Fn(usize, usize) -> i16) -> Vec<u8> {
        let dims = level_dims(iw / 2, ih / 2);
        let mut s = START_MARKER.to_vec();
        for (t, v) in [(12, 4), (20, iw as i16), (21, ih as i16), (84, 4), (106, 2), (107, 2), (108, 1), (102, 12), (109, 0xA800u16 as i16)] {
            s.extend(seg(t, v));
        }
        s.extend(extra);
        for c in 0..CHANNELS {
            for sub in 0..SUBBANDS {
                let (w, h) = dims[SUBBAND_LEVEL[sub]];
                s.extend(seg(62, c as i16));
                s.extend(seg(48, subband_tag(c, sub)));
                let payload = if sub == 0 {
                    s.extend(seg(35, 16));
                    encode_lowpass(&vec![comps[c]; w * h], 16)
                } else {
                    s.extend(seg(53, 1));
                    encode_band(&vec![0; w * h])
                };
                // optional (negative) large codeblock tag, as the cameras write it, then its size in segments
                s.extend(seg(-0x6000, (payload.len() / 4) as i16));
                s.extend(payload);
            }
        }
        s
    }

    fn plain(iw: usize, ih: usize) -> Vec<u8> {
        stream(iw, ih, [3000, 2148, 1948, 2098], &[], |_, s| s as i16)
    }

    #[test]
    fn a_stream_decodes_to_the_mosaic_in_the_pattern_order() {
        let log = log_curve();
        // R = 3200, G1 = 3050, G2 = 2950, B = 2800
        let (r, g1, g2, b) = (log[3200] >> 2, log[3050] >> 2, log[2950] >> 2, log[2800] >> 2);
        for (iw, ih) in [(64, 48), (70, 54), (68, 50)] {
            let out = decode(&plain(iw, ih), iw, ih, Mosaic::Rggb).unwrap();
            assert_eq!(out.len(), iw * ih);
            for y in 0..ih {
                for x in 0..iw {
                    let want = [[r, g1], [g2, b]][y % 2][x % 2];
                    assert_eq!(out[y * iw + x], want, "{iw}x{ih} at {x},{y}");
                }
            }
        }
        let out = decode(&plain(64, 48), 64, 48, Mosaic::Gbrg).unwrap();
        let (r, g1, g2, b) = (log[3200] >> 4, log[3050] >> 4, log[2950] >> 4, log[2800] >> 4);
        assert_eq!((out[0], out[1], out[64], out[65]), (g1, b, r, g2));
    }

    #[test]
    fn the_tags_after_the_header_may_carry_chunks_and_unknown_optional_elements() {
        let mut extra = seg(-0x4004, 2);
        extra.extend([7u8; 8]); // a small chunk (unique image identifier): 2 segments of payload
        extra.extend(seg(-0x6100, 1)); // a large metadata chunk: 1 segment
        extra.extend([9u8; 4]);
        extra.extend(seg(-0x2700, 123)); // a section marker: no payload
        extra.extend(seg(-777, 5)); // an unknown optional tag
        extra.extend(seg(0x4001, 1)); // a required inverse component permutation
        extra.extend([0u8; 4]);
        let ok = stream(64, 48, [3000, 2148, 1948, 2098], &extra, |_, s| s as i16);
        assert_eq!(decode(&ok, 64, 48, Mosaic::Rggb).unwrap(), decode(&plain(64, 48), 64, 48, Mosaic::Rggb).unwrap());
        // a chunk that claims more payload than the stream holds
        let mut cut = START_MARKER.to_vec();
        cut.extend(seg(-0x4004, 100));
        assert!(decode(&cut, 64, 48, Mosaic::Rggb).is_err());
        // a required tag nobody defined
        let mut unknown = START_MARKER.to_vec();
        unknown.extend(seg(0x0777, 0));
        assert!(decode(&unknown, 64, 48, Mosaic::Rggb).is_err());
        // the 16-bit inverse component transform is not supported
        let mut transform = START_MARKER.to_vec();
        transform.extend(seg(0x4003, 0));
        assert!(matches!(decode(&transform, 64, 48, Mosaic::Rggb), Err(RawError::Unsupported(_))));
    }

    #[test]
    fn a_truncated_stream_is_an_error_at_every_length() {
        let s = plain(64, 48);
        assert!(decode(&s, 64, 48, Mosaic::Rggb).is_ok());
        for n in 0..s.len() {
            assert!(decode(&s[..n], 64, 48, Mosaic::Rggb).is_err(), "{n} of {} bytes", s.len());
        }
    }

    #[test]
    fn damaged_streams_never_panic() {
        let s = plain(64, 48);
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..600 {
            let mut d = s.clone();
            for _ in 0..1 + next() % 4 {
                let i = (next() % d.len() as u64) as usize;
                d[i] = next() as u8;
            }
            let _ = decode(&d, 64, 48, Mosaic::Rggb);
        }
        // garbage after a valid marker, and a random stream
        for _ in 0..100 {
            let mut d = START_MARKER.to_vec();
            d.extend((0..(next() % 400)).map(|_| next() as u8));
            let _ = decode(&d, 64, 48, Mosaic::Rggb);
        }
    }

    #[test]
    fn the_header_must_describe_a_bayer_mosaic_the_dng_agrees_with() {
        let s = plain(64, 48);
        assert!(matches!(decode(&[], 64, 48, Mosaic::Rggb), Err(RawError::Corrupt(_))));
        assert!(matches!(decode(b"VC-4\0\0\0\0", 64, 48, Mosaic::Rggb), Err(RawError::Corrupt(_))));
        // the DNG's size must be the stream's
        assert!(matches!(decode(&s, 66, 48, Mosaic::Rggb), Err(RawError::Corrupt(_))));
        // three channels, a format other than raw, a pattern other than 2x2: described, not decoded
        for (at, value, what) in [(6usize, 3i16, "channels"), (18, 1, "format"), (22, 3, "pattern")] {
            let mut d = s.clone();
            d[at..at + 2].copy_from_slice(&value.to_be_bytes());
            assert!(matches!(decode(&d, 64, 48, Mosaic::Rggb), Err(RawError::Unsupported(_))), "{what}");
        }
        // odd mosaic sizes and images too small for three wavelet levels
        for (iw, ih) in [(63, 48), (64, 47), (0, 0), (32, 48), (64, 32)] {
            let small = stream(iw, ih, [0; 4], &[], |_, s| s as i16);
            assert!(matches!(decode(&small, iw, ih, Mosaic::Rggb), Err(RawError::Unsupported(_))), "{iw}x{ih}");
        }
        // 65534 x 65534 is far beyond the pixel limit, and no band of it is allocated
        let mut huge = START_MARKER.to_vec();
        for (t, v) in [(12, 4), (20, -2), (21, -2), (84, 4), (106, 2), (107, 2), (108, 1)] {
            huge.extend(seg(t, v));
        }
        huge.extend(seg(-0x6000, 1));
        huge.extend([0u8; 4]);
        assert!(matches!(decode(&huge, 65534, 65534, Mosaic::Rggb), Err(RawError::Limit(_))));
    }

    #[test]
    fn bad_subband_and_channel_numbers_are_errors() {
        let ok = |iw, ih, tag: &dyn Fn(usize, usize) -> i16| decode(&stream(iw, ih, [3000, 2148, 1948, 2098], &[], tag), iw, ih, Mosaic::Rggb);
        assert!(ok(64, 48, &|_, s| s as i16).is_ok());
        // the lowpass band twice
        assert!(ok(64, 48, &|c, s| if (c, s) == (0, 1) { 0 } else { s as i16 }).is_err());
        // a subband that does not exist
        assert!(ok(64, 48, &|c, s| if (c, s) == (0, 1) { 10 } else { s as i16 }).is_err());
        assert!(ok(64, 48, &|c, s| if (c, s) == (0, 1) { -1 } else { s as i16 }).is_err());
    }

    #[test]
    fn a_lowpass_precision_outside_the_defined_range_is_an_error() {
        for bad_precision in [0i16, 7, 33, -1] {
            let mut d = START_MARKER.to_vec();
            d.extend(seg(35, bad_precision));
            assert!(decode(&d, 64, 48, Mosaic::Rggb).is_err(), "{bad_precision}");
        }
    }

    #[test]
    fn a_codeblock_larger_than_the_stream_is_an_error() {
        let mut d = plain(64, 48);
        // the first codeblock's size field: the tag follows the header (9 tags), channel, subband and precision tags
        let at = START_MARKER.len() + 4 * (9 + 3) + 2;
        assert_eq!(&d[at - 2..at], (-0x6000i16).to_be_bytes());
        d[at..at + 2].copy_from_slice(&0x7fffi16.to_be_bytes());
        assert!(decode(&d, 64, 48, Mosaic::Rggb).is_err());
    }

    /// The one-strip DNG a GPR is: IFD 0 is the raw image, `Compression` 9, and the strip is the whole VC-5 stream.
    fn gpr(iw: usize, ih: usize, pattern: [u8; 4], stream: Vec<u8>) -> Vec<u8> {
        use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value, tags as t};
        let mut ifd = IfdBuilder::new();
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![iw as u32]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![ih as u32]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![32803]));
        ifd.set(t::COMPRESSION, Value::Short(vec![9]));
        ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        ifd.set(t::CFA_PATTERN_EP, Value::Byte(pattern.to_vec()));
        ifd.set(t::WHITE_LEVEL, Value::Long(vec![16383]));
        ifd.set_image(ImageData::Strips { rows_per_strip: ih as u32, strips: vec![stream] });
        TiffWriter::default().write(&[ifd]).unwrap()
    }

    #[test]
    fn a_gpr_dng_decodes_through_the_dng_reader() {
        use crate::{RawData, RawFormat};
        let log = log_curve();
        let file = gpr(64, 48, [0, 1, 1, 2], plain(64, 48));
        let raw = crate::decode(&file).unwrap();
        assert_eq!((raw.format, raw.width, raw.height, raw.cpp), (RawFormat::Dng, 64, 48, 1));
        let RawData::U16(v) = &raw.data else { panic!("integer samples expected") };
        assert_eq!((v[0], v[1], v[64], v[65]), (log[3200] >> 2, log[3050] >> 2, log[2950] >> 2, log[2800] >> 2));
        raw.validate().unwrap();
        // a GBRG file keeps 12 bits and its own cell order
        let file = gpr(64, 48, [1, 2, 0, 1], plain(64, 48));
        let raw = crate::decode(&file).unwrap();
        let RawData::U16(v) = &raw.data else { panic!("integer samples expected") };
        assert_eq!((v[0], v[1], v[64], v[65]), (log[3050] >> 4, log[2800] >> 4, log[3200] >> 4, log[2950] >> 4));
        // other mosaic orders are not GoPro's, a different size is a corrupt file
        assert!(matches!(crate::decode(&gpr(64, 48, [2, 1, 1, 0], plain(64, 48))), Err(RawError::Unsupported(_))));
        assert!(matches!(crate::decode(&gpr(64, 50, [0, 1, 1, 2], plain(64, 48))), Err(RawError::Corrupt(_))));
        // the header probe describes the file whatever its stream holds
        assert!(crate::probe_info(&file).is_some());
    }
}
