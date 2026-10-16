//! Classic Slideshow playback (`slideshow.play`): the engine's timeline drawn full screen on the
//! backdrop, paused and stepped with the keys. Space pauses, the arrow and page keys step, Escape
//! ends it, and it ends by itself after the last slide unless the show repeats. The frame is the
//! whole view: there is no progress window.
//!
//! Each slide is rendered once through the export pipeline and kept while it shows; a fade mixes
//! two kept slides. The renders run on the UI thread, so a show with large photos can stutter at
//! a cross-fade's start.

use crate::LightcraftApp;
use crate::pick::{PickRequest, Picked};
use crate::text_field::TextField;
use lightcraft_engine::Session;
use lightcraft_engine::slideshow::{self, Frame, Model, Timeline};
use lightcraft_raster::Rgba8;
use serde_json::{Value, json};

/// Long edge of the playback frame, in pixels.
const LONG_EDGE: usize = 1600;
/// Rendered slides kept: the one showing, the one it fades from, and one spare.
const KEPT: usize = 3;
/// Fade steps shown (a new frame is drawn only when the fade moves to the next step).
const FADE_STEPS: f64 = 32.0;

/// One playback: the settings and photos fixed when it started.
#[derive(Clone, Debug)]
pub struct Run {
    model: Model,
    timeline: Timeline,
    size: (usize, usize),
    /// Egui time the show started, moved by each pause.
    started: f64,
    /// Seconds into the show while paused (`None` while playing).
    paused: Option<f64>,
    /// Rendered slides by timeline index.
    slides: Vec<(usize, Rgba8)>,
    /// The frame last drawn: (current, previous, fade step).
    shown: Option<(usize, Option<usize>, u32)>,
}

impl Run {
    /// A playback that starts at the first slide: running when the show advances by itself, paused
    /// (stepped with the keys) when it is manual.
    fn new(model: Model, timeline: Timeline, size: (usize, usize), now: f64) -> Run {
        let paused = if model.playback.auto { None } else { Some(0.0) };
        Run { model, timeline, size, started: now, paused, slides: Vec::new(), shown: None }
    }

    fn elapsed(&self, now: f64) -> f64 {
        self.paused.unwrap_or(now - self.started)
    }

    fn seek(&mut self, to: f64, now: f64) {
        if self.paused.is_some() {
            self.paused = Some(to);
        } else {
            self.started = now - to;
        }
    }

    /// Pauses at the current slide time, or resumes from the paused one.
    fn toggle(&mut self, now: f64) {
        match self.paused.take() {
            Some(at) => self.started = now - at,
            None => self.paused = Some(now - self.started),
        }
    }

    /// Moves to the slide after the current one (or its start, or the one before it). `false`
    /// when there is no such slide: the show is over.
    fn step(&mut self, now: f64, forward: bool) -> bool {
        let elapsed = self.elapsed(now);
        let Some(frame) = self.timeline.frame_at(elapsed) else { return false };
        let to = if forward {
            let next = frame.current + 1;
            if next < self.timeline.entries.len() {
                self.timeline.start_of(next)
            } else if self.timeline.repeat {
                0.0
            } else {
                return false;
            }
        } else {
            let here = self.timeline.start_of(frame.current);
            // back past the start of this slide only when it has barely begun
            if elapsed - here > 0.5 || frame.current == 0 { here } else { self.timeline.start_of(frame.current - 1) }
        };
        self.seek(to, now);
        true
    }

    fn ensure(&mut self, session: &mut Session, index: usize) -> Result<(), String> {
        if self.slides.iter().any(|(i, _)| *i == index) {
            return Ok(());
        }
        let slide = self.timeline.slide(index).ok_or("no such slide")?;
        let (w, h) = self.size;
        let img = slideshow::render_slide(session, &self.model, slide, w, h)?;
        self.slides.push((index, img));
        while self.slides.len() > KEPT {
            self.slides.remove(0);
        }
        Ok(())
    }

