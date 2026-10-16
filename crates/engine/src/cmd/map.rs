//! The Map module's commands (#83, #114): `map.markers`, the photos with a GPS position grouped into the markers
//! the map draws at a zoom. The view itself (tiles, pan, zoom) is the frontend's; the geometry is [`crate::map`].
//!
//! Sources: own design.

use std::collections::HashSet;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, f64_or};
use crate::map::{CLUSTER_CELL_PX, MAX_ZOOM, cluster, is_position};
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

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(query "map.markers", "Map Markers", [], None, "{zoom?: 0 to 19 (3), cellPx?: pixels within which photos share a marker (56), selected?: bool (only the selected photos)} → {photos, markers: [{lat, lon, count, ids}]} — the shown photos that have a GPS position, grouped into the markers the Map draws at that zoom (ids: the first 200)", always, markers)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::Op;

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
