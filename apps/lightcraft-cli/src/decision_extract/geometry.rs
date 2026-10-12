//! Measured similarity registration with disjoint validation correspondences.
use lightcraft_geom::{Homography, Point};
use lightcraft_merge::{
    align::work_image,
    features,
    ransac::{self, Model},
};
use lightcraft_raster::{Plane, Rgb32f};
use serde_json::{Value, json};

pub const CONTRACT: &str = "similarity.v2: 512px comparison grid; MOPS mutual ratio 0.75; coordinate-hashed, quadrant-stratified 1/5 holdout; deterministic RANSAC on remaining correspondences; >=12 fit inliers, >=6 held-out inliers, >=70% raw held-out matches within 1px; inlier p90<=1px with raw p90/outlier count retained; held-out inliers in >=3 quadrants; >=99% target coverage";

#[derive(Debug)]
pub struct AlignmentError {
    pub reason: String,
    pub evidence: Value,
}

impl From<&str> for AlignmentError {
    fn from(reason: &str) -> Self {
        Self { reason: reason.into(), evidence: json!({}) }
    }
}
impl From<String> for AlignmentError {
    fn from(reason: String) -> Self {
        Self { reason, evidence: json!({}) }
    }
}
fn failure(reason: &str, evidence: &Value) -> AlignmentError {
    AlignmentError { reason: reason.into(), evidence: evidence.clone() }
}

pub fn fingerprint(image: &Rgb32f) -> Vec<u8> {
    let (plane, _) = work_image(image, 1.0, 512);
    let mut differences = Vec::with_capacity(256);
    for y in 0..16 {
        for x in 0..16 {
            let a = features::bilinear(&plane, (x as f32 + 0.5) * plane.width as f32 / 17.0, (y as f32 + 0.5) * plane.height as f32 / 16.0);
            let b = features::bilinear(&plane, (x as f32 + 1.5) * plane.width as f32 / 17.0, (y as f32 + 0.5) * plane.height as f32 / 16.0);
            differences.push(u8::from(a > b));
        }
    }
    differences
}

struct Correspondences {
    train_a: Vec<Point>,
    train_b: Vec<Point>,
    validation: Vec<(Point, Point)>,
}

fn partition(a: &[Point], b: &[Point], width: usize, height: usize) -> Result<Correspondences, String> {
    let mut train_a = Vec::new();
    let mut train_b = Vec::new();
    let mut validation = Vec::new();
    // Stratify independently of detector strength/scale ordering. Validation points never fit h.
    let mut pairs: Vec<_> = a.iter().copied().zip(b.iter().copied()).collect();
    pairs.sort_by_key(|(p, _)| {
        (p.x.to_bits().wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17) ^ p.y.to_bits().rotate_left(31)).wrapping_mul(0xbf58_476d_1ce4_e5b9)
    });
    let mut quadrant_counts = [0usize; 4];
    for (source, target) in pairs {
        let quadrant = usize::from(target.x >= width as f64 / 2.0) + 2 * usize::from(target.y >= height as f64 / 2.0);
        let count = quadrant_counts.get_mut(quadrant).ok_or("alignment_quadrant_invalid")?;
        if *count % 5 == 0 {
            validation.push((source, target));
        } else {
            train_a.push(source);
            train_b.push(target);
        }
        *count += 1;
    }
    Ok(Correspondences { train_a, train_b, validation })
}

