//! Set Poster Frame from the player (issue #76): the button dispatches `video.setPoster` with the time the player is at, the Photo
//! menu item takes the same time, and neither has a time to take before the clip is loaded in the player. No ffmpeg is needed: a
//! seek puts the clock where it is asked even when no frame can be fetched.

use std::time::Duration;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::menubar::{MenuNode, menu_bar, run_item};
use crate::tests_video_player::app_with_video;

const T: Duration = Duration::from_secs(20);

fn poster(h: &Headless, id: PhotoId) -> Option<f64> {
    h.app.session.catalog.photo(id).and_then(|p| p.develop.poster_time)
}

fn click(h: &mut Headless, widget: &str) {
    let r = h.request("ui.clickWidget", json!({"id": widget}), T);
    assert_eq!(r["ok"], true, "{widget}: {r}");
    h.step();
}

fn photo_menu_item(h: &Headless, id: &str) -> Option<(String, bool)> {
    fn find(nodes: &[MenuNode], want: &str) -> Option<(String, bool)> {
        nodes.iter().find_map(|n| match n {
            MenuNode::Item { id, label, enabled, .. } if id == want => Some((label.clone(), *enabled)),
            MenuNode::Submenu { children, .. } => find(children, want),
            _ => None,
        })
    }
    let photo = menu_bar(&h.app).into_iter().find(|(title, _)| title == "Photo")?.1;
    find(&photo, id)
}

#[test]
fn the_button_sets_the_poster_to_the_time_the_player_is_at() {
    let (mut h, id) = app_with_video();
    // scrubbed to 7.25 s (the default poster of a 20 s clip is 2 s)
    let _ = h.app.run("video.seek", json!({"id": id.0, "t": 7.25}));
    h.step();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:videoPoster"), "the button is in the player");
    click(&mut h, "button:videoPoster");
    assert_eq!(poster(&h, id), Some(7.25), "video.setPoster went out with the current time");
    let photo = h.app.session.catalog.photo(id).unwrap();
    assert!(photo.history.last().is_some_and(|s| s.label == "Set Poster Frame"), "one undo step");

    // a later time replaces it; the clock is where the scrubber left it
    let _ = h.app.run("video.seek", json!({"t": 12.5}));
    h.step();
    click(&mut h, "button:videoPoster");
    assert_eq!(poster(&h, id), Some(12.5));
}

#[test]
fn the_button_waits_for_a_clip_with_a_current_time() {
    let (mut h, id) = app_with_video();
    assert!(h.app.session.video.photo().is_none(), "nothing is loaded in the player yet");
    click(&mut h, "button:videoPoster");
    assert_eq!(poster(&h, id), None, "no current time, no poster change");
}

#[test]
fn set_poster_frame_is_in_the_photo_menu_and_takes_the_players_time() {
    let (mut h, id) = app_with_video();
    let Some((label, enabled)) = photo_menu_item(&h, "video.setPoster") else { panic!("Set Poster Frame is not in the Photo menu") };
    assert_eq!(label, "Set Poster Frame");
    assert!(enabled, "a video is selected");
    // before the clip is in the player the item has no time to take, and says so
    let refused: Result<Value, String> = run_item(&mut h.app, "video.setPoster", json!({}));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(poster(&h, id), None);

    let _ = h.app.run("video.seek", json!({"id": id.0, "t": 9.5}));
    let r = run_item(&mut h.app, "video.setPoster", json!({})).unwrap();
    assert_eq!(r["t"].as_f64(), Some(9.5), "{r}");
    assert_eq!(poster(&h, id), Some(9.5));

    // a still has nothing to set
    let still = h.app.session.catalog.photos().find(|p| p.kind != lightcraft_catalog::MediaKind::Video).map(|p| p.id).unwrap();
    crate::tests_video_player::select(&mut h, still);
    let (_, enabled) = photo_menu_item(&h, "video.setPoster").unwrap();
    assert!(!enabled, "not for a photo");
}
