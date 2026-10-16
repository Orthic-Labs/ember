//! Classic Print: photos laid out on paper in millimetres for a print job, rendered through the
//! export pipeline at the job's ppi and written as a multi-page PDF or one JPEG per page. Styles:
//! a single image, a contact sheet, a picture package (cells packed onto one page) and a custom
//! grid. Geometry is in millimetres, so a page keeps its proportions at any ppi (resolution
//! independence). A job writes PDF or JPEG only: OS printing needs a platform API this build lacks.
//! The print lives with the session; it is not saved with the library.
use crate::Session;
use crate::book::{self, Rect, font_px, line_height, text_line};
use crate::contact_sheet::{Pdf, blit};
use crate::export::{SharpenAmount, SharpenFor, encode_image, output_sharpen};
use crate::proof::{self, ProofIntent};
use crate::slideshow::{self, Fields};
use lightcraft_catalog::PhotoId;
use lightcraft_pipeline::{OutputDepth, OutputSpace, Proof};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most pages one print holds.
pub const MAX_PAGES: usize = 200;
/// Most photos one print places.
pub const MAX_PHOTOS: usize = 1000;
/// Most picture-package cells one page holds.
const MAX_CELLS: usize = 64;
/// Longest text a field may hold, in characters.
const MAX_TEXT: usize = 500;
/// Most pixels one page renders to (A4 at 600 ppi is about 35 MP).
const MAX_PAGE_PIXELS: usize = 40_000_000;
/// Largest PDF the pages may add up to, in bytes.
const MAX_PDF_BYTES: usize = 256 * 1024 * 1024;
/// The ppi a print renders at when none is given.
pub const DEFAULT_PPI: f64 = 300.0;
/// The smallest cell side a grid or a custom cell may have, in millimetres.
const MIN_CELL_MM: f64 = 5.0;
/// Tolerance for millimetre sums that should meet exactly.
const EPS: f64 = 1e-6;
/// The colour of printed text.
const TEXT_COLOUR: [u8; 3] = [32, 32, 32];

/// The whole print job, edited through `print.set` and `print.applyTemplate`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    pub layout: Layout,
    pub image: ImageSettings,
    pub guides: Guides,
    /// Picture-package cells, in the order the photos fill them (style `package`).
    pub package: Vec<CellSize>,
    pub page: PageOptions,
    pub job: Job,
}

/// The layout style, paper, grid, margins and spacing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Layout {
    /// `single` (one photo fills the page), `contact` (a grid), `package` (the package cells) or `custom`
    /// (a grid of fixed cells, when a cell size is given; otherwise a grid).
    pub style: String,
    /// `a4` or `letter`.
    pub paper: String,
    pub landscape: bool,
    /// Grid cells across (1–8) and down (1–10).
    pub columns: usize,
    pub rows: usize,
    /// Margin round the page's edges, in millimetres (0–50).
    pub margin_mm: f64,
    /// Space between cells, in millimetres (0–20).
    pub spacing_mm: f64,
    /// Custom cell size in millimetres (5–1000); both 0 = the grid fills the page.
    pub cell_width_mm: f64,
    pub cell_height_mm: f64,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            style: "contact".into(),
            paper: "a4".into(),
            landscape: false,
            columns: 3,
            rows: 4,
            margin_mm: 10.0,
            spacing_mm: 3.0,
            cell_width_mm: 0.0,
            cell_height_mm: 0.0,
        }
    }
}

/// How each photo sits in its cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageSettings {
    /// Crop the photo to fill its cell (otherwise it fits inside).
    pub zoom_to_fill: bool,
    /// Turn a photo 90° when that fits its cell better.
    pub rotate_to_fit: bool,
    /// One photo per page, repeated in every cell of that page.
    pub repeat_one: bool,
    /// A frame round each photo, in points (0–10).
    pub stroke_pt: f64,
    pub stroke_color: [u8; 3],
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self { zoom_to_fill: false, rotate_to_fit: true, repeat_one: false, stroke_pt: 0.0, stroke_color: [0, 0, 0] }
    }
}

