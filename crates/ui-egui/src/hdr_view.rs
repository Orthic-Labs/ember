//! HDR display output (LR-VIEW-HDR-DISPLAY): View ▸ HDR Display and the decision it feeds. A photo
//! edited in HDR (`develop.hdr`) is shown with its highlights above SDR white only when the user
//! asked for it, the window surface can carry values above 1.0 (`Services::hdr_surface`) and the
//! screen reports headroom above SDR white; otherwise every preview is the SDR rendition, tone
//! mapped exactly as before the toggle existed.

use crate::LightcraftApp;

/// The headroom of the screen showing the window: its peak over SDR white (1.0 on an SDR screen or
/// with HDR off in the OS; `None`: not known). Polled about once a second.
pub type HeadroomFn = Box<dyn FnMut() -> Option<f32>>;

/// Headroom is clamped here (16x SDR white, four stops): a misreporting display can't ask for more.
pub const MAX_HEADROOM: f32 = 16.0;

/// How previews reach the display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HdrView {
    /// Tone mapped to SDR white, as ever.
    Sdr { reason: &'static str },
    /// Values above 1.0 kept, up to `headroom` times SDR white.
    Hdr { headroom: f32 },
}

impl HdrView {
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Hdr { .. })
    }

    /// The headroom to render with: `None` means tone map to SDR.
    pub fn headroom(self) -> Option<f32> {
        match self {
            Self::Hdr { headroom } => Some(headroom),
            Self::Sdr { .. } => None,
        }
    }
}

/// HDR only with the toggle on, a surface that carries it and display headroom above 1.0.
pub fn decide(enabled: bool, surface_hdr: bool, headroom: Option<f32>) -> HdrView {
    if !enabled {
        return HdrView::Sdr { reason: "HDR Display is off" };
    }
    if !surface_hdr {
        return HdrView::Sdr { reason: "the window cannot show HDR on this system" };
    }
    match headroom {
        Some(h) if h.is_finite() && h > 1.0 => HdrView::Hdr { headroom: h.min(MAX_HEADROOM) },
        Some(_) => HdrView::Sdr { reason: "the display is in SDR mode" },
        None => HdrView::Sdr { reason: "the display's HDR headroom is unknown" },
    }
}

/// View ▸ Preview SDR Rendition: with it on, an HDR view shows the SDR rendition instead (what an SDR
/// screen or export gets); an SDR view stays as it is.
pub fn with_sdr_preview(view: HdrView, preview_sdr: bool) -> HdrView {
    if preview_sdr && view.is_hdr() { HdrView::Sdr { reason: "previewing the SDR rendition" } } else { view }
}

impl LightcraftApp {
    /// How previews are shown now (SDR unless [`decide`] says HDR and the SDR preview is off).
    pub fn hdr_view(&self) -> HdrView {
        with_sdr_preview(decide(self.ui.hdr_display, self.services.hdr_surface, self.hdr_headroom), self.ui.hdr_preview_sdr)
    }

