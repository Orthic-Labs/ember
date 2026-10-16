//! Saved locations (`Op::SetSavedLocation`, format version 10): circles on the map that group photos by GPS.
//!
//! Scenarios, in the words of someone who keeps a place list for their trips:
//!
//! * A photo exactly on the edge of a location is in it; one a metre past the edge is not.
//! * I can add a location, rename it, mark it private and delete it, and undo each step exactly.
//! * A location that makes no sense (no name, a position off the Earth, no radius) is refused and nothing changes.
//! * The count says how many library photos are in a location: deleted and browsed-only photos are not counted.
//! * A private location covers the photos inside it on export, and no others.
//! * A library saved before locations existed opens with none.

use crate::locations::{EARTH_RADIUS_M, SavedLocation, distance_m};
use crate::*;

const HOME: (f64, f64) = (43.0, -110.0);

fn place(name: &str, radius_m: f64, private: bool) -> SavedLocation {
    SavedLocation { name: name.into(), lat: HOME.0, lon: HOME.1, radius_m, private }
}

fn photo_at(c: &mut Catalog, name: &str, gps: Option<(f64, f64)>) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: format!("/pics/{name}") }, name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    p.meta.gps = gps;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

#[test]
fn the_boundary_of_a_circle_is_inside_and_a_metre_past_it_is_not() {
    let edge = (HOME.0 + (1000.0 / EARTH_RADIUS_M).to_degrees(), HOME.1);
    let d = distance_m(HOME, edge);
    assert!((d - 1000.0).abs() < 0.01, "1 km north of the centre is {d} m away");
    // the radius the edge point measures exactly: the edge is in
    assert!(place("Edge", d, false).contains(edge));
    // a metre inside the same radius: the edge is out
    assert!(!place("Short", d - 1.0, false).contains(edge));
    // the centre is always in, and a point on the other side of the Earth never is
    assert!(place("Tiny", 0.5, false).contains(HOME));
    assert!(!place("Far", 20_000_000.0, false).contains((-43.0, 70.0)));
}

#[test]
fn great_circle_distance_is_the_earths_radius_times_the_angle() {
    // one degree of longitude on the equator is the radius times pi / 180
    let d = distance_m((0.0, 0.0), (0.0, 1.0));
    assert!((d - EARTH_RADIUS_M.to_radians()).abs() < 1e-3, "{d}");
    // across the antimeridian the short way round
    assert!((distance_m((0.0, 179.9), (0.0, -179.9)) - distance_m((0.0, 0.0), (0.0, 0.2))).abs() < 1e-3);
    assert_eq!(distance_m(HOME, HOME), 0.0);
}

#[test]
fn a_location_is_added_renamed_made_private_and_deleted_with_undo_at_each_step() {
    let mut c = Catalog::new();
    let id = c.alloc_location_id();
    let before = c.to_snapshot();
    let undo_add = c.apply(Op::SetSavedLocation { id, location: Some(place("Home", 500.0, false)) }).unwrap();
    assert_eq!(c.saved_location(id), Some(&place("Home", 500.0, false)));

    // rename keeps the circle and gives back the old record
    let renamed = SavedLocation { name: "Flat".into(), ..place("Home", 500.0, false) };
    let undo_rename = c.apply(Op::SetSavedLocation { id, location: Some(renamed.clone()) }).unwrap();
    assert_eq!(c.saved_location(id), Some(&renamed));
    // the privacy mark is a replacement too
    let private = SavedLocation { private: true, ..renamed.clone() };
    let undo_private = c.apply(Op::SetSavedLocation { id, location: Some(private.clone()) }).unwrap();
    assert_eq!(c.saved_location(id), Some(&private));

    // deleting it: gone, and undo brings it back exactly
    let undo_delete = c.apply(Op::SetSavedLocation { id, location: None }).unwrap();
    assert!(c.saved_location(id).is_none());
    c.apply(undo_delete).unwrap();
    assert_eq!(c.saved_location(id), Some(&private));
    c.apply(undo_private).unwrap();
    assert_eq!(c.saved_location(id), Some(&renamed));
    c.apply(undo_rename).unwrap();
    assert_eq!(c.saved_location(id), Some(&place("Home", 500.0, false)));
    c.apply(undo_add).unwrap();
    assert_eq!(c.to_snapshot(), before, "undoing the add leaves the catalog as it was");
}

