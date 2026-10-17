//! AI mask updates (Update AI Settings): a mask an older algorithm made, or a crop changed under, is stale;
//! Update restamps it as one undo step; a current mask is left alone; a batch covers the selection.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{AI_MASK_VERSION, DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, NO_AI_MODEL, geometry_stamp};
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    Session::with_demo()
}

/// A Subject selection stamped with `ai_version` (no geometry stamp).
fn subject(ai_version: u32) -> Mask {
    Mask {
        id: 1,
        name: "Subject".into(),
        components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Subject }],
        ai_version,
        ..Default::default()
    }
}

fn set_masks(s: &mut Session, id: PhotoId, masks: Vec<Mask>) {
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    d.masks = masks;
    s.set_develop(id, d, "setup").unwrap();
}

fn masks_of(s: &Session, id: PhotoId) -> Vec<Mask> {
    s.develop_of(id).unwrap_or_default().masks.clone()
}

fn select(s: &mut Session, ids: &[PhotoId]) {
    s.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
}

/// Stamp `m` as computed now for photo `id`: its source and its geometry.
fn stamped(s: &Session, id: PhotoId, mut m: Mask) -> Mask {
    let (source, geometry) = s.ai_stamps(id);
    m.restamp(source, geometry, s.ai_model_now());
    m
}

/// An Object selection (a stored segmentation the model has not computed yet).
fn object() -> Mask {
    Mask {
        id: 2,
        name: "Object".into(),
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
        }],
        ..Default::default()
    }
}

#[test]
fn a_mask_from_an_older_algorithm_is_reported_stale() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    set_masks(&mut s, id, vec![subject(0)]);
    let r = s.execute("develop.updateAi", &json!({"dryRun": true})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["changed"].as_u64()), (Some(1), Some(1)));
    assert_eq!(masks_of(&s, id)[0].ai_version, 0, "a dry run changes nothing");
}

#[test]
fn update_restamps_the_stale_mask_as_one_undo_step() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    set_masks(&mut s, id, vec![subject(0)]);
    let r = s.execute("develop.updateAi", &json!({})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["updated"].as_u64(), r["pending"].as_u64()), (Some(1), Some(1), Some(0)));
    let m = masks_of(&s, id).remove(0);
    assert_eq!(m.ai_version, AI_MASK_VERSION);
    assert_eq!(m.ai_geometry, Some(geometry_stamp(&s.develop_of(id).unwrap_or_default())));
    assert!(!m.ai_stale(s.ai_source_of(id), geometry_stamp(&s.develop_of(id).unwrap_or_default()), s.ai_model_now()));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(masks_of(&s, id)[0].ai_version, 0, "one undo restores the stale stamp");
}

#[test]
fn a_current_mask_is_left_alone() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    set_masks(&mut s, id, vec![subject(AI_MASK_VERSION)]);
    let before = (*s.develop_of(id).unwrap_or_default()).clone();
    let commits = s.commits();
    let r = s.execute("develop.updateAi", &json!({})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["changed"].as_u64()), (Some(0), Some(0)));
    assert_eq!(*s.develop_of(id).unwrap_or_default(), before);
    assert_eq!(s.commits(), commits, "nothing stale, no undo step");
}

#[test]
fn update_over_a_selection_changes_only_the_selected_photos_in_one_step() {
    let mut s = demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    for id in &ids {
        set_masks(&mut s, *id, vec![subject(0)]);
    }
    select(&mut s, &ids[..2]);
    let r = s.execute("develop.updateAi", &json!({})).unwrap();
    assert_eq!(r["changed"], 2);
    assert_eq!(masks_of(&s, ids[0])[0].ai_version, AI_MASK_VERSION);
    assert_eq!(masks_of(&s, ids[1])[0].ai_version, AI_MASK_VERSION);
    assert_eq!(masks_of(&s, ids[2])[0].ai_version, 0, "a photo outside the selection stays stale");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(masks_of(&s, ids[0])[0].ai_version, 0);
    assert_eq!(masks_of(&s, ids[1])[0].ai_version, 0);
}

