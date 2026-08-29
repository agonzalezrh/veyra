use cgmath::InnerSpace;
use cgmath::Matrix4;
use cgmath::SquareMatrix;
use cgmath::Vector3;
use cgmath::Vector4;

use crate::scene::{Scene, VisualId};
use crate::input::Camera;

/// Which mode the interaction controller is in.
/// This decides how pointer events are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    /// No special mode — events go to content or camera.
    Normal,
    /// Dragging a visual through the scene.
    Drag,
}

/// Modes of manipulation active on the selected visual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManipMode {
    None,
    Translate,
    RotateY,
    RotateZ,
    RotateX,
    Scale,
}

/// Tracks an in-progress interaction (e.g. a drag).
#[derive(Debug, Clone)]
struct ActiveManip {
    mode: ManipMode,
    /// The visual being manipulated (authoritative, not derived from selection).
    vid: VisualId,
    /// The hit point on the visual in world space when drag started.
    origin: Vector3<f32>,
    /// The offset from visual center to grab point (for smooth drag start).
    grab_offset: Vector3<f32>,
    /// The plane normal for translation (camera forward at drag start).
    plane_normal: Vector3<f32>,
    /// Original transform values for relative manipulation.
    start_position: Vector3<f32>,
    start_rotation: cgmath::Quaternion<f32>,
}

/// Translates raw pointer events into scene/camera operations.
///
/// Owns no rendering or Wayland state — only scene and camera references.
#[derive(Debug)]
pub struct InteractionController {
    pub selection_enabled: bool,
    pub manipulation_enabled: bool,
    /// Last known mouse position in window coordinates.
    pub mouse_x: f64,
    pub mouse_y: f64,
    /// Window size for NDC conversion.
    pub window_size: (f32, f32),
    active: Option<ActiveManip>,
}

impl InteractionController {
    pub fn new() -> Self {
        InteractionController {
            selection_enabled: true,
            manipulation_enabled: true,
            mouse_x: 0.0,
            mouse_y: 0.0,
            window_size: (1280.0, 720.0),
            active: None,
        }
    }

