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

/// `location.delete`: `{id}`; undo brings the location back as it was.
fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "location.delete";
    let id = location_id(p, C)?;
    existing(s, id, C)?;
    s.commit("Delete Saved Location", Op::SetSavedLocation { id, location: None })?;
    Ok(json!({"id": id.0}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "location.list", "Saved Locations", [], None, "{} → {locations: [{id, name, lat, lon, radiusM, private, photos}]} — saved locations by name; photos = library photos inside the circle", always, list),
        cmd!("location.add", "Add Saved Location", [], None, "{name, lat, lon, radiusM (metres, more than 0), private?: bool} → {id} — a named circle on the map; a private one strips the GPS and place names of the photos inside it on export; one undo step", always, add),
        cmd!("location.rename", "Rename Saved Location", [], None, "{id, name} → {id} — renames a saved location; one undo step", always, rename),
        cmd!("location.setPrivate", "Set Location Privacy", [], None, "{id, private: bool} → {id, private} — marks a saved location private (its photos' GPS and place names leave out of exports) or public; one undo step", always, set_private),
        cmd!("location.delete", "Delete Saved Location", [], None, "{id} → {id} — deletes a saved location (the photos keep their GPS); one undo step", always, delete),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_home(s: &mut Session, private: bool) -> u64 {
        let r = s.execute("location.add", &json!({"name": " Home ", "lat": 43.0, "lon": -110.0, "radiusM": 500.0, "private": private})).unwrap();
        r["id"].as_u64().unwrap()
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
}
