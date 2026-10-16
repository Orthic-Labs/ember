//! The Classic Print view (`print.view`): the print's pages one at a time on the paper, with the
//! layout, image and page settings beside them and a PDF export. The settings are the engine's
//! (`print.set`) and the preview is the engine's page render at 72 ppi; the view keeps no print of
//! its own. Text boxes keep a draft while they are being typed in, so a frame does not reset them.

use crate::LightcraftApp;
use crate::pick::{PickRequest, Picked};
use crate::text_field::TextField;
use lightcraft_engine::print;
use serde_json::{Value, json};

/// Preview resolution in pixels per inch: the page on screen, not the print.
const PREVIEW_PPI: f64 = 72.0;
/// The layout styles, as the engine names them.
const STYLES: [&str; 4] = ["single", "contact", "package", "custom"];
/// The papers, as the engine names them.
const PAPERS: [&str; 2] = ["a4", "letter"];

/// The print view's state: the page shown and the text being typed. Not saved.
#[derive(Clone, Debug, Default)]
pub struct Run {
    /// The page shown, from 0.
    pub page: usize,
    /// The preview last drawn: its key (settings, page and photos).
    shown: Option<String>,
    /// The identity plate and info line as they are being typed, until the edit ends.
    identity: Option<String>,
    info: Option<String>,
}

/// Opens the print view (`print.view`); it prints the selected photos, or the visible ones.
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    app.ui.classic_print = Some(Run::default());
    Ok(Value::Null)
}

fn preview_id() -> egui::Id {
    egui::Id::new("classic-print-page")
}

