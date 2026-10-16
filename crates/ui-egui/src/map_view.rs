//! The Classic Map view (`map.view`): the photos that have a GPS position as markers on a Web Mercator map you
//! pan (drag) and zoom (wheel, pinch, + / −, double-click), with the saved locations drawn as circles. Markers
//! merge into one numbered cluster at low zoom; a click selects the photos of a marker. A strip of thumbnails
//! under the map is where photos are dragged from onto the map to give them a GPS position (`photo.setLocation`,
//! one undo step). A GPX track loaded with Load Tracklog is drawn as a line (`map.loadTrack`, `map.showTrack`); the
//! place search centres the map (`map.search`); a saved location is added on the map's context menu, moved by dragging its
//! centre, and edited or deleted in its window (`location.setCircle`, `location.delete`, one undo step each); the map style
//! is the offline grid, OpenStreetMap's standard layer or a custom address (`map.style`).
//!
//! The map is offline by default: a plain latitude / longitude grid, and no network request of any kind. Tiles
//! come only when the user turned **Online map** on in Settings (`map.online`); they are fetched by
//! [`lightcraft_engine::map_tiles`], which keeps OpenStreetMap's tile usage policy, and the credit
//! "© OpenStreetMap contributors" is drawn on the map. The geometry is [`lightcraft_engine::map`]; the markers
//! are [`lightcraft_engine::cmd::map::geotagged`].

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::{LocationId, PhotoId};
use lightcraft_engine::map::{self, Cluster, TileKey, View};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::AppSettings;
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::register;

/// Radius of a marker, and how close a click must be to hit one (screen points).
const MARKER_R: f32 = 12.0;
const HIT_R: f64 = 16.0;
/// The most tiles asked for at once, whatever the window size.
const MAX_TILES: usize = 64;
/// Tile textures kept; the ones off screen are dropped past this.
const MAX_TEXTURES: usize = 192;
/// Meridians and parallels are drawn at least this many points apart.
const GRID_MIN_PX: f64 = 80.0;
/// The position list is rebuilt this often even when nothing seems to have changed.
const POINTS_REFRESH: Duration = Duration::from_secs(1);
/// The strip under the map: its height, the header line's, and a thumbnail's width (points).
const STRIP_H: f32 = 118.0;
const STRIP_HEAD: f32 = 22.0;
const STRIP_CELL: f32 = 92.0;

#[cfg(not(target_arch = "wasm32"))]
mod tiles {
    pub use lightcraft_engine::map_tiles::{TileLoader, TileState};
}

/// The web build has no tile loader (no threads, no sockets): the map is the offline grid there.
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
mod tiles {
    use lightcraft_engine::map::TileKey;

    pub struct TileImage {
        pub width: usize,
        pub height: usize,
        pub rgba: Vec<u8>,
    }

    #[derive(Clone)]
    pub enum TileState {
        Offline,
        Loading,
        Failed,
        Ready(std::sync::Arc<TileImage>),
    }

    #[derive(Clone)]
    pub struct TileLoader;

    impl TileLoader {
        pub fn http(_: lightcraft_engine::activity::Activity) -> TileLoader {
            TileLoader
        }

        pub fn want(&self, _: bool, _: &str, keys: &[TileKey]) -> Vec<TileState> {
            vec![TileState::Offline; keys.len()]
        }
    }
}

use tiles::{TileLoader, TileState};

/// The tile loader, made the first time the online map is on.
#[derive(Clone)]
struct Loader(TileLoader);

impl std::fmt::Debug for Loader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TileLoader")
    }
}

/// The tile textures on the graphics card, by tile.
#[derive(Clone, Default)]
struct Textures(HashMap<TileKey, egui::TextureHandle>);

impl std::fmt::Debug for Textures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Textures({})", self.0.len())
    }
}

/// The map view's state: where it looks, the markers and the tile textures. Not saved.
#[derive(Clone, Debug, Default)]
pub struct Run {
    pub view: View,
    /// The view has been fitted to the photos (done once, on the first frame that has a map size).
    fitted: bool,
    /// The shown photos with a position, rebuilt when the library, the filter or an edit changes them.
    points: Vec<(PhotoId, f64, f64)>,
    points_key: Option<(u64, usize, usize)>,
    points_at: Option<web_time::Instant>,
    points_version: u64,
    clusters: Vec<Cluster>,
    clusters_key: Option<(u64, u64)>,
    loader: Option<Loader>,
    textures: Textures,
    /// The tile address the textures belong to.
    textures_for: String,
    /// The photos the strip lists (the shown ones, or only those without a position).
    strip: Vec<PhotoId>,
    only_untagged: bool,
    /// The photos being dragged from the strip, until the pointer is let go.
    drag: Option<Vec<PhotoId>>,
    /// The place search: the text typed, its results and the text they were found for.
    search: String,
    search_hits: Vec<Value>,
    search_for: String,
    /// A saved location's centre being dragged: its id and where the pointer has taken it.
    moving: Option<(u64, f64, f64)>,
    /// The saved location window, when open.
    editor: Option<Editor>,
    /// The saved location under the pointer when the context menu opened, and the place it opened at.
    menu_hit: Option<(u64, f64, f64)>,
    menu_at: (f64, f64),
}

impl Run {
    /// Photos on the map.
    pub fn photos(&self) -> usize {
        self.points.len()
    }
}

/// Search results shown under the toolbar.
const SEARCH_SHOWN: usize = 8;

/// A saved location open in the window: `id` is `None` for one to be added at `lat`, `lon`.
#[derive(Clone, Debug)]
struct Editor {
    id: Option<u64>,
    name: String,
    radius: String,
    lat: f64,
    lon: f64,
}

/// What the map's context menu chose.
#[derive(Clone, Copy, Debug)]
enum MapMenu {
    New,
    Edit(u64),
    Delete(u64),
}

/// Opens the map view (`map.view`).
pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    app.ui.classic_map = Some(Run::default());
    Ok(json!({"online": app.ui.settings.online_map, "tileUrl": template(&app.ui.settings)}))
}

/// `map.online {enabled?}`: the Online map setting (no `enabled`: toggles it). Off, the map makes no network request.
pub fn set_online(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let on = p.get("enabled").and_then(Value::as_bool).unwrap_or(!app.ui.settings.online_map);
    app.ui.settings.online_map = on;
    Ok(json!({"online": on}))
}

/// `map.tileUrl {url?}`: the tile address with `{z}`, `{x}` and `{y}`; none or empty restores OpenStreetMap's.
pub fn set_tile_url(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let url = p.get("url").and_then(Value::as_str).unwrap_or("").trim();
    if url.is_empty() || url == map::DEFAULT_TILE_URL {
        app.ui.settings.map_tile_url.clear();
    } else {
        map::check_template(url)?;
        app.ui.settings.map_tile_url = url.to_string();
    }
    Ok(json!({"tileUrl": template(&app.ui.settings)}))
}

/// The tile address in use.
pub fn template(settings: &AppSettings) -> String {
    if settings.map_tile_url.trim().is_empty() { map::DEFAULT_TILE_URL.to_string() } else { settings.map_tile_url.trim().to_string() }
}

