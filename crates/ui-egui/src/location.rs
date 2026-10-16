//! Show on Map (`photo.showOnMap`): the active photo's GPS position in OpenStreetMap, opened in the
//! browser through the host's `open_url` service. No map is embedded, and the position leaves the
//! app only inside the URL the user chose to open.

use serde_json::Value;

use crate::LightcraftApp;

/// The OpenStreetMap page centred on a position. A position that is not on Earth (NaN, infinite,
/// latitude beyond ±90, longitude beyond ±180) is refused before any URL exists.
pub fn map_url(lat: f64, lon: f64) -> Result<String, String> {
    if !(lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0) {
        return Err(format!("{lat}, {lon} is not a GPS position (latitude -90 to 90, longitude -180 to 180)"));
    }
    Ok(format!("https://www.openstreetmap.org/?mlat={lat:.6}&mlon={lon:.6}#map=15/{lat:.6}/{lon:.6}"))
}

/// `photo.showOnMap`: open the active photo's GPS position in OpenStreetMap.
pub fn show_on_map(app: &mut LightcraftApp) -> Result<Value, String> {
    let id = app.session.active().ok_or("select a photo first")?;
    let (lat, lon) = app.session.catalog.photo(id).and_then(|p| p.meta.gps).ok_or("this photo has no GPS position")?;
    let url = map_url(lat, lon)?;
    crate::links::open(app, &url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;

    #[test]
    fn map_url_centres_the_page_on_the_position() {
        assert_eq!(map_url(48.8566, 2.3522).unwrap(), "https://www.openstreetmap.org/?mlat=48.856600&mlon=2.352200#map=15/48.856600/2.352200");
        assert!(map_url(-90.0, 180.0).is_ok());
    }

    #[test]
    fn malformed_positions_are_refused() {
        for (lat, lon) in [(f64::NAN, 0.0), (0.0, f64::INFINITY), (91.0, 0.0), (0.0, -180.5), (-90.5, 10.0)] {
            assert!(map_url(lat, lon).is_err(), "{lat}, {lon} must be refused");
        }
    }

    #[test]
    fn show_on_map_needs_an_active_photo() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        assert_eq!(show_on_map(&mut app).unwrap_err(), "select a photo first");
    }
}