    /// Reads the screen's headroom about once a second, so moving the window to another screen or
    /// switching HDR in the OS is followed.
    pub(crate) fn follow_hdr_headroom(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.services.display_headroom.is_none() || self.hdr_headroom_read.is_some_and(|t| (0.0..1.0).contains(&(now - t))) {
            return;
        }
        self.hdr_headroom_read = Some(now);
        if let Some(read) = self.services.display_headroom.as_mut() {
            self.hdr_headroom = read();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LightcraftApp, Services};

    #[test]
    fn headroom_decides_hdr_or_tone_map() {
        assert_eq!(decide(true, true, Some(4.0)), HdrView::Hdr { headroom: 4.0 });
        // too much is clamped
        assert_eq!(decide(true, true, Some(1000.0)).headroom(), Some(MAX_HEADROOM));
        // no headroom (1.0), unknown, or nonsense: tone mapped to SDR
        for h in [Some(1.0), Some(0.5), Some(0.0), Some(-3.0), Some(f32::NAN), Some(f32::INFINITY), None] {
            assert!(!decide(true, true, h).is_hdr(), "{h:?}");
            assert_eq!(decide(true, true, h).headroom(), None, "{h:?}");
        }
        // a surface that can't carry HDR is SDR whatever the display says
        assert!(!decide(true, false, Some(4.0)).is_hdr());
    }

    #[test]
    fn toggle_off_is_sdr_whatever_the_system_reports() {
        for surface in [false, true] {
            for h in [None, Some(1.0), Some(2.0), Some(8.0)] {
                assert!(!decide(false, surface, h).is_hdr());
            }
        }
        // the app: HDR surface and a display with headroom, but the default (off) stays SDR
        let services = Services { png: None, hdr_surface: true, display_headroom: Some(Box::new(|| Some(4.0))), ..Default::default() };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        assert!(!app.ui.hdr_display, "SDR is the default");
        let ctx = egui::Context::default();
        app.follow_hdr_headroom(&ctx);
        assert_eq!(app.hdr_headroom, Some(4.0));
        assert!(!app.hdr_view().is_hdr());
        app.ui.hdr_display = true;
        assert_eq!(app.hdr_view(), HdrView::Hdr { headroom: 4.0 });
        // no HDR surface (eframe's 8-bit one today): still SDR with the toggle on
        app.services.hdr_surface = false;
        assert!(!app.hdr_view().is_hdr());
    }

    #[test]
    fn sdr_preview_turns_hdr_into_the_sdr_rendition_only() {
        let hdr = HdrView::Hdr { headroom: 4.0 };
        assert_eq!(with_sdr_preview(hdr, false), hdr);
        let sdr = with_sdr_preview(hdr, true);
        assert!(!sdr.is_hdr());
        assert_eq!(sdr.headroom(), None);
        // an SDR view keeps its own reason
        let off = decide(false, true, Some(4.0));
        assert_eq!(with_sdr_preview(off, true), off);
        assert_eq!(with_sdr_preview(off, false), off);
    }

    #[test]
    fn sdr_preview_toggle_overrides_an_active_hdr_view() {
        let services = Services { png: None, hdr_surface: true, display_headroom: Some(Box::new(|| Some(4.0))), ..Default::default() };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        app.run(crate::early_access::COMMAND, serde_json::json!({"feature": "hdrDisplay", "enabled": true})).expect("early access on");
        app.follow_hdr_headroom(&egui::Context::default());
        app.ui.hdr_display = true;
        assert!(!app.ui.hdr_preview_sdr, "off by default");
        assert_eq!(app.hdr_view(), HdrView::Hdr { headroom: 4.0 });
        let on = app.run("view.previewSdr", serde_json::json!({})).expect("toggles on");
        assert_eq!((on["on"].as_bool(), on["hdrAvailable"].as_bool(), on["active"].as_bool()), (Some(true), Some(true), Some(false)));
        assert!(!app.hdr_view().is_hdr());
        let off = app.run("view.previewSdr", serde_json::json!({"on": false})).expect("switches off");
        assert_eq!((off["on"].as_bool(), off["active"].as_bool()), (Some(false), Some(true)));
        assert!(app.hdr_view().is_hdr());
    }

    #[test]
    fn sdr_preview_command_follows_the_hdr_display_early_access_gate() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        assert!(app.run("view.previewSdr", serde_json::json!({})).is_err(), "refused while the early access feature is off");
        assert!(!app.ui.hdr_preview_sdr);
        app.run(crate::early_access::COMMAND, serde_json::json!({"feature": "hdrDisplay", "enabled": true})).expect("early access on");
        let r = app.run("view.previewSdr", serde_json::json!({})).expect("runs");
        assert_eq!(r["on"], true);
        assert_eq!(r["hdrAvailable"], false, "no HDR display in tests, so the preview changes nothing");
        assert_eq!(r["active"], false);
    }

    #[test]
    fn toggle_command_flips_and_reports() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        app.run(crate::early_access::COMMAND, serde_json::json!({"feature": "hdrDisplay", "enabled": true})).expect("early access on");
        let on = app.run("view.hdrDisplay", serde_json::json!({})).expect("toggles");
        assert_eq!(on["on"], true);
        assert_eq!(on["active"], false, "no HDR surface in tests");
        assert!(on["reason"].is_string());
        assert!(app.ui.hdr_display);
        let off = app.run("view.hdrDisplay", serde_json::json!({"on": false})).expect("switches off");
        assert_eq!(off["on"], false);
        assert!(!app.ui.hdr_display);
    }
}