#[test]
fn an_object_mask_needs_the_model_and_stays_stale() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let object = Mask {
        id: 1,
        name: "Object".into(),
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 },
        }],
        ai_version: 0,
        ..Default::default()
    };
    set_masks(&mut s, id, vec![object]);
    let r = s.execute("develop.updateAi", &json!({})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["updated"].as_u64(), r["pending"].as_u64()), (Some(1), Some(0), Some(1)));
    assert_eq!(masks_of(&s, id)[0].ai_version, 0);
}

#[test]
fn a_source_or_model_change_stales_an_ai_mask_but_a_slider_does_not() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let m = stamped(&s, id, subject(AI_MASK_VERSION));
    set_masks(&mut s, id, vec![m]);
    let d = s.develop_of(id).unwrap_or_default();
    let (source, g) = s.ai_stamps(id);
    let m = masks_of(&s, id).remove(0);
    assert!(!m.ai_stale(source, g, s.ai_model_now()), "freshly stamped");
    assert_eq!(s.ai_stale_count(id), 0);
    let mut light = (*d).clone();
    light.light.exposure = 0.5;
    assert!(!m.ai_stale(source, geometry_stamp(&light), s.ai_model_now()), "an exposure slider is not a geometry change");
    assert!(m.ai_stale(source ^ 1, g, s.ai_model_now()), "a changed source file makes it stale");
    let mut older = m.clone();
    older.ai_version = 0;
    assert!(older.ai_stale(source, g, s.ai_model_now()), "an older model version makes it stale");
}

#[test]
fn a_crop_or_straighten_stales_no_ai_mask_and_a_transform_or_flip_stales_only_a_segmentation() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let sel = stamped(&s, id, subject(AI_MASK_VERSION));
    let seg = stamped(&s, id, object());
    set_masks(&mut s, id, vec![sel, seg]);
    let d = (*s.develop_of(id).unwrap_or_default()).clone();
    let (source, _) = s.ai_stamps(id);
    let masks = masks_of(&s, id);
    let (sel, seg) = (&masks[0], &masks[1]);
    let mut cropped = d.clone();
    cropped.crop.geometry.angle = 5.0;
    cropped.crop.aspect = Some((1, 1));
    assert!(!sel.ai_stale(source, geometry_stamp(&cropped), s.ai_model_now()), "a semantic selection is evaluated on render");
    assert!(!seg.ai_stale(source, geometry_stamp(&cropped), s.ai_model_now()), "a crop or straighten moves nothing in a stored segmentation");
    let mut transformed = d.clone();
    transformed.geometry.rotate = 3.0;
    assert!(seg.ai_stale(source, geometry_stamp(&transformed), s.ai_model_now()), "a Transform change moves the segmentation");
    let mut flipped = d.clone();
    flipped.crop.flip_h = !flipped.crop.flip_h;
    assert!(seg.ai_stale(source, geometry_stamp(&flipped), s.ai_model_now()), "a flip moves the segmentation");
    assert!(!sel.ai_stale(source, geometry_stamp(&flipped), s.ai_model_now()), "a semantic selection survives a flip");
}

#[test]
fn updating_a_source_change_clears_it_as_one_undo_step() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let mut m = stamped(&s, id, subject(AI_MASK_VERSION));
    m.ai_source = Some(s.ai_source_of(id) ^ 1);
    set_masks(&mut s, id, vec![m]);
    assert_eq!(s.ai_stale_count(id), 1, "a source that changed since the mask was made");
    let commits = s.commits();
    let r = s.execute("develop.updateAi", &json!({})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["updated"].as_u64()), (Some(1), Some(1)));
    assert_eq!(s.ai_stale_count(id), 0, "the update clears the staleness");
    assert_eq!(s.commits(), commits + 1, "one undo step");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.ai_stale_count(id), 1, "one undo restores the staleness");
}

#[test]
fn masks_saved_without_a_stamp_count_as_current_and_current_ones_write_none() {
    let old: Mask = serde_json::from_str(r#"{"id":1,"components":[{"shape":{"kind":"subject"}}]}"#).unwrap();
    assert_eq!(old.ai_version, AI_MASK_VERSION);
    assert_eq!(old.ai_geometry, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("ai_version"), "no churn: the stamp is not written while current");
    let mut stale = old.clone();
    stale.ai_version = 0;
    assert!(serde_json::to_string(&stale).unwrap().contains("\"ai_version\":0"));
}

#[test]
fn the_indicator_clears_when_the_stale_ai_masks_are_updated() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    assert_eq!(s.ai_stale_count(id), 0);
    set_masks(&mut s, id, vec![subject(0)]);
    assert_eq!(s.ai_stale_count(id), 1, "the badge shows");
    s.execute("develop.updateAi", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.ai_stale_count(id), 0, "the badge clears");
}