    fn kept(&self, index: usize) -> Option<&Rgba8> {
        self.slides.iter().find(|(i, _)| *i == index).map(|(_, img)| img)
    }

    /// The picture for `frame`: its current slide, faded over the previous one.
    fn compose(&mut self, session: &mut Session, frame: Frame) -> Result<Rgba8, String> {
        self.ensure(session, frame.current)?;
        let current = self.kept(frame.current).cloned().ok_or("slide not rendered")?;
        if frame.alpha >= 1.0 {
            return Ok(current);
        }
        let from = match frame.previous {
            Some(p) => {
                self.ensure(session, p)?;
                self.kept(p).cloned().ok_or("slide not rendered")?
            }
            None => slideshow::backdrop_for(session, &self.model, self.size.0, self.size.1)?,
        };
        Ok(slideshow::blend(&from, &current, frame.alpha))
    }
}

/// Starts a playback of the selected photos (or the visible ones) with the current slideshow settings.
pub fn start(app: &mut LightcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let mut ids = app.session.selected();
    if ids.is_empty() {
        ids = app.session.visible_cloned();
    }
    if ids.is_empty() {
        return Err("no photos to show".into());
    }
    let model = app.session.slideshow.clone();
    let size = slideshow::frame_size(&model, LONG_EDGE)?;
    let timeline = Timeline::new(&model, &ids);
    if timeline.entries.is_empty() {
        return Err("no slides: select photos or add a title".into());
    }
    let slides = timeline.entries.len();
    let duration = timeline.total;
    app.ui.classic_slideshow = Some(Run::new(model, timeline, size, ctx.input(|i| i.time)));
    Ok(json!({"slides": slides, "seconds": duration}))
}

/// Draws the frame of the running playback and handles its keys; ends it when it is over.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let now = ctx.input(|i| i.time);
    let (toggle, forward, back, close) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::Space),
            i.key_pressed(egui::Key::ArrowRight)
                || i.key_pressed(egui::Key::ArrowDown)
                || i.key_pressed(egui::Key::PageDown)
                || i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::ArrowLeft)
                || i.key_pressed(egui::Key::ArrowUp)
                || i.key_pressed(egui::Key::PageUp)
                || i.key_pressed(egui::Key::Backspace),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if close {
        app.ui.classic_slideshow = None;
        return;
    }
    let Some(run) = app.ui.classic_slideshow.as_mut() else { return };
    if toggle {
        run.toggle(now);
    }
    if (forward || back) && !run.step(now, forward) {
        app.ui.classic_slideshow = None;
        return;
    }
    let Some(run) = app.ui.classic_slideshow.as_mut() else { return };
    let Some(frame) = run.timeline.frame_at(run.elapsed(now)) else {
        app.ui.classic_slideshow = None;
        return;
    };
    let key = (frame.current, frame.previous, (frame.alpha * FADE_STEPS).round() as u32);
    if run.shown != Some(key) {
        let mut img = match run.compose(&mut app.session, frame) {
            Ok(img) => img,
            Err(e) => {
                log::warn!("slideshow frame: {e}");
                app.ui.classic_slideshow = None;
                return;
            }
        };
        // shown through the monitor profile like every other preview
        if let Some(d) = app.renderer.display()
            && let Err(e) = d.profile.from_srgb(&mut img)
        {
            log::warn!("display profile: {e}");
        }
        let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
        let tex = ctx.load_texture("classic-slideshow-frame", color, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(frame_id(), tex));
        run.shown = Some(key);
    }
    if run.paused.is_none() {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
    let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(frame_id())) else { return };
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
    let size = tex.size_vec2();
    let scale = (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0));
    ui.centered_and_justified(|ui| {
        ui.add(egui::Image::new((tex.id(), size * scale)));
    });
}

/// Long edge of the module's preview, in pixels.
const PREVIEW_LONG_EDGE: usize = 960;

