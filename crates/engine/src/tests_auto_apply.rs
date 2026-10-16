//! Auto in two phases (#192): the measure runs without the session, the apply is one undo step on
//! the session's thread and refuses a measure whose photo has changed since.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::auto::regions::RegionAuto;
use serde_json::{Value, json};

use crate::Session;
use crate::auto_measure::{AutoInput, auto_basis, auto_input, measure_auto, with_lens_pick};
use crate::media::{MediaCache, SourceLevel};

fn demo() -> Session {
    Session::with_demo()
}

fn dev(s: &Session) -> DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

/// Phase 1 the way a render worker does it: its own decode of the photo's origin, the input's
/// settings and facts, no session.
fn measure_elsewhere(input: &AutoInput) -> RegionAuto {
    let mut media = MediaCache::default();
    let src = media.origin_ref(&input.origin, SourceLevel::Thumb.max_edge()).load_source().unwrap();
    let info = with_lens_pick(src.info_or(input.info.clone()), input.lens_pick);
    measure_auto(&src.image, &info, &input.settings, &input.faces, input.regions)
}

fn apply_params(id: PhotoId, r: &RegionAuto, basis: &str) -> Value {
    json!({"id": id.0, "values": serde_json::to_value(r).unwrap(), "basis": basis})
}

#[test]
fn measure_then_apply_equals_auto() {
    for regions in [true, false] {
        let mut native = demo();
        let mut split = demo();
        let id = split.active().unwrap();
        assert_eq!(native.active(), Some(id));
        let shown = native.execute("develop.auto", &json!({"dryRun": true, "regions": regions})).unwrap();
        native.execute("develop.auto", &json!({"regions": regions})).unwrap();

        let input = auto_input(&split, id, regions, None).unwrap();
        let r = measure_elsewhere(&input);
        assert_eq!(serde_json::to_value(&r).unwrap(), shown, "the same values, measured without the session (regions {regions})");
        split.execute("develop.applyAutoValues", &apply_params(id, &r, &input.basis)).unwrap();
        assert_eq!(dev(&split), dev(&native), "the same settings (regions {regions})");
    }
}

#[test]
fn a_stale_basis_is_refused_and_changes_nothing() {
    let mut s = demo();
    let id = s.active().unwrap();
    let input = auto_input(&s, id, true, None).unwrap();
    let r = measure_elsewhere(&input);
    // the photo's settings change while the measure is away
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    assert_ne!(auto_basis(&s, id).as_deref(), Some(input.basis.as_str()));
    let edited = dev(&s);
    assert!(s.execute("develop.applyAutoValues", &apply_params(id, &r, &input.basis)).is_err(), "the settings changed");
    assert_eq!(dev(&s), edited, "a refused apply writes nothing");
    // a measure made against what is there now applies
    let fresh = auto_basis(&s, id).unwrap();
    s.execute("develop.applyAutoValues", &apply_params(id, &r, &fresh)).unwrap();
    assert_ne!(dev(&s), edited);
    // a basis that was never one, and a photo that is not there
    assert!(s.execute("develop.applyAutoValues", &apply_params(id, &r, "bogus")).is_err());
    assert!(s.execute("develop.applyAutoValues", &apply_params(PhotoId(u64::MAX), &r, &fresh)).is_err());
}

#[test]
fn apply_is_one_undo_step() {
    let mut s = demo();
    let id = s.active().unwrap();
    let before = dev(&s);
    let input = auto_input(&s, id, true, None).unwrap();
    let r = measure_elsewhere(&input);
    s.execute("develop.applyAutoValues", &apply_params(id, &r, &input.basis)).unwrap();
    let applied = dev(&s);
    assert_ne!(applied, before);
    assert_eq!(applied.light.exposure, r.global.exposure);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(dev(&s), before, "one undo takes the values and the masks back together");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(dev(&s), applied);
}

#[test]
fn one_slider_applies_alone() {
    let mut s = demo();
    let id = s.active().unwrap();
    let before = dev(&s);
    let input = auto_input(&s, id, false, Some("light.exposure".into())).unwrap();
    let r = measure_elsewhere(&input);
    let mut p = apply_params(id, &r, &input.basis);
    p["control"] = json!("light.exposure");
    let out = s.execute("develop.applyAutoValues", &p).unwrap();
    assert_eq!(out["control"], "light.exposure");
    let now = dev(&s);
    assert_eq!(now.light.exposure, out["value"].as_f64().unwrap());
    assert_eq!(now.light.exposure, r.global.exposure);
    assert_eq!((now.light.contrast, now.light.highlights, now.light.shadows), (before.light.contrast, before.light.highlights, before.light.shadows));
    assert_eq!(now.masks, before.masks, "no masks: only that slider moved");
    // a control Auto does not set is refused
    p["control"] = json!("crop.angle");
    assert!(s.execute("develop.applyAutoValues", &p).is_err());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(dev(&s), before);
}
