//! Classic Book view (`book.view`): the book drawn in the window as one page, as a two-page spread or
//! as a grid of every page (overview), with page navigation, the print guides (bleed outside the trim,
//! safe area inside it) and the photo and page text fields. Pages are rendered by the engine
//! ([`lightcraft_engine::book::render_page`]) at a preview resolution; a page is rendered again only
//! when the book changes. Escape closes the view.

use crate::LightcraftApp;
use crate::text_field::TextField;
use egui::{Color32, Rect, Sense, Stroke, StrokeKind, TextureHandle, pos2, vec2};
use lightcraft_engine::book::{self, Model};
use serde_json::{Value, json};

/// Long edge of the page preview, in pixels.
const LONG_EDGE: f64 = 1200.0;
/// Long edge of an overview thumbnail's render, in pixels.
const THUMB_EDGE: f64 = 360.0;
/// Width of an overview thumbnail, in points.
const THUMB_WIDTH: f32 = 150.0;
/// Space between thumbnails and around the grid, in points.
const GAP: f32 = 12.0;
/// Thumbnails rendered in one frame; the rest follow in later frames.
const THUMBS_PER_FRAME: usize = 6;
/// The colour of the current page's outline.
const ACCENT: Color32 = Color32::from_rgb(120, 170, 255);

/// How the book is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// One page.
    #[default]
    Page,
    /// Two facing pages.
    Spread,
    /// Every page as a thumbnail.
    Overview,
}

/// The view's state: how the book is shown, the current page and the text being edited. Not saved.
#[derive(Clone, Debug, Default)]
pub struct Run {
    /// Index of the current page: the page the text fields refer to, and the one a click selects.
    pub page: usize,
    /// How the book is shown.
    pub view: View,
    /// Guides drawn over the pages.
    pub guides: bool,
    photo_text: String,
    page_text: String,
    /// The book changed: every render is made again before it is drawn.
    dirty: bool,
    /// Renders of the pages on screen (page and spread views).
    shown: Vec<(usize, TextureHandle)>,
    /// Pages whose render failed since the book last changed (not tried again until then).
    failed: Vec<usize>,
    /// Thumbnails of every page (overview), made a few per frame.
    thumbs: Vec<Option<TextureHandle>>,
    /// Thumbnails whose render failed since the book last changed.
    failed_thumbs: Vec<usize>,
}

impl Run {
    /// The pages on screen in the page and spread views (none in the overview).
    pub fn visible(&self, pages: usize) -> Vec<usize> {
        match self.view {
            View::Page => {
                if self.page < pages {
                    vec![self.page]
                } else {
                    Vec::new()
                }
            }
            View::Spread => {
                let (left, right) = spread_pages(self.page, pages);
                left.into_iter().chain(right).collect()
            }
            View::Overview => Vec::new(),
        }
    }
}

/// Index of the spread holding `page`. Page 0 is a spread of its own (the first page sits on the
/// right, as in a book); the pages after it pair up as (1, 2), (3, 4), ...
pub fn spread_of(page: usize) -> usize {
    if page == 0 { 0 } else { page.div_ceil(2) }
}

/// The first page of spread `spread`: where the spread view opens it.
pub fn spread_start(spread: usize) -> usize {
    if spread == 0 { 0 } else { 2 * spread - 1 }
}

/// Number of spreads in a book of `pages` pages.
pub fn spread_count(pages: usize) -> usize {
    if pages == 0 { 0 } else { 1 + pages / 2 }
}

/// The left and right pages of the spread holding `page`; a side with no page in the book is `None`.
pub fn spread_pages(page: usize, pages: usize) -> (Option<usize>, Option<usize>) {
    let spread = spread_of(page);
    let (left, right) = if spread == 0 { (None, Some(0)) } else { (Some(2 * spread - 1), Some(2 * spread)) };
    let in_book = |p: Option<usize>| p.filter(|p| *p < pages);
    (in_book(left), in_book(right))
}

