//! Early-access features (LR-PREF-TECHPREVIEW, LR-BEHAV-AI-EA): one table of the features still
//! being built or checked. Each has an on/off toggle (Settings ▸ Early Access, or the
//! `app.earlyAccess {feature, enabled?}` command), an "Early Access" badge beside its UI entries,
//! and a gate: while it is off its menu entries are hidden and its commands are refused with
//! "early access feature is off".
//!
//! A choice is saved in the app settings (`ui.json`, key `settings.earlyAccess`, feature key → on or
//! off). A feature the user has not chosen takes its `default_on`, so a missing key (an older
//! `ui.json`) reads as the default. The gate runs in [`crate::LightcraftApp::run_typed`], the one
//! entry point for the menus, the Settings dialog and the control channel; the engine's own
//! `Session::execute` (CLI, MCP) is not gated.

use serde_json::{Value, json};

use crate::state::AppSettings;

/// The command that sets a feature on or off.
pub const COMMAND: &str = "app.earlyAccess";

/// One early-access feature.
pub struct Feature {
    /// Stable key: the `feature` parameter and the settings key.
    pub key: &'static str,
    /// The name in Settings ▸ Early Access and in a refusal.
    pub label: &'static str,
    /// What it does and what is still rough, in one line.
    pub note: &'static str,
    /// On or off until the user chooses. Experimental and unverified features are off; a feature
    /// that already behaves as shipped stays on so nothing visible disappears.
    pub default_on: bool,
    /// Command ids the feature owns: menu-hidden and refused while off.
    pub commands: &'static [&'static str],
    /// Command-id prefixes the feature owns (every command of a module).
    pub prefixes: &'static [&'static str],
    /// `(command, param, value prefix)`: a call whose string `param` starts with the prefix is refused
    /// while off, for a feature that is one setting of a shared command.
    pub params: &'static [(&'static str, &'static str, &'static str)],
}

/// Every early-access feature, in the order Settings lists them.
pub const FEATURES: &[Feature] = &[
    Feature {
        key: "lensBlur",
        label: "Lens Blur",
        note: "Depth-aware bokeh from a two-layer depth proxy (subject in focus, background blurred). Its controls are set by command only.",
        default_on: false,
        commands: &[],
        prefixes: &[],
        params: &[
            ("develop.set", "control", "lensBlur."),
            ("develop.adjust", "control", "lensBlur."),
            ("develop.resetControl", "control", "lensBlur."),
        ],
    },
    Feature {
        key: "rawDetails",
        label: "Raw Details demosaic",
        note: "A higher-quality Bayer demosaic for raw files, under Detail. Edits made without it keep their look.",
        default_on: false,
        commands: &["develop.rawDetails"],
        prefixes: &[],
        params: &[],
    },
    Feature {
        key: "naturalSearch",
        label: "Natural-language search",
        note: "Find photos by describing them in words. No search box offers it yet, so it changes nothing today.",
        default_on: false,
        commands: &[],
        prefixes: &[],
        params: &[],
    },
    Feature {
        key: "denoiseGpu",
        label: "ONNX denoise acceleration",
        note: "Lets AI Denoise models run on the graphics card when you choose it. Automatic already uses the card where it is faster.",
        default_on: true,
        commands: &[],
        prefixes: &[],
        params: &[("denoise.settings", "runOn", "gpu")],
    },
    Feature {
        key: "slideshowModule",
        label: "Classic Slideshow module",
        note: "View ▸ Classic Slideshow: templates, timeline and export. View ▸ Slideshow (full screen) is not affected.",
        default_on: false,
        commands: &[],
        prefixes: &["slideshow."],
        params: &[],
    },
    Feature {
        key: "bookModule",
        label: "Classic Book module",
        note: "View ▸ Classic Book: page layouts, text, backgrounds and PDF or JPEG export.",
        default_on: false,
        commands: &[],
        prefixes: &["book."],
        params: &[],
    },
    Feature {
        key: "hdrDisplay",
        label: "HDR display output",
        note: "View ▸ HDR Display: HDR edits above SDR white where the screen and window can show them. The 8-bit window surface is not enough yet.",
        default_on: false,
        commands: &["view.hdrDisplay", "view.previewSdr"],
        prefixes: &[],
        params: &[],
    },
];

