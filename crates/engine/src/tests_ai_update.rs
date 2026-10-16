//! AI mask updates (Update AI Settings): a mask an older algorithm made, or a crop changed under, is stale;
//! Update restamps it as one undo step; a current mask is left alone; a batch covers the selection.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{AI_MASK_VERSION, Mask, MaskComponent, MaskOp, MaskShape, geometry_stamp};
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
    assert!(!m.ai_stale(geometry_stamp(&s.develop_of(id).unwrap_or_default())));
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
fn a_crop_change_makes_a_stamped_ai_mask_stale_but_an_unstamped_one_is_not() {
    let mut s = demo();
    let id = s.visible_cloned()[0];
    select(&mut s, &[id]);
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let mut stamped = subject(AI_MASK_VERSION);
    stamped.ai_geometry = Some(geometry_stamp(&d));
    let mut unstamped = subject(AI_MASK_VERSION);
    unstamped.id = 2;
    d.masks = vec![stamped, unstamped];
    s.set_develop(id, d, "setup").unwrap();
    assert_eq!(s.execute("develop.updateAi", &json!({"dryRun": true})).unwrap()["stale"], 0, "same geometry: current");
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    d.crop.flip_h = !d.crop.flip_h;
    s.set_develop(id, d, "flip").unwrap();
    let r = s.execute("develop.updateAi", &json!({"dryRun": true})).unwrap();
    assert_eq!(r["stale"], 1, "the crop changed under the stamped mask; the unstamped one is never geometry-stale");
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
    assert_eq!(lightcraft_develop::ai_stale_in(&s.develop_of(id).unwrap_or_default()), 0);
    set_masks(&mut s, id, vec![subject(0)]);
    assert_eq!(lightcraft_develop::ai_stale_in(&s.develop_of(id).unwrap_or_default()), 1, "the badge shows");
    s.execute("develop.updateAi", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(lightcraft_develop::ai_stale_in(&s.develop_of(id).unwrap_or_default()), 0, "the badge clears");
}
