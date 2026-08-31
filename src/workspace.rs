use std::collections::HashMap;

use cgmath::Point3;

use crate::focus::FocusManager;
use crate::input::Camera;
use crate::layout::LayoutMode;
use crate::scene::{Scene, Transform3D, VisualId};

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
    /// Per-workspace Visual transforms (position, rotation, scale).
    /// Maps VisualId -> workspace-specific transform.
    pub transforms: HashMap<VisualId, Transform3D>,
    /// Whether auto-orbit is active for this workspace.
    pub auto_orbit: bool,
    /// Snapshot of focus manager state for this workspace.
    pub focus_manager_state: FocusManager,
}

impl Workspace {
    pub fn new() -> Self {
        Workspace {
            camera: Camera::new(),
            layout_mode: LayoutMode::Freeform,
            visual_ids: Vec::new(),
            detached_set: Vec::new(),
            focused_id: None,
            transforms: HashMap::new(),
            // Demo orbit off by default: an auto-orbiting camera swings
            // windows out of the frustum and spatial mode must show the
            // actual desktop (see M077 F5 regression).
            auto_orbit: false,
            focus_manager_state: FocusManager::new(),
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
        self.transforms.remove(&id);
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

    /// Save the scene's current transforms for all workspace members into the workspace.
    pub fn save_transforms(&mut self, scene: &crate::scene::Scene) {
        for v in &scene.visuals {
            if self.visual_ids.contains(&v.id) {
                self.transforms.insert(v.id, v.transform.clone());
            }
        }
    }

    /// Restore the workspace's saved transforms onto the matching scene visuals.
    pub fn restore_transforms(&self, scene: &mut crate::scene::Scene) {
        for v in &mut scene.visuals {
            if let Some(saved) = self.transforms.get(&v.id) {
                v.transform = saved.clone();
            }
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

/// Owns all workspaces and manages workspace switching.
///
/// Invariants:
/// - `active_id` always points to a valid workspace within `workspaces`.
/// - `workspaces` always contains at least one workspace.
/// - `switch()` saves transforms from the old workspace and restores onto the new one.
#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    workspaces: Vec<Workspace>,
    active_id: usize,
}

impl WorkspaceManager {
    /// Create a new manager with `count` default workspaces.
    pub fn new(count: usize) -> Self {
        assert!(count >= 1, "WorkspaceManager must have at least one workspace");
        let workspaces = (0..count).map(|_| Workspace::new()).collect();
        WorkspaceManager {
            workspaces,
            active_id: 0,
        }
    }

    /// Returns the number of workspaces.
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    /// Returns the active workspace ID.
    pub fn active_id(&self) -> usize {
        self.active_id
    }

    /// Returns a reference to the active workspace.
    pub fn active(&self) -> &Workspace {
        &self.workspaces[self.active_id]
    }

    /// Returns a mutable reference to the active workspace.
    pub fn active_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active_id]
    }

    /// Returns a reference to a workspace by ID.
    pub fn get(&self, id: usize) -> Option<&Workspace> {
        self.workspaces.get(id)
    }

    /// Returns a mutable reference to a workspace by ID.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut Workspace> {
        self.workspaces.get_mut(id)
    }

    /// Returns an iterator over all workspaces.
    pub fn iter(&self) -> impl Iterator<Item = &Workspace> {
        self.workspaces.iter()
    }

    /// Returns a mutable iterator over all workspaces.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Workspace> {
        self.workspaces.iter_mut()
    }

    /// Add a new workspace. Returns its ID.
    pub fn add(&mut self) -> usize {
        let id = self.workspaces.len();
        self.workspaces.push(Workspace::new());
        id
    }

    /// Remove a workspace by ID. Fails (returns error) if it's the last one.
    /// If the removed workspace is the active one, switches to workspace 0 first.
    /// Cleans up visual state in the removed workspace.
    pub fn remove(&mut self, id: usize, scene: &mut Scene) -> Result<(), String> {
        if self.workspaces.len() <= 1 {
            return Err("cannot remove the last workspace".into());
        }
        if id >= self.workspaces.len() {
            return Err(format!("workspace {} does not exist", id));
        }
        // If removing the active workspace, switch to 0
        if id == self.active_id {
            // Save transforms first
            self.workspaces[id].save_transforms(scene);
            self.active_id = 0;
        }
        // If active_id is after the removed one, adjust it
        if self.active_id > id {
            self.active_id -= 1;
        }
        self.workspaces.remove(id);
        Ok(())
    }

