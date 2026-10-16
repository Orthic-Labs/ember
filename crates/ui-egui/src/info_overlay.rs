//! The loupe's info overlay (`view.infoOverlay`, Cmd+I) and its two configurable line sets (the
//! View Options dialog, `view.viewOptions`). Pure data and text, with no egui, so the text is
//! tested without a window.

use lightcraft_catalog::model::{ColorLabel, Photo, Source};
use serde::{Deserialize, Serialize};

/// Lines per info overlay.
pub const INFO_LINES: usize = 3;

/// A field of the photo that one overlay line can show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InfoField {
    /// Nothing: the line is left out.
    #[default]
    None,
    FileName,
    /// Capture date and time (the import date for a file without one).
    CaptureDate,
    /// Width × height after the crop.
    Dimensions,
    /// Shutter, aperture, ISO and focal length, then the lens.
    ExposureIsoLens,
    Camera,
    Rating,
    /// The colour label, under the name it has in the catalog.
    Label,
    Title,
    Caption,
    /// The folder the file is in.
    Folder,
}

impl InfoField {
    /// Every choice, in the order the dialog lists them.
    pub const ALL: [InfoField; 11] = [
        InfoField::None,
        InfoField::FileName,
        InfoField::CaptureDate,
        InfoField::Dimensions,
        InfoField::ExposureIsoLens,
        InfoField::Camera,
        InfoField::Rating,
        InfoField::Label,
        InfoField::Title,
        InfoField::Caption,
        InfoField::Folder,
    ];

    /// The choice's name in the View Options dialog (passed through the translations).
    pub fn name(self) -> &'static str {
        match self {
            InfoField::None => "None",
            InfoField::FileName => "File name",
            InfoField::CaptureDate => "Capture date & time",
            InfoField::Dimensions => "Dimensions (cropped)",
            InfoField::ExposureIsoLens => "Exposure, ISO & lens",
            InfoField::Camera => "Camera",
            InfoField::Rating => "Rating",
            InfoField::Label => "Color label",
            InfoField::Title => "Title",
            InfoField::Caption => "Caption",
            InfoField::Folder => "Folder",
        }
    }
}

/// One info overlay: its fields, one per line, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoLines {
    pub lines: [InfoField; INFO_LINES],
}

impl Default for InfoLines {
    fn default() -> Self {
        Self { lines: [InfoField::None; INFO_LINES] }
    }
}

/// Info 1 by default: file name, capture date and dimensions.
fn info1_default() -> InfoLines {
    InfoLines { lines: [InfoField::FileName, InfoField::CaptureDate, InfoField::Dimensions] }
}

/// Info 2 by default: exposure, ISO and lens, then the camera.
fn info2_default() -> InfoLines {
    InfoLines { lines: [InfoField::ExposureIsoLens, InfoField::Camera, InfoField::None] }
}

fn yes() -> bool {
    true
}

/// The View Options: the two info overlays and whether the loupe says it is rendering. Saved with
/// the UI state; every key has a default, so settings written before this existed load as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ViewOptions {
    /// The overlay Cmd+I shows first.
    #[serde(default = "info1_default")]
    pub info1: InfoLines,
    /// The overlay Cmd+I shows second.
    #[serde(default = "info2_default")]
    pub info2: InfoLines,
    /// "Show message when loading or rendering photos": the "Rendering…" note on the loupe.
    #[serde(default = "yes")]
    pub render_message: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self { info1: info1_default(), info2: info2_default(), render_message: true }
    }
}

/// The overlay's text lines for `photo`, top to bottom. A configured field with no value (no
/// title, no label, no lens) leaves its line out. `label_name` gives a colour label's catalog name.
pub fn lines(photo: &Photo, config: &InfoLines, label_name: &dyn Fn(ColorLabel) -> String) -> Vec<String> {
    config.lines.iter().filter_map(|f| field_text(photo, *f, label_name)).collect()
}

fn field_text(photo: &Photo, field: InfoField, label_name: &dyn Fn(ColorLabel) -> String) -> Option<String> {
    let m = &photo.meta;
    let text = match field {
        InfoField::None => return None,
        InfoField::FileName => photo.file_name.clone(),
        InfoField::CaptureDate => pretty_date(photo.captured.as_deref().unwrap_or(&photo.imported)),
        InfoField::Dimensions => {
            // the crop's share of the frame (a straightening angle is not applied)
            let rect = photo.develop.crop.geometry.rect;
            let w = (f64::from(photo.width) * rect.width()).round() as u64;
            let h = (f64::from(photo.height) * rect.height()).round() as u64;
            format!("{w} × {h}")
        }
        InfoField::ExposureIsoLens => {
            let mut parts = Vec::new();
            if !m.shutter.is_empty() {
                parts.push(format!("{} s", m.shutter));
            }
            if let Some(a) = m.aperture {
                parts.push(format!("f/{a:.1}"));
            }
            if let Some(i) = m.iso {
                parts.push(format!("ISO {i}"));
            }
            if let Some(f) = m.focal_mm {
                parts.push(format!("{f:.0} mm"));
            }
            if !m.lens.is_empty() {
                parts.push(m.lens.clone());
            }
            if parts.is_empty() { "No exposure information".to_string() } else { parts.join("  ") }
        }
        InfoField::Camera => m.camera.clone(),
        InfoField::Rating => match photo.rating {
            0 => "Unrated".to_string(),
            n => format!("{n} of 5 stars"),
        },
        InfoField::Label => photo.label.map(label_name)?,
        InfoField::Title => m.title.clone(),
        InfoField::Caption => m.caption.clone(),
        InfoField::Folder => match &photo.source {
            Source::File { path } => std::path::Path::new(path).parent()?.display().to_string(),
            Source::Demo { .. } => return None,
        },
    };
    (!text.is_empty()).then_some(text)
}