/// The Classic Slideshow module view's state (`slideshow.view`): the slide shown, the text fields
/// being typed in and the music length and template name. Not saved; the settings are the engine's
/// (`slideshow.set`), and the preview is the engine's slide render.
#[derive(Clone, Debug)]
pub struct Module {
    /// The slide shown, by timeline index.
    pub slide: usize,
    /// The preview last drawn: its key (settings, slide and photos).
    shown: Option<String>,
    /// Text fields being typed in, until their edit ends: (field, text).
    drafts: Vec<(&'static str, String)>,
    /// The length of the music typed for "Fit slides to music", in seconds.
    pub music_seconds: f64,
    /// The name typed for a template to save.
    pub template_name: String,
    /// The template picked in the list, until it is applied again or deleted.
    pub template: Option<String>,
}

impl Default for Module {
    fn default() -> Self {
        Self { slide: 0, shown: None, drafts: Vec::new(), music_seconds: 180.0, template_name: String::new(), template: None }
    }
}

/// Opens the module view (`slideshow.view`); it shows the selected photos, or the visible ones.
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    app.ui.classic_slideshow_view = Some(Module::default());
    Ok(Value::Null)
}

fn preview_id() -> egui::Id {
    egui::Id::new("classic-slideshow-module-slide")
}

/// A single-line setting: its draft while it is typed, and `Some(text)` on the frame the edit ends.
fn text_setting(ui: &mut egui::Ui, drafts: &mut Vec<(&'static str, String)>, id: &'static str, label: &str, current: &str) -> Option<String> {
    ui.label(label);
    let mut text = drafts.iter().find(|(k, _)| *k == id).map_or_else(|| current.to_string(), |(_, t)| t.clone());
    let r = TextField::singleline(id, &mut text).show(ui);
    drafts.retain(|(k, _)| *k != id);
    if r.editing {
        drafts.push((id, text.clone()));
    }
    r.committed().then_some(text)
}

/// The slide at `index` rendered for the preview, at the preview's size.
fn render_preview(session: &mut Session, model: &Model, timeline: &Timeline, index: usize) -> Result<lightcraft_raster::Rgba8, String> {
    let (w, h) = slideshow::frame_size(model, PREVIEW_LONG_EDGE)?;
    let slide = timeline.slide(index).ok_or("no slide to show")?;
    slideshow::render_slide(session, model, slide, w, h)
}

/// Draws the module view: the settings on the left, the slide at the right.
pub fn show_module(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let Some(run) = app.ui.classic_slideshow_view.clone() else { return };
    let ids = {
        let selected = app.session.selected();
        if selected.is_empty() { app.session.visible_cloned() } else { selected }
    };
    let model = app.session.slideshow.clone();
    let timeline = Timeline::new(&model, &ids);
    let slides = timeline.entries.len();
    let slide = run.slide.min(slides.saturating_sub(1));
    let mut next = run.clone();
    next.slide = slide;
    let mut m = model.clone();
    let mut templates: Vec<String> = slideshow::builtin_templates().into_iter().map(|t| t.name).collect();
    let user: Vec<String> = app.session.slideshow_templates.iter().map(|t| t.name.clone()).collect();
    templates.extend(user.iter().cloned());
    // the backdrop photo is one of the photos shown; its name comes from the catalog
    let picks: Vec<(u64, String)> = ids.iter().filter_map(|id| app.session.catalog.photo(*id).map(|p| (id.0, p.file_name.clone()))).collect();
    let backdrop_name = model.backdrop.image.and_then(|id| app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).map(|p| p.file_name.clone()));
    let (mut close, mut export, mut export_jpeg, mut play, mut fit, mut save) = (false, false, false, false, false, false);
    let (mut chosen, mut delete): (Option<String>, Option<String>) = (None, None);
    egui::SidePanel::left("classicSlideshowSettings").resizable(false).default_width(290.0).show_inside(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui.button(crate::i18n::tr("Close")).clicked() {
                close = true;
            }
            if ui.add_enabled(slides > 0, egui::Button::new(crate::i18n::tr("Play"))).clicked() {
                play = true;
            }
            if ui.add_enabled(slides > 0, egui::Button::new(crate::i18n::tr("Export PDF…"))).clicked() {
                export = true;
            }
            if ui.add_enabled(slides > 0, egui::Button::new(crate::i18n::tr("Export JPEGs…"))).clicked() {
                export_jpeg = true;
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let picked = run.template.clone();
            let shown = picked.clone().unwrap_or_else(|| crate::i18n::tr("Apply template").to_string());
            egui::ComboBox::from_id_salt("slideshowTemplate").selected_text(shown).show_ui(ui, |ui| {
                for name in &templates {
                    if ui.selectable_label(picked.as_deref() == Some(name.as_str()), name.as_str()).clicked() {
                        chosen = Some(name.clone());
                    }
                }
            });
            // a user template can be deleted; the built-in ones stay
            let user_pick = run.template.clone().filter(|name| user.contains(name));
            if ui.add_enabled(user_pick.is_some(), egui::Button::new(crate::i18n::tr("Delete template"))).clicked() {
                delete = user_pick;
            }
        });
        ui.horizontal(|ui| {
            TextField::singleline("slideshowTemplateName", &mut next.template_name).show(ui);
            let named = !next.template_name.trim().is_empty();
            if ui.add_enabled(named, egui::Button::new(crate::i18n::tr("Save as template"))).clicked() {
                save = true;
            }
        });
        ui.separator();
        ui.label(crate::i18n::tr("Aspect (width ÷ height)"));
        ui.add(egui::DragValue::new(&mut m.layout.aspect).range(0.2..=5.0).speed(0.01));
        ui.label(crate::i18n::tr("Margin"));
        ui.add(egui::DragValue::new(&mut m.layout.margin).range(0.0..=0.25).speed(0.001));
        ui.checkbox(&mut m.layout.guides, crate::i18n::tr("Guides"));
        ui.separator();
        ui.checkbox(&mut m.options.zoom_to_fill, crate::i18n::tr("Zoom to fill"));
        ui.label(crate::i18n::tr("Stroke width"));
        ui.add(egui::DragValue::new(&mut m.options.stroke_width).range(0.0..=0.05).speed(0.001));
        ui.label(crate::i18n::tr("Stroke colour"));
        ui.color_edit_button_srgb(&mut m.options.stroke_color);
        ui.checkbox(&mut m.options.shadow, crate::i18n::tr("Shadow"));
        ui.separator();
        ui.label(crate::i18n::tr("Backdrop colour"));
        ui.color_edit_button_srgb(&mut m.backdrop.color);
        let mut gradient = m.backdrop.gradient_to.is_some();
        if ui.checkbox(&mut gradient, crate::i18n::tr("Gradient to")).changed() {
            let to = m.backdrop.gradient_to.unwrap_or([0, 0, 0]);
            m.backdrop.gradient_to = gradient.then_some(to);
        }
        if let Some(mut to) = m.backdrop.gradient_to
            && ui.color_edit_button_srgb(&mut to).changed()
        {
            m.backdrop.gradient_to = Some(to);
        }
        // a photo covers the backdrop instead of the colour; it is chosen from the photos shown
        ui.label(crate::i18n::tr("Backdrop photo"));
        ui.horizontal(|ui| {
            let shown = backdrop_name.clone().unwrap_or_else(|| crate::i18n::tr("None").to_string());
            egui::ComboBox::from_id_salt("slideshowBackdropPhoto").selected_text(shown).show_ui(ui, |ui| {
                for (id, name) in &picks {
                    if ui.selectable_label(m.backdrop.image == Some(*id), name.as_str()).clicked() {
                        m.backdrop.image = Some(*id);
                    }
                }
            });
            if ui.add_enabled(m.backdrop.image.is_some(), egui::Button::new(crate::i18n::tr("Clear"))).clicked() {
                m.backdrop.image = None;
            }
        });
        ui.separator();
        if let Some(text) = text_setting(ui, &mut next.drafts, "slideshowIdentity", crate::i18n::tr("Identity plate"), &m.overlays.identity_plate) {
            m.overlays.identity_plate = text;
        }
        let overlay_label = crate::i18n::tr("Overlay text (metadata fields in braces)");
        if let Some(text) = text_setting(ui, &mut next.drafts, "slideshowText", overlay_label, &m.overlays.text) {
            m.overlays.text = text;
        }
        ui.checkbox(&mut m.overlays.rating_stars, crate::i18n::tr("Rating stars"));
        ui.separator();
        if let Some(text) = text_setting(ui, &mut next.drafts, "slideshowIntro", crate::i18n::tr("Intro title"), &m.titles.intro) {
            m.titles.intro = text;
        }
        if let Some(text) = text_setting(ui, &mut next.drafts, "slideshowEnding", crate::i18n::tr("Ending title"), &m.titles.ending) {
            m.titles.ending = text;
        }
        ui.separator();
        ui.label(crate::i18n::tr("Seconds per photo"));
        ui.add(egui::DragValue::new(&mut m.playback.slide_seconds).range(0.5..=60.0).speed(0.1));
        ui.label(crate::i18n::tr("Fade seconds"));
        ui.add(egui::DragValue::new(&mut m.playback.fade_seconds).range(0.0..=60.0).speed(0.05));
        ui.checkbox(&mut m.playback.random, crate::i18n::tr("Random order"));
        ui.checkbox(&mut m.playback.repeat, crate::i18n::tr("Repeat"));
        ui.checkbox(&mut m.playback.auto, crate::i18n::tr("Advance automatically"));
        ui.separator();
        if let Some(path) = text_setting(ui, &mut next.drafts, "slideshowMusic", crate::i18n::tr("Music file"), &m.music.path) {
            m.music.path = path;
        }
        ui.label(crate::i18n::tr("Music length (seconds)"));
        ui.add(egui::DragValue::new(&mut next.music_seconds).range(1.0..=7200.0).speed(1.0));
        if ui.button(crate::i18n::tr("Fit slides to music")).clicked() {
            fit = true;
        }
    });
    if m != model
        && let Err(e) = app.run("slideshow.set", json!(m))
    {
        app.toast_error(ctx, e);
    }
    if let Some(name) = chosen {
        match app.run("slideshow.applyTemplate", json!({"name": name.clone()})) {
            Ok(_) => next.template = Some(name),
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if let Some(name) = delete {
        match app.run("slideshow.deleteTemplate", json!({"name": name.clone()})) {
            Ok(_) => {
                app.toast(ctx, format!("Deleted template {name}"));
                next.template = None;
            }
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if save {
        let name = next.template_name.trim().to_string();
        match app.run("slideshow.saveTemplate", json!({"name": name})) {
            Ok(_) => {
                app.toast(ctx, format!("Saved template {name}"));
                next.template_name.clear();
            }
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if fit {
        match app.run("slideshow.fitToMusic", json!({"seconds": next.music_seconds})) {
            Ok(done) => app.toast(ctx, format!("{:.2} s per photo", done["slideSeconds"].as_f64().unwrap_or(0.0))),
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if slides == 0 {
        ui.label(crate::i18n::tr("Select photos to show."));
    } else {
        ui.horizontal(|ui| {
            if ui.add_enabled(slide > 0, egui::Button::new(crate::i18n::tr("Previous slide"))).clicked() {
                next.slide = slide - 1;
            }
            ui.label(format!("{} {} / {}", crate::i18n::tr("Slide"), slide + 1, slides));
            if ui.add_enabled(slide + 1 < slides, egui::Button::new(crate::i18n::tr("Next slide"))).clicked() {
                next.slide = slide + 1;
            }
        });
        let key = format!("{}|{}|{:?}", serde_json::to_string(&m).unwrap_or_default(), slide, ids);
        if run.shown.as_deref() != Some(key.as_str()) {
            next.shown = Some(key);
            match render_preview(&mut app.session, &m, &timeline, slide) {
                Ok(mut img) => {
                    if let Some(d) = app.renderer.display()
                        && let Err(e) = d.profile.from_srgb(&mut img)
                    {
                        log::warn!("display profile: {e}");
                    }
                    let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
                    let tex = ctx.load_texture("classic-slideshow-module-slide", color, egui::TextureOptions::LINEAR);
                    ctx.data_mut(|d| d.insert_temp(preview_id(), tex));
                }
                Err(e) => app.toast_error(ctx, e),
            }
        }
        if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(preview_id())) {
            let size = tex.size_vec2();
            let rect = ui.max_rect();
            let scale = (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0));
            let shown = ui.centered_and_justified(|ui| ui.add(egui::Image::new((tex.id(), size * scale))).rect).inner;
            if m.layout.guides {
                // the margin lines, as the render places them: a fraction of the short edge
                let guide = shown.shrink(m.layout.margin as f32 * shown.width().min(shown.height()));
                let stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(160));
                let painter = ui.painter();
                painter.line_segment([guide.left_top(), guide.right_top()], stroke);
                painter.line_segment([guide.right_top(), guide.right_bottom()], stroke);
                painter.line_segment([guide.right_bottom(), guide.left_bottom()], stroke);
                painter.line_segment([guide.left_bottom(), guide.left_top()], stroke);
            }
        }
    }
    if play {
        match start(app, ctx) {
            Ok(_) => app.toast(ctx, crate::i18n::tr("Classic slideshow · Space pauses · arrows step · Esc ends")),
            Err(e) => app.toast_error(ctx, e),
        }
    }
    if export {
        export_file(app, ctx, false);
    }
    if export_jpeg {
        export_file(app, ctx, true);
    }
    if close {
        app.ui.classic_slideshow_view = None;
    } else if let Some(open) = app.ui.classic_slideshow_view.as_mut() {
        *open = next;
    }
}

/// Asks for a PDF path, or a folder for the JPEGs, then writes the show there (`slideshow.export`).
fn export_file(app: &mut LightcraftApp, ctx: &egui::Context, jpeg: bool) {
    let format = if jpeg { "jpeg" } else { "pdf" };
    let params = json!({"format": format});
    let (key, request) = if jpeg {
        ("dir", PickRequest::folder(crate::i18n::tr("Slideshow JPEG folder")))
    } else {
        ("path", PickRequest::save("Slideshow PDF", "PDF", &["pdf"], "Slideshow.pdf"))
    };
    match crate::pick::ask(app, "slideshow.export", &params, key, request, |_| None) {
        Picked::Now(paths) => {
            let Some(path) = paths.first() else { return };
            let mut params = params;
            params[key] = json!(path);
            match app.run("slideshow.export", params) {
                Ok(done) => {
                    let slides = if jpeg { json!(done["files"].as_array().map_or(0, Vec::len)) } else { done["slides"].clone() };
                    app.toast(ctx, format!("Exported {slides} slides to {path}"));
                }
                Err(e) => app.toast_error(ctx, e),
            }
        }
        Picked::Later => {}
        Picked::Unavailable => app.toast_error(ctx, format!("no file dialog on this platform; slideshow.export needs a {key}")),
    }
}

fn frame_id() -> egui::Id {
    egui::Id::new("classic-slideshow-frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::PhotoId;

    fn run(seconds: f64, fade: f64, repeat: bool, slides: u64) -> Run {
        let mut model = Model::default();
        model.playback.slide_seconds = seconds;
        model.playback.fade_seconds = fade;
        model.playback.repeat = repeat;
        let ids: Vec<PhotoId> = (1..=slides).map(PhotoId).collect();
        let timeline = Timeline::new(&model, &ids);
        Run { model, timeline, size: (16, 16), started: 0.0, paused: None, slides: Vec::new(), shown: None }
    }

    #[test]
    fn pause_holds_the_time_and_resume_continues_from_it() {
        let mut r = run(3.0, 0.0, false, 3);
        r.started = 0.0;
        assert_eq!(r.timeline.frame_at(r.elapsed(4.0)).unwrap().current, 1);
        r.toggle(4.0); // pause at 4 s (slide 1)
        assert_eq!(r.elapsed(100.0), 4.0, "time stands still while paused");
        r.toggle(10.0); // resume
        assert!((r.elapsed(11.0) - 5.0).abs() < 1e-9, "one second after resume is 5 s into the show");
    }

    #[test]
    fn stepping_moves_by_slide_and_ends_without_repeat() {
        let mut r = run(3.0, 0.0, false, 3);
        r.toggle(0.0); // paused at 0 s
        assert!(r.step(0.0, true));
        assert_eq!(r.elapsed(0.0), 3.0, "next starts slide 1");
        assert!(r.step(0.0, true));
        assert_eq!(r.elapsed(0.0), 6.0);
        assert!(r.step(0.0, false), "back to the start of slide 1 when slide 2 has begun");
        assert_eq!(r.elapsed(0.0), 3.0);
        assert!(r.step(0.0, false));
        assert_eq!(r.elapsed(0.0), 0.0, "a slide that has barely begun steps back to the one before");
        r.seek(6.0, 0.0);
        assert!(!r.step(0.0, true), "no slide after the last one");
    }

    #[test]
    fn repeat_wraps_the_next_step_to_the_first_slide() {
        let mut r = run(3.0, 0.0, true, 2);
        r.toggle(0.0);
        r.seek(3.0, 0.0);
        assert!(r.step(0.0, true));
        assert_eq!(r.elapsed(0.0), 0.0);
    }

    #[test]
    fn manual_playback_starts_paused_on_the_first_slide() {
        let mut model = Model::default();
        model.playback.auto = false;
        let timeline = Timeline::new(&model, &[PhotoId(1), PhotoId(2)]);
        let manual = Run::new(model, timeline, (16, 16), 0.0);
        assert_eq!(manual.elapsed(100.0), 0.0, "time stands still until a step or Space");
        assert_eq!(manual.timeline.frame_at(manual.elapsed(100.0)).unwrap().current, 0);
        let auto_model = Model::default();
        let auto = Run::new(auto_model.clone(), Timeline::new(&auto_model, &[PhotoId(1)]), (16, 16), 0.0);
        assert!(auto.paused.is_none(), "an automatic show runs from the start");
    }

    #[test]
    fn compose_fades_the_current_slide_over_the_previous_one() {
        let mut s = Session::with_demo();
        let ids: Vec<PhotoId> = s.catalog.photos().take(2).map(|p| p.id).collect();
        let mut r = run(3.0, 1.0, false, 0);
        r.timeline = Timeline::new(&r.model, &ids);
        let frame = r.timeline.frame_at(3.5).unwrap();
        assert_eq!((frame.current, frame.previous), (1, Some(0)));
        let mixed = r.compose(&mut s, frame).unwrap();
        assert_eq!(r.slides.len(), 2, "both slides are kept while they fade");
        let from = r.kept(0).unwrap().clone();
        let to = r.kept(1).unwrap().clone();
        assert_eq!(mixed, slideshow::blend(&from, &to, 0.5));
        // the first slide fades in over the backdrop only
        let first = r.compose(&mut s, r.timeline.frame_at(0.0).unwrap()).unwrap();
        assert_eq!(first, slideshow::backdrop(&r.model, 16, 16));
    }
}
