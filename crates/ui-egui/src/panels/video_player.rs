//! The video player in the loupe (issues #73 and #76): the playing picture over the poster, the transport bar (play or pause, time,
//! scrubber, Set Poster Frame), and the "Video support isn't installed" message.
//!
//! The playback itself is the engine's (`video.play`, `video.pause`, `video.seek`, `video.togglePlay`; `lightcraft_engine::video_player`):
//! this module asks it for the frame due once per UI frame ([`lightcraft_engine::Session::video_tick_now`], which times frames by
//! their pts), uploads a texture when there is a new one, and draws it over the loupe where the poster is. A clip shows its poster
//! until it is played; a paused or ended clip keeps the frame it stopped on. Everything the user does here is a command.

use egui::{Color32, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use lightcraft_catalog::{MediaKind, Photo, PhotoId};
use lightcraft_engine::video_player::PlayState;
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;
use crate::theme::Tokens;
use crate::widgets::register;

/// Height of the transport bar (points) and the widest it grows.
const BAR_H: f32 = 36.0;
const BAR_MAX_W: f32 = 760.0;
/// Below this width the bar is not drawn (a pane too narrow to scrub in).
const BAR_MIN_W: f32 = 220.0;
/// Room under the bar for the Trim row (`video_trim`: 28 points tall, 14 above the canvas' bottom) and a gap above it.
const TRIM_ROW_CLEARANCE: f32 = 14.0 + 28.0 + 8.0;

/// What the player remembers between frames: the picture on the GPU and a scrub in progress.
#[derive(Clone, Default)]
struct View {
    texture: Option<TextureHandle>,
    /// The engine's frame counter the texture shows, and the photo it is of.
    seq: u64,
    photo: Option<PhotoId>,
    /// While the scrubber is dragged: where, in seconds. The clip seeks when it is let go.
    scrub: Option<f64>,
}

fn view_id() -> egui::Id {
    egui::Id::new("video-player-view")
}

fn load_view(ctx: &egui::Context) -> View {
    ctx.data(|d| d.get_temp::<View>(view_id())).unwrap_or_default()
}

fn store_view(ctx: &egui::Context, view: View) {
    ctx.data_mut(|d| d.insert_temp(view_id(), view));
}

/// `1:05`, or `1:02:03` from an hour on; whole seconds, rounded down; anything that is not a time is `0:00`.
pub fn format_time(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The time a point `x` along a scrubber of `track` stands for, in a clip of `duration` seconds: kept inside the clip.
pub fn scrub_time(x: f32, track: Rect, duration: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    let share = ((x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    f64::from(share) * duration
}

/// Called every UI frame: a clip that is no longer on screen (another photo, another view, a dialog over it) is stopped, so no child
/// process decodes for a picture nobody sees.
pub fn idle(app: &mut LightcraftApp) {
    let Some(loaded) = app.session.video.photo() else { return };
    let on_screen = matches!(app.ui.view, ViewMode::Detail) && app.session.active() == Some(loaded);
    if !on_screen {
        let _ = app.run("video.stop", json!({}));
    }
}

/// Space on a video in the loupe plays or pauses it. Returns whether it did (else Space keeps its other meaning).
pub fn space_toggles(app: &mut LightcraftApp) -> bool {
    let is_video = app.session.active().and_then(|id| app.session.catalog.photo(id)).is_some_and(|p| p.kind == MediaKind::Video);
    if !is_video || !matches!(app.ui.view, ViewMode::Detail) {
        return false;
    }
    // a refusal (no sidecar) is kept by the player and shown in the loupe
    let _ = app.run("video.togglePlay", json!({}));
    true
}

/// The playing picture, over the poster in `img`. Also moves the clock on (once per UI frame).
pub fn draw_frame(app: &mut LightcraftApp, ui: &egui::Ui, canvas: Rect, img: Rect, photo: &Photo) {
    if photo.kind != MediaKind::Video {
        return;
    }
    let ctx = ui.ctx().clone();
    app.session.video_tick_now();
    let mut view = load_view(&ctx);
    let shown = app.session.video.frame().filter(|(owner, _)| *owner == photo.id).map(|(_, f)| f.clone());
    match shown {
        Some(frame) => {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let fits = w.checked_mul(h).and_then(|n| n.checked_mul(4)) == Some(frame.rgba.len());
            if fits && (view.seq != app.session.video.frame_seq() || view.photo != Some(photo.id) || view.texture.is_none()) {
                let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &frame.rgba);
                match view.texture.as_mut() {
                    Some(texture) => texture.set(image, TextureOptions::LINEAR),
                    None => view.texture = Some(ctx.load_texture("video-frame", image, TextureOptions::LINEAR)),
                }
                view.seq = app.session.video.frame_seq();
                view.photo = Some(photo.id);
            }
            if let Some(texture) = view.texture.as_ref().filter(|_| view.photo == Some(photo.id)) {
                let at = crate::panels::detail::fit_texture_rect(img, texture.size());
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter_at(canvas).image(texture.id(), at, uv, Color32::WHITE);
            }
        }
        // nothing of this clip on screen: the picture of another one is let go
        None => {
            if view.photo.is_some() {
                view.texture = None;
                view.photo = None;
            }
        }
    }
    store_view(&ctx, view);
    if app.session.video.is_streaming() {
        // the clock moves on by itself: look again about every half frame
        let half_frame = 0.5 / app.session.video.fps().max(1.0);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(half_frame.clamp(0.004, 0.05)));
    }
}

fn disc(p: &egui::Painter, c: Pos2, r: f32, fill: Color32) {
    p.circle_filled(c, r, fill);
}

/// The transport bar at the bottom of the canvas, and the message above it when the clip cannot play.
pub fn transport(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, photo: &Photo) {
    if photo.kind != MediaKind::Video {
        return;
    }
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let mut view = load_view(&ctx);
    let player = &app.session.video;
    let mine = player.photo() == Some(photo.id);
    let playing = mine && player.state() == PlayState::Playing;
    let known = if mine { player.duration() } else { None };
    let duration = known.or(photo.duration).filter(|d| d.is_finite() && *d > 0.0);
    let position = if mine { app.session.video_position_now() } else { 0.0 };
    let error = if mine { player.error().cloned() } else { None };

    let width = (canvas.width() - 32.0).clamp(0.0, BAR_MAX_W);
    if width < BAR_MIN_W {
        return;
    }
    // above the Trim row, which the loupe shows on a clip with a duration (not in the full-screen preview)
    let has_trim_row = photo.duration.is_some_and(|d| d.is_finite() && d > 0.0) && !app.ui.fullscreen;
    let gap = if has_trim_row { TRIM_ROW_CLEARANCE } else { 18.0 };
    let bar = Rect::from_center_size(pos2(canvas.center().x, canvas.bottom() - gap - BAR_H / 2.0), vec2(width, BAR_H));
    let p = ui.painter_at(canvas);
    p.rect_filled(bar, BAR_H / 2.0, Color32::from_black_alpha(190));
    register(&ctx, "video:bar", bar);

    // play / pause
    let play_rect = Rect::from_center_size(pos2(bar.left() + 6.0 + 14.0, bar.center().y), vec2(28.0, 28.0));
    let play = ui.interact(play_rect, egui::Id::new("video-play"), Sense::click());
    let label = crate::i18n::tr(if playing { "Pause" } else { "Play" });
    play.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    register(&ctx, "video:play", play_rect);
    if play.hovered() {
        disc(&p, play_rect.center(), 14.0, Color32::from_white_alpha(40));
    }
    let c = play_rect.center();
    if playing {
        for dx in [-3.5, 3.5] {
            p.rect_filled(Rect::from_center_size(pos2(c.x + dx, c.y), vec2(3.5, 12.0)), 1.0, Color32::WHITE);
        }
    } else {
        let tri = vec![pos2(c.x - 4.0, c.y - 7.0), pos2(c.x - 4.0, c.y + 7.0), pos2(c.x + 7.0, c.y)];
        p.add(egui::Shape::convex_polygon(tri, Color32::WHITE, Stroke::NONE));
    }
    let clicked_play = play.on_hover_text(label).clicked();

    // Set Poster Frame, at the right end: the clip must be loaded (played or scrubbed) to have a current time
    let poster_label = crate::i18n::tr("Set Poster Frame");
    let poster_w = (p.layout_no_wrap(poster_label.to_string(), t.semibold(11.5), t.text).size().x + 18.0).max(37.0);
    let show_poster = width >= 440.0;
    let poster_rect = Rect::from_min_size(pos2(bar.right() - 6.0 - poster_w, bar.center().y - 12.0), vec2(poster_w, 24.0));
    let mut set_poster = false;
    if show_poster {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(poster_rect));
        let r = child.add_enabled_ui(mine, |ui| crate::widgets::text_button(ui, "videoPoster", "Set Poster Frame", false)).inner;
        set_poster = r.on_hover_text(crate::i18n::tr("Use the frame on screen as this clip's poster in the grid, the loupe and exports")).clicked();
    }

    // time, then the scrubber across what is left
    let shown_t = view.scrub.unwrap_or(position);
    let clock = match duration {
        Some(d) => format!("{} / {}", format_time(shown_t), format_time(d)),
        None => format_time(shown_t),
    };
    let clock_x = play_rect.right() + 8.0;
    let clock_galley = p.layout_no_wrap(clock, t.font(12.0), Color32::from_gray(225));
    let clock_w = clock_galley.size().x;
    p.galley(pos2(clock_x, bar.center().y - clock_galley.size().y / 2.0), clock_galley, Color32::from_gray(225));
    let right_edge = if show_poster { poster_rect.left() - 10.0 } else { bar.right() - 12.0 };
    let track = Rect::from_min_max(pos2(clock_x + clock_w + 12.0, bar.center().y - 10.0), pos2(right_edge, bar.center().y + 10.0));
    let mut seek_to = None;
    if track.width() > 30.0 {
        let line = Rect::from_center_size(track.center(), vec2(track.width(), 4.0));
        p.rect_filled(line, 2.0, Color32::from_white_alpha(60));
        if let Some(d) = duration {
            let share = (shown_t / d).clamp(0.0, 1.0) as f32;
            let done = Rect::from_min_max(line.min, pos2(line.left() + line.width() * share, line.bottom()));
            p.rect_filled(done, 2.0, t.accent);
            let scrub = ui.interact(track, egui::Id::new("video-scrub"), Sense::click_and_drag());
            register(&ctx, "video:scrub", track);
            let thumb = if scrub.hovered() || scrub.dragged() { 7.0 } else { 5.5 };
            disc(&p, pos2(done.right(), line.center().y), thumb, Color32::WHITE);
            let at = scrub.interact_pointer_pos().or(scrub.hover_pos());
            if (scrub.dragged() || scrub.drag_started())
                && let Some(x) = at.map(|q| q.x)
            {
                view.scrub = Some(scrub_time(x, track, d));
            }
            if scrub.drag_stopped() {
                seek_to = view.scrub.take();
            } else if scrub.clicked()
                && let Some(x) = at.map(|q| q.x)
            {
                view.scrub = None;
                seek_to = Some(scrub_time(x, track, d));
            }
            scrub.widget_info(|| egui::WidgetInfo::slider(true, shown_t, crate::i18n::tr("Playback position")));
        }
    }

    // why the clip is not playing
    if let Some(e) = error {
        let text = if e.unavailable { crate::i18n::tr(lightcraft_engine::video_player::NOT_INSTALLED).to_string() } else { e.message };
        let g = p.layout_no_wrap(text, t.font(12.0), Color32::from_gray(235));
        let size = vec2((g.size().x + 24.0).min(canvas.width() - 16.0), 24.0);
        let r = Rect::from_center_size(pos2(canvas.center().x, bar.top() - 10.0 - size.y / 2.0), size);
        p.rect_filled(r, 12.0, Color32::from_black_alpha(190));
        p.with_clip_rect(r.shrink2(vec2(10.0, 0.0))).galley(pos2(r.left() + 12.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(235));
        register(&ctx, "video:message", r);
    }

    store_view(&ctx, view);
    if clicked_play {
        let _ = app.run("video.togglePlay", json!({}));
    }
    if let Some(t) = seek_to {
        let _ = app.run("video.seek", json!({"id": photo.id.0, "t": t}));
    }
    if set_poster {
        // the time on the bar: the clip's clock now (a scrub in progress has already been let go above)
        let now = app.session.video_position_now();
        let _ = app.run("video.setPoster", json!({"id": photo.id.0, "t": now}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_minutes_and_seconds_and_hours_when_needed() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(5.9), "0:05");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(3600.0), "1:00:00");
        assert_eq!(format_time(3725.4), "1:02:05");
        for odd in [-3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_time(odd), "0:00", "{odd}");
        }
    }

    #[test]
    fn a_point_on_the_scrubber_is_a_time_inside_the_clip() {
        let track = Rect::from_min_max(pos2(100.0, 0.0), pos2(300.0, 20.0));
        assert_eq!(scrub_time(100.0, track, 60.0), 0.0);
        assert_eq!(scrub_time(200.0, track, 60.0), 30.0);
        assert_eq!(scrub_time(300.0, track, 60.0), 60.0);
        assert_eq!(scrub_time(-50.0, track, 60.0), 0.0, "left of the track is the start");
        assert_eq!(scrub_time(9999.0, track, 60.0), 60.0, "right of it is the end");
        assert_eq!(scrub_time(f32::NAN, track, 60.0), 0.0, "no NaN time");
    }
}
