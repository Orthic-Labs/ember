//! The Map module's geometry (#83, #114): Web Mercator projection (EPSG:3857, 256 px tiles), the view
//! (centre and zoom) with pan and zoom about a point, the tile grid a view needs, photo clustering at
//! low zoom, and the tile address and policy constants. No network and no I/O: the tile loader is
//! [`crate::map_tiles`], the drawing is the UI's.
//!
//! Sources: the Web Mercator formulas and the slippy-map tile scheme (z / x / y, y down from the north)
//! as documented on the OpenStreetMap wiki ("Slippy map tilenames"); own implementation.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use lightcraft_catalog::PhotoId;

/// Side of a tile in world pixels at zoom 0 (a zoom-`z` world is `256 · 2^z` pixels wide).
pub const TILE_PX: f64 = 256.0;
/// Web Mercator reaches the poles only at infinity: the standard cut-off, where the world is square.
pub const MAX_LAT: f64 = 85.051_128_779_806_59;
pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 19.0;
/// The default tile address: OpenStreetMap's standard layer. Its usage policy
/// (<https://operations.osmfoundation.org/policies/tiles/>) is kept by `map_tiles`: a User-Agent that
/// names Ember and where to reach us, a disk cache, no bulk download, two connections at most, a stop
/// when the server refuses us, and the attribution below on the map.
pub const DEFAULT_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// Shown on the map whenever tiles are.
pub const ATTRIBUTION: &str = "© OpenStreetMap contributors";
/// Where the attribution links to.
pub const COPYRIGHT_URL: &str = "https://www.openstreetmap.org/copyright";
/// Ember's contact address in the User-Agent (the tile operators' contact requirement).
pub const CONTACT_URL: &str = "https://github.com/Orthic-Labs/ember";
/// Markers closer than this many screen pixels merge into one cluster.
pub const CLUSTER_CELL_PX: f64 = 56.0;
/// From this zoom on only photos at (nearly) the same spot share a marker.
pub const CLUSTER_OFF_ZOOM: f64 = 16.0;
/// The longest tile address accepted.
const MAX_TEMPLATE: usize = 512;

/// A position that is on Earth: finite, latitude within ±90, longitude within ±180.
pub fn is_position(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0
}

/// A longitude wrapped into [-180, 180).
pub fn normalize_lon(lon: f64) -> f64 {
    if !lon.is_finite() {
        return 0.0;
    }
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

/// The world's width and height in pixels at `zoom`.
pub fn world_size(zoom: f64) -> f64 {
    TILE_PX * 2f64.powf(zoom.clamp(0.0, MAX_ZOOM))
}

/// World pixel position (x east from the antimeridian, y south from the northern limit) of a position at `zoom`.
/// A latitude beyond Web Mercator's limit is held at the limit.
pub fn project(lat: f64, lon: f64, zoom: f64) -> (f64, f64) {
    let size = world_size(zoom);
    let s = lat.clamp(-MAX_LAT, MAX_LAT).to_radians().sin();
    let x = (lon + 180.0) / 360.0 * size;
    let y = (0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * PI)) * size;
    (x, y)
}

/// The inverse of [`project`]: latitude and longitude of a world pixel (y beyond the map is held at its edge).
pub fn unproject(x: f64, y: f64, zoom: f64) -> (f64, f64) {
    let size = world_size(zoom);
    let lon = normalize_lon(x / size * 360.0 - 180.0);
    let n = PI - 2.0 * PI * y.clamp(0.0, size) / size;
    (n.sinh().atan().to_degrees(), lon)
}

/// Metres on the ground per screen pixel at `lat` and `zoom` (for drawing a circle of a given radius).
pub fn metres_per_pixel(lat: f64, zoom: f64) -> f64 {
    const EQUATOR_M: f64 = 40_075_016.685_578_5;
    EQUATOR_M * lat.clamp(-MAX_LAT, MAX_LAT).to_radians().cos() / world_size(zoom)
}

/// One tile of the slippy-map scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// Where the map looks: a centre and a (fractional) zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
}

