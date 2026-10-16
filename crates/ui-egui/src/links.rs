//! Community and project links (Help menu, About dialog, top-bar Discord button).

/// Ember repository slug.
pub const APP: &str = "ember";
/// Ember community destination; legacy constant retained for callers.
pub const DISCORD: &str = "https://github.com/Orthic-Labs/ember/issues";
/// Maintainer organization.
pub const WEBSITE: &str = "https://github.com/Orthic-Labs";
/// Fork project page.
pub const APP_PAGE: &str = "https://github.com/Orthic-Labs/ember";
/// This app's source repository.
pub const GITHUB: &str = "https://github.com/Orthic-Labs/ember";
/// Published Ember builds. Help ▸ Check for Updates opens this page; egui has no self-updater.
pub const RELEASES: &str = "https://github.com/Orthic-Labs/ember/releases";

/// (UI command id, menu label, URL) for each link, in Help-menu order.
/// The user documentation (docs/ in the repository).
pub const HELP: &str = "https://github.com/Orthic-Labs/ember";

pub const LINKS: &[(&str, &str, &str)] = &[
    ("app.help", "Ember Help", HELP),
    ("app.discord", "Ember Community…", DISCORD),
    ("app.website", "Ember Project", APP_PAGE),
    ("app.github", "Ember on GitHub", GITHUB),
    ("app.checkUpdates", "Check for Updates…", RELEASES),
    ("app.artcraft", "Orthic Labs", WEBSITE),
    ("app.feedback", "Send Feedback…", FEEDBACK),
];

/// Where feedback and bug reports go.
pub const FEEDBACK: &str = "https://github.com/Orthic-Labs/ember/issues/new";

/// The URL behind a link command id.
pub fn url_of(cmd: &str) -> Option<&'static str> {
    LINKS.iter().find(|(id, _, _)| *id == cmd).map(|(_, _, u)| *u)
}

/// Open `url` in the user's browser (through the host's `open_url` service).
pub fn open(app: &mut crate::LightcraftApp, url: &str) -> Result<serde_json::Value, String> {
    let open = app.services.open_url.as_mut().ok_or("can't open links here")?;
    open(url)?;
    Ok(serde_json::json!({ "url": url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_https_and_name_this_app() {
        for (id, label, url) in LINKS {
            assert!(url.starts_with("https://"), "{id}");
            assert!(!label.is_empty());
            assert_eq!(url_of(id), Some(*url));
        }
        assert_eq!(APP_PAGE, GITHUB);
        assert!(GITHUB.ends_with(&format!("/Orthic-Labs/{APP}")));
    }

    #[test]
    fn check_for_updates_command_resolves_the_releases_page() {
        assert!(crate::menus::ui_commands().any(|(id, _, _, _)| *id == "app.checkUpdates"));
        assert_eq!(url_of("app.checkUpdates"), Some(RELEASES));
        assert_eq!(RELEASES, "https://github.com/Orthic-Labs/ember/releases");
    }
}