/// Meridians and parallels every this many degrees: the smallest step that keeps them [`GRID_MIN_PX`] apart.
pub fn grid_step(zoom: f64) -> f64 {
    const STEPS: [f64; 18] = [0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 45.0, 90.0];
    let per_degree = map::world_size(zoom) / 360.0;
    STEPS.iter().copied().find(|step| step * per_degree >= GRID_MIN_PX).unwrap_or(90.0)
}

/// A grid label: `value` degrees with as many decimals as the step needs.
pub fn degrees_label(value: f64, step: f64) -> String {
    let decimals = (-step.log10() - 1e-9).ceil().max(0.0) as usize;
    format!("{value:.decimals$}°")
}

/// The nearest marker within `radius` points of `pointer` (both relative to the map's centre), if any.
pub fn marker_at(view: &View, clusters: &[Cluster], pointer: (f64, f64), radius: f64) -> Option<usize> {
    clusters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (dx, dy) = view.offset(c.lat, c.lon);
            (i, (dx - pointer.0).hypot(dy - pointer.1))
        })
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The map style in use, by its `map.style` name: `grid` (offline), `osm` (OpenStreetMap's standard layer) or `custom`.
pub fn style_name(settings: &AppSettings) -> &'static str {
    if !settings.online_map {
        "grid"
    } else if template(settings) == map::DEFAULT_TILE_URL {
        "osm"
    } else {
        "custom"
    }
}

/// The label of a map style in the style menu.
fn style_label(style: &str) -> &'static str {
    match style {
        "osm" => crate::i18n::tr("OpenStreetMap standard"),
        "custom" => crate::i18n::tr("Custom URL…"),
        _ => crate::i18n::tr("Offline grid"),
    }
}

/// `map.style {style: grid | osm | custom, url?}`: the map's tiles. `grid` makes no request and is the default; `osm` is
/// OpenStreetMap's standard layer; `custom` uses `url`, a tile address with `{z}`, `{x}` and `{y}`.
pub fn set_style(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let style = p.get("style").and_then(Value::as_str).unwrap_or("");
    let url = p.get("url").and_then(Value::as_str).unwrap_or("");
    let tiles = map::style_tile_url(style, url)?;
    app.ui.settings.online_map = tiles.is_some();
    match style {
        "custom" => app.ui.settings.map_tile_url = tiles.unwrap_or_default(),
        "osm" => app.ui.settings.map_tile_url.clear(),
        _ => {}
    }
    Ok(json!({"style": style_name(&app.ui.settings), "online": app.ui.settings.online_map, "tileUrl": template(&app.ui.settings)}))
}

/// The saved location whose centre is within [`HIT_R`] points of `pointer` (on a map whose centre is `centre`): its id
/// and position, the nearest when several are.
fn centre_at(app: &LightcraftApp, view: &View, centre: Pos2, pointer: Pos2) -> Option<(u64, f64, f64)> {
    let (ox, oy) = (f64::from(pointer.x - centre.x), f64::from(pointer.y - centre.y));
    app.session
        .catalog
        .saved_locations()
        .map(|(id, l)| {
            let (dx, dy) = view.offset(l.lat, l.lon);
            (id.0, l.lat, l.lon, (dx - ox).hypot(dy - oy))
        })
        .filter(|h| h.3 <= HIT_R)
        .min_by(|a, b| a.3.total_cmp(&b.3))
        .map(|h| (h.0, h.1, h.2))
}

/// Runs the place search again when its text has changed (`map.search`, offline).
fn refresh_search(app: &mut LightcraftApp, run: &mut Run) {
    if run.search == run.search_for {
        return;
    }
    run.search_for = run.search.clone();
    run.search_hits = match app.run("map.search", json!({"query": run.search})) {
        Ok(v) => v["results"].as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };
}

/// Opens the saved location window on the location `id`.
fn open_editor(app: &LightcraftApp, run: &mut Run, id: u64) {
    if let Some(l) = app.session.catalog.saved_location(LocationId(id)) {
        run.editor = Some(Editor { id: Some(id), name: l.name.clone(), radius: format!("{:.0}", l.radius_m), lat: l.lat, lon: l.lon });
    }
}

/// The saved location window: a name, a radius in metres and the centre, with Save, Delete (for a saved one) and Cancel.
/// Returns whether it stays open.
fn edit_window(app: &mut LightcraftApp, ctx: &egui::Context, editor: &mut Editor) -> bool {
    let tr = crate::i18n::tr;
    let mut open = true;
    let mut action: Option<&'static str> = None;
    egui::Window::new(tr("Saved Location")).id(egui::Id::new("map-location-editor")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
        egui::Grid::new("map-location-grid").num_columns(2).show(ui, |ui| {
            ui.label(tr("Name"));
            if TextField::singleline("field:mapLocationName", &mut editor.name).width(220.0).show(ui).committed() {
                action = Some("save");
            }
            ui.end_row();
            ui.label(tr("Radius (metres)"));
            if TextField::singleline("field:mapLocationRadius", &mut editor.radius).width(220.0).select_on_focus(true).show(ui).committed() {
                action = Some("save");
            }
            ui.end_row();
            ui.label(tr("Centre"));
            ui.label(format!("{:.5}, {:.5}", editor.lat, editor.lon));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button(tr("Save")).clicked() {
                action = Some("save");
            }
            if editor.id.is_some() && ui.button(tr("Delete")).clicked() {
                action = Some("delete");
            }
            if ui.button(tr("Cancel")).clicked() {
                action = Some("cancel");
            }
        });
    });
    let mut keep = open;
    match action {
        Some("save") => {
            let name = editor.name.trim().to_string();
            match editor.radius.trim().parse::<f64>() {
                Ok(radius_m) => {
                    let result = match editor.id {
                        Some(id) => app.run("location.setCircle", json!({"id": id, "name": name, "lat": editor.lat, "lon": editor.lon, "radiusM": radius_m})),
                        None => app.run("location.add", json!({"name": name, "lat": editor.lat, "lon": editor.lon, "radiusM": radius_m})),
                    };
                    match result {
                        Ok(_) => keep = false,
                        Err(e) => app.toast_error(ctx, e),
                    }
                }
                Err(_) => app.toast_error(ctx, tr("The radius must be a number of metres")),
            }
        }
        Some("delete") => {
            if let Some(id) = editor.id {
                match app.run("location.delete", json!({"id": id})) {
                    Ok(_) => keep = false,
                    Err(e) => app.toast_error(ctx, e),
                }
            }
        }
        Some(_) => keep = false,
        None => {}
    }
    keep
}

/// Rebuild the list of photos with a position when the library, the filter or an edit changed it.
fn refresh_points(app: &mut LightcraftApp, run: &mut Run) {
    let (generation, _) = app.session.visible_shared();
    let key = (generation, app.session.undo.len(), app.session.redo.len());
    if run.points_key == Some(key) && run.points_at.is_some_and(|t| t.elapsed() < POINTS_REFRESH) {
        return;
    }
    let points = lightcraft_engine::cmd::map::geotagged(&mut app.session);
    if points != run.points {
        run.points = points;
        run.points_version += 1;
    }
    run.points_key = Some(key);
    run.points_at = Some(web_time::Instant::now());
    let shown = app.session.visible_cloned();
    run.strip = if run.only_untagged {
        shown.into_iter().filter(|id| app.session.catalog.photo(*id).is_some_and(|p| p.meta.gps.is_none())).collect()
    } else {
        shown
    };
}