    fn proj_matrix(&self, _camera: &Camera, spatial_mode: bool) -> Matrix4<f32> {
        let (w, h) = self.window_size;
        if w <= 0.0 || h <= 0.0 {
            return cgmath::ortho(-640.0, 640.0, -360.0, 360.0, -1000.0, 1000.0);
        }
        if spatial_mode {
            cgmath::perspective(cgmath::Deg(45.0), w / h, 1.0, 10000.0)
        } else {
            cgmath::ortho(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        }
    }

    /// Convert window coordinates to NDC.
    fn ndc(&self, x: f64, y: f64) -> (f32, f32) {
        let (w, h) = self.window_size;
        if w <= 0.0 || h <= 0.0 {
            return (0.0, 0.0);
        }
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        (ndc_x, ndc_y)
    }

    /// Unproject NDC to two world-space points along the ray.
    fn world_ray(&self, ndc_x: f32, ndc_y: f32, camera: &Camera, spatial_mode: bool) -> (Vector3<f32>, Vector3<f32>) {
        let pv = self.proj_matrix(camera, spatial_mode) * camera.view_matrix();
        let inv_pv = pv.invert().unwrap_or(Matrix4::identity());
        let near = inv_pv * Vector4::new(ndc_x, ndc_y, -1.0, 1.0);
        let far = inv_pv * Vector4::new(ndc_x, ndc_y, 1.0, 1.0);
        let near_pt = Vector3::new(near.x / near.w, near.y / near.w, near.z / near.w);
        let far_pt = Vector3::new(far.x / far.w, far.y / far.w, far.z / far.w);
        (near_pt, far_pt)
    }

    /// Intersect a ray with a plane.
    pub fn ray_plane_intersect(
        ray_origin: Vector3<f32>,
        ray_dir: Vector3<f32>,
        plane_point: Vector3<f32>,
        plane_normal: Vector3<f32>,
    ) -> Option<Vector3<f32>> {
        let denom = plane_normal.dot(ray_dir);
        if denom.abs() < 1e-8 {
            return None;
        }
        let t = (plane_point - ray_origin).dot(plane_normal) / denom;
        if t < 0.0 {
            return None;
        }
        Some(ray_origin + ray_dir * t)
    }

    /// Compute the drag translation plane normal.
    ///
    /// The plane is vertical and perpendicular to the camera's horizontal
    /// view direction so drags slide windows along a wall facing the camera.
    /// When that plane is edge-on to the view ray (front-facing camera,
    /// e.g. normal 2D mode with yaw = 0), it falls back to a screen-parallel
    /// plane (normal = camera forward) so drags still start and the window
    /// follows the cursor at constant depth.
    fn drag_plane_normal(fwd: Vector3<f32>, ray_dir: Vector3<f32>) -> Vector3<f32> {
        let mut n = Vector3::new(fwd.z, 0.0, -fwd.x);
        if n.magnitude2() < 1e-12 {
            n = fwd;
        }
        let n = n.normalize();
        if n.dot(ray_dir).abs() < 1e-4 {
            fwd.normalize()
        } else {
            n
        }
    }

    /// Handle a pointer button press.
    ///
    /// Always picks and selects the visual under the cursor (if any).
    /// Starts a manipulation drag only when a modifier key (shift/ctrl/alt) is held.
    ///
    /// Returns `Some(ManipMode)` if a drag was started, `None` otherwise.
    /// The caller (LookingGlass) uses this to decide whether to route the
    /// event to content input or scene manipulation.
    /// `visible_ids` optionally restricts picking to a set of visual IDs
    /// (e.g., the active workspace). If None, all visuals are pickable.
    pub fn handle_pointer_down(
        &mut self,
        x: f64,
        y: f64,
        scene: &mut Scene,
        camera: &Camera,
        spatial_mode: bool,
        shift: bool,
        ctrl: bool,
        alt: bool,
        visible_ids: Option<Vec<crate::scene::VisualId>>,
    ) -> Option<ManipMode> {
        self.mouse_x = x;
        self.mouse_y = y;
        let (nx, ny) = self.ndc(x, y);
        let pv = self.proj_matrix(camera, spatial_mode) * camera.view_matrix();

        if !self.selection_enabled {
            return None;
        }

        let picked = match visible_ids {
            Some(ref ids) => scene.pick_visible(&pv, nx, ny, ids),
            None => scene.pick(&pv, nx, ny),
        };
        let Some((vid, _dist)) = picked else {
            scene.select(None);
            return None;
        };

        scene.select(Some(vid));

        // Modifier keys determine manipulation mode.
        // Without modifiers, no manipulation starts — the event is for content.
        let mode = if shift {
            ManipMode::RotateY
        } else if ctrl {
            ManipMode::RotateZ
        } else if alt {
            ManipMode::RotateX
        } else {
            return None;
        };

        let (ray_origin, ray_far) = self.world_ray(nx, ny, camera, spatial_mode);
        let ray_dir = (ray_far - ray_origin).normalize();

        if let Some(visual) = scene.visuals.iter().find(|v| v.id == vid) {
            let pos = visual.transform.position;
            let fwd = camera.forward();
            let plane_normal = Self::drag_plane_normal(fwd, ray_dir);

            // Mark as detached from layout when user starts manipulating
            if !scene.detached_set.contains(&vid) {
                scene.detached_set.push(vid);
            }

            if let Some(hit) = Self::ray_plane_intersect(ray_origin, ray_dir, pos, plane_normal) {
                let grab_offset = hit - pos;
                self.active = Some(ActiveManip {
                    mode,
                    vid,
                    origin: hit,
                    grab_offset,
                    plane_normal,
                    start_position: pos,
                    start_rotation: visual.transform.rotation,
                });
                return Some(mode);
            }
        }
        None
    }

    /// Whether a manipulation drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.active.is_some()
    }

    /// Whether a specific visual is being dragged.
    pub fn is_dragging_visual(&self, vid: VisualId) -> bool {
        self.active.as_ref().map_or(false, |a| a.vid == vid)
    }

