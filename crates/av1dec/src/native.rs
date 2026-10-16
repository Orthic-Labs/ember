//! The rav1d FFI. This is the only module in the crate with `unsafe`; the safe API is in `lib.rs`.
#![allow(unsafe_code)]

use crate::{Colour, Error, Layout, MAX_DIMENSION, MAX_PIXELS, Picture, Plane};
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dLogger, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422, DAV1D_PIXEL_LAYOUT_I444, Dav1dSequenceHeader,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data,
};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// An open decoder context, closed exactly once on drop.
struct Context(Option<Dav1dContext>);

impl Context {
    fn open() -> Result<Context, Error> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: `settings` is a live, aligned local that is valid for writes of one
        // `Dav1dSettings`; `dav1d_default_settings` fully initialises it without reading it.
        unsafe { dav1d_default_settings(NonNull::from(&mut settings).cast()) };
        // SAFETY: the call above initialised every field.
        let mut settings = unsafe { settings.assume_init() };
        // One thread, no frame delay: the picture is decoded inside `dav1d_send_data`, no worker
        // threads are spawned, and nothing is left running when the context is closed.
        settings.n_threads = 1;
        settings.max_frame_delay = 1;
        // Film grain applies (it is part of the picture); only the highest spatial layer is output.
        settings.apply_grain = 1;
        settings.operating_point = 0;
        settings.all_layers = 0;
        // The decoder refuses frames over this many pixels (2^28 fits a `c_uint`).
        settings.frame_size_limit = MAX_PIXELS as std::ffi::c_uint;
        // SAFETY: with no callback the logger is never invoked (`Dav1dLogger::new` only requires a
        // given callback to be callable). This silences the decoder's stderr logging.
        settings.logger = unsafe { Dav1dLogger::new(None, None) };

        let mut ctx = Context(None);
        // SAFETY: both pointers come from live locals valid for reads and writes. On success
        // `ctx.0` holds a context from `dav1d_open`, which `Drop` closes exactly once; on failure
        // `dav1d_open` leaves it `None`, which `dav1d_close` ignores.
        let r = unsafe { dav1d_open(Some(NonNull::from(&mut ctx.0)), Some(NonNull::from(&mut settings))) };
        if r.0 != 0 || ctx.0.is_none() {
            return Err(Error::Decoder(r.0));
        }
        Ok(ctx)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: `self.0` is `None` or a context from `dav1d_open` not yet closed; `dav1d_close`
        // takes it out of the option, so it cannot be closed twice. The pointer is to a live field.
        unsafe { dav1d_close(Some(NonNull::from(&mut self.0))) };
    }
}

/// A decoder input buffer, released on drop (a no-op once the decoder consumed it).
struct Input(Dav1dData);

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: the pointer is to a live, initialised `Dav1dData` (default, created or consumed).
        unsafe { dav1d_data_unref(Some(NonNull::from(&mut self.0))) };
    }
}

/// A decoded picture handle, unreferenced on drop (a no-op for an empty one).
struct Pic(Dav1dPicture);

impl Drop for Pic {
    fn drop(&mut self) {
        // SAFETY: the pointer is to a live, initialised `Dav1dPicture` (default or from
        // `dav1d_get_picture`); unref releases its buffers and resets it, and is idempotent.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.0))) };
    }
}

pub(crate) fn decode(data: &[u8]) -> Result<Picture, Error> {
    if data.is_empty() {
        return Err(Error::Empty);
    }
    // Drop order is the reverse of declaration: the picture, then the input, then the context.
    let ctx = Context::open()?;
    let mut input = Input(Dav1dData::default());
    // SAFETY: the pointer is to a live `Dav1dData` valid for writes; on success it owns a buffer
    // of `data.len()` bytes that `input` releases.
    let ptr = unsafe { dav1d_data_create(Some(NonNull::from(&mut input.0)), data.len()) };
    if ptr.is_null() {
        return Err(Error::Malformed("out of memory for the AV1 input"));
    }
    // SAFETY: `dav1d_data_create` returned a non-null pointer to `data.len()` writable bytes,
    // exclusively owned by `input`, and nothing else touches them until `dav1d_send_data`.
    unsafe { std::slice::from_raw_parts_mut(ptr, data.len()) }.copy_from_slice(data);

    // SAFETY: `ctx.0` is an open context; the pointer is to a live `Dav1dData` valid for reads
    // and writes whose buffer came from `dav1d_data_create`.
    let r = unsafe { dav1d_send_data(ctx.0, Some(NonNull::from(&mut input.0))) };
    if r.0 != 0 {
        return Err(Error::Decoder(r.0));
    }

    let mut pic = Pic(Dav1dPicture::default());
    // SAFETY: `ctx.0` is an open context; the pointer is to a live `Dav1dPicture` valid for writes
    // (it holds no resources yet, so overwriting it leaks nothing).
    let r = unsafe { dav1d_get_picture(ctx.0, Some(NonNull::from(&mut pic.0))) };
    if r.0 != 0 {
        // A still picture is decoded inside `dav1d_send_data`: "try again" here means the stream
        // ended before a complete frame (truncated); any other code is a decode error.
        return Err(Error::Decoder(r.0));
    }
    extract(&pic.0)
}

