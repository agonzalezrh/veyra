use crate::scene::VisualId;

/// Menu geometry, proportional to the display so the menu stays readable
/// on any panel size. All pixel values are framebuffer pixels.
#[derive(Debug, Clone, Copy)]
pub struct MenuMetrics {
    pub menu_width: f32,
    pub item_height: f32,
    /// Integer scale factor for the 5x7 bitmap glyphs.
    pub glyph_scale: f32,
}

impl MenuMetrics {
    /// Derive metrics from the framebuffer size. Baseline: 220px menu,
    /// 24px rows, 2x glyphs at 1280x720. The glyph fills ~58% of the row
    /// height at every size (round per-step, not even-only scales, so
    /// 1.5x/3x displays get 3x/6x glyphs instead of staying at 2x).
    pub fn for_framebuffer(w: f32, h: f32) -> Self {
        let su = (w / 1280.0).clamp(1.0, 2.5);
        let sv = (h / 720.0).clamp(1.0, 2.5);
        let item_height = 24.0 * sv;
        let glyph_scale = ((item_height * 0.58) / 7.0).round().clamp(2.0, 6.0);
        MenuMetrics { menu_width: 220.0 * su, item_height, glyph_scale }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MenuAction {
    Focus,
    Arrange,
    MoveToWorkspace(usize),
    Group,
    Ungroup,
    DeEmphasize,
    Restore,
    ResetTransform,
    Maximize,
    Fullscreen,
    Minimize,
    Close,
    Dismiss,
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: MenuAction) -> Self {
        MenuItem { label: label.into(), action }
    }
}

#[derive(Debug, Clone)]
pub struct ContextMenu {
    pub visible: bool,
    pub position: (f64, f64),
    pub target: Option<VisualId>,
    pub items: Vec<MenuItem>,
    pub selected: Option<usize>,
}

impl ContextMenu {
    pub fn new() -> Self {
        ContextMenu {
            visible: false,
            position: (0.0, 0.0),
            target: None,
            items: Vec::new(),
            selected: None,
        }
    }

    /// Show the context menu at the given screen position for the given visual.
    pub fn show(&mut self, x: f64, y: f64, target: VisualId, _workspace_count: usize) {
        self.visible = true;
        self.position = (x, y);
        self.target = Some(target);
        self.selected = None;

        self.items = vec![
            MenuItem::new("Focus", MenuAction::Focus),
            MenuItem::new("Arrange", MenuAction::Arrange),
            MenuItem::new("Move to Workspace ▸", MenuAction::MoveToWorkspace(0)),
            MenuItem::new("Group", MenuAction::Group),
            MenuItem::new("Ungroup", MenuAction::Ungroup),
            MenuItem::new("De-emphasize", MenuAction::DeEmphasize),
            MenuItem::new("Restore", MenuAction::Restore),
            MenuItem::new("Reset Transform", MenuAction::ResetTransform),
            MenuItem::new("Maximize", MenuAction::Maximize),
            MenuItem::new("Fullscreen", MenuAction::Fullscreen),
            MenuItem::new("Minimize", MenuAction::Minimize),
            MenuItem::new("Close", MenuAction::Close),
        ];
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
        self.target = None;
        self.selected = None;
        self.items.clear();
    }

    /// Rename the Maximize item for an already-maximized target.
    /// Called by the compositor right after `show`.
    pub fn set_maximize_label(&mut self, maximized: bool) {
        if !maximized {
            return;
        }
        if let Some(item) = self.items.iter_mut().find(|i| i.action == MenuAction::Maximize) {
            item.label = "Restore size".into();
        }
    }

    /// Returns true if the given screen position hits the menu.
    pub fn hit_test(&self, x: f64, y: f64, menu_width: f64, item_height: f64) -> bool {
        if !self.visible {
            return false;
        }
        let (mx, my) = self.position;
        let menu_height = self.items.len() as f64 * item_height;
        x >= mx && x <= mx + menu_width && y >= my && y <= my + menu_height
    }

