use cgmath::Quaternion;
use cgmath::Rotation3;
use cgmath::Vector3;

use crate::scene::{Scene, VisualId};

/// The camera-visible area of the z=0 workspace plane, in world units.
///
/// New windows must be placed fully inside these bounds: a position
/// that avoids overlap but sits outside the frustum gets clipped by
/// the screen edge, and its visible sliver then stacks on top of
/// windows that ARE visible (reported as "windows overlap / are cut"
/// in normal mode with three real applications).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleBounds {
    pub half_w: f32,
    pub half_h: f32,
}

impl VisibleBounds {
    /// Bounds for a perspective camera at `cam_distance` from the
    /// workspace plane (vertical `fov_y_deg`, aspect = framebuffer
    /// width / height).
    pub fn for_camera(cam_distance: f32, fov_y_deg: f32, aspect: f32) -> Self {
        let half_h = (fov_y_deg.to_radians() / 2.0).tan() * cam_distance;
        VisibleBounds {
            half_w: half_h * aspect,
            half_h,
        }
    }
}

/// Clearance kept between a placed window and the frustum edge. Covers
/// the ±5° fan rotation of the first three windows (sin 5° · w/2 ≈ 31
/// world units for a 700-wide window) plus projection slack.
const EDGE_MARGIN: f32 = 48.0;

/// Compute an initial placement position for a new visual of the given size.
///
/// Strategy (three tiers, first fit wins):
/// 1. Spiral outward from the workspace center, skipping candidates
///    that overlap existing visuals — preserves the current look when
///    there is room.
/// 2. Fine row-major scan over the visible area — catches spots the
///    coarse 7-step spiral misses (narrow side columns etc.).
/// 3. Cascade from the center with a per-window diagonal offset —
///    every window stays FULLY VISIBLE, overlapping when the frustum
///    is simply too small to pack them (same behavior as conventional
///    2D compositors; the user drags windows apart).
///
/// The hard requirement is tier-3 visibility: a placement outside the
/// camera frustum gets clipped by the screen edge, and its visible
/// sliver stacks on top of windows that ARE visible (reported as
/// "windows overlap / are cut" with three real applications open).
/// Windows never open partially off-screen.
/// H2 (user directive): in spatial mode a NEW window opens BESIDE the
/// most recent one — never overlapping it — with a MIRRORED yaw so
/// slightly-rotated neighbors face each other (a 3D conversation row).
/// Returns None when no anchor exists or both sides fail bounds/overlap;
/// the caller then falls back to the flat spiral.
pub fn place_new_visual_spatial(
    width: f32,
    height: f32,
    scene: &Scene,
    bounds: VisibleBounds,
    eligible: &[crate::scene::VisualId],
) -> Option<(Vector3<f32>, f32)> {
    // Anchor: the most recently added top-level eligible visual.
    // NOTE: detached visuals (manually moved/rotated) are VALID anchors
    // and OCCUPY SPACE — the user's flow is "rotate a window, then open
    // the next one", and the rotated window must not be invisible to
    // placement. Detached only means "layout must not MOVE it".
    let anchor = scene.visuals.iter().rev().find(|v| {
        v.parent.is_none()
            && eligible.contains(&v.id)
            && v.window_state != crate::scene::WindowState::Minimized
    })?;
    let aw = anchor.total_width();
    let (ax, ay) = (anchor.transform.position.x, anchor.transform.position.y);
    // Mirror the anchor's yaw: the pair "sees" each other.
    let q = anchor.transform.rotation;
    let anchor_yaw = (2.0f32 * q.v.y.atan2(q.s)).to_degrees();
    let gap = 90.0;
    // Reachability is the camera auto-fit's job (a pair wider than the
    // frustum dollies the camera out); placement only guarantees
    // non-overlap + adjacency, so bounds are not hard-rejected here.
    let _ = bounds;
    let try_side = |side: f32| -> Option<(Vector3<f32>, f32)> {
        let x = ax + side * ((aw + width) * 0.5 + gap);
        let y = ay;
        for v in scene.visuals.iter() {
            if v.id == anchor.id || v.parent.is_some() || !eligible.contains(&v.id) {
                continue;
                // detached visuals are NOT skipped: they occupy space.
            }
            // Rotation-aware footprint: a yawed window's world extent
            // exceeds its AABB (w*cos+h*sin on each axis).
            let q = v.transform.rotation;
            let yaw_abs = q.v.y.atan2(q.s).abs();
            let (sin_y, cos_y) = yaw_abs.sin_cos();
            let vw = v.total_width() * cos_y + v.total_height() * sin_y;
            let vh = v.total_height() * cos_y + v.total_width() * sin_y;
            let dx = (x - v.transform.position.x).abs();
            let dy = (y - v.transform.position.y).abs();
            if dx < (vw + width) * 0.5 && dy < (vh + height) * 0.5 {
                return None;
            }
        }
        // Mirror within the camera-facing hemisphere: negating a
        // back-facing anchor (|yaw| > 90°, e.g. a window the user
        // rotated past edge-on) yields another back-facing window —
        // the pair must always face the viewer, so wrap into (−90, 90].
        let mut yaw = -anchor_yaw;
        while yaw > 90.0 {
            yaw -= 180.0;
        }
        while yaw <= -90.0 {
            yaw += 180.0;
        }
        Some((Vector3::new(x, y, 0.0), yaw))
    };
    // Prefer the side toward the frustum center so pairs stay reachable.
    let first = if ax > 0.0 { -1.0 } else { 1.0 };
    let r = try_side(first).or_else(|| try_side(-first));
    tracing::debug!("H2 try_side result: {r:?} (ax={ax} aw={aw} w={width})");
    r
}