/// The page a step lands on from `page` in `view`: the next or previous page, or the next or previous
/// spread. `None` at the ends of the book, and in the overview (which is clicked, not stepped).
pub fn neighbour(view: View, page: usize, pages: usize, forward: bool) -> Option<usize> {
    match view {
        View::Page if forward => (page + 1 < pages).then_some(page + 1),
        View::Page => page.checked_sub(1),
        View::Spread => {
            let spread = spread_of(page);
            if forward {
                (spread + 1 < spread_count(pages)).then(|| spread_start(spread + 1))
            } else {
                spread.checked_sub(1).map(spread_start)
            }
        }
        View::Overview => None,
    }
}

/// Changes the view. A spread opens on the spread that holds the current page.
pub fn set_view(run: &mut Run, view: View) {
    run.view = view;
    if view == View::Spread {
        run.page = spread_start(spread_of(run.page));
    }
}

fn page_text(app: &LightcraftApp, index: usize) -> String {
    app.session.book.pages.get(index).map(|p| p.text.clone()).unwrap_or_default()
}

/// Opens the book at its first page (`book.view`).
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    let run = Run {
        page: 0,
        guides: app.session.book.guides.show,
        photo_text: app.session.book.text.photo.clone(),
        page_text: page_text(app, 0),
        dirty: true,
        ..Default::default()
    };
    app.ui.classic_book = Some(run);
    Ok(json!({"pages": app.session.book.pages.len()}))
}

fn go(app: &LightcraftApp, run: &mut Run, page: usize) {
    run.page = page;
    run.page_text = page_text(app, page);
}

/// Draws the book view and handles its keys. It stays open until closed.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let Some(mut run) = app.ui.classic_book.take() else { return };
    let pages = app.session.book.pages.len();
    if run.dirty {
        run.shown.clear();
        run.failed.clear();
        run.thumbs.clear();
        run.failed_thumbs.clear();
        run.dirty = false;
    }
    if pages > 0 && run.page >= pages {
        let last = pages - 1;
        go(app, &mut run, last);
    }
    let typing = ctx.memory(|m| m.focused().is_some());
    let (back, forward) = ctx.input(|i| {
        (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::PageUp), i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::PageDown))
    });
    let mut target: Option<usize> = None;
    if !typing && back {
        target = neighbour(run.view, run.page, pages, false);
    } else if !typing && forward {
        target = neighbour(run.view, run.page, pages, true);
    }
    let mut close = false;
    let mut view = run.view;
    let (mut commit_photo, mut commit_page) = (None, None);
    ui.horizontal(|ui| {
        let (prev, next) = (neighbour(run.view, run.page, pages, false), neighbour(run.view, run.page, pages, true));
        if ui.add_enabled(prev.is_some(), egui::Button::new(crate::i18n::tr("Previous"))).clicked() {
            target = prev;
        }
        ui.label(format!("{} {} / {}", crate::i18n::tr("Page"), run.page + 1, pages.max(1)));
        if ui.add_enabled(next.is_some(), egui::Button::new(crate::i18n::tr("Next"))).clicked() {
            target = next;
        }
        ui.selectable_value(&mut view, View::Page, crate::i18n::tr("Page"));
        ui.selectable_value(&mut view, View::Spread, crate::i18n::tr("Spread"));
        ui.selectable_value(&mut view, View::Overview, crate::i18n::tr("Overview"));
        ui.checkbox(&mut run.guides, crate::i18n::tr("Guides"));
        if ui.button(crate::i18n::tr("Close")).clicked() {
            close = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Photo text"));
        if TextField::singleline("field:bookPhotoText", &mut run.photo_text).width(260.0).show(ui).committed() {
            commit_photo = Some(run.photo_text.clone());
        }
        ui.label(crate::i18n::tr("Page text"));
        if TextField::singleline("field:bookPageText", &mut run.page_text).width(260.0).show(ui).committed() {
            commit_page = Some(run.page_text.clone());
        }
    });
    if let Some(text) = commit_photo {
        match app.session.execute("book.set", &json!({"text": {"photo": text}})) {
            Ok(_) => run.dirty = true,
            Err(e) => app.toast_error(ctx, e.to_string()),
        }
    }
    if let Some(text) = commit_page
        && pages > 0
    {
        match app.session.execute("book.setPage", &json!({"index": run.page, "text": text})) {
            Ok(_) => run.dirty = true,
            Err(e) => app.toast_error(ctx, e.to_string()),
        }
    }
    if view != run.view {
        set_view(&mut run, view);
        let page = run.page;
        go(app, &mut run, page);
    }
    if let Some(page) = target {
        go(app, &mut run, page);
    }
    if pages == 0 {
        ui.label(crate::i18n::tr("The book has no pages yet: auto layout photos into it first (book.autoLayout)."));
    } else {
        let picked = match run.view {
            View::Overview => overview(app, ctx, ui, &mut run).map(|page| {
                run.view = View::Page;
                page
            }),
            View::Page | View::Spread => draw_pages(app, ctx, ui, &mut run),
        };
        if let Some(page) = picked {
            go(app, &mut run, page);
        }
    }
    app.ui.classic_book = if close { None } else { Some(run) };
}