    /// Returns the index of the item at the given screen position.
    pub fn item_at(&self, x: f64, y: f64, menu_width: f64, item_height: f64) -> Option<usize> {
        if !self.hit_test(x, y, menu_width, item_height) {
            return None;
        }
        let (_, my) = self.position;
        let idx = ((y - my) / item_height) as usize;
        if idx < self.items.len() { Some(idx) } else { None }
    }

    /// Select the next item in the menu (down arrow).
    pub fn select_next(&mut self) {
        if !self.visible || self.items.is_empty() { return; }
        let current = match self.selected {
            Some(i) => i,
            None => {
                self.selected = Some(0);
                return;
            }
        };
        self.selected = Some((current + 1) % self.items.len());
    }

    /// Select the previous item in the menu (up arrow).
    pub fn select_prev(&mut self) {
        if !self.visible || self.items.is_empty() { return; }
        let current = match self.selected {
            Some(i) => i,
            None => {
                self.selected = Some(self.items.len() - 1);
                return;
            }
        };
        self.selected = Some((current + self.items.len() - 1) % self.items.len());
    }

    /// Confirm the current selection and return the action.
    pub fn confirm_selection(&self) -> Option<MenuAction> {
        self.selected.and_then(|idx| self.items.get(idx)).map(|item| item.action)
    }
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_starts_hidden() {
        let menu = ContextMenu::new();
        assert!(!menu.visible);
        assert!(menu.target.is_none());
        assert!(menu.items.is_empty());
    }

    #[test]
    fn metrics_baseline_matches_verified_geometry() {
        let m = MenuMetrics::for_framebuffer(1280.0, 720.0);
        assert_eq!(m.menu_width, 220.0);
        assert_eq!(m.item_height, 24.0);
        assert_eq!(m.glyph_scale, 2.0);
    }

    #[test]
    fn metrics_scale_up_on_hidpi() {
        let m = MenuMetrics::for_framebuffer(2560.0, 1440.0);
        assert_eq!(m.menu_width, 440.0);
        assert_eq!(m.item_height, 48.0);
        assert_eq!(m.glyph_scale, 4.0); // 28px glyphs in 48px rows
        // Glyph fills ~58% of the row at every size (readable)
        let ink = 7.0 * m.glyph_scale;
        assert!((ink / m.item_height - 0.58).abs() < 0.06);
        // 1.5x display (e.g. 1080p panel): must get 3x glyphs, not 2x
        let m15 = MenuMetrics::for_framebuffer(1920.0, 1080.0);
        assert_eq!(m15.glyph_scale, 3.0);
        assert_eq!(m15.item_height, 36.0);
    }

    #[test]
    fn metrics_small_displays_keep_baseline() {
        let m = MenuMetrics::for_framebuffer(800.0, 600.0);
        assert_eq!(m.menu_width, 220.0);
        assert_eq!(m.item_height, 24.0);
        assert_eq!(m.glyph_scale, 2.0);
    }

    #[test]
    fn show_creates_items() {
        let mut menu = ContextMenu::new();
        let vid = VisualId(42);
        menu.show(100.0, 200.0, vid, 3);
        assert!(menu.visible);
        assert_eq!(menu.target, Some(vid));
        assert_eq!(menu.position, (100.0, 200.0));
        assert!(!menu.items.is_empty());
    }

    #[test]
    fn dismiss_clears_state() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        assert!(menu.visible);
        menu.dismiss();
        assert!(!menu.visible);
        assert!(menu.target.is_none());
        assert!(menu.items.is_empty());
    }

    #[test]
    fn hit_test_outside() {
        let mut menu = ContextMenu::new();
        menu.show(100.0, 100.0, VisualId(1), 3);
        assert!(!menu.hit_test(10.0, 10.0, 200.0, 24.0));
        assert!(menu.hit_test(110.0, 110.0, 200.0, 24.0));
    }

