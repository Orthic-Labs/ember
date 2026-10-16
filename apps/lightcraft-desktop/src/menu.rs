//! Stable native menu IDs shared with engine command metadata.

#![forbid(unsafe_code)]

use rightkit_tauri_shell::{MenuEntry, MenuGroup, MenuSpec};

fn modifier() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Ctrl"
    }
}

fn accelerator(key: &str) -> String {
    key.strip_prefix("Cmd+").map_or_else(|| key.to_string(), |suffix| format!("{}+{suffix}", modifier()))
}

fn group(label: &str, entries: &[(&str, &str, Option<&str>)]) -> MenuGroup {
    MenuGroup {
        label: label.into(),
        entries: entries
            .iter()
            .map(|(id, title, key)| {
                key.map_or_else(
                    || MenuEntry::item(id, title),
                    |key| {
                        let key = accelerator(key);
                        MenuEntry::accel(id, title, key.as_str())
                    },
                )
            })
            .collect(),
    }
}

/// Native menu contains IDs used by existing `menus.rs`; frontend forwards clicks through
/// `rightkit-shell://menu` so command routing remains authoritative in engine/session.
pub fn spec() -> MenuSpec {
    MenuSpec {
        app_name: "Ember".into(),
        settings: Some(("app.settings".into(), accelerator("Cmd+,"))),
        groups: vec![
            group(
                "File",
                &[
                    ("file.addPhotos", "Import Photos…", Some("Cmd+Shift+I")),
                    ("file.addFolder", "Import from Folder…", None),
                    ("file.importLightroom", "Import LRCAT Catalog…", None),
                    ("file.addFromDevice", "Import from Device", None),
                    ("file.findMissing", "Find Missing Photos…", None),
                    ("file.backupLibrary", "Back Up Library…", None),
                    ("file.restoreLibrary", "Restore Library from Backup…", None),
                    ("file.importPresets", "Import Profiles & Presets…", None),
                    ("file.exportPresets", "Export Presets…", None),
                    ("file.importCurvePresets", "Import Point Curve Presets…", None),
                    ("file.exportCurvePresets", "Export Point Curve Presets…", None),
                    ("dialog.export", "Export…", None),
                    ("app.openLibrary", "Open Library…", None),
                    ("app.quit", "Quit Ember", Some("Cmd+Q")),
                ],
            ),
            group(
                "Edit",
                &[
                    ("edit.undo", "Undo", Some("Cmd+Z")),
                    ("edit.redo", "Redo", Some("Cmd+Shift+Z")),
                    ("edit.selectOnlyActive", "Select Only Active Photo", Some("Cmd+Shift+D")),
                    ("view.focusSearch", "Find…", Some("Cmd+F")),
                ],
            ),
            group(
                "View",
                &[
                    ("view.photoGrid", "Photo Grid", None),
                    ("view.squareGrid", "Square Grid", None),
                    ("view.gridToggle", "Grid", Some("G")),
                    ("view.detail", "Detail", Some("D")),
                    ("view.compare", "Compare", Some("Shift+C")),
                    ("view.survey", "Survey", Some("N")),
                    ("view.people", "People", None),
                    ("view.reference", "Reference View", Some("Shift+R")),
                    ("view.filmstrip", "Filmstrip", Some("/")),
                    ("view.beforeAfter", "Compare Before and After", Some("Y")),
                    ("view.beforeAfterSplit", "Before/After Split", Some("Shift+Y")),
                    ("view.beforeAfterTopBottom", "Before/After Top/Bottom", Some("Alt+Y")),
                    ("view.beforeAfterSplitTopBottom", "Before/After Split Top/Bottom", Some("Alt+Shift+Y")),
                    ("view.showOriginal", "Show Original", Some("\\")),
                    ("view.zoomFit", "Zoom to Fit", Some("Cmd+0")),
                    ("view.zoom100", "Zoom 100%", Some("Cmd+Alt+0")),
                    ("view.zoomToggle", "Toggle Zoom", Some("Z")),
                    ("view.zoomIn", "Zoom In", Some("Cmd+=")),
                    ("view.zoomOut", "Zoom Out", Some("Cmd+-")),
                    ("view.clipping", "Show Clipping", Some("J")),
                    ("view.softProof", "Soft Proofing", Some("S")),
                    ("view.histogram", "Histogram", Some("Cmd+Shift+H")),
                    ("view.maskOverlay", "Show Mask Overlay", Some("O")),
                    ("view.filterBar", "Filter Bar", Some("Shift+F")),
                    ("view.navigator", "Navigator", None),
                    ("view.enterFullScreen", "Enter Full Screen", Some("Cmd+Shift+F")),
                    ("view.infoOverlay", "Cycle Info Overlay", Some("Cmd+I")),
                    ("view.viewOptions", "View Options…", None),
                ],
            ),
            group(
                "Photo",
                &[
                    ("photo.editInExternal", "Edit in External Editor", Some("Cmd+Shift+E")),
                    ("photo.locate", "Locate Missing File…", None),
                    ("dialog.rename", "Rename Photos…", Some("F2")),
                    ("dialog.allMetadata", "All Metadata…", None),
                ],
            ),
            group(
                "Window",
                &[
                    ("view.secondWindow", "Second Window", Some("Cmd+F11")),
                    ("panel.edit", "Edit", Some("E")),
                    ("panel.crop", "Crop & Rotate", Some("C")),
                    ("panel.masking", "Masking", Some("M")),
                    ("panel.presets", "Presets", Some("Shift+P")),
                    ("panel.info", "Info", Some("I")),
                    ("panel.keywords", "Keywords", Some("K")),
                    ("panel.versions", "Versions", Some("Shift+V")),
                    ("panel.activity", "History", None),
                    ("panel.close", "Close Panel", None),
                    ("tool.brush", "Brush", Some("B")),
                    ("tool.linear", "Linear Gradient", Some("L")),
                    ("tool.radial", "Radial Gradient", Some("R")),
                    ("tool.wbPicker", "White Balance Selector", Some("W")),
                ],
            ),
        ],
        help: vec![
            MenuEntry::item("app.about", "About Ember"),
            MenuEntry::item("app.checkUpdates", "Check for Updates…"),
            MenuEntry::item("app.help", "Ember Help"),
            MenuEntry::item("app.github", "Ember on GitHub"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{accelerator, spec};
    use rightkit_tauri_shell::MenuEntry;

    #[test]
    fn help_menu_offers_check_for_updates() {
        assert!(spec().help.contains(&MenuEntry::item("app.checkUpdates", "Check for Updates…")));
    }

    #[test]
    fn platform_modifier_is_used_only_for_command_shortcuts() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(accelerator("Cmd+Z"), "Cmd+Z");
            assert_eq!(accelerator("Cmd+Shift+I"), "Cmd+Shift+I");
            assert_eq!(accelerator("Cmd+,"), "Cmd+,");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(accelerator("Cmd+Z"), "Ctrl+Z");
            assert_eq!(accelerator("Cmd+Shift+I"), "Ctrl+Shift+I");
            assert_eq!(accelerator("Cmd+,"), "Ctrl+,");
        }
        assert_eq!(accelerator("Shift+C"), "Shift+C");
    }
}
