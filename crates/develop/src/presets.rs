//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the groups it includes. Applying merges it
//! into the photo's settings; `amount` (0..200 %) interpolates numeric values between the current
//! settings and the preset's. Copy/paste/sync use the same group selection ([`SettingsGroup`]).
//!
//! The rendering process ([`crate::ProcessVersion`]) is in no group: copy, paste, sync and presets
//! made from a photo never carry it, so the photos they change keep their own. A partial that names
//! `process` itself (Apply Settings JSON) sets it like any other field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{AI_MASK_VERSION, DevelopSettings, Mask, geometry_stamp};

/// Groups shown in "Choose settings to copy" / "Create preset" (Lightroom's checklist).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsGroup {
    Profile,
    Treatment,
    WhiteBalance,
    Light,
    ToneCurve,
    Color,
    ColorMixer,
    ColorGrading,
    Effects,
    Vignette,
    Grain,
    Detail,
    Optics,
    Geometry,
    Crop,
    Masks,
    Spots,
    RedEye,
    LensBlur,
    Calibration,
}

impl SettingsGroup {
    pub const ALL: [SettingsGroup; 20] = [
        SettingsGroup::Profile,
        SettingsGroup::Treatment,
        SettingsGroup::WhiteBalance,
        SettingsGroup::Light,
        SettingsGroup::ToneCurve,
        SettingsGroup::Color,
        SettingsGroup::ColorMixer,
        SettingsGroup::ColorGrading,
        SettingsGroup::Effects,
        SettingsGroup::Vignette,
        SettingsGroup::Grain,
        SettingsGroup::Detail,
        SettingsGroup::Optics,
        SettingsGroup::Geometry,
        SettingsGroup::Crop,
        SettingsGroup::Masks,
        SettingsGroup::Spots,
        SettingsGroup::RedEye,
        SettingsGroup::LensBlur,
        SettingsGroup::Calibration,
    ];

    /// Groups included by default when copying (Lightroom excludes crop, masks and spots by default).
    pub fn default_copy() -> Vec<SettingsGroup> {
        SettingsGroup::ALL
            .into_iter()
            .filter(|g| !matches!(g, SettingsGroup::Crop | SettingsGroup::Masks | SettingsGroup::Spots | SettingsGroup::RedEye))
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsGroup::Profile => "Profile",
            SettingsGroup::Treatment => "Treatment",
            SettingsGroup::WhiteBalance => "White Balance",
            SettingsGroup::Light => "Light",
            SettingsGroup::ToneCurve => "Tone Curve",
            SettingsGroup::Color => "Color",
            SettingsGroup::ColorMixer => "Color Mixer",
            SettingsGroup::ColorGrading => "Color Grading",
            SettingsGroup::Effects => "Effects",
            SettingsGroup::Vignette => "Vignette",
            SettingsGroup::Grain => "Grain",
            SettingsGroup::Detail => "Detail",
            SettingsGroup::Optics => "Optics",
            SettingsGroup::Geometry => "Geometry",
            SettingsGroup::Crop => "Crop",
            SettingsGroup::Masks => "Masking",
            SettingsGroup::Spots => "Remove",
            SettingsGroup::RedEye => "Red Eye",
            SettingsGroup::LensBlur => "Lens Blur",
            SettingsGroup::Calibration => "Calibration",
        }
    }

    /// Top-level JSON keys of `DevelopSettings` that belong to the group.
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            SettingsGroup::Profile => &["profile"],
            SettingsGroup::Treatment => &["treatment"],
            SettingsGroup::WhiteBalance => &["wb"],
            SettingsGroup::Light => &["light", "hdr"],
            SettingsGroup::ToneCurve => &["curve"],
            SettingsGroup::Color => &["color"],
            SettingsGroup::ColorMixer => &["mixer", "bw_mix", "point_colors"],
            SettingsGroup::ColorGrading => &["grading"],
            SettingsGroup::Effects => &["effects"],
            SettingsGroup::Vignette => &["vignette"],
            SettingsGroup::Grain => &["grain"],
            SettingsGroup::Detail => &["detail", "enhance"],
            SettingsGroup::Optics => &["optics"],
            SettingsGroup::Geometry => &["geometry"],
            SettingsGroup::Crop => &["crop", "orientation"],
            SettingsGroup::Masks => &["masks"],
            SettingsGroup::Spots => &["spots"],
            SettingsGroup::RedEye => &["red_eye"],
            SettingsGroup::LensBlur => &["lens_blur"],
            SettingsGroup::Calibration => &["calibration"],
        }
    }
}

