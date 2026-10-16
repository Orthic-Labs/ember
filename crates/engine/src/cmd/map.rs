//! The Map module's commands (#83, #114, #115): `map.markers`, the photos with a GPS position grouped into the markers
//! the map draws at a zoom, and `photo.setLocation`, which a photo dropped on the map uses. The view itself (tiles,
//! pan, zoom) is the frontend's; the geometry is [`crate::map`].
//!
//! Sources: own design.

use std::collections::{BTreeMap, HashSet};

use lightcraft_catalog::{Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, f64_or, f64_req, str_param};
use crate::map::{CLUSTER_CELL_PX, MAX_ZOOM, MapTrack, cluster, is_position};
use crate::{Result, Session};

/// The most photo ids listed for one marker (the count is always complete).
const MAX_IDS: usize = 200;

/// The photos the library shows that have a GPS position on Earth, with it.
pub fn geotagged(s: &mut Session) -> Vec<(PhotoId, f64, f64)> {
    let shown = s.visible_cloned();
    shown
        .into_iter()
        .filter_map(|id| {
            let (lat, lon) = s.catalog.photo(id)?.meta.gps?;
            is_position(lat, lon).then_some((id, lat, lon))
        })
        .collect()
}

/// `map.markers`: `{zoom?: 0 to 19 (3), cellPx?: how close in pixels photos share a marker (56), selected?: bool}`.
fn markers(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.markers";
    let zoom = f64_or(p, "zoom", 3.0);
    let cell = f64_or(p, "cellPx", CLUSTER_CELL_PX);
    if !(zoom.is_finite() && (0.0..=MAX_ZOOM).contains(&zoom)) {
        return Err(bad(C, format!("`zoom` is 0 to {MAX_ZOOM}")));
    }
    if !(cell.is_finite() && cell >= 1.0) {
        return Err(bad(C, "`cellPx` is at least 1"));
    }
    let mut points = geotagged(s);
    if bool_or(p, "selected", false) {
        let selected: HashSet<PhotoId> = s.selected().into_iter().collect();
        points.retain(|(id, _, _)| selected.contains(id));
    }
    let markers: Vec<Value> = cluster(&points, zoom, cell)
        .into_iter()
        .map(|c| {
            let ids: Vec<u64> = c.ids.iter().take(MAX_IDS).map(|i| i.0).collect();
            json!({"lat": c.lat, "lon": c.lon, "count": c.ids.len(), "ids": ids})
        })
        .collect();
    Ok(json!({"photos": points.len(), "markers": markers}))
}

/// `photo.setLocation`: `{lat, lon, ids?}` sets the GPS position of the photos (the selection when no `ids`), or
/// `{clear: true, ids?}` removes it. One undo step for all of them. Place names are left as they are (Fill Location
/// reads them again from the new position).
fn set_location(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.setLocation";
    let position = if bool_or(p, "clear", false) {
        None
    } else {
        let (lat, lon) = (f64_req(p, "lat", C)?, f64_req(p, "lon", C)?);
        if !is_position(lat, lon) {
            return Err(bad(C, format!("{lat}, {lon} is not a GPS position (latitude -90 to 90, longitude -180 to 180)")));
        }
        // seven decimals are a centimetre, as in the track log tagger
        Some(((lat * 1e7).round() / 1e7, (lon * 1e7).round() / 1e7))
    };
    let mut seen = HashSet::new();
    let ids: Vec<PhotoId> = s.targets(p)?.into_iter().filter(|id| seen.insert(*id)).collect();
    if ids.is_empty() {
        return Err(bad(C, "no photos: select some or give `ids`"));
    }
    if let Some(missing) = ids.iter().find(|id| s.catalog.photo(**id).is_none()) {
        return Err(bad(C, format!("no photo {}", missing.0)));
    }
    let mut ops = Vec::new();
    for id in &ids {
        let Some(photo) = s.catalog.photo(*id) else { continue };
        if photo.meta.gps != position {
            let mut meta = photo.meta.clone();
            meta.gps = position;
            ops.push(Op::SetMeta { id: *id, meta: Box::new(meta) });
        }
    }
    let changed = ops.len();
    if changed > 0 {
        s.commit(if position.is_some() { "Set Location" } else { "Remove Location" }, Op::Batch { ops })?;
    }
    Ok(json!({"changed": changed, "photos": ids.len(), "lat": position.map(|g| g.0), "lon": position.map(|g| g.1)}))
}

