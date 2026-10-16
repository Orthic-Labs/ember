//! `camera.calibrate`: Ember's own camera colour calibration from a photographed ColorChecker Classic chart.
//!
//! The command reads one raw photo of the chart, averages its 24 patches from the chart-region coordinates
//! ([`lightcraft_raw::calibrate::sample_chart`]), fits DNG colour tags by least squares against the chart's
//! published reference colours ([`lightcraft_raw::calibrate::fit_chart`]) and stores them as the camera model's
//! chart profile ([`crate::camera_calibration`]). Raws of that model without colour matrices of their own then start
//! from the measured matrices. Nothing in the catalog changes, so the command is not journaled.

use serde_json::Value;

use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

const C: &str = "camera.calibrate";

/// The chart's patch-grid corners from `corners` (four `[x, y]`: patch 1 top left, patch 6 top right, patch 24 bottom
/// right, patch 19 bottom left) or `region` (`[x, y, width, height]`, upright chart), as fractions of the developed,
/// oriented picture.
fn corners_of(p: &Value) -> Result<[[f64; 2]; 4]> {
    if let Some(c) = p.get("corners").filter(|v| !v.is_null()) {
        let wrong = || {
            bad(
                C,
                "`corners` must be four [x, y] points, as fractions 0 to 1 of the picture: top left of patch 1 (dark skin), top right of patch 6, bottom right of patch 24 (black), bottom left of patch 19 (white)",
            )
        };
        let points = c.as_array().filter(|a| a.len() == 4).ok_or_else(wrong)?;
        let mut out = [[0.0f64; 2]; 4];
        for (slot, point) in out.iter_mut().zip(points) {
            let q = point.as_array().filter(|q| q.len() == 2).ok_or_else(wrong)?;
            let x = q.first().and_then(Value::as_f64).ok_or_else(wrong)?;
            let y = q.get(1).and_then(Value::as_f64).ok_or_else(wrong)?;
            *slot = [x, y];
        }
        return Ok(out);
    }
    let wrong = || bad(C, "give `region` [x, y, width, height] (an upright chart) or `corners`, as fractions 0 to 1 of the picture");
    let r = p.get("region").and_then(Value::as_array).filter(|a| a.len() == 4).ok_or_else(wrong)?;
    let v: Vec<f64> = r.iter().filter_map(Value::as_f64).collect();
    let [x, y, w, h] = <[f64; 4]>::try_from(v).map_err(|_| wrong())?;
    if !(w > 0.0 && h > 0.0) {
        return Err(wrong());
    }
    Ok(lightcraft_raw::calibrate::corners_of_rect([x, y, w, h]))
}

