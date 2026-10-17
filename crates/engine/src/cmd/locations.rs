//! Saved locations (issue #116): named circles on the map (a centre and a radius in metres), kept in the
//! catalog. Each command that changes one is one undo step. A private location strips the GPS and place
//! names of the photos inside it on export (`export::removes_location`).
//!
//! Sources: own design.

use lightcraft_catalog::locations::SavedLocation;
use lightcraft_catalog::{LocationId, Op};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, f64_req, str_param};
use crate::{Result, Session};

/// The saved location a command names (`id`, from `location.list`).
fn location_id(p: &Value, command: &str) -> Result<LocationId> {
    p.get("id").and_then(Value::as_u64).map(LocationId).ok_or_else(|| bad(command, "missing `id` (a saved location's id, from location.list)"))
}

/// The trimmed `name` a command gives; an empty name is refused.
fn name_param(p: &Value, command: &str) -> Result<String> {
    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(|| bad(command, "missing `name`"))
}

/// The record `id` names, or an error that says which id it was.
fn existing(s: &Session, id: LocationId, command: &str) -> Result<SavedLocation> {
    s.catalog.saved_location(id).cloned().ok_or_else(|| bad(command, format!("no saved location {}", id.0)))
}

/// Save `location` under `id` as one undo step. A location that makes no sense is refused with the reason.
fn save(s: &mut Session, command: &str, label: &str, id: LocationId, location: SavedLocation) -> Result<()> {
    location.check().map_err(|e| bad(command, e))?;
    s.commit(label, Op::SetSavedLocation { id, location: Some(location) })
}

/// `location.list`: every saved location by name, with the library photos inside each.
fn list(s: &mut Session, _p: &Value) -> Result<Value> {
    let mut all: Vec<(LocationId, SavedLocation)> = s.catalog.saved_locations().map(|(id, l)| (id, l.clone())).collect();
    all.sort_by_key(|(_, l)| l.name.to_lowercase());
    let rows: Vec<Value> = all
        .iter()
        .map(|(id, l)| {
            json!({
                "id": id.0, "name": l.name, "lat": l.lat, "lon": l.lon, "radiusM": l.radius_m, "private": l.private,
                "photos": s.catalog.saved_location_count(*id),
            })
        })
        .collect();
    Ok(json!({"locations": rows}))
}

/// `location.add`: `{name, lat, lon, radiusM, private?}` → `{id}`.
fn add(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "location.add";
    let location = SavedLocation {
        name: name_param(p, C)?,
        lat: f64_req(p, "lat", C)?,
        lon: f64_req(p, "lon", C)?,
        radius_m: f64_req(p, "radiusM", C)?,
        private: bool_or(p, "private", false),
    };
    let id = s.catalog.alloc_location_id();
    save(s, C, "Add Saved Location", id, location)?;
    Ok(json!({"id": id.0}))
}

/// `location.rename`: `{id, name}`; the circle stays where it is.
fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "location.rename";
    let id = location_id(p, C)?;
    let location = SavedLocation { name: name_param(p, C)?, ..existing(s, id, C)? };
    save(s, C, "Rename Saved Location", id, location)?;
    Ok(json!({"id": id.0}))
}

/// `location.setPrivate`: `{id, private}`; a private location strips its photos' location on export.
fn set_private(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "location.setPrivate";
    let id = location_id(p, C)?;
    let private = p.get("private").and_then(Value::as_bool).ok_or_else(|| bad(C, "missing `private` (true or false)"))?;
    let location = SavedLocation { private, ..existing(s, id, C)? };
    save(s, C, "Set Location Privacy", id, location)?;
    Ok(json!({"id": id.0, "private": private}))
}

/// `location.delete`: `{id}`; undo brings the location back as it was. A filter by it is cleared.
fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "location.delete";
    let id = location_id(p, C)?;
    existing(s, id, C)?;
    s.commit("Delete Saved Location", Op::SetSavedLocation { id, location: None })?;
    if s.filter.saved_location == Some(id) {
        s.filter.saved_location = None;
    }
    Ok(json!({"id": id.0}))
}