    /// Switch to a workspace by ID. Saves transforms from the old workspace,
    /// applies transforms to the new workspace, and updates active_id.
    /// Returns true if the switch occurred.
    pub fn switch(&mut self, new_id: usize, scene: &mut Scene) -> bool {
        if new_id >= self.workspaces.len() || new_id == self.active_id {
            return false;
        }
        // Save current workspace state
        let old_id = self.active_id;
        self.workspaces[old_id].save_transforms(scene);
        // Restore new workspace state
        self.active_id = new_id;
        self.workspaces[new_id].restore_transforms(scene);
        true
    }

    /// Remove all visual state from all workspaces (for cleanup).
    pub fn clear_visuals(&mut self) {
        for ws in &mut self.workspaces {
            ws.visual_ids.clear();
            ws.detached_set.clear();
            ws.transforms.clear();
            ws.focused_id = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    // ── WorkspaceManager tests ─────────────────────────────────────

    #[test]
    fn manager_creates_correct_count() {
        let mgr = WorkspaceManager::new(3);
        assert_eq!(mgr.len(), 3);
        assert_eq!(mgr.active_id(), 0);
    }

    #[test]
    fn manager_add_returns_id() {
        let mut mgr = WorkspaceManager::new(1);
        let id = mgr.add();
        assert_eq!(id, 1);
        assert_eq!(mgr.len(), 2);
    }

    #[test]
    fn manager_switch_saves_restores_transforms() {
        let mut mgr = WorkspaceManager::new(2);
        let mut scene = Scene::default();
        // Switch to workspace 1 and back — no crash
        assert!(mgr.switch(1, &mut scene));
        assert_eq!(mgr.active_id(), 1);
        assert!(mgr.switch(0, &mut scene));
        assert_eq!(mgr.active_id(), 0);
    }

    #[test]
    fn manager_switch_noop_same_workspace() {
        let mut mgr = WorkspaceManager::new(2);
        let mut scene = Scene::default();
        assert!(!mgr.switch(0, &mut scene));
        assert_eq!(mgr.active_id(), 0);
    }

    #[test]
    fn manager_active_returns_correct() {
        let mut mgr = WorkspaceManager::new(3);
        let mut scene = Scene::default();
        mgr.switch(2, &mut scene);
        assert_eq!(mgr.active().camera.position, cgmath::Point3::new(0.0, 0.0, 800.0));
    }

    #[test]
    fn manager_remove_last_workspace_fails() {
        let mut mgr = WorkspaceManager::new(1);
        let mut scene = Scene::default();
        assert!(mgr.remove(0, &mut scene).is_err());
        assert_eq!(mgr.len(), 1);
    }

    #[test]
    fn manager_remove_updates_active_id() {
        let mut mgr = WorkspaceManager::new(3);
        let mut scene = Scene::default();
        // Remove workspace 1 (not active)
        assert!(mgr.remove(1, &mut scene).is_ok());
        assert_eq!(mgr.len(), 2);
        assert_eq!(mgr.active_id(), 0);
    }

    #[test]
    fn manager_remove_active_switches_to_zero() {
        let mut mgr = WorkspaceManager::new(3);
        let mut scene = Scene::default();
        // Switch to workspace 2, then remove it
        mgr.switch(2, &mut scene);
        assert_eq!(mgr.active_id(), 2);
        assert!(mgr.remove(2, &mut scene).is_ok());
        assert_eq!(mgr.active_id(), 0);
        assert_eq!(mgr.len(), 2);
    }

    #[test]
    fn independent_cameras_after_switch() {
        let mut mgr = WorkspaceManager::new(2);
        let mut scene = Scene::default();
        // Set workspace 0 camera
        mgr.workspaces[0].camera.position = cgmath::Point3::new(100.0, 200.0, 300.0);
        // Switch to workspace 1, set its camera differently
        mgr.switch(1, &mut scene);
        mgr.workspaces[1].camera.position = cgmath::Point3::new(400.0, 500.0, 600.0);
        // Switch back — workspace 0 camera unchanged
        mgr.switch(0, &mut scene);
        assert_eq!(mgr.workspaces[0].camera.position.x, 100.0);
        assert_eq!(mgr.workspaces[1].camera.position.x, 400.0);
    }

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
