//! The unnamed faces in the People view: every face nobody has named, as cropped pictures, with look-alikes next to each
//! other (once recognition has looked at them). Click faces to select them (Shift-click selects a range), type a name in
//! the bar above, press Enter: they are all named at once, in one undo step. A face the named ones recognise carries the
//! name they suggest; clicking that name accepts it for that face alone.
//!
//! The engine does the looking (`faces.unnamed`) and the naming (`faces.nameFaces`); this draws and selects.

use std::sync::Arc;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// One unnamed face: which photo and region, the box to show it by, and a suggested name.
#[derive(Clone, Debug, PartialEq)]
pub struct UnnamedFace {
    pub photo: u64,
    pub index: usize,
    pub view: lightcraft_geom::Rect,
    pub suggestion: Option<String>,
}

/// A group of look-alike unnamed faces (`faces.clusters`), largest first: all of its faces, so naming the group names them all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cluster {
    pub faces: Vec<UnnamedFace>,
}

/// What `faces.unnamed` said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unnamed {
    /// Unnamed faces in the library (more than are listed when the list was cut).
    pub total: usize,
    pub faces: Vec<UnnamedFace>,
    /// Look-alikes are next to each other.
    pub ordered: bool,
    /// Recognition is running.
    pub ready: bool,
    /// The look-alike groups of two or more faces, from `faces.clusters`.
    pub clusters: Vec<Cluster>,
}

fn rect_of(v: &Value) -> Option<lightcraft_geom::Rect> {
    Some(lightcraft_geom::Rect { x0: v["x0"].as_f64()?, y0: v["y0"].as_f64()?, x1: v["x1"].as_f64()?, y1: v["y1"].as_f64()? })
}

fn face_of(v: &Value) -> Option<UnnamedFace> {
    Some(UnnamedFace {
        photo: v["photo"].as_u64()?,
        index: usize::try_from(v["index"].as_u64()?).ok()?,
        view: rect_of(&v["view"]).or_else(|| rect_of(&v["rect"]))?,
        suggestion: v["suggestion"]["name"].as_str().map(str::to_string),
    })
}

/// The engine's answer as a list (anything that does not make sense is left out).
pub fn parse(v: &Value) -> Unnamed {
    Unnamed {
        total: v["total"].as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(0)),
        faces: v["faces"].as_array().map(|a| a.iter().filter_map(face_of).collect()).unwrap_or_default(),
        ordered: v["ordered"] == true,
        ready: v["ready"] == true,
        clusters: Vec::new(),
    }
}

/// The look-alike groups in `faces.clusters`' answer: a group of one is no group, and odd entries are left out.
pub fn clusters_of(v: &Value) -> Vec<Cluster> {
    let groups = v["clusters"].as_array().into_iter().flatten();
    groups
        .filter_map(|g| g["faces"].as_array().map(|f| Cluster { faces: f.iter().filter_map(face_of).collect() }))
        .filter(|c| c.faces.len() >= 2)
        .collect()
}

/// The unnamed faces, asked for again only when the catalog changed, or (while the scan runs and learns more faces) at
/// most every few seconds, since putting look-alikes together looks at every pair.
pub fn faces(app: &mut LightcraftApp, now: f64) -> Arc<Unnamed> {
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    if let Some((r, i, at, list)) = &app.caches.unnamed
        && *r == rev
        && (*i == indexed || now - *at < 3.0)
    {
        return list.clone();
    }
    let v = app.session.execute("faces.unnamed", &json!({})).unwrap_or(Value::Null);
    let groups = app.session.execute("faces.clusters", &json!({})).unwrap_or(Value::Null);
    let mut list = Unnamed { clusters: clusters_of(&groups), ..parse(&v) };
    // the groups do not say what each face is suggested, so a group's faces take it from the unnamed list
    let suggested: std::collections::HashMap<(u64, usize), Option<String>> =
        list.faces.iter().map(|f| ((f.photo, f.index), f.suggestion.clone())).collect();
    for f in list.clusters.iter_mut().flat_map(|c| c.faces.iter_mut()) {
        if let Some(s) = suggested.get(&(f.photo, f.index)) {
            f.suggestion = s.clone();
        }
    }
    let list = Arc::new(list);
    // what was selected and has since been named (or removed) is no longer selected
    let present: std::collections::HashSet<(u64, usize)> = list.faces.iter().map(|f| (f.photo, f.index)).collect();
    app.ui.unnamed_selected.retain(|k| present.contains(k));
    // an expanded group whose first face is gone (named, or no longer a group) is collapsed again
    if !list.clusters.is_empty() {
        let firsts: std::collections::HashSet<(u64, usize)> = list.clusters.iter().filter_map(group_key).collect();
        app.ui.expanded_groups.retain(|k| firsts.contains(k));
    }
    app.caches.unnamed = Some((rev, indexed, now, list.clone()));
    list
}

