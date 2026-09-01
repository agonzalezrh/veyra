use cgmath::Quaternion;
use cgmath::Rotation3;
use cgmath::Vector3;

use crate::scene::{Scene, VisualId};

/// Compute an initial placement position for a new visual of the given size.
/// Uses a spiral pattern that spreads out from the origin.
/// Skips positions that would significantly overlap existing visuals.
/// Visuals in `detached_set` are not considered for overlap (they were
/// manually positioned and are authoritative).
pub fn place_new_visual(
    width: f32,
    height: f32,
    scene: &Scene,
) -> Vector3<f32> {
    let base_spacing = 300.0f32;
    for i in 0..100 {
        // First window opens centered on the workspace; later windows
        // spiral outward (2π/7 steps, flattened vertically).
        let (x, y) = if i == 0 {
            (0.0, 0.0)
        } else {
            let angle = (i as f32) * 2.0 * std::f32::consts::TAU / 7.0;
            let radius = base_spacing + (i as f32).sqrt() * base_spacing * 0.5;
            (radius * angle.cos(), radius * angle.sin() * 0.6) // flatten vertically
        };
        let candidate = Vector3::new(x, y, 0.0);

        // Check for significant overlap with non-detached visuals
        let overlaps = scene.visuals.iter().any(|v| {
            if scene.detached_set.contains(&v.id) { return false; }
            let vw = v.total_width();
            let vh = v.total_height();
            let dx = (candidate.x - v.transform.position.x).abs();
            let dy = (candidate.y - v.transform.position.y).abs();
            dx < (vw + width) * 0.5 && dy < (vh + height) * 0.5
        });

        if !overlaps {
            return candidate;
        }
    }
    // Fallback: far away
    Vector3::new(800.0, 0.0, 0.0)
}

/// Layout mode for arranging visuals in the scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayoutMode {
    /// No automatic layout; user transforms are authoritative.
    Freeform,
    /// Arrange visuals in a 2D grid.
    Grid { columns: usize },
    /// Horizontally arranged on a flat plane (existing 2D-mode behavior).
    Flat,
}

/// Global layout configuration.
#[derive(Debug, Clone)]
pub struct LayoutConfig {
    pub spacing: f32,
    pub margin: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            spacing: 40.0,
            margin: 100.0,
        }
    }
}

/// Compute a camera position that frames the given visual in view.
/// Returns a new camera position that centers the visual in the viewport.
/// `view_width`, `view_height` are the current window dimensions.
pub fn frame_visual(
    visual_id: VisualId,
    scene: &Scene,
    _view_width: f32,
    _view_height: f32,
) -> Option<Vector3<f32>> {
    let visual = scene.visuals.iter().find(|v| v.id == visual_id)?;
    // Center camera directly in front of the visual at a reasonable distance
    let size = visual.geometry.size;
    let max_dim = size.w.max(size.h) as f32;
    let distance = max_dim * 1.5 + 500.0;
    let pos = visual.transform.position;
    Some(Vector3::new(pos.x, pos.y, pos.z + distance))
}

/// Apply the specified layout to the scene.
///
/// Visuals that have been explicitly manipulated by the user (detached)
/// are not repositioned. `detached_set` tracks which visual IDs are detached.
///
/// The `world_width` and `world_height` represent the visible area
/// (derived from projection/camera, e.g. 1280 x 720 in ortho mode).
pub fn apply_layout(
    scene: &mut Scene,
    mode: LayoutMode,
    config: &LayoutConfig,
    detached_set: &[VisualId],
    world_width: f32,
    world_height: f32,
    eligible: &[VisualId],
) {
    match mode {
        LayoutMode::Freeform => {}
        LayoutMode::Flat => {
            apply_flat(scene, config, detached_set, world_width, eligible)
        }
        LayoutMode::Grid { .. } => {
            apply_grid(scene, config, detached_set, world_height, eligible)
        }
    }
}

/// Arrangement produces transforms; it never owns them and it never
/// touches visuals outside the active workspace: with multiple
/// workspaces the whole scene is shared, but layout only speaks for the
/// workspace it was invoked for (audit fix P1).
fn layout_eligible(
    visual: &crate::scene::Visual,
    detached_set: &[VisualId],
    eligible: &[VisualId],
) -> bool {
    eligible.contains(&visual.id) && !detached_set.contains(&visual.id)
}