/// Guides shown over the print view: rulers, cell dimensions and the bleed beyond the trim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Guides {
    pub rulers: bool,
    pub dimensions: bool,
    /// Bleed beyond the trim edge, in millimetres (0–10).
    pub bleed_mm: f64,
    /// Whether the guides are drawn at all.
    pub show: bool,
}

impl Default for Guides {
    fn default() -> Self {
        Self { rulers: true, dimensions: true, bleed_mm: 3.0, show: true }
    }
}

/// One picture-package cell: its size in millimetres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CellSize {
    pub width_mm: f64,
    pub height_mm: f64,
}

impl Default for CellSize {
    fn default() -> Self {
        Self { width_mm: 50.0, height_mm: 40.0 }
    }
}

/// Text on the page: an identity plate above the cells and an info line below, each a metadata
/// template (`{file}`, `{city}`, ... as in slideshows); a filename caption under each photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PageOptions {
    pub identity_plate: String,
    pub info_text: String,
    pub captions: bool,
    /// Text size in points (4–72).
    pub text_size_pt: f64,
    /// The paper's colour (sRGB).
    pub background: [u8; 3],
}

impl Default for PageOptions {
    fn default() -> Self {
        Self { identity_plate: String::new(), info_text: String::new(), captions: false, text_size_pt: 8.0, background: [255, 255, 255] }
    }
}

/// The output: resolution, sharpening, the file kind, and an optional output profile to soft-proof
/// against (the photos are converted through it, then written as sRGB).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Job {
    /// Pixels per inch (72–600).
    pub ppi: f64,
    /// `none`, `screen`, `matte` or `glossy`.
    pub sharpen: String,
    /// `low`, `standard` or `high`.
    pub sharpen_amount: String,
    /// `pdf`, `jpeg` or `printer` (refused: no OS print API in this build).
    pub output: String,
    /// The output profile's path; empty = no soft proof.
    pub profile: String,
    /// `perceptual` or `relative`.
    pub intent: String,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            ppi: DEFAULT_PPI,
            sharpen: "none".into(),
            sharpen_amount: "standard".into(),
            output: "pdf".into(),
            profile: String::new(),
            intent: "perceptual".into(),
        }
    }
}

/// A rectangle in millimetres on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectMm {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Model {
    /// Refuses values outside the ranges the layout and the renderer support.
    pub fn validate(&self) -> Result<(), String> {
        fn check(ok: bool, msg: &str) -> Result<(), String> {
            if ok { Ok(()) } else { Err(msg.to_string()) }
        }
        let (l, i, g, p, j) = (&self.layout, &self.image, &self.guides, &self.page, &self.job);
        check(matches!(l.style.as_str(), "single" | "contact" | "package" | "custom"), "layout.style must be single, contact, package or custom")?;
        check(matches!(l.paper.as_str(), "a4" | "letter"), "layout.paper must be a4 or letter")?;
        check((1..=8).contains(&l.columns) && (1..=10).contains(&l.rows), "columns must be 1–8 and rows 1–10")?;
        check((0.0..=50.0).contains(&l.margin_mm), "marginMm must be 0–50")?;
        check((0.0..=20.0).contains(&l.spacing_mm), "spacingMm must be 0–20")?;
        check(
            (l.cell_width_mm == 0.0 && l.cell_height_mm == 0.0)
                || ((MIN_CELL_MM..=1000.0).contains(&l.cell_width_mm) && (MIN_CELL_MM..=1000.0).contains(&l.cell_height_mm)),
            "cellWidthMm and cellHeightMm are both 0 or both 5–1000",
        )?;
        check(self.package.len() <= MAX_CELLS, "a picture package holds at most 64 cells")?;
        check(
            self.package.iter().all(|c| (MIN_CELL_MM..=1000.0).contains(&c.width_mm) && (MIN_CELL_MM..=1000.0).contains(&c.height_mm)),
            "package cells are 5–1000 mm",
        )?;
        check(l.style != "package" || !self.package.is_empty(), "a picture package needs cells in package")?;
        check((0.0..=10.0).contains(&i.stroke_pt), "image.strokePt must be 0–10")?;
        check((0.0..=10.0).contains(&g.bleed_mm), "guides.bleedMm must be 0–10")?;
        check(p.identity_plate.chars().count() <= MAX_TEXT && p.info_text.chars().count() <= MAX_TEXT, "text must be at most 500 characters")?;
        check((4.0..=72.0).contains(&p.text_size_pt), "page.textSizePt must be 4–72")?;
        check((72.0..=600.0).contains(&j.ppi), "job.ppi must be 72–600")?;
        sharpening(j)?;
        check(matches!(j.output.as_str(), "pdf" | "jpeg" | "printer"), "job.output must be pdf, jpeg or printer")?;
        ProofIntent::parse(&j.intent).ok_or("job.intent must be perceptual or relative")?;
        Ok(())
    }
}

