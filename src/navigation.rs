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
}

/// Central model for all compositor keyboard navigation bindings.
#[derive(Debug)]
pub struct NavigationModel {
    pub bindings: Vec<(Binding, KeyBinding)>,
}

impl NavigationModel {
    pub fn new() -> Self {
        // XKB keycodes (Linux evdev + 8):
        // 23=Tab, 24=Q, 32=O, 33=P, 36=Enter, 40=D, 41=F, 66=M,
        // 67=F1, 68=F2, 69=F3, 71=F5, 72=F6,
        // 110=Home, 111=Up, 116=Down, 135=Menu, 65=Space, 61=/
        use Binding::*;
        let bindings = vec![
            (ToggleSpatial,          KeyBinding::new(23)),               // Tab
            (ToggleSpatial,          KeyBinding::new(71)),               // F5
            (ToggleFocus,            KeyBinding::new(72)),               // F6
            (ToggleOverview,         KeyBinding::new(32)),               // O
            (ToggleWorkspaceOverview,KeyBinding::new(33)),               // P
            (WorkspaceNext,          KeyBinding::ctrl(23)),              // Ctrl+Tab
            (WorkspacePrev,          KeyBinding::ctrl_shift(23)),        // Ctrl+Shift+Tab
            (AppNext,                KeyBinding::alt(23)),               // Alt+Tab
            (AppPrev,                KeyBinding::alt_shift(23)),         // Alt+Shift+Tab
            (DeEmphasize,            KeyBinding::new(66)),               // M
            (FrameSelected,          KeyBinding::new(41)),               // F
            (FrameAll,               KeyBinding::new(110)),              // Home
            (ToggleShelf,            KeyBinding::meta(40)),              // Meta+D
            (SendToShelf,            KeyBinding::meta(116)),             // Meta+Down
            (Launcher,               KeyBinding::meta(65)),              // Meta+Space
            (Escape,                 KeyBinding::new(9)),                // Escape
            (CloseApp,               KeyBinding::meta(25)),              // Meta+W (25 = XKB W)
            (CycleVisuals,           KeyBinding::meta(23)),              // Meta+Tab
            (OpenContextMenu,        KeyBinding::new(135)),              // Menu key
            (HelpOverlay,            KeyBinding::meta(61)),              // Meta+/
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
        assert_eq!(nav.match_binding(23, false, false, false, false), Some(Binding::ToggleSpatial));
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
        assert_eq!(nav.match_binding(23, false, false, false, true), Some(Binding::CycleVisuals));
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
}