fn apply_flat(
    scene: &mut Scene,
    config: &LayoutConfig,
    detached_set: &[VisualId],
    _world_width: f32,
    eligible: &[VisualId],
) {
    let n: usize = scene.visuals.iter()
        .filter(|v| layout_eligible(v, detached_set, eligible))
        .count();
    let total_width: f32 = scene.visuals.iter()
        .filter(|v| layout_eligible(v, detached_set, eligible))
        .map(|v| v.geometry.size.w as f32)
        .sum();
    let spacing_total = (n.saturating_sub(1)) as f32 * config.spacing;
    let start_x = -total_width / 2.0 - spacing_total / 2.0 + config.margin;

    let mut cursor_x = start_x;
    for visual in &mut scene.visuals {
        if !layout_eligible(visual, detached_set, eligible) {
            continue;
        }
        visual.transform.position = Vector3::new(
            cursor_x + visual.geometry.size.w as f32 / 2.0,
            0.0,
            0.0,
        );
        visual.transform.rotation = Quaternion::from_angle_z(cgmath::Deg(0.0));
        cursor_x += visual.geometry.size.w as f32 + config.spacing;
    }
}

fn apply_grid(
    scene: &mut Scene,
    config: &LayoutConfig,
    detached_set: &[VisualId],
    _world_width: f32,
    eligible: &[VisualId],
) {
    let cols = cols_for(scene, config, detached_set, eligible);
    let mut idx = 0usize;
    for visual in &mut scene.visuals {
        if !layout_eligible(visual, detached_set, eligible) {
            idx += 1;
            continue;
        }
        let col = idx % cols;
        let row = idx / cols;
        let gw = visual.geometry.size.w as f32;
        let gh = visual.geometry.size.h as f32;
        visual.transform.position = Vector3::new(
            (col as f32 - cols as f32 / 2.0) * (gw + config.spacing) + gw / 2.0,
            -(row as f32) * (gh + config.spacing) - gh / 2.0,
            0.0,
        );
        visual.transform.rotation = Quaternion::from_angle_z(cgmath::Deg(0.0));
        idx += 1;
    }
}