/// The settings with `patch` applied. A patch holds sections (`layout`, `image`, `guides`, `page`,
/// `job`), each with only the settings to change; `package` replaces the cells. A name that is not a
/// setting is refused, and nothing changes.
pub fn merged(current: &Model, patch: &Value) -> Result<Model, String> {
    let Value::Object(sections) = patch else { return Err("settings must be an object of sections".into()) };
    let mut v = serde_json::to_value(current).map_err(|e| e.to_string())?;
    for (section, values) in sections {
        if section.as_str() == "package" {
            v["package"] = values.clone();
            continue;
        }
        if !matches!(section.as_str(), "layout" | "image" | "guides" | "page" | "job") {
            return Err(format!("no settings section `{section}`"));
        }
        let Value::Object(values) = values else { return Err(format!("`{section}` must be an object")) };
        let target = v.get_mut(section.as_str()).and_then(Value::as_object_mut).ok_or_else(|| format!("no settings section `{section}`"))?;
        for (key, value) in values {
            let slot = target.get_mut(key.as_str()).ok_or_else(|| format!("unknown setting `{section}.{key}`"))?;
            *slot = value.clone();
        }
    }
    let model: Model = serde_json::from_value(v).map_err(|e| e.to_string())?;
    model.validate()?;
    Ok(model)
}

/// The built-in print templates, by name.
pub fn builtin_templates() -> &'static [&'static str] {
    &["Contact Sheet", "Single Image", "Picture Package", "Custom Grid"]
}

/// The layout, image and package settings a built-in template sets; the job and page options stay.
pub fn template(name: &str) -> Result<Model, String> {
    let mut m = Model::default();
    match name.trim().to_ascii_lowercase().as_str() {
        "contact sheet" => {}
        "single image" => m.layout.style = "single".into(),
        "picture package" => {
            m.layout.style = "package".into();
            m.package = [(120.0, 80.0), (60.0, 80.0), (60.0, 80.0), (90.0, 60.0)]
                .into_iter()
                .map(|(width_mm, height_mm)| CellSize { width_mm, height_mm })
                .collect();
        }
        "custom grid" => {
            m.layout.style = "custom".into();
            m.layout.columns = 2;
            m.layout.rows = 2;
            m.layout.cell_width_mm = 80.0;
            m.layout.cell_height_mm = 60.0;
        }
        _ => return Err(format!("no print template `{name}`")),
    }
    Ok(m)
}

/// The paper's size in millimetres, landscape turned over.
pub fn paper_mm(l: &Layout) -> Result<(f64, f64), String> {
    let (w, h) = match l.paper.as_str() {
        "a4" => (210.0, 297.0),
        "letter" => (215.9, 279.4),
        _ => return Err("paper must be a4 or letter".into()),
    };
    Ok(if l.landscape { (h, w) } else { (w, h) })
}

/// The page's pixel size at `ppi`.
pub fn page_size(w_mm: f64, h_mm: f64, ppi: f64) -> Result<(usize, usize), String> {
    if !(72.0..=600.0).contains(&ppi) {
        return Err("ppi must be 72–600".into());
    }
    let w = book::px(w_mm, ppi).round().max(1.0) as usize;
    let h = book::px(h_mm, ppi).round().max(1.0) as usize;
    if w.saturating_mul(h) > MAX_PAGE_PIXELS {
        return Err("the page is too large at this ppi; lower the ppi or use a smaller paper".into());
    }
    Ok((w, h))
}