    /// Start a translate drag on the selected visual (without modifier).
    /// Used for title-bar drags and content-area spatial manipulation.
    pub fn force_translate(
        &mut self,
        x: f64,
        y: f64,
        scene: &mut Scene,
        camera: &Camera,
        spatial_mode: bool,
    ) {
        let Some(vid) = scene.selected_id else { return };
        let (nx, ny) = self.ndc(x, y);
        let (ray_origin, ray_far) = self.world_ray(nx, ny, camera, spatial_mode);
        let ray_dir = (ray_far - ray_origin).normalize();

        if let Some(visual) = scene.visuals.iter().find(|v| v.id == vid) {
            let pos = visual.transform.position;
            // Compute camera forward in world space using camera orientation
            let fwd = camera.forward();
            let plane_normal = Self::drag_plane_normal(fwd, ray_dir);

            let plane_point = pos;
            if !scene.detached_set.contains(&vid) {
                scene.detached_set.push(vid);
            }

            if let Some(hit) = Self::ray_plane_intersect(ray_origin, ray_dir, plane_point, plane_normal) {
                let grab_offset = hit - pos;
                self.active = Some(ActiveManip {
                    mode: ManipMode::Translate,
                    vid,
                    origin: hit,
                    grab_offset,
                    plane_normal,
                    start_position: pos,
                    start_rotation: visual.transform.rotation,
                });
            }
        }
    }

    /// Handle pointer button release.
    pub fn handle_pointer_up(&mut self) {
        self.active = None;
    }

    /// Handle pointer motion during drag.
    pub fn handle_pointer_move(
        &mut self,
        x: f64,
        y: f64,
        scene: &mut Scene,
        camera: &Camera,
        spatial_mode: bool,
    ) {
        let dx = x - self.mouse_x;
        let dy = y - self.mouse_y;
        self.mouse_x = x;
        self.mouse_y = y;

        let Some(ref active) = self.active.clone() else { return };
        let visual = match scene.get_mut(active.vid) {
            Some(v) => v,
            None => return,
        };

        let (nx, ny) = self.ndc(x, y);

        match active.mode {
            ManipMode::Translate => {
                let (ray_origin, ray_far) = self.world_ray(nx, ny, camera, spatial_mode);
                let ray_dir = (ray_far - ray_origin).normalize();
                if let Some(hit) = Self::ray_plane_intersect(
                    ray_origin, ray_dir, active.origin, active.plane_normal,
                ) {
                    let delta = hit - active.origin;
                    visual.transform.position = active.start_position + delta;
                }
            }
            ManipMode::RotateY => {
                use cgmath::Rotation3;
                let delta_rot = cgmath::Quaternion::from_angle_y(cgmath::Deg(dx as f32 * 0.3));
                visual.transform.rotation = delta_rot * active.start_rotation;
            }
            ManipMode::RotateZ => {
                use cgmath::Rotation3;
                let delta_rot = cgmath::Quaternion::from_angle_z(cgmath::Deg(dx as f32 * 0.3));
                visual.transform.rotation = delta_rot * active.start_rotation;
            }
            ManipMode::RotateX => {
                use cgmath::Rotation3;
                let delta_rot = cgmath::Quaternion::from_angle_x(cgmath::Deg(dy as f32 * 0.3));
                visual.transform.rotation = delta_rot * active.start_rotation;
            }
            ManipMode::Scale | ManipMode::None => {}
        }
    }

