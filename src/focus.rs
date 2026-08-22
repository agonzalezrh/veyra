//! Focus management for the spatial workspace.
//!
//! Owns focus state transitions and the focus-mode camera.
//! Focus mode is expressed as a camera state, not a window state.
//! Entering focus mode smoothly moves the camera toward the focused
//! visual; exiting restores the previous camera position.

use cgmath::Point3;

use crate::input::Camera;
use crate::scene::{Scene, VisualId};

/// Tracks focus state and focus-mode camera transitions.
#[derive(Debug, Clone)]
pub struct FocusManager {
    /// Whether focus mode is active.
    pub focus_mode: bool,
    /// Camera position saved when entering focus mode (for restore).
    saved_camera: Option<Camera>,
    /// The visual that was focused when entering focus mode.
    pub focus_target: Option<VisualId>,
    /// Interpolation progress (0.0 = workspace, 1.0 = focused).
    pub transition: f32,
}

impl FocusManager {
    pub fn new() -> Self {
        FocusManager {
            focus_mode: false,
            saved_camera: None,
            focus_target: None,
            transition: 0.0,
        }
    }

    /// Enter focus mode: save current camera state and set target.
    pub fn enter(&mut self, camera: &Camera, target: VisualId) {
        self.focus_mode = true;
        self.saved_camera = Some(camera.clone());
        self.focus_target = Some(target);
        self.transition = 0.0;
    }

    /// Exit focus mode: restore the saved camera state.
    pub fn exit(&mut self, camera: &mut Camera, scene: &Scene) {
        if let Some(ref saved) = self.saved_camera {
            camera.position = saved.position;
            camera.yaw = saved.yaw;
            camera.pitch = saved.pitch;
        }
        self.focus_mode = false;
        self.focus_target = None;
        self.transition = 0.0;
        self.saved_camera = None;
    }

    /// Update the focus transition by `dt` (0..1).
    /// Returns `true` if the transition is still in progress.
    pub fn update_transition(&mut self, dt: f32) -> bool {
        if !self.focus_mode {
            return false;
        }
        self.transition = (self.transition + dt).min(1.0);
        self.transition < 1.0
    }

    /// Compute the interpolated camera for the current frame.
    /// Blends between the saved workspace camera and the target
    /// focus camera that frames the focused visual.
    /// If the focus target has been destroyed, returns the workspace
    /// camera so the user doesn't jump to a default position.
    pub fn interpolated_camera(&self, camera: &Camera, scene: &Scene) -> Camera {
        if !self.focus_mode {
            return camera.clone();
        }

        // If transition is complete, return the target camera
        if self.transition >= 1.0 {
            if let Some(vid) = self.focus_target {
                if let Some(target) = target_focus_camera(vid, scene) {
                    return target;
                }
            }
            return camera.clone();
        }

        let vid = match self.focus_target {
            Some(v) => v,
            None => return camera.clone(),
        };

        let target_cam = match target_focus_camera(vid, scene) {
            Some(c) => c,
            None => return camera.clone(), // target destroyed — stay at current
        };

        let saved = match self.saved_camera {
            Some(ref s) => s,
            None => return camera.clone(),
        };

        let t = smoothstep(self.transition);
        Camera {
            position: Point3::new(
                lerp(saved.position.x, target_cam.position.x, t),
                lerp(saved.position.y, target_cam.position.y, t),
                lerp(saved.position.z, target_cam.position.z, t),
            ),
            yaw: lerp(saved.yaw, target_cam.yaw, t),
            pitch: lerp(saved.pitch, target_cam.pitch, t),
            speed: camera.speed,
            sensitivity: camera.sensitivity,
            zoom_speed: camera.zoom_speed,
            bookmarks: camera.bookmarks.clone(),
        }
    }
}

/// Compute a camera position that frames the given visual prominently.
/// Returns `None` if the visual no longer exists in the scene.
fn target_focus_camera(vid: VisualId, scene: &Scene) -> Option<Camera> {
    let visual = scene.visuals.iter().find(|v| v.id == vid)?;
    let size = visual.geometry.size;
    let max_dim = size.w.max(size.h) as f32;
    let distance = max_dim * 1.2 + 200.0;
    let pos = visual.transform.position;
    Some(Camera {
        position: Point3::new(pos.x, pos.y, pos.z + distance),
        yaw: 0.0,
        pitch: 0.0,
        ..Camera::new()
    })
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
        fm.enter(&cam, VisualId(1));
        assert!(fm.focus_mode);
        assert_eq!(fm.focus_target, Some(VisualId(1)));

        fm.exit(&mut cam, &scene);
        assert!(!fm.focus_mode);
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
        fm.enter(&cam, VisualId(1));
        assert!(fm.update_transition(0.5));
        assert!((fm.transition - 0.5).abs() < 1e-4);
        assert!(!fm.update_transition(0.6)); // 1.1 -> clamped to 1.0, done
    }

    #[test]
    fn enter_exit_repeated() {
        // Multiple enter/exit cycles must not degrade state
        let mut fm = FocusManager::new();
        let mut cam = Camera::new();

        for i in 0..5 {
            let pos = Point3::new(i as f32 * 100.0, 0.0, 0.0);
            cam.position = pos;
            let scene = Scene::default();
            fm.enter(&cam, VisualId(i as u64));
            assert!(fm.focus_mode);
            assert_eq!(fm.focus_target, Some(VisualId(i as u64)));
            fm.exit(&mut cam, &scene);
            assert!(!fm.focus_mode);
            assert_eq!(cam.position.x, pos.x);
        }
    }

    #[test]
    fn no_focus_mode_default() {
        let fm = FocusManager::new();
        assert!(!fm.focus_mode);
        assert_eq!(fm.focus_target, None);
        assert!((fm.transition - 0.0).abs() < 1e-4);
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
        fm.enter(&cam, VisualId(999)); // doesn't exist
        assert!(fm.focus_mode);

        let scene = Scene::default();
        // interpolated_camera should return workspace camera when target missing
        let result = fm.interpolated_camera(&cam, &scene);
        assert_eq!(result.position, cam.position);
    }
}
