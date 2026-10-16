//! View Options (`view.viewOptions`): the loupe's two info overlays, their lines, and the render
//! message. Each change applies at once, as in Settings; the dialog closes with Close.

use crate::LightcraftApp;
use crate::info_overlay::{INFO_LINES, InfoField, InfoLines};
use crate::state::InfoOverlay;
use crate::theme::Tokens;

pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    ui.set_min_width(440.0);
    let mut shown = app.ui.info_overlay != InfoOverlay::Off;
    let r = ui.checkbox(&mut shown, crate::i18n::tr("Show info overlay"));
    crate::widgets::register(ui.ctx(), "viewOptions:showOverlay", r.rect);
    if r.changed() {
        // turning it on shows Info 1 (or the one that was showing); off keeps the choice for later
        app.ui.info_overlay = match (shown, app.ui.info_overlay) {
            (false, _) => InfoOverlay::Off,
            (true, InfoOverlay::Off) => InfoOverlay::Info1,
            (true, current) => current,
        };
    }
    ui.add_space(10.0);
    info_lines(ui, t, "Info 1", "viewOptions:info1", &mut app.ui.view_options.info1);
    ui.add_space(10.0);
    info_lines(ui, t, "Info 2", "viewOptions:info2", &mut app.ui.view_options.info2);
    ui.add_space(10.0);
    let r = ui.checkbox(&mut app.ui.view_options.render_message, crate::i18n::tr("Show message when loading or rendering photos"));
    crate::widgets::register(ui.ctx(), "viewOptions:renderMessage", r.rect);
}

/// One overlay's heading and its lines, each a field picked from a list.
fn info_lines(ui: &mut egui::Ui, t: &Tokens, title: &str, id: &str, lines: &mut InfoLines) {
    ui.label(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(14.0)).color(t.text));
    for (i, field) in lines.lines.iter_mut().enumerate().take(INFO_LINES) {
        ui.horizontal(|ui| {
            ui.label(format!("{}.", i + 1));
            egui::ComboBox::from_id_salt(format!("{id}:{i}")).selected_text(crate::i18n::tr(field.name())).show_ui(ui, |ui| {
                for choice in InfoField::ALL {
                    ui.selectable_value(&mut *field, choice, crate::i18n::tr(choice.name()));
                }
            });
        });
    }
}