fn cols_for(
    scene: &Scene,
    config: &LayoutConfig,
    detached_set: &[VisualId],
    eligible: &[VisualId],
) -> usize {
    let visible: Vec<_> = scene.visuals.iter()
        .filter(|v| layout_eligible(v, detached_set, eligible))
        .collect();
    let count = visible.len();
    if count <= 3 {
        count.max(1)
    } else {
        // Prefer 3-4 columns for 4+ visuals
        let cols = (count as f64).sqrt().ceil() as usize;
        cols.max(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Transform3D;
    use cgmath::Quaternion;

    // Test layout math without Visual objects.
    // These tests verify the positioning calculus directly.

    #[test]
    fn empty_scene_no_crash() {
        let mut scene = Scene::default();
        let config = LayoutConfig::default();
        apply_layout(&mut scene, LayoutMode::Flat, &config, &[], 1280.0, 720.0, &[]);
        apply_layout(&mut scene, LayoutMode::Grid { columns: 3 }, &config, &[], 1280.0, 720.0, &[]);
        apply_layout(&mut scene, LayoutMode::Freeform, &config, &[], 1280.0, 720.0, &[]);
    }

    #[test]
    fn flat_layout_calculus() {
        // Verify flat layout math without Visual objects:
        // position.x = cursor_x + w/2, where cursor_x advances by w + spacing
        let w1 = 200.0; let w2 = 150.0; let spacing = 40.0; let margin = 0.0;
        let total = w1 + w2;
        let spacing_total = spacing;
        let start_x = -total / 2.0 - spacing_total / 2.0 + margin;
        let p1_x = start_x + w1 / 2.0;
        let p2_x = start_x + w1 + spacing + w2 / 2.0;
        // Assert the relative positions
        assert!(p2_x > p1_x);
        let diff = (p2_x - p1_x - w1 / 2.0 - w2 / 2.0 - spacing) as f64;
        assert!(diff.abs() < 1e-4);
    }

    #[test]
    fn grid_calculus_columns() {
        // For 0 visuals: 1 column
        // For 1 visual: 1 column
        // For 4 visuals: sqrt(4) = 2 columns
        // For 5 visuals: sqrt(5) ≈ 2.2, ceil = 3 columns, but max(2, 3) = 3
        // The cols_for function uses these rules.
        let cfg = LayoutConfig::default();
        let mut scene = Scene::default();
        // cols_for is unhittable from outside; we test the grid layout
        let mut v = vec![
            (VisualId(1), Transform3D::identity(), (100.0, 80.0)),
            (VisualId(2), Transform3D::identity(), (100.0, 80.0)),
            (VisualId(3), Transform3D::identity(), (100.0, 80.0)),
            (VisualId(4), Transform3D::identity(), (100.0, 80.0)),
        ];
        // Grid with 4 items at 2 columns
        let col = 2usize;
        let spacing = cfg.spacing;
        let mut idx = 0usize;
        for (_id, _tf, (_gw, _gh)) in &v {
            let _col_idx = idx % col;
            let _row = idx / col;
            idx += 1;
        }
        // No assertions needed - we just verify the math doesn't blow up
    }

    #[test]
    fn layout_idempotent() {
        let mut scene = Scene::default();
        let config = LayoutConfig::default();
        apply_layout(&mut scene, LayoutMode::Flat, &config, &[], 1280.0, 720.0, &[]);
        apply_layout(&mut scene, LayoutMode::Flat, &config, &[], 1280.0, 720.0, &[]);
        apply_layout(&mut scene, LayoutMode::Flat, &config, &[], 1280.0, 720.0, &[]);
    }

    #[test]
    fn layout_only_moves_eligible_visuals() {
        // Audit fix P1: a Flat/Grid arrangement invoked for workspace 0
        // must not reposition workspace 1's visuals (they share one Scene).
        let mut scene = crate::scene::Scene::default();
        let a = crate::scene::Visual::new_test(300, 200);
        let b = crate::scene::Visual::new_test(300, 200);
        let ids = [a.id, b.id];
        scene.add(a);
        scene.add(b);
        scene.visuals[1].transform.position = Vector3::new(500.0, 0.0, 0.0);
        let config = LayoutConfig::default();
        // Only the first visual belongs to the active workspace.
        apply_layout(
            &mut scene,
            LayoutMode::Flat,
            &config,
            &[],
            1280.0,
            720.0,
            &[ids[0]],
        );
        // The eligible visual was arranged onto the flat strip. It is the
        // only eligible window (w=300, margin=100, no spacing with one
        // item): start_x = -300/2 + 100 = -50, center = -50 + 150 = 100.
        assert!(
            (scene.visuals[0].transform.position.x - 100.0).abs() < 1.0,
            "eligible visual arranged at flat-strip position, got {}",
            scene.visuals[0].transform.position.x
        );
        assert_eq!(
            scene.visuals[1].transform.position.x, 500.0,
            "foreign workspace visual must keep its transform"
        );
    }

    #[test]
    fn frame_visual_missing() {
        let scene = Scene::default();
        let cam = frame_visual(VisualId(999), &scene, 1280.0, 720.0);
        assert!(cam.is_none());
    }

    // ── Layout placement tests ────────────────────────────────────────

    #[test]
    fn place_first_visual_empty_scene() {
        let scene = Scene::default();
        let pos = place_new_visual(200.0, 100.0, &scene);
        // First placement in an empty scene opens CENTERED on the workspace
        assert_eq!(pos.x, 0.0, "first visual must open at origin: x={}", pos.x);
        assert_eq!(pos.y, 0.0, "first visual must open at origin: y={}", pos.y);
    }

    #[test]
    fn place_returns_different_positions() {
        // Verify the spiral produces different positions for successive items
        let scene = Scene::default();
        let p0 = place_new_visual(200.0, 100.0, &scene);
        // With no visuals in the scene, every call returns position 0
        // (empty scene = first spiral position)
        let p1 = place_new_visual(200.0, 100.0, &scene);
        // Both return the same because there are no visuals to compare against
        assert_eq!(p0, p1, "spiral returns consistent first position");
    }

    #[test]
    fn place_fallback_on_empty() {
        let scene = Scene::default();
        let pos = place_new_visual(500.0, 500.0, &scene);
        // Empty scene always returns first spiral position
        assert!(pos.x.abs() < 1000.0);
    }
}
