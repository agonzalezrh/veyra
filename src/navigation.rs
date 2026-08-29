/// Key binding identifiers for compositor actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    WorkspaceNext,
    WorkspacePrev,
    AppNext,
    AppPrev,
    ToggleFocus,
    ToggleSpatial,
    ToggleOverview,
    ToggleWorkspaceOverview,
    ToggleShelf,
    SendToShelf,
    Launcher,
    ResetCamera,
    DeEmphasize,
    FrameSelected,
    FrameAll,
    Escape,
    CloseApp,
    ReopenClosed,
    CycleVisuals,
    OpenContextMenu,
    HelpOverlay,
}

/// Describes a key binding composed of modifiers and a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub key: u32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

impl KeyBinding {
    pub const fn new(key: u32) -> Self {
        KeyBinding { key, ctrl: false, shift: false, alt: false, meta: false }
    }
    pub const fn ctrl(key: u32) -> Self {
        KeyBinding { key, ctrl: true, shift: false, alt: false, meta: false }
    }
    pub const fn ctrl_shift(key: u32) -> Self {
        KeyBinding { key, ctrl: true, shift: true, alt: false, meta: false }
    }
    pub const fn alt(key: u32) -> Self {
        KeyBinding { key, ctrl: false, shift: false, alt: true, meta: false }
    }
    pub const fn alt_shift(key: u32) -> Self {
        KeyBinding { key, ctrl: false, shift: true, alt: true, meta: false }
    }
    pub const fn meta(key: u32) -> Self {
        KeyBinding { key, ctrl: false, shift: false, alt: false, meta: true }
    }
    pub const fn meta_shift(key: u32) -> Self {
        KeyBinding { key, ctrl: false, shift: true, alt: false, meta: true }
    }
}

/// Central model for all compositor keyboard navigation bindings.
#[derive(Debug)]
pub struct NavigationModel {
    pub bindings: Vec<(Binding, KeyBinding)>,
}

impl NavigationModel {
    pub fn new() -> Self {
        use crate::keys;
        use Binding::*;
        // Plain key bindings (no modifier required — safe for compositor):
        // These interfere minimal with normal typing: Tab, F-keys, Escape,
        // Home, Menu. All letter/number shortcuts require a modifier.
        let bindings = vec![
            (ToggleSpatial,          KeyBinding::new(keys::F5)),
            (ToggleFocus,            KeyBinding::new(keys::F6)),
            (WorkspaceNext,          KeyBinding::ctrl(keys::TAB)),
            (WorkspacePrev,          KeyBinding::ctrl_shift(keys::TAB)),
            (AppNext,                KeyBinding::alt(keys::TAB)),
            (AppPrev,                KeyBinding::alt_shift(keys::TAB)),
            (Escape,                 KeyBinding::new(keys::ESCAPE)),
            (FrameAll,               KeyBinding::new(keys::HOME)),
            (OpenContextMenu,        KeyBinding::new(keys::MENU)),
            // Modifier-required bindings — these use modifier keys to avoid
            // intercepting ordinary typing. Meta is the primary compositor
            // modifier, with Alt for window management.
            (ToggleOverview,         KeyBinding::meta(keys::O)),
            (ToggleWorkspaceOverview,KeyBinding::meta(keys::P)),
            (ToggleSpatial,          KeyBinding::meta(keys::TAB)),
            (DeEmphasize,            KeyBinding::meta(keys::M)),
            (FrameSelected,          KeyBinding::meta(keys::F)),
            (ToggleShelf,            KeyBinding::meta(keys::D)),
            (SendToShelf,            KeyBinding::meta(keys::DOWN)),
            (Launcher,               KeyBinding::meta(keys::SPACE)),
            (CloseApp,               KeyBinding::meta(keys::W)),
            (ReopenClosed,           KeyBinding::meta_shift(keys::T)),
            (HelpOverlay,            KeyBinding::meta(keys::SLASH)),
        ];
        NavigationModel { bindings }
    }

