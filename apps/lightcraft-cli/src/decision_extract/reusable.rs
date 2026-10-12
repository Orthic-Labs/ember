//! Private, content-addressed intermediates & derived diagnostics for reusable corpus work.
//! These measurements are not semantic ground truth or recovered editor settings.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use lightcraft_color::{REC2020, Xy, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_photo_ai::digest;
use lightcraft_pipeline::{SourceInfo, auto};
use lightcraft_raster::{Plane, Rgb32f};
use serde_json::{Value, json};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn pixel_bytes(image: &Rgb32f) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(image.data.len().saturating_mul(12).saturating_add(16));
    bytes.extend_from_slice(&(image.width as u64).to_le_bytes());
    bytes.extend_from_slice(&(image.height as u64).to_le_bytes());
    for pixel in &image.data {
        for value in pixel {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

fn store(root: Option<&Path>, bytes: &[u8], width: usize, height: usize, channels: usize, interpretation: &str) -> Result<Value, String> {
    let hash = digest(bytes);
    let Some(root) = root else { return Ok(json!({"retained": false, "reason": "proxy_cache_not_requested", "sha256": hash})) };
    fs::create_dir_all(root).map_err(|_| "proxy_cache_directory_failed")?;
    let name = format!("{hash}.{channels}xf32le");
    let destination = root.join(&name);
    if !destination.exists() {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = root.join(format!(".{}-{sequence}.partial", std::process::id()));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|_| "proxy_cache_create_failed")?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
            return Err("proxy_cache_write_failed".into());
        }
        // Publish complete bytes atomically without replacing another writer's existing object.
        let linked = fs::hard_link(&temporary, &destination);
        let removed = fs::remove_file(&temporary);
        if linked.is_err() && !destination.exists() {
            return Err("proxy_cache_publish_failed".into());
        }
        removed.map_err(|_| "proxy_cache_temporary_cleanup_failed")?;
    }
    let retained = super::read_bounded(&destination, 48_000_016).map_err(|_| "proxy_cache_unreadable")?;
    if retained.len() != bytes.len() || digest(&retained) != hash {
        return Err("proxy_cache_integrity_failed".into());
    }
    Ok(json!({"retained": true, "object": name, "sha256": hash, "bytes": bytes.len(),
        "width": width, "height": height, "channels": channels,
        "encoding": "u64le width,height followed by row-major interleaved f32le samples", "interpretation": interpretation}))
}

pub(super) fn image(root: Option<&Path>, image: &Rgb32f, interpretation: &str) -> Result<Value, String> {
    super::finite_image(image)?;
    store(root, &pixel_bytes(image), image.width, image.height, 3, interpretation)
}

fn plane(root: Option<&Path>, plane: &Plane) -> Result<Value, String> {
    if plane.width.checked_mul(plane.height) != Some(plane.data.len())
        || plane.data.len() > super::EDGE * super::EDGE
        || plane.data.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err("invalid_region_plane".into());
    }
    let mut bytes = Vec::with_capacity(plane.data.len().saturating_mul(4).saturating_add(16));
    bytes.extend_from_slice(&(plane.width as u64).to_le_bytes());
    bytes.extend_from_slice(&(plane.height as u64).to_le_bytes());
    for value in &plane.data {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    store(root, &bytes, plane.width, plane.height, 1, "heuristic region alpha; not human segmentation labels")
}

pub(super) fn colour(image: &Rgb32f) -> Value {
    // Rendered Rec.2020 pixels need rendered-source WB semantics, not original camera matrices.
    let mut sum = [0.0f64; 3];
    let mut count = 0usize;
    for pixel in &image.data {
        let y = luminance_2020(*pixel);
        if (0.05..=0.8).contains(&y) && pixel.iter().all(|v| v.is_finite() && *v >= 0.0) {
            for (acc, value) in sum.iter_mut().zip(pixel) {
                *acc += f64::from(*value);
            }
            count = count.saturating_add(1);
        }
    }
    if count == 0 {
        return json!({"status": "unavailable", "reason": "no_nonnegative_midtones", "samples": 0});
    }
    let mean = sum.map(|v| v / count as f64);
    let xy = Xy::from_xyz(REC2020.to_xyz().apply(mean));
    let (temp, tint) = auto::neutral_wb(image, &SourceInfo::default());
    json!({"status": "measured", "samples": count, "midtone_linear_rgb_mean": mean,
        "midtone_xy": [xy.x, xy.y], "neutralizing_wb_estimate": {"temperature": temp, "tint": tint},
        "interpretation": "rendered-image grey-world diagnostic; not historical camera Kelvin or intended neutral colour"})
}