/// Markers for the current zoom (in quarter levels, so a wheel turn doesn't regroup every frame).
fn refresh_clusters(run: &mut Run) {
    let zoom = (run.view.zoom * 4.0).round() / 4.0;
    let key = (run.points_version, zoom.to_bits());
    if run.clusters_key != Some(key) {
        run.clusters = map::cluster(&run.points, zoom, map::CLUSTER_CELL_PX);
        run.clusters_key = Some(key);
    }
}

/// The state of each tile the map shows. With the online map off nothing is asked for and no loader is made.
fn request_tiles(run: &mut Run, activity: &lightcraft_engine::activity::Activity, settings: &AppSettings, keys: &[TileKey]) -> Vec<TileState> {
    if !settings.online_map || cfg!(target_arch = "wasm32") {
        return vec![TileState::Offline; keys.len()];
    }
    let loader = run.loader.get_or_insert_with(|| Loader(TileLoader::http(activity.clone())));
    loader.0.want(true, &template(settings), keys)
}

/// Draws the map view: a toolbar, then the map.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui) {
    let Some(mut run) = app.ui.classic_map.take() else { return };
    let t = Tokens::get(ctx);
    refresh_points(app, &mut run);
    let (mut close, mut fit) = (false, false);
    let mut zoom = 0.0;
    let tr = crate::i18n::tr;
    let style_now = style_name(&app.ui.settings);
    let custom_url = app.ui.settings.map_tile_url.trim().to_string();
    let mut show_track = app.session.map_show_track;
    let (mut track_toggled, mut load_track, mut style_chosen) = (false, false, None::<&'static str>);
    ui.horizontal(|ui| {
        let r = ui.button(crate::i18n::tr("Close"));
        register(ctx, "button:mapClose", r.rect);
        close = r.clicked();
        let r = ui.button(crate::i18n::tr("Fit photos"));
        register(ctx, "button:mapFit", r.rect);
        fit = r.clicked();
        let r = ui.button("−").on_hover_text(crate::i18n::tr("Zoom Out"));
        register(ctx, "button:mapZoomOut", r.rect);
        if r.clicked() {
            zoom -= 1.0;
        }
        let r = ui.button("+").on_hover_text(crate::i18n::tr("Zoom In"));
        register(ctx, "button:mapZoomIn", r.rect);
        if r.clicked() {
            zoom += 1.0;
        }
        let r = egui::ComboBox::from_id_salt("map-style").selected_text(style_label(style_now)).show_ui(ui, |ui| {
            for (style, label) in [("grid", tr("Offline grid")), ("osm", tr("OpenStreetMap standard")), ("custom", tr("Custom URL…"))] {
                if ui.selectable_label(style == style_now, label).clicked() {
                    style_chosen = Some(style);
                }
            }
        });
        let _ = r.response.on_hover_text(tr("Map Style"));
        let _ = TextField::singleline("field:mapSearch", &mut run.search).width(200.0).hint(tr("Search places or lat, lon")).show(ui);
        let r = ui.button(tr("Load Tracklog…"));
        register(ctx, "button:mapLoadTrack", r.rect);
        load_track = r.clicked();
        track_toggled = ui.checkbox(&mut show_track, tr("Show Track")).changed();
        let n = run.points.len();
        let what = crate::i18n::tr(if n == 1 { "photo on the map" } else { "photos on the map" });
        ui.label(egui::RichText::new(format!("{n} {what}")).color(t.text_dim));
    });
    refresh_search(app, &mut run);
    if !run.search_hits.is_empty() {
        ui.horizontal_wrapped(|ui| {
            let mut picked = None;
            for hit in run.search_hits.iter().take(SEARCH_SHOWN) {
                let (Some(name), Some(lat), Some(lon), Some(zoom)) = (hit["name"].as_str(), hit["lat"].as_f64(), hit["lon"].as_f64(), hit["zoom"].as_f64())
                else {
                    continue;
                };
                let kind = match hit["kind"].as_str() {
                    Some("position") => crate::i18n::tr("Position"),
                    Some("location") => crate::i18n::tr("Saved Location"),
                    _ => crate::i18n::tr("Place"),
                };
                if ui.small_button(format!("{name} · {kind}")).clicked() {
                    picked = Some((lat, lon, zoom));
                }
            }
            if let Some((lat, lon, zoom)) = picked {
                run.view = View::new(lat, lon, zoom);
                run.search.clear();
            }
        });
    }
    ui.add_space(4.0);
    let total = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(vec2(total.x, (total.y - STRIP_H - 6.0).max(0.0)), Sense::click_and_drag());
    ui.add_space(6.0);
    let (strip_rect, _) = ui.allocate_exact_size(vec2(total.x, STRIP_H), Sense::hover());
    if rect.width() < 8.0 || rect.height() < 8.0 {
        app.ui.classic_map = (!close).then_some(run);
        return;
    }
    if !run.fitted || fit {
        run.fitted = true;
        fit_to_photos(app, &mut run, rect);
    }
    if zoom != 0.0 {
        run.view.zoom_at(zoom, 0.0, 0.0);
    }
    let centre = rect.center();
    let pointer_offset = |p: Pos2| (f64::from(p.x - centre.x), f64::from(p.y - centre.y));
    // a drag that starts on a saved location's centre moves it; any other drag pans; wheel and pinch zoom about the pointer
    if response.drag_started() {
        run.moving = ui.input(|i| i.pointer.press_origin()).and_then(|p| centre_at(app, &run.view, centre, p));
    }
    if response.dragged() {
        match (run.moving, response.interact_pointer_pos()) {
            (Some((id, _, _)), Some(p)) => {
                let (lat, lon) = run.view.locate(f64::from(p.x - centre.x), f64::from(p.y - centre.y));
                run.moving = Some((id, lat, lon));
            }
            _ => {
                let d = response.drag_delta();
                run.view.pan(f64::from(d.x), f64::from(d.y));
            }
        }
    }
    if response.drag_stopped()
        && let Some((id, lat, lon)) = run.moving.take()
        && app.session.catalog.saved_location(LocationId(id)).is_some_and(|l| l.lat != lat || l.lon != lon)
        && let Err(e) = app.run("location.setCircle", json!({"id": id, "lat": lat, "lon": lon}))
    {
        app.toast_error(ctx, e);
    }
    if response.hovered() {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let levels = f64::from(scroll) / 240.0 + f64::from(pinch).log2();
        if levels.abs() > 1e-6
            && let Some(p) = response.hover_pos()
        {
            let (ox, oy) = pointer_offset(p);
            run.view.zoom_at(levels, ox, oy);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    if response.double_clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let (ox, oy) = pointer_offset(p);
        run.view.zoom_at(1.0, ox, oy);
    }
    refresh_clusters(&mut run);
    // the context menu: a saved location under the pointer can be edited or deleted; anywhere, a new one can be added
    if response.secondary_clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let (ox, oy) = pointer_offset(p);
        run.menu_at = run.view.locate(ox, oy);
        run.menu_hit = centre_at(app, &run.view, centre, p);
    }
    let hit = run.menu_hit;
    let mut menu: Option<MapMenu> = None;
    response.context_menu(|ui| {
        if let Some((id, _, _)) = hit {
            if ui.button(tr("Edit Saved Location…")).clicked() {
                menu = Some(MapMenu::Edit(id));
                ui.close();
            }
            if ui.button(tr("Delete Saved Location")).clicked() {
                menu = Some(MapMenu::Delete(id));
                ui.close();
            }
            ui.separator();
        }
        if ui.button(tr("New Saved Location Here")).clicked() {
            menu = Some(MapMenu::New);
            ui.close();
        }
    });
    match menu {
        Some(MapMenu::New) => {
            let (lat, lon) = run.menu_at;
            run.editor = Some(Editor { id: None, name: String::new(), radius: "500".into(), lat, lon });
        }
        Some(MapMenu::Edit(id)) => open_editor(app, &mut run, id),
        Some(MapMenu::Delete(id)) => {
            if let Err(e) = app.run("location.delete", json!({"id": id})) {
                app.toast_error(ctx, e);
            }
        }
        None => {}
    }
    // a click on a marker selects its photos
    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
        && let Some(i) = marker_at(&run.view, &run.clusters, pointer_offset(p), HIT_R)
    {
        let ids: Vec<u64> = run.clusters.get(i).map(|c| c.ids.iter().map(|id| id.0).collect()).unwrap_or_default();
        if let Err(e) = app.run("library.select", json!({"ids": ids, "mode": "replace"})) {
            app.toast_error(ctx, e);
        }
    }
    let hovered = response.hover_pos().and_then(|p| marker_at(&run.view, &run.clusters, pointer_offset(p), HIT_R));
    paint(app, ctx, ui, &t, &mut run, rect, hovered);
    if let Some(i) = hovered
        && let Some(c) = run.clusters.get(i)
    {
        let text = match c.ids.as_slice() {
            [one] => app.session.catalog.photo(*one).map(|p| p.file_name.clone()).unwrap_or_default(),
            many => format!("{} {}", many.len(), crate::i18n::tr("photos on the map")),
        };
        let _ = response.on_hover_text_at_pointer(text);
    }
    strip(app, ctx, ui, &t, &mut run, strip_rect);
    drag(app, ctx, &mut run, rect, &t);
    if let Some(mut editor) = run.editor.take()
        && edit_window(app, ctx, &mut editor)
    {
        run.editor = Some(editor);
    }
    if let Some(style) = style_chosen {
        // Custom URL… uses the address in Settings when there is one, else opens Settings to enter it
        let result = match style {
            "custom" if custom_url.is_empty() => app.run("app.settings", json!({})),
            "custom" => app.run("map.style", json!({"style": "custom", "url": custom_url})),
            _ => app.run("map.style", json!({"style": style})),
        };
        if let Err(e) = result {
            app.toast_error(ctx, e);
        }
    }
    if track_toggled && let Err(e) = app.run("map.showTrack", json!({"enabled": show_track})) {
        app.toast_error(ctx, e);
    }
    if load_track && let Err(e) = app.run("map.loadTrack", json!({})) {
        app.toast_error(ctx, e);
    }
    if !close {
        app.ui.classic_map = Some(run);
    }
}

