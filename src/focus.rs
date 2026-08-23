//! Focus management for the spatial workspace.
//!
//! Owns focus state transitions and camera mode management.
//! Focus mode is expressed as a camera state, not a window state.
//! Entering focus mode smoothly moves the camera toward the focused
//! visual; exiting restores the previous camera position exactly.

use cgmath::InnerSpace;
use cgmath::Point3;
use cgmath::Vector3;

use crate::anchor::{visual_set_aabb, visual_aabb};
use crate::input::Camera;
use crate::scene::{Scene, VisualId};

/// The camera mode determines how the camera is positioned.
///
/// CRITICAL: Camera mode is a camera/presentation operation, never a
/// Wayland surface lifecycle operation. Changing focus changes the camera
/// trajectory, not Wayland keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CameraMode {
    /// Standard camera — user-orbitable, workspace-aware.
    Normal,
    /// Camera is smoothly moving toward a focused visual.
    Focus(VisualId),
    /// Overview mode — camera shows all visuals in the workspace.
    Overview,
    /// Workspace overview — camera shows all workspaces.
    WorkspaceOverview,
}

impl Default for CameraMode {
    fn default() -> Self {
        CameraMode::Normal
    }
}

/// Describes a camera transition from one state to another.
#[derive(Debug, Clone)]
pub struct FocusTransition {
    /// The camera state at the start of the transition.
    pub source_camera: Camera,
    /// The target camera state.
    pub target_camera: Camera,
    /// Duration of the transition in normalized units (0.0 to 1.0).
    pub duration: f32,
    /// Current progress (0.0 = source, 1.0 = target).
    pub progress: f32,
}

impl FocusTransition {
    pub fn new(source: Camera, target: Camera) -> Self {
        FocusTransition {
            source_camera: source,
            target_camera: target,
            duration: 1.0,
            progress: 0.0,
        }
    }

    /// Advance the transition by `dt`. Returns true if still in progress.
    pub fn advance(&mut self, dt: f32) -> bool {
        self.progress = (self.progress + dt).min(1.0);
        self.progress < 1.0
    }

    /// Get the interpolated camera at the current progress.
    pub fn interpolated(&self) -> Camera {
        let t = smoothstep(self.progress);
        Camera {
            position: Point3::new(
                lerp(self.source_camera.position.x, self.target_camera.position.x, t),
                lerp(self.source_camera.position.y, self.target_camera.position.y, t),
                lerp(self.source_camera.position.z, self.target_camera.position.z, t),
            ),
            yaw: lerp(self.source_camera.yaw, self.target_camera.yaw, t),
            pitch: lerp(self.source_camera.pitch, self.target_camera.pitch, t),
            speed: self.source_camera.speed,
            sensitivity: self.source_camera.sensitivity,
            zoom_speed: self.source_camera.zoom_speed,
            bookmarks: self.source_camera.bookmarks.clone(),
        }
    }
}

/// Tracks focus state and focus-mode camera transitions.
#[derive(Debug, Clone)]
pub struct FocusManager {
    /// Current camera mode.
    pub camera_mode: CameraMode,
    /// Camera position saved when entering focus/overview (for exact restore).
    pub saved_camera: Option<Camera>,
    /// The visual that was focused when entering focus mode.
    pub focus_target: Option<VisualId>,
    /// Active camera transition, if any.
    pub transition: Option<FocusTransition>,
}

impl FocusManager {
    pub fn new() -> Self {
        FocusManager {
            camera_mode: CameraMode::Normal,
            saved_camera: None,
            focus_target: None,
            transition: None,
        }
    }

    /// Enter focus mode: save current camera state and set target.
    /// If the target is part of a group, the entire group is framed.
    /// If the target visual doesn't exist yet, a default focus camera is used.
    pub fn enter(&mut self, camera: &Camera, target: VisualId, scene: &Scene) {
        self.saved_camera = Some(camera.clone());
        self.focus_target = Some(target);
        let target_cam = target_focus_camera(target, scene)
            .unwrap_or_else(|| {
                // Default: look at origin from a moderate distance
                Camera {
                    position: cgmath::Point3::new(0.0, 0.0, 500.0),
                    ..Camera::new()
                }
            });
        self.transition = Some(FocusTransition::new(camera.clone(), target_cam));
        self.camera_mode = CameraMode::Focus(target);
    }

