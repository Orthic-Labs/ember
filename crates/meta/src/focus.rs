//! Focus distance (metres) from the Exif `SubjectDistance` tag and from maker notes.
//!
//! Lens profiles with a focus-distance dimension (lensfun vignetting) need it. Cameras record it in vendor
//! specific ways; the layouts here follow the public tag documentation of each vendor's maker note, and a value is
//! accepted only when it is a plausible finite distance (zero, "infinity" sentinels and absurd values give `None`,
//! which the lens lookup treats as infinity).
//!
//! | Source | Tag | Unit |
//! |---|---|---|
//! | Exif `SubjectDistance` | `0x9206` (RATIONAL) | metres |
//! | Canon maker note | `ShotInfo` (`0x0004`), entries 19 (`FocusDistanceUpper`) and 20 (`FocusDistanceLower`) | 0.01 m, 65535 = infinity |
//! | Olympus (new style) maker note | `FocusInfo` sub-IFD (`0x2050`), `FocusDistance` (`0x0305`, RATIONAL) | mm |
//!
//! Makers whose focus distance is encrypted or not documented to a confident level (Nikon `LensData`, Sony,
//! Fujifilm, Pentax, Panasonic) are not read from the maker note; they fall back to `SubjectDistance` when present.

use lightcraft_tiff::makernote::{MakerNote, MakerNoteKind, parse_makernote};
use lightcraft_tiff::{ParseOptions, Tiff, parse_ifd_at, tags as t};

/// Exif `SubjectDistance`.
const SUBJECT_DISTANCE: u16 = 0x9206;
/// Canon maker note `ShotInfo`.
const CANON_SHOT_INFO: u16 = 0x0004;
/// Olympus maker note `FocusInfo` sub-IFD pointer.
const OLYMPUS_FOCUS_INFO: u16 = 0x2050;
/// Olympus `FocusInfo` `FocusDistance`, millimetres.
const OLYMPUS_FOCUS_DISTANCE: u16 = 0x0305;
/// A distance at or beyond this many metres is a sentinel for infinity, not a measurement.
const MAX_METRES: f64 = 1000.0;

fn plausible(metres: f64) -> Option<f64> {
    (metres.is_finite() && metres > 0.0 && metres < MAX_METRES).then_some(metres)
}

/// The Exif `SubjectDistance` in metres, when it is a real measurement (0 = unknown, 0xFFFFFFFF = infinity).
pub(crate) fn subject_distance(tiff: &Tiff) -> Option<f64> {
    plausible(tiff.exif()?.f64(SUBJECT_DISTANCE)?)
}

/// The focus distance in metres from the maker note, for the makers documented above. `data` is the buffer `tiff`
/// was parsed from (maker-note offsets are absolute positions in it).
pub(crate) fn maker_note_distance(tiff: &Tiff, data: &[u8]) -> Option<f64> {
    let entry = tiff.exif()?.get(t::MAKER_NOTE)?;
    let make = tiff.ifds.first().and_then(|i| i.string(t::MAKE)).unwrap_or_default();
    let note = parse_makernote(data, entry.offset, entry.count() as u64, tiff.order, &make)?;
    match note.kind {
        MakerNoteKind::Plain if make.trim_start().to_ascii_uppercase().starts_with("CANON") => canon(&note),
        MakerNoteKind::OlympusNew => olympus(data, &note),
        _ => None,
    }
}

/// Canon `ShotInfo`: the focus range's far (`FocusDistanceUpper`) and near (`FocusDistanceLower`) ends in
/// hundredths of a metre. The upper end must be set (a camera that doesn't measure leaves both 0); the distance
/// is the middle of the range, or the upper end alone when the lower one is missing.
fn canon(note: &MakerNote) -> Option<f64> {
    let info = note.ifd.f64s(CANON_SHOT_INFO)?;
    let end = |i: usize| info.get(i).copied().filter(|v| *v > 0.0 && *v < 65535.0);
    let upper = end(19)?;
    let centimetres = match end(20) {
        Some(lower) => (upper + lower) / 2.0,
        None => upper,
    };
    plausible(centimetres / 100.0)
}

/// Olympus `FocusInfo` (a sub-IFD whose offset is relative to the note start): `FocusDistance` in millimetres.
fn olympus(data: &[u8], note: &MakerNote) -> Option<f64> {
    let at = note.base.checked_add(note.ifd.u64(OLYMPUS_FOCUS_INFO)?)?;
    let opts = ParseOptions { max_ifds: 2, max_depth: 1, ..Default::default() };
    let (info, _) = parse_ifd_at(data, at, note.order, note.base, false, &opts).ok()?;
    plausible(info.f64(OLYMPUS_FOCUS_DISTANCE)? / 1000.0)
}