/// Packs cells of the given sizes (millimetres) in order, left to right and then on the next shelf
/// down, `gap` apart. Fails when a cell is larger than the area or the shelves run past its bottom.
pub fn pack(width: f64, height: f64, gap: f64, sizes: &[(f64, f64)]) -> Result<Vec<RectMm>, String> {
    let (mut x, mut y, mut shelf) = (0.0_f64, 0.0_f64, 0.0_f64);
    let mut out = Vec::with_capacity(sizes.len());
    for &(w, h) in sizes {
        if w > width + EPS || h > height + EPS {
            return Err("a picture package cell is larger than the page".into());
        }
        if x + w > width + EPS {
            x = 0.0;
            y += shelf + gap;
            shelf = 0.0;
        }
        if y + h > height + EPS {
            return Err("the picture package cells do not fit on one page: use smaller or fewer cells".into());
        }
        out.push(RectMm { x, y, w, h });
        x += w + gap;
        shelf = shelf.max(h);
    }
    Ok(out)
}

/// A grid of `columns` × `rows` cells that fills the content area, `spacing_mm` apart.
fn grid(l: &Layout, cw: f64, ch: f64) -> Result<Vec<RectMm>, String> {
    let (cols, rows) = (l.columns, l.rows);
    let w = (cw - (cols - 1) as f64 * l.spacing_mm) / cols as f64;
    let h = (ch - (rows - 1) as f64 * l.spacing_mm) / rows as f64;
    if w < MIN_CELL_MM || h < MIN_CELL_MM {
        return Err("the grid is too dense for the page: fewer columns or rows, less spacing, or a smaller margin".into());
    }
    let mut out = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            out.push(RectMm { x: c as f64 * (w + l.spacing_mm), y: r as f64 * (h + l.spacing_mm), w, h });
        }
    }
    Ok(out)
}

/// A grid of fixed-size cells from the top-left of the content area.
fn fixed(l: &Layout, cw: f64, ch: f64) -> Result<Vec<RectMm>, String> {
    let mut out = Vec::with_capacity(l.columns * l.rows);
    for r in 0..l.rows {
        for c in 0..l.columns {
            let x = c as f64 * (l.cell_width_mm + l.spacing_mm);
            let y = r as f64 * (l.cell_height_mm + l.spacing_mm);
            if x + l.cell_width_mm > cw + EPS || y + l.cell_height_mm > ch + EPS {
                return Err("the custom cells do not fit the page: smaller cells, fewer rows or columns, or a smaller margin".into());
            }
            out.push(RectMm { x, y, w: l.cell_width_mm, h: l.cell_height_mm });
        }
    }
    Ok(out)
}

/// The cells of one page, in millimetres from the paper's top-left corner, in the order photos fill them.
pub fn cells_mm(m: &Model) -> Result<Vec<RectMm>, String> {
    m.validate()?;
    let l = &m.layout;
    let (pw, ph) = paper_mm(l)?;
    let (cw, ch) = (pw - 2.0 * l.margin_mm, ph - 2.0 * l.margin_mm);
    let relative = match l.style.as_str() {
        "single" => vec![RectMm { x: 0.0, y: 0.0, w: cw, h: ch }],
        "package" => {
            let sizes: Vec<(f64, f64)> = m.package.iter().map(|c| (c.width_mm, c.height_mm)).collect();
            pack(cw, ch, l.spacing_mm, &sizes)?
        }
        "custom" if l.cell_width_mm > 0.0 => fixed(l, cw, ch)?,
        _ => grid(l, cw, ch)?,
    };
    Ok(relative.into_iter().map(|r| RectMm { x: r.x + l.margin_mm, y: r.y + l.margin_mm, ..r }).collect())
}

