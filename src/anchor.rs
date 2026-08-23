use cgmath::Vector3;

use crate::scene::{Scene, VisualId};

/// Which edge of a visual's bounding box to anchor to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    Front,
    Back,
}

/// A spatial anchor is a position in workspace coordinates,
/// resolved from a scene-independent reference.
///
/// Anchors are purely workspace-coordinate based and are
/// independent of the camera, rendering, or any visual
/// representation. Changing the camera must never change
/// an anchor's resolved position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpatialAnchor {
    /// The workspace origin (0, 0, 0).
    WorkspaceOrigin,
    /// The center of a visual's current bounds.
    VisualCenter(VisualId),
    /// A specific edge of a visual's bounds.
    VisualEdge(VisualId, Edge),
    /// An explicit custom position.
    Custom(Vector3<f32>),
}

/// Resolve a spatial anchor to a workspace-coordinate position.
///
/// Returns `None` if the anchor references a visual that doesn't exist.
/// This is a pure function — no side effects, no camera involvement.
pub fn resolve_anchor(scene: &Scene, anchor: &SpatialAnchor) -> Option<Vector3<f32>> {
    match anchor {
        SpatialAnchor::WorkspaceOrigin => Some(Vector3::new(0.0, 0.0, 0.0)),
        SpatialAnchor::VisualCenter(vid) => {
            let visual = scene.visuals.iter().find(|v| &v.id == vid)?;
            Some(visual.transform.position)
        }
        SpatialAnchor::VisualEdge(vid, edge) => {
            let visual = scene.visuals.iter().find(|v| &v.id == vid)?;
            let half_w = visual.total_width() * 0.5;
            let half_h = visual.total_height() * 0.5;
            let center = visual.transform.position;
            let pos = match edge {
                Edge::Left => Vector3::new(center.x - half_w, center.y, center.z),
                Edge::Right => Vector3::new(center.x + half_w, center.y, center.z),
                Edge::Top => Vector3::new(center.x, center.y + half_h, center.z),
                Edge::Bottom => Vector3::new(center.x, center.y - half_h, center.z),
                Edge::Front => Vector3::new(center.x, center.y, center.z - 0.5),
                Edge::Back => Vector3::new(center.x, center.y, center.z + 0.5),
            };
            Some(pos)
        }
        SpatialAnchor::Custom(pos) => Some(*pos),
    }
}

/// Resolve an anchor, returning the origin as default if the anchor
/// references a non-existent visual.
pub fn resolve_anchor_or_default(scene: &Scene, anchor: &SpatialAnchor) -> Vector3<f32> {
    resolve_anchor(scene, anchor).unwrap_or(Vector3::new(0.0, 0.0, 0.0))
}

/// Compute the axis-aligned bounding box of a visual in workspace coordinates.
/// Returns (min, max) corners.
pub fn visual_aabb(scene: &Scene, vid: VisualId) -> Option<(Vector3<f32>, Vector3<f32>)> {
    let visual = scene.visuals.iter().find(|v| v.id == vid)?;
    let half_w = visual.total_width() * 0.5;
    let half_h = visual.total_height() * 0.5;
    let p = visual.transform.position;
    let min = Vector3::new(p.x - half_w, p.y - half_h, p.z);
    let max = Vector3::new(p.x + half_w, p.y + half_h, p.z);
    Some((min, max))
}

/// Compute the axis-aligned bounding box for a set of visual IDs.
/// Returns (min, max) corners, or None if none of the IDs exist.
pub fn visual_set_aabb(scene: &Scene, ids: &[VisualId]) -> Option<(Vector3<f32>, Vector3<f32>)> {
    let mut initialized = false;
    let mut min = Vector3::new(f32::MAX, f32::MAX, f32::MAX);
    let mut max = Vector3::new(f32::MIN, f32::MIN, f32::MIN);
    for vid in ids {
        if let Some((vmin, vmax)) = visual_aabb(scene, *vid) {
            min.x = min.x.min(vmin.x);
            min.y = min.y.min(vmin.y);
            min.z = min.z.min(vmin.z);
            max.x = max.x.max(vmax.x);
            max.y = max.y.max(vmax.y);
            max.z = max.z.max(vmax.z);
            initialized = true;
        }
    }
    if initialized { Some((min, max)) } else { None }
}