/// `location.setCircle`: `{id, lat?, lon?, radiusM?, name?}` moves the centre, changes the radius or renames the location;
/// the parts not given stay as they are. One undo step. The Map view uses it when a centre is dragged or a circle edited.
fn set_circle(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "location.setCircle";
    let id = location_id(p, C)?;
    let current = existing(s, id, C)?;
    let location = SavedLocation {
        name: if p.get("name").is_some() { name_param(p, C)? } else { current.name.clone() },
        lat: if p.get("lat").is_some() { f64_req(p, "lat", C)? } else { current.lat },
        lon: if p.get("lon").is_some() { f64_req(p, "lon", C)? } else { current.lon },
        radius_m: if p.get("radiusM").is_some() { f64_req(p, "radiusM", C)? } else { current.radius_m },
        private: current.private,
    };
    let (lat, lon, radius_m) = (location.lat, location.lon, location.radius_m);
    save(s, C, "Edit Saved Location", id, location)?;
    Ok(json!({"id": id.0, "lat": lat, "lon": lon, "radiusM": radius_m}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "location.list", "Saved Locations", [], None, "{} → {locations: [{id, name, lat, lon, radiusM, private, photos}]} — saved locations by name; photos = library photos inside the circle", always, list),
        cmd!(
            "location.add",
            "Add Saved Location",
            [],
            None,
            "{name, lat, lon, radiusM (metres, more than 0), private?: bool} → {id} — a named circle on the map; a private one strips the GPS and place names of the photos inside it on export; one undo step",
            always,
            add
        ),
        cmd!("location.rename", "Rename Saved Location", [], None, "{id, name} → {id} — renames a saved location; one undo step", always, rename),
        cmd!(
            "location.setCircle",
            "Edit Saved Location",
            [],
            None,
            "{id, lat?, lon?, radiusM?, name?} → {id, lat, lon, radiusM} — moves a saved location's centre, changes its radius or renames it (the parts not given stay); one undo step",
            always,
            set_circle
        ),
        cmd!(
            "location.setPrivate",
            "Set Location Privacy",
            [],
            None,
            "{id, private: bool} → {id, private} — marks a saved location private (its photos' GPS and place names leave out of exports) or public; one undo step",
            always,
            set_private
        ),
        cmd!(
            "location.delete",
            "Delete Saved Location",
            [],
            None,
            "{id} → {id} — deletes a saved location (the photos keep their GPS); one undo step",
            always,
            delete
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::PhotoId;

    fn add_home(s: &mut Session, private: bool) -> u64 {
        let r = s.execute("location.add", &json!({"name": " Home ", "lat": 43.0, "lon": -110.0, "radiusM": 500.0, "private": private})).unwrap();
        r["id"].as_u64().unwrap()
    }

    /// The photos the library shows under `filter` (a partial Filter), in id order.
    /// The photos one `library.filter` patch shows: the location, GPS and place filters start clear (a patch merges into the
    /// current filter, so a filter left by an earlier call would narrow this one).
    fn shown(s: &mut Session, filter: Value) -> Vec<PhotoId> {
        s.execute("library.filter", &json!({"savedLocation": null, "hasGps": null, "namedPlace": null})).unwrap();
        s.execute("library.filter", &filter).unwrap();
        let mut ids = s.visible_cloned();
        ids.sort();
        ids
    }

    fn put(s: &mut Session, id: PhotoId, gps: Option<(f64, f64)>, city: &str) {
        let mut meta = s.catalog.photo(id).unwrap().meta.clone();
        meta.gps = gps;
        // Demo scenes carry a `location` (sublocation); clear every place field so this fixture
        // controls `namedPlace` through `city` alone.
        meta.location.clear();
        meta.city = city.into();
        meta.state.clear();
        meta.country.clear();
        s.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }

    fn list_names(s: &mut Session) -> Vec<(String, u64)> {
        let r = s.execute("location.list", &json!({})).unwrap();
        r["locations"].as_array().unwrap().iter().map(|l| (l["name"].as_str().unwrap().to_string(), l["photos"].as_u64().unwrap())).collect()
    }

    #[test]
    fn add_rename_privacy_and_delete_round_trip_with_undo() {
        let mut s = Session::with_demo();
        let id = add_home(&mut s, false);
        assert_eq!(list_names(&mut s), vec![("Home".to_string(), 0)], "the name is trimmed");
        s.execute("location.rename", &json!({"id": id, "name": "Flat"})).unwrap();
        s.execute("location.setPrivate", &json!({"id": id, "private": true})).unwrap();
        let r = s.execute("location.list", &json!({})).unwrap();
        assert_eq!(
            (r["locations"][0]["name"].as_str(), r["locations"][0]["private"].as_bool(), r["locations"][0]["radiusM"].as_f64()),
            (Some("Flat"), Some(true), Some(500.0))
        );
        s.execute("location.delete", &json!({"id": id})).unwrap();
        assert!(list_names(&mut s).is_empty());
        // undo each step back to the empty list
        s.undo_step().unwrap();
        assert_eq!(list_names(&mut s), vec![("Flat".to_string(), 0)]);
        s.undo_step().unwrap();
        assert_eq!(s.execute("location.list", &json!({})).unwrap()["locations"][0]["private"].as_bool(), Some(false));
        s.undo_step().unwrap();
        assert_eq!(list_names(&mut s), vec![("Home".to_string(), 0)]);
        s.undo_step().unwrap();
        assert!(list_names(&mut s).is_empty());
    }

    #[test]
    fn a_circle_is_moved_resized_and_renamed_each_as_one_undo_step() {
        let mut s = Session::with_demo();
        let id = add_home(&mut s, false);
        let undo_before = s.undo.len();
        s.execute("location.setCircle", &json!({"id": id, "lat": 49.0, "lon": 3.0})).unwrap();
        let moved = s.catalog.saved_location(LocationId(id)).cloned().unwrap();
        assert_eq!((moved.lat, moved.lon, moved.radius_m, moved.name.as_str()), (49.0, 3.0, 500.0, "Home"));
        s.execute("location.setCircle", &json!({"id": id, "radiusM": 2000.0, "name": "Flat"})).unwrap();
        let resized = s.catalog.saved_location(LocationId(id)).cloned().unwrap();
        assert_eq!((resized.lat, resized.lon, resized.radius_m, resized.name.as_str()), (49.0, 3.0, 2000.0, "Flat"), "the parts not given stay");
        assert_eq!(s.undo.len(), undo_before + 2, "one step for the move, one for the edit");
        s.undo_step().unwrap();
        assert_eq!(s.catalog.saved_location(LocationId(id)).unwrap().radius_m, 500.0);
        s.undo_step().unwrap();
        assert_eq!(s.catalog.saved_location(LocationId(id)).unwrap().lat, 43.0, "undo brings the centre back");
    }

    #[test]
    fn a_circle_that_makes_no_sense_or_a_location_that_is_not_there_is_refused() {
        let mut s = Session::with_demo();
        let id = add_home(&mut s, false);
        let before = s.catalog.saved_location(LocationId(id)).cloned();
        let undo_before = s.undo.len();
        for bad in [
            json!({"id": id, "radiusM": 0.0}),
            json!({"id": id, "lat": 95.0}),
            json!({"id": id, "name": "  "}),
            json!({"id": 99, "lat": 1.0}),
            json!({"lat": 1.0}),
        ] {
            assert!(s.execute("location.setCircle", &bad).is_err(), "{bad}");
        }
        assert_eq!(s.catalog.saved_location(LocationId(id)).cloned(), before);
        assert_eq!(s.undo.len(), undo_before);
    }

    #[test]
    fn bad_input_is_refused_with_a_reason_and_changes_nothing() {
        let mut s = Session::with_demo();
        assert!(s.execute("location.add", &json!({"name": "  ", "lat": 1.0, "lon": 1.0, "radiusM": 10.0})).is_err());
        assert!(s.execute("location.add", &json!({"name": "x", "lat": 91.0, "lon": 1.0, "radiusM": 10.0})).is_err());
        assert!(s.execute("location.add", &json!({"name": "x", "lat": 1.0, "lon": 1.0})).is_err(), "no radius");
        assert!(s.execute("location.rename", &json!({"id": 99, "name": "x"})).is_err());
        assert!(s.execute("location.delete", &json!({"id": 99})).is_err());
        assert!(s.execute("location.setPrivate", &json!({"id": 99, "private": true})).is_err());
        assert!(list_names(&mut s).is_empty());
    }

    #[test]
    fn the_count_is_the_photos_with_gps_inside_the_circle() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        for (id, gps) in [(ids[0], (43.0, -110.0)), (ids[1], (43.001, -110.0)), (ids[2], (50.0, 8.0))] {
            let mut meta = s.catalog.photo(id).unwrap().meta.clone();
            meta.gps = Some(gps);
            s.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        }
        add_home(&mut s, false);
        // 0.001 degrees of latitude is about 111 m: inside the 500 m circle
        assert_eq!(list_names(&mut s), vec![("Home".to_string(), 2)]);
    }

    #[test]
    fn the_location_filter_selects_the_photos_inside_the_circle_and_the_gps_and_place_filters() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);
        put(&mut s, a, Some((43.0, -110.0)), "");
        put(&mut s, b, Some((43.002, -110.0)), "Jackson");
        put(&mut s, c, Some((50.0, 8.0)), "");
        put(&mut s, d, None, "Paris");
        let home = LocationId(add_home(&mut s, false));
        let mut inside = vec![a, b];
        inside.sort();
        assert_eq!(shown(&mut s, json!({"savedLocation": home.0})), inside, "the photos inside the circle, and only those");
        let with_gps = shown(&mut s, json!({"hasGps": true}));
        assert!(with_gps.contains(&a) && with_gps.contains(&b) && with_gps.contains(&c) && !with_gps.contains(&d));
        let without_gps = shown(&mut s, json!({"hasGps": false}));
        assert!(without_gps.contains(&d) && !without_gps.contains(&a), "a photo with no GPS shows under No GPS");
        let named = shown(&mut s, json!({"namedPlace": true}));
        assert!(named.contains(&b) && named.contains(&d) && !named.contains(&a) && !named.contains(&c));
        assert!(shown(&mut s, json!({"namedPlace": false})).contains(&a));
        // the filters combine: a place name inside the circle
        assert_eq!(shown(&mut s, json!({"savedLocation": home.0, "namedPlace": true})), vec![b]);
        // a location that does not exist is refused, and the filter is left as it was
        assert!(s.execute("library.filter", &json!({"savedLocation": 999})).is_err());
        assert_eq!(shown(&mut s, json!({"savedLocation": home.0, "namedPlace": true})), vec![b]);
        // deleting the location clears the filter that named it
        s.execute("location.delete", &json!({"id": home.0})).unwrap();
        assert_eq!(s.filter.saved_location, None);
    }
}
