//! `library.searchText`: local ranking drives the grid, an empty query changes nothing, the
//! ScaleDown result (the only network step, run by the host) applies as a filter patch.

use lightcraft_catalog::{Op, Photo, Source};
use serde_json::json;

use crate::Session;

/// A demo library with one synthetic photo whose metadata matches "surfing on a beach at dusk".
fn session_with_surf_photo() -> (Session, lightcraft_catalog::PhotoId) {
    let mut s = Session::with_demo();
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, "surf.jpg", "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.meta.keywords = vec!["surf".into(), "beach".into()];
    p.meta.caption = "Sunset over the waves".into();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    (s, id)
}

#[test]
fn local_search_ranks_the_expected_photo_first_and_filters_the_grid() {
    let (mut s, id) = session_with_surf_photo();
    let r = s.execute("library.searchText", &json!({"query": "surfing on a beach at dusk"})).unwrap();
    assert_eq!(r["changed"], true);
    assert_eq!(r["top"][0], json!(id.0), "{r}");
    assert!(s.visible().contains(&id));
}

#[test]
fn an_empty_query_changes_nothing() {
    let (mut s, _) = session_with_surf_photo();
    let before = serde_json::to_value(&s.filter).unwrap();
    let r = s.execute("library.searchText", &json!({"query": "  \t "})).unwrap();
    assert_eq!(r["changed"], false);
    let r = s.execute("library.searchText", &json!({})).unwrap();
    assert_eq!(r["changed"], false);
    assert_eq!(serde_json::to_value(&s.filter).unwrap(), before);
}

#[test]
fn no_match_is_an_error_and_leaves_the_filter_alone() {
    let (mut s, _) = session_with_surf_photo();
    let before = serde_json::to_value(&s.filter).unwrap();
    assert!(s.execute("library.searchText", &json!({"query": "zebra crossing"})).is_err());
    assert_eq!(serde_json::to_value(&s.filter).unwrap(), before);
}

#[test]
fn a_scaledown_result_applies_as_a_filter_patch_without_any_network_call() {
    let (mut s, _) = session_with_surf_photo();
    // the host passes the Extract result; the command maps it and fetches nothing
    let found = json!({"entities": [{"type": "keyword", "text": "beach", "confidence": 0.9}]});
    let r = s.execute("library.searchText", &json!({"scaledown": found})).unwrap();
    assert_eq!(r["changed"], true);
    assert_eq!(s.filter.keyword.as_deref(), Some("beach"));
    assert!(s.filter.only.is_empty(), "a hosted search replaces the local match set");
    assert!(s.execute("library.searchText", &json!({"scaledown": "not a result"})).is_err());
}
