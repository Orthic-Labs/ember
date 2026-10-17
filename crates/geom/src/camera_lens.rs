//! The camera's own radial lens-correction data, as a maker-neutral model.
//!
//! Cameras write vignetting and lateral chromatic aberration (CA) tables into their maker notes; a raw reader turns the
//! layout of one maker into this model and the optics pipeline applies it (an opt-in develop setting, "Use camera lens
//! corrections"). Radii are in half diagonals of the recorded frame (0 on the optical axis, 1 at the corners), so the
//! model does not depend on resolution.

use serde::{Deserialize, Serialize};

/// Most knots a [`RadialTable`] keeps (maker tables have 8 to 11).
pub const MAX_KNOTS: usize = 16;

/// A piecewise-linear function of the normalized radius: `len` knots (strictly increasing, from 0 or above) and the value
/// at each. Linear between knots, flat outside them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadialTable {
    pub len: u8,
    pub knots: [f32; MAX_KNOTS],
    pub values: [f32; MAX_KNOTS],
}

impl RadialTable {
    /// Build a table from knot radii and values. `None` unless there are 2 to [`MAX_KNOTS`] pairs, every number is
    /// finite, the first knot is not negative and the knots strictly increase.
    pub fn new(knots: &[f64], values: &[f64]) -> Option<Self> {
        let n = knots.len();
        if n != values.len() || !(2..=MAX_KNOTS).contains(&n) {
            return None;
        }
        let representable = |x: &f64| x.is_finite() && x.abs() < f64::from(f32::MAX);
        if !knots.iter().all(representable) || !values.iter().all(representable) || knots.first().is_none_or(|k| *k < 0.0) {
            return None;
        }
        if knots.windows(2).any(|w| !matches!(w, [a, b] if b > a)) {
            return None;
        }
        let mut t = RadialTable { len: u8::try_from(n).ok()?, ..Default::default() };
        for ((slot, k), (vslot, v)) in t.knots.iter_mut().zip(knots).zip(t.values.iter_mut().zip(values)) {
            *slot = *k as f32;
            *vslot = *v as f32;
        }
        Some(t)
    }

    /// The knots and values in use.
    pub fn points(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        let n = usize::from(self.len).min(MAX_KNOTS);
        self.knots.iter().zip(&self.values).take(n).map(|(k, v)| (f64::from(*k), f64::from(*v)))
    }

    /// The value at radius `x`: linear between knots, the end values outside; 0 for an empty table.
    pub fn at(&self, x: f64) -> f64 {
        let mut previous: Option<(f64, f64)> = None;
        for (k, v) in self.points() {
            if x <= k || x.is_nan() {
                return match previous {
                    Some((pk, pv)) if k > pk => pv + (v - pv) * (x - pk) / (k - pk),
                    _ => v,
                };
            }
            previous = Some((k, v));
        }
        previous.map_or(0.0, |(_, v)| v)
    }
}

/// What a camera records about its lens, in the model's units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraCorrection {
    /// Relative illumination (1 on the axis, below 1 towards the corners) at the radius on the sensor. The correction
    /// multiplies by its reciprocal.
    pub vignette: Option<RadialTable>,
    /// Lateral CA of the red and blue planes against green: a point at radius `r` in green lies at `r · (1 + s(r))` in
    /// that plane, `s` the table's value. The correction samples the plane there.
    pub ca: Option<[RadialTable; 2]>,
}

impl CameraCorrection {
    pub fn is_empty(&self) -> bool {
        self.vignette.is_none() && self.ca.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_and_holds_the_ends() {
        let t = RadialTable::new(&[0.0, 0.5, 1.0], &[1.0, 0.9, 0.5]).unwrap();
        assert_eq!(t.at(-1.0), 1.0);
        assert!((t.at(0.25) - 0.95).abs() < 1e-6);
        assert!((t.at(0.75) - 0.7).abs() < 1e-6);
        assert_eq!(t.at(2.0), 0.5);
        assert_eq!(t.at(f64::NAN), 1.0);
        assert_eq!(RadialTable::default().at(0.5), 0.0);
    }

    #[test]
    fn rejects_malformed_tables() {
        assert!(RadialTable::new(&[0.0], &[1.0]).is_none());
        assert!(RadialTable::new(&[0.0, 1.0], &[1.0]).is_none());
        assert!(RadialTable::new(&[0.5, 0.5], &[1.0, 1.0]).is_none());
        assert!(RadialTable::new(&[1.0, 0.5], &[1.0, 1.0]).is_none());
        assert!(RadialTable::new(&[-0.1, 0.5], &[1.0, 1.0]).is_none());
        assert!(RadialTable::new(&[0.0, f64::NAN], &[1.0, 1.0]).is_none());
        assert!(RadialTable::new(&[0.0, 1.0], &[1.0, f64::INFINITY]).is_none());
        let many: Vec<f64> = (0..=MAX_KNOTS).map(|i| i as f64).collect();
        assert!(RadialTable::new(&many, &many).is_none());
    }
}