impl Default for View {
    /// The whole world, nothing chosen yet.
    fn default() -> Self {
        View { lat: 20.0, lon: 0.0, zoom: 2.0 }
    }
}

impl View {
    /// A view; a centre that is not on Earth or a zoom out of range is brought back into range.
    pub fn new(lat: f64, lon: f64, zoom: f64) -> View {
        let lat = if lat.is_finite() { lat.clamp(-MAX_LAT, MAX_LAT) } else { 0.0 };
        let zoom = if zoom.is_finite() { zoom.clamp(MIN_ZOOM, MAX_ZOOM) } else { MIN_ZOOM };
        View { lat, lon: normalize_lon(lon), zoom }
    }

    fn centre_px(&self) -> (f64, f64) {
        project(self.lat, self.lon, self.zoom)
    }

    /// Pixels east and south of the view's centre where `lat` / `lon` is drawn (the way round the globe that is shorter).
    pub fn offset(&self, lat: f64, lon: f64) -> (f64, f64) {
        let (cx, cy) = self.centre_px();
        let (x, y) = project(lat, lon, self.zoom);
        let size = world_size(self.zoom);
        let dx = x - cx;
        (dx - size * (dx / size).round(), y - cy)
    }

    /// The position `dx` pixels east and `dy` south of the view's centre (the inverse of [`View::offset`]).
    pub fn locate(&self, dx: f64, dy: f64) -> (f64, f64) {
        let (cx, cy) = self.centre_px();
        unproject(cx + dx, cy + dy, self.zoom)
    }

    /// Drag the map by `dx`, `dy` pixels: the map follows the pointer, so the centre moves the other way.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        let (lat, lon) = self.locate(-dx, -dy);
        *self = View::new(lat, lon, self.zoom);
    }

    /// Zoom by `delta` levels keeping the position under the point `ox`, `oy` pixels from the centre where it is.
    pub fn zoom_at(&mut self, delta: f64, ox: f64, oy: f64) {
        let (alat, alon) = self.locate(ox, oy);
        let zoom = (self.zoom + if delta.is_finite() { delta } else { 0.0 }).clamp(MIN_ZOOM, MAX_ZOOM);
        let (x, y) = project(alat, alon, zoom);
        let (lat, lon) = unproject(x - ox, y - oy, zoom);
        *self = View::new(lat, lon, zoom);
    }

    /// The view that shows every position in a `width` × `height` pixel map with `margin` pixels around them
    /// (one position: a street-level view of it; none: the world).
    pub fn fit(points: &[(f64, f64)], width: f64, height: f64, margin: f64) -> View {
        let mut valid = points.iter().filter(|(lat, lon)| is_position(*lat, *lon)).map(|(lat, lon)| project(*lat, *lon, 0.0));
        let Some(first) = valid.next() else { return View::default() };
        let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
        for (x, y) in valid {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let (w, h) = ((width - 2.0 * margin).max(1.0), (height - 2.0 * margin).max(1.0));
        let zoom = (w / (x1 - x0).max(1e-9)).log2().min((h / (y1 - y0).max(1e-9)).log2()).min(15.0);
        let (lat, lon) = unproject((x0 + x1) / 2.0, (y0 + y1) / 2.0, 0.0);
        View::new(lat, lon, zoom)
    }
}

/// A tile to draw: its address and where its top-left corner is, in pixels from the view's centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub key: TileKey,
    pub dx: f64,
    pub dy: f64,
    /// Drawn side in pixels (the tile's 256 scaled by the fraction of a zoom level).
    pub size: f64,
}

