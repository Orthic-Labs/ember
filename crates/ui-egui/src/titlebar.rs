//! The integrated title bar's window gestures (macOS: the native title bar is hidden and the top
//! bar takes its place, so dragging and double-click-to-zoom have to be done here).
//!
//! Pure policy, no egui widgets: [`command_for`] maps a gesture on the bar's empty space and the
//! window's current state to the viewport command the host should run, and [`press_lands_on_widget`]
//! decides whether a press belongs to a widget or to the empty strip.

use egui::ViewportCommand;

/// What the user did on the bar's empty space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// A primary-button drag began: the window follows the pointer.
    DragStarted,
    /// A primary-button double-click: the user's double-click action (zoom by default).
    DoubleClicked,
}

/// The window as the host reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// Zoomed (macOS) / maximized (elsewhere).
    pub maximized: bool,
    /// In the OS's full-screen mode.
    pub fullscreen: bool,
}

/// What a double-click on the bar does: the macOS "Double-click a window's title bar to" setting, as the bar honours it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DoubleClickAction {
    /// Zoom, or restore a zoomed window. The default, and what `Maximize` and `Fill` mean.
    #[default]
    Zoom,
    /// Minimize the window to the Dock.
    Minimize,
    /// Do nothing.
    Nothing,
}

/// Reads the `AppleActionOnDoubleClick` default. `Minimize` and `None` change the action; `Maximize`,
/// `Fill`, a missing value and anything unknown all zoom, the bar's behaviour before the setting was read.
pub fn parse_double_click_action(value: Option<&str>) -> DoubleClickAction {
    match value.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("Minimize") => DoubleClickAction::Minimize,
        Some(v) if v.eq_ignore_ascii_case("None") => DoubleClickAction::Nothing,
        _ => DoubleClickAction::Zoom,
    }
}

/// A widget under the pointer when a press began, as the title bar's hit-test sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hovered {
    /// The bar's own empty-space handle (never a widget).
    pub is_handle: bool,
    /// The widget takes clicks or drags. A label that only hovers does not take the press.
    pub senses_pointer: bool,
}

/// True when a press belongs to a widget rather than the empty strip: some other widget under the pointer takes
/// clicks or drags. The handle alone (or only hover-only widgets) leaves the press to the bar.
pub fn press_lands_on_widget(hovered: &[Hovered]) -> bool {
    hovered.iter().any(|h| !h.is_handle && h.senses_pointer)
}