fn validate_fit(a: &[Point], b: &[Point], width: usize, height: usize) -> Result<(Homography, Value), AlignmentError> {
    let mut receipt = json!({"model": "similarity", "comparison_long_edge": 512,
        "matches": a.len(), "target_matches": b.len(), "target_dimensions": [width, height]});
    if a.len() != b.len() || a.len() < 30 || width == 0 || height == 0 {
        return Err(failure("alignment_insufficient_matches", &receipt));
    }
    let Correspondences { train_a, train_b, validation } = partition(a, b, width, height)?;
    receipt["fit_matches"] = json!(train_a.len());
    receipt["validation_matches"] = json!(validation.len());
    let fitted = ransac::ransac(Model::Similarity, &train_a, &train_b, 1.5, 12, 0x454d424552)
        .ok_or_else(|| failure("alignment_similarity_fit_failed", &receipt))?;
    receipt["fit_inliers"] = json!(fitted.inliers.len());
    receipt["fit_rms_px"] = json!(fitted.rms);
    receipt["source_proxy_to_target_proxy"] = json!(fitted.h.0);
    if fitted.inliers.len() * 2 < train_a.len() || !fitted.h.0.iter().all(|v| v.is_finite()) {
        return Err(failure("alignment_low_consensus", &receipt));
    }
    let mut residuals: Vec<f64> = validation.iter().map(|(p, q)| ransac::transfer_error2(&fitted.h, *p, *q).sqrt()).collect();
    if residuals.iter().any(|v| !v.is_finite()) {
        return Err(failure("alignment_nonfinite_validation", &receipt));
    }
    residuals.sort_by(f64::total_cmp);
    let raw_p90 = residuals.get((residuals.len().saturating_sub(1) * 9) / 10).copied().ok_or_else(|| failure("alignment_no_validation", &receipt))?;
    let good = residuals.iter().filter(|value| **value <= 1.0).count();
    let mut quadrants = [false; 4];
    for &(p, q) in &validation {
        if ransac::transfer_error2(&fitted.h, p, q) <= 1.0 {
            let index = usize::from(q.x >= width as f64 / 2.0) + 2 * usize::from(q.y >= height as f64 / 2.0);
            if let Some(value) = quadrants.get_mut(index) {
                *value = true;
            }
        }
    }
    let quadrant_count = quadrants.into_iter().filter(|present| *present).count();
    receipt["validation_inliers"] = json!(good);
    receipt["validation_outliers"] = json!(validation.len().saturating_sub(good));
    receipt["validation_inlier_fraction"] = json!(good as f64 / validation.len().max(1) as f64);
    receipt["validation_raw_p90_residual_px"] = json!(raw_p90);
    receipt["validation_quadrant_count"] = json!(quadrant_count);
    receipt["validation_residual_population"] = json!("held-out inliers at <=1px");
    receipt["residual_coordinate_frame"] = json!("decoded target proxy pixels (long edge <=512)");
    if good < 6 || good * 10 < residuals.len() * 7 {
        return Err(failure("alignment_held_out_residual_exceeds_one_proxy_pixel", &receipt));
    }
    // Descriptive inlier p90; the independent gate is >=70% of all held-out matches within 1px.
    let p90 = residuals.get((good.saturating_sub(1) * 9) / 10).copied().ok_or_else(|| failure("alignment_no_validation", &receipt))?;
    receipt["validation_p90_residual_px"] = json!(p90);
    if quadrant_count < 3 {
        return Err(failure("alignment_insufficient_spatial_coverage", &receipt));
    }
    Ok((fitted.h, receipt))
}

pub fn align(source: &Rgb32f, before: &Rgb32f, finished: &Rgb32f) -> Result<(Rgb32f, Value), AlignmentError> {
    if source.width != before.width || source.height != before.height {
        return Err("before_render_changes_source_frame".into());
    }
    let source_plane = gray(before);
    let target_plane = gray(finished);
    let a = features::detect(&source_plane, 900);
    let b = features::detect(&target_plane, 900);
    let matches = features::match_features(&a, &b, 0.75);
    let mut source_points = Vec::with_capacity(matches.len());
    let mut target_points = Vec::with_capacity(matches.len());
    for (i, j) in matches {
        let p = a.points.get(i).ok_or("alignment_source_index_invalid")?;
        let q = b.points.get(j).ok_or("alignment_target_index_invalid")?;
        source_points.push(Point::new(p.x as f64, p.y as f64));
        target_points.push(Point::new(q.x as f64, q.y as f64));
    }
    let (h, mut receipt) = validate_fit(&source_points, &target_points, finished.width, finished.height)?;
    let inverse = h.inverse().ok_or_else(|| failure("alignment_transform_singular", &receipt))?;
    let [c, _, _, s, _, _, _, _, _] = h.0;
    let scale = c.hypot(s);
    if !(0.2..=5.0).contains(&scale) {
        return Err(failure("alignment_scale_outside_bounds", &receipt));
    }
    let mut valid = 0usize;
    let mut aligned = Rgb32f::new(finished.width, finished.height);
    for (index, pixel) in aligned.data.iter_mut().enumerate() {
        let x = (index % finished.width) as f64 + 0.5;
        let y = (index / finished.width) as f64 + 0.5;
        let p = inverse.apply(Point::new(x, y));
        if !p.x.is_finite() || !p.y.is_finite() {
            return Err(failure("alignment_nonfinite_transform", &receipt));
        }
        if p.x >= 0.5 && p.x <= source.width as f64 - 0.5 && p.y >= 0.5 && p.y <= source.height as f64 - 0.5 {
            valid += 1;
        }
        *pixel = source.sample_bilinear(p.x as f32, p.y as f32);
    }
    let coverage = valid as f64 / aligned.data.len().max(1) as f64;
    receipt["target_coverage"] = json!(coverage);
    receipt["source_dimensions"] = json!([source.width, source.height]);
    receipt["target_dimensions"] = json!([finished.width, finished.height]);
    receipt["target_proxy_to_source_proxy"] = json!(inverse.0);
    if coverage < 0.99 {
        return Err(failure("alignment_target_outside_source", &receipt));
    }
    Ok((aligned, receipt))
}