/// `2026-10-16T09:30:00` → `2026-10-16 09:30`.
fn pretty_date(iso: &str) -> String {
    let d = iso.replacen('T', " ", 1);
    d.get(..16).map(str::to_string).unwrap_or(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{InfoOverlay, UiState};
    use lightcraft_catalog::model::PhotoId;

    fn photo() -> Photo {
        let mut p =
            Photo::new(PhotoId(1), Source::File { path: "/photos/2026/IMG_1.jpg".into() }, "IMG_1.jpg", "JPEG", 6000, 4000, "2026-10-01T08:00:00");
        p.captured = Some("2026-10-16T09:30:00".into());
        p
    }

    fn name(l: ColorLabel) -> String {
        format!("{l:?}")
    }

    #[test]
    fn default_info1_reproduces_the_previous_file_and_date_text() {
        // the old "File" overlay said the file name, then "date · width × height" on one line;
        // the default Info 1 has the same three values, each on its own line
        let lines = lines(&photo(), &ViewOptions::default().info1, &name);
        assert_eq!(lines, vec!["IMG_1.jpg", "2026-10-16 09:30", "6000 × 4000"]);
    }

    #[test]
    fn default_info2_reproduces_the_previous_exposure_text() {
        let mut p = photo();
        p.meta.shutter = "1/250".into();
        p.meta.aperture = Some(2.8);
        p.meta.iso = Some(400);
        p.meta.focal_mm = Some(50.0);
        p.meta.lens = "RF 50mm".into();
        p.meta.camera = "Canon EOS R5".into();
        let lines = lines(&p, &ViewOptions::default().info2, &name);
        assert_eq!(lines, vec!["1/250 s  f/2.8  ISO 400  50 mm  RF 50mm", "Canon EOS R5"]);
    }

    #[test]
    fn default_info2_without_exposure_says_so() {
        let lines = lines(&photo(), &ViewOptions::default().info2, &name);
        assert_eq!(lines, vec!["No exposure information"]);
    }

    #[test]
    fn custom_info1_renders_its_configured_fields_in_order() {
        let mut p = photo();
        p.meta.title = "Harbour".into();
        p.rating = 3;
        p.label = Some(ColorLabel::Green);
        let config = InfoLines { lines: [InfoField::Title, InfoField::Rating, InfoField::Folder] };
        assert_eq!(lines(&p, &config, &name), vec!["Harbour", "3 of 5 stars", "/photos/2026"]);
        let config = InfoLines { lines: [InfoField::Label, InfoField::None, InfoField::Caption] };
        assert_eq!(lines(&p, &config, &name), vec!["Green"]);
    }

    #[test]
    fn empty_fields_leave_their_lines_out() {
        let config = InfoLines { lines: [InfoField::Caption, InfoField::Camera, InfoField::Label] };
        assert!(lines(&photo(), &config, &name).is_empty());
    }

    #[test]
    fn dimensions_follow_the_crop() {
        let mut p = photo();
        std::sync::Arc::make_mut(&mut p.develop).crop.geometry.rect = lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 0.5 };
        let config = InfoLines { lines: [InfoField::Dimensions, InfoField::None, InfoField::None] };
        assert_eq!(lines(&p, &config, &name), vec!["3000 × 2000"]);
    }

    #[test]
    fn old_settings_load_with_the_new_defaults() {
        // the overlay mode was saved as off / basic / exposure before View Options
        assert_eq!(serde_json::from_str::<InfoOverlay>("\"off\"").ok(), Some(InfoOverlay::Off));
        assert_eq!(serde_json::from_str::<InfoOverlay>("\"basic\"").ok(), Some(InfoOverlay::Info1));
        assert_eq!(serde_json::from_str::<InfoOverlay>("\"exposure\"").ok(), Some(InfoOverlay::Info2));
        let ui: UiState = serde_json::from_str(r#"{"infoOverlay":"exposure"}"#).expect("old ui.json loads");
        assert_eq!(ui.info_overlay, InfoOverlay::Info2);
        assert_eq!(ui.view_options, ViewOptions::default());
        // a partial View Options block keeps the defaults for what it leaves out
        let partial: ViewOptions = serde_json::from_str(r#"{"renderMessage":false}"#).expect("partial loads");
        assert!(!partial.render_message);
        assert_eq!(partial.info1, ViewOptions::default().info1);
        assert_eq!(partial.info2, ViewOptions::default().info2);
    }
}