/// The tiles that cover a `width` × `height` pixel map showing `view`, nearest the centre first and at most
/// `max_tiles` of them (so a huge window never asks for a huge area).
pub fn layout(view: &View, width: f64, height: f64, max_tiles: usize) -> Vec<Placement> {
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    let z = view.zoom.round().clamp(0.0, MAX_ZOOM);
    let scale = 2f64.powf(view.zoom - z);
    let (cx, cy) = project(view.lat, view.lon, z);
    let n = 2f64.powf(z);
    let (half_w, half_h) = (width / 2.0 / scale, height / 2.0 / scale);
    let x_first = ((cx - half_w) / TILE_PX).floor() as i64;
    let x_last = ((cx + half_w) / TILE_PX).floor() as i64;
    let y_first = ((cy - half_h) / TILE_PX).floor().max(0.0) as i64;
    let y_last = ((cy + half_h) / TILE_PX).floor().min(n - 1.0) as i64;
    let mut out = Vec::new();
    for x in x_first..=x_last {
        for y in y_first..=y_last {
            let key = TileKey { z: z as u8, x: x.rem_euclid(n as i64) as u32, y: y as u32 };
            out.push(Placement { key, dx: (x as f64 * TILE_PX - cx) * scale, dy: (y as f64 * TILE_PX - cy) * scale, size: TILE_PX * scale });
        }
    }
    let distance = |p: &Placement| {
        let (mx, my) = (p.dx + p.size / 2.0, p.dy + p.size / 2.0);
        mx * mx + my * my
    };
    out.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
    out.truncate(max_tiles);
    out
}

/// A marker: one or more photos at (about) one place.
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    pub lat: f64,
    pub lon: f64,
    /// Photo ids, ascending.
    pub ids: Vec<PhotoId>,
}

/// Group photo positions that fall in the same `cell_px` square of the map at `zoom` (from [`CLUSTER_OFF_ZOOM`] on
/// the square is two pixels, so only photos at the same spot share a marker). A cluster sits at the mean of its
/// photos; the result is in map order, so it does not flicker from frame to frame. A position that is not on
/// Earth is left out.
pub fn cluster(points: &[(PhotoId, f64, f64)], zoom: f64, cell_px: f64) -> Vec<Cluster> {
    let cell = if zoom >= CLUSTER_OFF_ZOOM { 2.0 } else { cell_px.max(1.0) };
    let mut cells: BTreeMap<(i64, i64), Vec<(PhotoId, f64, f64)>> = BTreeMap::new();
    for (id, lat, lon) in points {
        if !is_position(*lat, *lon) {
            continue;
        }
        let (x, y) = project(*lat, *lon, zoom);
        cells.entry(((x / cell).floor() as i64, (y / cell).floor() as i64)).or_default().push((*id, x, y));
    }
    cells
        .into_values()
        .map(|members| {
            let n = members.len() as f64;
            let (x, y) = members.iter().fold((0.0, 0.0), |(sx, sy), (_, x, y)| (sx + x, sy + y));
            let (lat, lon) = unproject(x / n, y / n, zoom);
            let mut ids: Vec<PhotoId> = members.iter().map(|(id, _, _)| *id).collect();
            ids.sort();
            Cluster { lat, lon, ids }
        })
        .collect()
}

/// Why a tile address cannot be used, or `Ok`: it must be an `http://` or `https://` address with `{z}`, `{x}` and `{y}`
/// once each.
pub fn check_template(template: &str) -> Result<(), String> {
    if template.len() > MAX_TEMPLATE {
        return Err(format!("the tile address is longer than {MAX_TEMPLATE} characters"));
    }
    if template.chars().any(|c| c.is_control() || c == ' ') {
        return Err("the tile address has a space or control character".into());
    }
    if !(template.starts_with("https://") || template.starts_with("http://")) {
        return Err("the tile address must start with https:// (or http://)".into());
    }
    for tag in ["{z}", "{x}", "{y}"] {
        match template.matches(tag).count() {
            1 => {}
            0 => return Err(format!("the tile address needs {tag} (for example {DEFAULT_TILE_URL})")),
            _ => return Err(format!("{tag} must appear once in the tile address")),
        }
    }
    let host = tile_host(template).ok_or("the tile address has no host")?;
    if host.is_empty() || host.contains(['{', '}']) {
        return Err("the tile address has no host before the path".into());
    }
    Ok(())
}

