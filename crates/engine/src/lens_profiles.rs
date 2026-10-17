//! Lens profile corrections from a lens database the user supplies ([`lightcraft_lensdb`]).
//!
//! When a raw carries no lens correction of its own (DNG opcodes, or a camera correction a raw reader converts,
//! [`crate::files::embedded_lens`]), the lens named in its EXIF is looked up in a lensfun-format database: the
//! directory in `LIGHTCRAFT_LENSFUN_DIR` (e.g. lensfun's `data/db`, CC-BY-SA, which is never bundled with the app).
//! The profile is converted to the same [`lightcraft_develop::EmbeddedLens`] form, so "Enable Profile Corrections", its
//! amount sliders, the GPU path and DNG export treat both alike. Without the variable nothing changes.
use std::path::PathBuf;
use std::sync::OnceLock;

use lightcraft_develop::{EmbeddedLens, LensOverride};
use lightcraft_lensdb::{Camera, LensDb, Profile, Shot};

/// The environment variable naming the lens database directory.
pub const ENV: &str = "LIGHTCRAFT_LENSFUN_DIR";

/// The configured database, read once per process; `None` when none is configured or it holds no lenses.
pub fn database() -> Option<&'static LensDb> {
    static DB: OnceLock<Option<LensDb>> = OnceLock::new();
    DB.get_or_init(|| {
        let dir = PathBuf::from(std::env::var_os(ENV)?);
        match LensDb::load_dir(&dir) {
            Ok((db, errors)) => {
                for (file, e) in errors {
                    log::warn!("lens database: skipped {file}: {e}");
                }
                (!db.lenses.is_empty()).then_some(db)
            }
            Err(e) => {
                log::warn!("{e}");
                None
            }
        }
    })
    .as_ref()
}

/// The database profile for a photo with metadata `m`, for its `w × h` (unoriented, default-cropped) image,
/// re-expressed after `orientation`. `None` without a database, lens name, focal length or matching profile.
pub fn lookup(db: &LensDb, m: &lightcraft_meta::Metadata, w: f64, h: f64, orientation: lightcraft_geom::Orientation) -> Option<EmbeddedLens> {
    let camera = db.find_camera(m.make.as_deref().unwrap_or_default(), m.model.as_deref().unwrap_or_default());
    let focal = m.focal_length.filter(|f| f.is_finite() && *f > 0.0)?;
    let lens = db.find_lens(m.lens_make.as_deref(), m.lens_model.as_deref()?, camera)?;
    correction(lens, camera, m, focal, w, h, orientation)
}

/// The profile a lens picked by hand ([`LensOverride`]) gives the photo with metadata `m`: the same correction as
/// [`lookup`], from that lens instead of the one the EXIF names. `None` when the database has no such lens, the
/// photo has no focal length, or the profile has no usable correction at it.
pub fn lookup_override(
    db: &LensDb,
    pick: &LensOverride,
    m: &lightcraft_meta::Metadata,
    w: f64,
    h: f64,
    orientation: lightcraft_geom::Orientation,
) -> Option<EmbeddedLens> {
    let lens = db.find_exact(&pick.make, &pick.model)?;
    let camera = db.find_camera(m.make.as_deref().unwrap_or_default(), m.model.as_deref().unwrap_or_default());
    let focal = m.focal_length.filter(|f| f.is_finite() && *f > 0.0)?;
    correction(lens, camera, m, focal, w, h, orientation)
}

fn correction(
    lens: &Profile,
    camera: Option<&Camera>,
    m: &lightcraft_meta::Metadata,
    focal: f64,
    w: f64,
    h: f64,
    orientation: lightcraft_geom::Orientation,
) -> Option<EmbeddedLens> {
    let crop = frame_crop_factor(camera.map(|c| c.crop_factor), focal, m.focal_length_35mm);
    let shot = Shot {
        focal,
        aperture: m.f_number,
        distance: m.focus_distance.filter(|d| d.is_finite() && *d > 0.0),
        crop_factor: crop,
        width: w,
        height: h,
    };
    let corr = lens.correction(&shot)?;
    Some(lightcraft_pipeline::optics::reorient_lens(&corr, orientation, w, h))
}

