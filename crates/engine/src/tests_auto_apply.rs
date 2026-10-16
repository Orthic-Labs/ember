//! Auto in two phases (#192): the measure runs without the session, the apply is one undo step on
//! the session's thread and refuses a measure whose photo has changed since.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::auto::regions::RegionAuto;
use serde_json::{Value, json};

use crate::Session;
use crate::auto_measure::{AutoInput, AutoKind, AutoMeasured, auto_basis, auto_input, auto_input_for, measure_auto, measure_kind, with_lens_pick};
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

// ---- the other Auto commands: measured elsewhere, applied by the command itself (#192)

fn dev_of(s: &Session, id: PhotoId) -> DevelopSettings {
    (*s.develop_of(id).unwrap()).clone()
}

/// Phase 1 of any kind the way a render worker does it: its own decode at the level the kind reads,
/// the input's settings and facts, no session.
fn measure_kind_elsewhere(input: &AutoInput) -> AutoMeasured {
    let mut media = MediaCache::default();
    let src = media.origin_ref(&input.origin, input.kind.level().max_edge()).load_source().unwrap();
    let info = with_lens_pick(src.info_or(input.info.clone()), input.lens_pick);
    let m = measure_kind(input.kind, &src.image, &info, &input.settings, &input.faces, input.regions);
    // through JSON, as it crosses the worker boundary
    serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap()
}

/// Hand the session what a host measured elsewhere for `ids`.
fn prepare(s: &mut Session, ids: &[PhotoId], kind: AutoKind, regions: bool) {
    for id in ids {
        let input = auto_input_for(s, *id, kind, regions, None).unwrap();
        let m = measure_kind_elsewhere(&input);
        s.prepare_auto(*id, regions, input.basis, m);
    }
}

/// A host's run of `command`: measure `kind` of each of `ids` elsewhere, hand the values over, run
/// the command, drop what it left.
fn run_with_measures(s: &mut Session, command: &str, params: &Value, ids: &[PhotoId], kind: AutoKind, regions: bool) -> crate::Result<Value> {
    prepare(s, ids, kind, regions);
    let r = s.execute(command, params);
    s.clear_prepared_auto();
    r
}

fn select(s: &mut Session, ids: &[PhotoId]) {
    s.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
}

/// A demo session with its first `n` photos selected (the first one active).
fn demo_selecting(n: usize) -> (Session, Vec<PhotoId>) {
    let mut s = demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(n).collect();
    assert_eq!(ids.len(), n, "the demo library has {n} photos");
    select(&mut s, &ids);
    (s, ids)
}

fn edit_exposure(s: &mut Session) {
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
}

#[test]
fn auto_batch_measured_elsewhere_equals_auto_batch_as_one_undo_step() {
    for regions in [true, false] {
        let (mut native, ids) = demo_selecting(3);
        let (mut split, _) = demo_selecting(3);
        let before: Vec<_> = ids.iter().map(|i| dev_of(&split, *i)).collect();
        let params = json!({"regions": regions});
        let want = native.execute("develop.autoBatch", &params).unwrap();
        let got = run_with_measures(&mut split, "develop.autoBatch", &params, &ids, AutoKind::Tone, regions).unwrap();
        assert_eq!(got, want, "the same answer (regions {regions})");
        for id in &ids {
            assert_eq!(dev_of(&split, *id), dev_of(&native, *id), "photo {} (regions {regions})", id.0);
        }
        assert!(ids.iter().zip(&before).any(|(i, b)| dev_of(&split, *i) != *b), "Auto changed something");
        split.execute("edit.undo", &json!({})).unwrap();
        for (id, b) in ids.iter().zip(&before) {
            assert_eq!(dev_of(&split, *id), *b, "one undo takes the whole batch back (regions {regions})");
        }
    }
}

#[test]
fn auto_batch_refuses_a_stale_photo_and_writes_none_of_the_batch() {
    let (mut s, ids) = demo_selecting(3);
    prepare(&mut s, &ids, AutoKind::Tone, true);
    // the last photo changes while the measures are away
    select(&mut s, &ids[2..]);
    edit_exposure(&mut s);
    select(&mut s, &ids);
    let after_edit: Vec<_> = ids.iter().map(|i| dev_of(&s, *i)).collect();
    let r = s.execute("develop.autoBatch", &json!({}));
    s.clear_prepared_auto();
    assert!(r.is_err(), "the third photo changed");
    for (id, d) in ids.iter().zip(&after_edit) {
        assert_eq!(dev_of(&s, *id), *d, "a refused batch writes nothing (photo {})", id.0);
    }
}

#[test]
fn auto_bw_mix_measured_elsewhere_equals_auto_bw_mix_and_refuses_a_stale_photo() {
    let mut native = demo();
    let mut split = demo();
    let id = split.active().unwrap();
    let before = dev_of(&split, id);
    let want = native.execute("develop.autoBwMix", &json!({})).unwrap();
    let got = run_with_measures(&mut split, "develop.autoBwMix", &json!({}), &[id], AutoKind::BwMix, false).unwrap();
    assert_eq!(got, want);
    assert_eq!(dev_of(&split, id), dev_of(&native, id));
    split.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(dev_of(&split, id), before, "one undo");

    prepare(&mut split, &[id], AutoKind::BwMix, false);
    edit_exposure(&mut split);
    let edited = dev_of(&split, id);
    assert!(split.execute("develop.autoBwMix", &json!({})).is_err());
    split.clear_prepared_auto();
    assert_eq!(dev_of(&split, id), edited, "a refused apply writes nothing");
}