/// The photos under the map: thumbnails to drag onto it. One that already has a position wears a pin.
fn strip(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, t: &Tokens, run: &mut Run, rect: Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.canvas);
    let tr = crate::i18n::tr;
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), STRIP_HEAD));
    let mut head_ui = ui.new_child(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let r = head_ui.checkbox(&mut run.only_untagged, tr("Only photos without a position"));
    register(ctx, "check:mapOnlyUntagged", r.rect);
    if r.changed() {
        run.points_at = None;
    }
    head_ui.label(egui::RichText::new(tr("Drag photos onto the map to give them a GPS position.")).size(11.0).color(t.text_dim));
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + STRIP_HEAD), rect.max);
    let ids = std::mem::take(&mut run.strip);
    if ids.is_empty() {
        painter.text(body.center(), Align2::CENTER_CENTER, tr("No photos"), t.font(12.5), t.text_dim);
        run.strip = ids;
        return;
    }
    let size = (256.0 * ctx.pixels_per_point().min(2.0) / 2.0) as usize * 2;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    child.style_mut().always_scroll_the_only_direction = true;
    egui::ScrollArea::horizontal().id_salt("map-film").auto_shrink([false, false]).show_viewport(&mut child, |ui, vp| {
        let (area, _) = ui.allocate_exact_size(vec2(ids.len() as f32 * STRIP_CELL, (body.height() - 14.0).max(0.0)), Sense::hover());
        for (i, id) in ids.iter().enumerate() {
            let left = i as f32 * STRIP_CELL;
            if left + STRIP_CELL < vp.left() - STRIP_CELL || left > vp.right() + STRIP_CELL {
                continue;
            }
            let cell = Rect::from_min_size(pos2(area.left() + left, area.top()), vec2(STRIP_CELL, area.height()));
            let resp = ui.interact(cell, egui::Id::new(("map-film", id.0)), Sense::click_and_drag());
            register(ui.ctx(), format!("mapfilm:{}", id.0), cell);
            let selected = app.session.selection.state_of(*id) != lightcraft_engine::SelectionState::NotSelected;
            let base = if selected {
                t.cell_selected
            } else if resp.hovered() {
                t.cell_selected.gamma_multiply(0.6)
            } else {
                t.canvas
            };
            ui.painter().rect_filled(cell, 0.0, base);
            crate::panels::grid::request_thumb(app, *id, size, 8);
            let room = cell.shrink(6.0);
            if let Some(tex) = app.renderer.thumb(*id) {
                let [tw, th] = tex.size;
                let fit = (room.width() / tw.max(1) as f32).min(room.height() / th.max(1) as f32);
                let image = Rect::from_center_size(room.center(), vec2(tw as f32 * fit, th as f32 * fit));
                ui.painter().image(tex.tex.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            if app.session.catalog.photo(*id).is_some_and(|p| p.meta.gps.is_some()) {
                let pin = pos2(cell.right() - 10.0, cell.top() + 10.0);
                ui.painter().circle_filled(pin, 5.0, t.accent);
                ui.painter().circle_stroke(pin, 5.0, Stroke::new(1.5, Color32::WHITE));
            }
            if resp.drag_started() {
                run.drag = Some(drag_ids(app, *id));
            }
            if resp.clicked() {
                let m = ui.input(|i| i.modifiers);
                let mode = if m.command {
                    "toggle"
                } else if m.shift {
                    "range"
                } else {
                    "replace"
                };
                if let Err(e) = app.run("library.select", json!({"ids": [id.0], "mode": mode})) {
                    app.toast_error(ui.ctx(), e);
                }
            }
        }
    });
    run.strip = ids;
}

/// The photos a drag from the strip carries: the whole selection when the photo is part of it, else the photo alone.
pub fn drag_ids(app: &LightcraftApp, id: PhotoId) -> Vec<PhotoId> {
    let selected = app.session.selected();
    if selected.contains(&id) { selected } else { vec![id] }
}

/// Give `ids` the position under `pos` on the map shown in `rect` (`photo.setLocation`: one undo step). `None` when
/// `pos` is not on the map.
pub fn drop_on_map(app: &mut LightcraftApp, view: &View, rect: Rect, ids: &[PhotoId], pos: Pos2) -> Option<Result<Value, String>> {
    if ids.is_empty() || !rect.contains(pos) {
        return None;
    }
    let (lat, lon) = view.locate(f64::from(pos.x - rect.center().x), f64::from(pos.y - rect.center().y));
    let ids: Vec<u64> = ids.iter().map(|id| id.0).collect();
    Some(app.run("photo.setLocation", json!({"ids": ids, "lat": lat, "lon": lon})))
}

/// While photos are dragged from the strip a badge with their number follows the pointer and, over the map, a ring
/// with the position they would get; let go over the map and they are placed there.
fn drag(app: &mut LightcraftApp, ctx: &egui::Context, run: &mut Run, map_rect: Rect, t: &Tokens) {
    let Some(ids) = run.drag.clone() else { return };
    let (down, pointer) = ctx.input(|i| (i.pointer.any_down(), i.pointer.latest_pos()));
    if down {
        ctx.request_repaint();
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(p) = pointer {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("map-drag")));
            if map_rect.contains(p) {
                let (lat, lon) = run.view.locate(f64::from(p.x - map_rect.center().x), f64::from(p.y - map_rect.center().y));
                painter.circle_stroke(p, 14.0, Stroke::new(2.0, t.accent));
                painter.circle_filled(p, 3.0, t.accent);
                let place = painter.layout_no_wrap(format!("{lat:.5}, {lon:.5}"), t.font(11.5), Color32::WHITE);
                let pill = Rect::from_min_size(p + vec2(20.0, -30.0), place.size() + vec2(12.0, 6.0));
                painter.rect_filled(pill, 4.0, Color32::from_black_alpha(190));
                painter.galley(pill.min + vec2(6.0, 3.0), place, Color32::WHITE);
            }
            let count = painter.layout_no_wrap(ids.len().to_string(), t.semibold(11.0), Color32::WHITE);
            let badge = Rect::from_center_size(p + vec2(12.0, 14.0), vec2(count.size().x + 14.0, 18.0));
            painter.rect_filled(badge, 9.0, t.accent);
            painter.galley(badge.center() - count.size() / 2.0, count, Color32::WHITE);
        }
        return;
    }
    run.drag = None;
    let Some(p) = pointer else { return };
    match drop_on_map(app, &run.view, map_rect, &ids, p) {
        Some(Ok(done)) => {
            let n = done["changed"].as_u64().unwrap_or(0);
            if n > 0 {
                let what = crate::i18n::tr(if n == 1 { "photo placed on the map" } else { "photos placed on the map" });
                app.toast(ctx, format!("{n} {what}"));
            }
        }
        Some(Err(e)) => app.toast_error(ctx, e),
        None => {}
    }
}

