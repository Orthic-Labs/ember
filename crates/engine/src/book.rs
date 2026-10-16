//! Classic Book: pages of photos laid out in millimetres and rendered through the export pipeline
//! ([`Session::export_job`]). Geometry is in millimetres and converted to pixels at the render DPI,
//! so a page keeps its proportions at any resolution (resolution independence). Text is drawn in
//! the bundled Inter face only. The book lives with the session and is saved with the library
//! preferences (`prefs.json`), so it reopens with the library.
use crate::Session;
use crate::contact_sheet::{Pdf, blit};
use crate::export::{Anchor, Watermark, draw_watermark, encode_image};
use crate::slideshow::{self, Fields};
use lightcraft_catalog::PhotoId;
use lightcraft_pipeline::{OutputDepth, OutputSpace};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most pages one book holds.
pub const MAX_PAGES: usize = 200;
/// Most photos one auto layout places.
pub const MAX_PHOTOS: usize = 1000;
/// Longest text a field may hold, in characters.
const MAX_TEXT: usize = 500;
/// Most pixels one page renders to (A3 at 300 dpi is about 17 MP).
const MAX_PAGE_PIXELS: usize = 40_000_000;
/// Largest PDF the pages may add up to, in bytes.
const MAX_PDF_BYTES: usize = 256 * 1024 * 1024;
/// The DPI a page renders at when none is given.
pub const DEFAULT_DPI: f64 = 150.0;
/// The bundled face: the only one a book's text may use.
pub const FONT: &str = "Inter";
/// A text line is this many font sizes tall.
const LINE: f64 = 1.6;
/// The cover title is this many times the body text size.
const COVER_SCALE: f64 = 3.0;

/// The whole book, edited through `book.set` (settings) and the page commands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    pub settings: Settings,
    pub typography: Typography,
    pub text: Text,
    pub guides: Guides,
    pub pages: Vec<Page>,
}

/// Page size, margins, cell padding, paper and cover.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Page width and height in millimetres (50–1000).
    pub width_mm: f64,
    pub height_mm: f64,
    /// Margin round the cells; the page text sits in the bottom margin (0–50 mm).
    pub margin_mm: f64,
    /// Space between a cell's edge and the photo in it (0–20 mm).
    pub cell_padding_mm: f64,
    /// The sheet's colour (sRGB), under every page.
    pub paper_color: [u8; 3],
    /// Output colour space: only sRGB is available.
    pub colour_space: String,
    pub cover: Cover,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            width_mm: 280.0,
            height_mm: 210.0,
            margin_mm: 10.0,
            cell_padding_mm: 3.0,
            paper_color: [255, 255, 255],
            colour_space: "srgb".into(),
            cover: Cover::default(),
        }
    }
}

/// A cover title drawn in the top margin of the first page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Cover {
    pub enabled: bool,
    pub title: String,
}

/// The face, size, alignment and colour of all book text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Typography {
    /// Always `Inter`, the bundled face.
    pub font: String,
    /// Body text size in points (4–72).
    pub size_pt: f64,
    /// `left`, `center` or `right`.
    pub align: String,
    pub color: [u8; 3],
}

impl Default for Typography {
    fn default() -> Self {
        Self { font: FONT.into(), size_pt: 11.0, align: "left".into(), color: [32, 32, 32] }
    }
}

/// Text under each photo: a metadata template (`{file}`, `{title}`, `{city}`, ... as in slideshows)
/// or free text. Empty = no photo text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Text {
    pub photo: String,
}

/// Print guides shown over the page view: bleed beyond the trim, and the safe area inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Guides {
    /// Bleed outside the trim edge, in millimetres (0–10).
    pub bleed_mm: f64,
    /// Safe area inside the trim edge, in millimetres (0–30).
    pub safe_mm: f64,
    pub show: bool,
}

impl Default for Guides {
    fn default() -> Self {
        Self { bleed_mm: 3.0, safe_mm: 5.0, show: true }
    }
}

