//! "Not this person" for a suggested face (`Op::SetFaceRejection`, format version 9).
//!
//! Scenarios, in the words of someone sorting faces by hand:
//!
//! * When I say a face is not Ana, that pair is remembered, however I typed the name later.
//! * The same name on another face of the same photo, or on another photo, is not affected.
//! * Saying it again changes nothing; undo puts back what there was before.
//! * A photo that is not in the library is refused and nothing changes.
//! * The rejections survive saving and reopening the library, and a library saved before they
//!   existed opens with none.

use crate::*;
use lightcraft_meta::Rect;

fn library() -> (Catalog, PhotoId) {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: "/pics/a.jpg".into() }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    (c, id)
}

fn face() -> Rect {
    Rect { x0: 0.25, y0: 0.1, x1: 0.5, y1: 0.4 }
}

fn reject(c: &mut Catalog, photo: PhotoId, rect: Rect, person: &str, rejected: bool) -> Op {
    c.apply(Op::SetFaceRejection { photo, rect, person: person.into(), rejected }).unwrap()
}

#[test]
fn a_rejected_pair_is_remembered_whatever_the_name_case_or_box_jitter() {
    let (mut c, id) = library();
    reject(&mut c, id, face(), "  Ana Lima ", true);
    assert!(c.is_face_rejected(id, &face(), "ana lima"));
    assert!(c.is_face_rejected(id, &face(), "ANA LIMA"));
    // a box that differs below the 1/10000 the face index keeps is the same face
    let nudged = Rect { x0: 0.25 + 1e-7, ..face() };
    assert!(c.is_face_rejected(id, &nudged, "Ana Lima"));
}

#[test]
fn other_people_other_faces_and_other_photos_are_not_affected() {
    let (mut c, id) = library();
    reject(&mut c, id, face(), "Ana Lima", true);
    assert!(!c.is_face_rejected(id, &face(), "Bruno"), "another person on the same face");
    assert!(!c.is_face_rejected(id, &Rect { x0: 0.6, y0: 0.1, x1: 0.8, y1: 0.4 }, "Ana Lima"), "another face");
    let other = c.alloc_photo_id();
    let p = Photo::new(other, Source::File { path: "/pics/b.jpg".into() }, "b.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    assert!(!c.is_face_rejected(other, &face(), "Ana Lima"), "the same box on another photo");
}

#[test]
fn saying_it_twice_changes_nothing_and_undo_restores_the_earlier_state() {
    let (mut c, id) = library();
    // (compared as snapshots: `revision` counts every applied op, so the structs differ by it)
    let before = c.to_snapshot();
    let undo = reject(&mut c, id, face(), "Ana Lima", true);
    let once = c.to_snapshot();
    let undo_again = reject(&mut c, id, face(), "Ana Lima", true);
    assert_eq!(c.to_snapshot(), once, "a second rejection is a no-op");
    // undoing the second one puts back "already rejected", undoing the first clears it
    c.apply(undo_again).unwrap();
    assert_eq!(c.to_snapshot(), once);
    c.apply(undo).unwrap();
    assert_eq!(c.to_snapshot(), before);
    assert!(!c.is_face_rejected(id, &face(), "Ana Lima"));
}

#[test]
fn clearing_a_rejection_removes_it() {
    let (mut c, id) = library();
    reject(&mut c, id, face(), "Ana Lima", true);
    reject(&mut c, id, face(), "Ana Lima", false);
    assert!(!c.is_face_rejected(id, &face(), "Ana Lima"));
}

#[test]
fn a_photo_that_is_not_in_the_library_is_refused_and_nothing_changes() {
    let (mut c, _) = library();
    let before = c.to_snapshot();
    let missing = PhotoId(9_999);
    assert!(c.apply(Op::SetFaceRejection { photo: missing, rect: face(), person: "Ana".into(), rejected: true }).is_err());
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn rejections_survive_a_snapshot_and_an_older_snapshot_opens_with_none() {
    let (mut c, id) = library();
    reject(&mut c, id, face(), "Ana Lima", true);
    let saved = c.to_snapshot();
    let reopened = Catalog::from_snapshot(&saved).unwrap();
    assert!(reopened.is_face_rejected(id, &face(), "ana lima"));

    // a snapshot from before version 9 has no such field: it still opens, with no rejections
    let (plain, _) = library();
    let old = plain.to_snapshot();
    assert!(!old.contains("face_rejections"), "an empty list is not written");
    let opened = Catalog::from_snapshot(&old).unwrap();
    assert!(!opened.is_face_rejected(id, &face(), "Ana Lima"));
}