/// The feature with `key`.
pub fn feature(key: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.key == key)
}

/// Whether `f` is on: the user's choice, else its default.
pub fn is_on(settings: &AppSettings, f: &Feature) -> bool {
    settings.early_access.get(f.key).copied().unwrap_or(f.default_on)
}

/// Whether the feature with `key` is on (false for an unknown key).
pub fn on(settings: &AppSettings, key: &str) -> bool {
    feature(key).is_some_and(|f| is_on(settings, f))
}

/// Whether `f` owns `command` (listed or prefixed).
fn claims(f: &Feature, command: &str) -> bool {
    f.commands.iter().any(|c| *c == command) || f.prefixes.iter().any(|p| command.starts_with(*p))
}

/// The feature whose whole command (or module) `command` is.
fn owner(command: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| claims(f, command))
}

/// Whether a menu entry carries the Early Access badge.
pub fn owns(command: &str) -> bool {
    owner(command).is_some()
}

/// Whether a menu entry is listed: an early-access command is hidden while its feature is off.
pub fn menu_shows(settings: &AppSettings, command: &str) -> bool {
    match owner(command) {
        Some(f) => is_on(settings, f),
        None => true,
    }
}

/// The feature that refuses a call to `command` with `params` while it is off, if any.
fn refusing(command: &str, params: &Value) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| {
        claims(f, command)
            || f.params
                .iter()
                .any(|(c, key, prefix)| *c == command && params.get(*key).and_then(Value::as_str).is_some_and(|v| v.starts_with(*prefix)))
    })
}

/// Refuses a call to a feature's command while the feature is off.
pub fn gate(settings: &AppSettings, command: &str, params: &Value) -> Result<(), String> {
    match refusing(command, params) {
        Some(f) if !is_on(settings, f) => Err(format!("early access feature is off: {} (turn it on in Settings ▸ Early Access)", f.label)),
        _ => Ok(()),
    }
}

/// Sets feature `key` on or off (toggles it when `enabled` is `None`); returns `{feature, enabled}`.
pub fn set(settings: &mut AppSettings, key: &str, enabled: Option<bool>) -> Result<Value, String> {
    let f = feature(key).ok_or_else(|| format!("unknown early access feature: {key}"))?;
    let on = match enabled {
        Some(on) => on,
        None => !is_on(settings, f),
    };
    settings.early_access.insert(f.key.to_string(), on);
    Ok(json!({"feature": f.key, "enabled": on}))
}

/// The toggle command's parameters: `feature` (required) and `enabled` (optional bool).
fn toggle(settings: &mut AppSettings, params: &Value) -> Result<Value, String> {
    let key = params.get("feature").and_then(Value::as_str).ok_or("`feature` names an early access feature")?;
    let enabled = match params.get("enabled") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or("`enabled` must be true or false")?),
    };
    set(settings, key, enabled)
}

/// The early-access handling of one command: the toggle itself, or the gate that refuses a feature's
/// command while it is off. `None` when the command has nothing to do with early access.
pub fn intercept(settings: &mut AppSettings, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == COMMAND {
        return Some(toggle(settings, params));
    }
    gate(settings, id, params).err().map(Err)
}