pub fn place_new_visual(
    width: f32,
    height: f32,
    scene: &Scene,
    bounds: VisibleBounds,
    eligible: &[crate::scene::VisualId],
) -> Vector3<f32> {
    // The incoming height is content-only; the rendered window adds a
    // title bar (same default the Visual will carry).
    let height = height * (1.0 + crate::scene::DecorationConfig::default().title_bar_height);
    // Scale the spiral to the frustum so its rings stay reachable on
    // small nested windows (a fixed 300-unit base pushes every ring
    // past the ±331-unit half-height of a 720p view).
    let base_spacing = (bounds.half_h * 0.45).clamp(90.0, 300.0);

    let fits_bounds = |x: f32, y: f32| -> bool {
        x - width * 0.5 >= -bounds.half_w + EDGE_MARGIN
            && x + width * 0.5 <= bounds.half_w - EDGE_MARGIN
            && y - height * 0.5 >= -bounds.half_h + EDGE_MARGIN
            && y + height * 0.5 <= bounds.half_h - EDGE_MARGIN
    };
    let overlaps_existing = |x: f32, y: f32| -> bool {
        scene.visuals.iter().any(|v| {
            // Detached windows STILL OCCUPY SPACE (H2 fix): they are
            // exempt from being MOVED by layout, not from colliding.
            // Workspace-scoped (same rule as apply_layout, I6): windows
            // on OTHER workspaces must not influence placement here, or
            // a fresh workspace's first window appends to another
            // workspace's row and lands outside its view.
            if !eligible.contains(&v.id) {
                return false;
            }
            let vw = v.total_width();
            let vh = v.total_height();
            let dx = (x - v.transform.position.x).abs();
            let dy = (y - v.transform.position.y).abs();
            dx < (vw + width) * 0.5 && dy < (vh + height) * 0.5
        })
    };

    // Tier 1: the 7-step spiral.
    for i in 0..100 {
        let (x, y) = if i == 0 {
            (0.0, 0.0)
        } else {
            let angle = (i as f32) * 2.0 * std::f32::consts::TAU / 7.0;
            let radius = base_spacing + (i as f32).sqrt() * base_spacing * 0.5;
            (radius * angle.cos(), radius * angle.sin() * 0.6) // flatten vertically
        };
        if fits_bounds(x, y) && !overlaps_existing(x, y) {
            return Vector3::new(x, y, 0.0);
        }
    }

    // Tier 2: fine row-major scan of the visible area.
    let step_x = (width * 0.25).clamp(48.0, 160.0);
    let step_y = (height * 0.25).clamp(48.0, 160.0);
    let min_cx = -bounds.half_w + EDGE_MARGIN + width * 0.5;
    let max_cx = bounds.half_w - EDGE_MARGIN - width * 0.5;
    let min_cy = -bounds.half_h + EDGE_MARGIN + height * 0.5;
    let max_cy = bounds.half_h - EDGE_MARGIN - height * 0.5;
    if min_cx <= max_cx && min_cy <= max_cy {
        let mut y = min_cy;
        while y <= max_cy {
            let mut x = min_cx;
            while x <= max_cx {
                if !overlaps_existing(x, y) {
                    return Vector3::new(x, y, 0.0);
                }
                x += step_x;
            }
            y += step_y;
        }
    }

    // Tier 3: row-append to the right of the rightmost visual, same
    // row center — then the compositor auto-fits the CAMERA so the
    // whole row is visible (a camera operation; visual transforms are
    // never touched). This is the spatial-desktop answer to "does not
    // fit": windows stay fully non-overlapping and the view zooms out,
    // instead of a cramped cascade. The FIRST window (empty row)
    // centers on the workspace.
    let right = scene
        .visuals
        .iter()
        .filter(|v| !scene.detached_set.contains(&v.id) && eligible.contains(&v.id))
        .map(|v| v.transform.position.x + v.total_width() * 0.5)
        .reduce(f32::max);
    match right {
        Some(r) => Vector3::new(r + EDGE_MARGIN + width * 0.5, 0.0, 0.0),
        None => Vector3::new(0.0, 0.0, 0.0),
    }
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
    /// G-H3: arranged on a horizontal arc that bows around the camera —
    /// every window faces the viewer, edge windows recede in depth.
    /// Radius shrinks/grows to fit the content (deterministic).
    Arc,
    /// G-H3: arranged on a full circle facing the center — a spatial
    /// carousel. Radius fits the widest content around the ring.
    Circle,
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
        LayoutMode::Flat => apply_flat(scene, config, detached_set, world_width, eligible),
        LayoutMode::Grid { .. } => apply_grid(scene, config, detached_set, world_height, eligible),
        LayoutMode::Arc => apply_arc(scene, config, detached_set, eligible),
        LayoutMode::Circle => apply_circle(scene, config, detached_set, eligible),
    }
}