/// Copy a decoded picture out into owned planes, validating everything the decoder reported.
fn extract(pic: &Dav1dPicture) -> Result<Picture, Error> {
    let w = u32::try_from(pic.p.w).map_err(|_| Error::Malformed("negative width"))?;
    let h = u32::try_from(pic.p.h).map_err(|_| Error::Malformed("negative height"))?;
    if w == 0 || h == 0 {
        return Err(Error::Malformed("empty picture"));
    }
    if w > MAX_DIMENSION || h > MAX_DIMENSION || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(Error::TooLarge(u64::from(w), u64::from(h)));
    }
    let bit_depth: u8 = match pic.p.bpc {
        8 => 8,
        10 => 10,
        12 => 12,
        _ => return Err(Error::Unsupported("bit depth")),
    };
    let bytes = if bit_depth == 8 { 1 } else { 2 };
    let layout = match pic.p.layout {
        DAV1D_PIXEL_LAYOUT_I400 => Layout::I400,
        DAV1D_PIXEL_LAYOUT_I420 => Layout::I420,
        DAV1D_PIXEL_LAYOUT_I422 => Layout::I422,
        DAV1D_PIXEL_LAYOUT_I444 => Layout::I444,
        _ => return Err(Error::Unsupported("chroma layout")),
    };
    let (width, height) = (w as usize, h as usize);
    let mut planes = Vec::with_capacity(3);
    // SAFETY: the picture is alive (the caller holds the `Pic` guard) and `data[0]` / `stride[0]`
    // describe its luma plane as the decoder allocated it.
    planes.push(unsafe { copy_plane(pic.data[0], pic.stride[0], width, height, bytes) }?);
    if let Some((sx, sy)) = layout.shifts() {
        let cw = (width + sx as usize) >> sx;
        let ch = (height + sy as usize) >> sy;
        // SAFETY: as above for the Cb plane; both chroma planes share the second stride.
        planes.push(unsafe { copy_plane(pic.data[1], pic.stride[1], cw, ch, bytes) }?);
        // SAFETY: as above for the Cr plane.
        planes.push(unsafe { copy_plane(pic.data[2], pic.stride[1], cw, ch, bytes) }?);
    }
    let colour = pic.seq_hdr.map(|hdr| {
        // SAFETY: the picture holds a reference to its sequence header, so the pointer is valid
        // for as long as the picture is alive, and nothing mutates it.
        let s: &Dav1dSequenceHeader = unsafe { hdr.as_ref() };
        let code = |v: std::ffi::c_uint| u8::try_from(v).unwrap_or(2);
        Colour {
            primaries: code(s.pri),
            transfer: code(s.trc),
            matrix: code(s.mtrx),
            full_range: s.color_range != 0,
            described: s.color_description_present != 0,
        }
    });
    Ok(Picture { width: w, height: h, bit_depth, layout, planes, colour })
}

/// Copy `height` rows of `width` samples (`bytes` per sample, native endian) into a tight `Vec<u16>`.
///
/// # Safety
///
/// `base` must be the first byte of a plane of a live decoded picture whose rows start `stride`
/// bytes apart, with at least `width * bytes` readable bytes in each of the `height` rows.
unsafe fn copy_plane(base: Option<NonNull<c_void>>, stride: isize, width: usize, height: usize, bytes: usize) -> Result<Plane, Error> {
    let base = base.ok_or(Error::Malformed("missing picture plane"))?;
    let row_bytes = width.checked_mul(bytes).ok_or(Error::Malformed("plane too wide"))?;
    let count = width.checked_mul(height).ok_or(Error::Malformed("plane too large"))?;
    let stride_u = usize::try_from(stride).map_err(|_| Error::Malformed("negative plane stride"))?;
    if stride_u < row_bytes || stride_u == 0 {
        return Err(Error::Malformed("plane stride shorter than a row"));
    }
    let mut data: Vec<u16> = Vec::new();
    data.try_reserve_exact(count).map_err(|_| Error::Malformed("out of memory for a picture plane"))?;
    for y in 0..height {
        let offset = y.checked_mul(stride_u).and_then(|o| isize::try_from(o).ok()).ok_or(Error::Malformed("plane offset overflow"))?;
        // SAFETY: by this function's contract row `y < height` starts `y * stride` bytes after
        // `base` and has `row_bytes` readable bytes; the offset fits an `isize` (checked above).
        let row = unsafe { std::slice::from_raw_parts(base.as_ptr().cast::<u8>().cast_const().offset(offset), row_bytes) };
        if bytes == 1 {
            data.extend(row.iter().map(|&v| u16::from(v)));
        } else {
            data.extend(row.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])));
        }
    }
    Ok(Plane { width, height, data })
}