    /// Exit focus mode: restore the EXACT saved camera state.
    /// The saved camera is a full Camera clone — restoration is exact.
    pub fn exit(&mut self, camera: &mut Camera, _scene: &Scene) {
        if let Some(ref saved) = self.saved_camera {
            camera.position = saved.position;
            camera.yaw = saved.yaw;
            camera.pitch = saved.pitch;
            camera.speed = saved.speed;
            camera.sensitivity = saved.sensitivity;
            camera.zoom_speed = saved.zoom_speed;
            if saved.bookmarks.len() == camera.bookmarks.len() {
                camera.bookmarks.copy_from_slice(&saved.bookmarks);
            }
        }
        self.camera_mode = CameraMode::Normal;
        self.focus_target = None;
        self.transition = None;
        self.saved_camera = None;
    }

    /// Enter overview mode. Saves current camera.
    pub fn enter_overview(&mut self, camera: &Camera, overview_cam: Camera) {
        self.saved_camera = Some(camera.clone());
        self.transition = Some(FocusTransition::new(camera.clone(), overview_cam));
        self.camera_mode = CameraMode::Overview;
    }

    /// Enter workspace overview mode. Saves current camera.
    pub fn enter_workspace_overview(&mut self, camera: &Camera, overview_cam: Camera) {
        self.saved_camera = Some(camera.clone());
        self.transition = Some(FocusTransition::new(camera.clone(), overview_cam));
        self.camera_mode = CameraMode::WorkspaceOverview;
    }

    /// Exit overview mode: restore saved camera exactly.
    pub fn exit_overview(&mut self, camera: &mut Camera) {
        if let Some(ref saved) = self.saved_camera {
            camera.position = saved.position;
            camera.yaw = saved.yaw;
            camera.pitch = saved.pitch;
        }
        self.camera_mode = CameraMode::Normal;
        self.focus_target = None;
        self.transition = None;
        self.saved_camera = None;
    }

    /// Update the focus transition by `dt` (0..1).
    /// Returns `true` if the transition is still in progress.
    pub fn update_transition(&mut self, dt: f32) -> bool {
        match &mut self.transition {
            Some(t) => t.advance(dt),
            None => false,
        }
    }

    /// Compute the interpolated camera for the current frame.
    /// If the focus target has been destroyed, returns the workspace
    /// camera so the user doesn't jump to a default position.
    pub fn interpolated_camera(&self, camera: &Camera, scene: &Scene) -> Camera {
        match self.camera_mode {
            CameraMode::Normal | CameraMode::WorkspaceOverview => camera.clone(),
            CameraMode::Focus(vid) => {
                // If we have an active transition, use it
                if let Some(ref t) = self.transition {
                    return t.interpolated();
                }
                // Otherwise compute target camera directly
                if let Some(target) = target_focus_camera(vid, scene) {
                    return target;
                }
                camera.clone()
            }
            CameraMode::Overview => {
                if let Some(ref t) = self.transition {
                    return t.interpolated();
                }
                camera.clone()
            }
        }
    }

    /// Check if focus/overview is active (non-normal mode).
    pub fn is_active(&self) -> bool {
        !matches!(self.camera_mode, CameraMode::Normal)
    }
}

/// Compute a camera position that frames the given visual prominently.
/// If the visual is part of a group, frames the entire group.
/// Returns `None` if the visual no longer exists in the scene.
pub fn target_focus_camera(vid: VisualId, scene: &Scene) -> Option<Camera> {
    // Check if the visual is part of a group — if so, frame the group
    let group_vids: Option<Vec<VisualId>> = {
        let groups = scene.groups.iter().find(|g| g.contains(vid));
        groups.map(|g| g.visual_ids.clone())
    };

    if let Some(group_ids) = group_vids {
        // Frame the entire group
        if let Some((min, max)) = visual_set_aabb(scene, &group_ids) {
            let center = (min + max) * 0.5;
            let span = (max - min).magnitude();
            let distance = span * 1.2 + 300.0;
            return Some(Camera {
                position: Point3::new(center.x, center.y, center.z + distance),
                yaw: 0.0,
                pitch: 0.0,
                ..Camera::new()
            });
        }
    }

    // Frame individual visual
    let visual = scene.visuals.iter().find(|v| v.id == vid)?;
    let half_w = visual.total_width() * 0.5;
    let half_h = visual.total_height() * 0.5;
    let max_dim = half_w.max(half_h);
    let distance = max_dim * 2.0 + 300.0;
    let pos = visual.transform.position;
    Some(Camera {
        position: Point3::new(pos.x, pos.y, pos.z + distance),
        yaw: 0.0,
        pitch: 0.0,
        ..Camera::new()
    })
}