/// The viewport command for `gesture`, if any. Full screen has no title bar to drag or zoom.
pub fn command_for(gesture: Gesture, window: WindowState, double_click: DoubleClickAction) -> Option<ViewportCommand> {
    if window.fullscreen {
        return None;
    }
    match gesture {
        Gesture::DragStarted => Some(ViewportCommand::StartDrag),
        Gesture::DoubleClicked => match double_click {
            DoubleClickAction::Zoom => Some(ViewportCommand::Maximized(!window.maximized)),
            DoubleClickAction::Minimize => Some(ViewportCommand::Minimized(true)),
            DoubleClickAction::Nothing => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: WindowState = WindowState { maximized: false, fullscreen: false };
    const ZOOMED: WindowState = WindowState { maximized: true, fullscreen: false };
    const FULL: WindowState = WindowState { maximized: false, fullscreen: true };
    const ZOOM: DoubleClickAction = DoubleClickAction::Zoom;

    // Scenario: dragging the title bar moves the window
    #[test]
    fn given_a_normal_window_when_dragging_then_the_window_follows_the_pointer() {
        assert_eq!(command_for(Gesture::DragStarted, NORMAL, ZOOM), Some(ViewportCommand::StartDrag));
    }

    // Scenario: a zoomed window can still be dragged (macOS lets you pull it free)
    #[test]
    fn given_a_zoomed_window_when_dragging_then_the_window_follows_the_pointer() {
        assert_eq!(command_for(Gesture::DragStarted, ZOOMED, ZOOM), Some(ViewportCommand::StartDrag));
    }

    // Scenario: double-click zooms
    #[test]
    fn given_a_normal_window_when_double_clicking_then_it_zooms() {
        assert_eq!(command_for(Gesture::DoubleClicked, NORMAL, ZOOM), Some(ViewportCommand::Maximized(true)));
    }

    // Scenario: double-click again restores
    #[test]
    fn given_a_zoomed_window_when_double_clicking_then_it_is_restored() {
        assert_eq!(command_for(Gesture::DoubleClicked, ZOOMED, ZOOM), Some(ViewportCommand::Maximized(false)));
    }

    // Scenario: the Minimize preference minimizes the window on double-click, zoomed or not
    #[test]
    fn given_the_minimize_preference_when_double_clicking_then_the_window_minimizes() {
        assert_eq!(command_for(Gesture::DoubleClicked, NORMAL, DoubleClickAction::Minimize), Some(ViewportCommand::Minimized(true)));
        assert_eq!(command_for(Gesture::DoubleClicked, ZOOMED, DoubleClickAction::Minimize), Some(ViewportCommand::Minimized(true)));
    }

    // Scenario: the None preference leaves a double-click alone, but dragging still moves the window
    #[test]
    fn given_the_none_preference_when_double_clicking_then_nothing_happens_and_dragging_still_moves() {
        assert_eq!(command_for(Gesture::DoubleClicked, NORMAL, DoubleClickAction::Nothing), None);
        assert_eq!(command_for(Gesture::DragStarted, NORMAL, DoubleClickAction::Nothing), Some(ViewportCommand::StartDrag));
    }

    // Scenario: full screen ignores both
    #[test]
    fn given_a_full_screen_window_then_neither_gesture_does_anything() {
        assert_eq!(command_for(Gesture::DragStarted, FULL, ZOOM), None);
        assert_eq!(command_for(Gesture::DoubleClicked, FULL, DoubleClickAction::Minimize), None);
        assert_eq!(command_for(Gesture::DoubleClicked, FULL, ZOOM), None);
    }

    // Scenario: the preference values the system writes
    #[test]
    fn given_the_system_double_click_values_then_each_maps_to_its_action() {
        assert_eq!(parse_double_click_action(Some("Maximize")), DoubleClickAction::Zoom);
        assert_eq!(parse_double_click_action(Some("Fill")), DoubleClickAction::Zoom);
        assert_eq!(parse_double_click_action(Some("Minimize")), DoubleClickAction::Minimize);
        assert_eq!(parse_double_click_action(Some("None")), DoubleClickAction::Nothing);
    }

    // Scenario: a missing or unknown preference keeps the zoom the bar always had
    #[test]
    fn given_no_or_an_unknown_preference_then_the_bar_zooms() {
        assert_eq!(parse_double_click_action(None), DoubleClickAction::Zoom);
        assert_eq!(parse_double_click_action(Some("")), DoubleClickAction::Zoom);
        assert_eq!(parse_double_click_action(Some("Shrink")), DoubleClickAction::Zoom);
    }

    // Scenario: the preference is matched without regard to case or stray spaces
    #[test]
    fn given_a_preference_in_other_case_or_padded_then_it_still_matches() {
        assert_eq!(parse_double_click_action(Some(" minimize ")), DoubleClickAction::Minimize);
        assert_eq!(parse_double_click_action(Some("NONE")), DoubleClickAction::Nothing);
    }

    // Scenario: a press on the empty strip (only the handle is under it) is the bar's
    #[test]
    fn given_only_the_handle_under_the_press_then_it_is_the_empty_strip() {
        let handle = Hovered { is_handle: true, senses_pointer: true };
        assert!(!press_lands_on_widget(&[handle]));
        assert!(!press_lands_on_widget(&[]));
    }

    // Scenario: a press on a button or the search field is theirs
    #[test]
    fn given_a_clickable_widget_under_the_press_then_it_is_a_widget() {
        let handle = Hovered { is_handle: true, senses_pointer: true };
        let button = Hovered { is_handle: false, senses_pointer: true };
        assert!(press_lands_on_widget(&[handle, button]));
        assert!(press_lands_on_widget(&[button]));
    }

    // Scenario: a widget that only hovers (a label) does not take the press from the bar
    #[test]
    fn given_a_hover_only_widget_under_the_press_then_the_bar_keeps_it() {
        let handle = Hovered { is_handle: true, senses_pointer: true };
        let label = Hovered { is_handle: false, senses_pointer: false };
        assert!(!press_lands_on_widget(&[handle, label]));
    }
}
