use cgmath::Point3;

use crate::input::Camera;
use crate::layout::LayoutMode;
use crate::scene::VisualId;

/// A workspace is a spatial presentation of a subset of the global Scene.
///
/// Each workspace has:
/// - Its own camera (position, yaw, pitch) — switching restores the view
/// - Its own set of visible visuals (visual_ids)
/// - Its own layout mode and detached set
/// - Its own focused visual
///
/// Wayland surfaces are compositor-global; Visuals have workspace membership.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub camera: Camera,
    pub layout_mode: LayoutMode,
    /// Which visuals are members of this workspace.
    pub visual_ids: Vec<VisualId>,
    /// Per-workspace detached tracking (user-manipulated visuals).
    pub detached_set: Vec<VisualId>,
    /// The focused visual in this workspace.
    pub focused_id: Option<VisualId>,
}

impl Workspace {
    pub fn new() -> Self {
        Workspace {
            camera: Camera::new(),
            layout_mode: LayoutMode::Freeform,
            visual_ids: Vec::new(),
            detached_set: Vec::new(),
            focused_id: None,
        }
    }

    pub fn add(&mut self, id: VisualId) {
        if !self.visual_ids.contains(&id) {
            self.visual_ids.push(id);
        }
    }

    pub fn remove(&mut self, id: VisualId) {
        self.visual_ids.retain(|v| *v != id);
        self.detached_set.retain(|v| *v != id);
        if self.focused_id == Some(id) {
            self.focused_id = None;
        }
    }

    pub fn contains(&self, id: VisualId) -> bool {
        self.visual_ids.contains(&id)
    }

    pub fn focus(&mut self, id: Option<VisualId>) {
        self.focused_id = id;
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    #[test]
    fn new_workspace_defaults() {
        let ws = Workspace::new();
        assert_eq!(ws.layout_mode, LayoutMode::Freeform);
        assert_eq!(ws.camera.position, Point3::new(0.0, 0.0, 800.0));
    }

    #[test]
    fn workspace_camera_independence() {
        let mut ws1 = Workspace::new();
        let mut ws2 = Workspace::new();
        ws1.camera.position = Point3::new(100.0, 200.0, 300.0);
        ws2.camera.position = Point3::new(400.0, 500.0, 600.0);
        ws1.layout_mode = LayoutMode::Flat;
        ws2.layout_mode = LayoutMode::Grid { columns: 3 };
        assert_ne!(ws1.camera.position, ws2.camera.position);
        assert_ne!(ws1.layout_mode, ws2.layout_mode);
    }

    #[test]
    fn workspace_switch_preserves_state() {
        let mut ws1 = Workspace::new();
        ws1.camera.position = Point3::new(100.0, 200.0, 300.0);
        ws1.layout_mode = LayoutMode::Flat;

        let mut ws2 = Workspace::new();
        ws2.camera.position = Point3::new(400.0, 500.0, 600.0);
        ws2.layout_mode = LayoutMode::Grid { columns: 2 };

        // Simulate switching: save current to ws1, restore ws2
        let (cam1, lay1) = (ws1.camera.clone(), ws1.layout_mode);
        let (cam2, lay2) = (ws2.camera.clone(), ws2.layout_mode);
        assert_eq!(cam1.position, Point3::new(100.0, 200.0, 300.0));
        assert_eq!(cam2.position, Point3::new(400.0, 500.0, 600.0));
        assert_eq!(lay1, LayoutMode::Flat);
        assert_eq!(lay2, LayoutMode::Grid { columns: 2 });
    }

    #[test]
    fn add_visual_sets_membership() {
        let mut ws = Workspace::new();
        let vid = VisualId(42);
        ws.add(vid);
        assert!(ws.contains(vid));
        assert_eq!(ws.visual_ids.len(), 1);
    }

    #[test]
    fn remove_visual_cleans_state() {
        let mut ws = Workspace::new();
        let vid = VisualId(42);
        ws.add(vid);
        ws.detached_set.push(vid);
        ws.focus(Some(vid));
        ws.remove(vid);
        assert!(!ws.contains(vid));
        assert!(!ws.detached_set.contains(&vid));
        assert_eq!(ws.focused_id, None);
    }

    #[test]
    fn focus_isolation_between_workspaces() {
        let mut ws1 = Workspace::new();
        let mut ws2 = Workspace::new();
        ws1.focus(Some(VisualId(10)));
        ws2.focus(Some(VisualId(20)));
        assert_eq!(ws1.focused_id, Some(VisualId(10)));
        assert_eq!(ws2.focused_id, Some(VisualId(20)));
    }

    #[test]
    fn detached_isolation_between_workspaces() {
        let mut ws1 = Workspace::new();
        let mut ws2 = Workspace::new();
        ws1.detached_set.push(VisualId(1));
        ws2.detached_set.push(VisualId(2));
        assert!(ws1.detached_set.contains(&VisualId(1)));
        assert!(!ws2.detached_set.contains(&VisualId(1)));
    }
}
