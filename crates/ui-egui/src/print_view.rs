//! The Classic Print view (`print.view`): the print's pages one at a time on the paper, with the
//! layout, image, guide, job and page settings beside them, the user's print templates and a PDF
//! export. The settings are the engine's (`print.set`) and the preview is the engine's page render at
//! 72 ppi. The guides are drawn over the preview only, never on the output. The view keeps no print of
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
/// The resolutions offered, in pixels per inch.
const RESOLUTIONS: [f64; 3] = [150.0, 300.0, 600.0];
/// Room left round the page on screen, so the bleed guide shows, in points.
const GUIDE_ROOM: f32 = 24.0;
/// Length of a ruler tick on the page, in points.
const TICK: f32 = 6.0;
/// Ruler labels every this many millimetres.
const LABEL_MM: f64 = 50.0;

/// The print view's state: the page shown, the text being typed and the template picked. Not saved.
#[derive(Clone, Debug, Default)]
pub struct Run {
    /// The page shown, from 0.
    pub page: usize,
    /// The preview last drawn: its key (settings, page and photos).
    shown: Option<String>,
    /// The identity plate and info line as they are being typed, until the edit ends.
    identity: Option<String>,
    info: Option<String>,
    /// The template name as it is typed, for saving or renaming.
    template_name: Option<String>,
    /// The template last picked in the list.
    template: Option<String>,
}

/// One guide on the preview: the trim, the bleed beyond it, a cell's outline or a short ruler tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Guide {
    Trim(egui::Rect),
    Bleed(egui::Rect),
    Cell(egui::Rect),
    Tick { from: egui::Pos2, to: egui::Pos2 },
}

/// Opens the print view (`print.view`); it prints the selected photos, or the visible ones.
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    app.ui.classic_print = Some(Run::default());
    Ok(Value::Null)
}

fn preview_id() -> egui::Id {
    egui::Id::new("classic-print-page")
}

/// The point on the preview `page` for the millimetre position (`x`, `y`) of a paper `paper_w` wide.
fn at(page: egui::Rect, paper_w: f64, x: f64, y: f64) -> egui::Pos2 {
    let k = page.width() / paper_w as f32;
    egui::pos2(page.left() + x as f32 * k, page.top() + y as f32 * k)
}

/// The guides laid over the preview `page`, in the order they are drawn.
pub(crate) fn guide_strokes(lines: &print::GuideLines, page: egui::Rect) -> Vec<Guide> {
    let w = lines.paper.0;
    let mut out = Vec::new();
    if let Some(b) = lines.bleed {
        out.push(Guide::Bleed(egui::Rect::from_min_max(at(page, w, b.x, b.y), at(page, w, b.x + b.w, b.y + b.h))));
    }
    out.push(Guide::Trim(page));
    for &x in &lines.ruler_x {
        let from = at(page, w, x, 0.0);
        out.push(Guide::Tick { from, to: from + egui::vec2(0.0, TICK) });
    }
    for &y in &lines.ruler_y {
        let from = at(page, w, 0.0, y);
        out.push(Guide::Tick { from, to: from + egui::vec2(TICK, 0.0) });
    }
    for (cell, _) in &lines.cells {
        out.push(Guide::Cell(egui::Rect::from_min_max(at(page, w, cell.x, cell.y), at(page, w, cell.x + cell.w, cell.y + cell.h))));
    }
    out
}

/// Draws the guides over the preview `page`, with the cell sizes and the ruler labels.
fn draw_guides(ui: &egui::Ui, lines: &print::GuideLines, page: egui::Rect) {
    let painter = ui.painter();
    let trim = egui::Color32::from_rgb(40, 40, 40);
    let bleed = egui::Color32::from_rgb(230, 70, 70);
    let cell = egui::Color32::from_rgb(60, 190, 110);
    let font = egui::FontId::proportional(10.0);
    for g in guide_strokes(lines, page) {
        match g {
            Guide::Trim(r) => {
                painter.rect_stroke(r, 0.0_f32, egui::Stroke::new(1.0, trim), egui::StrokeKind::Inside);
            }
            Guide::Bleed(r) => {
                painter.rect_stroke(r, 0.0_f32, egui::Stroke::new(1.0, bleed), egui::StrokeKind::Outside);
            }
            Guide::Cell(r) => {
                painter.rect_stroke(r, 0.0_f32, egui::Stroke::new(1.0, cell), egui::StrokeKind::Inside);
            }
            Guide::Tick { from, to } => {
                painter.line_segment([from, to], egui::Stroke::new(1.0, trim));
            }
        }
    }
    for (c, label) in &lines.cells {
        let centre = at(page, lines.paper.0, c.x + c.w / 2.0, c.y + c.h / 2.0);
        painter.text(centre, egui::Align2::CENTER_CENTER, label, font.clone(), trim);
    }
    for x in lines.ruler_x.iter().copied().filter(|x| x % LABEL_MM < 1e-6) {
        let at_top = at(page, lines.paper.0, x, 0.0) - egui::vec2(0.0, TICK);
        painter.text(at_top, egui::Align2::CENTER_BOTTOM, format!("{x:.0}"), font.clone(), trim);
    }
}