/// One page: its template, the photo in each cell (`None` = empty), its text and background.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Page {
    /// A built-in template name, see [`builtin_layouts`].
    pub template: String,
    /// One entry per cell of the template.
    pub photos: Vec<Option<u64>>,
    /// Page text: a metadata template (fields of the page's first photo) or free text.
    pub text: String,
    pub background: Background,
}

impl Default for Page {
    fn default() -> Self {
        Self { template: "Single".into(), photos: vec![None], text: String::new(), background: Background::default() }
    }
}

/// The page's background: a colour (`None` = the sheet colour), and optionally a photo covering the
/// page at `opacity` over it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Background {
    pub color: Option<[u8; 3]>,
    pub photo: Option<u64>,
    /// 0–1: how far the photo covers the colour.
    pub opacity: f64,
}

impl Default for Background {
    fn default() -> Self {
        Self { color: None, photo: None, opacity: 1.0 }
    }
}

/// A cell as fractions (0–1) of the content area inside the margins.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A built-in page template: a name and its cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub name: String,
    pub cells: Vec<Cell>,
}

/// A rectangle in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

fn grid(cols: usize, rows: usize) -> Vec<Cell> {
    let (w, h) = (1.0 / cols as f64, 1.0 / rows as f64);
    (0..rows).flat_map(|r| (0..cols).map(move |c| Cell { x: c as f64 * w, y: r as f64 * h, w, h })).collect()
}

/// The built-in page templates.
pub fn builtin_layouts() -> Vec<Layout> {
    vec![
        Layout { name: "Single".into(), cells: grid(1, 1) },
        Layout { name: "Two Across".into(), cells: grid(2, 1) },
        Layout { name: "Two Stacked".into(), cells: grid(1, 2) },
        Layout { name: "Four Grid".into(), cells: grid(2, 2) },
        Layout { name: "Six Grid".into(), cells: grid(3, 2) },
    ]
}