fn gray(image: &Rgb32f) -> Plane {
    // Robust to exposure differences; gamma-coded luminance uses the existing merge matcher.
    work_image(image, 1.0, 512).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points() -> Vec<Point> {
        (0..80).map(|i| Point::new(40.0 + (i % 10) as f64 * 38.0, 40.0 + (i / 10) as f64 * 48.0)).collect()
    }

    #[test]
    fn accepts_known_crop_similarity_with_held_out_measurements() {
        let a = points();
        let h = Homography([1.05, -0.01, -7.0, 0.01, 1.05, 4.0, 0.0, 0.0, 1.0]);
        let b: Vec<_> = a.iter().map(|p| h.apply(*p)).collect();
        let (found, receipt) = validate_fit(&a, &b, 512, 512).expect("known similarity");
        assert!(receipt["validation_p90_residual_px"].as_f64().unwrap() < 1e-6);
        assert!(receipt["validation_quadrant_count"].as_u64().unwrap() >= 3);
        assert!(ransac::transfer_error2(&found, a[3], b[3]) < 1e-10);
    }

    #[test]
    fn validation_correspondences_cannot_be_hidden_by_refitting() {
        let a = points();
        let mut b = a.clone();
        let split = partition(&a, &b, 512, 512).unwrap();
        for (p, _) in split.validation {
            let i = a.iter().position(|point| *point == p).unwrap();
            b[i].x += 8.0;
        }
        let failure = validate_fit(&a, &b, 512, 512).unwrap_err();
        assert_eq!(failure.evidence["matches"], a.len());
        assert!(failure.evidence["validation_inlier_fraction"].as_f64().unwrap() < 0.7);
        assert!(failure.evidence["validation_quadrant_count"].as_u64().is_some());
    }

    #[test]
    fn held_out_descriptor_outliers_are_not_geometric_error() {
        let a = points();
        let mut b = a.clone();
        let split = partition(&a, &b, 512, 512).unwrap();
        for (p, _) in split.validation.iter().take(split.validation.len() / 5) {
            let i = a.iter().position(|point| point == p).unwrap();
            b[i].x += 8.0;
        }
        let (_, receipt) = validate_fit(&a, &b, 512, 512).expect("independent spatial support despite descriptor outliers");
        assert!(receipt["validation_raw_p90_residual_px"].as_f64().unwrap() > 1.0);
        assert!(receipt["validation_p90_residual_px"].as_f64().unwrap() < 1e-6);
        assert!(receipt["validation_quadrant_count"].as_u64().unwrap() >= 3);
        assert!(receipt["validation_inlier_fraction"].as_f64().unwrap() >= 0.7);
        assert!(receipt["validation_outliers"].as_u64().unwrap() > 0);
    }

    #[test]
    fn geometry_needs_spatially_distributed_evidence() {
        let a: Vec<_> = points().iter().map(|p| Point::new(p.x / 4.0, p.y / 4.0)).collect();
        assert!(validate_fit(&a, &a, 512, 512).is_err());
    }

    #[test]
    fn blank_images_never_gain_identity_alignment_by_fallback() {
        let image = Rgb32f::from_fn(512, 384, |_, _| [0.3; 3]);
        assert!(align(&image, &image, &image).is_err());
    }
}