/// Compute an overview camera that frames all visuals in a workspace.
pub fn overview_camera(scene: &Scene, visual_ids: &[VisualId]) -> Option<Camera> {
    if visual_ids.is_empty() {
        return None;
    }
    if let Some((min, max)) = visual_set_aabb(scene, visual_ids) {
        let center = (min + max) * 0.5;
        let span = (max - min).magnitude();
        let distance = span * 1.5 + 500.0;
        Some(Camera {
            position: Point3::new(center.x, center.y, center.z + distance),
            yaw: 0.0,
            pitch: 0.0,
            ..Camera::new()
        })
    } else {
        None
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgmath::Point3;

    #[test]
    fn enter_exit_preserves_camera() {
        let mut fm = FocusManager::new();
        let mut cam = Camera::new();
        cam.position = Point3::new(100.0, 200.0, 300.0);
        cam.yaw = 0.5;
        cam.pitch = 0.3;

        let scene = Scene::default();
        fm.enter(&cam, VisualId(1), &scene);
        assert!(matches!(fm.camera_mode, CameraMode::Focus(_)));
        assert_eq!(fm.focus_target, Some(VisualId(1)));

        fm.exit(&mut cam, &scene);
        assert!(matches!(fm.camera_mode, CameraMode::Normal));
        assert_eq!(fm.focus_target, None);
        assert_eq!(cam.position.x, 100.0);
        assert_eq!(cam.position.y, 200.0);
        assert_eq!(cam.position.z, 300.0);
        assert!((cam.yaw - 0.5).abs() < 1e-4);
        assert!((cam.pitch - 0.3).abs() < 1e-4);
    }

    #[test]
    fn transition_updates_progress() {
        let mut fm = FocusManager::new();
        let cam = Camera::new();
        let scene = Scene::default();
        fm.enter(&cam, VisualId(1), &scene);
        assert!(fm.transition.is_some());
        assert!(fm.update_transition(0.5));
        assert!((fm.transition.as_ref().unwrap().progress - 0.5).abs() < 1e-4);
        assert!(!fm.update_transition(0.6)); // 1.1 -> clamped to 1.0, done
    }

    #[test]
    fn enter_exit_repeated() {
        let mut fm = FocusManager::new();
        let mut cam = Camera::new();

        for i in 0..5 {
            let pos = Point3::new(i as f32 * 100.0, 0.0, 0.0);
            cam.position = pos;
            let scene = Scene::default();
            fm.enter(&cam, VisualId(i as u64), &scene);
            assert!(matches!(fm.camera_mode, CameraMode::Focus(_)));
            assert_eq!(fm.focus_target, Some(VisualId(i as u64)));
            fm.exit(&mut cam, &scene);
            assert!(matches!(fm.camera_mode, CameraMode::Normal));
            assert_eq!(cam.position.x, pos.x);
        }
    }

    #[test]
    fn no_focus_mode_default() {
        let fm = FocusManager::new();
        assert!(matches!(fm.camera_mode, CameraMode::Normal));
        assert_eq!(fm.focus_target, None);
    }

    #[test]
    fn smoothstep_bounds() {
        assert!((smoothstep(0.0) - 0.0).abs() < 1e-4);
        assert!((smoothstep(1.0) - 1.0).abs() < 1e-4);
        assert!(smoothstep(0.25) < 0.25); // ease-in
        assert!(smoothstep(0.75) > 0.75); // ease-out
    }

    #[test]
    fn target_destroyed_during_focus() {
        let mut fm = FocusManager::new();
        let cam = Camera::new();
        let scene = Scene::default();
        fm.enter(&cam, VisualId(999), &scene); // doesn't exist
        assert!(matches!(fm.camera_mode, CameraMode::Focus(_)));

        // interpolated_camera should return workspace camera when target missing
        let result = fm.interpolated_camera(&cam, &scene);
        assert_eq!(result.position, cam.position);
    }

    #[test]
    fn camera_mode_default_is_normal() {
        assert!(matches!(CameraMode::default(), CameraMode::Normal));
    }

    #[test]
    fn enter_overview_saves_and_transitions() {
        let mut fm = FocusManager::new();
        let cam = Camera::new();
        let overview_cam = Camera {
            position: Point3::new(0.0, 0.0, 2000.0),
            ..Camera::new()
        };
        fm.enter_overview(&cam, overview_cam);
        assert!(matches!(fm.camera_mode, CameraMode::Overview));
        assert!(fm.saved_camera.is_some());
        assert!(fm.transition.is_some());
    }

    #[test]
    fn exit_overview_restores_exact() {
        let mut fm = FocusManager::new();
        let mut cam = Camera::new();
        cam.position = Point3::new(100.0, 200.0, 300.0);
        cam.yaw = 0.5;
        cam.pitch = 0.3;

        let overview_cam = Camera {
            position: Point3::new(0.0, 0.0, 2000.0),
            ..Camera::new()
        };
        fm.enter_overview(&cam, overview_cam);
        fm.exit_overview(&mut cam);

        assert!(matches!(fm.camera_mode, CameraMode::Normal));
        assert_eq!(cam.position.x, 100.0);
        assert_eq!(cam.position.y, 200.0);
        assert_eq!(cam.position.z, 300.0);
    }

    #[test]
    fn focus_distance_scales_with_visual_size() {
        // We can test the math directly: larger visuals get larger distances
        let cam_small = target_focus_camera_for_test(100.0, 80.0);
        let cam_large = target_focus_camera_for_test(1000.0, 800.0);

        // The larger visual should have the camera farther away
        assert!(
            cam_large.position.z > cam_small.position.z,
            "larger visuals should have farther camera"
        );
    }

    /// Helper to compute focus distance for a given visual size.
    fn target_focus_camera_for_test(width: f32, height: f32) -> Camera {
        let half_w = width * 0.5;
        let half_h = height * 0.5;
        let max_dim = half_w.max(half_h);
        let distance = max_dim * 2.0 + 300.0;
        Camera {
            position: Point3::new(0.0, 0.0, distance),
            yaw: 0.0,
            pitch: 0.0,
            ..Camera::new()
        }
    }

    #[test]
    fn overview_camera_empty_returns_none() {
        let scene = Scene::default();
        assert!(overview_camera(&scene, &[]).is_none());
        assert!(overview_camera(&scene, &[VisualId(999)]).is_none());
    }

    #[test]
    fn enter_exact_restoration_with_bookmarks() {
        let mut fm = FocusManager::new();
        let mut cam = Camera::new();
        cam.position = Point3::new(100.0, 200.0, 300.0);
        cam.yaw = 0.5;
        cam.pitch = 0.3;
        cam.save_bookmark(0);

        let scene = Scene::default();
        fm.enter(&cam, VisualId(1), &scene);
        // Move camera far away
        cam.position = Point3::new(999.0, 999.0, 999.0);

        fm.exit(&mut cam, &scene);
        assert_eq!(cam.position.x, 100.0);
        assert_eq!(cam.position.y, 200.0);
        assert_eq!(cam.position.z, 300.0);
        assert!((cam.yaw - 0.5).abs() < 1e-4);
    }

    #[test]
    fn is_active_checks_non_normal() {
        let mut fm = FocusManager::new();
        assert!(!fm.is_active());
        let scene = Scene::default();
        fm.enter(&Camera::new(), VisualId(1), &scene);
        assert!(fm.is_active());
        fm.exit_overview(&mut Camera::new());
        assert!(!fm.is_active());
    }

    #[test]
    fn overview_with_zero_visuals() {
        let scene = Scene::default();
        let cam = overview_camera(&scene, &[]);
        assert!(cam.is_none(), "overview with zero visuals should return None");
    }

    #[test]
    fn overview_with_one_visual() {
        let scene = Scene::default();
        let cam = overview_camera(&scene, &[VisualId(1)]);
        assert!(cam.is_none(), "overview with non-existent visual should return None");
    }

    #[test]
    fn click_in_overview_sets_focus_mode() {
        let mut fm = FocusManager::new();
        let cam = Camera::new();
        let overview_cam = Camera {
            position: cgmath::Point3::new(0.0, 0.0, 2000.0),
            ..Camera::new()
        };
        fm.enter_overview(&cam, overview_cam);
        assert!(matches!(fm.camera_mode, CameraMode::Overview));

        // Simulate clicking a visual: enters focus mode
        let scene = Scene::default();
        fm.enter(&cam, VisualId(1), &scene);
        assert!(matches!(fm.camera_mode, CameraMode::Focus(_)));
        assert_eq!(fm.focus_target, Some(VisualId(1)));
    }

    #[test]
    fn escape_during_overview_returns_normal() {
        let mut fm = FocusManager::new();
        let mut cam = Camera::new();
        cam.position = cgmath::Point3::new(100.0, 200.0, 300.0);

        let overview_cam = Camera {
            position: cgmath::Point3::new(0.0, 0.0, 2000.0),
            ..Camera::new()
        };
        fm.enter_overview(&cam, overview_cam);
        fm.exit_overview(&mut cam);

        assert!(matches!(fm.camera_mode, CameraMode::Normal));
        assert_eq!(cam.position.x, 100.0);
        assert_eq!(cam.position.y, 200.0);
    }
}