/// The crop factor of the recorded frame, which scales the profile's radii: the body's (`camera`), else what the
/// 35 mm equivalent focal length implies. A body shooting a crop of its sensor (a full-frame camera in APS-C /
/// Super 35 mode, or switched there by an APS-C lens) records a larger 35 mm equivalent than its own crop factor
/// gives; the frame's corners are then nearer the optical axis, so that crop wins. Within 10 % of the body's, the
/// difference is the tag's whole-millimetre rounding and the body's value stands.
fn frame_crop_factor(camera: Option<f64>, focal: f64, focal_35mm: Option<f64>) -> Option<f64> {
    let frame = focal_35mm.map(|f35| f35 / focal).filter(|c| c.is_finite() && (0.1..=20.0).contains(c));
    match (camera, frame) {
        (Some(body), Some(frame)) if frame > body * 1.1 => Some(frame),
        (body, frame) => body.or(frame),
    }
}

/// The unoriented, default-cropped frame of `raw`, in pixels.
fn frame_of(raw: &lightcraft_raw::RawInfo) -> (f64, f64) {
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let (w, h) = if c.width == 0 || c.height == 0 { (raw.active_area.width, raw.active_area.height) } else { (c.width, c.height) };
    (w as f64, h as f64)
}

/// [`crate::files::embedded_lens`], else the database profile ([`lookup`]) for a raw with metadata `meta`, with the
/// camera's own maker-note tables of the file `bytes` ([`attach_camera`]).
pub fn for_raw(raw: &lightcraft_raw::RawInfo, bytes: &[u8], meta: impl FnOnce() -> lightcraft_meta::Metadata) -> Option<EmbeddedLens> {
    let profile = crate::files::embedded_lens(raw).or_else(|| {
        let db = database()?;
        let (w, h) = frame_of(raw);
        lookup(db, &meta(), w, h, raw.orientation)
    });
    attach_camera(profile, bytes)
}

/// `lens` with the vignetting and lateral CA tables the camera wrote into the raw file `bytes` ([`EmbeddedLens::camera`],
/// applied only by the opt-in "Use camera lens corrections"). A file with such tables and no profile gets a lens that
/// holds only them ([`EmbeddedLens::has_profile`] is then false); one without them keeps `lens` as it is.
pub fn attach_camera(lens: Option<EmbeddedLens>, bytes: &[u8]) -> Option<EmbeddedLens> {
    match lightcraft_raw::camera_correction(bytes) {
        Some(camera) => Some(EmbeddedLens { camera: Some(camera), ..lens.unwrap_or_default() }),
        None => lens,
    }
}

/// The lens data of the raw file `bytes`: the automatic result ([`for_raw`]), or with `pick` the picked database
/// profile in place of the file's own and the automatic match. `None` for a file that is not a raw, or has no
/// usable lens data.
pub fn for_bytes(bytes: &[u8], pick: Option<&LensOverride>) -> Option<EmbeddedLens> {
    let raw = lightcraft_raw::probe_info(bytes).ok()?;
    match pick {
        None => for_raw(&raw, bytes, || lightcraft_meta::extract(bytes)),
        Some(pick) => {
            let (w, h) = frame_of(&raw);
            attach_camera(Some(lookup_override(database()?, pick, &lightcraft_meta::extract(bytes), w, h, raw.orientation)?), bytes)
        }
    }
}