/// The Early Access badge beside a feature's UI entry.
pub fn badge(ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Early Access")).size(10.5).color(t.text_dim));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LightcraftApp, Services};

    #[test]
    fn keys_are_unique() {
        let mut keys: Vec<&str> = FEATURES.iter().map(|f| f.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), FEATURES.len(), "two early-access features share a key");
    }

    #[test]
    fn defaults_follow_maturity() {
        let s = AppSettings::default();
        // experimental or not yet built: off
        for key in ["lensBlur", "rawDetails", "naturalSearch", "slideshowModule", "bookModule", "hdrDisplay"] {
            assert!(!on(&s, key), "{key} should default off");
        }
        // already behaves as shipped (automatic already picks the card): on, so nothing disappears
        assert!(on(&s, "denoiseGpu"));
        assert!(!on(&s, "no-such-feature"));
    }

    #[test]
    fn toggle_round_trips_through_saved_settings() {
        let mut s = AppSettings::default();
        assert_eq!(set(&mut s, "hdrDisplay", Some(true)).unwrap(), json!({"feature": "hdrDisplay", "enabled": true}));
        assert!(on(&s, "hdrDisplay"));
        // no value: flips back
        assert_eq!(set(&mut s, "hdrDisplay", None).unwrap()["enabled"], false);
        set(&mut s, "denoiseGpu", Some(false)).unwrap();
        let saved = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.early_access, s.early_access);
        assert!(!on(&back, "denoiseGpu"));
        // a settings file from before the table: every feature reads as its default
        let old: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(on(&old, "denoiseGpu") && !on(&old, "lensBlur"));
        assert!(set(&mut s, "nope", Some(true)).is_err());
        assert!(toggle(&mut s, &json!({"feature": "hdrDisplay", "enabled": "yes"})).is_err());
        assert!(toggle(&mut s, &json!({})).is_err());
    }

    #[test]
    fn gated_commands_refuse_cleanly_while_off() {
        let mut s = AppSettings::default();
        let err = gate(&s, "book.render", &json!({"page": 0})).unwrap_err();
        assert!(err.contains("early access feature is off") && err.contains("Classic Book"), "{err}");
        assert!(gate(&s, "slideshow.export", &json!({})).is_err());
        assert!(gate(&s, "view.hdrDisplay", &json!({})).is_err());
        assert!(gate(&s, "develop.rawDetails", &json!({"enabled": true})).is_err());
        assert!(gate(&s, "develop.set", &json!({"control": "lensBlur.amount", "value": 10})).is_err());
        // the same commands pass once on, and unrelated commands always pass
        set(&mut s, "bookModule", Some(true)).unwrap();
        assert!(gate(&s, "book.render", &json!({"page": 0})).is_ok());
        assert!(gate(&s, "develop.set", &json!({"control": "exposure", "value": 1})).is_ok());
        assert!(gate(&s, "library.import", &json!({})).is_ok());
        // a shared command is refused only for the feature's own param
        assert!(gate(&s, "denoise.settings", &json!({"runOn": "gpu"})).is_ok(), "denoiseGpu defaults on");
        set(&mut s, "denoiseGpu", Some(false)).unwrap();
        assert!(gate(&s, "denoise.settings", &json!({"runOn": "gpu"})).is_err());
        assert!(gate(&s, "denoise.settings", &json!({"runOn": "cpu"})).is_ok());
    }

    #[test]
    fn menu_entries_hide_while_off() {
        let mut s = AppSettings::default();
        assert!(!menu_shows(&s, "book.view") && !menu_shows(&s, "view.hdrDisplay"));
        assert!(menu_shows(&s, "library.import") && owns("book.view") && !owns("library.import"));
        set(&mut s, "hdrDisplay", Some(true)).unwrap();
        assert!(menu_shows(&s, "view.hdrDisplay"));
    }

    #[test]
    fn app_command_refuses_then_runs_after_toggle() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let err = app.run("view.hdrDisplay", json!({})).unwrap_err();
        assert!(err.contains("early access feature is off"), "{err}");
        assert!(!app.ui.hdr_display);
        app.run(COMMAND, json!({"feature": "hdrDisplay", "enabled": true})).expect("turns on");
        assert!(app.run("view.hdrDisplay", json!({})).is_ok());
        assert!(app.ui.hdr_display);
    }
}