/// A click on face number `i` of `faces`: select it (or deselect it), or with Shift every face from the last one clicked.
fn click(app: &mut LightcraftApp, faces: &[UnnamedFace], i: usize, shift: bool) {
    let Some(f) = faces.get(i) else { return };
    match app.ui.unnamed_anchor.filter(|_| shift) {
        Some(a) => {
            for f in faces.get(a.min(i)..=a.max(i)).unwrap_or_default() {
                app.ui.unnamed_selected.insert((f.photo, f.index));
            }
        }
        None => {
            let key = (f.photo, f.index);
            if !app.ui.unnamed_selected.remove(&key) {
                app.ui.unnamed_selected.insert(key);
            }
            app.ui.unnamed_anchor = Some(i);
        }
    }
    // typing goes to the name box at once
    app.ui.unnamed_focus = !app.ui.unnamed_selected.is_empty();
}

/// The request that marks `f` "not this person" for its suggested name (`faces.rejectName`), or nothing when it has no
/// suggestion to reject.
pub fn reject_request(f: &UnnamedFace) -> Option<Value> {
    let who = f.suggestion.as_ref()?;
    Some(json!({"id": f.photo, "index": f.index, "name": who}))
}

/// The key of a look-alike group in `UiState::expanded_groups`: its first face (photo, region).
pub fn group_key(c: &Cluster) -> Option<(u64, usize)> {
    c.faces.first().map(|f| (f.photo, f.index))
}

/// Flip a look-alike group between stacked and expanded in `expanded`; returns whether it is expanded now.
pub fn toggle_expanded(expanded: &mut std::collections::HashSet<(u64, usize)>, key: (u64, usize)) -> bool {
    if expanded.remove(&key) {
        false
    } else {
        expanded.insert(key);
        true
    }
}

/// Whether `c` is shown expanded into its faces.
pub fn is_expanded(app: &LightcraftApp, c: &Cluster) -> bool {
    group_key(c).is_some_and(|k| app.ui.expanded_groups.contains(&k))
}

/// The head row of an expanded group: its name button, Collapse, and the count, above its faces.
pub const EXPANDED_HEAD_H: f32 = 30.0;

/// Where each look-alike group goes in the groups' grid, `cols` wide. Each entry is (faces, expanded). A stacked group
/// takes one cell of `tile` width and `tile + label_h` height; an expanded group starts a new row and takes the full width,
/// its faces in rows of `cols`. Returns each group's (column, top offset) and the height of the whole grid.
pub fn group_grid(groups: &[(usize, bool)], cols: usize, tile: f32, gap: f32, label_h: f32) -> (Vec<(usize, f32)>, f32) {
    let cols = cols.max(1);
    let stacked_h = tile + label_h + gap;
    let (mut at, mut col, mut y) = (Vec::with_capacity(groups.len()), 0usize, 0f32);
    for &(faces, expanded) in groups {
        if expanded {
            if col != 0 {
                y += stacked_h;
                col = 0;
            }
            at.push((0, y));
            y += EXPANDED_HEAD_H + faces.div_ceil(cols) as f32 * (tile + gap);
        } else {
            at.push((col, y));
            col += 1;
            if col == cols {
                col = 0;
                y += stacked_h;
            }
        }
    }
    if col != 0 {
        y += stacked_h;
    }
    (at, y)
}

/// Flip a group between stacked and expanded (the count on its picture, or Collapse).
fn toggle_group(app: &mut LightcraftApp, cluster: &Cluster) {
    if let Some(k) = group_key(cluster) {
        toggle_expanded(&mut app.ui.expanded_groups, k);
    }
}

