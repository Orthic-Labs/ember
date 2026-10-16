//! The video player in the loupe (issue #73): a transport bar on a video and none on a still, Space plays instead of toggling the
//! zoom, the "Video support isn't installed" message, and a clip that leaves the screen being stopped. The engine's playback is
//! tested with its own streams (`lightcraft_engine::tests_video_player`); these drive the real app headless, so they need no ffmpeg.

use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library plus one 20 s video, in the loupe with the video selected: the headless app and the video's id.
pub(crate) fn app_with_video() -> (Headless, PhotoId) {
    let mut session = lightcraft_engine::Session::with_demo();
    let id = session.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/clips/test.mov".into() }, "test.mov", "MOV", 1920, 1080, "2026-10-17T10:00:00");
    p.kind = MediaKind::Video;
    p.duration = Some(20.0);
    session.commit("add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    select(&mut h, id);
    (h, id)
}

pub(crate) fn select(h: &mut Headless, id: PhotoId) {
    h.app.session.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    h.settle(SETTLE);
    h.step();
}

fn has_widget(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn a_still(h: &Headless) -> PhotoId {
    h.app.session.catalog.photos().find(|p| p.kind != MediaKind::Video).map(|p| p.id).unwrap()
}

#[test]
fn a_video_in_the_loupe_has_a_transport_bar_and_a_still_has_none() {
    let (mut h, _) = app_with_video();
    for w in ["video:bar", "video:play", "video:scrub"] {
        assert!(has_widget(&h, w), "{w} is drawn for a video");
    }
    let still = a_still(&h);
    select(&mut h, still);
    for w in ["video:bar", "video:play", "video:scrub"] {
        assert!(!has_widget(&h, w), "{w} is not drawn for a photo");
    }
}

#[test]
fn space_plays_a_video_in_the_loupe_instead_of_toggling_the_zoom() {
    let (mut h, id) = app_with_video();
    let zoom = h.app.ui.zoom;
    let r = h.request("ui.key", json!({"key": "Space"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.video.photo(), Some(id), "Space went to the player");
    assert_eq!(h.app.ui.zoom, zoom, "and not to the zoom toggle");

    // on a still it is still the zoom toggle, and nothing is loaded in the player
    h.app.session.video_stop();
    let still = a_still(&h);
    select(&mut h, still);
    let r = h.request("ui.key", json!({"key": "Space"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.video.photo(), None);
    assert_ne!(h.app.ui.zoom, zoom, "Space still toggles the zoom on a photo");
}

#[test]
fn without_the_sidecar_the_play_button_says_video_support_is_not_installed() {
    if lightcraft_engine::video_player::sidecar_available() {
        eprintln!("SKIP: the ffmpeg sidecar is installed here");
        return;
    }
    let (mut h, _) = app_with_video();
    assert!(!has_widget(&h, "video:message"), "nothing to say before it is tried");
    let r = h.request("ui.clickWidget", json!({"id": "video:play"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has_widget(&h, "video:message"), "the message is on screen");
    assert!(h.painted_text().iter().any(|t| t == "Video support isn't installed"), "{:?}", h.painted_text());
    assert_eq!(h.app.session.video.error().map(|e| e.unavailable), Some(true));
}

#[test]
fn a_clip_that_leaves_the_screen_is_stopped() {
    let (mut h, id) = app_with_video();
    let _ = h.app.session.execute("video.seek", &json!({"id": id.0, "t": 5.0}));
    assert_eq!(h.app.session.video.photo(), Some(id), "the clip is loaded in the player");
    h.step();
    assert_eq!(h.app.session.video.photo(), Some(id), "and stays while it is in the loupe");
    h.app.ui.view = ViewMode::PhotoGrid;
    h.step();
    assert_eq!(h.app.session.video.photo(), None, "leaving the loupe stops it");

    let _ = h.app.session.execute("video.seek", &json!({"id": id.0, "t": 5.0}));
    h.app.ui.view = ViewMode::Detail;
    let still = a_still(&h);
    select(&mut h, still);
    assert_eq!(h.app.session.video.photo(), None, "another photo stops it too");
}

#[test]
fn set_in_and_set_out_take_the_players_time_once_the_clip_is_loaded_there() {
    let (mut h, id) = app_with_video();
    let trim = |h: &Headless| {
        let d = &h.app.session.catalog.photo(id).unwrap().develop;
        (d.trim_in, d.trim_out)
    };
    // scrubbed to 7.25 s (the poster frame, where Set In went before there was a player, is at 2 s)
    let _ = h.app.run("video.seek", json!({"id": id.0, "t": 7.25}));
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "loupe:trimIn"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(trim(&h), (Some(7.25), None));
    let _ = h.app.run("video.seek", json!({"t": 12.5}));
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "loupe:trimOut"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(trim(&h), (Some(7.25), Some(12.5)));
}

#[test]
fn the_transport_bar_sits_above_the_trim_row() {
    let (h, _) = app_with_video();
    let rect = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap();
    let (bar, trim) = (rect("video:bar"), rect("loupe:trim"));
    assert!(bar.bottom() <= trim.top(), "the bar {bar:?} must not cover the trim row {trim:?}");
}