/// The DPI at which a page's long edge is `edge` pixels long.
fn dpi_for(s: &book::Settings, edge: f64) -> f64 {
    (edge / (s.width_mm.max(s.height_mm) / 25.4)).clamp(36.0, 600.0)
}

/// Renders page `index` at `dpi` into a texture, shown through the monitor profile like every other
/// preview.
fn render_texture(app: &mut LightcraftApp, ctx: &egui::Context, model: &Model, index: usize, dpi: f64, name: &str) -> Result<TextureHandle, String> {
    let mut img = book::render_page(&mut app.session, model, index, dpi)?;
    if let Some(d) = app.renderer.display()
        && let Err(e) = d.profile.from_srgb(&mut img)
    {
        log::warn!("display profile: {e}");
    }
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
    Ok(ctx.load_texture(format!("{name}-{index}"), color, egui::TextureOptions::LINEAR))
}

/// The largest rectangle of `aspect` (width over height) centred in `area`.
fn fit(area: Rect, aspect: f32) -> Rect {
    let (w, h) = if area.width() / area.height().max(1.0) > aspect {
        (area.height().max(1.0) * aspect, area.height().max(1.0))
    } else {
        (area.width().max(1.0), area.width().max(1.0) / aspect)
    };
    Rect::from_center_size(area.center(), vec2(w, h))
}

/// The print guides of a page drawn in `rect` when `shown`: the bleed outside the trim, the trim, and
/// the safe area inside it, each with its colour and stroke side. Empty when the guides are off.
fn guide_lines(rect: Rect, model: &Model, shown: bool) -> Vec<(Rect, Color32, StrokeKind)> {
    if !shown {
        return Vec::new();
    }
    let k = rect.width() / model.settings.width_mm as f32;
    let g = &model.guides;
    vec![
        (rect.expand(g.bleed_mm as f32 * k), Color32::from_rgb(230, 70, 70), StrokeKind::Outside),
        (rect, Color32::from_rgb(40, 40, 40), StrokeKind::Inside),
        (rect.shrink(g.safe_mm as f32 * k), Color32::from_rgb(60, 190, 110), StrokeKind::Inside),
    ]
}