/// `map.loadTrack {path? | gpx?, clear?: bool}` → `{name, tracks, points, segments}`: a GPX track log to draw on the
/// Map (`map.showTrack`). It is read by the same parser as Auto-Tag from Tracklog; a track with fewer than two timed
/// points is refused. `clear: true` removes the loaded track.
fn load_track(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.loadTrack";
    if bool_or(p, "clear", false) {
        s.map_track = None;
        return Ok(json!({"points": 0}));
    }
    let (text, name) = match (str_param(p, "gpx"), str_param(p, "path")) {
        (Some(text), _) => (text.to_string(), "Track".to_string()),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path).map_err(|e| bad(C, format!("can't read {path}: {e}")))?;
            let name = std::path::Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("Track").to_string();
            (text, name)
        }
        (None, None) => return Err(bad(C, "missing `path` (a .gpx file) or `gpx` (its text)")),
    };
    let log = lightcraft_meta::parse_gpx(&text).map_err(|e| bad(C, e.to_string()))?;
    // one run per segment, in segment order; the points of a run keep their time order
    let mut by_segment: BTreeMap<u32, Vec<(f64, f64)>> = BTreeMap::new();
    for point in &log.points {
        by_segment.entry(point.segment).or_default().push((point.latitude, point.longitude));
    }
    let track = MapTrack { name, segments: by_segment.into_values().collect() };
    if track.points() < 2 {
        return Err(bad(C, "the track log has no timed track points to draw"));
    }
    let reply = json!({"name": track.name, "tracks": log.tracks, "points": track.points(), "segments": track.segments.len()});
    s.map_track = Some(track);
    Ok(reply)
}

/// `map.showTrack {enabled?: bool}`: draws the loaded track on the Map, or hides it (no `enabled`: toggles it).
fn show_track(s: &mut Session, p: &Value) -> Result<Value> {
    s.map_show_track = p.get("enabled").and_then(Value::as_bool).unwrap_or(!s.map_show_track);
    Ok(json!({"show": s.map_show_track, "loaded": s.map_track.is_some()}))
}

