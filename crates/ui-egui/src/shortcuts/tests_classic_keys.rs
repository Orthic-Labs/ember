//! The Classic keymap (`app.classicKeymap`), one test per KEYC row of docs/parity-checklist.md: each
//! row's keys dispatch the commands it names. `dispatched` resolves a key the way `handle` does: the
//! Classic layer's keys replace the primary binding on the same key, and aliases yield to the layer.
//! Context rules (grids, Compare) are checked by key presses where a row depends on them.

use egui::{Key, Modifiers};
use serde_json::json;

use super::{ALIASES, CLASSIC_KEYS, CONTEXTUAL, Keymap, bindable, binding, find_bindable, parse};
use crate::LightcraftApp;
use crate::state::{RightPanel, ViewMode};

fn library() -> crate::headless::Headless {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut session = lightcraft_engine::Session::new();
    for id in 1..=4 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-shortcuts/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            100,
            100,
            "2026-10-08",
        );
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    session.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    session.execute("library.select", &json!({"ids": [1]})).unwrap();
    let app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = ViewMode::PhotoGrid;
    for _ in 0..3 {
        h.step();
    }
    h
}

/// A key press as `ui.key` sends it.
fn press(h: &mut crate::headless::Headless, key: &str, cmd: bool, alt: bool, shift: bool) {
    let r = h.request("ui.key", json!({"key": key, "cmd": cmd, "alt": alt, "shift": shift}), std::time::Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
}

/// Every binding the keymap runs with the Classic layer on: the primary bindings and aliases the layer
/// does not take, then the layer's own keys.
fn classic_bindings() -> Vec<(Modifiers, Key, &'static str)> {
    let keymap = Keymap::new();
    let classic: Vec<(Modifiers, Key)> = CLASSIC_KEYS.iter().filter_map(|(sc, _)| parse(sc)).collect();
    let mut out = Vec::new();
    for b in bindable() {
        if let Some((m, k)) = binding(&keymap, b.id, b.default).and_then(parse)
            && !classic.contains(&(m, k))
        {
            out.push((m, k, b.id));
        }
    }
    for (sc, id, _) in ALIASES {
        if let Some((m, k)) = parse(sc)
            && !classic.contains(&(m, k))
        {
            out.push((m, k, *id));
        }
    }
    for (sc, id) in CLASSIC_KEYS {
        if let Some((m, k)) = parse(sc) {
            out.push((m, k, *id));
        }
    }
    out
}

/// The commands the Classic keymap runs for a key, before context rules pick one.
fn dispatched(sc: &str) -> Vec<&'static str> {
    let Some((m, k)) = parse(sc) else { return Vec::new() };
    classic_bindings().into_iter().filter(|(bm, bk, _)| (*bm, *bk) == (m, k)).map(|(_, _, id)| id).collect()
}

/// Each `(shortcut, command id)` pair of a KEYC row dispatches that command with the Classic keymap on.
fn assert_dispatch(row: &str, pairs: &[(&str, &'static str)]) {
    for (sc, id) in pairs {
        let got = dispatched(sc);
        assert!(got.contains(id), "{row}: {sc} runs {id} with the Classic keymap on (got {got:?})");
    }
}

#[test]
fn classic_keymap_has_no_colliding_bindings() {
    let mut keys: Vec<(Modifiers, Key)> = Vec::new();
    for (sc, _) in CLASSIC_KEYS {
        let key = parse(sc).expect("every Classic key parses");
        assert!(!keys.contains(&key), "{sc} is listed twice in CLASSIC_KEYS");
        keys.push(key);
    }
    let bindings = classic_bindings();
    for (i, (m, k, a)) in bindings.iter().enumerate() {
        for (m2, k2, b) in &bindings[i + 1..] {
            if (m, k) == (m2, k2) && a != b {
                let contextual = CONTEXTUAL.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a));
                assert!(contextual, "{a} and {b} both run on {m:?} {k:?} with the Classic keymap on");
            }
        }
    }
}

