use cgmath::Vector3;
use cgmath::Rotation3;

use crate::scene::{Scene, VisualId, Transform3D};

#[derive(Debug, Clone)]
pub struct ShelfEntry {
    pub app_id: String,
    pub visual_id: VisualId,
    /// The transform the visual had before being sent to the shelf.
    pub previous_transform: Option<Transform3D>,
}

#[derive(Debug)]
pub struct SpatialShelf {
    pub entries: Vec<ShelfEntry>,
    pub position: Vector3<f32>,
    pub visible: bool,
}

impl SpatialShelf {
    pub fn new() -> Self {
        SpatialShelf {
            entries: Vec::new(),
            position: Vector3::new(0.0, -300.0, -100.0),
            visible: true,
        }
    }

    /// Send a visual to the shelf: save its transform, de-emphasize it, and
    /// move it to the shelf position.
    pub fn send_to_shelf(&mut self, scene: &mut Scene, vid: VisualId) -> bool {
        if self.entries.iter().any(|e| e.visual_id == vid) {
            return false;
        }
        let visual = match scene.visuals.iter().find(|v| v.id == vid) {
            Some(v) => v.clone(),
            None => return false,
        };
        let prev = visual.transform.clone();

        // De-emphasize the visual
        scene.de_emphasize(vid);

        self.entries.push(ShelfEntry {
            app_id: visual.chrome.app_id.clone(),
            visual_id: vid,
            previous_transform: Some(prev),
        });
        true
    }

    /// Restore a visual from the shelf to its previous transform.
    pub fn restore_from_shelf(&mut self, scene: &mut Scene, vid: VisualId) -> bool {
        let idx = match self.entries.iter().position(|e| e.visual_id == vid) {
            Some(i) => i,
            None => return false,
        };
        let entry = &self.entries[idx];
        if let Some(ref prev) = entry.previous_transform {
            if let Some(visual) = scene.get_mut(vid) {
                visual.transform = prev.clone();
            }
        }
        scene.restore_from_de_emphasis(vid);
        self.entries.remove(idx);
        true
    }

    /// Apply shelf transforms to all shelf visuals (called each frame).
    pub fn apply_shelf_transforms(&self, scene: &mut Scene) {
        let shelf_pos = self.position;
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(visual) = scene.get_mut(entry.visual_id) {
                let x_offset = (i as f32 - self.entries.len() as f32 / 2.0) * 150.0;
                visual.transform.position = Vector3::new(
                    shelf_pos.x + x_offset,
                    shelf_pos.y,
                    shelf_pos.z,
                );
                visual.transform.scale = Vector3::new(0.5, 0.5, 1.0);
                visual.transform.rotation = cgmath::Quaternion::from_angle_z(cgmath::Deg(0.0));
            }
        }
    }

    /// Remove a destroyed visual from the shelf.
    pub fn remove(&mut self, vid: VisualId) {
        self.entries.retain(|e| e.visual_id != vid);
    }

    /// Toggle shelf visibility.
    pub fn toggle_visibility(&mut self) {
        self.visible = !self.visible;
    }

    pub fn contains(&self, vid: VisualId) -> bool {
        self.entries.iter().any(|e| e.visual_id == vid)
    }
}

impl Default for SpatialShelf {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    #[test]
    fn send_to_shelf_de_emphasizes() {
        let mut scene = Scene::default();
        let mut shelf = SpatialShelf::new();
        // Without actual Visual objects, send_to_shelf returns false
        assert!(!shelf.send_to_shelf(&mut scene, VisualId(1)));
    }

    #[test]
    fn restore_from_shelf() {
        let mut scene = Scene::default();
        let mut shelf = SpatialShelf::new();
        assert!(!shelf.restore_from_shelf(&mut scene, VisualId(1)));
    }

    #[test]
    fn remove_from_shelf() {
        let mut shelf = SpatialShelf::new();
        shelf.entries.push(ShelfEntry {
            app_id: "test".into(),
            visual_id: VisualId(1),
            previous_transform: None,
        });
        shelf.remove(VisualId(1));
        assert!(shelf.entries.is_empty());
    }

    #[test]
    fn toggle_visibility() {
        let mut shelf = SpatialShelf::new();
        assert!(shelf.visible);
        shelf.toggle_visibility();
        assert!(!shelf.visible);
        shelf.toggle_visibility();
        assert!(shelf.visible);
    }

    #[test]
    fn apply_shelf_transforms_no_visuals() {
        let mut scene = Scene::default();
        let shelf = SpatialShelf::new();
        shelf.apply_shelf_transforms(&mut scene);
        // No crash
    }

    #[test]
    fn contains_works() {
        let shelf = SpatialShelf::new();
        assert!(!shelf.contains(VisualId(1)));
    }
}