/// Compute the AABB center for a set of visuals.
pub fn visual_set_center(scene: &Scene, ids: &[VisualId]) -> Option<Vector3<f32>> {
    visual_set_aabb(scene, ids).map(|(min, max)| (min + max) * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Camera;

    /// Helper to add a visual to a scene with given position and size.
    /// We work around the GlesTexture requirement by using the fact that
    /// Scene methods operate on VisualId — but for resolve_anchor we
    /// need actual Visual entries. We push dummy entries directly.
    fn add_dummy(scene: &mut Scene, x: f32, y: f32, z: f32, w: f32, h: f32) -> VisualId {
        // We can't construct Visuals without a real GlesTexture in tests,
        // but we CAN test anchor resolution by creating a Scene and
        // manually constructing the internal state. The visual ids are
        // created by VisualId::next() internally — we just use scene.focus
        // to create entries in the scene tracking.
        //
        // For anchor tests we need actual visuals. We'll simulate by
        // working with VisualId tracking. The pure anchor functions
        // test data flow, not visual rendering.
        //
        // Use the focus tracking as a proxy for "visual exists"
        let id = VisualId(1000 + (h as u64));
        scene.focus(Some(id));
        scene.select(Some(id));
        id
    }

    #[test]
    fn workspace_origin_is_zero() {
        let scene = Scene::default();
        let pos = resolve_anchor(&scene, &SpatialAnchor::WorkspaceOrigin);
        assert_eq!(pos, Some(Vector3::new(0.0, 0.0, 0.0)));
    }

    #[test]
    fn invalid_visual_id_returns_none() {
        let scene = Scene::default();
        assert!(resolve_anchor(&scene, &SpatialAnchor::VisualCenter(VisualId(999))).is_none());
        assert!(resolve_anchor(&scene, &SpatialAnchor::VisualEdge(VisualId(999), Edge::Left)).is_none());
    }

    #[test]
    fn custom_anchor_returns_given_position() {
        let scene = Scene::default();
        let pos = resolve_anchor(&scene, &SpatialAnchor::Custom(Vector3::new(42.0, 99.0, -10.0)));
        assert_eq!(pos, Some(Vector3::new(42.0, 99.0, -10.0)));
    }

    #[test]
    fn resolve_or_default_falls_back() {
        let scene = Scene::default();
        let pos = resolve_anchor_or_default(&scene, &SpatialAnchor::VisualCenter(VisualId(999)));
        assert_eq!(pos, Vector3::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn camera_independent_resolution_no_crash() {
        // Camera movement should not affect anchor computation.
        // This is a compile-time safety check + basic logic test.
        let scene = Scene::default();
        let anchor = SpatialAnchor::WorkspaceOrigin;
        let pos1 = resolve_anchor(&scene, &anchor);
        let mut cam = Camera::new();
        cam.position = cgmath::Point3::new(999.0, 999.0, 999.0);
        cam.yaw = 1.5;
        cam.pitch = 0.8;
        drop(cam);
        let pos2 = resolve_anchor(&scene, &anchor);
        assert_eq!(pos1, pos2, "camera must not affect anchor position");
    }

    #[test]
    fn visual_set_center_empty_returns_none() {
        let scene = Scene::default();
        assert!(visual_set_aabb(&scene, &[VisualId(999)]).is_none());
    }

    #[test]
    fn visual_aabb_unknown_id() {
        let scene = Scene::default();
        assert!(visual_aabb(&scene, VisualId(999)).is_none());
    }
}