/// Show every photo that has a position (the selected ones, when any of them has).
fn fit_to_photos(app: &LightcraftApp, run: &mut Run, rect: Rect) {
    let selected: HashSet<PhotoId> = app.session.selected().into_iter().collect();
    let chosen: Vec<(f64, f64)> = run.points.iter().filter(|(id, _, _)| selected.contains(id)).map(|(_, lat, lon)| (*lat, *lon)).collect();
    let all: Vec<(f64, f64)> = run.points.iter().map(|(_, lat, lon)| (*lat, *lon)).collect();
    let positions = if chosen.is_empty() { all } else { chosen };
    run.view = View::fit(&positions, f64::from(rect.width()), f64::from(rect.height()), 60.0);
}

/// Everything inside the map rectangle: the grid, the tiles, the saved locations, the markers and the notices.
fn paint(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, t: &Tokens, run: &mut Run, rect: Rect, hovered: Option<usize>) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.grid_bg);
    paint_grid(&painter, t, &run.view, rect);
    let centre = rect.center();
    let place = |dx: f64, dy: f64| pos2(centre.x + dx as f32, centre.y + dy as f32);

    // tiles: only with the online map on; the grid shows through while they load
    let placements = map::layout(&run.view, f64::from(rect.width()), f64::from(rect.height()), MAX_TILES);
    let keys: Vec<TileKey> = placements.iter().map(|p| p.key).collect();
    let settings = app.ui.settings.clone();
    let online = settings.online_map && !cfg!(target_arch = "wasm32");
    let source = template(&settings);
    if run.textures_for != source || !online {
        run.textures.0.clear();
        run.textures_for = source.clone();
    }
    let states = request_tiles(run, &app.session.activity, &settings, &keys);
    let (mut loading, mut failed) = (false, false);
    for (placement, state) in placements.iter().zip(&states) {
        match state {
            TileState::Ready(image) => {
                let id = match run.textures.0.get(&placement.key) {
                    Some(texture) => texture.id(),
                    None => {
                        let k = placement.key;
                        let color = egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba);
                        let texture = ctx.load_texture(format!("map-tile-{}-{}-{}", k.z, k.x, k.y), color, egui::TextureOptions::LINEAR);
                        let id = texture.id();
                        run.textures.0.insert(k, texture);
                        id
                    }
                };
                let tile = Rect::from_min_size(place(placement.dx, placement.dy), vec2(placement.size as f32, placement.size as f32));
                painter.image(id, tile, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            TileState::Loading => loading = true,
            TileState::Failed => failed = true,
            TileState::Offline => {}
        }
    }
    if run.textures.0.len() > MAX_TEXTURES {
        let shown: HashSet<TileKey> = keys.iter().copied().collect();
        run.textures.0.retain(|k, _| shown.contains(k));
    }
    if loading {
        ctx.request_repaint_after(Duration::from_millis(120));
    }

    // the GPX track, when it is shown
    if app.session.map_show_track
        && let Some(track) = app.session.map_track.as_ref()
    {
        let stroke = Stroke::new(3.0, t.accent.gamma_multiply(0.85));
        for line in map::track_lines(&run.view, &track.segments, map::MAX_TRACK_POINTS) {
            let points: Vec<Pos2> = line.iter().map(|(dx, dy)| place(*dx, *dy)).collect();
            painter.add(egui::Shape::line(points, stroke));
        }
    }

    // saved locations: circles of their radius; a centre being dragged is drawn where the pointer has taken it
    for (id, location) in app.session.catalog.saved_locations() {
        let (lat, lon) = match run.moving {
            Some((moving, lat, lon)) if moving == id.0 => (lat, lon),
            _ => (location.lat, location.lon),
        };
        let (dx, dy) = run.view.offset(lat, lon);
        let radius = (location.radius_m / map::metres_per_pixel(lat, run.view.zoom)).max(4.0) as f32;
        let centre = place(dx, dy);
        if !rect.expand(radius).contains(centre) {
            continue;
        }
        let colour = if location.private { t.reject } else { t.accent };
        painter.circle_filled(centre, radius, colour.gamma_multiply(0.18));
        painter.circle_stroke(centre, radius, Stroke::new(1.5, colour));
        painter.text(centre + vec2(0.0, -radius - 3.0), Align2::CENTER_BOTTOM, &location.name, t.font(11.0), t.text);
    }

    // markers
    let selected: HashSet<PhotoId> = app.session.selected().into_iter().collect();
    for (i, cluster) in run.clusters.iter().enumerate() {
        let (dx, dy) = run.view.offset(cluster.lat, cluster.lon);
        let at = place(dx, dy);
        if !rect.expand(MARKER_R).contains(at) {
            continue;
        }
        let chosen = cluster.ids.iter().any(|id| selected.contains(id));
        let radius = if hovered == Some(i) { MARKER_R + 2.0 } else { MARKER_R };
        painter.circle_filled(at + vec2(0.0, 1.5), radius, Color32::from_black_alpha(70));
        painter.circle_filled(at, radius, if chosen { Color32::WHITE } else { t.accent });
        painter.circle_stroke(at, radius, Stroke::new(2.0, if chosen { t.accent } else { Color32::WHITE }));
        if cluster.ids.len() > 1 {
            let colour = if chosen { Color32::BLACK } else { Color32::WHITE };
            painter.text(at, Align2::CENTER_CENTER, cluster.ids.len().to_string(), t.semibold(10.5), colour);
        }
    }

    if run.drag.is_some() {
        painter.rect_stroke(rect, 0.0, Stroke::new(3.0, t.accent), StrokeKind::Inside);
    }

    // notices
    let tr = crate::i18n::tr;
    let notice = if !online {
        Some(tr("Offline map: a plain latitude and longitude grid. Turn on Online map in Settings to show map tiles."))
    } else if map::check_template(&source).is_err() {
        Some(tr("The tile address in Settings is not usable. Use https:// with {z}, {x} and {y}."))
    } else if failed {
        Some(tr("Some map tiles could not be loaded; they are asked for again in a minute."))
    } else if run.points.is_empty() {
        Some(tr("No shown photo has a GPS position."))
    } else {
        None
    };
    if let Some(text) = notice {
        let galley = painter.layout(text.to_string(), t.font(11.5), t.text, (rect.width() - 150.0).max(80.0));
        let size = galley.size();
        let pad = vec2(8.0, 5.0);
        let pill = Rect::from_min_size(pos2(rect.left() + 8.0, rect.bottom() - size.y - pad.y * 2.0 - 8.0), size + pad * 2.0);
        painter.rect_filled(pill, 4.0, t.chrome.gamma_multiply(0.9));
        painter.galley(pill.min + pad, galley, t.text);
        if !online {
            let r = ui.put(Rect::from_min_size(pos2(pill.right() + 6.0, pill.top()), vec2(90.0, pill.height())), egui::Button::new(tr("Settings…")));
            register(ctx, "button:mapSettings", r.rect);
            if r.clicked()
                && let Err(e) = app.run("app.settings", json!({}))
            {
                app.toast_error(ctx, e);
            }
        }
    }
    if online {
        paint_attribution(app, ctx, ui, t, &painter, rect, &source);
    }
}

