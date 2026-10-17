//! Tagging faces by hand in the loupe: drag a box where detection missed a face and name it (`faces.addRegion`), or
//! right-click a face box to remove it (`faces.removeRegion`). The drag maths is pure, so it is tested without a window.

use egui::{Pos2, Rect, Sense, Stroke, StrokeKind, vec2};
use lightcraft_catalog::Photo;
use lightcraft_geom::{Orientation, Point};
use serde_json::json;

use crate::LightcraftApp;
use crate::panels::detail::CanvasMap;
use crate::panels::faces::{self, Editor};
use crate::state::NameEdit;

/// The smallest box a drag draws, on screen: a click is not a face.
const MIN_BOX_PX: f32 = 8.0;

/// The box a drag from `a` to `b` spans, in normalized image coordinates (`norm` maps a screen point to them, zoom and
/// pan included). The corners are clamped to the photo; a box under `min_px` on screen along either side is no box.
/// The result is in the frame the photo is shown in (the stored frame is the inverse of the photo's orientation).
pub fn drawn_box(a: Pos2, b: Pos2, min_px: f32, norm: impl Fn(Pos2) -> (f64, f64)) -> Option<lightcraft_geom::Rect> {
    if (a.x - b.x).abs() < min_px || (a.y - b.y).abs() < min_px {
        return None;
    }
    let ((ax, ay), (bx, by)) = (norm(a), norm(b));
    let clamp = |v: f64| v.clamp(0.0, 1.0);
    let r = lightcraft_geom::Rect { x0: clamp(ax.min(bx)), y0: clamp(ay.min(by)), x1: clamp(ax.max(bx)), y1: clamp(ay.max(by)) };
    (r.x1 > r.x0 && r.y1 > r.y0).then_some(r)
}

/// Where a normalized point of the photo is on screen.
fn screen_of(map: &CanvasMap, x: f64, y: f64) -> Pos2 {
    map.screen(Point::new(x, y))
}

/// A stored face box (in the photo's upright frame) on screen, where the loupe draws it.
fn box_on_screen(map: &CanvasMap, orient: Orientation, r: &lightcraft_geom::Rect) -> Rect {
    let n = orient.map_norm_rect(*r);
    Rect::from_two_pos(screen_of(map, n.x0, n.y0), screen_of(map, n.x1, n.y1))
}

/// A screen point as normalized image coordinates.
fn norm_of(map: &CanvasMap, q: Pos2) -> (f64, f64) {
    let n = map.norm(q);
    (n.x, n.y)
}