#[test]
fn auto_noise_measured_elsewhere_equals_auto_noise_dry_run_and_stale() {
    let mut native = demo();
    let mut split = demo();
    let id = split.active().unwrap();
    let before = dev_of(&split, id);
    // a dry run answers with the numbers and writes nothing
    let want = native.execute("develop.autoNoise", &json!({"dryRun": true})).unwrap();
    let shown = run_with_measures(&mut split, "develop.autoNoise", &json!({"dryRun": true}), &[id], AutoKind::Noise, false).unwrap();
    assert_eq!(shown, want);
    assert_eq!(dev_of(&split, id), before, "a dry run writes nothing");
    let want = native.execute("develop.autoNoise", &json!({})).unwrap();
    let got = run_with_measures(&mut split, "develop.autoNoise", &json!({}), &[id], AutoKind::Noise, false).unwrap();
    assert_eq!(got, want);
    assert_eq!(dev_of(&split, id), dev_of(&native, id));
    split.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(dev_of(&split, id), before);

    prepare(&mut split, &[id], AutoKind::Noise, false);
    edit_exposure(&mut split);
    let edited = dev_of(&split, id);
    assert!(split.execute("develop.autoNoise", &json!({})).is_err());
    split.clear_prepared_auto();
    assert_eq!(dev_of(&split, id), edited);
}

#[test]
fn white_balance_auto_measured_elsewhere_equals_white_balance_auto_and_stale() {
    for (command, params) in [("develop.wb", json!({"mode": "auto"})), ("develop.autoWhiteBalance", json!({}))] {
        let mut native = demo();
        let mut split = demo();
        let id = split.active().unwrap();
        let before = dev_of(&split, id);
        let want = native.execute(command, &params).unwrap();
        let got = run_with_measures(&mut split, command, &params, &[id], AutoKind::WhiteBalance, false).unwrap();
        assert_eq!(got, want, "{command}");
        assert_eq!(dev_of(&split, id), dev_of(&native, id), "{command}");
        split.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(dev_of(&split, id), before, "{command}: one undo");

        prepare(&mut split, &[id], AutoKind::WhiteBalance, false);
        edit_exposure(&mut split);
        let edited = dev_of(&split, id);
        assert!(split.execute(command, &params).is_err(), "{command}");
        split.clear_prepared_auto();
        assert_eq!(dev_of(&split, id), edited, "{command}");
    }
}

#[test]
fn a_preset_with_auto_measured_elsewhere_equals_the_native_apply_and_stale() {
    let setup = || {
        let (mut s, ids) = demo_selecting(2);
        let made = s.execute("preset.create", &json!({"name": "Auto Detail", "groups": ["detail"], "auto": true})).unwrap();
        let pid = made["id"].as_str().unwrap().to_string();
        (s, ids, pid)
    };
    let (mut native, ids, pid) = setup();
    let (mut split, _, _) = setup();
    let before: Vec<_> = ids.iter().map(|i| dev_of(&split, *i)).collect();
    let params = json!({"id": pid});
    native.execute("preset.apply", &params).unwrap();
    run_with_measures(&mut split, "preset.apply", &params, &ids, AutoKind::Tone, false).unwrap();
    for id in &ids {
        assert_eq!(dev_of(&split, *id), dev_of(&native, *id), "photo {}", id.0);
    }
    assert!(ids.iter().zip(&before).any(|(i, b)| dev_of(&split, *i) != *b), "Auto changed something");
    split.execute("edit.undo", &json!({})).unwrap();
    for (id, b) in ids.iter().zip(&before) {
        assert_eq!(dev_of(&split, *id), *b, "one undo for the whole batch");
    }

    prepare(&mut split, &ids, AutoKind::Tone, false);
    edit_exposure(&mut split);
    let edited: Vec<_> = ids.iter().map(|i| dev_of(&split, *i)).collect();
    assert!(split.execute("preset.apply", &params).is_err(), "the active photo changed");
    split.clear_prepared_auto();
    for (id, d) in ids.iter().zip(&edited) {
        assert_eq!(dev_of(&split, *id), *d, "a refused apply writes nothing");
    }
}

#[test]
fn auto_dry_run_measured_elsewhere_answers_with_the_values_and_writes_nothing() {
    for command in ["develop.auto", "develop.autoTone"] {
        let mut native = demo();
        let mut split = demo();
        let id = split.active().unwrap();
        let before = dev_of(&split, id);
        let params = json!({"dryRun": true});
        let want = native.execute(command, &params).unwrap();
        let got = run_with_measures(&mut split, command, &params, &[id], AutoKind::Tone, true).unwrap();
        assert_eq!(got, want, "{command}");
        assert_eq!(dev_of(&split, id), before, "{command}: measure only");
        // a measure made for other region options is not used: the command measures for itself
        let got = run_with_measures(&mut split, command, &params, &[id], AutoKind::Tone, false).unwrap();
        assert_eq!(got, want, "{command}");
        // stale
        prepare(&mut split, &[id], AutoKind::Tone, true);
        edit_exposure(&mut split);
        assert!(split.execute(command, &params).is_err(), "{command}");
        split.clear_prepared_auto();
    }
}

#[test]
fn a_prepared_measure_is_used_once_and_cleared() {
    let mut s = demo();
    let id = s.active().unwrap();
    prepare(&mut s, &[id], AutoKind::BwMix, false);
    s.execute("develop.autoBwMix", &json!({})).unwrap();
    // used up: taking it again finds none, and nothing is stale
    assert!(s.take_prepared_auto(id, AutoKind::BwMix, false).unwrap().is_none());
    prepare(&mut s, &[id], AutoKind::Noise, false);
    s.clear_prepared_auto();
    assert!(s.take_prepared_auto(id, AutoKind::Noise, false).unwrap().is_none());
}