/// `map.search {query}` → `{results: [{kind, name, lat, lon, zoom}]}`: an offline search. A typed `lat, lon` comes
/// first; then the saved locations whose name matches; then the City, State and Country names of the photos with a
/// position (each at the centre of those photos). Nothing is looked up online.
fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let query = str_param(p, "query").unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Ok(json!({"results": []}));
    }
    let locations: Vec<(String, f64, f64)> = s.catalog.saved_locations().map(|(_, l)| (l.name.clone(), l.lat, l.lon)).collect();
    // name -> (sum of latitudes, sum of longitudes, photos)
    let mut sums: BTreeMap<String, (f64, f64, usize)> = BTreeMap::new();
    for photo in s.catalog.photos() {
        let Some((lat, lon)) = photo.meta.gps.filter(|(lat, lon)| is_position(*lat, *lon)) else { continue };
        for name in [&photo.meta.city, &photo.meta.state, &photo.meta.country] {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let sum = sums.entry(name.to_string()).or_insert((0.0, 0.0, 0));
            sum.0 += lat;
            sum.1 += lon;
            sum.2 += 1;
        }
    }
    let places: Vec<(String, f64, f64)> = sums.into_iter().map(|(name, (la, lo, n))| (name, la / n as f64, lo / n as f64)).collect();
    let results: Vec<Value> = crate::map::search_places(&query, &locations, &places)
        .into_iter()
        .map(|h| json!({"kind": h.kind, "name": h.name, "lat": h.lat, "lon": h.lon, "zoom": h.zoom}))
        .collect();
    Ok(json!({"results": results}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "map.loadTrack",
            "Load Tracklog",
            [],
            None,
            "{path? | gpx?, clear?: bool} → {name, tracks, points, segments} — loads a GPX track log to draw on the Map (read as Auto-Tag from Tracklog reads it); `clear: true` removes it. Nothing is changed in the library",
            always,
            load_track
        ),
        cmd!("map.showTrack", "Show Track", [], None, "{enabled?: bool} → {show, loaded} — draws the loaded track on the Map, or hides it (no `enabled`: toggles it)", always, show_track),
        cmd!(
            query "map.search",
            "Search Places",
            [],
            None,
            "{query} → {results: [{kind: position|location|place, name, lat, lon, zoom}]} — an offline search: a typed `lat, lon`, the saved locations whose name matches, and the City / State / Country names of the photos with a position (at their centre). Nothing is looked up online",
            always,
            search
        ),
        cmd!(
            "photo.setLocation",
            "Set Location",
            [],
            None,
            "{lat, lon, ids?} or {clear: true, ids?} → {changed, photos, lat, lon} — sets the GPS position of the photos (the selection when no `ids`), or removes it with `clear`; one undo step for all of them. Place names are left as they are. A photo dropped on the Map uses it",
            always,
            set_location
        ),
        cmd!(query "map.markers", "Map Markers", [], None, "{zoom?: 0 to 19 (3), cellPx?: pixels within which photos share a marker (56), selected?: bool (only the selected photos)} → {photos, markers: [{lat, lon, count, ids}]} — the shown photos that have a GPS position, grouped into the markers the Map draws at that zoom (ids: the first 200)", always, markers),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(s: &mut Session, id: PhotoId, gps: Option<(f64, f64)>) {
        let mut meta = s.catalog.photo(id).unwrap().meta.clone();
        meta.gps = gps;
        s.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }

    fn counts(s: &mut Session, p: Value) -> Vec<u64> {
        let r = s.execute("map.markers", &p).unwrap();
        let mut counts: Vec<u64> = r["markers"].as_array().unwrap().iter().map(|m| m["count"].as_u64().unwrap()).collect();
        counts.sort();
        counts
    }

    #[test]
    fn markers_group_the_photos_with_a_position_by_zoom() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        put(&mut s, ids[0], Some((48.8566, 2.3522)));
        put(&mut s, ids[1], Some((48.8567, 2.3523)));
        put(&mut s, ids[2], Some((51.5074, -0.1278)));
        put(&mut s, ids[3], Some((f64::NAN, 0.0)));
        let r = s.execute("map.markers", &json!({"zoom": 3})).unwrap();
        assert_eq!(r["photos"], 3, "a photo without a position on Earth is not on the map");
        assert_eq!(counts(&mut s, json!({"zoom": 3})), vec![3], "one marker on a world map");
        assert_eq!(counts(&mut s, json!({"zoom": 8})), vec![1, 2], "Paris and London apart at zoom 8, Paris's two photos together");
        assert_eq!(counts(&mut s, json!({"zoom": 18})), vec![1, 1, 1], "every photo its own marker close up");
        // only the selected photos
        s.execute("library.select", &json!({"ids": [ids[0].0, ids[2].0]})).unwrap();
        assert_eq!(counts(&mut s, json!({"zoom": 8, "selected": true})), vec![1, 1]);
        let first = s.execute("map.markers", &json!({"zoom": 8, "selected": true})).unwrap();
        assert!(first["markers"][0]["ids"].as_array().unwrap().len() == 1);
    }

    fn gps(s: &Session, id: PhotoId) -> Option<(f64, f64)> {
        s.catalog.photo(id).unwrap().meta.gps
    }

    #[test]
    fn a_location_is_set_on_every_photo_in_one_undo_step() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        put(&mut s, c, Some((1.0, 2.0)));
        let undo_before = s.undo.len();
        let r = s.execute("photo.setLocation", &json!({"ids": [a.0, b.0, a.0], "lat": 48.856_614_999, "lon": 2.352_222_2})).unwrap();
        assert_eq!((r["changed"].as_u64(), r["photos"].as_u64()), (Some(2), Some(2)), "a repeated id counts once");
        assert_eq!(gps(&s, a), Some((48.856_615, 2.352_222_2)), "rounded to seven decimals");
        assert_eq!(gps(&s, a), gps(&s, b));
        assert_eq!(gps(&s, c), Some((1.0, 2.0)), "a photo that was not named is left alone");
        assert_eq!(s.undo.len(), undo_before + 1, "one history entry for both photos");
        // the map sees them at once
        assert_eq!(s.execute("map.markers", &json!({"zoom": 10})).unwrap()["photos"], 3);
        // one undo takes both back, one redo puts both back
        s.undo_step().unwrap();
        assert_eq!((gps(&s, a), gps(&s, b), gps(&s, c)), (None, None, Some((1.0, 2.0))));
        s.redo_step().unwrap();
        assert_eq!(gps(&s, a), Some((48.856_615, 2.352_222_2)));
        assert_eq!(gps(&s, b), gps(&s, a));
    }

    #[test]
    fn the_selection_is_the_default_and_clear_removes_the_position_with_undo() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        s.execute("library.select", &json!({"ids": [ids[3].0]})).unwrap();
        s.execute("photo.setLocation", &json!({"lat": -33.8688, "lon": 151.2093})).unwrap();
        assert_eq!(gps(&s, ids[3]), Some((-33.8688, 151.2093)));
        assert_eq!(gps(&s, ids[0]), None);
        let r = s.execute("photo.setLocation", &json!({"clear": true})).unwrap();
        assert_eq!((r["changed"].as_u64(), r["lat"].is_null()), (Some(1), true));
        assert_eq!(gps(&s, ids[3]), None);
        s.undo_step().unwrap();
        assert_eq!(gps(&s, ids[3]), Some((-33.8688, 151.2093)), "undo brings the position back");
    }

    #[test]
    fn setting_the_position_a_photo_already_has_changes_nothing_and_adds_no_history() {
        let mut s = Session::with_demo();
        let id = s.visible_cloned()[0];
        put(&mut s, id, Some((10.0, 20.0)));
        let undo_before = s.undo.len();
        let r = s.execute("photo.setLocation", &json!({"ids": [id.0], "lat": 10.0, "lon": 20.0})).unwrap();
        assert_eq!(r["changed"], 0);
        assert_eq!(s.undo.len(), undo_before);
    }

    #[test]
    fn a_position_that_is_not_on_earth_or_photos_that_do_not_exist_are_refused_and_nothing_changes() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let undo_before = s.undo.len();
        for bad in [
            json!({"ids": [ids[0].0], "lat": 91.0, "lon": 0.0}),
            json!({"ids": [ids[0].0], "lat": 0.0, "lon": -181.0}),
            json!({"ids": [ids[0].0], "lat": 5.0}),
            json!({"ids": [ids[0].0], "lat": "north", "lon": 5.0}),
            json!({"ids": "x", "lat": 5.0, "lon": 5.0}),
            json!({"ids": [], "lat": 5.0, "lon": 5.0}),
            json!({"ids": [ids[0].0, 987_654], "lat": 5.0, "lon": 5.0}),
        ] {
            assert!(s.execute("photo.setLocation", &bad).is_err(), "{bad} must be refused");
        }
        // nothing selected, nothing named
        s.selection.ids.clear();
        s.selection.active = None;
        assert!(s.execute("photo.setLocation", &json!({"lat": 5.0, "lon": 5.0})).is_err());
        assert_eq!(s.undo.len(), undo_before);
        assert_eq!(gps(&s, ids[0]), None);
    }

    const TRACK: &str = r#"<gpx version="1.1"><trk>