/// The photo (an index into the selection) each cell of each page shows; `None` = an empty cell.
/// With `repeat_one` each photo gets a page of its own, repeated in every cell.
pub fn plan(m: &Model, photos: usize) -> Result<Vec<Vec<Option<usize>>>, String> {
    let cells = cells_mm(m)?.len();
    if cells == 0 {
        return Err("the layout has no cells: a picture package needs cells".into());
    }
    if photos == 0 || photos > MAX_PHOTOS {
        return Err(format!("select between 1 and {MAX_PHOTOS} photos"));
    }
    let pages: Vec<Vec<Option<usize>>> = if m.image.repeat_one {
        (0..photos).map(|i| vec![Some(i); cells]).collect()
    } else {
        (0..photos)
            .step_by(cells)
            .map(|start| (start..start + cells).map(|i| (i < photos).then_some(i)).collect::<Vec<Option<usize>>>())
            .collect()
    };
    if pages.len() > MAX_PAGES {
        return Err(format!("a print holds at most {MAX_PAGES} pages; select fewer photos or use larger cells"));
    }
    Ok(pages)
}

fn rgba(c: [u8; 3]) -> [u8; 4] {
    [c[0], c[1], c[2], 255]
}

/// A cell in whole pixels at `ppi`; edges are rounded after the millimetres are converted.
fn cell_px(c: &RectMm, ppi: f64) -> Rect {
    let edge = |mm: f64| book::px(mm, ppi).round().max(0.0) as usize;
    let (x0, y0, x1, y1) = (edge(c.x), edge(c.y), edge(c.x + c.w), edge(c.y + c.h));
    Rect { x: x0, y: y0, w: x1.saturating_sub(x0), h: y1.saturating_sub(y0) }
}

fn sharpening(job: &Job) -> Result<(SharpenFor, SharpenAmount), String> {
    let target = SharpenFor::parse(&job.sharpen).ok_or("job.sharpen must be none, screen, matte or glossy")?;
    let amount = SharpenAmount::parse(&job.sharpen_amount).ok_or("job.sharpenAmount must be low, standard or high")?;
    Ok((target, amount))
}

/// The id of the job's output profile for soft proofing, or `None` when the job names no profile.
fn proof_id(job: &Job) -> Result<Option<u64>, String> {
    let path = job.profile.trim();
    if path.is_empty() {
        return Ok(None);
    }
    let intent = ProofIntent::parse(&job.intent).ok_or("job.intent must be perceptual or relative")?;
    Ok(Some(proof::load(path, intent, false)?.id))
}

/// Turns `img` by 90° clockwise.
fn rotate90(img: &Rgba8) -> Rgba8 {
    let (w, h) = (img.width, img.height);
    let mut out = Rgba8::filled(h, w, [0, 0, 0, 255]);
    for y in 0..h {
        for x in 0..w {
            let src = img.data.get(y * w + x).copied().unwrap_or([0, 0, 0, 255]);
            if let Some(dst) = out.data.get_mut(x * h + (h - 1 - y)) {
                *dst = src;
            }
        }
    }
    out
}

/// A photo fitted (or covering, with zoom to fill) into `w` × `h` pixels, turned to suit the cell and
/// sharpened for output.
fn fitted(session: &mut Session, m: &Model, id: PhotoId, w: usize, h: usize, proof: Option<u64>) -> Result<Rgba8, String> {
    let (pw, ph) = session.catalog.photo(id).map(|p| (p.width, p.height)).ok_or("no such photo")?;
    let turn = m.image.rotate_to_fit && w != h && pw != ph && ((pw > ph) != (w > h));
    let (bw, bh) = if turn { (h, w) } else { (w, h) };
    let mut job = session.export_job(id, bw, bh, OutputSpace::Srgb, OutputDepth::U8)?;
    if let Some(profile) = proof {
        job = job.with_proof(Some(Proof { space: OutputSpace::Srgb, dest_warning: false, display_warning: false, profile }));
    }
    let mut img = job.run().rendered?.image;
    if turn {
        img = rotate90(&img);
    }
    if m.image.zoom_to_fill {
        img = slideshow::cover(&img, w, h);
    }
    let (target, amount) = sharpening(&m.job)?;
    if target != SharpenFor::None {
        output_sharpen(&mut img, target, amount);
    }
    Ok(img)
}

/// Draws a `width`-pixel frame of `colour` round the rectangle `r`, outside it.
fn frame(img: &mut Rgba8, r: Rect, width: usize, colour: [u8; 3]) {
    let c = rgba(colour);
    let (x0, y0) = (r.x.saturating_sub(width), r.y.saturating_sub(width));
    let (x1, y1) = ((r.x + r.w + width).min(img.width), (r.y + r.h + width).min(img.height));
    let stride = img.width;
    for y in y0..y1 {
        for x in x0..x1 {
            let inside = x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
            if inside {
                continue;
            }
            if let Some(p) = img.data.get_mut(y * stride + x) {
                *p = c;
            }
        }
    }
}