#[cfg(not(target_arch = "wasm32"))]
fn calibrate(s: &mut Session, p: &Value) -> Result<Value> {
    use lightcraft_raw::calibrate::{ChartEdition, fit_chart, sample_chart};
    use serde_json::json;

    let corners = corners_of(p)?;
    let edition = match p.get("edition").and_then(Value::as_str) {
        None => ChartEdition::default(),
        Some(e) => ChartEdition::parse(e).ok_or_else(|| bad(C, "`edition` is `after-2014` (the default, current charts) or `before-2014`"))?,
    };
    let kelvin = super::f64_or(p, "kelvin", 5000.0);
    let tint = super::f64_or(p, "tint", 0.0);
    if !(2000.0..=12000.0).contains(&kelvin) || !tint.is_finite() {
        return Err(bad(C, "`kelvin` (the chart light's colour temperature) must be 2000 to 12000"));
    }
    let ids = s.targets(p)?;
    let Some(&id) = ids.first().filter(|_| ids.len() == 1) else {
        return Err(bad(C, "name exactly one photo (`id`) or select one: the raw photo of the chart"));
    };
    let Some(lightcraft_catalog::Source::File { path }) = s.catalog.photo(id).map(|ph| &ph.source) else {
        return Err(bad(C, "the photo is not a file on disk"));
    };
    let bytes = std::fs::read(path).map_err(|e| bad(C, format!("can't read {path}: {e}")))?;
    let mut raw = lightcraft_raw::decode(&bytes).map_err(|e| bad(C, format!("not a readable raw: {e}")))?;
    let model = raw.metadata.model.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned);
    let Some(model) = model else { return Err(bad(C, "the file names no camera model to store the profile under")) };
    let make = raw.metadata.make.clone();
    // colour only: a lens correction would move the chart
    raw.opcodes.list3.retain(|op| !op.is_lens_correction());
    let img = raw.develop(lightcraft_raw::demosaic::Method::Ahd).map_err(|e| bad(C, format!("can't develop the raw: {e}")))?;
    let img = img.into_oriented(raw.orientation);
    let patches = sample_chart(&img, &corners).map_err(|e| bad(C, e))?;
    let illuminant = lightcraft_color::cct::temp_tint_to_xy(kelvin, tint);
    let fit = fit_chart(&patches, edition, illuminant).map_err(|e| bad(C, e))?;
    let force = super::bool_or(p, "force", false);
    if fit.delta_e_mean > MAX_MEAN_DELTA_E && !force {
        return Err(bad(
            C,
            format!(
                "the fit is poor (mean ΔE {:.1}, worst patch {}): check the chart corners, the edition and the exposure; `force: true` saves it anyway",
                fit.delta_e_mean,
                fit.worst_patch + 1
            ),
        ));
    }
    let save = super::bool_or(p, "save", true);
    let mut saved = Value::Null;
    if save {
        let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let profile = crate::camera_calibration::ChartProfile::new(&model, make.as_deref(), edition, &fit, created);
        let path = crate::camera_calibration::save(profile).map_err(|e| bad(C, e))?;
        saved = json!(path.display().to_string());
    }
    let round = |v: f64| (v * 100.0).round() / 100.0;
    Ok(json!({
        "model": model,
        "make": make,
        "edition": edition.name(),
        "kelvin": kelvin,
        "deltaEMean": round(fit.delta_e_mean),
        "deltaEMax": round(fit.delta_e_max),
        "worstPatch": fit.worst_patch + 1,
        "colorMatrix": fit.color_matrix.0,
        "forwardMatrix": fit.forward_matrix.0,
        "neutral": fit.neutral,
        "saved": saved,
        "note": "applies to raws of this model opened after this, when they carry no colour matrices of their own"
    }))
}

/// Mean CIE76 ΔE above which a fit is refused unless `force` is set (a linear matrix on a good chart shot lands well below).
#[cfg(not(target_arch = "wasm32"))]
const MAX_MEAN_DELTA_E: f64 = 8.0;

#[cfg(target_arch = "wasm32")]
fn calibrate(_: &mut Session, p: &Value) -> Result<Value> {
    corners_of(p)?;
    Err(bad(C, "camera calibration needs the file on disk"))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        explicit_targets: true,
        ..cmd!(
            query "camera.calibrate",
            "Calibrate Camera from Chart",
            [],
            None,
            "{id?, region: [x, y, w, h] | corners: [[x, y] ×4], edition?, kelvin?, tint?, save?, force?}: fit this camera model's colour from a raw photo `id` (else the selection) of a 24-patch ColorChecker Classic chart. `region` is the upright chart's patch grid, `corners` its outer corners (patch 1 top left, patch 6 top right, patch 24 bottom right, patch 19 bottom left), fractions 0 to 1 of the developed, oriented picture; `edition` `after-2014` (default) or `before-2014`; `kelvin` (default 5000) and `tint` the chart light, which only anchors the Kelvin scale. Least squares against the published reference colours; saves an Ember chart profile for the camera model (`save` false: fit only; a fit with mean ΔE over 8 needs `force`). Raws of that model without colour matrices of their own then start from it → {model, edition, deltaEMean, deltaEMax, worstPatch, colorMatrix, forwardMatrix, neutral, saved}",
            always,
            calibrate
        )
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn region_and_corners_are_read_and_bad_input_is_refused() {
        let c = corners_of(&json!({"region": [0.1, 0.2, 0.5, 0.25]})).unwrap();
        let want = [[0.1, 0.2], [0.6, 0.2], [0.6, 0.45], [0.1, 0.45]];
        assert!(c.iter().flatten().zip(want.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-12), "{c:?}");
        let c = corners_of(&json!({"corners": [[0.9, 0.8], [0.1, 0.8], [0.1, 0.2], [0.9, 0.2]]})).unwrap();
        assert_eq!(c[0], [0.9, 0.8]);
        for bad_params in [
            json!(null),
            json!({}),
            json!({"region": [0.1, 0.2, 0.5]}),
            json!({"region": [0.1, 0.2, 0.0, 0.5]}),
            json!({"region": "all"}),
            json!({"corners": [[0.0, 0.0]]}),
            json!({"corners": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0]]}),
            json!({"corners": [["a", 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]}),
        ] {
            assert!(corners_of(&bad_params).is_err(), "{bad_params}");
        }
    }
}