#[test]
fn locations_survive_saving_and_reopening_and_ids_are_not_reused() {
    let mut c = Catalog::new();
    let a = c.alloc_location_id();
    c.apply(Op::SetSavedLocation { id: a, location: Some(place("Home", 500.0, true)) }).unwrap();
    let json = c.to_snapshot();
    let mut back = Catalog::from_snapshot(&json).unwrap();
    assert_eq!(back.saved_location(a), Some(&place("Home", 500.0, true)));
    assert_ne!(back.alloc_location_id(), a, "a reopened library hands out a new id");
    // a public location writes no privacy flag at all
    let mut c = Catalog::new();
    let b = c.alloc_location_id();
    c.apply(Op::SetSavedLocation { id: b, location: Some(place("Park", 300.0, false)) }).unwrap();
    assert!(!c.to_snapshot().contains("private"));
}

#[test]
fn a_location_that_makes_no_sense_is_refused_and_nothing_changes() {
    let mut c = Catalog::new();
    let id = c.alloc_location_id();
    let before = c.to_snapshot();
    let bad = [
        SavedLocation { name: "  ".into(), ..place("x", 500.0, false) },
        SavedLocation { name: "x".repeat(201), ..place("x", 500.0, false) },
        SavedLocation { lat: 91.0, ..place("x", 500.0, false) },
        SavedLocation { lon: f64::NAN, ..place("x", 500.0, false) },
        place("x", 0.0, false),
        place("x", f64::INFINITY, false),
        place("x", 20_200_000.0, false),
    ];
    for loc in bad {
        assert!(c.apply(Op::SetSavedLocation { id, location: Some(loc.clone()) }).is_err(), "{loc:?} must be refused");
    }
    assert_eq!(c.to_snapshot(), before);
    assert!(c.saved_location(id).is_none());
}

#[test]
fn the_count_is_the_library_photos_inside_the_circle() {
    let mut c = Catalog::new();
    let inside = photo_at(&mut c, "in.jpg", Some(HOME));
    photo_at(&mut c, "out.jpg", Some((44.0, -110.0)));
    photo_at(&mut c, "none.jpg", None);
    let deleted = photo_at(&mut c, "gone.jpg", Some(HOME));
    c.apply(Op::SetDeleted { id: deleted, deleted: true }).unwrap();
    let browsed = photo_at(&mut c, "browsed.jpg", Some(HOME));
    c.apply(Op::SetLocal { id: browsed, local: true }).unwrap();
    let id = c.alloc_location_id();
    c.apply(Op::SetSavedLocation { id, location: Some(place("Home", 5_000.0, false)) }).unwrap();
    assert_eq!(c.saved_location_count(id), 1, "only {inside:?} is in the library and inside the circle");
}

#[test]
fn a_private_location_marks_the_photos_inside_it_and_no_others() {
    let mut c = Catalog::new();
    let at_home = photo_at(&mut c, "home.jpg", Some(HOME));
    let away = photo_at(&mut c, "away.jpg", Some((44.0, -110.0)));
    let none = photo_at(&mut c, "none.jpg", None);
    let id = c.alloc_location_id();
    c.apply(Op::SetSavedLocation { id, location: Some(place("Home", 500.0, true)) }).unwrap();
    let photo = |id| c.photo(id).unwrap().clone();
    let (home_photo, away_photo, none_photo) = (photo(at_home), photo(away), photo(none));
    assert!(c.in_private_location(&home_photo));
    assert!(!c.in_private_location(&away_photo));
    assert!(!c.in_private_location(&none_photo));
    // a public location of the same circle hides nothing
    c.apply(Op::SetSavedLocation { id, location: Some(place("Home", 500.0, false)) }).unwrap();
    assert!(!c.in_private_location(&home_photo));
}

#[test]
fn a_library_saved_before_locations_opens_with_none() {
    let c = Catalog::new();
    assert!(!c.to_snapshot().contains("saved_locations"), "an empty list is not written");
    let back = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    assert_eq!(back.saved_locations().count(), 0);
}