/// A small × in the top right of `picture`, drawn only on a face that has a suggested name: a click marks the face "not
/// this person" for that name, so the suggestion moves on and a look-alike group no longer holds it. `ns` names the
/// widget (the grid's tiles and an expanded group's faces each have their own), so no two share an id.
fn reject_mark(app: &mut LightcraftApp, ui: &mut egui::Ui, picture: Rect, f: &UnnamedFace, ns: &str) {
    let (Some(request), Some(who)) = (reject_request(f), f.suggestion.as_ref()) else { return };
    let r = Rect::from_min_size(pos2(picture.right() - 22.0, picture.top() + 4.0), vec2(18.0, 18.0));
    let resp = ui.interact(r, egui::Id::new(("reject", ns, f.photo, f.index)), Sense::click());
    register(ui.ctx(), format!("{ns}-reject:{}:{}", f.photo, f.index), r);
    let hot = resp.hovered();
    let p = ui.painter();
    p.rect_filled(r, 4.0, Color32::from_black_alpha(if hot { 210 } else { 160 }));
    let c = r.center();
    let cross = Stroke::new(1.6, Color32::WHITE);
    p.line_segment([pos2(c.x - 4.0, c.y - 4.0), pos2(c.x + 4.0, c.y + 4.0)], cross);
    p.line_segment([pos2(c.x + 4.0, c.y - 4.0), pos2(c.x - 4.0, c.y + 4.0)], cross);
    if resp.on_hover_text(crate::i18n::tr_format!("Not this person: {name}", name = who.as_str())).clicked() {
        let _ = app.run("faces.rejectName", request);
    }
}

/// Name `faces` (photo, region) `name`, as one undo step.
fn name(app: &mut LightcraftApp, faces: &[(u64, usize)], name: &str) {
    let list: Vec<Value> = faces.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
    let _ = app.run("faces.nameFaces", json!({"faces": list, "name": name}));
}

/// The bar above the faces while some are selected: how many, a box to name them all, and the people already named.
pub fn naming_bar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if app.ui.unnamed_selected.is_empty() {
        app.ui.unnamed_name.clear();
        return;
    }
    let t = Tokens::get(ui.ctx());
    let selected: Vec<(u64, usize)> = {
        let mut v: Vec<_> = app.ui.unnamed_selected.iter().copied().collect();
        v.sort_unstable();
        v
    };
    // the name the named faces suggest, when every selected face has the same one
    let suggested: Option<String> = {
        let list = app.caches.unnamed.as_ref().map(|c| c.3.clone());
        list.and_then(|l| {
            let names: std::collections::BTreeSet<Option<&str>> =
                l.faces.iter().filter(|f| app.ui.unnamed_selected.contains(&(f.photo, f.index))).map(|f| f.suggestion.as_deref()).collect();
            (names.len() == 1).then(|| names.into_iter().next().flatten().map(str::to_string)).flatten()
        })
    };
    let mut submit: Option<String> = None;
    let mut clear = false;
    ui.horizontal(|ui| {
        ui.add_space(super::people::PAD - 4.0);
        let n = selected.len();
        ui.label(RichText::new(if n == 1 { "1 face selected".to_string() } else { format!("{n} faces selected") }).color(t.text));
        let hint = suggested.clone().unwrap_or_else(|| "Name…".to_string());
        let field = crate::text_field::TextField::singleline("field:unnamedName", &mut app.ui.unnamed_name).hint(hint).width(240.0).show(ui);
        let edit = &field.response;
        if std::mem::take(&mut app.ui.unnamed_focus) {
            edit.request_focus();
        }
        let typed = app.ui.unnamed_name.trim().to_string();
        let chosen = if typed.is_empty() { suggested.clone().unwrap_or_default() } else { typed.clone() };
        if field.ending == Some(crate::text_field::Ending::Return) && !chosen.is_empty() {
            submit = Some(chosen.clone());
        }
        let label = if n == 1 { "Name face".to_string() } else { format!("Name {n} faces") };
        let b = ui.add_enabled(!chosen.is_empty(), egui::Button::new(label));
        register(ui.ctx(), "unnamed:name", b.rect);
        if b.clicked() {
            submit = Some(chosen);
        }
        let c = ui.button("Clear");
        register(ui.ctx(), "unnamed:clear", c.rect);
        clear = c.clicked() || field.cancelled();
    });
    // the people already named, matching what is typed (a click names the faces)
    let typed = app.ui.unnamed_name.trim().to_lowercase();
    if !typed.is_empty() {
        let people = app.caches.person_names(&app.session.catalog);
        let matching: Vec<&String> = people.iter().filter(|p| p.to_lowercase().starts_with(&typed) && p.to_lowercase() != typed).take(6).collect();
        if !matching.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(super::people::PAD - 4.0);
                for p in matching {
                    let b = ui.add(egui::Button::new(RichText::new(p.as_str()).color(t.text_label)).frame(false));
                    register(ui.ctx(), format!("unnamed:pick:{p}"), b.rect);
                    if b.clicked() {
                        submit = Some(p.clone());
                    }
                }
            });
        }
    }
    if let Some(who) = submit {
        name(app, &selected, &who);
        app.ui.unnamed_selected.clear();
        app.ui.unnamed_name.clear();
        app.ui.unnamed_anchor = None;
    } else if clear {
        app.ui.unnamed_selected.clear();
        app.ui.unnamed_name.clear();
        app.ui.unnamed_anchor = None;
    }
}