/// The map's credit, bottom right, linking to the tile provider's copyright page (OpenStreetMap's usage policy
/// requires it wherever its tiles are shown).
fn paint_attribution(app: &mut LightcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, t: &Tokens, painter: &egui::Painter, rect: Rect, source: &str) {
    let text = map::attribution(source);
    let galley = painter.layout_no_wrap(text, t.font(10.5), Color32::from_gray(0x20));
    let pad = vec2(6.0, 3.0);
    let pill = Rect::from_min_size(rect.right_bottom() - galley.size() - pad * 2.0 - vec2(4.0, 4.0), galley.size() + pad * 2.0);
    painter.rect_filled(pill, 3.0, Color32::from_white_alpha(215));
    painter.galley(pill.min + pad, galley, Color32::from_gray(0x20));
    let r = ui.interact(pill, egui::Id::new("map-attribution"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    register(ctx, "link:mapAttribution", pill);
    if r.clicked()
        && let Err(e) = crate::links::open(app, map::COPYRIGHT_URL)
    {
        app.toast_error(ctx, e);
    }
}

/// Meridians and parallels, the poles' darkened edge and degree labels: the whole map while it is offline, the
/// ground under the tiles while they load.
fn paint_grid(painter: &egui::Painter, t: &Tokens, view: &View, rect: Rect) {
    let (w, h) = (f64::from(rect.width()), f64::from(rect.height()));
    let step = grid_step(view.zoom);
    let size = map::world_size(view.zoom);
    let (cx, cy) = map::project(view.lat, view.lon, view.zoom);
    let centre = rect.center();
    let line = Stroke::new(1.0, t.divider);
    let strong = Stroke::new(1.5, t.text_dim.gamma_multiply(0.7));
    // beyond the northern and southern limits of the projection
    let (north, south) = (centre.y + (-cy) as f32, centre.y + (size - cy) as f32);
    let shade = Color32::from_black_alpha(60);
    if north > rect.top() {
        painter.rect_filled(Rect::from_min_max(rect.min, pos2(rect.right(), north.min(rect.bottom()))), 0.0, shade);
    }
    if south < rect.bottom() {
        painter.rect_filled(Rect::from_min_max(pos2(rect.left(), south.max(rect.top())), rect.max), 0.0, shade);
    }
    // meridians (the world repeats sideways, so longitudes are not wrapped)
    let half_deg = w / 2.0 / (size / 360.0);
    let first = ((view.lon - half_deg) / step).floor() as i64;
    let last = (((view.lon + half_deg) / step).ceil() as i64).min(first + 600);
    for k in first..=last {
        let lon = k as f64 * step;
        let x = centre.x + ((lon + 180.0) / 360.0 * size - cx) as f32;
        let on_prime = (lon / 360.0).fract() == 0.0;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], if on_prime { strong } else { line });
        painter.text(pos2(x + 3.0, rect.top() + 2.0), Align2::LEFT_TOP, degrees_label(map::normalize_lon(lon), step), t.font(10.0), t.text_dim);
    }
    // parallels
    let (top, bottom) = (view.locate(0.0, -h / 2.0).0, view.locate(0.0, h / 2.0).0);
    let (lo, hi) = (top.min(bottom), top.max(bottom));
    let first = (lo / step).floor() as i64;
    let last = ((hi / step).ceil() as i64).min(first + 600);
    for k in first..=last {
        let lat = k as f64 * step;
        if lat.abs() > map::MAX_LAT {
            continue;
        }
        let y = centre.y + (map::project(lat, 0.0, view.zoom).1 - cy) as f32;
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], if k == 0 { strong } else { line });
        painter.text(pos2(rect.left() + 3.0, y - 2.0), Align2::LEFT_BOTTOM, degrees_label(lat, step), t.font(10.0), t.text_dim);
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, t.divider), StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Instant;

    use lightcraft_engine::map_tiles::{FetchError, Fetcher};

    use super::*;
    use crate::Services;

    fn app() -> LightcraftApp {
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default())
    }

    /// Counts requests and answers each with a small PNG.
    struct Counting {
        urls: std::sync::Mutex<Vec<String>>,
        count: AtomicUsize,
    }

    impl Fetcher for Counting {
        fn get(&self, url: &str, _: &AtomicBool) -> Result<Vec<u8>, FetchError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut urls) = self.urls.lock() {
                urls.push(url.to_string());
            }
            let data: Vec<u8> = (0..64 * 64).flat_map(|_| [10u8, 120, 200, 255]).collect();
            let image = lightcraft_codecs::EncodeImage { width: 64, height: 64, channels: 4, samples: lightcraft_codecs::Samples::U8(&data) };
            lightcraft_codecs::encode_png(&image, &lightcraft_codecs::EncodeMeta::default()).map_err(|e| FetchError::Failed(e.to_string()))
        }
    }

    fn counting() -> Arc<Counting> {
        Arc::new(Counting { urls: std::sync::Mutex::new(Vec::new()), count: AtomicUsize::new(0) })
    }

    fn keys() -> Vec<TileKey> {
        (0..4).map(|x| TileKey { z: 5, x, y: 10 }).collect()
    }

    fn with_loader(fake: &Arc<Counting>) -> Run {
        let loader = TileLoader::new(fake.clone(), None, lightcraft_engine::map_tiles::CACHE_CAP, Default::default());
        Run { loader: Some(Loader(loader)), ..Run::default() }
    }

    #[test]
    fn the_online_map_is_off_by_default_and_an_old_settings_file_reads_as_off() {
        assert!(!AppSettings::default().online_map);
        assert!(AppSettings::default().map_tile_url.is_empty());
        let old: AppSettings = serde_json::from_str("{\"gpu\": true}").unwrap();
        assert!(!old.online_map, "a ui.json from before the Map has no such key");
        assert_eq!(template(&old), map::DEFAULT_TILE_URL);
    }

    #[test]
    fn no_request_is_made_and_no_loader_is_made_while_the_online_map_is_off() {
        let fake = counting();
        let mut run = with_loader(&fake);
        let mut settings = AppSettings::default();
        let activity = lightcraft_engine::activity::Activity::default();
        for _ in 0..3 {
            let states = request_tiles(&mut run, &activity, &settings, &keys());
            assert!(states.iter().all(|s| matches!(s, TileState::Offline)));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.count.load(Ordering::SeqCst), 0, "online is off: nothing is fetched");
        let mut fresh = Run::default();
        request_tiles(&mut fresh, &activity, &settings, &keys());
        assert!(fresh.loader.is_none(), "not even a loader exists until the user turns the online map on");
        // turned on: the tiles are asked for, at the default address
        settings.online_map = true;
        let start = Instant::now();
        while request_tiles(&mut run, &activity, &settings, &keys()).iter().any(|s| !matches!(s, TileState::Ready(_))) {
            assert!(start.elapsed() < Duration::from_secs(10), "tiles never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fake.count.load(Ordering::SeqCst), 4);
        assert!(fake.urls.lock().unwrap().contains(&"https://tile.openstreetmap.org/5/2/10.png".to_string()));
        // and off again: the map stops asking at once
        settings.online_map = false;
        assert!(request_tiles(&mut run, &activity, &settings, &keys()).iter().all(|s| matches!(s, TileState::Offline)));
        assert_eq!(fake.count.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_custom_tile_address_is_used_and_a_bad_one_is_refused() {
        let mut a = app();
        assert!(set_tile_url(&mut a, &json!({"url": "https://tiles.example/{z}/{x}/{y}.png"})).is_ok());
        assert_eq!(template(&a.ui.settings), "https://tiles.example/{z}/{x}/{y}.png");
        assert!(set_tile_url(&mut a, &json!({"url": "https://tiles.example/{z}/{x}.png"})).is_err());
        assert_eq!(template(&a.ui.settings), "https://tiles.example/{z}/{x}/{y}.png", "a refused address changes nothing");
        set_tile_url(&mut a, &json!({})).unwrap();
        assert_eq!(template(&a.ui.settings), map::DEFAULT_TILE_URL, "empty restores OpenStreetMap's");
        let fake = counting();
        let mut run = with_loader(&fake);
        let settings = AppSettings { online_map: true, map_tile_url: "https://tiles.example/{z}/{x}/{y}.png".into(), ..Default::default() };
        let activity = lightcraft_engine::activity::Activity::default();
        let start = Instant::now();
        while fake.count.load(Ordering::SeqCst) == 0 {
            request_tiles(&mut run, &activity, &settings, &keys()[..1]);
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fake.urls.lock().unwrap()[0], "https://tiles.example/5/0/10.png");
    }

    #[test]
    fn the_style_menu_sets_the_grid_openstreetmap_or_a_custom_address() {
        let mut a = app();
        assert_eq!(style_name(&a.ui.settings), "grid", "the offline grid is the default");
        assert_eq!(a.run("map.style", json!({"style": "osm"})).unwrap()["online"], true);
        assert_eq!(style_name(&a.ui.settings), "osm");
        assert_eq!(template(&a.ui.settings), map::DEFAULT_TILE_URL);
        let custom = "https://tiles.example.org/{z}/{x}/{y}.png";
        assert_eq!(a.run("map.style", json!({"style": "custom", "url": custom})).unwrap()["tileUrl"], custom);
        assert_eq!(style_name(&a.ui.settings), "custom");
        assert!(a.run("map.style", json!({"style": "custom"})).is_err(), "custom needs an address");
        assert!(a.run("map.style", json!({"style": "satellite"})).is_err(), "no other preset is offered");
        assert_eq!(a.run("map.style", json!({"style": "grid"})).unwrap()["online"], false);
        assert!(!a.ui.settings.online_map, "the grid asks for no tiles");
    }

    #[test]
    fn a_saved_location_is_found_under_the_pointer_by_its_centre_only() {
        let mut a = app();
        let id = a.run("location.add", json!({"name": "Home", "lat": 48.0, "lon": 2.0, "radiusM": 500.0})).unwrap()["id"].as_u64().unwrap();
        let view = View::new(48.0, 2.0, 12.0);
        let centre = pos2(400.0, 300.0);
        assert_eq!(centre_at(&a, &view, centre, pos2(405.0, 302.0)), Some((id, 48.0, 2.0)));
        assert_eq!(centre_at(&a, &view, centre, pos2(460.0, 300.0)), None, "a point well off the centre is not the location");
    }


        let mut a = app();
        assert_eq!(a.run("map.online", json!({"enabled": true})).unwrap()["online"], true);
        assert!(a.ui.settings.online_map);
        assert_eq!(a.run("map.online", json!({})).unwrap()["online"], false, "no `enabled` toggles");
        assert!(!a.ui.settings.online_map);
        assert!(a.run("map.tileUrl", json!({"url": "ftp://x/{z}/{x}/{y}"})).is_err());
    }

    #[test]
    fn opening_the_map_shows_the_view_and_escape_closes_it() {
        let mut a = app();
        let r = a.run("map.view", json!({})).unwrap();
        assert_eq!(r["online"], false, "the map opens offline");
        assert!(a.ui.classic_map.is_some());
        a.run("view.back", json!({})).unwrap();
        assert!(a.ui.classic_map.is_none());
    }

    #[test]
    fn the_grid_is_as_fine_as_the_zoom_allows_and_labels_have_the_decimals_it_needs() {
        assert_eq!(grid_step(1.0), 90.0);
        assert_eq!(grid_step(4.0), 10.0);
        assert!(grid_step(12.0) <= 0.05 && grid_step(19.0) <= 0.001);
        for z in [1.0, 2.5, 7.0, 13.3, 19.0] {
            assert!(grid_step(z) * map::world_size(z) / 360.0 >= GRID_MIN_PX || grid_step(z) == 90.0, "lines at zoom {z} are too close");
        }
        assert_eq!(degrees_label(48.0, 1.0), "48°");
        assert_eq!(degrees_label(-0.5, 0.5), "-0.5°");
        assert_eq!(degrees_label(2.35, 0.05), "2.35°");
        assert_eq!(degrees_label(2.352, 0.001), "2.352°");
        assert_eq!(degrees_label(10.0, 10.0), "10°");
    }

    #[test]
    fn a_click_hits_the_nearest_marker_within_reach_and_nothing_further() {
        let view = View::new(48.0, 2.0, 8.0);
        let clusters =
            vec![Cluster { lat: 48.0, lon: 2.0, ids: vec![PhotoId(1)] }, Cluster { lat: 48.0, lon: 2.05, ids: vec![PhotoId(2), PhotoId(3)] }];
        let (dx, dy) = view.offset(48.0, 2.05);
        assert_eq!(marker_at(&view, &clusters, (0.0, 0.0), HIT_R), Some(0));
        assert_eq!(marker_at(&view, &clusters, (dx + 3.0, dy - 2.0), HIT_R), Some(1));
        assert_eq!(marker_at(&view, &clusters, (dx / 2.0, dy / 2.0 + 60.0), HIT_R), None);
        assert_eq!(marker_at(&view, &[], (0.0, 0.0), HIT_R), None);
    }

    #[test]
    fn a_photo_dropped_on_the_map_gets_the_position_under_the_pointer_in_one_undo_step() {
        let mut a = app();
        let ids = a.session.visible_cloned();
        let view = View::new(10.0, 20.0, 8.0);
        let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let pos = pos2(100.0 + 400.0 + 120.0, 50.0 + 300.0 - 40.0);
        let (lat, lon) = view.locate(120.0, -40.0);
        let undo_before = a.session.undo.len();
        let r = drop_on_map(&mut a, &view, rect, &[ids[0], ids[1]], pos).expect("on the map").unwrap();
        assert_eq!(r["changed"], 2);
        for id in [ids[0], ids[1]] {
            let (la, lo) = a.session.catalog.photo(id).unwrap().meta.gps.expect("has a position");
            assert!((la - lat).abs() < 1e-6 && (lo - lon).abs() < 1e-6, "{la},{lo} is not {lat},{lon}");
        }
        assert_eq!(a.session.undo.len(), undo_before + 1, "both photos are one history entry");
        a.session.undo_step().unwrap();
        assert!(ids[..2].iter().all(|id| a.session.catalog.photo(*id).unwrap().meta.gps.is_none()), "one undo takes both back");
        // the centre of the map is the centre of the view
        drop_on_map(&mut a, &view, rect, &[ids[2]], rect.center()).expect("on the map").unwrap();
        let (la, lo) = a.session.catalog.photo(ids[2]).unwrap().meta.gps.unwrap();
        assert!((la - 10.0).abs() < 1e-6 && (lo - 20.0).abs() < 1e-6);
    }

    #[test]
    fn a_drop_off_the_map_or_of_nothing_changes_nothing() {
        let mut a = app();
        let ids = a.session.visible_cloned();
        let view = View::new(10.0, 20.0, 8.0);
        let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let undo_before = a.session.undo.len();
        assert!(drop_on_map(&mut a, &view, rect, &[ids[0]], pos2(50.0, 300.0)).is_none(), "left of the map is the strip's side, not the map");
        assert!(drop_on_map(&mut a, &view, rect, &[ids[0]], pos2(500.0, 700.0)).is_none(), "below the map");
        assert!(drop_on_map(&mut a, &view, rect, &[], rect.center()).is_none(), "nothing dragged");
        assert_eq!(a.session.undo.len(), undo_before);
        assert!(a.session.catalog.photo(ids[0]).unwrap().meta.gps.is_none());
        // a photo that is not in the library is refused with the engine's reason
        let err = drop_on_map(&mut a, &view, rect, &[PhotoId(987_654)], rect.center()).expect("on the map").unwrap_err();
        assert!(err.contains("987654"), "{err}");
    }

    #[test]
    fn dragging_a_selected_photo_carries_the_selection_and_any_other_photo_goes_alone() {
        let mut a = app();
        let ids = a.session.visible_cloned();
        a.run("library.select", json!({"ids": [ids[0].0, ids[1].0]})).unwrap();
        assert_eq!(drag_ids(&a, ids[1]), vec![ids[0], ids[1]]);
        assert_eq!(drag_ids(&a, ids[3]), vec![ids[3]]);
    }

    #[test]
    fn the_strip_can_list_only_the_photos_without_a_position() {
        use lightcraft_catalog::Op;
        let mut a = app();
        let ids = a.session.visible_cloned();
        let mut meta = a.session.catalog.photo(ids[0]).unwrap().meta.clone();
        meta.gps = Some((1.0, 2.0));
        a.session.commit("test", Op::SetMeta { id: ids[0], meta: Box::new(meta) }).unwrap();
        let mut run = Run::default();
        refresh_points(&mut a, &mut run);
        assert_eq!(run.strip, ids, "every shown photo is on the strip");
        run.only_untagged = true;
        run.points_at = None;
        refresh_points(&mut a, &mut run);
        assert_eq!(run.strip.len(), ids.len() - 1);
        assert!(!run.strip.contains(&ids[0]));
    }

    #[test]
    fn the_markers_follow_the_photos_with_a_position_and_the_filter() {
        use lightcraft_catalog::Op;
        let mut a = app();
        let ids = a.session.visible_cloned();
        for (id, gps) in [(ids[0], (48.8566, 2.3522)), (ids[1], (48.8566, 2.3522)), (ids[2], (51.5, -0.12))] {
            let mut meta = a.session.catalog.photo(id).unwrap().meta.clone();
            meta.gps = Some(gps);
            a.session.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        }
        let mut run = Run::default();
        refresh_points(&mut a, &mut run);
        assert_eq!(run.photos(), 3);
        run.view = View::new(50.0, 1.0, 3.0);
        refresh_clusters(&mut run);
        assert_eq!(run.clusters.iter().map(|c| c.ids.len()).sum::<usize>(), 3);
        run.view = View::new(50.0, 1.0, 12.0);
        refresh_clusters(&mut run);
        assert_eq!(run.clusters.len(), 2, "Paris's two photos share a marker, London's is its own");
        // an edit shows without reopening the map
        a.session.undo_step().unwrap();
        run.points_at = None;
        refresh_points(&mut a, &mut run);
        assert_eq!(run.photos(), 2);
    }
}
