//! The segmentation model behind AI masks, whichever is installed: MobileSAM (the default: ungated,
//! Apache-2.0, 40.6 MB; clicks and boxes) or SAM 3 (optional: gated, 3.4 GB; also text).
//!
//! A folder holding the complete SAM 3 checkpoint loads SAM 3 (it can do everything MobileSAM does and
//! text prompts besides: someone who put it there chose it); otherwise a folder holding the MobileSAM
//! checkpoint loads MobileSAM. Describe (text) needs a grounding model, which MobileSAM is not: with
//! MobileSAM alone it answers [`Error::NeedsGrounding`] (issue #221 adds the grounding step).

use std::path::Path;

use crate::mobilesam::{MobileEncoded, MobileSam};
use crate::{Click, Encoded, Error, Result, Sam3, mobilesam_prep, remote};

/// Which model a [`Model`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    MobileSam,
    Sam3,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::MobileSam => "MobileSAM",
            Kind::Sam3 => "SAM 3",
        }
    }
}

/// A loaded model.
pub enum Model {
    Mobile(Box<MobileSam>),
    Sam3(Box<Sam3>),
}

/// A photo run through a [`Model`]'s image encoder.
pub enum Scene {
    Mobile(Box<MobileEncoded>),
    Sam3(Box<Encoded>),
}

/// A mask: `side²` logits over the whole photo (stretched to the square) and the model's confidence (0..1).
#[derive(Clone, Debug)]
pub struct Mask {
    pub logits: Vec<f32>,
    pub side: usize,
    pub score: f32,
}

/// Which model `dir` holds, if any (SAM 3 first).
pub fn installed_kind(dir: &Path) -> Option<Kind> {
    if crate::is_model_dir(dir) {
        Some(Kind::Sam3)
    } else if crate::is_mobilesam_dir(dir) {
        Some(Kind::MobileSam)
    } else {
        None
    }
}

fn other_scene() -> Error {
    Error::Model("the photo was analyzed by a different model; analyze it again".into())
}

impl Model {
    /// Load whichever model `dir` holds ([`Error::Missing`] when neither).
    pub fn load(dir: &Path) -> Result<Self> {
        match installed_kind(dir) {
            Some(Kind::Sam3) => Ok(Model::Sam3(Box::new(Sam3::load(dir)?))),
            Some(Kind::MobileSam) => Ok(Model::Mobile(Box::new(MobileSam::load(dir)?))),
            None => Err(Error::Missing(dir.to_path_buf())),
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Model::Mobile(_) => Kind::MobileSam,
            Model::Sam3(_) => Kind::Sam3,
        }
    }

    /// Whether Describe (text prompts) works with this model.
    pub fn supports_text(&self) -> bool {
        matches!(self, Model::Sam3(_))
    }

    /// Run the image encoder on an RGB photo (8-bit, row-major, `w × h`), and prepare what clicks need.
    pub fn encode(&mut self, rgb: &[u8], w: usize, h: usize) -> Result<Scene> {
        match self {
            Model::Mobile(m) => Ok(Scene::Mobile(Box::new(m.encode(rgb, w, h)?))),
            Model::Sam3(m) => {
                let mut enc = m.encode(rgb, w, h)?;
                m.prepare_clicks(&mut enc)?;
                Ok(Scene::Sam3(Box::new(enc)))
            }
        }
    }

    /// Segment the object `clicks` pick (positions normalized to the photo, 0..1).
    pub fn segment_clicks(&mut self, scene: &mut Scene, clicks: &[Click]) -> Result<Mask> {
        if clicks.is_empty() {
            return Err(Error::Model("no clicks".into()));
        }
        match (self, scene) {
            (Model::Mobile(m), Scene::Mobile(e)) => {
                let p = m.segment(e, clicks, None)?;
                Ok(Mask { logits: p.logits, side: mobilesam_prep::OUTPUT_SIDE, score: p.score })
            }
            (Model::Sam3(m), Scene::Sam3(e)) => {
                let p = m.segment_clicks(e, clicks)?;
                Ok(Mask { logits: p.logits, side: crate::MASK_SIDE, score: p.score })
            }
            _ => Err(other_scene()),
        }
    }

    /// Segment the object inside the box `[x0, y0, x1, y1]` (normalized to the photo), refined by
    /// `clicks` if any. MobileSAM only: SAM 3's prompt path here is clicks and text.
    pub fn segment_box(&mut self, scene: &mut Scene, bx: [f32; 4], clicks: &[Click]) -> Result<Mask> {
        match (self, scene) {
            (Model::Mobile(m), Scene::Mobile(e)) => {
                let p = m.segment(e, clicks, Some(bx))?;
                Ok(Mask { logits: p.logits, side: mobilesam_prep::OUTPUT_SIDE, score: p.score })
            }
            (Model::Sam3(_), Scene::Sam3(_)) => Err(Error::Model("box prompts need the MobileSAM model; SAM 3 takes clicks and text here".into())),
            _ => Err(other_scene()),
        }
    }

    /// Segment everything the comma-separated phrases in `text` describe (merged); `None` when nothing
    /// matches. [`Error::NeedsGrounding`] with a model that has no text path.
    pub fn segment_text(&mut self, scene: &mut Scene, text: &str) -> Result<Option<Mask>> {
        match (self, scene) {
            (Model::Sam3(m), Scene::Sam3(e)) => {
                let logits = remote::text_logits(m, e, text).map_err(Error::Model)?;
                Ok(logits.map(|logits| Mask { logits, side: crate::MASK_SIDE, score: 1.0 }))
            }
            (Model::Mobile(_), _) => Err(Error::NeedsGrounding),
            _ => Err(other_scene()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-segment-model-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_installed_model_is_found_by_its_files_and_sam3_wins() {
        let dir = scratch("kinds");
        assert_eq!(installed_kind(&dir), None);
        let e = Model::load(&dir).err().unwrap();
        assert!(matches!(e, Error::Missing(_)) && e.to_string().contains("not found"), "{e}");
        std::fs::write(dir.join(crate::MOBILESAM_FILE), b"x").unwrap();
        assert_eq!(installed_kind(&dir), Some(Kind::MobileSam));
        for f in [crate::WEIGHTS_FILE, "vocab.json", "merges.txt"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert_eq!(installed_kind(&dir), Some(Kind::Sam3));
        assert_eq!(Kind::Sam3.name(), "SAM 3");
        assert_eq!(Kind::MobileSam.name(), "MobileSAM");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_mobilesam_is_an_error_naming_the_file() {
        let dir = scratch("damaged");
        std::fs::write(dir.join(crate::MOBILESAM_FILE), b"not a checkpoint").unwrap();
        let e = Model::load(&dir).err().unwrap().to_string();
        assert!(e.contains(crate::MOBILESAM_FILE), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_needs_a_grounding_model_with_mobilesam() {
        let e = Error::NeedsGrounding.to_string();
        assert!(e.contains("Describe") && e.contains("#221") && e.contains("SAM 3"), "{e}");
    }
}