#[test]
fn classic_keymap_entries_name_existing_commands() {
    for (sc, id) in CLASSIC_KEYS {
        assert!(parse(sc).is_some(), "{sc} parses");
        assert!(find_bindable(id).is_some(), "{sc} runs {id}, which is not a command");
    }
}

/// KEYC-PANELS: Tab, ⇧Tab and F5–F8 (T has no command yet; solo mode is Alt-click, not a key).
#[test]
fn keyc_panels_keys_dispatch_the_panel_commands() {
    assert_dispatch(
        "KEYC-PANELS",
        &[
            ("Tab", "view.panels"),
            ("Shift+Tab", "view.sidePanels"),
            ("F5", "view.topPanel"),
            ("F6", "view.filmstrip"),
            ("F7", "view.leftPanel"),
            ("F8", "view.rightPanel"),
        ],
    );
}

/// KEYC-VIEWS: E, G, C, N, L, F, I, ⇧R and ⌘⌥0 (E, C, L and I come from the Classic layer).
#[test]
fn keyc_views_keys_dispatch_the_view_commands() {
    assert_dispatch(
        "KEYC-VIEWS",
        &[
            ("E", "view.detail"),
            ("G", "view.gridToggle"),
            ("C", "view.compare"),
            ("N", "view.survey"),
            ("L", "view.lightsOut"),
            ("F", "view.fullScreenPreview"),
            ("I", "view.infoOverlay"),
            ("Shift+R", "view.reference"),
            ("Cmd+Alt+0", "view.zoom100"),
        ],
    );
}

/// KEYC-MODULES: ⌘⌥1 Library, ⌘⌥2 Develop, ⌘⌥3 Map, ⌘⌥5 Slideshow, ⌘⌥6 Print (⌘⌥4 Book and ⌘⌥7 Web have no module yet).
#[test]
fn keyc_modules_keys_dispatch_the_module_commands() {
    assert_dispatch(
        "KEYC-MODULES",
        &[
            ("Cmd+Alt+1", "view.photoGrid"),
            ("Cmd+Alt+2", "view.detail"),
            ("Cmd+Alt+3", "map.view"),
            ("Cmd+Alt+5", "slideshow.view"),
            ("Cmd+Alt+6", "print.view"),
        ],
    );
}

/// KEYC-MODULES: the module keys switch the view while the Classic keymap is on, and do nothing while it is off.
#[test]
fn classic_module_keys_switch_between_library_and_develop() {
    let mut h = library();
    press(&mut h, "2", true, true, false);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "⌘⌥2 has no binding while the Classic keymap is off");
    h.app.run("app.classicKeymap", json!({"on": true})).unwrap();
    press(&mut h, "2", true, true, false);
    assert_eq!(h.app.ui.view, ViewMode::Detail, "⌘⌥2 opens Develop (the Detail view)");
    press(&mut h, "1", true, true, false);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "⌘⌥1 opens Library (the photo grid)");
}

/// KEYC-MODULE-OUTPUT: the Map, Slideshow and Print module keys (Book and Web are missing).
#[test]
fn keyc_module_output_keys_dispatch_the_map_slideshow_and_print_modules() {
    assert_dispatch("KEYC-MODULE-OUTPUT", &[("Cmd+Alt+3", "map.view"), ("Cmd+Alt+5", "slideshow.view"), ("Cmd+Alt+6", "print.view")]);
}

/// KEYC-COMPARE: Z, Home / End, = / −, ⌘⇧D and S. In the grids S runs `stack.toggle` (test
/// `s_expands_and_collapses_the_stack_in_the_grid`) and Z picks (test `z_picks_in_grids_and_zooms_in_detail`).
#[test]
fn keyc_compare_keys_dispatch_the_grid_and_compare_commands() {
    assert_dispatch(
        "KEYC-COMPARE",
        &[
            ("Z", "view.zoomToggle"),
            ("Home", "library.first"),
            ("End", "library.last"),
            ("=", "view.thumbLarger"),
            ("-", "view.thumbSmaller"),
            ("Cmd+Shift+D", "library.selectOnlyActive"),
            ("S", "view.softProof"),
        ],
    );
}