/// Draws the print view: the settings on the left, the page at the right.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let Some(run) = app.ui.classic_print.clone() else { return };
    let ids = {
        let selected = app.session.selected();
        if selected.is_empty() { app.session.visible_cloned() } else { selected }
    };
    let model = app.session.print.clone();
    let user: Vec<String> = app.session.print_templates.iter().map(|t| t.name.clone()).collect();
    let pages = print::plan(&model, ids.len()).map(|p| p.len()).unwrap_or(0);
    let page = run.page.min(pages.saturating_sub(1));
    let mut next = run.clone();
    next.page = page;
    let (mut close, mut export) = (false, false);
    // The engine command the panel asked for this frame; it replaces the settings patch for the frame.
    let mut command: Option<(&'static str, Value)> = None;
    let (mut layout, mut image, mut texts) = (model.layout.clone(), model.image.clone(), model.page.clone());
    let (mut guides, mut job) = (model.guides.clone(), model.job.clone());
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
        ui.label(crate::i18n::tr("Print template"));
        let mut chosen = run.template.clone();
        let names: Vec<String> = print::builtin_templates().iter().map(|s| s.to_string()).chain(user.iter().cloned()).collect();
        egui::ComboBox::from_id_salt("printTemplate").selected_text(chosen.clone().unwrap_or_default()).show_ui(ui, |ui| {
            for name in &names {
                ui.selectable_value(&mut chosen, Some(name.clone()), name.as_str());
            }
        });
        if chosen != run.template
            && let Some(name) = chosen.clone()
        {
            command = Some(("print.applyTemplate", json!({"name": name})));
        }
        next.template = chosen.clone();
        let selected_user = chosen.clone().filter(|t| user.contains(t));
        ui.label(crate::i18n::tr("Template name"));
        let mut name = run.template_name.clone().unwrap_or_default();
        let _ = TextField::singleline("printTemplateName", &mut name).show(ui);
        let typed = name.trim().to_string();
        next.template_name = Some(name.clone());
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!typed.is_empty(), egui::Button::new(crate::i18n::tr("Save template"))).clicked() {
                command = Some(("print.saveTemplate", json!({"name": typed})));
                next.template_name = Some(String::new());
            }
            if ui.add_enabled(selected_user.is_some() && !typed.is_empty(), egui::Button::new(crate::i18n::tr("Rename template"))).clicked()
                && let Some(from) = &selected_user
            {
                command = Some(("print.renameTemplate", json!({"name": from, "to": typed})));
                next.template = Some(typed.clone());
                next.template_name = Some(String::new());
            }
            if ui.add_enabled(selected_user.is_some(), egui::Button::new(crate::i18n::tr("Delete template"))).clicked()
                && let Some(from) = &selected_user
            {
                command = Some(("print.deleteTemplate", json!({"name": from})));
                next.template = None;
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
        ui.label(crate::i18n::tr("Resolution"));
        egui::ComboBox::from_id_salt("printResolution").selected_text(format!("{:.0} ppi", job.ppi)).show_ui(ui, |ui| {
            for ppi in RESOLUTIONS {
                ui.selectable_value(&mut job.ppi, ppi, format!("{ppi:.0} ppi"));
            }
        });
        ui.separator();
        ui.checkbox(&mut guides.show, crate::i18n::tr("Show guides"));
        ui.add_enabled_ui(guides.show, |ui| {
            ui.checkbox(&mut guides.rulers, crate::i18n::tr("Rulers"));
            ui.checkbox(&mut guides.dimensions, crate::i18n::tr("Cell dimensions"));
            ui.add(egui::DragValue::new(&mut guides.bleed_mm).range(0.0..=10.0).speed(0.1).suffix(" mm bleed"));
        });
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
    if command.is_none() && (layout != model.layout || image != model.image || texts != model.page || guides != model.guides || job != model.job) {
        let patch = json!({"layout": layout, "image": image, "page": texts, "guides": guides, "job": job});
        if let Err(e) = app.run("print.set", patch) {
            app.toast_error(ctx, e);
        }
    }
    if let Some((id, params)) = command
        && let Err(e) = app.run(id, params)
    {
        app.toast_error(ctx, e);
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
            let area = ui.max_rect().shrink(GUIDE_ROOM);
            let scale = (area.width() / size.x.max(1.0)).min(area.height() / size.y.max(1.0));
            let page_rect = egui::Rect::from_center_size(ui.max_rect().center(), size * scale);
            ui.put(page_rect, egui::Image::new((tex.id(), size * scale)));
            if let Ok(Some(lines)) = print::guide_lines(&model) {
                draw_guides(ui, &lines, page_rect);
            }
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

/// Asks for a PDF path, then writes the print there (`print.export`), at the job's resolution.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_fall_on_the_preview_page_and_the_bleed_surrounds_it() {
        let mut m = print::Model::default();
        m.layout.style = "single".into();
        let page = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(210.0, 297.0));
        let lines = print::guide_lines(&m).unwrap().unwrap();
        let strokes = guide_strokes(&lines, page);
        let bleed = strokes.iter().find_map(|g| if let Guide::Bleed(r) = g { Some(*r) } else { None }).unwrap();
        assert_eq!(bleed, egui::Rect::from_min_max(egui::pos2(97.0, 47.0), egui::pos2(313.0, 350.0)), "3 mm beyond the trim on each side");
        assert!(strokes.contains(&Guide::Trim(page)), "the trim is the page");
        assert_eq!(strokes.iter().filter(|g| matches!(g, Guide::Tick { .. })).count(), 21 + 30, "rulers every 10 mm");
        assert_eq!(strokes.iter().filter(|g| matches!(g, Guide::Cell(_))).count(), 1, "the cell outline");
        // switched off in the settings, only the trim is left
        m.guides = print::Guides { rulers: false, dimensions: false, bleed_mm: 0.0, show: true };
        let plain = guide_strokes(&print::guide_lines(&m).unwrap().unwrap(), page);
        assert_eq!(plain, vec![Guide::Trim(page)]);
    }
}