#[test]
fn update_can_be_limited_to_named_masks() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let other = Mask { id: 3, name: "Sky".into(), ..subject(0) };
    set_masks(&mut s, id, vec![subject(0), other]);
    let r = s.execute("develop.updateAi", &json!({"masks": [3], "dryRun": true})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["updated"].as_u64()), (Some(1), Some(1)), "only the named mask counts");
    s.execute("develop.updateAi", &json!({"masks": [3]})).unwrap();
    let versions = |s: &Session| masks_of(s, id).iter().map(|m| m.ai_version).collect::<Vec<_>>();
    assert_eq!(versions(&s), [0, AI_MASK_VERSION], "only mask 3 is updated");
    assert_eq!(s.ai_stale_count(id), 1, "the other is still stale");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(versions(&s), [0, 0], "one undo step");
    // a mask that is not there updates nothing and leaves no undo step
    let commits = s.commits();
    let r = s.execute("develop.updateAi", &json!({"masks": [99]})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["changed"].as_u64()), (Some(0), Some(0)));
    assert_eq!(s.commits(), commits);
    assert!(s.execute("develop.updateAi", &json!({"masks": ["a"]})).is_err());
    assert!(s.execute("develop.updateAi", &json!({"masks": 3})).is_err());
    // without `masks`: every stale mask, as before
    let r = s.execute("develop.updateAi", &json!({})).unwrap();
    assert_eq!(r["updated"], 2);
}

/// Auto's result for a photo with a sky region.
fn sky_region() -> lightcraft_pipeline::auto::regions::RegionAuto {
    use lightcraft_pipeline::auto::AutoTone;
    use lightcraft_pipeline::auto::regions::{Region, RegionAdjust, RegionAuto};
    let region = RegionAdjust { region: Region::Sky, share: 0.3, adjust: lightcraft_develop::LocalAdjustments::default() };
    RegionAuto { global: AutoTone::default(), regions: vec![region] }
}

/// The photo's Auto region is stamped for its source file and geometry now, so it is current, and a
/// replaced file stales it.
fn assert_auto_region_stamped(s: &Session, id: PhotoId, what: &str) {
    let d = s.develop_of(id).unwrap_or_default();
    let auto: Vec<&Mask> = d.masks.iter().filter(|m| m.auto).collect();
    assert_eq!(auto.len(), 1, "{what}: Auto wrote its sky region");
    let (source, geometry) = s.ai_stamps(id);
    assert_eq!((auto[0].ai_source, auto[0].ai_geometry), (Some(source), Some(geometry)), "{what}");
    assert_eq!(s.ai_stale_count(id), 0, "{what}: current when written");
    assert_eq!(lightcraft_develop::ai_stale_in(&d, source ^ 1, s.ai_model_now()), 1, "{what}: another source file stales it");
}

// issue #65: the regions Auto measures are AI selections like any other, so they carry the source stamp
#[test]
fn auto_measured_regions_are_stamped_with_the_source() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    // Auto Settings (develop.auto, and a measure applied by develop.applyAutoValues)
    let basis = crate::auto_measure::auto_basis(&s, id).unwrap();
    s.execute("develop.applyAutoValues", &json!({"id": id.0, "values": sky_region(), "basis": basis})).unwrap();
    assert_auto_region_stamped(&s, id, "applyAutoValues");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.develop_of(id).unwrap_or_default().masks.iter().all(|m| !m.auto));
    // Auto Settings to Selection
    let basis = crate::auto_measure::auto_basis(&s, id).unwrap();
    s.prepare_auto(id, true, basis, crate::auto_measure::AutoMeasured::Tone(sky_region()));
    s.execute("develop.autoBatch", &json!({})).unwrap();
    s.clear_prepared_auto();
    assert_auto_region_stamped(&s, id, "autoBatch");
}

