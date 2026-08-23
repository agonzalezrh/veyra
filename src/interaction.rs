use cgmath::InnerSpace;
use cgmath::Matrix4;
use cgmath::Rad;
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
    start_scale: Vector3<f32>,
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

    /// Handle a pointer button press.
    ///
    /// Always picks and selects the visual under the cursor (if any).
    /// Starts a manipulation drag only when a modifier key (shift/ctrl/alt) is held.
    ///
    /// Returns `Some(ManipMode)` if a drag was started, `None` otherwise.
    /// The caller (LookingGlass) uses this to decide whether to route the
    /// event to content input or scene manipulation.
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
    ) -> Option<ManipMode> {
        self.mouse_x = x;
        self.mouse_y = y;
        let (nx, ny) = self.ndc(x, y);
        let pv = self.proj_matrix(camera, spatial_mode) * camera.view_matrix();

        if !self.selection_enabled {
            return None;
        }

        let picked = scene.pick(&pv, nx, ny);
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
            let plane_normal = Vector3::new(fwd.z, 0.0, -fwd.x).normalize();

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
                    start_scale: visual.transform.scale,
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
            let plane_normal = Vector3::new(fwd.z, 0.0, -fwd.x).normalize();
            // Use a vertical plane for dragging to match expected behavior

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
                    start_scale: visual.transform.scale,
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
}
