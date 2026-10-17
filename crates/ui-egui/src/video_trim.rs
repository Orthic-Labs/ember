//! The loupe's Trim row for a video (issue #74): the clip's in and out points, with buttons that set them at the frame on
//! screen and one that clears them. The frame on screen is where the player is once the clip is loaded in it (played or
//! scrubbed), else the poster frame (`video.setPoster`). Every change goes through `video.setTrim` and `video.clearTrim`, so it
//! is one undo step.

use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::Photo;
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// Seconds as a clock: `0:07.5`, `12:03.0`, `1:02:03.0`. A time that is not a number of seconds shows as zero.
pub fn clock(secs: f64) -> String {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    // tenths, rounded once, so 59.96 s reads 1:00.0 and never 0:60.0
    let tenths = (secs * 10.0).round() as u64;
    let (whole, tenth) = (tenths / 10, tenths % 10);
    let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{tenth}") } else { format!("{m}:{s:02}.{tenth}") }
}

/// The in and out points as one label: `0:02.5 – 0:12.0`; an unset in point reads as the start, an unset out point as the
/// end of the clip (blank when its length is not known).
pub fn trim_label(start: Option<f64>, end: Option<f64>, duration: Option<f64>) -> String {
    let from = clock(start.unwrap_or(0.0));
    let to = end.or(duration.filter(|d| d.is_finite() && *d > 0.0)).map(clock).unwrap_or_default();
    format!("{from} – {to}").trim_end().to_string()
}

/// One button of the row: a label, an automation id and a hover text. Returns whether it was clicked.
fn button(ui: &mut egui::Ui, p: &egui::Painter, t: &Tokens, at: Rect, id: &'static str, label: &str, tip: &str) -> bool {
    let resp = ui.interact(at, egui::Id::new(id), Sense::click());
    register(ui.ctx(), id, at);
    let fill = if resp.hovered() { Color32::from_white_alpha(46) } else { Color32::from_white_alpha(22) };
    p.rect_filled(at, 9.0, fill);
    p.rect_stroke(at, 9.0, Stroke::new(1.0, Color32::from_white_alpha(60)), egui::StrokeKind::Inside);
    let g = p.layout_no_wrap(label.to_string(), t.font(12.0), Color32::from_gray(235));
    p.galley(pos2(at.center().x - g.size().x / 2.0, at.center().y - g.size().y / 2.0), g, Color32::WHITE);
    resp.on_hover_text(tip).clicked()
}

/// Draw the row at the bottom centre of `canvas` and act on its buttons. Nothing for a clip whose duration is not known
/// (it was not probed: there is nothing to cut).
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, photo: &Photo) {
    if photo.duration.is_none_or(|d| !d.is_finite() || d <= 0.0) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let (start, end) = (photo.develop.trim_in, photo.develop.trim_out);
    let trimmed = start.is_some() || end.is_some();
    let title = p.layout_no_wrap(crate::i18n::tr("Trim").to_string(), t.font(12.0), Color32::from_gray(190));
    let times = p.layout_no_wrap(trim_label(start, end, photo.duration), t.font(12.0), Color32::WHITE);
    let labels = [crate::i18n::tr("Set In"), crate::i18n::tr("Set Out"), crate::i18n::tr("Clear Trim")];
    let widths: Vec<f32> = labels.iter().map(|l| p.layout_no_wrap(l.to_string(), t.font(12.0), Color32::WHITE).size().x + 20.0).collect();
    let shown = if trimmed { 3 } else { 2 };
    let gap = 8.0;
    let buttons: f32 = widths.iter().take(shown).sum::<f32>() + gap * shown as f32;
    let width = 14.0 + title.size().x + gap + times.size().x + gap + buttons + 6.0;
    let height = 28.0;
    let r = Rect::from_min_size(pos2(canvas.center().x - width / 2.0, canvas.bottom() - height - 14.0), vec2(width, height));
    if r.left() < canvas.left() {
        return;
    }
    p.rect_filled(r, height / 2.0, Color32::from_black_alpha(185));
    register(ui.ctx(), "loupe:trim", r);
    let mut x = r.left() + 14.0;
    p.galley(pos2(x, r.center().y - title.size().y / 2.0), title.clone(), Color32::WHITE);
    x += title.size().x + gap;
    p.galley(pos2(x, r.center().y - times.size().y / 2.0), times.clone(), Color32::WHITE);
    x += times.size().x + gap;
    let at = |x: f32, w: f32| Rect::from_min_size(pos2(x, r.top() + 3.0), vec2(w, height - 6.0));
    let now =
        if app.session.video.photo() == Some(photo.id) { app.session.video_position_now() } else { lightcraft_engine::video::poster_secs_of(photo) };
    let mut set = None;
    let w_in = widths.first().copied().unwrap_or(60.0);
    if button(ui, &p, &t, at(x, w_in), "loupe:trimIn", labels[0], crate::i18n::tr("Start the clip at the frame on screen")) {
        set = Some(json!({"id": photo.id.0, "in": now}));
    }
    x += w_in + gap;
    let w_out = widths.get(1).copied().unwrap_or(60.0);
    if button(ui, &p, &t, at(x, w_out), "loupe:trimOut", labels[1], crate::i18n::tr("End the clip at the frame on screen")) {
        set = Some(json!({"id": photo.id.0, "out": now}));
    }
    x += w_out + gap;
    let mut clear = false;
    if trimmed {
        let w_clear = widths.get(2).copied().unwrap_or(80.0);
        clear = button(ui, &p, &t, at(x, w_clear), "loupe:trimClear", labels[2], crate::i18n::tr("Keep the whole clip again"));
    }
    let result = match (set, clear) {
        (Some(params), _) => Some(app.run("video.setTrim", params)),
        (None, true) => Some(app.run("video.clearTrim", json!({"id": photo.id.0}))),
        (None, false) => None,
    };
    if let Some(Err(message)) = result {
        app.toast(ui.ctx(), message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_reads_minutes_seconds_and_tenths() {
        assert_eq!(clock(0.0), "0:00.0");
        assert_eq!(clock(7.5), "0:07.5");
        assert_eq!(clock(125.04), "2:05.0");
        assert_eq!(clock(59.96), "1:00.0", "rounded once, never 0:60.0");
        assert_eq!(clock(3723.0), "1:02:03.0");
        assert_eq!(clock(-4.0), "0:00.0");
        assert_eq!(clock(f64::NAN), "0:00.0");
        assert_eq!(clock(f64::INFINITY), "0:00.0");
    }

    #[test]
    fn the_label_shows_both_points_with_the_clip_ends_for_unset_ones() {
        assert_eq!(trim_label(Some(2.5), Some(12.0), Some(20.0)), "0:02.5 – 0:12.0");
        assert_eq!(trim_label(Some(2.5), None, Some(20.0)), "0:02.5 – 0:20.0");
        assert_eq!(trim_label(None, Some(4.0), Some(20.0)), "0:00.0 – 0:04.0");
        assert_eq!(trim_label(Some(2.5), None, None), "0:02.5 –", "no end to show when the length is not known");
    }
}