// masks.recompute (#65): the model marker, the model-change state, and the command's runs.

#[test]
fn an_ai_mask_stores_the_model_it_was_computed_with() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let m = stamped(&s, id, subject(AI_MASK_VERSION));
    assert_eq!(m.ai_model.as_deref(), Some(s.ai_model_now()), "the marker is written when the mask is stamped");
    let saved: Mask = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(saved.ai_model, m.ai_model, "the marker survives a save and reload");
    assert_eq!(saved.ai_source, m.ai_source);
}

#[test]
fn a_model_change_flips_an_ai_mask_to_needs_update_and_a_recompute_clears_it() {
    let mut m = subject(AI_MASK_VERSION);
    assert!(!m.ai_stale(7, 0, "MobileSAM"), "a mask with no recorded model is never stale for its model");
    m.restamp(7, 0, "MobileSAM");
    assert!(!m.ai_stale(7, 0, "MobileSAM"), "current for the model it was computed with");
    assert!(m.ai_stale(7, 0, "SAM 3"), "another model id makes it stale");
    assert!(m.ai_stale(7, 0, NO_AI_MODEL), "no model installed makes it stale");
    let mut d = DevelopSettings::default();
    d.masks = vec![m.clone()];
    assert_eq!(lightcraft_develop::ai_stale_in(&d, 7, "MobileSAM"), 0);
    assert_eq!(lightcraft_develop::ai_stale_in(&d, 7, "SAM 3"), 1, "the photo's count follows the model");
    m.restamp(7, 0, "SAM 3");
    assert!(!m.ai_stale(7, 0, "SAM 3") && m.ai_model.as_deref() == Some("SAM 3"), "a restamp records the new model");
}

#[test]
fn recompute_restamps_a_stale_semantic_mask_as_one_undo_step() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    set_masks(&mut s, id, vec![subject(0)]);
    let r = s.execute("masks.recompute", &json!({})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["recomputed"].as_u64(), r["pending"].as_u64(), r["changed"].as_u64()), (Some(1), Some(1), Some(0), Some(1)));
    let m = masks_of(&s, id).remove(0);
    assert_eq!(m.ai_version, AI_MASK_VERSION);
    assert_eq!(m.ai_model.as_deref(), Some(s.ai_model_now()), "the recomputed mask records the model");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(masks_of(&s, id)[0].ai_version, 0, "one undo restores the stale stamp");
    assert_eq!(masks_of(&s, id)[0].ai_model, None);
}

#[test]
fn recompute_all_also_restamps_current_masks_and_a_dry_run_changes_nothing() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    set_masks(&mut s, id, vec![subject(AI_MASK_VERSION)]);
    let commits = s.commits();
    let r = s.execute("masks.recompute", &json!({})).unwrap();
    assert_eq!((r["stale"].as_u64(), r["recomputed"].as_u64()), (Some(0), Some(0)), "a current mask is left alone by default");
    assert_eq!(s.commits(), commits, "nothing to do, no undo step");
    let r = s.execute("masks.recompute", &json!({"all": true, "dryRun": true})).unwrap();
    assert_eq!((r["recomputed"].as_u64(), r["changed"].as_u64()), (Some(1), Some(1)), "all: the current mask is re-run");
    assert_eq!(masks_of(&s, id)[0].ai_model, None, "a dry run changes nothing");
    s.execute("masks.recompute", &json!({"all": true})).unwrap();
    assert_eq!(masks_of(&s, id)[0].ai_model.as_deref(), Some(s.ai_model_now()));
}

#[test]
fn recompute_leaves_an_object_mask_pending_when_no_model_runs_here() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    set_masks(&mut s, id, vec![object()]);
    let commits = s.commits();
    let before = masks_of(&s, id);
    let r = s.execute("masks.recompute", &json!({"all": true})).unwrap();
    assert_eq!((r["recomputed"].as_u64(), r["pending"].as_u64(), r["changed"].as_u64()), (Some(0), Some(1), Some(0)), "{r}");
    assert_eq!(masks_of(&s, id), before, "the segmentation is untouched without a model");
    assert_eq!(s.commits(), commits, "nothing committed");
}