<trkseg>
<trkpt lat="48.85" lon="2.35"><time>2026-10-01T10:00:00Z</time></trkpt>
<trkpt lat="48.86" lon="2.36"><time>2026-10-01T10:01:00Z</time></trkpt>
</trkseg>
<trkseg>
<trkpt lat="51.5" lon="-0.12"><time>2026-10-01T12:00:00Z</time></trkpt>
<trkpt lat="51.51" lon="-0.11"><time>2026-10-01T12:01:00Z</time></trkpt>
</trkseg>
</trk></gpx>"#;

    #[test]
    fn a_track_log_is_loaded_per_segment_shown_and_removed_and_the_library_is_not_changed() {
        let mut s = Session::with_demo();
        let undo_before = s.undo.len();
        let r = s.execute("map.loadTrack", &json!({"gpx": TRACK})).unwrap();
        assert_eq!((r["points"].as_u64(), r["segments"].as_u64(), r["tracks"].as_u64()), (Some(4), Some(2), Some(1)));
        let track = s.map_track.clone().unwrap();
        assert_eq!(track.segments.len(), 2, "one line per segment, never bridged");
        assert_eq!(track.segments[1][0], (51.5, -0.12));
        assert!(!s.map_show_track, "a loaded track is not drawn until asked");
        assert_eq!(s.execute("map.showTrack", &json!({})).unwrap(), json!({"show": true, "loaded": true}), "no enabled: toggles");
        assert_eq!(s.execute("map.showTrack", &json!({"enabled": false})).unwrap()["show"], false);
        assert_eq!(s.undo.len(), undo_before, "loading and showing are not library edits");
        s.execute("map.loadTrack", &json!({"clear": true})).unwrap();
        assert!(s.map_track.is_none());
    }

    #[test]
    fn a_track_that_is_not_gpx_or_has_no_timed_points_is_refused_and_nothing_is_replaced() {
        let mut s = Session::with_demo();
        s.execute("map.loadTrack", &json!({"gpx": TRACK})).unwrap();
        for bad in [
            json!({}),
            json!({"gpx": "<kml/>"}),
            json!({"gpx": "<gpx><trk><trkseg/></trk></gpx>"}),
            json!({"path": "D:/no/such/track.gpx"}),
        ] {
            assert!(s.execute("map.loadTrack", &bad).is_err(), "{bad}");
        }
        assert_eq!(s.map_track.as_ref().map(|t| t.points()), Some(4), "the track loaded before is kept");
    }

    fn place(s: &mut Session, id: PhotoId, gps: Option<(f64, f64)>, city: &str, country: &str) {
        let mut meta = s.catalog.photo(id).unwrap().meta.clone();
        meta.gps = gps;
        meta.city = city.into();
        meta.country = country.into();
        s.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }

    #[test]
    fn a_place_search_finds_saved_locations_photo_places_and_typed_positions_offline() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        place(&mut s, ids[0], Some((48.8, 2.3)), "Paris", "France");
        place(&mut s, ids[1], Some((48.9, 2.4)), "Paris", "France");
        place(&mut s, ids[2], Some((40.0, -3.0)), "Madrid", "Spain");
        s.execute("location.add", &json!({"name": "Paris flat", "lat": 48.85, "lon": 2.35, "radiusM": 500.0})).unwrap();
        let undo_before = s.undo.len();
        let found = |s: &mut Session, q: &str| -> Vec<(String, String)> {
            let r = s.execute("map.search", &json!({"query": q})).unwrap();
            r["results"].as_array().unwrap().iter().map(|h| (h["kind"].as_str().unwrap().to_string(), h["name"].as_str().unwrap().to_string())).collect()
        };
        assert_eq!(
            found(&mut s, "paris"),
            vec![("place".to_string(), "Paris".to_string()), ("location".to_string(), "Paris flat".to_string())],
            "the photo place first (exact), then the saved location"
        );
        let r = s.execute("map.search", &json!({"query": "PARIS"})).unwrap();
        let centre = &r["results"][0];
        assert_eq!((centre["lat"].as_f64(), centre["lon"].as_f64()), (Some(48.85), Some(2.35)), "the centre of the two photos");
        assert_eq!(found(&mut s, "48.85, 2.35")[0].0, "position");
        assert!(found(&mut s, "madrid").iter().all(|(k, _)| k == "place"), "Madrid is a place name too");
        assert!(found(&mut s, "atlantis").is_empty());
        assert!(found(&mut s, "  ").is_empty());
        assert_eq!(s.undo.len(), undo_before, "searching changes nothing");
    }

    #[test]
    fn markers_follow_the_filter_and_refuse_bad_numbers() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        put(&mut s, ids[0], Some((10.0, 10.0)));
        put(&mut s, ids[1], Some((10.0, 10.0)));
        assert_eq!(counts(&mut s, json!({})), vec![2]);
        s.execute("library.filter", &json!({"hasGps": false})).unwrap();
        assert_eq!(counts(&mut s, json!({})), Vec::<u64>::new(), "the photos the filter hides are not on the map");
        for bad in [json!({"zoom": 20}), json!({"zoom": -1}), json!({"zoom": "x"}), json!({"cellPx": 0})] {
            let r = s.execute("map.markers", &bad);
            assert!(r.is_ok() == (bad == json!({"zoom": "x"})), "{bad}: {r:?}");
        }
    }
}