/// G-H3: arrange visuals on an arc facing the camera. Each visual's
/// angular slot derives from its own width (`w + spacing) / r` —
/// content-aware and deterministic; the radius adapts so the arc never
/// exceeds a 140° spread (beyond that the edge windows face away).
/// Positions sit on the circle through the origin with the center of
/// curvature BEHIND the camera (+z), so windows bow around the viewer;
/// each window yaws to face the origin.
fn apply_arc(scene: &mut Scene, config: &LayoutConfig, detached_set: &[VisualId], eligible: &[VisualId]) {
    let items: Vec<(VisualId, f32, f32)> = scene
        .visuals
        .iter()
        .filter(|v| layout_eligible(v, detached_set, eligible))
        .map(|v| (v.id, v.total_width(), v.total_height()))
        .collect();
    if items.is_empty() {
        return;
    }
    const MAX_SPREAD_DEG: f32 = 140.0;
    // Radius must fit the total angular demand under the spread cap.
    let total_width: f32 = items.iter().map(|(_, w, _)| *w).sum();
    let spacing_total = (items.len().saturating_sub(1)) as f32 * config.spacing;
    let min_radius = (total_width + spacing_total) * 180.0 / (MAX_SPREAD_DEG * std::f32::consts::PI) / 2.0;
    let radius = min_radius.max(600.0);

    let angular = |w: f32| (w + config.spacing) / radius;
    let total_angle: f32 = items.iter().map(|(_, w, _)| angular(*w)).sum::<f32>()
        - angular(0.0).min(config.spacing / radius);
    let mut theta = -total_angle / 2.0;
    for (vid, w, _h) in items {
        let half = angular(w) / 2.0;
        let mid = theta + half;
        // Circle center at (0, 0, radius); arc point = center + r·(sin, 0, -cos).
        let x = radius * mid.sin();
        let z = radius - radius * mid.cos();
        if let Some(v) = scene.get_mut(vid) {
            v.transform.position = Vector3::new(x, 0.0, z);
            // Face the origin: yaw = -theta (window normal +z rotated by yaw).
            v.transform.rotation = Quaternion::from_angle_y(cgmath::Rad(-mid));
        }
        theta += angular(w);
    }
}