    #[test]
    fn item_at_returns_correct_index() {
        let mut menu = ContextMenu::new();
        menu.show(100.0, 100.0, VisualId(1), 3);
        // Item at y=100 is index 0, y=124 is index 1, etc.
        assert_eq!(menu.item_at(110.0, 100.0, 200.0, 24.0), Some(0));
        assert_eq!(menu.item_at(110.0, 124.0, 200.0, 24.0), Some(1));
        // Last item (index 8) is within menu height (100 + 9*24 = 316)
        assert_eq!(menu.item_at(110.0, 315.0, 200.0, 24.0), Some(8));
        // Below menu
        assert_eq!(menu.item_at(110.0, 400.0, 200.0, 24.0), None);
    }

    #[test]
    fn menu_has_expected_items() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::Focus)));
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::Arrange)));
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::Close)));
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::ResetTransform)));
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::Maximize)));
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::Fullscreen)));
        assert!(menu.items.iter().any(|i| matches!(i.action, MenuAction::Minimize)));
    }

    #[test]
    fn maximize_label_renames_when_maximized() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        let label = |m: &ContextMenu| {
            m.items
                .iter()
                .find(|i| i.action == MenuAction::Maximize)
                .map(|i| i.label.clone())
                .expect("maximize item present")
        };
        assert_eq!(label(&menu), "Maximize");
        menu.set_maximize_label(false);
        assert_eq!(label(&menu), "Maximize");
        menu.set_maximize_label(true);
        assert_eq!(label(&menu), "Restore size");
        menu.dismiss();
    }

    #[test]
    fn keyboard_navigate_next() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        assert_eq!(menu.selected, None);
        menu.select_next();
        assert_eq!(menu.selected, Some(0)); // Focus
        menu.select_next();
        assert_eq!(menu.selected, Some(1)); // Arrange
    }

    #[test]
    fn keyboard_navigate_prev() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        menu.select_prev();
        // Wraps around to last item
        let last = menu.items.len() - 1;
        assert_eq!(menu.selected, Some(last));
    }

    #[test]
    fn keyboard_navigate_wraps_around() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        let count = menu.items.len();
        // Navigate past the end
        for _ in 0..count + 1 {
            menu.select_next();
        }
        assert_eq!(menu.selected, Some(0)); // Back to start
    }

    #[test]
    fn keyboard_confirm_selection() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        // No selection initially
        assert!(menu.confirm_selection().is_none());
        menu.selected = Some(0);
        assert_eq!(menu.confirm_selection(), Some(MenuAction::Focus));
        menu.selected = Some(8); // Maximize
        assert_eq!(menu.confirm_selection(), Some(MenuAction::Maximize));
        menu.selected = Some(9); // Fullscreen (I7)
        assert_eq!(menu.confirm_selection(), Some(MenuAction::Fullscreen));
        menu.selected = Some(10); // Minimize (I5)
        assert_eq!(menu.confirm_selection(), Some(MenuAction::Minimize));
        menu.selected = Some(11); // Close
        assert_eq!(menu.confirm_selection(), Some(MenuAction::Close));
    }

    #[test]
    fn escape_dismisses_context_menu() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        assert!(menu.visible);
        menu.dismiss();
        assert!(!menu.visible);
        assert!(menu.target.is_none());
    }

    #[test]
    fn select_next_from_none_starts_at_zero() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        menu.selected = None;
        menu.select_next();
        assert_eq!(menu.selected, Some(0));
    }

    #[test]
    fn select_prev_from_none_starts_at_end() {
        let mut menu = ContextMenu::new();
        menu.show(0.0, 0.0, VisualId(1), 3);
        menu.selected = None;
        menu.select_prev();
        assert_eq!(menu.selected, Some(menu.items.len() - 1));
    }
}