/// Draws the pages on screen (one page, or a spread) fitted to the window, with their guides when on.
/// A click on a page returns that page.
fn draw_pages(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, run: &mut Run) -> Option<usize> {
    let model: Model = app.session.book.clone();
    let pages = model.pages.len();
    let visible = run.visible(pages);
    for &index in &visible {
        if run.shown.iter().any(|(i, _)| *i == index) || run.failed.contains(&index) {
            continue;
        }
        match render_texture(app, ctx, &model, index, dpi_for(&model.settings, LONG_EDGE), "classic-book-page") {
            Ok(tex) => run.shown.push((index, tex)),
            Err(e) => {
                run.failed.push(index);
                app.toast_error(ctx, e);
            }
        }
    }
    run.shown.retain(|(i, _)| visible.contains(i));
    let avail = ui.available_rect_before_wrap();
    ui.allocate_rect(avail, Sense::hover());
    let aspect = (model.settings.width_mm / model.settings.height_mm) as f32;
    let spread = run.view == View::Spread;
    let sheet = fit(avail.shrink(12.0), if spread { 2.0 * aspect } else { aspect });
    // the sides: a spread's two pages side by side (an empty side is a grey well), or the one page
    let slots: Vec<(Option<usize>, Rect)> = if spread {
        let page_w = sheet.width() / 2.0;
        let (left, right) = spread_pages(run.page, pages);
        let left_rect = Rect::from_min_size(sheet.min, vec2(page_w, sheet.height()));
        let right_rect = Rect::from_min_size(pos2(sheet.min.x + page_w, sheet.min.y), vec2(page_w, sheet.height()));
        vec![(left, left_rect), (right, right_rect)]
    } else {
        vec![(visible.first().copied(), sheet)]
    };
    let mut picked = None;
    let painter = ui.painter();
    for (slot, side) in slots {
        let Some(index) = slot else {
            painter.rect_filled(side, 0.0_f32, Color32::from_gray(40));
            continue;
        };
        if let Some((_, tex)) = run.shown.iter().find(|(i, _)| *i == index) {
            painter.image(tex.id(), side, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        for (line, color, kind) in guide_lines(side, &model, run.guides) {
            painter.rect_stroke(line, 0.0_f32, Stroke::new(1.0, color), kind);
        }
        if spread && index == run.page {
            painter.rect_stroke(side, 0.0_f32, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
        }
        if ui.interact(side, ui.id().with(("book-side", index)), Sense::click()).clicked() {
            picked = Some(index);
        }
    }
    picked
}

/// Draws every page as a thumbnail in a grid, a few renders per frame, with the current page outlined.
/// A click on a thumbnail returns its page.
fn overview(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, run: &mut Run) -> Option<usize> {
    let model: Model = app.session.book.clone();
    let pages = model.pages.len();
    run.thumbs.resize(pages, None);
    let thumb_dpi = dpi_for(&model.settings, THUMB_EDGE);
    let mut budget = THUMBS_PER_FRAME;
    for index in 0..pages {
        if run.thumbs.get(index).is_some_and(Option::is_some) || run.failed_thumbs.contains(&index) {
            continue;
        }
        if budget == 0 {
            ctx.request_repaint();
            break;
        }
        budget -= 1;
        match render_texture(app, ctx, &model, index, thumb_dpi, "book-thumb") {
            Ok(tex) => {
                if let Some(slot) = run.thumbs.get_mut(index) {
                    *slot = Some(tex);
                }
            }
            Err(e) => {
                run.failed_thumbs.push(index);
                app.toast_error(ctx, e);
            }
        }
    }
    let aspect = (model.settings.width_mm / model.settings.height_mm).max(0.01) as f32;
    let width = ui.available_width().max(THUMB_WIDTH + 2.0 * GAP);
    let cols = ((width - GAP) / (THUMB_WIDTH + GAP)).floor().max(1.0) as usize;
    let cell_h = THUMB_WIDTH / aspect;
    let rows = pages.div_ceil(cols);
    let mut picked = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let (area, _) = ui.allocate_exact_size(vec2(width, rows as f32 * (cell_h + GAP) + GAP), Sense::hover());
        let painter = ui.painter();
        for index in 0..pages {
            let (col, row) = (index % cols, index / cols);
            let origin = area.min + vec2(GAP + col as f32 * (THUMB_WIDTH + GAP), GAP + row as f32 * (cell_h + GAP));
            let cell = Rect::from_min_size(origin, vec2(THUMB_WIDTH, cell_h));
            match run.thumbs.get(index).and_then(Option::as_ref) {
                Some(tex) => {
                    painter.image(tex.id(), cell, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    painter.rect_filled(cell, 0.0_f32, Color32::from_gray(50));
                }
            }
            if index == run.page {
                painter.rect_stroke(cell.expand(3.0), 0.0_f32, Stroke::new(2.0, ACCENT), StrokeKind::Outside);
            }
            if ui.interact(cell, ui.id().with(("book-thumb-cell", index)), Sense::click()).clicked() {
                picked = Some(index);
            }
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_pair_the_pages_with_the_first_on_the_right() {
        // 5 pages: [0] | [1 2] [3 4]
        assert_eq!(spread_count(5), 3);
        assert_eq!(spread_pages(0, 5), (None, Some(0)));
        assert_eq!(spread_pages(1, 5), (Some(1), Some(2)));
        assert_eq!(spread_pages(2, 5), (Some(1), Some(2)));
        assert_eq!(spread_pages(4, 5), (Some(3), Some(4)));
        // an even count leaves the last page alone on the left
        assert_eq!(spread_count(4), 3);
        assert_eq!(spread_pages(3, 4), (Some(3), None));
        assert_eq!((spread_count(1), spread_pages(0, 1)), (1, (None, Some(0))));
        assert_eq!(spread_count(0), 0);
        // a spread opens on its first page, which holds the page it was opened from
        for page in 0..9 {
            assert!(spread_start(spread_of(page)) <= page, "{page}");
            let (l, r) = spread_pages(page, 9);
            assert!(l == Some(page) || r == Some(page), "{page}");
        }
    }

    #[test]
    fn steps_move_by_page_or_by_spread_and_stop_at_the_ends() {
        assert_eq!(neighbour(View::Page, 0, 3, true), Some(1));
        assert_eq!(neighbour(View::Page, 2, 3, true), None);
        assert_eq!(neighbour(View::Page, 0, 3, false), None);
        // spreads: [0] [1 2] [3 4]
        assert_eq!(neighbour(View::Spread, 0, 5, true), Some(1));
        assert_eq!(neighbour(View::Spread, 2, 5, true), Some(3));
        assert_eq!(neighbour(View::Spread, 4, 5, true), None);
        assert_eq!(neighbour(View::Spread, 4, 5, false), Some(1));
        assert_eq!(neighbour(View::Spread, 1, 5, false), Some(0));
        assert_eq!(neighbour(View::Overview, 2, 5, true), None);
    }

    #[test]
    fn switching_views_keeps_the_current_page_in_view() {
        let mut run = Run { page: 2, ..Default::default() };
        assert_eq!(run.visible(5), vec![2]);
        set_view(&mut run, View::Spread);
        assert_eq!(run.page, 1);
        assert_eq!(run.visible(5), vec![1, 2]);
        // the first spread shows page 0 alone
        let mut first = Run::default();
        set_view(&mut first, View::Spread);
        assert_eq!(first.visible(5), vec![0]);
        // the overview shows no page; the page view then shows the current page
        set_view(&mut run, View::Overview);
        assert!(run.visible(5).is_empty());
        set_view(&mut run, View::Page);
        assert_eq!(run.page, 1);
        assert_eq!(run.visible(5), vec![1]);
        // a page past the end of the book is not on screen
        let past = Run { page: 7, ..Default::default() };
        assert!(past.visible(5).is_empty());
    }

    #[test]
    fn guides_toggle_shows_and_hides_the_bleed_and_safe_guides() {
        // a 280 x 210 mm page drawn 560 x 420 px: 2 px per mm, bleed 3 mm, safe area 5 mm
        let model = Model::default();
        let page = Rect::from_min_size(pos2(100.0, 50.0), vec2(560.0, 420.0));
        let mut run = Run::default();
        assert!(!run.guides);
        assert!(guide_lines(page, &model, run.guides).is_empty(), "guides drawn while off");
        run.guides = true;
        let lines = guide_lines(page, &model, run.guides);
        assert_eq!(lines.len(), 3);
        let (bleed, trim, safe) = (lines[0].0, lines[1].0, lines[2].0);
        assert_eq!(trim, page);
        assert!((bleed.min.x - 94.0).abs() < 1e-3 && (bleed.min.y - 44.0).abs() < 1e-3, "bleed {bleed:?}");
        assert!((bleed.width() - 572.0).abs() < 1e-3, "bleed {bleed:?}");
        assert!((safe.min.x - 110.0).abs() < 1e-3 && (safe.min.y - 60.0).abs() < 1e-3, "safe {safe:?}");
        assert!((safe.width() - 540.0).abs() < 1e-3, "safe {safe:?}");
        // the toggle hides them again
        run.guides = false;
        assert!(guide_lines(page, &model, run.guides).is_empty());
    }
}
