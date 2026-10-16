//! Classic Book view (`book.view`): one page of the book drawn in the window, with page navigation,
//! the print guides (bleed outside the trim, safe area inside it) and the photo and page text
//! fields. Pages are rendered by the engine ([`lightcraft_engine::book::render_page`]) at a preview
//! resolution, and a page is rendered again only when it changes. Escape closes the view.

use crate::LightcraftApp;
use crate::text_field::TextField;
use egui::{Color32, Rect, Stroke, StrokeKind, TextureHandle, pos2, vec2};
use lightcraft_engine::book::{self, Model};
use serde_json::{Value, json};

/// Long edge of the page preview, in pixels.
const LONG_EDGE: f64 = 1200.0;

/// The view's state: the page shown and the text being edited. Not saved.
#[derive(Clone, Debug, Default)]
pub struct Run {
    /// Index of the page shown.
    pub page: usize,
    /// Guides drawn over the page.
    pub guides: bool,
    photo_text: String,
    page_text: String,
    /// The page must be rendered again before it is drawn.
    dirty: bool,
}

fn texture_id() -> egui::Id {
    egui::Id::new("classic-book-page")
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
    };
    app.ui.classic_book = Some(run);
    Ok(json!({"pages": app.session.book.pages.len()}))
}

fn go(app: &LightcraftApp, run: &mut Run, page: usize) {
    run.page = page;
    run.page_text = page_text(app, page);
    run.dirty = true;
}

/// Draws the book view and handles its keys. It stays open until closed.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let Some(mut run) = app.ui.classic_book.take() else { return };
    let pages = app.session.book.pages.len();
    let typing = ctx.memory(|m| m.focused().is_some());
    let (back, forward) = ctx.input(|i| {
        (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::PageUp), i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::PageDown))
    });
    let mut step: Option<usize> = None;
    if !typing && forward && run.page + 1 < pages {
        step = Some(run.page + 1);
    }
    if !typing && back && run.page > 0 {
        step = Some(run.page - 1);
    }
    let mut close = false;
    let (mut commit_photo, mut commit_page) = (None, None);
    ui.horizontal(|ui| {
        if ui.add_enabled(run.page > 0, egui::Button::new(crate::i18n::tr("Previous"))).clicked() {
            step = Some(run.page.saturating_sub(1));
        }
        ui.label(format!("{} {} / {}", crate::i18n::tr("Page"), run.page + 1, pages.max(1)));
        if ui.add_enabled(run.page + 1 < pages, egui::Button::new(crate::i18n::tr("Next"))).clicked() {
            step = Some(run.page + 1);
        }
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
    if let Some(page) = step {
        go(app, &mut run, page);
    }
    if pages == 0 {
        ui.label(crate::i18n::tr("The book has no pages yet: auto layout photos into it first (book.autoLayout)."));
    } else {
        draw_page(app, ctx, ui, &mut run);
    }
    app.ui.classic_book = if close { None } else { Some(run) };
}

/// Draws the current page fitted to the window, with its guides when they are on.
fn draw_page(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, run: &mut Run) {
    let model: Model = app.session.book.clone();
    let s = &model.settings;
    if run.dirty || ctx.data(|d| d.get_temp::<TextureHandle>(texture_id())).is_none() {
        let dpi = (LONG_EDGE / (s.width_mm.max(s.height_mm) / 25.4)).clamp(36.0, 600.0);
        let rendered = book::render_page(&mut app.session, &model, run.page, dpi);
        run.dirty = false;
        match rendered {
            Ok(mut img) => {
                // shown through the monitor profile like every other preview
                if let Some(d) = app.renderer.display()
                    && let Err(e) = d.profile.from_srgb(&mut img)
                {
                    log::warn!("display profile: {e}");
                }
                let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
                let tex = ctx.load_texture("classic-book-page", color, egui::TextureOptions::LINEAR);
                ctx.data_mut(|d| d.insert_temp(texture_id(), tex));
            }
            Err(e) => {
                ctx.data_mut(|d| d.remove::<TextureHandle>(texture_id()));
                app.toast_error(ctx, e);
            }
        }
    }
    let avail = ui.available_rect_before_wrap();
    ui.allocate_rect(avail, egui::Sense::hover());
    let Some(tex) = ctx.data(|d| d.get_temp::<TextureHandle>(texture_id())) else { return };
    let aspect = (s.width_mm / s.height_mm) as f32;
    let area = avail.shrink(12.0);
    let (w, h) = if area.width() / area.height().max(1.0) > aspect {
        (area.height().max(1.0) * aspect, area.height().max(1.0))
    } else {
        (area.width().max(1.0), area.width().max(1.0) / aspect)
    };
    let rect = Rect::from_center_size(avail.center(), vec2(w, h));
    let painter = ui.painter();
    painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    if run.guides {
        let k = rect.width() / s.width_mm as f32;
        let g = &model.guides;
        painter.rect_stroke(rect.expand(g.bleed_mm as f32 * k), 0.0_f32, Stroke::new(1.0, Color32::from_rgb(230, 70, 70)), StrokeKind::Outside);
        painter.rect_stroke(rect, 0.0_f32, Stroke::new(1.0, Color32::from_rgb(40, 40, 40)), StrokeKind::Inside);
        painter.rect_stroke(rect.shrink(g.safe_mm as f32 * k), 0.0_f32, Stroke::new(1.0, Color32::from_rgb(60, 190, 110)), StrokeKind::Inside);
    }
}