/// The built-in template a name refers to (case-insensitive).
pub fn layout_named(name: &str) -> Result<Layout, String> {
    let name = name.trim();
    builtin_layouts().into_iter().find(|l| l.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no page template `{name}`"))
}

impl Model {
    /// Refuses values outside the ranges the layout and renderer support.
    pub fn validate(&self) -> Result<(), String> {
        fn check(ok: bool, msg: &str) -> Result<(), String> {
            if ok { Ok(()) } else { Err(msg.to_string()) }
        }
        let (s, t, x, g) = (&self.settings, &self.typography, &self.text, &self.guides);
        check((50.0..=1000.0).contains(&s.width_mm) && (50.0..=1000.0).contains(&s.height_mm), "width and height must be 50–1000 mm")?;
        check(
            (0.0..=50.0).contains(&s.margin_mm) && 2.0 * s.margin_mm < s.width_mm.min(s.height_mm),
            "margin must be 0–50 mm and leave room for the cells",
        )?;
        check((0.0..=20.0).contains(&s.cell_padding_mm), "cellPaddingMm must be 0–20")?;
        check(s.colour_space.eq_ignore_ascii_case("srgb"), "only sRGB output is available (colourSpace \"srgb\")")?;
        check(s.cover.title.chars().count() <= MAX_TEXT, "text must be at most 500 characters")?;
        check(t.font == FONT, "typography.font must be \"Inter\", the bundled face")?;
        check((4.0..=72.0).contains(&t.size_pt), "typography.sizePt must be 4–72")?;
        check(matches!(t.align.as_str(), "left" | "center" | "right"), "typography.align must be left, center or right")?;
        check(x.photo.chars().count() <= MAX_TEXT, "text must be at most 500 characters")?;
        check((0.0..=10.0).contains(&g.bleed_mm), "guides.bleedMm must be 0–10")?;
        check((0.0..=30.0).contains(&g.safe_mm), "guides.safeMm must be 0–30")?;
        check(self.pages.len() <= MAX_PAGES, "a book holds at most 200 pages")?;
        for page in &self.pages {
            let layout = layout_named(&page.template)?;
            check(page.photos.len() == layout.cells.len(), "a page's photos must match its template's cells")?;
            check(page.text.chars().count() <= MAX_TEXT, "text must be at most 500 characters")?;
            check((0.0..=1.0).contains(&page.background.opacity), "background opacity must be 0–1")?;
        }
        Ok(())
    }
}

/// The settings with `patch` applied. A patch holds sections (`settings`, `typography`, `text`,
/// `guides`), each with only the settings to change; `settings.cover` may be patched in part too.
/// Pages change through the page commands. A name that is not a setting is refused.
pub fn merged(current: &Model, patch: &Value) -> Result<Model, String> {
    let Value::Object(sections) = patch else { return Err("settings must be an object of sections".into()) };
    let mut v = serde_json::to_value(current).map_err(|e| e.to_string())?;
    for (section, values) in sections {
        if !matches!(section.as_str(), "settings" | "typography" | "text" | "guides") {
            return Err(format!("no settings section `{section}`"));
        }
        let Value::Object(values) = values else { return Err(format!("`{section}` must be an object")) };
        let target = v.get_mut(section).and_then(Value::as_object_mut).ok_or_else(|| format!("no settings section `{section}`"))?;
        for (key, value) in values {
            let Some(slot) = target.get_mut(key) else { return Err(format!("unknown setting `{section}.{key}`")) };
            match (slot, value) {
                (Value::Object(inner), Value::Object(new)) => {
                    for (k, nv) in new {
                        let Some(s) = inner.get_mut(k) else { return Err(format!("unknown setting `{section}.{key}.{k}`")) };
                        *s = nv.clone();
                    }
                }
                (slot, value) => *slot = value.clone(),
            }
        }
    }
    let model: Model = serde_json::from_value(v).map_err(|e| e.to_string())?;
    model.validate()?;
    Ok(model)
}

/// Appends a page with `template`; returns the page count.
pub fn add_page(model: &mut Model, template: &str) -> Result<usize, String> {
    if model.pages.len() >= MAX_PAGES {
        return Err(format!("a book holds at most {MAX_PAGES} pages"));
    }
    let layout = layout_named(template)?;
    model.pages.push(Page { template: layout.name.clone(), photos: vec![None; layout.cells.len()], ..Default::default() });
    Ok(model.pages.len())
}

/// Removes the page at `index`.
pub fn remove_page(model: &mut Model, index: usize) -> Result<(), String> {
    if index >= model.pages.len() {
        return Err(format!("no page {index}"));
    }
    model.pages.remove(index);
    Ok(())
}

/// Changes a page's template; photos keep their cells, and new cells are empty.
pub fn set_template(page: &mut Page, name: &str) -> Result<(), String> {
    let layout = layout_named(name)?;
    page.template = layout.name.clone();
    page.photos.resize(layout.cells.len(), None);
    Ok(())
}

/// Puts `photo` (or nothing) in cell `cell` of `page`.
pub fn set_cell(page: &mut Page, cell: usize, photo: Option<u64>) -> Result<(), String> {
    let slot = page.photos.get_mut(cell).ok_or_else(|| format!("no cell {cell} on this page"))?;
    *slot = photo;
    Ok(())
}

/// Fills pages from `ids` in order, `template`'s cells per page. With `replace` the book's pages
/// are first cleared; otherwise the new pages follow the existing ones. Returns the pages added.
pub fn auto_layout(model: &mut Model, template: &str, ids: &[PhotoId], replace: bool) -> Result<usize, String> {
    let layout = layout_named(template)?;
    if ids.is_empty() || ids.len() > MAX_PHOTOS {
        return Err(format!("select between 1 and {MAX_PHOTOS} photos"));
    }
    let added = ids.len().div_ceil(layout.cells.len());
    let kept = if replace { 0 } else { model.pages.len() };
    if kept + added > MAX_PAGES {
        return Err(format!("a book holds at most {MAX_PAGES} pages; select fewer photos or use a larger template"));
    }
    if replace {
        model.pages.clear();
    }
    for chunk in ids.chunks(layout.cells.len()) {
        let mut photos = vec![None; layout.cells.len()];
        for (slot, id) in photos.iter_mut().zip(chunk) {
            *slot = Some(id.0);
        }
        model.pages.push(Page { template: layout.name.clone(), photos, ..Default::default() });
    }
    Ok(added)
}

/// Millimetres at `dpi` as pixels.
pub fn px(mm: f64, dpi: f64) -> f64 {
    mm * dpi / 25.4
}

/// The page's pixel size at `dpi`.
pub fn page_size(settings: &Settings, dpi: f64) -> Result<(usize, usize), String> {
    if !(36.0..=600.0).contains(&dpi) {
        return Err("dpi must be 36–600".into());
    }
    let w = px(settings.width_mm, dpi).round().max(1.0) as usize;
    let h = px(settings.height_mm, dpi).round().max(1.0) as usize;
    if w.saturating_mul(h) > MAX_PAGE_PIXELS {
        return Err("the page is too large at this dpi; lower the dpi or the page size".into());
    }
    Ok((w, h))
}

/// Cell `cell` on a page at `dpi`: inside the margins, inset by the cell padding. Edges are rounded
/// to whole pixels after the millimetres are converted, so the geometry scales with the DPI.
pub fn cell_rect(settings: &Settings, cell: &Cell, dpi: f64) -> Rect {
    let (cw, ch) = (settings.width_mm - 2.0 * settings.margin_mm, settings.height_mm - 2.0 * settings.margin_mm);
    let pad = settings.cell_padding_mm;
    let (x0, y0) = (settings.margin_mm + cell.x * cw + pad, settings.margin_mm + cell.y * ch + pad);
    let (x1, y1) = (settings.margin_mm + (cell.x + cell.w) * cw - pad, settings.margin_mm + (cell.y + cell.h) * ch - pad);
    let edge = |mm: f64| px(mm, dpi).round().max(0.0) as usize;
    let (left, top) = (edge(x0), edge(y0));
    Rect { x: left, y: top, w: edge(x1).saturating_sub(left), h: edge(y1).saturating_sub(top) }
}

fn rgba(c: [u8; 3]) -> [u8; 4] {
    [c[0], c[1], c[2], 255]
}

/// A line of text `width` × `height` pixels on `base`, `size_px` tall, aligned as `align`.
pub(crate) fn text_line(width: usize, height: usize, base: [u8; 3], text: &str, size_px: f64, align: &str, color: [u8; 3]) -> Rgba8 {
    let mut strip = Rgba8::filled(width.max(1), height.max(1), rgba(base));
    if text.trim().is_empty() {
        return strip;
    }
    let short = width.min(height).max(1) as f64;
    let anchor = match align {
        "center" => Anchor::Center,
        "right" => Anchor::Right,
        _ => Anchor::Left,
    };
    let size = (size_px / short) as f32;
    let wm = Watermark { text: text.to_string(), size, opacity: 1.0, anchor, inset_x: 0.0, inset_y: 0.0, color, shadow: false, ..Default::default() };
    draw_watermark(&mut strip, &wm);
    strip
}

pub(crate) fn font_px(size_pt: f64, dpi: f64) -> f64 {
    size_pt / 72.0 * dpi
}

/// The height of a text line at `size_pt`, kept within `limit` pixels.
pub(crate) fn line_height(size_pt: f64, dpi: f64, limit: usize) -> usize {
    ((font_px(size_pt, dpi) * LINE).round() as usize).clamp(1, limit.max(1))
}

/// One page rendered at `dpi`: background, cells with their photos and photo text, page text and
/// the cover title. Photos come through the normal export pipeline with the develop edits applied.
pub fn render_page(session: &mut Session, model: &Model, index: usize, dpi: f64) -> Result<Rgba8, String> {
    model.validate()?;
    let page = model.pages.get(index).ok_or("no such page")?;
    let (s, t) = (&model.settings, &model.typography);
    let (w, h) = page_size(s, dpi)?;
    let base = page.background.color.unwrap_or(s.paper_color);
    let mut img = Rgba8::filled(w, h, rgba(base));
    if let Some(id) = page.background.photo {
        let job = session.export_job(PhotoId(id), w, h, OutputSpace::Srgb, OutputDepth::U8)?;
        let photo = slideshow::cover(&job.run().rendered?.image, w, h);
        img = slideshow::blend(&img, &photo, page.background.opacity);
    }
    let margin_px = px(s.margin_mm, dpi).round() as usize;
    let band_w = w.saturating_sub(2 * margin_px).max(1);
    let band_x = (w - band_w) / 2;
    if index == 0 && s.cover.enabled && !s.cover.title.trim().is_empty() {
        let size = t.size_pt * COVER_SCALE;
        let lh = line_height(size, dpi, margin_px.max(1));
        let y = margin_px.saturating_sub(lh) / 2;
        let strip = text_line(band_w, lh, base, &s.cover.title, font_px(size, dpi), "center", t.color);
        blit(&mut img, &strip, band_x, y)?;
    }
    let layout = layout_named(&page.template)?;
    let photo_text = !model.text.photo.trim().is_empty();
    for (i, cell) in layout.cells.iter().enumerate() {
        let Some(Some(id)) = page.photos.get(i) else { continue };
        let id = PhotoId(*id);
        let rect = cell_rect(s, cell, dpi);
        let strip_h = if photo_text { line_height(t.size_pt, dpi, rect.h) } else { 0 };
        let (pw, ph) = (rect.w, rect.h - strip_h);
        if pw > 0 && ph > 0 {
            let job = session.export_job(id, pw, ph, OutputSpace::Srgb, OutputDepth::U8)?;
            let photo = job.run().rendered?.image;
            let x = rect.x + pw.saturating_sub(photo.width) / 2;
            let y = rect.y + ph.saturating_sub(photo.height) / 2;
            blit(&mut img, &photo, x, y)?;
        }
        if strip_h > 0 {
            let fields = session.catalog.photo(id).map(|p| Fields::of(p)).ok_or("no such photo")?;
            let text = slideshow::expand(&model.text.photo, &fields);
            let strip = text_line(rect.w, strip_h, base, &text, font_px(t.size_pt, dpi), &t.align, t.color);
            blit(&mut img, &strip, rect.x, rect.y + rect.h - strip_h)?;
        }
    }
    if !page.text.trim().is_empty() {
        let fields = match page.photos.iter().flatten().next() {
            Some(id) => session.catalog.photo(PhotoId(*id)).map(|p| Fields::of(p)).unwrap_or_default(),
            None => Fields::default(),
        };
        let text = slideshow::expand(&page.text, &fields);
        let lh = line_height(t.size_pt, dpi, margin_px.max(1));
        let y = h.saturating_sub(margin_px) + margin_px.saturating_sub(lh) / 2;
        let strip = text_line(band_w, lh, base, &text, font_px(t.size_pt, dpi), &t.align, t.color);
        blit(&mut img, &strip, band_x, y.min(h.saturating_sub(lh)))?;
    }
    Ok(img)
}

/// The book as a PDF: one page per book page, each a JPEG at `dpi` on a page of the book's size.
pub fn export_pdf(session: &mut Session, model: &Model, dpi: f64) -> Result<Vec<u8>, String> {
    model.validate()?;
    if model.pages.is_empty() {
        return Err("the book has no pages: add a page first".into());
    }
    let (w, h) = page_size(&model.settings, dpi)?;
    let points = (model.settings.width_mm * 72.0 / 25.4, model.settings.height_mm * 72.0 / 25.4);
    let mut pdf = Pdf::new(model.pages.len(), points);
    let mut total = 0usize;
    for i in 0..model.pages.len() {
        let img = render_page(session, model, i, dpi)?;
        let jpg = encode_image(&img, &slideshow::jpeg_options())?;
        total = total.saturating_add(jpg.len());
        if total > MAX_PDF_BYTES {
            return Err("the book exceeds 256 MiB; export fewer pages or a lower dpi".into());
        }
        pdf.page(i, &jpg, w, h);
    }
    Ok(pdf.finish())
}

/// One JPEG per book page at `dpi`, named `page-0001.jpg`, `page-0002.jpg`, ...
pub fn export_jpegs(session: &mut Session, model: &Model, dpi: f64) -> Result<Vec<(String, Vec<u8>)>, String> {
    model.validate()?;
    if model.pages.is_empty() {
        return Err("the book has no pages: add a page first".into());
    }
    page_size(&model.settings, dpi)?;
    let mut files = Vec::with_capacity(model.pages.len());
    for i in 0..model.pages.len() {
        let img = render_page(session, model, i, dpi)?;
        files.push((format!("page-{:04}.jpg", i + 1), encode_image(&img, &slideshow::jpeg_options())?));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(n: u64) -> Vec<PhotoId> {
        (1..=n).map(PhotoId).collect()
    }

    #[test]
    fn serde_defaults_fill_missing_fields() {
        let empty: Model = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Model::default());
        let partial: Model = serde_json::from_value(json!({"settings": {"widthMm": 300.0}, "typography": {"sizePt": 14.0}})).unwrap();
        assert_eq!(partial.settings.width_mm, 300.0);
        assert_eq!(partial.settings.height_mm, Settings::default().height_mm);
        assert_eq!((partial.typography.font.as_str(), partial.typography.align.as_str()), ("Inter", "left"));
        assert!(partial.pages.is_empty());
        assert_eq!(Page::default().photos, vec![None]);
        assert_eq!(Background::default().opacity, 1.0);
        partial.validate().unwrap();
    }

    #[test]
    fn settings_patches_refuse_unknown_names_and_bad_values() {
        for p in [
            json!({"settings": {"widthMm": 10.0}}),
            json!({"typography": {"font": "Arial"}}),
            json!({"typography": {"align": "justify"}}),
            json!({"settings": {"colourSpace": "adobeRgb"}}),
            json!({"guides": {"bleedMm": 50.0}}),
            json!({"settings": {"typo": 1}}),
            json!({"settings": {"cover": {"titel": "x"}}}),
            json!({"pages": []}),
        ] {
            assert!(merged(&Model::default(), &p).is_err(), "{p}");
        }
        let m = merged(&Model::default(), &json!({"settings": {"cover": {"enabled": true, "title": "Trip"}}})).unwrap();
        assert!(m.settings.cover.enabled);
        assert_eq!(m.settings.cover.title, "Trip");
    }

    #[test]
    fn auto_layout_fills_template_cells_across_pages() {
        let mut m = Model::default();
        assert_eq!(auto_layout(&mut m, "four grid", &ids(7), true).unwrap(), 2);
        assert_eq!(m.pages.len(), 2);
        assert_eq!(m.pages[0].template, "Four Grid");
        assert_eq!(m.pages[0].photos, vec![Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(m.pages[1].photos, vec![Some(5), Some(6), Some(7), None]);
        // appending keeps the pages already there; replacing starts again
        assert_eq!(auto_layout(&mut m, "Single", &ids(2), false).unwrap(), 2);
        assert_eq!(m.pages.len(), 4);
        assert_eq!(auto_layout(&mut m, "Two Across", &ids(3), true).unwrap(), 2);
        assert_eq!(m.pages.len(), 2);
        assert_eq!(m.pages[1].photos, vec![Some(3), None]);
        // a refused layout leaves the pages alone
        assert!(auto_layout(&mut m, "Poster", &ids(3), true).is_err());
        assert!(auto_layout(&mut m, "Single", &[], true).is_err());
        assert_eq!(m.pages.len(), 2);
        m.validate().unwrap();
        // the page limit holds for appended pages too
        let many = ids(MAX_PHOTOS as u64);
        assert!(auto_layout(&mut m, "Single", &many, false).is_err());
    }

    #[test]
    fn cell_geometry_with_padding_scales_with_dpi() {
        let s = Settings { cell_padding_mm: 4.0, margin_mm: 10.0, ..Settings::default() };
        let layout = layout_named("two across").unwrap();
        let cell = &layout.cells[0];
        let (lo, hi) = (cell_rect(&s, cell, 150.0), cell_rect(&s, cell, 300.0));
        // the same millimetres at both resolutions: edges double, within a pixel of rounding
        assert!((hi.x as i64 - 2 * lo.x as i64).abs() <= 1, "{lo:?} {hi:?}");
        assert!((hi.w as i64 - 2 * lo.w as i64).abs() <= 2, "{lo:?} {hi:?}");
        assert!((hi.h as i64 - 2 * lo.h as i64).abs() <= 2, "{lo:?} {hi:?}");
        // padding is 4 mm in from the margin's inner edge, at each DPI
        let margin_px = |dpi: f64| px(10.0, dpi).round() as usize;
        assert!((hi.x - margin_px(300.0)).abs_diff(px(4.0, 300.0).round() as usize) <= 1);
        assert!((lo.x - margin_px(150.0)).abs_diff(px(4.0, 150.0).round() as usize) <= 1);
        // the cell keeps its shape and stays on the page
        let (a, b) = (lo.w as f64 / lo.h as f64, hi.w as f64 / hi.h as f64);
        assert!((a - b).abs() < 0.01);
        let (pw, ph) = page_size(&s, 300.0).unwrap();
        assert!(hi.x + hi.w <= pw && hi.y + hi.h <= ph);
    }

    #[test]
    fn text_templates_expand_from_metadata() {
        let fields = slideshow::Fields { file: "IMG_1.jpg".into(), city: "Oslo".into(), rating: 4, ..Default::default() };
        assert_eq!(slideshow::expand("{city}, {date} · {rating}/5 {unknown}", &fields), "Oslo,  · 4/5 {unknown}");
        assert_eq!(slideshow::expand("Our trip", &fields), "Our trip");
    }

    #[test]
    fn pages_render_and_export_pdf_and_jpegs() {
        let mut s = Session::with_demo();
        let photos: Vec<PhotoId> = s.catalog.photos().take(3).map(|p| p.id).collect();
        let mut m = Model::default();
        auto_layout(&mut m, "Two Across", &photos, true).unwrap();
        m.settings.cover = Cover { enabled: true, title: "Trip".into() };
        m.pages[1].text = "{file}".into();
        let plain = render_page(&mut s, &m, 0, 72.0).unwrap();
        assert_eq!((plain.width, plain.height), (794, 595));
        m.text.photo = "{file}".into();
        let captioned = render_page(&mut s, &m, 0, 72.0).unwrap();
        assert_ne!(plain, captioned, "photo text was not drawn");
        let doc = export_pdf(&mut s, &m, 72.0).unwrap();
        let text = String::from_utf8_lossy(&doc);
        assert_eq!(text.matches("/Type /Page ").count(), 2);
        assert!(text.contains("/Count 2 "));
        let files = export_jpegs(&mut s, &m, 72.0).unwrap();
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["page-0001.jpg", "page-0002.jpg"]);
        assert!(export_pdf(&mut s, &Model::default(), 72.0).is_err());
        assert!(render_page(&mut s, &m, 9, 72.0).is_err());
        assert!(page_size(&m.settings, 10.0).is_err());
    }
}
