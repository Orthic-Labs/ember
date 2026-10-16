//! Classic Slideshow playback (`slideshow.play`): the engine's timeline drawn full screen on the
//! backdrop, paused and stepped with the keys. Space pauses, the arrow and page keys step, Escape
//! ends it, and it ends by itself after the last slide unless the show repeats. The frame is the
//! whole view: there is no progress window.
//!
//! Each slide is rendered once through the export pipeline and kept while it shows; a fade mixes
//! two kept slides. The renders run on the UI thread, so a show with large photos can stutter at
//! a cross-fade's start.

use crate::LightcraftApp;
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
            None => slideshow::backdrop(&self.model, self.size.0, self.size.1),
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
    app.ui.classic_slideshow = Some(Run { model, timeline, size, started: ctx.input(|i| i.time), paused: None, slides: Vec::new(), shown: None });
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
