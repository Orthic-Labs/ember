//! A clip's non-destructive trim (issue #74): the in and out points (seconds) of the part of a clip that exports keep and
//! playback is limited to. The file is never cut; the points live in the photo's develop settings. Pure, so the engine's
//! commands, the UI and the export all agree on what a valid trim is.

/// The least a trim may keep (seconds): a shorter trim is no clip.
pub const MIN_TRIM_SECS: f64 = 0.05;

/// A trim as stored: either point may be unset (the clip's start, the clip's end).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Trim {
    /// The in point; `None` is the start of the clip.
    pub start: Option<f64>,
    /// The out point; `None` is the end of the clip.
    pub end: Option<f64>,
}

impl Trim {
    /// Neither point is set: the whole clip.
    pub fn is_none(&self) -> bool {
        self.start.is_none() && self.end.is_none()
    }
}

fn positive(v: Option<f64>) -> Option<f64> {
    v.filter(|d| d.is_finite() && *d > 0.0)
}

/// Check a trim against a clip of `duration` (`None` when unknown) and put it in its one canonical form: a point at the
/// clip's start or end is unset, an out point past the end is held to it, an in point at or past the end is refused, and the
/// points must leave at least [`MIN_TRIM_SECS`] between them. A point that is not a finite number of seconds, or is negative,
/// is refused.
pub fn check_trim(start: Option<f64>, end: Option<f64>, duration: Option<f64>) -> Result<Trim, String> {
    for (name, v) in [("in", start), ("out", end)] {
        if let Some(t) = v
            && (!t.is_finite() || t < 0.0)
        {
            return Err(format!("`{name}` must be zero or more seconds"));
        }
    }
    let duration = positive(duration);
    let start = start.filter(|s| *s > 0.0);
    let end = match (end, duration) {
        (Some(e), Some(d)) if e >= d => None,
        (e, _) => e,
    };
    if let (Some(s), Some(d)) = (start, duration)
        && s >= d
    {
        return Err(format!("`in` is past the end of the clip ({d:.3} s)"));
    }
    let (from, to) = (start.unwrap_or(0.0), end.or(duration));
    if let Some(to) = to
        && to - from < MIN_TRIM_SECS
    {
        return Err(format!("`in` must be before `out` (a trim keeps at least {MIN_TRIM_SECS} s)"));
    }
    Ok(Trim { start, end })
}

/// What a trim keeps of a clip of `duration`: where it starts (seconds) and how long it runs, `None` to the end of a clip
/// whose duration is not known.
pub fn kept_range(trim: Trim, duration: Option<f64>) -> (f64, Option<f64>) {
    let start = trim.start.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(0.0);
    let end = trim.end.filter(|e| e.is_finite() && *e > 0.0).or_else(|| positive(duration));
    (start, end.map(|e| (e - start).max(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trim_is_held_inside_the_clip_and_in_before_out() {
        let t = check_trim(Some(2.0), Some(8.0), Some(10.0)).unwrap();
        assert_eq!((t.start, t.end), (Some(2.0), Some(8.0)));
        assert!(!t.is_none());
        assert_eq!(check_trim(Some(2.0), None, Some(10.0)).unwrap(), Trim { start: Some(2.0), end: None });
        assert_eq!(check_trim(None, Some(4.0), Some(10.0)).unwrap(), Trim { start: None, end: Some(4.0) });
        assert!(check_trim(None, None, Some(10.0)).unwrap().is_none());
        assert!(check_trim(Some(8.0), Some(2.0), Some(10.0)).is_err(), "in after out");
        assert!(check_trim(Some(3.0), Some(3.0), Some(10.0)).is_err(), "nothing kept");
        assert!(check_trim(Some(3.0), Some(3.0 + MIN_TRIM_SECS / 2.0), None).is_err(), "less than the least, duration unknown");
        assert!(check_trim(None, Some(MIN_TRIM_SECS), None).is_ok(), "exactly the least is enough");
        assert!(check_trim(Some(10.0), None, Some(10.0)).is_err(), "in at the end keeps nothing");
        assert!(check_trim(Some(12.0), None, Some(10.0)).is_err(), "in past the end");
        assert!(check_trim(Some(9.99), None, Some(10.0)).is_err(), "less than the least before the end");
    }

    #[test]
    fn points_at_the_ends_are_unset_and_an_out_past_the_end_is_held() {
        assert!(check_trim(Some(0.0), Some(10.0), Some(10.0)).unwrap().is_none(), "the whole clip is no trim");
        assert_eq!(check_trim(Some(1.0), Some(500.0), Some(10.0)).unwrap(), Trim { start: Some(1.0), end: None });
        assert_eq!(check_trim(Some(1.0), Some(500.0), None).unwrap(), Trim { start: Some(1.0), end: Some(500.0) }, "no duration, no hold");
    }

    #[test]
    fn what_is_not_a_time_is_refused() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5] {
            assert!(check_trim(Some(bad), None, Some(10.0)).is_err(), "in {bad}");
            assert!(check_trim(None, Some(bad), Some(10.0)).is_err(), "out {bad}");
            assert!(check_trim(Some(bad), None, None).is_err(), "in {bad}, no duration");
        }
        // a duration that is no duration is ignored, not an error
        assert!(check_trim(Some(1.0), Some(2.0), Some(f64::NAN)).is_ok());
        assert!(check_trim(Some(1.0), Some(2.0), Some(-3.0)).is_ok());
    }

    #[test]
    fn the_kept_range_is_where_it_starts_and_how_long_it_runs() {
        assert_eq!(kept_range(Trim::default(), Some(10.0)), (0.0, Some(10.0)));
        assert_eq!(kept_range(Trim::default(), None), (0.0, None));
        assert_eq!(kept_range(Trim { start: Some(2.0), end: Some(8.5) }, Some(10.0)), (2.0, Some(6.5)));
        assert_eq!(kept_range(Trim { start: Some(2.0), end: None }, Some(10.0)), (2.0, Some(8.0)));
        assert_eq!(kept_range(Trim { start: Some(2.0), end: None }, None), (2.0, None));
        assert_eq!(kept_range(Trim { start: None, end: Some(4.0) }, None), (0.0, Some(4.0)));
        assert_eq!(kept_range(Trim { start: Some(9.0), end: Some(3.0) }, Some(10.0)), (9.0, Some(0.0)), "never a negative length");
        assert_eq!(kept_range(Trim { start: Some(f64::NAN), end: Some(f64::NAN) }, Some(10.0)), (0.0, Some(10.0)));
    }
}