/// Draws the print view: the settings on the left, the page at the right.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let Some(run) = app.ui.classic_print.clone() else { return };
    let ids = {
        let selected = app.session.selected();
        if selected.is_empty() { app.session.visible_cloned() } else { selected }
    };
    let model = app.session.print.clone();
    let pages = print::plan(&model, ids.len()).map(|p| p.len()).unwrap_or(0);
    let page = run.page.min(pages.saturating_sub(1));
    let mut next = run.clone();
    next.page = page;
    let (mut close, mut export) = (false, false);
    let (mut layout, mut image, mut texts) = (model.layout.clone(), model.image.clone(), model.page.clone());
    egui::SidePanel::left("classicPrintSettings").resizable(false).default_width(270.0).show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.button(crate::i18n::tr("Close")).clicked() {
                close = true;
            }
            if ui.add_enabled(pages > 0, egui::Button::new(crate::i18n::tr("Export PDF…"))).clicked() {
                export = true;
            }
        });
        ui.separator();
        egui::ComboBox::from_id_salt("printStyle").selected_text(layout.style.clone()).show_ui(ui, |ui| {
            for style in STYLES {
                ui.selectable_value(&mut layout.style, style.to_string(), style);
            }
        });
        egui::ComboBox::from_id_salt("printPaper").selected_text(layout.paper.clone()).show_ui(ui, |ui| {
            for paper in PAPERS {
                ui.selectable_value(&mut layout.paper, paper.to_string(), paper);
            }
        });
        ui.checkbox(&mut layout.landscape, crate::i18n::tr("Landscape"));
        ui.add(egui::DragValue::new(&mut layout.columns).range(1..=8).prefix("Columns "));
        ui.add(egui::DragValue::new(&mut layout.rows).range(1..=10).prefix("Rows "));
        ui.add(egui::DragValue::new(&mut layout.margin_mm).range(0.0..=50.0).suffix(" mm margin"));
        ui.add(egui::DragValue::new(&mut layout.spacing_mm).range(0.0..=20.0).suffix(" mm spacing"));
        ui.separator();
        ui.checkbox(&mut image.zoom_to_fill, crate::i18n::tr("Zoom to fill"));
        ui.checkbox(&mut image.rotate_to_fit, crate::i18n::tr("Rotate to fit"));
        ui.checkbox(&mut image.repeat_one, crate::i18n::tr("Repeat one photo per page"));
        ui.add(egui::DragValue::new(&mut image.stroke_pt).range(0.0..=10.0).speed(0.1).suffix(" pt stroke"));
        ui.separator();
        ui.checkbox(&mut texts.captions, crate::i18n::tr("Filename captions"));
        let mut identity = run.identity.clone().unwrap_or_else(|| model.page.identity_plate.clone());
        ui.label(crate::i18n::tr("Identity plate"));
        let r = TextField::singleline("printIdentity", &mut identity).show(ui);
        next.identity = r.editing.then(|| identity.clone());
        if r.committed() {
            texts.identity_plate = identity;
        }
        let mut info = run.info.clone().unwrap_or_else(|| model.page.info_text.clone());
        ui.label(crate::i18n::tr("Info text (metadata fields in braces)"));
        let r = TextField::singleline("printInfo", &mut info).show(ui);
        next.info = r.editing.then(|| info.clone());
        if r.committed() {
            texts.info_text = info;
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled(page > 0, egui::Button::new(crate::i18n::tr("Previous page"))).clicked() {
                next.page = page - 1;
            }
            if ui.add_enabled(page + 1 < pages, egui::Button::new(crate::i18n::tr("Next page"))).clicked() {
                next.page = page + 1;
            }
        });
    });
    if layout != model.layout || image != model.image || texts != model.page {
        let patch = json!({"layout": layout, "image": image, "page": texts});
        if let Err(e) = app.run("print.set", patch) {
            app.toast_error(ctx, e);
        }
    }
    if pages == 0 {
        ui.label(crate::i18n::tr("Select photos to print."));
    } else {
        ui.label(format!("{} {} / {}", crate::i18n::tr("Page"), page + 1, pages));
        let key = format!("{}|{}|{:?}", serde_json::to_string(&model).unwrap_or_default(), page, ids);
        if run.shown.as_deref() != Some(key.as_str()) {
            next.shown = Some(key);
            match print::render_page(&mut app.session, &model, &ids, page, PREVIEW_PPI) {
                Ok(mut img) => {
                    if let Some(d) = app.renderer.display()
                        && let Err(e) = d.profile.from_srgb(&mut img)
                    {
                        log::warn!("display profile: {e}");
                    }
                    let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
                    let tex = ctx.load_texture("classic-print-page", color, egui::TextureOptions::LINEAR);
                    ctx.data_mut(|d| d.insert_temp(preview_id(), tex));
                }
                Err(e) => app.toast_error(ctx, e),
            }
        }
        if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(preview_id())) {
            let size = tex.size_vec2();
            let rect = ui.max_rect();
            let scale = (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0));
            ui.centered_and_justified(|ui| {
                ui.add(egui::Image::new((tex.id(), size * scale)));
            });
        }
    }
    if export {
        export_pdf(app, ctx);
    }
    if close {
        app.ui.classic_print = None;
    } else if let Some(open) = app.ui.classic_print.as_mut() {
        *open = next;
    }
}

/// Asks for a PDF path, then writes the print there (`print.export`).
fn export_pdf(app: &mut LightcraftApp, ctx: &egui::Context) {
    let params = json!({"format": "pdf"});
    let request = PickRequest::save("Print PDF", "PDF", &["pdf"], "Print.pdf");
    match crate::pick::ask(app, "print.export", &params, "path", request, |_| None) {
        Picked::Now(paths) => {
            let Some(path) = paths.first() else { return };
            let mut params = params;
            params["path"] = json!(path);
            match app.run("print.export", params) {
                Ok(done) => app.toast(ctx, format!("Printed {} pages to {}", done["pages"], path)),
                Err(e) => app.toast_error(ctx, e),
            }
        }
        Picked::Later => {}
        Picked::Unavailable => app.toast_error(ctx, "no save dialog on this platform; print.export needs a path"),
    }
}