/// The subset of `s` covering `groups`, as a partial JSON object.
pub fn extract_groups(s: &DevelopSettings, groups: &[SettingsGroup]) -> Value {
    let full = s.to_json();
    let mut out = Map::new();
    for g in groups {
        for k in g.keys() {
            if let Some(v) = full.get(*k) {
                out.insert((*k).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Recursively merge `patch` into `base` (objects merge; everything else replaces).
pub fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => deep_merge(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// Interpolate numbers in `patch` towards/away from `base` by `t` (1 = patch as-is); non-numeric
/// values are taken from the patch when `t >= 0.5`.
fn scale_patch(base: &Value, patch: &Value, t: f64) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out = Map::new();
            for (k, pv) in p {
                let bv = b.get(k).unwrap_or(&Value::Null);
                out.insert(k.clone(), scale_patch(bv, pv, t));
            }
            Value::Object(out)
        }
        // Integer fields (the grain seed, ids) are discrete like strings: an interpolated seed would
        // not deserialize as an integer, and the whole preset would silently not apply.
        (Value::Number(b), Value::Number(p)) if b.is_f64() || p.is_f64() => {
            let (b, p) = (b.as_f64().unwrap_or(0.0), p.as_f64().unwrap_or(0.0));
            serde_json::Number::from_f64(b + (p - b) * t).map(Value::Number).unwrap_or(Value::Null)
        }
        (b, p) => {
            if t >= 0.5 {
                p.clone()
            } else {
                b.clone()
            }
        }
    }
}

/// Apply a partial settings object with an amount (1.0 = 100 %). Out-of-range values are clamped
/// through the control specs.
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    let base = s.to_json();
    let patch = if (amount - 1.0).abs() < 1e-9 { partial.clone() } else { scale_patch(&base, partial, amount) };
    // (a white balance without a scale keeps its old meaning: `DevelopSettings::merged`)
    let mut out = s.merged(&patch).unwrap_or_else(|_| s.clone());
    for c in crate::controls::CONTROLS {
        if let Some(v) = crate::controls::get(&out, c.id) {
            crate::controls::set(&mut out, c.id, v);
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub group: String,
    /// Partial settings.
    pub settings: Value,
    #[serde(default)]
    pub favorite: bool,
    /// Built-in presets ship with the app and can't be deleted.
    #[serde(default)]
    pub builtin: bool,
    /// Apply Auto Settings to the photo first, then this preset's settings on top ("apply Auto").
    #[serde(default)]
    pub auto: bool,
    /// Holds semantic masks (Subject, Sky, Background, People, Landscape) and no pixel masks: each photo it is
    /// applied to computes those masks for itself. See [`settle_masks`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub adaptive: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Whether a mask is defined by semantic kinds alone (every component is one): it stores no pixel data.
pub fn is_semantic_mask(m: &Mask) -> bool {
    !m.components.is_empty() && m.components.iter().all(|c| c.shape.is_semantic())
}

/// Whether the `masks` of a settings object include a semantic one.
fn has_semantic_mask(settings: &Value) -> bool {
    settings.get("masks").and_then(Value::as_array).is_some_and(|ms| {
        ms.iter().any(|m| serde_json::from_value::<Mask>(m.clone()).is_ok_and(|m| is_semantic_mask(&m)))
    })
}

/// Settle the masks of a preset's settings. A settings object with a semantic mask is an adaptive preset: only
/// its semantic masks are kept (pixel brush and shape masks hold pixels of one photo, so they are not saved).
/// Settings without one keep their masks as they are. Returns how many masks were left out.
pub fn settle_masks(settings: &mut Value) -> usize {
    if !has_semantic_mask(settings) {
        return 0;
    }
    let Some(masks) = settings.get_mut("masks").and_then(Value::as_array_mut) else { return 0 };
    let before = masks.len();
    masks.retain(|m| serde_json::from_value::<Mask>(m.clone()).is_ok_and(|m| is_semantic_mask(&m)));
    before - masks.len()
}

impl Preset {
    pub fn from_settings(id: &str, name: &str, group: &str, s: &DevelopSettings, groups: &[SettingsGroup]) -> Preset {
        Preset::from_settings_counted(id, name, group, s, groups).0
    }

    /// As [`Preset::from_settings`], also returning how many masks were left out (see [`settle_masks`]).
    pub fn from_settings_counted(id: &str, name: &str, group: &str, s: &DevelopSettings, groups: &[SettingsGroup]) -> (Preset, usize) {
        let mut settings = extract_groups(s, groups);
        let dropped = settle_masks(&mut settings);
        let adaptive = has_semantic_mask(&settings);
        let preset = Preset {
            id: id.into(),
            name: name.into(),
            group: group.into(),
            settings,
            favorite: false,
            builtin: false,
            auto: false,
            adaptive,
        };
        (preset, dropped)
    }

    /// Whether the preset is adaptive: it holds semantic masks, so applying it computes them for each photo.
    pub fn is_adaptive(&self) -> bool {
        self.adaptive || has_semantic_mask(&self.settings)
    }

    /// Apply at `amount` (1.0 = 100 %). Masks in the preset are added to the photo's own (ones it
    /// already has are skipped), their effect scaled by the amount. A semantic mask is stamped as computed for
    /// this photo's geometry now, so it is not stale the moment it is added.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        self.apply_to_photo(s, amount, None)
    }

    /// [`Preset::apply`] for a photo whose source has the stamp `source`: the preset's semantic masks carry it, so the
    /// first Update AI Settings after a source change finds them stale.
    pub fn apply_to_photo(&self, s: &DevelopSettings, amount: f64, source: Option<u64>) -> DevelopSettings {
        let Some(masks) = self.settings.get("masks").and_then(Value::as_array).filter(|m| !m.is_empty()) else {
            return apply_partial(s, &self.settings, amount);
        };
        let mut rest = self.settings.clone();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = apply_partial(s, &rest, amount);
        let stamp = geometry_stamp(&out);
        let mut next = out.masks.iter().map(|m| m.id).max().unwrap_or(0);
        for m in masks {
            let Ok(mut m) = serde_json::from_value::<Mask>(m.clone()) else { continue };
            if out.masks.iter().any(|q| q.name == m.name && q.components == m.components) {
                continue;
            }
            next += 1;
            m.id = next;
            m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
            if is_semantic_mask(&m) {
                m.ai_version = AI_MASK_VERSION;
                m.ai_source = source;
                m.ai_geometry = Some(stamp);
            }
            out.masks.push(m);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AI_MASK_VERSION, LocalAdjustments, MaskComponent, MaskOp, MaskShape};
    use serde_json::json;

    #[test]
    fn extract_and_apply_roundtrip() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.effects.clarity = 30.0;
        a.crop.geometry.angle = 5.0;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.get("crop").is_none());
        let b = p.apply(&DevelopSettings::default(), 1.0);
        assert_eq!(b.light.exposure, 1.0);
        assert_eq!(b.effects.clarity, 30.0);
        assert_eq!(b.crop.geometry.angle, 0.0);
    }

    #[test]
    fn amount_scales_numbers() {
        let base = DevelopSettings::default();
        let partial = json!({"light": {"exposure": 1.0, "contrast": 40}});
        let half = apply_partial(&base, &partial, 0.5);
        assert!((half.light.exposure - 0.5).abs() < 1e-9);
        assert!((half.light.contrast - 20.0).abs() < 1e-9);
        let more = apply_partial(&base, &partial, 2.0);
        assert!((more.light.contrast - 80.0).abs() < 1e-9);
        // clamped through specs
        let lots = apply_partial(&base, &json!({"light": {"exposure": 4.0}}), 2.0);
        assert_eq!(lots.light.exposure, 5.0);
    }

    /// A preset with the Grain group (its seed is an integer) used to do nothing at any amount
    /// other than 100 %: the interpolated seed failed to deserialize and the preset was dropped.
    #[test]
    fn amount_keeps_integer_fields_whole() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.grain.amount = 40.0;
        a.grain.seed = 12345;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.pointer("/grain/seed").is_some_and(Value::is_u64), "the default copy groups include the grain seed");
        let base = DevelopSettings::default();
        for (amount, seed) in [(0.3, 0), (0.5, 12345), (1.5, 12345), (2.0, 12345)] {
            let out = p.apply(&base, amount);
            assert!((out.light.exposure - amount.min(5.0)).abs() < 1e-9, "{amount}: exposure {}", out.light.exposure);
            assert!((out.grain.amount - (40.0 * amount).min(100.0)).abs() < 1e-9, "{amount}: grain {}", out.grain.amount);
            assert_eq!(out.grain.seed, seed, "{amount}: seed");
        }
    }

    #[test]
    fn deep_merge_objects() {
        let mut a = json!({"x": {"a": 1, "b": 2}, "y": [1]});
        deep_merge(&mut a, &json!({"x": {"b": 3}, "y": [2, 3]}));
        assert_eq!(a, json!({"x": {"a": 1, "b": 3}, "y": [2, 3]}));
    }

    fn local_mask(shape: MaskShape, name: &str) -> Mask {
        Mask {
            id: 1,
            name: name.into(),
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
            adjust: LocalAdjustments { exposure: 0.5, ..LocalAdjustments::default() },
            ..Mask::default()
        }
    }

    /// An adaptive preset stores its semantic mask by kind (no pixels), marks itself adaptive, and each photo it is
    /// applied to computes the mask with its own geometry stamp. Pixel brush masks are left out.
    #[test]
    fn adaptive_preset_keeps_semantic_masks_and_stamps_each_photo() {
        let mut a = DevelopSettings::default();
        a.masks = vec![local_mask(MaskShape::Subject, "Subject"), local_mask(MaskShape::Brush { strokes: Vec::new() }, "Brush")];
        let (p, dropped) = Preset::from_settings_counted("p", "Adaptive", "User", &a, &[SettingsGroup::Masks]);
        assert!(p.adaptive && p.is_adaptive());
        assert_eq!(dropped, 1, "the brush mask holds one photo's pixels and is not saved");
        assert_eq!(serde_json::to_value(&p).unwrap()["adaptive"], json!(true));
        let kept: Vec<Mask> = serde_json::from_value(p.settings["masks"].clone()).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].components[0].shape, MaskShape::Subject);

        let plain = DevelopSettings::default();
        let mut cropped = DevelopSettings::default();
        cropped.geometry.rotate = 5.0;
        let on_plain = p.apply(&plain, 1.0);
        let on_cropped = p.apply(&cropped, 1.0);
        let (m1, m2) = (&on_plain.masks[0], &on_cropped.masks[0]);
        assert_eq!(m1.components[0].shape, MaskShape::Subject);
        assert_eq!(m1.adjust.exposure, 0.5, "the preset's adjustments travel with the mask");
        assert_eq!(m1.ai_version, AI_MASK_VERSION);
        assert_eq!(m1.ai_geometry, Some(geometry_stamp(&on_plain)));
        assert_eq!(m2.ai_geometry, Some(geometry_stamp(&on_cropped)));
        assert_ne!(m1.ai_geometry, m2.ai_geometry, "each photo has its own stamp");
        assert!(!m1.ai_stale(0, geometry_stamp(&on_plain)));
    }

    /// Presets saved before the flag existed load, keep their pixel masks, and apply as they did.
    #[test]
    fn legacy_presets_load_and_keep_their_pixel_masks() {
        let legacy = r#"{"id":"user.old","name":"Old","group":"User","settings":{"masks":[{"id":1,"name":"Brush","visible":true,"invert":false,"components":[{"shape":{"kind":"brush","strokes":[]}}],"adjust":{}}],"light":{"exposure":1.0}}}"#;
        let p: Preset = serde_json::from_str(legacy).unwrap();
        assert!(!p.adaptive && !p.is_adaptive());
        let out = p.apply(&DevelopSettings::default(), 1.0);
        assert_eq!(out.masks.len(), 1);
        assert!(matches!(out.masks[0].components[0].shape, MaskShape::Brush { .. }), "a pixel mask stays a pixel mask");
        assert_eq!(out.light.exposure, 1.0);

        let mut a = DevelopSettings::default();
        a.masks = vec![local_mask(MaskShape::Brush { strokes: Vec::new() }, "Brush")];
        let (plain, dropped) = Preset::from_settings_counted("q", "Plain", "User", &a, &[SettingsGroup::Masks]);
        assert_eq!(dropped, 0);
        assert!(!plain.adaptive, "a pixel-only preset is not adaptive");
        assert_eq!(plain.settings["masks"].as_array().map(Vec::len), Some(1));
    }
}