/// KEYC-RATING: the flag, label and rating keys with a fixed command. Digits 0–9 are checked by the next test.
#[test]
fn keyc_rating_keys_dispatch_the_rating_commands() {
    assert_dispatch(
        "KEYC-RATING",
        &[
            ("P", "photo.pick"),
            ("X", "photo.reject"),
            ("U", "photo.unflag"),
            ("Shift+X", "photo.flag"),
            ("Shift+U", "photo.flag"),
            ("`", "photo.cycleFlag"),
            ("\\", "view.showOriginal"),
            ("[", "brush.smaller"),
            ("]", "brush.larger"),
        ],
    );
}

#[test]
fn keyc_rating_digits_rate_the_selection() {
    use lightcraft_catalog::PhotoId;
    let mut h = library();
    press(&mut h, "3", false, false, false);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().rating, 3);
}

/// KEYC-COLLECTIONS: ⌘N opens New Album and B adds the selection to the Quick Collection in the grids.
#[test]
fn keyc_collections_keys_dispatch_the_collection_commands() {
    use lightcraft_catalog::PhotoId;
    assert_dispatch("KEYC-COLLECTIONS", &[("Cmd+N", "dialog.newAlbum")]);
    // B is Brush in Develop; in the grids handle() runs album.toggleTarget instead (checked below)
    assert_dispatch("KEYC-COLLECTIONS", &[("B", "tool.brush")]);
    let mut h = library();
    press(&mut h, "B", false, false, false);
    let quick = h.app.session.catalog.quick_collection().expect("B makes the Quick Collection");
    assert!(h.app.session.catalog.album(quick).is_some_and(|a| a.photos.contains(&PhotoId(1))));
}

/// KEYC-METADATA: ⌘K, ⌘S, ⌘⌥⇧C and ⌘⌥⇧V.
#[test]
fn keyc_metadata_keys_dispatch_the_metadata_commands() {
    assert_dispatch(
        "KEYC-METADATA",
        &[
            ("Cmd+K", "panel.keywordsFocus"),
            ("Cmd+S", "photo.saveMetadataToFile"),
            ("Cmd+Alt+Shift+C", "photo.copyMetadata"),
            ("Cmd+Alt+Shift+V", "photo.pasteMetadata"),
        ],
    );
}

/// KEYC-DEVELOP: V, ⌘U, ⇧⌘U, R, Q, K, M, ⇧M, ⇧J and ⇧Q (⇧W has no Point Color tool yet).
#[test]
fn keyc_develop_keys_dispatch_the_develop_commands() {
    assert_dispatch(
        "KEYC-DEVELOP",
        &[
            ("V", "develop.treatment"),
            ("Cmd+U", "develop.autoTone"),
            ("Cmd+Shift+U", "develop.autoWhiteBalance"),
            ("R", "panel.crop"),
            ("Q", "panel.remove"),
            ("K", "tool.brush"),
            ("M", "tool.linear"),
            ("Shift+M", "tool.radial"),
            ("Shift+J", "view.clipping"),
            ("Shift+Q", "view.visualizeSpots"),
        ],
    );
}

/// KEYC-DEVELOP: Q opens the Remove (spot removal) panel while the Classic keymap is on, and is unbound while it is off.
#[test]
fn classic_q_opens_the_remove_panel_only_with_the_classic_keymap() {
    let mut h = library();
    h.app.ui.right = RightPanel::None;
    press(&mut h, "Q", false, false, false);
    assert_eq!(h.app.ui.right, RightPanel::None, "Q has no binding while the Classic keymap is off");
    h.app.run("app.classicKeymap", json!({"on": true})).unwrap();
    press(&mut h, "Q", false, false, false);
    assert_eq!(h.app.ui.right, RightPanel::Remove);
}