    /// Find which binding (if any) matches the given key/modifier state.
    pub fn match_binding(&self, key: u32, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Option<Binding> {
        for (binding, kb) in &self.bindings {
            if kb.key == key && kb.ctrl == ctrl && kb.shift == shift && kb.alt == alt && kb.meta == meta {
                return Some(*binding);
            }
        }
        None
    }
}

impl Default for NavigationModel {
    fn default() -> Self {
        Self::new()
    }
}

/// Determine which camera bookmark slot (0-9) a key activates, if any.
///
/// Bookmark keys are the digit row (XKB 10-18 for 1-9, 19 for 0) and
/// REQUIRE the Meta modifier. Plain digits and Ctrl/Alt/Shift+digit
/// combinations must be forwarded to the focused client.
///
/// The first 9 slots (1-9) save when a visual is selected and restore
/// otherwise; slot 0 (key 0) is always available.
pub fn bookmark_slot(key: u32, meta: bool) -> Option<usize> {
    if !meta {
        return None;
    }
    use crate::keys;
    if key >= keys::K1 && key <= keys::K9 {
        Some((key - keys::K1) as usize)
    } else if key == keys::K0 {
        Some(9)
    } else {
        None
    }
}

/// The deterministic escape chain priority:
/// 1. Cancel drag
/// 2. Exit workspace overview
/// 3. Exit overview
/// 4. Exit focus mode
/// 5. Reset camera
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeAction {
    CancelDrag,
    ExitWorkspaceOverview,
    ExitOverview,
    ExitFocus,
    ResetCamera,
}

/// Determine what Escape should do given the current state.
pub fn escape_chain(
    is_dragging: bool,
    in_workspace_overview: bool,
    in_overview: bool,
    in_focus: bool,
) -> EscapeAction {
    if is_dragging {
        EscapeAction::CancelDrag
    } else if in_workspace_overview {
        EscapeAction::ExitWorkspaceOverview
    } else if in_overview {
        EscapeAction::ExitOverview
    } else if in_focus {
        EscapeAction::ExitFocus
    } else {
        EscapeAction::ResetCamera
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_binding_workspace_next() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(23, true, false, false, false), Some(Binding::WorkspaceNext));
    }

    #[test]
    fn match_binding_workspace_prev() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(23, true, true, false, false), Some(Binding::WorkspacePrev));
    }

    #[test]
    fn match_binding_app_next() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(23, false, false, true, false), Some(Binding::AppNext));
    }

    #[test]
    fn match_binding_app_prev() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(23, false, true, true, false), Some(Binding::AppPrev));
    }

    #[test]
    fn match_binding_toggle_spatial() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(23, false, false, false, true), Some(Binding::ToggleSpatial));
    }

    #[test]
    fn match_binding_toggle_focus() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(72, false, false, false, false), Some(Binding::ToggleFocus));
    }

    #[test]
    fn match_binding_escape() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(9, false, false, false, false), Some(Binding::Escape));
    }

    #[test]
    fn match_binding_unknown_returns_none() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(99, false, false, false, false), None);
    }

    #[test]
    fn escape_chain_drag_first() {
        assert_eq!(escape_chain(true, true, true, true), EscapeAction::CancelDrag);
    }

    #[test]
    fn escape_chain_workspace_overview() {
        assert_eq!(escape_chain(false, true, false, false), EscapeAction::ExitWorkspaceOverview);
    }

    #[test]
    fn escape_chain_overview() {
        assert_eq!(escape_chain(false, false, true, false), EscapeAction::ExitOverview);
    }

    #[test]
    fn escape_chain_focus() {
        assert_eq!(escape_chain(false, false, false, true), EscapeAction::ExitFocus);
    }

    #[test]
    fn escape_chain_reset() {
        assert_eq!(escape_chain(false, false, false, false), EscapeAction::ResetCamera);
    }

    #[test]
    fn match_binding_shelf_toggle() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(40, false, false, false, true), Some(Binding::ToggleShelf));
    }

    #[test]
    fn match_binding_send_to_shelf() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(116, false, false, false, true), Some(Binding::SendToShelf));
    }

    #[test]
    fn match_binding_launcher() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(65, false, false, false, true), Some(Binding::Launcher));
    }

    #[test]
    fn match_binding_close_app() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(25, false, false, false, true), Some(Binding::CloseApp));
    }

    #[test]
    fn match_binding_cycle_visuals() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(23, false, false, false, true), Some(Binding::ToggleSpatial));
    }

    #[test]
    fn match_binding_open_context_menu() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(135, false, false, false, false), Some(Binding::OpenContextMenu));
    }

    #[test]
    fn match_binding_help_overlay() {
        let nav = NavigationModel::new();
        assert_eq!(nav.match_binding(61, false, false, false, true), Some(Binding::HelpOverlay));
    }

    #[test]
    fn match_binding_reopen_closed() {
        let nav = NavigationModel::new();
        // Meta+Shift+T reopens; plain T and Meta+T must not.
        assert_eq!(nav.match_binding(28, false, true, false, true), Some(Binding::ReopenClosed));
        assert_eq!(nav.match_binding(28, false, false, false, false), None);
        assert_eq!(nav.match_binding(28, false, false, false, true), None);
    }
}
