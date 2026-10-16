//! Saved locations (issue #116): named circles on the map, a centre and a radius in metres. A
//! photo is in a location when its GPS position is inside the circle (great-circle distance, the
//! boundary included). A location marked private takes the GPS and the place names off every photo
//! inside it on export (see `lightcraft_engine::export::removes_location`).
//!
//! Stored as [`Op::SetSavedLocation`] (format version 10): one op adds, replaces (rename, privacy)
//! or deletes a location, and its inverse is the previous record, so undo is exact.
//!
//! Sources: haversine formula (R = 6 371 008.8 m, the mean Earth radius), own design.

use serde::{Deserialize, Serialize};

use crate::{Catalog, Filter, Photo};

/// Mean Earth radius in metres (IUGG).
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;
/// Longest a location name may be, in characters.
pub const MAX_LOCATION_NAME: usize = 200;
/// Largest radius a saved location may have: half the Earth's circumference, rounded up.
pub const MAX_RADIUS_M: f64 = 20_100_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocationId(pub u64);

/// A named circle: `lat` / `lon` in degrees, `radius_m` in metres. `private` locations strip the
/// GPS and place names of the photos inside them on export.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLocation {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

/// The great-circle distance in metres between two GPS positions `(latitude, longitude)`, by the
/// haversine formula.
pub fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat_a, lat_b) = (a.0.to_radians(), b.0.to_radians());
    let dlat = (b.0 - a.0).to_radians();
    let dlon = (b.1 - a.1).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat_a.cos() * lat_b.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.clamp(0.0, 1.0).sqrt().asin()
}

impl SavedLocation {
    /// Whether a GPS position `(latitude, longitude)` is inside the circle (the boundary included).
    pub fn contains(&self, gps: (f64, f64)) -> bool {
        distance_m((self.lat, self.lon), gps) <= self.radius_m
    }

    /// Why this record can't be saved, in words the user can act on.
    pub fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("a location needs a name".into());
        }
        if self.name.chars().count() > MAX_LOCATION_NAME {
            return Err(format!("a location name is at most {MAX_LOCATION_NAME} characters"));
        }
        if !(self.lat.is_finite() && self.lon.is_finite() && self.lat.abs() <= 90.0 && self.lon.abs() <= 180.0) {
            return Err(format!("{}, {} is not a GPS position (latitude -90 to 90, longitude -180 to 180)", self.lat, self.lon));
        }
        if !(self.radius_m.is_finite() && self.radius_m > 0.0 && self.radius_m <= MAX_RADIUS_M) {
            return Err("the radius must be more than 0 metres and at most 20 100 km".into());
        }
        Ok(())
    }
}

/// Whether a photo has any place name (Sublocation, City, State or Country).
pub fn has_place_name(p: &Photo) -> bool {
    [&p.meta.location, &p.meta.city, &p.meta.state, &p.meta.country].iter().any(|s| !s.trim().is_empty())
}

impl Catalog {
    /// A fresh id for a saved location (the op that adds it carries the id).
    pub fn alloc_location_id(&mut self) -> LocationId {
        let id = LocationId(self.next_location.max(1));
        self.next_location = id.0.saturating_add(1);
        id
    }

    pub fn saved_location(&self, id: LocationId) -> Option<&SavedLocation> {
        self.saved_locations.get(&id)
    }

    /// Every saved location, by id.
    pub fn saved_locations(&self) -> impl Iterator<Item = (LocationId, &SavedLocation)> {
        self.saved_locations.iter().map(|(id, l)| (*id, l))
    }

    /// Photos in the library whose GPS position is inside the location (the filter's count).
    pub fn saved_location_count(&self, id: LocationId) -> usize {
        let filter = Filter { saved_location: Some(id), ..Default::default() };
        self.photos().filter(|p| filter.matches(p, self)).count()
    }

    /// Whether the photo is inside a private saved location: its GPS and place names stay out of exports.
    pub fn in_private_location(&self, p: &Photo) -> bool {
        p.meta.gps.is_some_and(|gps| self.saved_locations.values().any(|l| l.private && l.contains(gps)))
    }
}