/// Face tagging over the loupe's photo, shown at `img` through `map`, in its `orient` frame: the Tag Face drag and its
/// name box, and the right-click menu on a face box. Call it before the face boxes are drawn, so their handles take the
/// pointer first.
pub fn overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, p: &egui::Painter, map: &CanvasMap, img: Rect, photo: &Photo, orient: Orientation) {
    let pid = photo.id.0;
    // a draft or menu left on another photo is dropped
    if app.ui.face_draft.as_ref().is_some_and(|(_, e)| e.photo != pid) {
        app.ui.face_draft = None;
    }
    if app.ui.face_menu.is_some_and(|(m, _, _)| m != pid) {
        app.ui.face_menu = None;
    }
    if app.ui.face_tag && img.is_positive() {
        let drag = egui::Id::new("face-tag-drag");
        let resp = ui
            .interact(img, egui::Id::new("face-tag-draw"), Sense::drag())
            .on_hover_text(crate::i18n::tr("Drag a box around a face, then type its name"));
        if resp.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            ui.data_mut(|d| d.insert_temp(drag, origin));
        }
        let start: Option<Pos2> = ui.data(|d| d.get_temp(drag));
        let now = resp.interact_pointer_pos();
        if let (Some(a), Some(b)) = (start, now)
            && resp.dragged()
            && let Some(n) = drawn_box(a, b, MIN_BOX_PX, |q| norm_of(map, q))
        {
            let shown = Rect::from_two_pos(screen_of(map, n.x0, n.y0), screen_of(map, n.x1, n.y1));
            p.rect_stroke(shown, 0.0, Stroke::new(1.0, egui::Color32::WHITE), StrokeKind::Outside);
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove_temp::<Pos2>(drag));
            if let (Some(a), Some(b)) = (start, now)
                && let Some(n) = drawn_box(a, b, MIN_BOX_PX, |q| norm_of(map, q))
            {
                // stored in the photo's upright frame, like the regions read from XMP
                let stored = orient.inverse().map_norm_rect(n);
                app.ui.face_draft = Some((stored, NameEdit { photo: pid, index: usize::MAX, text: String::new(), fresh: true }));
                app.ui.face_menu = None;
            }
        }
    }
    // the name box for a box just drawn: Enter names it (and adds the face), Escape cancels
    if app.ui.face_draft.is_some() {
        let people = app.caches.person_names(&app.session.catalog);
        let setup = faces::setup(app, ui.ctx());
        let outcome = app.ui.face_draft.as_mut().map(|(stored, edit)| {
            let n = orient.map_norm_rect(*stored);
            let at = screen_of(map, n.x0, n.y1) + vec2(0.0, 8.0);
            (*stored, faces::name_editor(ui.ctx(), at, edit, &[], &people, setup))
        });
        match outcome {
            Some((stored, Editor::Submit(name))) => {
                app.ui.face_draft = None;
                let rect = [stored.x0, stored.y0, stored.x1 - stored.x0, stored.y1 - stored.y0];
                let _ = app.run("faces.addRegion", json!({"photo": pid, "rect": rect, "name": name}));
            }
            Some((_, Editor::Setup)) => {
                app.ui.face_draft = None;
                faces::take_step(app, ui.ctx(), setup);
            }
            Some((_, Editor::Cancel)) => app.ui.face_draft = None,
            _ => {}
        }
    }
    // a right-click on a face box opens Remove Face there
    let (secondary, at) = ui.input(|i| (i.pointer.secondary_clicked(), i.pointer.latest_pos()));
    if secondary && let Some(at) = at {
        app.ui.face_menu = photo
            .meta
            .regions
            .iter()
            .enumerate()
            .find(|(_, r)| r.kind == lightcraft_meta::RegionKind::Face && box_on_screen(map, orient, &r.rect).expand(3.0).contains(at))
            .map(|(index, _)| (pid, index, at));
    }
    if let Some((mp, index, at)) = app.ui.face_menu {
        let mut remove = false;
        let area =
            egui::Area::new(egui::Id::new("face-remove-menu")).order(egui::Order::Foreground).fixed_pos(at).constrain(true).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if ui.button(crate::i18n::tr("Remove Face")).clicked() {
                        remove = true;
                    }
                });
            });
        let (pressed, pointer, escape) = ui.input(|i| (i.pointer.any_pressed(), i.pointer.interact_pos(), i.key_pressed(egui::Key::Escape)));
        let outside = pressed && !pointer.is_some_and(|q| area.response.rect.contains(q));
        if remove {
            app.ui.face_menu = None;
            let _ = app.run("faces.removeRegion", json!({"face": {"photo": mp, "index": index}}));
        } else if outside || escape {
            app.ui.face_menu = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2, vec2};

    /// The normalized coordinates of a screen point on a photo shown at `img` (a plain, unrotated map).
    fn linear(img: Rect) -> impl Fn(Pos2) -> (f64, f64) {
        move |p: Pos2| (f64::from((p.x - img.left()) / img.width()), f64::from((p.y - img.top()) / img.height()))
    }

    fn assert_close(r: lightcraft_geom::Rect, want: (f64, f64, f64, f64)) {
        for (got, want) in [(r.x0, want.0), (r.y0, want.1), (r.x1, want.2), (r.y1, want.3)] {
            assert!((got - want).abs() < 1e-4, "{r:?} is not {want:?}");
        }
    }

    #[test]
    fn a_drag_gives_the_same_box_however_the_photo_is_zoomed_and_panned() {
        // fitted: the photo fills 200 x 100 points at (10, 20)
        let fit = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        let r = drawn_box(pos2(50.0, 40.0), pos2(110.0, 80.0), 8.0, linear(fit)).unwrap();
        assert_close(r, (0.2, 0.2, 0.5, 0.6));
        // zoomed 2x and panned: the same picture points are elsewhere on screen, and the same box comes out
        let zoomed = Rect::from_min_size(pos2(-190.0, -60.0), vec2(400.0, 200.0));
        let r = drawn_box(pos2(-110.0, -20.0), pos2(10.0, 60.0), 8.0, linear(zoomed)).unwrap();
        assert_close(r, (0.2, 0.2, 0.5, 0.6));
    }

    #[test]
    fn a_drag_runs_either_way_and_is_clamped_to_the_photo() {
        let fit = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        // from the bottom right corner back up and left
        let r = drawn_box(pos2(110.0, 80.0), pos2(50.0, 40.0), 8.0, linear(fit)).unwrap();
        assert_close(r, (0.2, 0.2, 0.5, 0.6));
        // from outside the photo (left and above it) to inside: the corner is the photo's edge
        let r = drawn_box(pos2(-50.0, 10.0), pos2(150.0, 90.0), 8.0, linear(fit)).unwrap();
        assert_close(r, (0.0, 0.0, 0.7, 0.7));
    }

    #[test]
    fn a_click_or_a_sliver_is_no_box() {
        let fit = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        assert!(drawn_box(pos2(50.0, 40.0), pos2(53.0, 60.0), 8.0, linear(fit)).is_none(), "narrower than 8 points");
        assert!(drawn_box(pos2(50.0, 40.0), pos2(50.0, 40.0), 8.0, linear(fit)).is_none(), "a click");
        // entirely to the right of the photo: clamped to a zero-width edge
        assert!(drawn_box(pos2(300.0, 40.0), pos2(400.0, 60.0), 8.0, linear(fit)).is_none());
    }
}