/// The heading of the section, at `at`, and what it says about where the faces come from.
pub fn section_header(app: &mut LightcraftApp, ui: &mut egui::Ui, list: &Unnamed, at: egui::Pos2, right: f32) {
    let t = Tokens::get(ui.ctx());
    let count = if list.total > list.faces.len() { format!("{} of {}", list.faces.len(), list.total) } else { list.total.to_string() };
    let p = ui.painter();
    p.text(at, Align2::LEFT_CENTER, "Unnamed faces", t.semibold(15.0), t.text);
    p.text(pos2(right, at.y), Align2::RIGHT_CENTER, count, t.font(13.0), t.text_dim);
    let sub = if !list.ready && list.faces.is_empty() {
        "Turn on face recognition in Settings to find the faces in your photos.".to_string()
    } else if list.faces.is_empty() && app.caches.faces_pending > 0 {
        format!("Looking through your photos… {} left", app.caches.faces_pending)
    } else if list.faces.is_empty() {
        "No unnamed faces. Faces found in your photos appear here until you name them.".to_string()
    } else if list.ordered {
        "Faces that look alike are next to each other. Click to select, Shift-click for a range, then type a name.".to_string()
    } else {
        "Click faces to select them, Shift-click for a range, then type a name.".to_string()
    };
    p.text(pos2(at.x, at.y + 24.0), Align2::LEFT_CENTER, sub, t.font(12.0), t.text_dim);
    if !list.ready && list.faces.is_empty() {
        let r = Rect::from_min_size(pos2(at.x, at.y + 38.0), vec2(120.0, 22.0));
        let b = ui.put(r, egui::Button::new("Open Settings"));
        register(ui.ctx(), "unnamed:openSettings", b.rect);
        if b.clicked() {
            let _ = app.run("app.settings", json!({"tab": "faces"}));
        }
    } else if !list.faces.is_empty() {
        let r = Rect::from_min_size(pos2(right - 130.0, at.y + 12.0), vec2(130.0, 22.0));
        let all = app.ui.unnamed_selected.len() == list.faces.len();
        let b = ui.put(r, egui::Button::new(if all { "Select none" } else { "Select all" }));
        register(ui.ctx(), "unnamed:selectAll", b.rect);
        if b.clicked() {
            app.ui.unnamed_selected.clear();
            if !all {
                app.ui.unnamed_selected.extend(list.faces.iter().map(|f| (f.photo, f.index)));
                app.ui.unnamed_focus = true;
            }
        }
    }
}

/// How far each face behind a group's top face is offset (down and to the right).
const STACK_STEP: f32 = 4.0;

/// Select a group's faces and put the keyboard in the name box, so the naming bar names them all at once.
fn select_group(app: &mut LightcraftApp, cluster: &Cluster) {
    app.ui.unnamed_selected.clear();
    app.ui.unnamed_selected.extend(cluster.faces.iter().map(|f| (f.photo, f.index)));
    app.ui.unnamed_anchor = None;
    app.ui.unnamed_focus = true;
}

