use cgmath::Matrix4;
use cgmath::Vector3;

use crate::scene::{Scene, Transform3D, VisualId};

/// A unique identifier for a spatial group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupId(pub u64);

impl GroupId {
    fn next() -> Self {
        use std::sync::atomic::AtomicU64;
        use std::sync::atomic::Ordering;
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        GroupId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// A spatial group is a set of visuals that move together.
///
/// CRITICAL: A group is NOT a Wayland object. It is presentation state only.
/// Groups contain presentation relationships, never Wayland protocol relationships.
///
/// The group's transform is composed with each member's local transform:
///   world_transform = group_transform * visual_local_transform
///
/// When the group moves, member local transforms are preserved — only the
/// group transform changes.
#[derive(Debug, Clone)]
pub struct SpatialGroup {
    pub id: GroupId,
    pub visual_ids: Vec<VisualId>,
    pub transform: Transform3D,
}

impl SpatialGroup {
    pub fn new(visual_ids: Vec<VisualId>) -> Self {
        SpatialGroup {
            id: GroupId::next(),
            visual_ids,
            transform: Transform3D::identity(),
        }
    }

    /// Compute the world matrix for this group.
    pub fn world_matrix(&self) -> Matrix4<f32> {
        self.transform.to_matrix()
    }

    /// Add a visual to this group.
    pub fn add(&mut self, vid: VisualId) {
        if !self.visual_ids.contains(&vid) {
            self.visual_ids.push(vid);
        }
    }

    /// Remove a visual from this group.
    pub fn remove(&mut self, vid: VisualId) {
        self.visual_ids.retain(|v| *v != vid);
    }

    /// Check if a visual is in this group.
    pub fn contains(&self, vid: VisualId) -> bool {
        self.visual_ids.contains(&vid)
    }

    /// Number of members.
    pub fn len(&self) -> usize {
        self.visual_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visual_ids.is_empty()
    }
}

// ── Scene integration ─────────────────────────────────────────────────

impl Scene {
    /// Create a new spatial group from the given visual IDs.
    /// Returns the new GroupId.
    /// All provided VisualId values are accepted — groups work at the
    /// VisualId level, regardless of whether the visual has content yet.
    pub fn create_group(&mut self, visual_ids: Vec<VisualId>) -> GroupId {
        let group = SpatialGroup::new(visual_ids);
        let id = group.id;
        self.groups.push(group);
        id
    }

    /// Add a visual to an existing group.
    /// Returns true if the group exists.
    pub fn add_to_group(&mut self, group_id: GroupId, vid: VisualId) -> bool {
        match self.groups.iter_mut().find(|g| g.id == group_id) {
            Some(g) => {
                g.add(vid);
                true
            }
            None => false,
        }
    }

    /// Remove a visual from a group. Does NOT destroy the visual.
    /// Returns true if the group existed.
    pub fn remove_from_group(&mut self, group_id: GroupId, vid: VisualId) -> bool {
        match self.groups.iter_mut().find(|g| g.id == group_id) {
            Some(g) => {
                g.remove(vid);
                true
            }
            None => false,
        }
    }

    /// Remove a visual from ALL groups it belongs to.
    pub fn remove_from_all_groups(&mut self, vid: VisualId) {
        for g in &mut self.groups {
            g.remove(vid);
        }
    }

    /// Remove (dissolve) a group. Members survive with their local transforms.
    /// Returns true if the group existed.
    pub fn remove_group(&mut self, group_id: GroupId) -> bool {
        let len_before = self.groups.len();
        self.groups.retain(|g| g.id != group_id);
        self.groups.len() < len_before
    }

    /// Get a group's world transform.
    pub fn group_transform(&self, group_id: GroupId) -> Option<Matrix4<f32>> {
        self.groups
            .iter()
            .find(|g| g.id == group_id)
            .map(|g| g.world_matrix())
    }

    /// Set a group's transform.
    pub fn set_group_transform(&mut self, group_id: GroupId, transform: Transform3D) -> bool {
        match self.groups.iter_mut().find(|g| g.id == group_id) {
            Some(g) => {
                g.transform = transform;
                true
            }
            None => false,
        }
    }

    /// Get the visual IDs in a group.
    pub fn group_visuals(&self, group_id: GroupId) -> Option<&[VisualId]> {
        self.groups
            .iter()
            .find(|g| g.id == group_id)
            .map(|g| g.visual_ids.as_slice())
    }

    /// Find the first group containing a visual. Returns None if not in any group.
    pub fn find_group_containing(&self, vid: VisualId) -> Option<GroupId> {
        self.groups
            .iter()
            .find(|g| g.contains(vid))
            .map(|g| g.id)
    }

    /// Find which group(s) a visual belongs to.
    pub fn groups_for_visual(&self, vid: VisualId) -> Vec<GroupId> {
        self.groups
            .iter()
            .filter(|g| g.contains(vid))
            .map(|g| g.id)
            .collect()
    }

    /// Compute a visual's world matrix, accounting for group transforms.
    ///
    /// world = group_transform * parent_transform * ... * local_transform
    pub fn world_matrix_with_groups(&self, id: VisualId) -> Matrix4<f32> {
        let local = self.world_matrix(id);
        // Find the first group containing this visual
        for g in &self.groups {
            if g.contains(id) {
                return g.world_matrix() * local;
            }
        }
        local
    }

    /// Get all groups.
    pub fn all_groups(&self) -> &[SpatialGroup] {
        &self.groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Transform3D;
    use cgmath::Vector3;
    use cgmath::Quaternion;
    use cgmath::Deg;
    use cgmath::Rotation3;

    fn make_scene_with_visuals(count: usize) -> Scene {
        let mut scene = Scene::default();
        for i in 0..count {
            let vid = VisualId(1000 + i as u64);
            scene.focus(Some(vid));
            // For actual transform testing, add to the internal visuals vec directly
            // (focus only tracks the ID, not the full Visual)
        }
        scene
    }

    #[test]
    fn create_group_returns_valid_id() {
        let mut scene = make_scene_with_visuals(3);
        let ids = vec![VisualId(1000), VisualId(1001)];
        let gid = scene.create_group(ids);
        assert!(scene.groups.iter().any(|g| g.id == gid));
    }

    #[test]
    fn create_group_accepts_any_visual_id() {
        let mut scene = Scene::default();
        // Groups accept any VisualId — they don't validate existence
        let ids = vec![VisualId(1000), VisualId(9999)];
        let gid = scene.create_group(ids);
        let members = scene.group_visuals(gid).unwrap();
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn add_to_group_and_verify_members() {
        let mut scene = make_scene_with_visuals(5);
        let ids = vec![VisualId(1000), VisualId(1001)];
        let gid = scene.create_group(ids);
        scene.add_to_group(gid, VisualId(1002));
        let members = scene.group_visuals(gid).unwrap();
        assert_eq!(members.len(), 3);
    }

    #[test]
    fn remove_from_group() {
        let mut scene = make_scene_with_visuals(3);
        let ids = vec![VisualId(1000), VisualId(1001), VisualId(1002)];
        let gid = scene.create_group(ids);
        scene.remove_from_group(gid, VisualId(1001));
        let members = scene.group_visuals(gid).unwrap();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&VisualId(1000)));
        assert!(members.contains(&VisualId(1002)));
    }

    #[test]
    fn dissolve_group_does_not_destroy_visuals() {
        let mut scene = make_scene_with_visuals(3);
        let ids = vec![VisualId(1000), VisualId(1001)];
        let gid = scene.create_group(ids);
        assert!(scene.remove_group(gid));
        assert!(!scene.groups.iter().any(|g| g.id == gid));
        // Visuals survive (their focus state in Scene persists)
        assert_eq!(scene.focused_id, Some(VisualId(1002)));
    }

    #[test]
    fn add_to_nonexistent_group_returns_false() {
        let mut scene = Scene::default();
        assert!(!scene.add_to_group(GroupId(999), VisualId(1)));
    }

    #[test]
    fn group_transform_composition() {
        let mut scene = make_scene_with_visuals(1);
        let gid = scene.create_group(vec![VisualId(1000)]);

        // Set group transform to translate by (100, 0, 0)
        let group_tf = Transform3D {
            position: Vector3::new(100.0, 0.0, 0.0),
            rotation: Quaternion::from_angle_z(Deg(0.0)),
            scale: Vector3::new(1.0, 1.0, 1.0),
        };
        scene.set_group_transform(gid, group_tf);

        // Verify world_matrix_with_groups includes group transform
        let world = scene.world_matrix_with_groups(VisualId(1000));
        // With identity local transform, world = group = translate(100, 0, 0)
        assert!((world[3][0] - 100.0).abs() < 0.01,
            "world x should include group translation");
    }

    #[test]
    fn groups_for_visual() {
        let mut scene = make_scene_with_visuals(5);
        let gid1 = scene.create_group(vec![VisualId(1000), VisualId(1001)]);
        let gid2 = scene.create_group(vec![VisualId(1000), VisualId(1002)]);

        let groups = scene.groups_for_visual(VisualId(1000));
        assert_eq!(groups.len(), 2);
        assert!(groups.contains(&gid1));
        assert!(groups.contains(&gid2));
    }

    #[test]
    fn remove_visual_from_all_groups() {
        let mut scene = make_scene_with_visuals(3);
        let gid = scene.create_group(vec![VisualId(1000), VisualId(1001)]);
        scene.remove_from_all_groups(VisualId(1000));
        let members = scene.group_visuals(gid).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0], VisualId(1001));
    }

    #[test]
    fn create_empty_group() {
        let mut scene = Scene::default();
        let gid = scene.create_group(vec![]);
        let members = scene.group_visuals(gid).unwrap();
        assert!(members.is_empty());
    }

    #[test]
    fn group_id_is_unique() {
        let id1 = GroupId::next();
        let id2 = GroupId::next();
        assert_ne!(id1, id2);
    }

    #[test]
    fn group_len_and_empty() {
        let g = SpatialGroup::new(vec![VisualId(1), VisualId(2)]);
        assert_eq!(g.len(), 2);
        assert!(!g.is_empty());

        let empty = SpatialGroup::new(vec![]);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
    }
}