/// The tile address for `key` from a template (`{z}`, `{x}`, `{y}`).
pub fn tile_url(template: &str, key: TileKey) -> String {
    template.replace("{z}", &key.z.to_string()).replace("{x}", &key.x.to_string()).replace("{y}", &key.y.to_string())
}

/// The host a template names (no port, no user info), lower case.
pub fn tile_host(template: &str) -> Option<String> {
    let rest = template.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    Some(host.to_ascii_lowercase())
}

/// The credit shown on the map for a tile source: OpenStreetMap's own text for its servers; for any other
/// address that text (their data is almost always OpenStreetMap's) and the host that served the tiles.
pub fn attribution(template: &str) -> String {
    match tile_host(template) {
        Some(host) if host != "openstreetmap.org" && !host.ends_with(".openstreetmap.org") => format!("{ATTRIBUTION} · tiles: {host}"),
        _ => ATTRIBUTION.to_string(),
    }
}

/// The `User-Agent` of every tile request: the application, its version and where to reach its makers.
pub fn user_agent() -> String {
    format!("Ember/{} (+{CONTACT_URL}; photo library, map view)", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(c: &Cluster) -> Vec<u64> {
        c.ids.iter().map(|i| i.0).collect()
    }

    #[test]
    fn projection_round_trips_across_the_map_and_every_zoom() {
        for lat in [-85.0, -60.0, -0.001, 0.0, 33.3, 48.8566, 85.0] {
            for lon in [-179.9, -90.0, 0.0, 12.3, 179.9] {
                for zoom in [1.0, 5.5, 12.0, 19.0] {
                    let (x, y) = project(lat, lon, zoom);
                    let (la, lo) = unproject(x, y, zoom);
                    assert!((la - lat).abs() < 1e-9 && (lo - lon).abs() < 1e-9, "{lat},{lon} at {zoom} came back as {la},{lo}");
                }
            }
        }
    }

    #[test]
    fn projection_matches_known_points_and_tiles() {
        assert_eq!(project(0.0, 0.0, 0.0), (128.0, 128.0));
        assert_eq!(project(0.0, -180.0, 0.0).0, 0.0);
        assert!(project(MAX_LAT, 0.0, 0.0).1.abs() < 1e-6, "the northern limit is the top edge");
        assert!((project(-MAX_LAT, 0.0, 0.0).1 - 256.0).abs() < 1e-6);
        // Paris at zoom 10 is in tile 518 / 352 (OpenStreetMap's slippy-map scheme)
        let (x, y) = project(48.8566, 2.3522, 10.0);
        assert_eq!(((x / TILE_PX) as u32, (y / TILE_PX) as u32), (518, 352));
        assert!((metres_per_pixel(0.0, 0.0) - 156_543.033_928).abs() < 1e-3);
        assert!((metres_per_pixel(60.0, 3.0) - metres_per_pixel(0.0, 3.0) / 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_latitude_past_the_limit_is_held_and_bad_numbers_never_panic() {
        assert_eq!(project(90.0, 0.0, 3.0), project(MAX_LAT, 0.0, 3.0));
        let (x, y) = project(f64::NAN, f64::INFINITY, 3.0);
        let _ = unproject(x, y, 3.0);
        let (la, lo) = unproject(-1e9, 1e9, 3.0);
        assert!(la.abs() <= 90.0 && lo.abs() <= 180.0);
        assert_eq!(View::new(f64::NAN, f64::NAN, f64::NAN), View { lat: 0.0, lon: 0.0, zoom: MIN_ZOOM });
        assert!(layout(&View::default(), f64::NAN, 100.0, 10).is_empty());
        assert!(layout(&View::default(), 0.0, 100.0, 10).is_empty());
    }

    #[test]
    fn screen_offsets_round_trip_and_wrap_the_short_way_round_the_globe() {
        let v = View::new(48.0, 179.0, 6.0);
        let (dx, dy) = v.offset(48.0, -179.0);
        assert!(dx > 0.0 && dx < 100.0, "-179 is two degrees east of 179, not the world away: {dx}");
        let (la, lo) = v.locate(dx, dy);
        assert!((la - 48.0).abs() < 1e-9 && (lo + 179.0).abs() < 1e-9);
        assert_eq!(v.offset(48.0, 179.0), (0.0, 0.0));
    }

    #[test]
    fn panning_follows_the_pointer_and_zooming_keeps_the_point_under_it() {
        let mut v = View::new(10.0, 20.0, 8.0);
        let before = v.locate(100.0, 40.0);
        v.pan(30.0, -10.0);
        let (dx, dy) = v.offset(before.0, before.1);
        assert!((dx - 130.0).abs() < 1e-6 && (dy - 30.0).abs() < 1e-6, "the map moved with the drag: {dx} {dy}");
        let anchor = v.locate(-80.0, 55.0);
        v.zoom_at(2.0, -80.0, 55.0);
        assert_eq!(v.zoom, 10.0);
        let (ax, ay) = v.offset(anchor.0, anchor.1);
        assert!((ax + 80.0).abs() < 1e-6 && (ay - 55.0).abs() < 1e-6, "{ax} {ay}");
        v.zoom_at(100.0, 0.0, 0.0);
        assert_eq!(v.zoom, MAX_ZOOM);
        v.zoom_at(-100.0, 0.0, 0.0);
        assert_eq!(v.zoom, MIN_ZOOM);
    }

    #[test]
    fn fit_shows_every_position_and_a_single_one_close_up() {
        assert_eq!(View::fit(&[], 800.0, 600.0, 20.0), View::default());
        assert_eq!(View::fit(&[(f64::NAN, 0.0)], 800.0, 600.0, 20.0), View::default());
        let one = View::fit(&[(48.8566, 2.3522)], 800.0, 600.0, 20.0);
        assert_eq!(one.zoom, 15.0);
        let pts = [(48.86, 2.34), (51.5, -0.12), (41.9, 12.5)];
        let v = View::fit(&pts, 800.0, 600.0, 20.0);
        for (lat, lon) in pts {
            let (dx, dy) = v.offset(lat, lon);
            assert!(dx.abs() <= 400.0 && dy.abs() <= 300.0, "{lat},{lon} at {dx},{dy} is off the map");
        }
    }

    #[test]
    fn the_layout_covers_the_map_nearest_tile_first_and_never_asks_for_more_than_the_cap() {
        let v = View::new(48.8566, 2.3522, 10.0);
        let all = layout(&v, 1024.0, 768.0, 1000);
        assert!((12..=30).contains(&all.len()), "{}", all.len());
        assert_eq!(all[0].key, TileKey { z: 10, x: 518, y: 352 }, "the tile under the centre comes first");
        for p in &all {
            assert!(p.dx < 512.0 && p.dx + p.size > -512.0 && p.dy < 384.0 && p.dy + p.size > -384.0, "{p:?} is off the map");
        }
        assert_eq!(layout(&v, 1024.0, 768.0, 5).len(), 5);
        // between two levels the tiles of the nearest level are scaled
        let half = layout(&View::new(48.8566, 2.3522, 10.5), 400.0, 400.0, 100);
        assert!(half.iter().all(|p| p.key.z == 11 && (p.size - 256.0 * 2f64.powf(-0.5)).abs() < 1e-9));
        // tiles wrap round the antimeridian and stop at the poles
        let edge = layout(&View::new(0.0, 179.9, 3.0), 2000.0, 400.0, 100);
        assert!(edge.iter().any(|p| p.key.x == 0) && edge.iter().any(|p| p.key.x == 7));
        let pole = layout(&View::new(MAX_LAT, 0.0, 3.0), 400.0, 2000.0, 100);
        assert!(!pole.is_empty() && pole.iter().all(|p| p.key.y < 8));
    }

    /// Photos placed by their pixel position on a zoom-5 map (cells are 56 px, so the cell of each is known).
    fn photos() -> Vec<(PhotoId, f64, f64)> {
        let at = |id: u64, x: f64, y: f64| {
            let (lat, lon) = unproject(x, y, 5.0);
            (PhotoId(id), lat, lon)
        };
        vec![at(1, 60.0, 60.0), at(2, 100.0, 100.0), at(3, 300.0, 300.0), at(4, 305.0, 305.0), at(5, 300.0, 300.0), (PhotoId(6), 40.0, f64::NAN)]
    }

    fn sizes(zoom: f64) -> Vec<usize> {
        cluster(&photos(), zoom, CLUSTER_CELL_PX).iter().map(|c| c.ids.len()).collect()
    }

    #[test]
    fn photos_cluster_at_low_zoom_and_separate_as_the_map_zooms_in() {
        let world = cluster(&photos(), 1.0, CLUSTER_CELL_PX);
        assert_eq!(world.len(), 1, "the whole library is one marker on a tiny world map");
        assert_eq!(ids(&world[0]), vec![1, 2, 3, 4, 5], "a position that is not on Earth is left out");
        assert_eq!(sizes(5.0), vec![2, 3], "zoom 5: the pair, and the three photos near the other spot");
        assert_eq!(sizes(6.0), vec![1, 1, 3], "zoom 6 pulls the pair apart");
        assert_eq!(sizes(17.0), vec![1, 1, 2, 1], "from zoom 16 only photos at the same spot share a marker");
        assert!(cluster(&[], 5.0, CLUSTER_CELL_PX).is_empty());
        let total = |zoom: f64| sizes(zoom).iter().sum::<usize>();
        assert!([1.0, 3.0, 5.0, 8.0, 17.0].iter().all(|z| total(*z) == 5), "no photo is lost or counted twice");
    }

    #[test]
    fn a_cluster_sits_between_its_photos_and_the_order_is_stable() {
        let a = cluster(&photos(), 5.0, CLUSTER_CELL_PX);
        let mut shuffled = photos();
        shuffled.reverse();
        let b = cluster(&shuffled, 5.0, CLUSTER_CELL_PX);
        assert_eq!(a.iter().map(ids).collect::<Vec<_>>(), b.iter().map(ids).collect::<Vec<_>>(), "the same markers in the same order");
        let (x, y) = project(a[0].lat, a[0].lon, 5.0);
        assert!((x - 80.0).abs() < 1e-6 && (y - 80.0).abs() < 1e-6, "the pair's marker is at its mean: {x},{y}");
    }

    #[test]
    fn tile_addresses_are_checked_and_filled_in() {
        assert!(check_template(DEFAULT_TILE_URL).is_ok());
        assert!(check_template("http://localhost:8080/{z}/{x}/{y}.png").is_ok());
        for bad in [
            "",
            "ftp://x.test/{z}/{x}/{y}.png",
            "https://x.test/{z}/{x}.png",
            "https://x.test/{z}/{x}/{y}/{y}.png",
            "https://x.test/ {z}/{x}/{y}.png",
            "https://{z}/{x}/{y}.png",
            "file:///{z}/{x}/{y}",
        ] {
            assert!(check_template(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(check_template(&format!("https://x.test/{}/{{z}}/{{x}}/{{y}}", "a".repeat(600))).is_err());
        assert_eq!(tile_url(DEFAULT_TILE_URL, TileKey { z: 10, x: 518, y: 352 }), "https://tile.openstreetmap.org/10/518/352.png");
        assert_eq!(tile_host("https://User@Tiles.Example:8443/{z}/{x}/{y}.png").as_deref(), Some("tiles.example"));
    }

    #[test]
    fn the_credit_names_openstreetmap_and_any_other_tile_host() {
        assert_eq!(attribution(DEFAULT_TILE_URL), "© OpenStreetMap contributors");
        assert_eq!(attribution("https://tiles.example/{z}/{x}/{y}.png"), "© OpenStreetMap contributors · tiles: tiles.example");
        let ua = user_agent();
        assert!(ua.starts_with("Ember/") && ua.contains(CONTACT_URL), "a User-Agent that names the app and a contact: {ua}");
    }
}
