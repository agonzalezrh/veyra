//! Focus management for the spatial workspace.
//!
//! Owns focus state transitions and the focus-mode camera.
//! Focus mode is expressed as a camera state, not a window state.
//! Entering focus mode smoothly moves the camera toward the focused
//! visual; exiting restores the previous camera position.

use cgmath::{Point3, Vector3};

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
    pub fn interpolated_camera(&self, camera: &Camera, scene: &Scene) -> Camera {
        if !self.focus_mode || self.transition >= 1.0 {
            return camera.clone();
        }

        let target_cam = match self.focus_target {
            Some(vid) => target_focus_camera(vid, scene, camera),
            None => return camera.clone(),
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
fn target_focus_camera(vid: VisualId, scene: &Scene, _current: &Camera) -> Camera {
    let mut cam = Camera::new();
    if let Some(visual) = scene.visuals.iter().find(|v| v.id == vid) {
        let size = visual.geometry.size;
        let max_dim = size.w.max(size.h) as f32;
        let distance = max_dim * 1.2 + 200.0;
        let pos = visual.transform.position;
        cam.position = Point3::new(pos.x, pos.y, pos.z + distance);
        cam.yaw = 0.0;
        cam.pitch = 0.0;
    }
    cam
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}
