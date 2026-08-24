use crate::scene::VisualId;

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
            MenuItem::new("Close", MenuAction::Close),
        ];
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
        self.target = None;
        self.selected = None;
        self.items.clear();
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
        menu.selected = Some(8); // Close
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
