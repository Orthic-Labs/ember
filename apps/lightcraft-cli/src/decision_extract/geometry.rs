//! Measured similarity registration with disjoint validation correspondences.
use lightcraft_geom::{Homography, Point};
use lightcraft_merge::{
    align::work_image,
    features,
    ransac::{self, Model},
};
use lightcraft_raster::{Plane, Rgb32f};
use serde_json::{Value, json};

pub const CONTRACT: &str = "similarity.v1: 512px comparison grid; MOPS mutual ratio 0.75; deterministic RANSAC on 4/5 correspondences; held-out 1/5; >=12 fit inliers, >=6 validation matches, >=70% validation inliers within 1px, validation p90<=1px; >=3 quadrants; >=99% target coverage";

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

fn validate_fit(a: &[Point], b: &[Point], width: usize, height: usize) -> Result<(Homography, Value), String> {
    if a.len() != b.len() || a.len() < 30 || width == 0 || height == 0 {
        return Err("alignment_insufficient_matches".into());
    }
    let mut train_a = Vec::new();
    let mut train_b = Vec::new();
    let mut validation = Vec::new();
    for (index, (&source, &target)) in a.iter().zip(b).enumerate() {
        if index % 5 == 0 {
            validation.push((source, target));
        } else {
            train_a.push(source);
            train_b.push(target);
        }
    }
    let fitted = ransac::ransac(Model::Similarity, &train_a, &train_b, 1.5, 12, 0x454d424552).ok_or("alignment_similarity_fit_failed")?;
    if fitted.inliers.len() * 2 < train_a.len() || !fitted.h.0.iter().all(|v| v.is_finite()) {
        return Err("alignment_low_consensus".into());
    }
    let mut residuals: Vec<f64> = validation.iter().map(|(p, q)| ransac::transfer_error2(&fitted.h, *p, *q).sqrt()).collect();
    if residuals.iter().any(|v| !v.is_finite()) {
        return Err("alignment_nonfinite_validation".into());
    }
    residuals.sort_by(f64::total_cmp);
    let p90 = residuals.get((residuals.len().saturating_sub(1) * 9) / 10).copied().ok_or("alignment_no_validation")?;
    let good = residuals.iter().filter(|value| **value <= 1.0).count();
    if good < 6 || good * 10 < residuals.len() * 7 || p90 > 1.0 {
        return Err("alignment_held_out_residual_exceeds_one_proxy_pixel".into());
    }
    let mut quadrants = [false; 4];
    for (&p, &q) in a.iter().zip(b) {
        if ransac::transfer_error2(&fitted.h, p, q) <= 1.0 {
            let index = usize::from(q.x >= width as f64 / 2.0) + 2 * usize::from(q.y >= height as f64 / 2.0);
            if let Some(value) = quadrants.get_mut(index) {
                *value = true;
            }
        }
    }
    if quadrants.into_iter().filter(|present| *present).count() < 3 {
        return Err("alignment_insufficient_spatial_coverage".into());
    }
    let receipt = json!({"model": "similarity", "comparison_long_edge": 512, "matches": a.len(),
        "fit_inliers": fitted.inliers.len(), "fit_rms_px": fitted.rms,
        "validation_matches": validation.len(), "validation_inliers": good,
        "validation_p90_residual_px": p90, "residual_coordinate_frame": "decoded target proxy pixels (long edge <=512)",
        "source_proxy_to_target_proxy": fitted.h.0});
    Ok((fitted.h, receipt))
}

pub fn align(source: &Rgb32f, before: &Rgb32f, finished: &Rgb32f) -> Result<(Rgb32f, Value), String> {
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
    let inverse = h.inverse().ok_or("alignment_transform_singular")?;
    let [c, _, _, s, _, _, _, _, _] = h.0;
    let scale = c.hypot(s);
    if !(0.2..=5.0).contains(&scale) {
        return Err("alignment_scale_outside_bounds".into());
    }
    let mut valid = 0usize;
    let mut aligned = Rgb32f::new(finished.width, finished.height);
    for (index, pixel) in aligned.data.iter_mut().enumerate() {
        let x = (index % finished.width) as f64 + 0.5;
        let y = (index / finished.width) as f64 + 0.5;
        let p = inverse.apply(Point::new(x, y));
        if !p.x.is_finite() || !p.y.is_finite() {
            return Err("alignment_nonfinite_transform".into());
        }
        if p.x >= 0.5 && p.x <= source.width as f64 - 0.5 && p.y >= 0.5 && p.y <= source.height as f64 - 0.5 {
            valid += 1;
        }
        *pixel = source.sample_bilinear(p.x as f32, p.y as f32);
    }
    let coverage = valid as f64 / aligned.data.len().max(1) as f64;
    if coverage < 0.99 {
        return Err("alignment_target_outside_source".into());
    }
    receipt["target_coverage"] = json!(coverage);
    receipt["source_dimensions"] = json!([source.width, source.height]);
    receipt["target_dimensions"] = json!([finished.width, finished.height]);
    receipt["target_proxy_to_source_proxy"] = json!(inverse.0);
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
        assert!(ransac::transfer_error2(&found, a[3], b[3]) < 1e-10);
    }

    #[test]
    fn validation_correspondences_cannot_be_hidden_by_refitting() {
        let a = points();
        let mut b = a.clone();
        for (i, p) in b.iter_mut().enumerate() {
            if i % 5 == 0 {
                p.x += 8.0;
            }
        }
        assert!(validate_fit(&a, &b, 512, 512).is_err());
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