/// One photo in its cell with its frame and caption.
fn place(session: &mut Session, m: &Model, id: PhotoId, cell: Rect, proof: Option<u64>, ppi: f64, img: &mut Rgba8) -> Result<(), String> {
    if cell.w == 0 || cell.h == 0 {
        return Ok(());
    }
    let size = m.page.text_size_pt;
    let strip = if m.page.captions { line_height(size, ppi, cell.h / 4) } else { 0 };
    let stroke = (m.image.stroke_pt / 72.0 * ppi).round().max(0.0) as usize;
    let (bw, bh) = (cell.w.saturating_sub(2 * stroke), cell.h.saturating_sub(strip + 2 * stroke));
    if bw > 0 && bh > 0 {
        let photo = fitted(session, m, id, bw, bh, proof)?;
        if photo.width > bw || photo.height > bh {
            return Err("the render exceeded the print cell".into());
        }
        let x = cell.x + stroke + (bw - photo.width) / 2;
        let y = cell.y + stroke + (bh - photo.height) / 2;
        blit(img, &photo, x, y)?;
        if stroke > 0 {
            frame(img, Rect { x, y, w: photo.width, h: photo.height }, stroke, m.image.stroke_color);
        }
    }
    if strip > 0 {
        let name = session.catalog.photo(id).map(|p| p.file_name.clone()).ok_or("no such photo")?;
        let line = text_line(cell.w, strip, m.page.background, &name, font_px(size, ppi), "center", TEXT_COLOUR);
        blit(img, &line, cell.x, cell.y + cell.h - strip)?;
    }
    Ok(())
}

/// A line of page text in the margin band, `top` or bottom; nothing is drawn with no margin.
fn band_text(img: &mut Rgba8, m: &Model, text: &str, band: usize, ppi: f64, top: bool, align: &str) -> Result<(), String> {
    if text.trim().is_empty() || band == 0 || img.width <= 2 * band || img.height < band {
        return Ok(());
    }
    let size = m.page.text_size_pt;
    let lh = line_height(size, ppi, band);
    let y = if top { (band - lh) / 2 } else { img.height - band + (band - lh) / 2 };
    let line = text_line(img.width - 2 * band, lh, m.page.background, text, font_px(size, ppi), align, TEXT_COLOUR);
    blit(img, &line, band, y)
}

/// Page `index` of the print of `photos` at `ppi`: the background, each cell's photo, and the
/// identity plate and info text expanded from the page's first photo.
pub fn render_page(session: &mut Session, m: &Model, photos: &[PhotoId], index: usize, ppi: f64) -> Result<Rgba8, String> {
    let pages = plan(m, photos.len())?;
    let page = pages.get(index).ok_or("no such page")?;
    let (w_mm, h_mm) = paper_mm(&m.layout)?;
    let (w, h) = page_size(w_mm, h_mm, ppi)?;
    let proof = proof_id(&m.job)?;
    let mut img = Rgba8::filled(w, h, rgba(m.page.background));
    for (cell, slot) in cells_mm(m)?.iter().zip(page) {
        let Some(i) = slot else { continue };
        let id = *photos.get(*i).ok_or("no such photo")?;
        place(session, m, id, cell_px(cell, ppi), proof, ppi, &mut img)?;
    }
    let first = page.iter().flatten().next().and_then(|i| photos.get(*i)).copied();
    let fields = first.and_then(|id| session.catalog.photo(id)).map(Fields::of).unwrap_or_default();
    let band = book::px(m.layout.margin_mm, ppi).round().max(0.0) as usize;
    let identity = slideshow::expand(&m.page.identity_plate, &fields);
    let info = slideshow::expand(&m.page.info_text, &fields);
    band_text(&mut img, m, &identity, band, ppi, true, "left")?;
    band_text(&mut img, m, &info, band, ppi, false, "right")?;
    Ok(img)
}