#[cfg(test)]
mod tests {
    use crate::{Metadata, read_exif};
    use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};

    /// A TIFF whose Exif IFD holds `note` (built by `build` from the note's absolute position in the file, which
    /// is the same for any note of the same length) as its maker note, plus an optional SubjectDistance.
    fn file(order: ByteOrder, make: &str, subject: Option<(u32, u32)>, build: impl Fn(u32) -> Vec<u8>) -> Vec<u8> {
        let write = |note: Vec<u8>| {
            let mut exif = IfdBuilder::new();
            exif.set(t::MAKER_NOTE, Value::Undefined(note));
            if let Some(s) = subject {
                exif.set(super::SUBJECT_DISTANCE, Value::Rational(vec![s]));
            }
            let mut ifd0 = IfdBuilder::new();
            ifd0.set(t::MAKE, Value::Ascii(make.into()));
            ifd0.set_child(t::EXIF_IFD, exif);
            TiffWriter { order, bigtiff: false }.write(&[ifd0]).unwrap()
        };
        let first = write(build(0));
        let tiff = lightcraft_tiff::Tiff::parse(&first).unwrap();
        let at = tiff.exif().unwrap().get(t::MAKER_NOTE).unwrap().offset as u32;
        write(build(at))
    }

    /// A Canon-style note: a plain IFD with one `ShotInfo` SHORT array whose values sit after the IFD and are
    /// addressed from the TIFF header (`at` is where the note starts).
    fn canon_note(order: ByteOrder, at: u32, upper: u16, lower: u16) -> Vec<u8> {
        let mut shot = vec![0u16; 21];
        shot[0] = 42;
        shot[19] = upper;
        shot[20] = lower;
        let mut n = Vec::new();
        order.put_u16(&mut n, 1);
        order.put_u16(&mut n, super::CANON_SHOT_INFO);
        order.put_u16(&mut n, 3);
        order.put_u32(&mut n, shot.len() as u32);
        order.put_u32(&mut n, at + 18);
        order.put_u32(&mut n, 0);
        for v in shot {
            order.put_u16(&mut n, v);
        }
        n
    }

    /// An Olympus new-style note with a `FocusInfo` sub-IFD holding `FocusDistance` = `mm` / 1.
    fn olympus_note(order: ByteOrder, mm: u32) -> Vec<u8> {
        let mut n = b"OLYMPUS\0".to_vec();
        n.extend_from_slice(if order == ByteOrder::Little { b"II" } else { b"MM" });
        order.put_u16(&mut n, 3);
        // main IFD at 12: one LONG entry pointing at the sub-IFD at 30
        order.put_u16(&mut n, 1);
        order.put_u16(&mut n, super::OLYMPUS_FOCUS_INFO);
        order.put_u16(&mut n, 4);
        order.put_u32(&mut n, 1);
        order.put_u32(&mut n, 30);
        order.put_u32(&mut n, 0);
        // sub-IFD at 30: one RATIONAL entry stored at 48
        order.put_u16(&mut n, 1);
        order.put_u16(&mut n, super::OLYMPUS_FOCUS_DISTANCE);
        order.put_u16(&mut n, 5);
        order.put_u32(&mut n, 1);
        order.put_u32(&mut n, 48);
        order.put_u32(&mut n, 0);
        order.put_u32(&mut n, mm);
        order.put_u32(&mut n, 1);
        n
    }

    fn distance(bytes: &[u8]) -> Option<f64> {
        read_exif(bytes).focus_distance
    }

    #[test]
    fn canon_shot_info_gives_the_middle_of_the_range() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            // 1.50 m to 2.50 m
            let f = file(order, "Canon", None, |at| canon_note(order, at, 250, 150));
            let d = distance(&f).unwrap();
            assert!((d - 2.0).abs() < 1e-9, "{d}");
            // only the upper end
            let f = file(order, "Canon", None, |at| canon_note(order, at, 120, 0));
            assert!((distance(&f).unwrap() - 1.2).abs() < 1e-9);
        }
    }

    #[test]
    fn canon_unmeasured_and_infinite_are_unknown() {
        let o = ByteOrder::Little;
        for (upper, lower) in [(0, 0), (65535, 65535), (0, 300)] {
            let f = file(o, "Canon", None, |at| canon_note(o, at, upper, lower));
            assert_eq!(distance(&f), None, "{upper} {lower}");
        }
    }

    #[test]
    fn olympus_focus_info_is_in_millimetres() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let f = file(order, "OLYMPUS CORPORATION", None, |_| olympus_note(order, 1500));
            assert!((distance(&f).unwrap() - 1.5).abs() < 1e-9);
            // 0 and the 32-bit "infinity" sentinel are not distances
            for mm in [0, u32::MAX] {
                let f = file(order, "OLYMPUS CORPORATION", None, |_| olympus_note(order, mm));
                assert_eq!(distance(&f), None, "{mm}");
            }
        }
    }

    #[test]
    fn subject_distance_is_the_fallback() {
        let o = ByteOrder::Little;
        // a maker note we don't read: the standard tag
        let f = file(o, "Test", Some((7, 2)), |_| vec![0xff; 24]);
        assert_eq!(distance(&f), Some(3.5));
        // the maker note wins over it
        let f = file(o, "Canon", Some((7, 2)), |at| canon_note(o, at, 100, 100));
        assert!((distance(&f).unwrap() - 1.0).abs() < 1e-9);
        // 0 = unknown, 0xFFFFFFFF = infinity
        for s in [(0, 1), (u32::MAX, 1), (1, 0)] {
            let f = file(o, "Test", Some(s), |_| vec![0xff; 24]);
            assert_eq!(distance(&f), None, "{s:?}");
        }
    }

    #[test]
    fn malformed_notes_give_none() {
        let o = ByteOrder::Little;
        // a Canon note that is not an IFD, a truncated Olympus note, an Olympus pointer far outside the file
        let f = file(o, "Canon", None, |_| vec![0xff; 40]);
        assert_eq!(distance(&f), None);
        let f = file(o, "OLYMPUS", None, |_| b"OLYMPUS\0II\x03\0\x01".to_vec());
        assert_eq!(distance(&f), None);
        let f = file(o, "OLYMPUS", None, |_| {
            let mut n = olympus_note(o, 1500);
            n[22..26].copy_from_slice(&u32::MAX.to_le_bytes());
            n
        });
        assert_eq!(distance(&f), None);
        // a Canon ShotInfo too short to hold the entries
        let f = file(o, "Canon", None, |at| {
            let mut n = canon_note(o, at, 100, 100);
            n[6..10].copy_from_slice(&5u32.to_le_bytes());
            n
        });
        assert_eq!(distance(&f), None);
        assert_eq!(read_exif(b"garbage"), Metadata::default());
    }
}
