//! Photo ▸ Enhance… (issues #68, #98): the command opens the Enhance dialog, which starts from the active photo's
//! AI Denoise and Raw Details settings.

use lightcraft_engine::Session;
use serde_json::json;

use crate::state::Dialog;
use crate::{LightcraftApp, Services};

#[test]
fn enhance_command_opens_the_dialog() {
    let mut app = LightcraftApp::new(Session::with_demo(), Services { png: None, ..Default::default() });
    app.run("dialog.enhance", json!({})).unwrap();
    assert!(matches!(app.ui.dialog, Some(Dialog::Enhance { denoise: false, raw_details: false, .. })));
}