/// One look-alike group in the People view, in a cell `edge` wide: its top face with the next ones stacked behind it, how
/// many faces it has, and a button that selects them all for naming. A click on the count shows every face instead (an
/// expanded group, drawn by `cluster_expanded`); `cols` is how many face cells fit across the view.
pub fn cluster_card(app: &mut LightcraftApp, ui: &mut egui::Ui, cluster: &Cluster, r: Rect, edge: f32, ppp: f32, cols: usize) {
    if is_expanded(app, cluster) {
        return cluster_expanded(app, ui, cluster, r, edge, ppp, cols);
    }
    let t = Tokens::get(ui.ctx());
    let Some(top) = cluster.faces.first() else { return };
    let picture = Rect::from_min_size(r.min, vec2(edge, edge));
    let job = app.session.face_job(lightcraft_catalog::PhotoId(top.photo), top.view, (edge * ppp).ceil() as usize);
    let tex = job.and_then(|job| app.renderer.variant(job));
    let p = ui.painter();
    for k in (1..cluster.faces.len().min(3)).rev() {
        let step = STACK_STEP * k as f32;
        let back = Rect::from_min_size(picture.min + vec2(step, step), vec2(edge, edge));
        p.rect_filled(back, 3.0, t.canvas);
        p.rect_stroke(back, 3.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
    }
    p.rect_filled(picture, 3.0, t.canvas);
    if let Some(tex) = tex {
        p.image(tex.tex.id(), picture, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // how many faces the group has, on the picture
    let g = p.layout_no_wrap(cluster.faces.len().to_string(), t.font(11.5), Color32::WHITE);
    let pill = Rect::from_min_size(pos2(picture.right() - g.size().x - 14.0, picture.bottom() - g.size().y - 11.0), g.size() + vec2(10.0, 6.0));
    p.rect_filled(pill, 4.0, Color32::from_black_alpha(175));
    p.galley(pill.min + vec2(5.0, 3.0), g, Color32::WHITE);
    let label = crate::i18n::tr_format!("{n} faces", n = cluster.faces.len());
    p.text(pos2(picture.left() + 2.0, picture.bottom() + 18.0), Align2::LEFT_CENTER, label, t.font(12.0), t.text_dim);
    // the count opens the group: every face in a row
    let count = ui.interact(pill, egui::Id::new(("group-count", top.photo, top.index)), Sense::click());
    register(ui.ctx(), format!("unnamed:groupCount:{}:{}", top.photo, top.index), pill);
    if count.on_hover_text(crate::i18n::tr("Show all faces")).clicked() {
        toggle_group(app, cluster);
        return;
    }
    // the group's top face is the one picture on the card, so the × for "not this person" is on it
    reject_mark(app, ui, picture, top, "unnamed-group");
    let b = ui.put(
        Rect::from_min_size(pos2(picture.left(), picture.bottom() + 26.0), vec2(edge.min(160.0), 24.0)),
        egui::Button::new(crate::i18n::tr("Name this person")),
    );
    register(ui.ctx(), format!("unnamed:group:{}:{}", top.photo, top.index), b.rect);
    if b.clicked() {
        select_group(app, cluster);
    }
}

/// A look-alike group shown expanded in a block `r` wide: a head row (Name this person, Collapse, the count), then every
/// face in rows of `cols`, each a tile like the Unnamed faces (click to select, a suggested name's × and accept).
fn cluster_expanded(app: &mut LightcraftApp, ui: &mut egui::Ui, cluster: &Cluster, r: Rect, edge: f32, ppp: f32, cols: usize) {
    let t = Tokens::get(ui.ctx());
    let Some(key) = group_key(cluster) else { return };
    let name_w = edge.min(160.0);
    let b = ui.put(Rect::from_min_size(pos2(r.left(), r.top() + 2.0), vec2(name_w, 24.0)), egui::Button::new(crate::i18n::tr("Name this person")));
    register(ui.ctx(), format!("unnamed:group:{}:{}", key.0, key.1), b.rect);
    if b.clicked() {
        select_group(app, cluster);
    }
    let c =
        ui.put(Rect::from_min_size(pos2(r.left() + name_w + 8.0, r.top() + 2.0), vec2(96.0, 24.0)), egui::Button::new(crate::i18n::tr("Collapse")));
    register(ui.ctx(), format!("unnamed:groupCollapse:{}:{}", key.0, key.1), c.rect);
    if c.clicked() {
        return toggle_group(app, cluster);
    }
    let n = crate::i18n::tr_format!("{n} faces", n = cluster.faces.len());
    ui.painter().text(pos2(r.left() + name_w + 112.0, r.top() + EXPANDED_HEAD_H / 2.0), Align2::LEFT_CENTER, n, t.font(12.0), t.text_dim);
    let step = edge + super::people::TILE_GAP;
    for j in 0..cluster.faces.len() {
        let min = r.min + vec2((j % cols) as f32 * step, EXPANDED_HEAD_H + (j / cols) as f32 * step);
        tile(app, ui, &cluster.faces, j, Rect::from_min_size(min, vec2(edge, edge)), ppp, "unnamed-group");
    }
}

/// One face tile, number `i` of `faces` (the Unnamed faces, or one look-alike group's). `ns` names its widgets: "unnamed"
/// for the grid, so a face shown in both places still has one id in each.
pub fn tile(app: &mut LightcraftApp, ui: &mut egui::Ui, faces: &[UnnamedFace], i: usize, r: Rect, ppp: f32, ns: &str) {
    let Some(f) = faces.get(i) else { return };
    let t = Tokens::get(ui.ctx());
    let key = (f.photo, f.index);
    let selected = app.ui.unnamed_selected.contains(&key);
    let resp = ui.interact(r, egui::Id::new(("face", ns, f.photo, f.index)), Sense::click());
    register(ui.ctx(), format!("{ns}-face:{}:{}", f.photo, f.index), r);
    // a suggested name along the bottom: a click accepts it for this face (added after the tile, so it wins there)
    let strip = Rect::from_min_max(pos2(r.left(), r.bottom() - 20.0), r.right_bottom());
    let accept = f.suggestion.as_ref().map(|_| ui.interact(strip, egui::Id::new(("accept", ns, f.photo, f.index)), Sense::click()));
    if let Some(a) = &accept {
        register(ui.ctx(), format!("{ns}-accept:{}:{}", f.photo, f.index), a.rect);
    }
    let hovered = resp.hovered() || accept.as_ref().is_some_and(|a| a.hovered());
    let p = ui.painter();
    p.rect_filled(r, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(lightcraft_catalog::PhotoId(f.photo), f.view, (r.width() * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if selected {
        p.rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(70, 130, 255, 55));
        p.rect_stroke(r, 3.0, Stroke::new(2.0, Color32::from_rgb(110, 160, 255)), StrokeKind::Inside);
        // a check at the top left
        let c = pos2(r.left() + 13.0, r.top() + 13.0);
        p.circle_filled(c, 9.0, Color32::from_rgb(70, 130, 255));
        p.line_segment([pos2(c.x - 4.0, c.y), pos2(c.x - 1.0, c.y + 3.5)], Stroke::new(1.8, Color32::WHITE));
        p.line_segment([pos2(c.x - 1.0, c.y + 3.5), pos2(c.x + 4.5, c.y - 3.5)], Stroke::new(1.8, Color32::WHITE));
    } else if hovered {
        p.rect_stroke(r, 3.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
    }
    if let Some(name) = &f.suggestion {
        p.rect_filled(strip, 0.0, Color32::from_black_alpha(165));
        let label = super::people::fit(p, name, t.font(11.5), r.width() - 18.0);
        p.text(pos2(r.left() + 5.0, strip.center().y), Align2::LEFT_CENTER, format!("{label}?"), t.font(11.5), Color32::from_gray(225));
    }
    reject_mark(app, ui, r, f, ns);
    if let (Some(a), Some(who)) = (&accept, &f.suggestion)
        && a.clone().on_hover_text(format!("Name this face {who}")).clicked()
    {
        name(app, &[key], who);
        return;
    }
    if resp.clicked() {
        let shift = ui.input(|i| i.modifiers.shift);
        click(app, faces, i, shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engines_answer_becomes_a_list_and_odd_entries_are_left_out() {
        let v = json!({
            "total": 9, "ordered": true, "ready": true,
            "faces": [
                {"photo": 3, "index": 1, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "view": {"x0": 0.12, "y0": 0.12, "x1": 0.28, "y1": 0.38}, "suggestion": {"name": "Jane Doe", "score": 0.7}},
                {"photo": 4, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "suggestion": null},
                {"photo": 5, "index": 0},
                {"index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}},
            ],
        });
        let list = parse(&v);
        assert_eq!((list.total, list.ordered, list.ready, list.faces.len()), (9, true, true, 2));
        assert_eq!(list.faces[0].suggestion.as_deref(), Some("Jane Doe"));
        assert_eq!(list.faces[0].view.x0, 0.12, "the view box is used when there is one");
        assert_eq!((list.faces[1].view.x0, list.faces[1].suggestion.clone()), (0.1, None), "else the face's own box");
        for bad in [Value::Null, json!([]), json!({"faces": 5, "total": -1})] {
            assert_eq!(parse(&bad), Unnamed::default(), "{bad}");
        }
    }

    #[test]
    fn a_group_of_one_is_no_group_and_odd_groups_are_left_out() {
        let face = |photo: u64| json!({"photo": photo, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}});
        let v = json!({"clusters": [
            {"size": 2, "faces": [face(3), face(4)]},
            {"size": 1, "faces": [face(5)]},
            {"size": 2},
            "junk",
        ]});
        let groups = clusters_of(&v);
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].faces.iter().map(|f| f.photo).collect::<Vec<_>>(), vec![3, 4]);
        assert!(clusters_of(&Value::Null).is_empty());
        assert_eq!(parse(&v).clusters, Vec::new(), "the groups come from faces.clusters, not faces.unnamed");
    }

    #[test]
    fn not_this_person_asks_for_the_suggested_name_and_only_then() {
        let face = |suggestion: Option<&str>| UnnamedFace {
            photo: 7,
            index: 2,
            view: lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
            suggestion: suggestion.map(str::to_string),
        };
        assert_eq!(reject_request(&face(Some("Jane Doe"))), Some(json!({"id": 7, "index": 2, "name": "Jane Doe"})));
        assert_eq!(reject_request(&face(None)), None, "no suggestion, so nothing to reject");
    }

    #[test]
    fn expanding_and_collapsing_a_group_flips_only_that_group() {
        let (a, b) = ((3u64, 0usize), (9u64, 4usize));
        let mut expanded = std::collections::HashSet::new();
        assert!(toggle_expanded(&mut expanded, a), "stacked, so the first click expands it");
        assert!(!toggle_expanded(&mut expanded, a), "expanded, so the next click collapses it");
        assert!(expanded.is_empty());
        toggle_expanded(&mut expanded, a);
        toggle_expanded(&mut expanded, b);
        assert_eq!(expanded.len(), 2, "each group keeps its own state");
        toggle_expanded(&mut expanded, a);
        assert_eq!(expanded.iter().copied().collect::<Vec<_>>(), vec![b], "collapsing a leaves b expanded");
    }

    #[test]
    fn a_group_is_keyed_by_its_first_face() {
        let face = |photo: u64, index: usize| UnnamedFace {
            photo,
            index,
            view: lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
            suggestion: None,
        };
        let group = Cluster { faces: vec![face(5, 1), face(6, 2)] };
        assert_eq!(group_key(&group), Some((5, 1)));
        assert_eq!(group_key(&Cluster::default()), None);
    }

    #[test]
    fn a_request_is_built_for_a_face_that_is_not_the_top_one() {
        let face = |photo: u64, index: usize, suggestion: Option<&str>| UnnamedFace {
            photo,
            index,
            view: lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
            suggestion: suggestion.map(str::to_string),
        };
        // the group's top face has no suggestion; its second face does, and that one is the request's
        let group = Cluster { faces: vec![face(1, 0, None), face(9, 3, Some("Jane Doe")), face(4, 2, Some("Jane Doe"))] };
        assert_eq!(reject_request(&group.faces[1]), Some(json!({"id": 9, "index": 3, "name": "Jane Doe"})));
        assert_eq!(reject_request(&group.faces[0]), None);
    }

    #[test]
    fn stacked_groups_share_rows_and_an_expanded_group_takes_a_full_row() {
        // three stacked groups in two columns: two on the first row, one on the second
        let (at, h) = group_grid(&[(2, false), (3, false), (2, false)], 2, 100.0, 14.0, 52.0);
        assert_eq!(at, vec![(0, 0.0), (1, 0.0), (0, 166.0)]);
        assert_eq!(h, 332.0);
        // an expanded group between them starts a row of its own, and the stacked one after it goes below it
        let (at, _) = group_grid(&[(2, false), (5, true), (2, false)], 2, 100.0, 14.0, 52.0);
        assert_eq!(at, vec![(0, 0.0), (0, 166.0), (0, 166.0 + 30.0 + 3.0 * 114.0)]);
        assert_eq!(group_grid(&[], 2, 100.0, 14.0, 52.0), (Vec::new(), 0.0));
    }
}