/// The print as a PDF: one page per print page, each a JPEG at `ppi` on a paper of the layout's size.
pub fn export_pdf(session: &mut Session, m: &Model, photos: &[PhotoId], ppi: f64) -> Result<Vec<u8>, String> {
    let pages = plan(m, photos.len())?.len();
    let (w_mm, h_mm) = paper_mm(&m.layout)?;
    let (w, h) = page_size(w_mm, h_mm, ppi)?;
    let mut doc = Pdf::new(pages, (w_mm * 72.0 / 25.4, h_mm * 72.0 / 25.4));
    let mut total = 0usize;
    for i in 0..pages {
        let jpg = encode_image(&render_page(session, m, photos, i, ppi)?, &slideshow::jpeg_options())?;
        total = total.saturating_add(jpg.len());
        if total > MAX_PDF_BYTES {
            return Err("the print exceeds 256 MiB; print fewer photos or use a lower ppi".into());
        }
        doc.page(i, &jpg, w, h);
    }
    Ok(doc.finish())
}

/// One JPEG per print page at `ppi`, named `print-0001.jpg`, `print-0002.jpg`, ...
pub fn export_jpegs(session: &mut Session, m: &Model, photos: &[PhotoId], ppi: f64) -> Result<Vec<(String, Vec<u8>)>, String> {
    let pages = plan(m, photos.len())?.len();
    let (w_mm, h_mm) = paper_mm(&m.layout)?;
    page_size(w_mm, h_mm, ppi)?;
    let mut files = Vec::with_capacity(pages);
    for i in 0..pages {
        let img = render_page(session, m, photos, i, ppi)?;
        files.push((format!("print-{:04}.jpg", i + 1), encode_image(&img, &slideshow::jpeg_options())?));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn photos(s: &Session, n: usize) -> Vec<PhotoId> {
        s.catalog.photos().take(n).map(|p| p.id).collect()
    }

    #[test]
    fn serde_defaults_fill_missing_fields() {
        let empty: Model = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Model::default());
        let partial: Model = serde_json::from_value(json!({"layout": {"columns": 5}, "job": {"ppi": 240.0}})).unwrap();
        assert_eq!(partial.layout.columns, 5);
        assert_eq!((partial.layout.rows, partial.layout.style.as_str()), (Layout::default().rows, "contact"));
        assert_eq!(partial.job.ppi, 240.0);
        assert_eq!((partial.job.output.as_str(), partial.job.intent.as_str()), ("pdf", "perceptual"));
        assert!(partial.image.rotate_to_fit && !partial.image.repeat_one);
        assert!(partial.package.is_empty());
        partial.validate().unwrap();
        assert_eq!(Model::default().job.ppi, DEFAULT_PPI);
        assert_eq!(CellSize::default(), CellSize { width_mm: 50.0, height_mm: 40.0 });
    }

    #[test]
    fn patches_refuse_unknown_names_and_bad_values() {
        for p in [
            json!({"layout": {"columns": 9}}),
            json!({"layout": {"paper": "poster"}}),
            json!({"layout": {"colums": 2}}),
            json!({"job": {"ppi": 10.0}}),
            json!({"job": {"output": "printer2"}}),
            json!({"job": {"intent": "saturation"}}),
            json!({"pages": []}),
            json!({"page": {"identityPlate": "x"}, "job": {"sharpen": "huge"}}),
            json!({"package": [{"widthMm": 1.0, "heightMm": 2.0}]}),
        ] {
            assert!(merged(&Model::default(), &p).is_err(), "{p}");
        }
        let m = merged(&Model::default(), &json!({"layout": {"style": "package"}, "package": [{"widthMm": 60.0, "heightMm": 40.0}]})).unwrap();
        assert_eq!(m.package.len(), 1);
        assert_eq!(m.layout.style, "package");
    }

    #[test]
    fn picture_package_cells_pack_without_overlap() {
        let sizes = [(90.0, 40.0), (90.0, 40.0), (60.0, 30.0), (60.0, 30.0), (60.0, 30.0)];
        let cells = pack(200.0, 100.0, 2.0, &sizes).unwrap();
        assert_eq!(cells.len(), sizes.len());
        for (i, a) in cells.iter().enumerate() {
            assert!(a.x >= 0.0 && a.y >= 0.0 && a.x + a.w <= 200.0 + EPS && a.y + a.h <= 100.0 + EPS, "{a:?}");
            for b in &cells[i + 1..] {
                let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        // the third cell starts the second shelf, below the first one's 40 mm and the 2 mm gap
        assert_eq!((cells[2].x, cells[2].y), (0.0, 42.0));
        assert!(pack(200.0, 50.0, 2.0, &sizes).is_err(), "cells past the bottom edge");
        assert!(pack(50.0, 100.0, 2.0, &sizes).is_err(), "a cell wider than the page");
    }

    #[test]
    fn cell_geometry_scales_with_ppi() {
        let mut m = Model::default();
        m.layout.style = "single".into();
        let cell = cells_mm(&m).unwrap()[0];
        let (lo, hi) = (cell_px(&cell, 150.0), cell_px(&cell, 300.0));
        assert_eq!((lo.x, lo.y), (59, 59));
        assert_eq!((hi.x, hi.y), (118, 118));
        assert_eq!((hi.w, hi.h), (2 * lo.w, 2 * lo.h));
        assert_eq!(page_size(210.0, 297.0, 300.0).unwrap(), (2480, 3508));
        assert!(page_size(210.0, 297.0, 10.0).is_err());
        // landscape turns the paper over
        m.layout.landscape = true;
        assert_eq!(paper_mm(&m.layout).unwrap(), (297.0, 210.0));
    }

    #[test]
    fn photos_fill_cells_then_pages() {
        let mut m = Model::default();
        let p = plan(&m, 13).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!((p[1][0], p[1][1]), (Some(12), None));
        m.image.repeat_one = true;
        let r = plan(&m, 3).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[2].len(), 12);
        assert!(r[2].iter().all(|c| *c == Some(2)));
        assert!(plan(&m, 0).is_err());
    }

    #[test]
    fn pdf_and_jpegs_have_one_page_per_print_page() {
        let mut s = Session::with_demo();
        let ids = photos(&s, 3);
        let mut m = Model::default();
        m.layout.columns = 1;
        m.layout.rows = 2;
        let doc = export_pdf(&mut s, &m, &ids, 72.0).unwrap();
        let text = String::from_utf8_lossy(&doc);
        assert_eq!(text.matches("/Type /Page ").count(), 2);
        assert!(text.contains("/Count 2 "));
        let files = export_jpegs(&mut s, &m, &ids, 72.0).unwrap();
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["print-0001.jpg", "print-0002.jpg"]);
        m.image.repeat_one = true;
        let doc = export_pdf(&mut s, &m, &ids, 72.0).unwrap();
        assert!(String::from_utf8_lossy(&doc).contains("/Count 3 "));
        assert!(export_pdf(&mut s, &m, &[], 72.0).is_err());
    }

    #[test]
    fn info_text_expands_from_metadata() {
        let mut s = Session::with_demo();
        let ids = photos(&s, 1);
        let name = s.catalog.photo(ids[0]).unwrap().file_name.clone();
        let mut m = Model::default();
        m.layout.style = "single".into();
        m.page.info_text = "{file}".into();
        let expanded = render_page(&mut s, &m, &ids, 0, 72.0).unwrap();
        m.page.info_text = name;
        let literal = render_page(&mut s, &m, &ids, 0, 72.0).unwrap();
        assert!(expanded == literal, "the info text was not expanded from the photo");
        m.page.info_text.clear();
        let plain = render_page(&mut s, &m, &ids, 0, 72.0).unwrap();
        assert!(plain != expanded, "the info text was not drawn");
    }

    #[test]
    fn templates_set_layout_and_refuse_unknown_names() {
        assert_eq!(builtin_templates().len(), 4);
        let pkg = template("picture package").unwrap();
        assert_eq!((pkg.layout.style.as_str(), pkg.package.len()), ("package", 4));
        assert!(pkg.validate().is_ok());
        assert_eq!(template("Custom Grid").unwrap().layout.cell_width_mm, 80.0);
        assert!(template("poster").is_err());
    }
}