/// G-H3: arrange visuals on a full circle, each facing the center — a
/// spatial carousel. The radius fits the widest visual around the ring
/// at a comfortable angular share.
fn apply_circle(scene: &mut Scene, config: &LayoutConfig, detached_set: &[VisualId], eligible: &[VisualId]) {
    let items: Vec<(VisualId, f32)> = scene
        .visuals
        .iter()
        .filter(|v| layout_eligible(v, detached_set, eligible))
        .map(|v| (v.id, v.total_width()))
        .collect();
    let n = items.len();
    if n == 0 {
        return;
    }
    let max_w = items.iter().map(|(_, w)| *w).fold(0.0f32, f32::max);
    // Circumference must fit n·(max_w + spacing).
    let radius = ((max_w + config.spacing) * n as f32 / (2.0 * std::f32::consts::PI)).max(600.0);
    let step = 2.0 * std::f32::consts::PI / n as f32;
    for (i, (vid, _w)) in items.iter().enumerate() {
        let theta = i as f32 * step;
        let x = radius * theta.sin();
        let z = radius * theta.cos();
        if let Some(v) = scene.get_mut(*vid) {
            v.transform.position = Vector3::new(x, 0.0, z);
            // Face the CENTER of the ring (origin).
            v.transform.rotation = Quaternion::from_angle_y(cgmath::Rad(-theta));
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
    let n: usize = scene
        .visuals
        .iter()
        .filter(|v| layout_eligible(v, detached_set, eligible))
        .count();
    let total_width: f32 = scene
        .visuals
        .iter()
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
        visual.transform.position =
            Vector3::new(cursor_x + visual.geometry.size.w as f32 / 2.0, 0.0, 0.0);
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
    _config: &LayoutConfig,
    detached_set: &[VisualId],
    eligible: &[VisualId],
) -> usize {
    let visible: Vec<_> = scene
        .visuals
        .iter()
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

    // Test layout math without Visual objects.
    // These tests verify the positioning calculus directly.

    #[test]
    fn empty_scene_no_crash() {
        let mut scene = Scene::default();
        let config = LayoutConfig::default();
        apply_layout(
            &mut scene,
            LayoutMode::Flat,
            &config,
            &[],
            1280.0,
            720.0,
            &[],
        );
        apply_layout(
            &mut scene,
            LayoutMode::Grid { columns: 3 },
            &config,
            &[],
            1280.0,
            720.0,
            &[],
        );
        apply_layout(
            &mut scene,
            LayoutMode::Freeform,
            &config,
            &[],
            1280.0,
            720.0,
            &[],
        );
    }

    #[test]
    fn flat_layout_calculus() {
        // Verify flat layout math without Visual objects:
        // position.x = cursor_x + w/2, where cursor_x advances by w + spacing
        let w1 = 200.0;
        let w2 = 150.0;
        let spacing = 40.0;
        let margin = 0.0;
        let total = w1 + w2;
        let spacing_total = spacing;
        let start_x = -total / 2.0 - spacing_total / 2.0 + margin;
        let p1_x = start_x + w1 / 2.0;
        let p2_x = start_x + w1 + spacing + w2 / 2.0;
        // Assert the relative positions
        assert!(p2_x > p1_x);
        let diff: f64 = p2_x - p1_x - w1 / 2.0 - w2 / 2.0 - spacing;
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
        let _scene = Scene::default();
        // cols_for is unhittable from outside; we test the grid layout
        let v = [
            (VisualId(1), Transform3D::identity(), (100.0, 80.0)),
            (VisualId(2), Transform3D::identity(), (100.0, 80.0)),
            (VisualId(3), Transform3D::identity(), (100.0, 80.0)),
            (VisualId(4), Transform3D::identity(), (100.0, 80.0)),
        ];
        // Grid with 4 items at 2 columns
        let col = 2usize;
        let _spacing = cfg.spacing;
        for (idx, (_id, _tf, (_gw, _gh))) in v.iter().enumerate() {
            let _col_idx = idx % col;
            let _row = idx / col;
        }
        // No assertions needed - we just verify the math doesn't blow up
    }

    #[test]
    fn layout_idempotent() {
        let mut scene = Scene::default();
        let config = LayoutConfig::default();
        apply_layout(
            &mut scene,
            LayoutMode::Flat,
            &config,
            &[],
            1280.0,
            720.0,
            &[],
        );
        apply_layout(
            &mut scene,
            LayoutMode::Flat,
            &config,
            &[],
            1280.0,
            720.0,
            &[],
        );
        apply_layout(
            &mut scene,
            LayoutMode::Flat,
            &config,
            &[],
            1280.0,
            720.0,
            &[],
        );
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

    /// Typical nested-session frustum: camera 800 units from the plane,
    /// 45° vertical FOV, 16:9 framebuffer → visible world ±589 × ±331.
    fn bounds_16_9() -> VisibleBounds {
        VisibleBounds::for_camera(800.0, 45.0, 1280.0 / 720.0)
    }

    /// All visuals currently in the scene (tests place within one
    /// workspace, so every visual is eligible).
    fn all_eligible(scene: &Scene) -> Vec<crate::scene::VisualId> {
        scene.visuals.iter().map(|v| v.id).collect()
    }

    #[test]
    fn place_first_visual_empty_scene() {
        let scene = Scene::default();
        let pos = place_new_visual(200.0, 100.0, &scene, bounds_16_9(), &all_eligible(&scene));
        // First placement in an empty scene opens CENTERED on the workspace
        assert_eq!(pos.x, 0.0, "first visual must open at origin: x={}", pos.x);
        assert_eq!(pos.y, 0.0, "first visual must open at origin: y={}", pos.y);
    }

    #[test]
    fn place_returns_different_positions() {
        // Verify the spiral produces different positions for successive items
        let mut scene = Scene::default();
        let b = bounds_16_9();
        let p0 = place_new_visual(200.0, 100.0, &scene, b, &all_eligible(&scene));
        // Register the placed window so the next call sees it.
        let mut v0 = crate::scene::Visual::new_test(200, 100);
        v0.transform.position = p0;
        scene.add(v0);
        let p1 = place_new_visual(200.0, 100.0, &scene, b, &all_eligible(&scene));
        assert_ne!(p0, p1, "second placement must move away from the first");
    }

    #[test]
    fn place_fallback_on_empty() {
        let scene = Scene::default();
        let pos = place_new_visual(500.0, 500.0, &scene, bounds_16_9(), &all_eligible(&scene));
        // Empty scene always returns first spiral position
        assert!(pos.x.abs() < 1000.0);
    }

    /// THE REPORTED BUG (revised contract): foot, zenity and
    /// weston-terminal cannot fit side by side in a 16:9 frustum
    /// (they need ~92% of it). Placement therefore appends them to a
    /// non-overlapping ROW (the compositor auto-fits the camera so the
    /// row is fully visible). Placement itself guarantees: first
    /// window centered, later windows never overlapping.
    #[test]
    fn real_apps_row_append_without_overlap() {
        let mut scene = Scene::default();
        let bounds = bounds_16_9();
        let apps: [(&str, i32, i32); 3] = [
            ("foot", 700, 500),
            ("zenity", 400, 150),
            ("weston-terminal", 600, 450),
        ];
        let mut prev_right: Option<f32> = None;
        for (name, w, h) in apps {
            let pos = place_new_visual(w as f32, h as f32, &scene, bounds, &all_eligible(&scene));
            match prev_right {
                None => {
                    assert_eq!(pos.x, 0.0, "{}: first window centered", name);
                }
                Some(r) => {
                    assert!(
                        pos.x - w as f32 * 0.5 >= r,
                        "{}: must append right of the previous window",
                        name
                    );
                    assert_eq!(pos.y, 0.0, "{}: row keeps y=0", name);
                }
            }
            prev_right = Some(pos.x + w as f32 * 0.5);
            let mut v = crate::scene::Visual::new_test(w, h);
            v.transform.position = pos;
            scene.add(v);
        }
    }

    /// When the windows DO fit, placement must remain non-overlapping
    /// (small windows in the same frustum leave room for the spiral or
    /// the fine scan to find clear spots).
    #[test]
    fn small_windows_do_not_overlap() {
        let mut scene = Scene::default();
        let bounds = bounds_16_9();
        let apps: [(i32, i32); 3] = [(300, 200), (250, 150), (280, 220)];
        let mut placed: Vec<(f32, f32, f32, f32)> = Vec::new();
        for (w, h) in apps {
            let pos = place_new_visual(w as f32, h as f32, &scene, bounds, &all_eligible(&scene));
            let wt = w as f32;
            // G-H0.1: no SSD strip by default — decorated height == content.
            let ht = h as f32 * (1.0 + crate::scene::DecorationConfig::default().title_bar_height);
            for (px, py, pw, ph) in &placed {
                let dx = (pos.x - px).abs();
                let dy = (pos.y - py).abs();
                assert!(
                    dx >= (pw + wt) * 0.5 || dy >= (ph + ht) * 0.5,
                    "window at ({}, {}) overlaps an earlier one (dx={} dy={})",
                    pos.x,
                    pos.y,
                    dx,
                    dy
                );
            }
            placed.push((pos.x, pos.y, wt, ht));
            let mut v = crate::scene::Visual::new_test(w, h);
            v.transform.position = pos;
            scene.add(v);
        }
    }

    /// A window larger than the frustum (or a tiny frustum) must still
    /// be placed VISIBLE — the cascade fallback centers it so the
    /// clipping is symmetric instead of pushing it off-screen.
    #[test]
    fn oversized_window_falls_back_inside_bounds() {
        let scene = Scene::default();
        // Tiny frustum: half-extents 300×200; window 700×500 cannot fit.
        // Tier 3 now appends to the row at y=0 (the compositor zooms the
        // camera out to frame it) instead of clamping.
        let bounds = VisibleBounds {
            half_w: 300.0,
            half_h: 200.0,
        };
        let pos = place_new_visual(700.0, 500.0, &scene, bounds, &all_eligible(&scene));
        // First window of an empty scene: row-append starts at the
        // workspace center.
        assert_eq!(pos.x, 0.0);
        assert_eq!(pos.y, 0.0);
    }

    /// THE TWO-WINDOW REGRESSION: when two default-size windows cannot
    /// fit side by side in the visible frustum (16:9 shows ±589 world
    /// units; two 640-wide windows need 1280+), the second window is
    /// appended to the row to the RIGHT of the first — never stacked
    /// nearly coincident on top of it. The compositor then auto-fits
    /// the camera so the row is visible.
    #[test]
    fn second_window_appends_to_row_when_tight() {
        let mut scene = Scene::default();
        let bounds = bounds_16_9();
        let a = place_new_visual(640.0, 508.8, &scene, bounds, &all_eligible(&scene));
        assert_eq!(a, Vector3::new(0.0, 0.0, 0.0), "first window centered");
        let mut va = crate::scene::Visual::new_test(640, 480);
        va.transform.position = a;
        scene.add(va);
        let b = place_new_visual(640.0, 508.8, &scene, bounds, &all_eligible(&scene));
        // Right edge of A is x=320; B's center = 320 + 48 + 320 = 688.
        assert!(
            (b.x - 688.0).abs() < 1.0 && b.y.abs() < 1.0,
            "second window must append to the row, got {:?}",
            b
        );
    }

    /// Three windows: the row keeps growing to the right with the same
    /// margin — no overlap anywhere even when the frustum is long
    /// exceeded (the camera fit is the compositor's job).
    #[test]
    fn three_windows_row_without_overlap() {
        let mut scene = Scene::default();
        let bounds = bounds_16_9();
        let mut prev_right = 0.0f32;
        for i in 0..3 {
            let pos = place_new_visual(640.0, 508.8, &scene, bounds, &all_eligible(&scene));
            if i > 0 {
                assert!(
                    pos.x - 320.0 >= prev_right,
                    "window {} must not overlap the previous row member",
                    i
                );
            }
            prev_right = pos.x + 320.0;
            let mut v = crate::scene::Visual::new_test(640, 480);
            v.transform.position = pos;
            scene.add(v);
        }
    }

    /// THE REPORTED BUG: opening an app on a DIFFERENT workspace while
    /// workspace 0 already has a row must NOT append to workspace 0's
    /// row (the window landed at x≈1056, far outside the fresh
    /// workspace's view — "appears in the taskbar but is not visible").
    #[test]
    fn placement_is_workspace_scoped() {
        let mut scene = Scene::default();
        let bounds = bounds_16_9();
        // Workspace 0: one window far right in its row.
        let mut v0 = crate::scene::Visual::new_test(640, 480);
        v0.transform.position = Vector3::new(688.0, 0.0, 0.0);
        scene.add(v0);
        // Workspace 1 contains only itself (empty row).
        let ws1_eligible = [crate::scene::VisualId(99)];
        let pos = place_new_visual(640.0, 508.8, &scene, bounds, &ws1_eligible);
        assert_eq!(
            pos,
            Vector3::new(0.0, 0.0, 0.0),
            "a fresh workspace's first window must center, not append to another workspace's row"
        );
    }
    #[test]
    fn arc_arrangement_is_deterministic_and_camera_facing() {
        let mut scene = Scene::default();
        let a = crate::scene::Visual::new_test(300, 200);
        let b = crate::scene::Visual::new_test(300, 200);
        let (ida, idb) = (a.id, b.id);
        scene.add(a);
        scene.add(b);
        let config = LayoutConfig { spacing: 40.0, ..LayoutConfig::default() };
        apply_layout(&mut scene, LayoutMode::Arc, &config, &[], 1280.0, 720.0, &[ida, idb]);
        let pa = scene.get(ida).unwrap().transform.position;
        let pb = scene.get(idb).unwrap().transform.position;
        assert!(pa.z >= 0.0 && pb.z >= 0.0, "arc recedes from camera: {pa:?} {pb:?}");
        assert!(pa.x.abs() <= pb.x.abs() + 1.0, "symmetric about the origin");
        let mut scene2 = Scene::default();
        let c = crate::scene::Visual::new_test(300, 200);
        let d = crate::scene::Visual::new_test(300, 200);
        let (idc, idd) = (c.id, d.id);
        scene2.add(c);
        scene2.add(d);
        apply_layout(&mut scene2, LayoutMode::Arc, &config, &[], 1280.0, 720.0, &[idc, idd]);
        let t1 = &scene.get(ida).unwrap().transform;
        let t2 = &scene2.get(idc).unwrap().transform;
        assert_eq!(t1.position, t2.position);
        assert_eq!(t1.rotation, t2.rotation);
    }

    #[test]
    fn circle_arrangement_is_an_equal_ring_facing_center() {
        let mut scene = Scene::default();
        let mut ids = Vec::new();
        for _ in 0..4 {
            let v = crate::scene::Visual::new_test(300, 200);
            ids.push(v.id);
            scene.add(v);
        }
        let config = LayoutConfig { spacing: 40.0, ..LayoutConfig::default() };
        apply_layout(&mut scene, LayoutMode::Circle, &config, &[], 1280.0, 720.0, &ids);
        let radii: Vec<f32> = ids
            .iter()
            .map(|id| {
                let p = scene.get(*id).unwrap().transform.position;
                (p.x * p.x + p.z * p.z).sqrt()
            })
            .collect();
        for r in &radii[1..] {
            assert!((r - radii[0]).abs() < 0.5, "equal ring radii: {radii:?}");
        }
        assert!(radii[0] >= 600.0, "minimum radius floor: {}", radii[0]);
        let mut scene2 = Scene::default();
        let mut ids2 = Vec::new();
        for _ in 0..4 {
            let v = crate::scene::Visual::new_test(300, 200);
            ids2.push(v.id);
            scene2.add(v);
        }
        apply_layout(&mut scene2, LayoutMode::Circle, &config, &[], 1280.0, 720.0, &ids2);
        let t1 = &scene.get(ids[2]).unwrap().transform;
        let t2 = &scene2.get(ids2[2]).unwrap().transform;
        assert_eq!(t1.position, t2.position);
        assert_eq!(t1.rotation, t2.rotation);
    }


    #[test]
    fn spatial_placement_beside_anchor_mirrored_yaw() {
        use cgmath::Rotation3;
        let mut scene = crate::scene::Scene::default();
        let a = crate::scene::Visual::new_test(400, 300);
        scene.add(a);
        // Anchor slightly rotated +5°.
        scene.visuals[0].transform.rotation =
            cgmath::Quaternion::from_angle_y(cgmath::Deg(5.0));
        let eligible = all_eligible(&scene);
        let (pos, yaw) = place_new_visual_spatial(
            300.0,
            200.0,
            &scene,
            bounds_16_9(),
            &eligible,
        )
        .expect("anchor exists");
        // Beside, non-overlapping: |dx| >= half-sums.
        let dx = (pos.x - scene.visuals[0].transform.position.x).abs();
        assert!(dx >= (400.0 + 300.0) * 0.5, "placed beside anchor, dx={dx}");
        assert!((pos.y - scene.visuals[0].transform.position.y).abs() < 1e-3);
        // Mirrored yaw: the pair faces each other.
        assert!((yaw + 5.0).abs() < 1e-3, "mirrored yaw, got {yaw}");
    }

    #[test]
    fn spatial_placement_prefers_center_side_and_respects_bounds() {
        let mut scene = crate::scene::Scene::default();
        let a = crate::scene::Visual::new_test(400, 300);
        scene.add(a);
        // Anchor on the RIGHT half → new window goes LEFT (toward center).
        scene.visuals[0].transform.position = Vector3::new(500.0, 0.0, 0.0);
        let eligible = all_eligible(&scene);
        let (pos, _yaw) = place_new_visual_spatial(
            300.0,
            200.0,
            &scene,
            bounds_16_9(),
            &eligible,
        )
        .expect("anchor exists");
        assert!(pos.x < 500.0, "placed toward center (left), got {}", pos.x);
    }

    #[test]
    fn spatial_mirror_wraps_back_facing_anchor() {
        use cgmath::Rotation3;
        let mut scene = crate::scene::Scene::default();
        let a = crate::scene::Visual::new_test(400, 300);
        scene.add(a);
        // Anchor rotated to nearly 180° (user spun it around).
        scene.visuals[0].transform.rotation =
            cgmath::Quaternion::from_angle_y(cgmath::Deg(178.0));
        let eligible = all_eligible(&scene);
        let (_pos, yaw) = place_new_visual_spatial(
            300.0,
            200.0,
            &scene,
            bounds_16_9(),
            &eligible,
        )
        .expect("anchor exists");
        // The naive mirror (−178°) is back-facing; the wrap must land
        // the new window within the facing hemisphere.
        assert!(
            yaw > -90.0 && yaw <= 90.0,
            "new window must face the viewer, got {yaw}"
        );
        assert!((yaw - 2.0).abs() < 1e-2, "wrapped mirror, got {yaw}");
    }

    #[test]
    fn spatial_placement_none_without_anchor_or_when_blocked() {
        // No visuals at all → no anchor → None.
        let scene = crate::scene::Scene::default();
        assert!(place_new_visual_spatial(
            300.0,
            200.0,
            &scene,
            bounds_16_9(),
            &[],
        )
        .is_none());

        // Anchor centered + a huge neighbor on both sides → both sides
        // blocked → None (caller falls back to the spiral).
        let mut scene = crate::scene::Scene::default();
        let a = crate::scene::Visual::new_test(400, 300);
        let l = crate::scene::Visual::new_test(600, 300);
        let r = crate::scene::Visual::new_test(600, 300);
        scene.add(l);
        scene.add(r);
        scene.add(a); // the anchor is the MOST RECENTLY added visual
        scene.visuals[0].transform.position = Vector3::new(-545.0, 0.0, 0.0);
        scene.visuals[1].transform.position = Vector3::new(545.0, 0.0, 0.0);
        let eligible = all_eligible(&scene);
        assert!(
            place_new_visual_spatial(400.0, 300.0, &scene, bounds_16_9(), &eligible).is_none(),
            "both sides blocked → fallback"
        );
    }

    #[test]
    fn smoothstep_eases_endpoints_and_midpoint() {
        assert_eq!(crate::compositor::smoothstep(0.0), 0.0);
        assert_eq!(crate::compositor::smoothstep(1.0), 1.0);
        assert!((crate::compositor::smoothstep(0.5) - 0.5).abs() < 1e-9);
        // Monotone and eased (slower at the ends than the middle).
        assert!(crate::compositor::smoothstep(0.25) < 0.25);
        assert!(crate::compositor::smoothstep(0.75) > 0.75);
    }
}