/// Every lens in the configured database as (maker, name), sorted: the manual picker's choices. Empty without a
/// database.
pub fn choices() -> &'static [(String, String)] {
    static CHOICES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    CHOICES.get_or_init(|| database().map(|db| db.choices().into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::Orientation;

    const DB: &str = r#"<lensdatabase>
      <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
      <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration>
          <distortion model="poly3" focal="18" k1="-0.03" />
          <vignetting model="pa" focal="18" aperture="3.5" distance="1000" k1="-0.4" k2="0" k3="0" />
        </calibration>
      </lens></lensdatabase>"#;

    fn meta() -> lightcraft_meta::Metadata {
        lightcraft_meta::Metadata {
            make: Some("Testcam".into()),
            model: Some("TC-1".into()),
            lens_model: Some("18-55mm F3.5-5.6".into()),
            focal_length: Some(18.0),
            f_number: Some(3.5),
            ..Default::default()
        }
    }

    #[test]
    fn camera_tables_attach_only_when_the_file_has_them() {
        let lens = EmbeddedLens { vignette: Some(Default::default()), ..Default::default() };
        // not a RAF (or any raw): the lens stays as it is, and nothing is invented for a photo without one
        assert_eq!(attach_camera(Some(lens), b"not a raw file"), Some(lens));
        assert_eq!(attach_camera(None, b"not a raw file"), None);
        assert_eq!(attach_camera(None, &[]), None);
        assert!(!EmbeddedLens::default().has_profile() && lens.has_profile());
    }

    #[test]
    fn exif_lens_is_found_and_converted() {
        let db = LensDb::parse(DB).unwrap();
        let lens = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
        let rotated = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Rotate90).unwrap();
        assert_eq!(rotated.warp.unwrap().center, lightcraft_geom::Point::new(0.5, 0.5));
        // no lens name, no focal length, an unknown lens: nothing
        assert!(lookup(&db, &lightcraft_meta::Metadata { lens_model: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        assert!(lookup(&db, &lightcraft_meta::Metadata { focal_length: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        let other = lightcraft_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..meta() };
        assert!(lookup(&db, &other, 6000.0, 4000.0, Orientation::Normal).is_none());
    }

    /// A lens picked by hand replaces the automatic match: the picked profile's correction, from its own calibration,
    /// whatever lens the EXIF names; an unknown pick, or none at the photo's focal length, gives nothing.
    #[test]
    fn a_picked_lens_overrides_the_exif_match() {
        let db = LensDb::parse(
            r#"<lensdatabase>
              <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
              <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
                <calibration><distortion model="poly3" focal="18" k1="-0.03" /></calibration></lens>
              <lens><maker>Othercam</maker><model>Othercam 18-35mm f/4</model><mount>T</mount><cropfactor>1.5</cropfactor>
                <calibration><distortion model="poly3" focal="18" k1="0.05" /></calibration></lens>
              <lens><maker>Telecam</maker><model>Telecam 70-200mm f/4</model><mount>T</mount><cropfactor>1.5</cropfactor>
                <calibration><distortion model="poly3" focal="70" k1="0.01" /><distortion model="poly3" focal="200" k1="0.02" /></calibration></lens>
              </lensdatabase>"#,
        )
        .unwrap();
        let pick = |make: &str, model: &str| LensOverride { make: make.into(), model: model.into() };
        let auto = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        let other = lookup_override(&db, &pick("Othercam", "Othercam 18-35mm f/4"), &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        assert_ne!(auto, other, "the picked lens, not the EXIF one");
        // picking the lens the EXIF names is the automatic result; case and an empty maker don't matter
        assert_eq!(lookup_override(&db, &pick("testcam", "TESTCAM 18-55MM F/3.5-5.6"), &meta(), 6000.0, 4000.0, Orientation::Normal), Some(auto));
        assert_eq!(lookup_override(&db, &pick("", "Othercam 18-35mm f/4"), &meta(), 6000.0, 4000.0, Orientation::Normal), Some(other));
        // it applies even where the EXIF names no lens at all
        let unnamed = lightcraft_meta::Metadata { lens_model: None, ..meta() };
        assert!(lookup(&db, &unnamed, 6000.0, 4000.0, Orientation::Normal).is_none());
        assert_eq!(lookup_override(&db, &pick("Othercam", "Othercam 18-35mm f/4"), &unnamed, 6000.0, 4000.0, Orientation::Normal), Some(other));
        // an unknown lens, no focal length, a degenerate frame: nothing
        assert!(lookup_override(&db, &pick("Othercam", "No such lens"), &meta(), 6000.0, 4000.0, Orientation::Normal).is_none());
        assert!(lookup_override(&db, &pick("", ""), &meta(), 6000.0, 4000.0, Orientation::Normal).is_none());
        let no_focal = lightcraft_meta::Metadata { focal_length: None, ..meta() };
        assert!(lookup_override(&db, &pick("Othercam", "Othercam 18-35mm f/4"), &no_focal, 6000.0, 4000.0, Orientation::Normal).is_none());
        assert!(lookup_override(&db, &pick("Othercam", "Othercam 18-35mm f/4"), &meta(), 0.0, 0.0, Orientation::Normal).is_none());
        // the automatic lookup stays the default: a photo with no pick is unchanged by the override code
        assert!(lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).is_some());
    }

    /// The focus distance read from the maker note picks the calibrated vignetting distance nearest it (in 1/d);
    /// without one, or at the farthest calibrated distance, the profile is the infinity one.
    #[test]
    fn focus_distance_selects_the_calibrated_vignetting_distance() {
        let db = LensDb::parse(
            r#"<lensdatabase>
              <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
              <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
                <calibration>
                  <vignetting model="pa" focal="18" aperture="3.5" distance="1000" k1="-0.4" k2="0" k3="0" />
                  <vignetting model="pa" focal="18" aperture="3.5" distance="0.5" k1="-0.1" k2="0" k3="0" />
                </calibration>
              </lens></lensdatabase>"#,
        )
        .unwrap();
        let at =
            |d: Option<f64>| lookup(&db, &lightcraft_meta::Metadata { focus_distance: d, ..meta() }, 6000.0, 4000.0, Orientation::Normal).unwrap();
        let far = at(None);
        let near = at(Some(0.55));
        assert_ne!(near, far, "a near focus differs from the farthest calibrated distance");
        assert_eq!(at(Some(0.5)), near, "0.55 m is closest to the 0.5 m calibration");
        assert_eq!(at(Some(900.0)), far, "far focus is the infinity calibration");
        // a hostile distance is ignored
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(at(Some(bad)), far, "{bad}");
        }
    }

    /// A full-frame body in APS-C / Super 35 mode (an ILCE-1M2 at 70 mm records 105 mm equivalent) frames only the
    /// middle of the image circle: the profile is applied at the frame's crop factor, not the body's, so the crop's
    /// corners don't get the full frame's corner correction.
    #[test]
    fn in_camera_crop_mode_uses_the_frames_crop_factor() {
        let db = LensDb::parse(
            r#"<lensdatabase>
              <camera><maker>Sony</maker><model>ILCE-1M2</model><mount>Sony E</mount><cropfactor>1</cropfactor></camera>
              <lens><maker>Sigma</maker><model>24-70mm F2.8 DG DN | Art 019</model><mount>Sony E</mount><cropfactor>1.0</cropfactor>
                <calibration>
                  <distortion model="ptlens" focal="70" a="0.0019947" b="0.0035263" c="0.0005016" />
                  <vignetting model="pa" focal="70" aperture="2.8" distance="1000" k1="-0.4" k2="0" k3="0" />
                </calibration>
              </lens></lensdatabase>"#,
        )
        .unwrap();
        let m = |f35: Option<f64>| lightcraft_meta::Metadata {
            make: Some("SONY".into()),
            model: Some("ILCE-1M2".into()),
            lens_model: Some("24-70mm F2.8 DG DN | Art 019".into()),
            focal_length: Some(70.0),
            focal_length_35mm: f35,
            f_number: Some(2.8),
            ..Default::default()
        };
        let at = |crop: f64, w: f64, h: f64| {
            let shot = Shot { focal: 70.0, aperture: Some(2.8), distance: None, crop_factor: Some(crop), width: w, height: h };
            db.lenses[0].correction(&shot).unwrap()
        };
        let aps_c = lookup(&db, &m(Some(105.0)), 5616.0, 3744.0, Orientation::Normal).unwrap();
        assert_eq!(aps_c, at(1.5, 5616.0, 3744.0));
        assert_ne!(aps_c, at(1.0, 5616.0, 3744.0), "the crop is not corrected as a full frame");
        // full frame, a 35 mm equivalent rounded off by a millimetre, or none at all: the body's crop factor
        for f35 in [Some(70.0), Some(71.0), None] {
            assert_eq!(lookup(&db, &m(f35), 8640.0, 5760.0, Orientation::Normal).unwrap(), at(1.0, 8640.0, 5760.0), "{f35:?}");
        }
        assert_eq!(frame_crop_factor(None, 70.0, Some(105.0)), Some(1.5), "no body in the database: the 35 mm equivalent's");
        assert_eq!(frame_crop_factor(Some(1.0), 0.0, Some(105.0)), Some(1.0), "a hostile focal length is ignored");
    }

    /// A synthetic grid recorded through the profile's barrel distortion comes out of the pipeline's geometry
    /// stage with straight lines.
    #[test]
    fn pipeline_straightens_a_distorted_grid() {
        let db = LensDb::parse(DB).unwrap();
        let profile = &db.lenses[0];
        let (w, h) = (600usize, 400usize);
        let lens = lookup(&db, &meta(), w as f64, h as f64, Orientation::Normal).unwrap();
        // record the grid: each sensor pixel sees the scene point whose distorted position it is
        let n = h as f64 / 2.0;
        let undistort = |x: f64, y: f64| {
            let (dx, dy) = ((x - w as f64 / 2.0) / n, (y - h as f64 / 2.0) / n);
            let rd = dx.hypot(dy);
            let (mut lo, mut hi) = (0.0, 3.0);
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if mid * profile.distortion_ratio(18.0, mid) < rd {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let s = if rd > 0.0 { lo / rd } else { 1.0 };
            (w as f64 / 2.0 + dx * s * n, h as f64 / 2.0 + dy * s * n)
        };
        // horizontal scene lines every 50 px (2 px wide, dark on light)
        let line = |v: f64| if v.rem_euclid(50.0) < 2.0 { 0.02f32 } else { 0.8 };
        let src = lightcraft_raster::Rgb32f::from_fn(w, h, |x, y| {
            let (_, sy) = undistort(x as f64 + 0.5, y as f64 + 0.5);
            [line(sy); 3]
        });
        // the dark row nearest y = 51 in each column, sub-pixel (centroid of darkness), as rendered
        let line_rows = |img: &lightcraft_raster::Rgba8| -> Vec<f64> {
            (20..w - 20)
                .step_by(10)
                .map(|x| {
                    let (mut sw, mut swy) = (0.0, 0.0);
                    for y in 35..70 {
                        let d = 255.0 - img.get(x, y)[1] as f64;
                        if d > 60.0 {
                            sw += d;
                            swy += d * (y as f64 + 0.5);
                        }
                    }
                    swy / sw.max(1e-9)
                })
                .collect()
        };
        let spread = |rows: &[f64]| rows.iter().cloned().fold(f64::MIN, f64::max) - rows.iter().cloned().fold(f64::MAX, f64::min);
        let req = lightcraft_pipeline::RenderRequest::fit(w, h);
        let mut s = lightcraft_develop::DevelopSettings::default();
        let info = lightcraft_pipeline::SourceInfo { lens: Some(lens), ..Default::default() };
        let before = spread(&line_rows(&lightcraft_pipeline::render(&src, &info, &s, &req).image));
        s.optics.lens_profile = true;
        let after = spread(&line_rows(&lightcraft_pipeline::render(&src, &info, &s, &req).image));
        assert!(before > 2.0, "the recorded line is curved: {before} px");
        // 8-bit output and a 2 px line: the centroid measurement itself is good to about a pixel
        assert!(after < 1.0 && after < before / 8.0, "corrected line bent by {after} px (uncorrected {before})");
    }
}
