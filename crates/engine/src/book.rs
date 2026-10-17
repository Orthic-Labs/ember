//! Classic Book: pages of photos laid out in millimetres and rendered through the export pipeline
//! ([`Session::export_job`]). Geometry is in millimetres and converted to pixels at the render DPI,
//! so a page keeps its proportions at any resolution (resolution independence). Text is drawn in
//! the bundled Inter face only. The book lives with the session and is saved with the library
//! preferences (`prefs.json`), so it reopens with the library.
use crate::Session;
use crate::contact_sheet::{Pdf, blit};
use crate::export::{Anchor, Watermark, encode_image, watermark_fonts, watermark_text_coverage};
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
/// The default line spacing: a text line is this many font sizes tall.
const DEFAULT_LEADING: f64 = 1.6;
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

/// The face, size, alignment, colour and leading of all book text.
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
    /// Line spacing: the distance between lines, in font sizes (1–3). Text wraps inside its cell.
    pub leading: f64,
}

impl Default for Typography {
    fn default() -> Self {
        Self { font: FONT.into(), size_pt: 11.0, align: "left".into(), color: [32, 32, 32], leading: DEFAULT_LEADING }
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
        check((1.0..=3.0).contains(&t.leading), "typography.leading must be 1–3")?;
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

pub(crate) fn font_px(size_pt: f64, dpi: f64) -> f64 {
    size_pt / 72.0 * dpi
}

/// The pitch of a text line at `size_pt` with `leading` font sizes per line, in pixels at `dpi`.
pub(crate) fn line_pitch(size_pt: f64, leading: f64, dpi: f64) -> usize {
    (font_px(size_pt, dpi) * leading).round().max(1.0) as usize
}

/// How a block of text is set: its size in pixels, its line pitch in pixels, its alignment and colour.
pub(crate) struct Setting<'a> {
    pub size_px: f64,
    pub pitch: usize,
    pub align: &'a str,
    pub color: [u8; 3],
}

/// The book's body text as set at `dpi`.
pub(crate) fn setting(t: &Typography, dpi: f64) -> Setting<'_> {
    Setting { size_px: font_px(t.size_pt, dpi), pitch: line_pitch(t.size_pt, t.leading, dpi), align: &t.align, color: t.color }
}

/// A line of text `width` × `height` pixels on `base`, aligned as `s` says and centred down the
/// strip. Unlike [`text_line`], the glyphs keep the size they are set at in a strip of any shape.
pub(crate) fn fitted_line(width: usize, height: usize, base: [u8; 3], text: &str, s: &Setting<'_>) -> Rgba8 {
    let mut strip = Rgba8::filled(width.max(1), height.max(1), rgba(base));
    draw_fitted(&mut strip, text, s);
    strip
}

/// Draws `text` as one line into `strip` with the watermark's shaping and face fallback. The line is
/// laid out on a canvas padded by the font size: the watermark sizes text by the short edge of the
/// canvas it is given, so a strip shorter than twice the text would shrink the glyphs.
fn draw_fitted(strip: &mut Rgba8, text: &str, s: &Setting<'_>) {
    let (w, h) = (strip.width, strip.height);
    if text.trim().is_empty() || w == 0 || h == 0 || !(s.size_px > 0.0) {
        return;
    }
    let pad = s.size_px.ceil().max(1.0) as usize;
    let vh = h + 2 * pad;
    let vw = w.max(vh) + 2 * pad;
    // the canvas x that lands on the strip's x = 0
    let (anchor, shift) = match s.align {
        "center" => (Anchor::Center, (vw - w) / 2),
        "right" => (Anchor::Right, vw - pad - w),
        _ => (Anchor::Left, pad),
    };
    let wm = Watermark {
        text: text.to_string(),
        size: (s.size_px / vh as f64) as f32,
        opacity: 1.0,
        anchor,
        inset_x: pad as f32 / vh as f32,
        inset_y: 0.0,
        color: s.color,
        shadow: false,
        ..Default::default()
    };
    watermark_text_coverage(vw, vh, &wm, |x, y, k, col| {
        let (Some(sx), Some(sy)) = (x.checked_sub(shift), y.checked_sub(pad)) else { return };
        if sx >= w || sy >= h {
            return;
        }
        if let Some(p) = strip.data.get_mut(sy * w + sx) {
            for c in 0..3 {
                p[c] = (p[c] as f32 + (col[c] as f32 - p[c] as f32) * k).round() as u8;
            }
        }
    });
}