    /// Handle scroll for scale.
    pub fn handle_scroll(
        &mut self,
        _x: f64,
        y: f64,
        scene: &mut Scene,
    ) {
        let Some(vid) = scene.selected_id else { return };
        let visual = match scene.get_mut(vid) {
            Some(v) => v,
            None => return,
        };
        let factor = 1.0 + (y as f32 * 0.05);
        visual.transform.scale = visual.transform.scale * factor;
        // Clamp scale to [0.01, 100]
        visual.transform.scale.x = visual.transform.scale.x.clamp(0.01, 100.0);
        visual.transform.scale.y = visual.transform.scale.y.clamp(0.01, 100.0);
        visual.transform.scale.z = visual.transform.scale.z.clamp(0.01, 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn ray_plane_intersect_hit() {
        let origin = Vector3::new(0.0, 0.0, 10.0);
        let dir = Vector3::new(0.0, 0.0, -1.0);
        let plane_pt = Vector3::new(0.0, 0.0, 0.0);
        let plane_normal = Vector3::new(0.0, 0.0, 1.0);
        let hit = InteractionController::ray_plane_intersect(origin, dir, plane_pt, plane_normal);
        assert!(hit.is_some());
        let h = hit.unwrap();
        assert!(approx_eq(h.x, 0.0, 1e-6));
        assert!(approx_eq(h.y, 0.0, 1e-6));
        assert!(approx_eq(h.z, 0.0, 1e-6));
    }

    #[test]
    fn ray_plane_intersect_miss_parallel() {
        let origin = Vector3::new(0.0, 0.0, 10.0);
        let dir = Vector3::new(1.0, 0.0, 0.0);
        let plane_pt = Vector3::new(0.0, 0.0, 0.0);
        let plane_normal = Vector3::new(0.0, 0.0, 1.0);
        let hit = InteractionController::ray_plane_intersect(origin, dir, plane_pt, plane_normal);
        assert!(hit.is_none(), "parallel ray should miss");
    }

    #[test]
    fn ray_plane_intersect_miss_behind() {
        let origin = Vector3::new(0.0, 0.0, -10.0);
        let dir = Vector3::new(0.0, 0.0, -1.0);
        let plane_pt = Vector3::new(0.0, 0.0, 0.0);
        let plane_normal = Vector3::new(0.0, 0.0, 1.0);
        let hit = InteractionController::ray_plane_intersect(origin, dir, plane_pt, plane_normal);
        assert!(hit.is_none(), "ray behind plane should miss");
    }

    #[test]
    fn ndc_conversion_center() {
        let mut ctrl = InteractionController::new();
        ctrl.window_size = (1280.0, 720.0);
        let (nx, ny) = ctrl.ndc(640.0, 360.0);
        assert!(approx_eq(nx, 0.0, 1e-4));
        assert!(approx_eq(ny, 0.0, 1e-4));
    }

    #[test]
    fn ndc_conversion_corner() {
        let mut ctrl = InteractionController::new();
        ctrl.window_size = (1280.0, 720.0);
        let (nx, ny) = ctrl.ndc(0.0, 0.0);
        assert!(approx_eq(nx, -1.0, 1e-4));
        assert!(approx_eq(ny, 1.0, 1e-4));
    }

    #[test]
    fn drag_plane_normal_front_facing_camera_falls_back_to_forward() {
        // Regression: with a front-facing camera (yaw = 0) the vertical
        // wall plane is edge-on to the view ray; the fallback keeps drags
        // working by using a screen-parallel plane.
        let fwd = Vector3::new(0.0, 0.0, -1.0);
        let ray = Vector3::new(0.0, 0.0, -1.0);
        let n = InteractionController::drag_plane_normal(fwd, ray);
        assert!(
            n.dot(ray).abs() > 0.5,
            "plane must not be parallel to the view ray"
        );
        // Orbited camera with an off-center ray keeps the vertical wall plane.
        let yaw = 0.5f32;
        let fwd2 = Vector3::new(-yaw.sin(), 0.0, -yaw.cos());
        let right2 = Vector3::new(yaw.cos(), 0.0, -yaw.sin());
        let ray2 = (fwd2 + right2 * 0.3).normalize();
        let n2 = InteractionController::drag_plane_normal(fwd2, ray2);
        assert!(approx_eq(n2.y, 0.0, 1e-6), "wall plane stays vertical");
        assert!(
            n2.dot(ray2).abs() > 1e-3,
            "wall plane intersects an off-center view ray"
        );
    }

    // ── I2: interaction state machine regression tests ──────────────

    /// Two 400x300 visuals in front of a straight-on camera: left at
    /// (-220, 0, 0), right at (220, 0, 0). Screen x=420 hits the left
    /// visual, x=860 the right (ortho 1:1 with 1280 px width).
    /// Returns (scene, [left, right]).
    fn two_visual_scene() -> (Scene, [VisualId; 2]) {
        let mut scene = Scene::default();
        let mut left = crate::scene::Visual::new_test(400, 300);
        left.transform.position = Vector3::new(-220.0, 0.0, 0.0);
        let mut right = crate::scene::Visual::new_test(400, 300);
        right.transform.position = Vector3::new(220.0, 0.0, 0.0);
        let ids = [left.id, right.id];
        scene.add(left);
        scene.add(right);
        (scene, ids)
    }

    /// Front-facing camera matching normal (2D) mode: z = 500, yaw = 0.
    fn front_camera() -> Camera {
        let mut cam = Camera::new();
        cam.position = cgmath::Point3::new(0.0, 0.0, 500.0);
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        cam
    }

    #[test]
    fn pointer_down_without_modifiers_selects_but_does_not_drag() {
        let (mut scene, ids) = two_visual_scene();
        let mut ctrl = InteractionController::new();
        let mode = ctrl.handle_pointer_down(
            420.0, 360.0, &mut scene, &front_camera(), false, false, false, false,
            Some(ids.to_vec()),
        );
        assert_eq!(mode, None, "no modifier — event is for content, not scene");
        assert!(!ctrl.is_dragging());
        assert_eq!(scene.selected_id, Some(ids[0]), "left visual picked");
    }

    #[test]
    fn pointer_down_respects_workspace_visible_ids() {
        let (mut scene, ids) = two_visual_scene();
        // Overlapping duplicate directly in front of the left visual.
        let mut decoy = crate::scene::Visual::new_test(400, 300);
        decoy.transform.position = Vector3::new(-220.0, 0.0, 10.0);
        let decoy_id = decoy.id;
        scene.add(decoy);

        // Restrict to the right visual only: its screen position must pick it.
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            860.0, 360.0, &mut scene, &front_camera(), false, false, false, false,
            Some(vec![ids[1]]),
        );
        assert_eq!(scene.selected_id, Some(ids[1]));

        // Restrict to the decoy: same ray as the left visual must select the decoy.
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            420.0, 360.0, &mut scene, &front_camera(), false, false, false, false,
            Some(vec![decoy_id]),
        );
        assert_eq!(scene.selected_id, Some(decoy_id));
    }

