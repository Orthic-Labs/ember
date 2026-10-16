//! Local natural-language search: stems, synonyms, stop words, ranking on a small synthetic catalog.

use super::*;

fn add(c: &mut Catalog, name: &str, keywords: &[&str], caption: &str, captured: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.captured = Some(captured.to_string());
    p.meta.keywords = keywords.iter().map(|k| k.to_string()).collect();
    p.meta.caption = caption.to_string();
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

#[test]
fn stems_fold_plurals_and_endings() {
    assert_eq!(search::stem("dogs"), "dog");
    assert_eq!(search::stem("puppies"), "puppy");
    assert_eq!(search::stem("beaches"), "beach");
    assert_eq!(search::stem("walking"), "walk");
    assert_eq!(search::stem("glass"), "glass", "double s is not a plural");
    assert_eq!(search::stem(""), "");
}

#[test]
fn stop_words_and_empty_queries_search_nothing() {
    assert!(search::terms("").is_empty());
    assert!(search::terms("  the a of, photos ").is_empty());
    assert_eq!(search::terms("photos of dogs").len(), 1, "only the subject is searched");
    let mut c = Catalog::new();
    add(&mut c, "a.jpg", &["dog"], "", "2026-04-01T10:00:00");
    assert!(search::rank(&c, "").is_empty());
    assert!(search::rank(&c, "on the at").is_empty());
}

/// "dog on a beach at sunset": the photo with a dog and a beach that is also a dusk photo comes first.
#[test]
fn the_expected_photo_ranks_first() {
    let mut c = Catalog::new();
    let dog_beach_dusk = add(&mut c, "dusk.jpg", &["dogs", "beach", "travel|portugal"], "Golden dusk on the shore", "2026-04-01T20:00:00");
    let beach_sunset = add(&mut c, "sunset.jpg", &["sunset", "beach"], "", "2026-05-01T20:00:00");
    let dog_park = add(&mut c, "park.jpg", &["dog", "park"], "Puppy in the snow", "2026-06-01T12:00:00");
    let city = add(&mut c, "city.jpg", &["city", "street"], "Rain on the tram", "2025-01-01T12:00:00");
    let hits = search::rank(&c, "dog on a beach at sunset");
    let order: Vec<PhotoId> = hits.iter().map(|(id, _)| *id).collect();
    assert_eq!(order.first(), Some(&dog_beach_dusk), "{hits:?}");
    assert_eq!(order.get(1), Some(&beach_sunset));
    assert_eq!(order.get(2), Some(&dog_park));
    assert!(!order.contains(&city), "no word matches");
    assert!(hits.windows(2).all(|w| w[0].1 >= w[1].1), "best first");
}

#[test]
fn people_places_and_years_are_searched() {
    use lightcraft_meta::{Rect, Region, RegionKind};
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, "jane.jpg", "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.captured = Some("2026-07-04T10:00:00".to_string());
    p.meta.regions = vec![Region {
        rect: Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
        kind: RegionKind::Face,
        name: Some("Jane Doe".to_string()),
        description: None,
    }];
    p.meta.city = "Lisbon".to_string();
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    add(&mut c, "other.jpg", &[], "", "2019-07-04T10:00:00");
    assert_eq!(search::rank(&c, "jane").first().map(|(id, _)| *id), Some(id));
    assert_eq!(search::rank(&c, "lisbon 2026").first().map(|(id, _)| *id), Some(id));
    assert_eq!(search::rank(&c, "2026").len(), 1, "the year matches the capture date only");
}

#[test]
fn synonyms_match_both_ways_and_deleted_photos_never_match() {
    let mut c = Catalog::new();
    let a = add(&mut c, "a.jpg", &["Kitten"], "", "2026-04-01T10:00:00");
    let b = add(&mut c, "b.jpg", &["cat"], "", "2026-04-02T10:00:00");
    let found: Vec<PhotoId> = search::rank(&c, "cats").iter().map(|(id, _)| *id).collect();
    assert!(found.contains(&a) && found.contains(&b), "{found:?}");
    let gone = add(&mut c, "gone.jpg", &["cat"], "", "2026-04-03T10:00:00");
    c.apply(Op::SetDeleted { id: gone, deleted: true }).unwrap();
    assert!(!search::rank(&c, "cat").iter().any(|(id, _)| *id == gone));
}

#[test]
fn score_matches_rank() {
    let mut c = Catalog::new();
    let id = add(&mut c, "a.jpg", &["dog"], "", "2026-04-01T10:00:00");
    let photo = c.photo(id).unwrap().clone();
    assert!(search::score(&photo, "dog") > 0);
    assert_eq!(search::score(&photo, "boat"), 0);
}
