//! Auto-adjust in two phases (#192).
//!
//! 1. **Measure** ([`measure_auto`]): the decoded source pixels, the source's facts and the photo's
//!    settings in, Auto's values out. It needs no [`Session`], so a host can run it where the
//!    pixels are, such as the web build's render workers.
//! 2. **Apply** (`develop.applyAutoValues`, see `cmd/develop.rs`): the values go into the photo's
//!    settings on the thread that owns the session, as one undo step. It carries the [`auto_basis`]
//!    the measure was made against and refuses when the photo's settings, face regions or source
//!    changed since: the values were measured for a photo that is no longer there.
//!
//! Native and desktop keep calling `develop.auto*`, which composes the two in place.

use std::sync::Arc;

use lightcraft_catalog::{PhotoId, Source};
use lightcraft_develop::{DevelopSettings, EmbeddedLens};
use lightcraft_pipeline::SourceInfo;
use lightcraft_pipeline::auto::regions::{RegionAuto, region_auto_with};
use lightcraft_pipeline::auto::{AutoHints, AutoTone, FaceBox, auto_tone_with};
use lightcraft_preview::{Hash128, Hasher128};
use lightcraft_raster::Rgb32f;

use crate::Session;
use crate::media::content_key;

/// What Auto finds for a photo from its decoded source (regions on: the subject, background and
/// sky too). Reads only; nothing is written.
pub fn measure_auto(src: &Rgb32f, info: &SourceInfo, d: &DevelopSettings, faces: &[FaceBox], regions: bool) -> RegionAuto {
    let hints = AutoHints { faces, clipped_share: None };
    if regions { region_auto_with(src, info, d, &hints) } else { RegionAuto::global_only(auto_tone_with(src, info, d, &hints)) }
}

/// A lens the user picked in place of the file's own: `Some(lens)` replaces the source's (even
/// with none), `None` leaves it as the decoder reported it.
pub fn with_lens_pick(mut info: SourceInfo, pick: Option<Option<EmbeddedLens>>) -> SourceInfo {
    if let Some(lens) = pick {
        info.lens = lens;
    }
    info
}

/// The controls Auto sets (`develop.autoControl`).
pub const AUTO_CONTROLS: [&str; 8] = [
    "light.exposure",
    "light.contrast",
    "light.highlights",
    "light.shadows",
    "light.whites",
    "light.blacks",
    "color.vibrance",
    "color.saturation",
];

/// Whether Auto sets control `id` (`develop.autoControl`).
pub fn is_auto_control(id: &str) -> bool {
    AUTO_CONTROLS.contains(&id)
}

/// Auto's value for one control, `None` for a control Auto does not set.
pub fn control_value(a: &AutoTone, id: &str) -> Option<f64> {
    Some(match id {
        "light.exposure" => a.exposure,
        "light.contrast" => a.contrast,
        "light.highlights" => a.highlights,
        "light.shadows" => a.shadows,
        "light.whites" => a.whites,
        "light.blacks" => a.blacks,
        "color.vibrance" => a.vibrance,
        "color.saturation" => a.saturation,
        _ => return None,
    })
}

/// The cached face regions that meter Auto's key (a Detect Faces run, or the file's own).
pub fn auto_faces(s: &Session, id: PhotoId) -> Vec<FaceBox> {
    s.catalog.photo(id).map(|ph| crate::cmd::face_detect::auto_face_boxes(&ph.meta.regions)).unwrap_or_default()
}

/// Identifies what a measure of photo `id` is made against: the photo, its source, its settings and
/// its face regions. `None` for a photo that does not exist.
pub fn auto_basis(s: &Session, id: PhotoId) -> Option<String> {
    let p = s.catalog.photo(id)?;
    let mut h = Hasher128::new();
    h.str(&content_key(p)).u64(id.0).u64(p.develop.hash64());
    for f in auto_faces(s, id) {
        for v in [f.x0, f.y0, f.x1, f.y1] {
            h.u64(u64::from(v.to_bits()));
        }
    }
    Some(h.finish().to_string())
}

/// Everything a host needs to measure Auto for one photo somewhere else: where its pixels come
/// from, the facts its settings are read against, and what to measure.
#[derive(Clone, Debug)]
pub struct AutoInput {
    pub photo: PhotoId,
    /// The subject, background and sky get their own masks too.
    pub regions: bool,
    /// One slider only (`develop.autoControl`); the values are the same, `apply` takes this one.
    pub control: Option<String>,
    pub origin: Source,
    /// Identity of the decoded source (its content and how it was made).
    pub source_identity: Hash128,
    /// The facts the catalog's header gives; the decoder's, when it has them, replace these.
    pub info: SourceInfo,
    pub lens_pick: Option<Option<EmbeddedLens>>,
    pub settings: Arc<DevelopSettings>,
    pub faces: Vec<FaceBox>,
    /// [`auto_basis`] when the input was made.
    pub basis: String,
}

/// The input for measuring photo `id`, or why there is none.
pub fn auto_input(s: &Session, id: PhotoId, regions: bool, control: Option<String>) -> Result<AutoInput, String> {
    let p = s.catalog.photo(id).ok_or("no such photo")?;
    let basis = auto_basis(s, id).ok_or("no such photo")?;
    Ok(AutoInput {
        photo: id,
        regions,
        control,
        origin: p.source.clone(),
        source_identity: s.media.source_identity(p),
        info: crate::media::source_info(p),
        lens_pick: p.develop.lens_override.is_some().then_some(p.embedded_lens),
        settings: p.develop.clone(),
        faces: auto_faces(s, id),
        basis,
    })
}