fn region_stats(plane: &Plane, scene: &Rgb32f, rendered: &Rgb32f) -> Result<Value, String> {
    if [scene.width, scene.height] != [plane.width, plane.height] || [rendered.width, rendered.height] != [plane.width, plane.height] {
        return Err("region_coordinate_frame_mismatch".into());
    }
    let mut selected = Vec::new();
    let mut ev = Vec::new();
    for ((alpha, source), output) in plane.data.iter().zip(&scene.data).zip(&rendered.data) {
        if auto::regions::inside(*alpha) {
            selected.push(*output);
            let y = luminance_2020(*source);
            if y.is_finite() && y > 1e-6 {
                ev.push((y / 0.18).log2());
            }
        }
    }
    if selected.is_empty() {
        return Ok(json!({"status": "unavailable", "reason": "empty_heuristic_population", "share": 0.0}));
    }
    let share = selected.len() as f64 / plane.data.len() as f64;
    let subset = Rgb32f { width: selected.len(), height: 1, data: selected };
    ev.sort_by(f32::total_cmp);
    let quantile = |q: f32| ev.get((ev.len().saturating_sub(1) as f32 * q) as usize).copied();
    let spread = quantile(0.95).zip(quantile(0.05)).map(|(hi, lo)| hi - lo);
    Ok(json!({"status": "measured", "share": share, "selected_pixels": subset.data.len(),
        "scene_ev_median": quantile(0.5), "scene_ev_spread": spread,
        "scene_ev_unavailable_reason": ev.is_empty().then_some("no_positive_scene_luminance"),
        "rendered_statistics": super::measured(&subset)?, "rendered_colour": colour(&subset)}))
}

pub(super) fn regions(
    root: Option<&Path>,
    source: &Rgb32f,
    info: &SourceInfo,
    before: &Rgb32f,
    baseline: &Rgb32f,
    finished: &Rgb32f,
) -> Result<Value, String> {
    let base = DevelopSettings::default();
    let Some(planes) = auto::regions::region_planes(source, info, &base) else {
        return Ok(json!({"status": "unavailable", "reason": "region_planes_unavailable"}));
    };
    let mut scene = source.clone();
    lightcraft_pipeline::local::scene_linear_pre(&mut scene, info, &base);
    super::finite_image(&scene)?;
    let decision = auto::regions::region_auto(source, info, &base);
    let mut settings = lightcraft_pipeline::look::settings_with(&base, decision.global);
    settings.masks = decision.masks(1).ok_or("region_mask_id_overflow")?;
    let regional_baseline = super::render_linear(source, info, &settings)?;
    let mut diagnostics = serde_json::Map::new();
    for (name, alpha) in [("subject", &planes.subject), ("background", &planes.background), ("sky", &planes.sky)] {
        diagnostics.insert(
            name.into(),
            json!({"plane": plane(root, alpha)?,
            "before": region_stats(alpha, &scene, before)?, "global_baseline": region_stats(alpha, &scene, baseline)?,
            "regional_baseline": region_stats(alpha, &scene, &regional_baseline)?, "finished": region_stats(alpha, &scene, finished)?}),
        );
    }
    Ok(json!({"status": "measured", "plane_population": "aligned source under default white balance; alpha>=0.6",
        "ground_truth": false, "decision": decision, "regional_baseline_settings": settings,
        "regional_baseline_proxy": image(root, &regional_baseline, "display-linear Rec.2020 D65; region-aware Auto")?,
        "regions": diagnostics}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_replays_exact_float_bytes_and_rejects_corruption() {
        let root = std::env::temp_dir().join(format!("ember-proxies-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        let pixels = Rgb32f::from_fn(3, 2, |x, _| [x as f32 * 0.1, 0.4, 0.2]);
        let first = image(Some(&root), &pixels, "test").unwrap();
        assert_eq!(first, image(Some(&root), &pixels, "test").unwrap());
        let path = root.join(first["object"].as_str().unwrap());
        assert_eq!(fs::read(&path).unwrap(), pixel_bytes(&pixels));
        fs::write(&path, b"corrupt").unwrap();
        assert_eq!(image(Some(&root), &pixels, "test").unwrap_err(), "proxy_cache_integrity_failed");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn colour_and_regions_preserve_direction_and_explicit_empty_populations() {
        let warm = Rgb32f::from_fn(4, 4, |_, _| [0.5, 0.3, 0.1]);
        let cool = Rgb32f::from_fn(4, 4, |_, _| [0.1, 0.3, 0.5]);
        assert_ne!(colour(&warm)["midtone_xy"], colour(&cool)["midtone_xy"]);
        let empty = Plane::new(4, 4);
        assert_eq!(region_stats(&empty, &warm, &cool).unwrap()["reason"], "empty_heuristic_population");
        let black = Rgb32f::new(4, 4);
        assert_eq!(colour(&black)["reason"], "no_nonnegative_midtones");
    }
}