    #[test]
    fn pointer_down_on_empty_space_deselects() {
        let (mut scene, ids) = two_visual_scene();
        scene.select(Some(ids[0]));
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            10.0, 10.0, &mut scene, &front_camera(), false, false, false, false,
            Some(ids.to_vec()),
        );
        assert_eq!(scene.selected_id, None, "miss clears selection");
    }

    #[test]
    fn modifier_drag_starts_only_with_modifier() {
        let (mut scene, ids) = two_visual_scene();
        let mut ctrl = InteractionController::new();
        let mode = ctrl.handle_pointer_down(
            420.0, 360.0, &mut scene, &front_camera(), false, true, false, false,
            Some(ids.to_vec()),
        );
        assert!(matches!(mode, Some(ManipMode::RotateY)), "shift starts rotate drag");
        assert!(ctrl.is_dragging());
        ctrl.handle_pointer_up();
        assert!(!ctrl.is_dragging(), "release terminates drag");
    }

    #[test]
    fn front_facing_title_bar_drag_moves_window() {
        // Regression for the front-facing camera drag deadlock.
        let (mut scene, ids) = two_visual_scene();
        let camera = front_camera();
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            420.0, 360.0, &mut scene, &camera, false, false, false, false,
            Some(ids.to_vec()),
        );
        let start = scene.get(ids[0]).unwrap().transform.position;
        ctrl.force_translate(420.0, 360.0, &mut scene, &camera, false);
        assert!(ctrl.is_dragging(), "drag must start with a front-facing camera");

        ctrl.handle_pointer_move(520.0, 360.0, &mut scene, &camera, false);
        let after = scene.get(ids[0]).unwrap().transform.position;
        assert!(
            (after.x - (start.x + 100.0)).abs() < 1.0,
            "ortho 1:1: +100 px screen = +100 world x, got {} -> {}",
            start.x,
            after.x
        );
        assert!(approx_eq(after.z, start.z, 1e-4), "depth unchanged (screen-parallel plane)");
        assert!(approx_eq(after.y, start.y, 1e-4), "no vertical drift");
    }

    #[test]
    fn drag_moves_only_target_visual_and_never_camera() {
        let (mut scene, ids) = two_visual_scene();
        let camera = front_camera();
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            860.0, 360.0, &mut scene, &camera, false, false, false, false,
            Some(ids.to_vec()),
        );
        ctrl.force_translate(860.0, 360.0, &mut scene, &camera, false);
        let other_before = scene.get(ids[0]).unwrap().transform.position;
        let cam_before = (
            camera.position,
            camera.yaw,
            camera.pitch,
        );

        ctrl.handle_pointer_move(400.0, 300.0, &mut scene, &camera, false);

        let other_after = scene.get(ids[0]).unwrap().transform.position;
        assert_eq!(other_before, other_after, "non-dragged visual untouched");
        assert_eq!(
            cam_before,
            (camera.position, camera.yaw, camera.pitch),
            "camera is passed by shared reference and must never move"
        );
    }

    #[test]
    fn drag_at_depth_stays_in_visual_plane() {
        let mut scene = Scene::default();
        let mut v = crate::scene::Visual::new_test(400, 300);
        v.transform.position = Vector3::new(0.0, 0.0, 300.0);
        let vid = v.id;
        scene.add(v);
        let camera = front_camera();
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            640.0, 360.0, &mut scene, &camera, false, false, false, false,
            Some(vec![vid]),
        );
        ctrl.force_translate(640.0, 360.0, &mut scene, &camera, false);
        ctrl.handle_pointer_move(840.0, 460.0, &mut scene, &camera, false);
        let after = scene.get(vid).unwrap().transform.position;
        assert!(
            approx_eq(after.z, 300.0, 1e-3),
            "visual keeps its own depth, got z={}",
            after.z
        );
    }

    #[test]
    fn no_movement_after_pointer_release() {
        let (mut scene, ids) = two_visual_scene();
        let camera = front_camera();
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            420.0, 360.0, &mut scene, &camera, false, false, false, false,
            Some(ids.to_vec()),
        );
        ctrl.force_translate(420.0, 360.0, &mut scene, &camera, false);
        ctrl.handle_pointer_move(520.0, 360.0, &mut scene, &camera, false);
        ctrl.handle_pointer_up();
        let frozen = scene.get(ids[0]).unwrap().transform.position;
        ctrl.handle_pointer_move(1200.0, 100.0, &mut scene, &camera, false);
        ctrl.handle_pointer_move(0.0, 700.0, &mut scene, &camera, false);
        assert_eq!(
            frozen,
            scene.get(ids[0]).unwrap().transform.position,
            "no movement after release"
        );
    }

    #[test]
    fn drag_of_removed_visual_is_safe_noop() {
        let (mut scene, ids) = two_visual_scene();
        let camera = front_camera();
        let mut ctrl = InteractionController::new();
        ctrl.handle_pointer_down(
            420.0, 360.0, &mut scene, &camera, false, false, false, false,
            Some(ids.to_vec()),
        );
        ctrl.force_translate(420.0, 360.0, &mut scene, &camera, false);
        scene.remove(ids[0]); // window closed mid-drag
        let other_before = scene.get(ids[1]).unwrap().transform.position;
        ctrl.handle_pointer_move(900.0, 500.0, &mut scene, &camera, false);
        assert_eq!(other_before, scene.get(ids[1]).unwrap().transform.position);
        ctrl.handle_pointer_up();
        assert!(!ctrl.is_dragging());
    }

    #[test]
    fn orbited_camera_drag_stays_on_drag_plane() {
        let mut scene = Scene::default();
        let mut v = crate::scene::Visual::new_test(400, 300);
        v.transform.position = Vector3::new(-220.0, 0.0, 0.0);
        let vid = v.id;
        scene.add(v);
        let mut camera = Camera::new();
        camera.position = cgmath::Point3::new(0.0, 0.0, 800.0);
        camera.yaw = 0.5;
        camera.pitch = 0.2;
        let mut ctrl = InteractionController::new();
        scene.select(Some(vid));
        ctrl.force_translate(640.0, 360.0, &mut scene, &camera, true);
        assert!(ctrl.is_dragging(), "orbited camera drags start");
        let start = scene.get(vid).unwrap().transform.position;
        ctrl.handle_pointer_move(800.0, 420.0, &mut scene, &camera, true);
        ctrl.handle_pointer_move(500.0, 250.0, &mut scene, &camera, true);
        let after = scene.get(vid).unwrap().transform.position;
        // Translation moves along the drag plane captured at drag start, so
        // the delta from the start position must be perpendicular to it.
        let fwd = camera.forward();
        let center_ray = {
            let (near, far) = ctrl.world_ray(0.0, 0.0, &camera, true);
            (far - near).normalize()
        };
        let normal = InteractionController::drag_plane_normal(fwd, center_ray);
        let drift = (after - start).dot(normal);
        assert!(
            drift.abs() < 1.0,
            "drag delta drifted off the drag plane, drift={}",
            drift
        );
        assert!(
            (after - start).magnitude() > 1.0,
            "drag actually moved the visual"
        );
    }
}