/// Whether a line may break before or after `c`: CJK ideographs, kana and fullwidth forms.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x30FF | 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

/// The advance of `text` at `size_px` with the watermark's face fallback. Kerning is left out, so a
/// line measured here is never wider than the same line drawn.
fn advance_px(fonts: &[ab_glyph::FontRef<'static>], text: &str, size_px: f32) -> f32 {
    use ab_glyph::{Font, PxScale, ScaleFont};
    let scale = PxScale::from(size_px.max(6.0));
    let Some(latin) = fonts.first() else { return 0.0 };
    text.chars()
        .map(|c| {
            let face = fonts.iter().position(|f| f.glyph_id(c).0 != 0).unwrap_or(0);
            let sf = fonts.get(face).unwrap_or(latin).as_scaled(scale);
            sf.h_advance(sf.glyph_id(c))
        })
        .sum()
}

/// Breaks `text` into lines no wider than `max_px` at `size_px`. Lines break at spaces and between
/// CJK characters; a word wider than a line breaks between its characters. Each `\n` ends a line.
pub(crate) fn wrap(text: &str, max_px: f64, size_px: f64) -> Vec<String> {
    let fonts = watermark_fonts(crate::fonts::CRAFT_FONTS);
    let (max, px) = (max_px as f32, size_px as f32);
    text.split('\n').flat_map(|para| wrap_paragraph(&fonts, para, max, px)).collect()
}

fn wrap_paragraph(fonts: &[ab_glyph::FontRef<'static>], para: &str, max: f32, px: f32) -> Vec<String> {
    let chars: Vec<char> = para.chars().collect();
    let span = |a: usize, b: usize| -> String { chars.get(a..b).unwrap_or_default().iter().collect() };
    let mut lines = Vec::new();
    let (mut start, mut i) = (0usize, 0usize);
    // the last place a line may break before char `i`
    let mut brk: Option<usize> = None;
    while i < chars.len() {
        let here = chars.get(i).copied().unwrap_or(' ');
        let before = i.checked_sub(1).and_then(|p| chars.get(p)).copied().unwrap_or(' ');
        if i > start && (before == ' ' || is_cjk(before) || is_cjk(here)) {
            brk = Some(i);
        }
        if i > start && advance_px(fonts, span(start, i + 1).trim_end(), px) > max {
            let at = brk.filter(|b| *b > start).unwrap_or(i);
            lines.push(span(start, at).trim_end().to_string());
            start = at;
            while chars.get(start) == Some(&' ') {
                start += 1;
            }
            i = start;
            brk = None;
            continue;
        }
        i += 1;
    }
    lines.push(span(start, chars.len()).trim_end().to_string());
    lines
}

/// The text wrapped to `width` pixels and set as `s`: a block on `base` at most `max_h` pixels tall.
/// Lines past the bottom of `max_h` are not drawn; the block is as tall as the lines it draws.
pub(crate) fn text_block(width: usize, max_h: usize, base: [u8; 3], text: &str, s: &Setting<'_>) -> Result<Rgba8, String> {
    let (width, pitch) = (width.max(1), s.pitch.max(1));
    let rows = max_h / pitch;
    let lines = wrap(text, width as f64, s.size_px);
    let drawn = lines.len().min(rows);
    let mut img = Rgba8::filled(width, drawn * pitch, rgba(base));
    for (k, line) in lines.iter().take(drawn).enumerate() {
        let strip = fitted_line(width, pitch, base, line, s);
        blit(&mut img, &strip, 0, k * pitch)?;
    }
    Ok(img)
}

/// The height of a text line at `size_pt`, kept within `limit` pixels.
pub(crate) fn line_height(size_pt: f64, dpi: f64, limit: usize) -> usize {
    ((font_px(size_pt, dpi) * DEFAULT_LEADING).round() as usize).clamp(1, limit.max(1))
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
        let cover = Setting {
            size_px: font_px(size, dpi),
            pitch: line_pitch(size, t.leading, dpi).min(margin_px.max(1)),
            align: "center",
            color: t.color,
        };
        let y = margin_px.saturating_sub(cover.pitch) / 2;
        let strip = fitted_line(band_w, cover.pitch, base, &s.cover.title, &cover);
        blit(&mut img, &strip, band_x, y)?;
    }
    let layout = layout_named(&page.template)?;
    let photo_text = !model.text.photo.trim().is_empty();
    for (i, cell) in layout.cells.iter().enumerate() {
        let Some(Some(id)) = page.photos.get(i) else { continue };
        let id = PhotoId(*id);
        let rect = cell_rect(s, cell, dpi);
        // the photo text wraps inside the cell, in at most half of it
        let caption = if photo_text {
            let fields = session.catalog.photo(id).map(|p| Fields::of(p)).ok_or("no such photo")?;
            let text = slideshow::expand(&model.text.photo, &fields);
            Some(text_block(rect.w, rect.h / 2, base, &text, &setting(t, dpi))?)
        } else {
            None
        };
        let strip_h = caption.as_ref().map_or(0, |c| c.height);
        let (pw, ph) = (rect.w, rect.h - strip_h);
        if pw > 0 && ph > 0 {
            let job = session.export_job(id, pw, ph, OutputSpace::Srgb, OutputDepth::U8)?;
            let photo = job.run().rendered?.image;
            let x = rect.x + pw.saturating_sub(photo.width) / 2;
            let y = rect.y + ph.saturating_sub(photo.height) / 2;
            blit(&mut img, &photo, x, y)?;
        }
        if let Some(strip) = caption {
            blit(&mut img, &strip, rect.x, rect.y + rect.h - strip_h)?;
        }
    }
    if !page.text.trim().is_empty() {
        let fields = match page.photos.iter().flatten().next() {
            Some(id) => session.catalog.photo(PhotoId(*id)).map(|p| Fields::of(p)).unwrap_or_default(),
            None => Fields::default(),
        };
        let text = slideshow::expand(&page.text, &fields);
        // the page text wraps across the bottom margin; lines past the margin are not drawn
        let block = text_block(band_w, margin_px, base, &text, &setting(t, dpi))?;
        let bh = block.height;
        let y = h.saturating_sub(margin_px) + margin_px.saturating_sub(bh) / 2;
        blit(&mut img, &block, band_x, y.min(h.saturating_sub(bh)))?;
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

    /// The box (min x, min y, max x, max y) of the pixels that differ from `base`.
    fn ink_bounds(img: &Rgba8, base: [u8; 3]) -> Option<(usize, usize, usize, usize)> {
        let mut found: Option<(usize, usize, usize, usize)> = None;
        for y in 0..img.height {
            for x in 0..img.width {
                let p = img.data[y * img.width + x];
                if [p[0], p[1], p[2]] == base {
                    continue;
                }
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        found
    }

    /// The width in pixels of `text` set as `s` in a cell `width` pixels wide.
    fn ink_width(width: usize, s: &Setting<'_>, text: &str) -> i64 {
        let base = [255, 255, 255];
        let strip = fitted_line(width, s.pitch, base, text, s);
        ink_bounds(&strip, base).map_or(0, |(x0, _, x1, _)| (x1 - x0) as i64)
    }

    #[test]
    fn wrapped_text_breaks_within_the_width_and_keeps_its_words() {
        let text = "Alpine lake at dawn, with mist over the water and the pines behind";
        let (size, max) = (font_px(11.0, 150.0), 300.0);
        let lines = wrap(text, max, size);
        assert!(lines.len() >= 2, "{lines:?}");
        let fonts = watermark_fonts(crate::fonts::CRAFT_FONTS);
        for line in &lines {
            assert!(advance_px(&fonts, line, size as f32) <= max as f32, "{line}");
        }
        assert_eq!(lines.join(" "), text);
        // a word wider than a line breaks between its characters; a \n ends a line
        let long = wrap("Supercalifragilistic\nend", 40.0, size);
        assert!(long.len() >= 3, "{long:?}");
        assert_eq!(long.last().map(String::as_str), Some("end"));
        assert_eq!(long[..long.len() - 1].concat(), "Supercalifragilistic");
        // CJK text breaks between its characters
        let cjk = wrap("漢字かな交じり文", 40.0, size);
        assert!(cjk.len() >= 2, "{cjk:?}");
        assert_eq!(cjk.concat(), "漢字かな交じり文");
        // a block holds only the lines that fit its height
        let s = setting(&Typography::default(), 150.0);
        let block = text_block(300, 100, [255; 3], &"word ".repeat(60), &s).unwrap();
        assert!(block.height > 0 && block.height <= 100, "{}", block.height);
        assert_eq!(block.height % s.pitch, 0);
    }

    #[test]
    fn the_same_text_at_two_dpis_breaks_alike_and_scales() {
        let text = "Alpine lake at dawn, with mist over the water and the pines behind";
        let t = Typography::default();
        let (lo, hi) = (setting(&t, 150.0), setting(&t, 300.0));
        // a 60 mm wide cell: the same millimetres at each DPI
        let (w_lo, w_hi) = (px(60.0, 150.0).round() as usize, px(60.0, 300.0).round() as usize);
        let lines = wrap(text, w_lo as f64, lo.size_px);
        assert!(lines.len() >= 2);
        assert_eq!(lines, wrap(text, w_hi as f64, hi.size_px), "the breaks differ between 150 and 300 dpi");
        // the line pitch doubles with the DPI (within a pixel of rounding), and so do the glyphs
        assert!((hi.pitch as i64 - 2 * lo.pitch as i64).abs() <= 1, "{} {}", lo.pitch, hi.pitch);
        let (a, b) = (ink_width(w_lo, &lo, "Alpine lake"), ink_width(w_hi, &hi, "Alpine lake"));
        assert!(a > 50, "{a}");
        assert!((b - 2 * a).abs() <= 3, "{a} {b}");
        // the block is the lines times the pitch at each DPI
        let (bl, bh) = (text_block(w_lo, 100_000, [255; 3], text, &lo).unwrap(), text_block(w_hi, 100_000, [255; 3], text, &hi).unwrap());
        assert_eq!(bl.height, lines.len() * lo.pitch);
        assert_eq!(bh.height, lines.len() * hi.pitch);
    }

    #[test]
    fn a_line_is_aligned_in_its_cell_and_centred_down_it() {
        let base = [255, 255, 255];
        let set = |align: &'static str| Setting { size_px: 30.0, pitch: 60, align, color: [0, 0, 0] };
        let ink = |align: &'static str| ink_bounds(&fitted_line(400, 60, base, "Hi", &set(align)), base).unwrap_or((0, 0, 0, 0));
        let (l, c, r) = (ink("left"), ink("center"), ink("right"));
        assert!(l.0 < 10, "left {l:?}");
        assert!(r.2 > 390, "right {r:?}");
        assert!(((c.0 + c.2) as i64 / 2 - 200).abs() <= 3, "centre {c:?}");
        assert!(((l.1 + l.3) as i64 / 2 - 30).abs() <= 3, "down the strip {l:?}");
        // the glyphs keep their size in a strip shorter than the text (the watermark alone would halve them)
        let tall = fitted_line(400, 20, base, "Hi", &set("left"));
        let (x0, y0, x1, y1) = ink_bounds(&tall, base).unwrap_or((0, 0, 0, 0));
        assert!(y1 - y0 + 1 >= 20, "{y0} {y1}");
        assert!(x1 > x0);
    }

    #[test]
    fn leading_is_the_line_spacing_in_font_sizes() {
        for bad in [0.5, 3.5] {
            assert!(merged(&Model::default(), &json!({"typography": {"leading": bad}})).is_err(), "{bad}");
        }
        let m = merged(&Model::default(), &json!({"typography": {"leading": 2.0}})).unwrap();
        assert_eq!(m.typography.leading, 2.0);
        assert_eq!(line_pitch(11.0, 2.0, 72.0), 22);
        assert!(line_pitch(11.0, 1.0, 150.0) < line_pitch(11.0, 2.0, 150.0));
    }
}
