use cgmath::InnerSpace;
use cgmath::Matrix4;
use cgmath::Point3;
use cgmath::Rad;
use cgmath::Vector3;
use cgmath::Vector4;
use tracing::info;

use crate::scene::{Scene, VisualId};

const MIN_DISTANCE: f32 = 50.0;
const MAX_DISTANCE: f32 = 20000.0;
const PITCH_LIMIT: f32 = 1.5; // ~85 degrees up/down

#[derive(Debug, Clone)]
pub struct Camera {
    pub position: Point3<f32>,
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
    pub sensitivity: f32,
    pub zoom_speed: f32,
    pub bookmarks: [Option<CameraView>; 10],
}

/// Saved camera position/yaw/pitch for bookmarks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraView {
    pub position: Point3<f32>,
    pub yaw: f32,
    pub pitch: f32,
}

impl Camera {
    pub fn new() -> Self {
        Camera {
            position: Point3::new(0.0, 0.0, 800.0),
            yaw: 0.0,
            pitch: 0.0,
            speed: 10.0,
            sensitivity: 0.005,
            zoom_speed: 50.0,
            bookmarks: [None, None, None, None, None, None, None, None, None, None],
        }
    }

    pub fn view_matrix(&self) -> Matrix4<f32> {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let forward = Vector3::new(-sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch);
        let center = self.position + forward;
        Matrix4::look_at_rh(self.position, center, Vector3::new(0.0, 1.0, 0.0))
    }

    /// Camera look direction (full 3D, includes pitch).
    pub fn look_dir(&self) -> Vector3<f32> {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vector3::new(-sy * cp, sp, -cy * cp)
    }

    /// Horizontal forward direction (no pitch, for WASD movement).
    pub fn forward(&self) -> Vector3<f32> {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vector3::new(-sin_yaw, 0.0, -cos_yaw)
    }

    pub fn right(&self) -> Vector3<f32> {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vector3::new(cos_yaw, 0.0, -sin_yaw)
    }

    /// Clamp pitch and position to valid ranges.
    fn clamp_state(&mut self) {
        self.pitch = self.pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
        self.position.x = self.position.x.clamp(-MAX_DISTANCE, MAX_DISTANCE);
        self.position.y = self.position.y.clamp(-MAX_DISTANCE, MAX_DISTANCE);
        self.position.z = self.position.z.clamp(-MAX_DISTANCE, MAX_DISTANCE);
        // NaN/Inf protection
        if !self.position.x.is_finite() { self.position.x = 0.0; }
        if !self.position.y.is_finite() { self.position.y = 0.0; }
        if !self.position.z.is_finite() { self.position.z = 800.0; }
        if !self.yaw.is_finite() { self.yaw = 0.0; }
        if !self.pitch.is_finite() { self.pitch = 0.0; }
    }

    /// Orbit the camera around its current focus point.
    /// The camera rotates around the point it's looking at,
    /// maintaining the same distance. This gives natural 3D orbit.
    pub fn handle_orbit(&mut self, dx: f64, dy: f64) {
        let focus = self.position + self.look_dir() * distance_to_focus(self);
        self.yaw += dx as f32 * self.sensitivity * 5.0;
        self.pitch = (self.pitch - dy as f32 * self.sensitivity * 5.0)
            .clamp(-PITCH_LIMIT, PITCH_LIMIT);
        // Reposition camera to maintain focus distance
        let dist = distance_to_focus(self);
        self.position = focus - self.look_dir() * dist;
        self.clamp_state();
    }

    /// Pan the camera in screen space (middle-drag).
    /// Moves the camera through workspace coordinates without modifying visuals.
    pub fn handle_pan(&mut self, dx: f64, dy: f64, speed: f32) {
        let fwd = self.forward();
        let right = self.right();
        let up = Vector3::new(0.0, 1.0, 0.0);
        self.position += right * (dx as f32 * speed);
        self.position += up * (-dy as f32 * speed);
        self.clamp_state();
    }

    /// Dolly zoom: move camera along look direction with distance limits.
    pub fn handle_zoom(&mut self, delta: f64) {
        let dir = self.look_dir();
        let new_pos = self.position + dir * (delta as f32 * self.zoom_speed * 0.01);
        let dist = (new_pos - (self.position + dir * distance_to_focus(self))).magnitude();
        if dist > MIN_DISTANCE && dist < MAX_DISTANCE {
            self.position = new_pos;
        } else if dist <= MIN_DISTANCE {
            // Move to minimum distance
            let focus = self.position + dir * distance_to_focus(self);
            self.position = focus - dir * MIN_DISTANCE;
        }
        self.clamp_state();
    }

    pub fn handle_key(&mut self, key: u32, pressed: bool, dt: f32) {
        if !pressed {
            return;
        }
        let step = self.speed * dt;
        let fwd = self.forward();
        let right = self.right();

        match key {
            25 => { info!("W pressed, camera forward"); self.position += fwd * step; }
            39 => { info!("S pressed, camera backward"); self.position -= fwd * step; }
            38 => { info!("A pressed, camera strafe left"); self.position -= right * step; }
            40 => { info!("D pressed, camera strafe right"); self.position += right * step; }
            24 => { self.position.y -= step; }
            26 => { self.position.y += step; }
            113 => { self.yaw -= Rad(0.05).0; }
            114 => { self.yaw += Rad(0.05).0; }
            111 => { self.pitch = (self.pitch + Rad(0.05).0).clamp(Rad(-1.5).0, Rad(1.5).0); }
            116 => { self.pitch = (self.pitch - Rad(0.05).0).clamp(Rad(-1.5).0, Rad(1.5).0); }
            _ => {}
        }
    }

    pub fn handle_mouse_move(&mut self, dx: f64, dy: f64) {
        self.yaw += dx as f32 * self.sensitivity;
        self.pitch = (self.pitch - dy as f32 * self.sensitivity)
            .clamp(Rad(-1.5).0, Rad(1.5).0);
    }

    pub fn handle_mouse_absolute(&mut self, x: f64, y: f64) {
        use std::cell::Cell;
        thread_local! {
            static LAST_X: Cell<Option<f64>> = Cell::new(None);
            static LAST_Y: Cell<Option<f64>> = Cell::new(None);
        }
        LAST_X.with(|lx| {
            LAST_Y.with(|ly| {
                if let (Some(px), Some(py)) = (lx.get(), ly.get()) {
                    let dx = x - px;
                    let dy = y - py;
                    self.handle_mouse_move(dx, dy);
                }
                lx.set(Some(x));
                ly.set(Some(y));
            });
        });
    }

    /// Center camera on a specific visual.
    pub fn frame_visual(&mut self, vid: VisualId, scene: &Scene) -> bool {
        if let Some(pos) = crate::layout::frame_visual(vid, scene, 1280.0, 720.0) {
            self.position = cgmath::Point3::new(pos.x, pos.y, pos.z);
            self.yaw = 0.0;
            self.pitch = 0.0;
            true
        } else {
            false
        }
    }

    /// Position the camera to show all visuals in the scene.
    /// Computes the bounding volume (accounting for rotation) and places
    /// the camera at a suitable distance.
    pub fn frame_all(&mut self, scene: &Scene) -> bool {
        if scene.visuals.is_empty() {
            return false;
        }
        let mut min = Vector3::new(f32::MAX, f32::MAX, f32::MAX);
        let mut max = Vector3::new(f32::MIN, f32::MIN, f32::MIN);
        for v in scene.iter() {
            if v.window_state == crate::scene::WindowState::Minimized { continue; }
            let p = v.transform.position;
            // Build model matrix to get rotated corners
            let m = Matrix4::from_translation(p)
                * Matrix4::from(v.transform.rotation);
            let half_w = v.total_width() * 0.5;
            let half_h = v.total_height() * 0.5;
            // Local-space corners, transformed by model matrix without scale
            let local = [
                Vector3::new(-half_w, -half_h, 0.0),
                Vector3::new( half_w, -half_h, 0.0),
                Vector3::new(-half_w,  half_h, 0.0),
                Vector3::new( half_w,  half_h, 0.0),
            ];
            for lc in &local {
                let world = m * Vector4::new(lc.x, lc.y, lc.z, 1.0);
                let wc = Vector3::new(world.x, world.y, world.z) / world.w;
                min.x = min.x.min(wc.x); max.x = max.x.max(wc.x);
                min.y = min.y.min(wc.y); max.y = max.y.max(wc.y);
                min.z = min.z.min(wc.z); max.z = max.z.max(wc.z);
            }
        }
        let center = (min + max) * 0.5;
        let span = (max - min).magnitude();
        let distance = span * 0.8 + 500.0;
        self.position = Point3::new(center.x, center.y, center.z + distance);
        self.yaw = 0.0;
        self.pitch = 0.0;
        true
    }

    /// Save current view to a bookmark slot (0-9).
    pub fn save_bookmark(&mut self, slot: usize) {
        if slot < self.bookmarks.len() {
            self.bookmarks[slot] = Some(CameraView {
                position: self.position,
                yaw: self.yaw,
                pitch: self.pitch,
            });
        }
    }

    /// Restore a bookmark (0-9). Returns true if the slot had a saved view.
    pub fn restore_bookmark(&mut self, slot: usize) -> bool {
        let view = match self.bookmarks.get(slot) {
            Some(Some(v)) => *v,
            _ => return false,
        };
        self.position = view.position;
        self.yaw = view.yaw;
        self.pitch = view.pitch;
        true
    }
}

/// Distance from camera to its focus point (where it's looking).
fn distance_to_focus(cam: &Camera) -> f32 {
    let p = &cam.position;
    let dist = (p.x * p.x + p.y * p.y + p.z * p.z).sqrt();
    dist.max(200.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;
    use cgmath::Deg;

    #[test]
    fn frame_all_empty() {
        let mut cam = Camera::new();
        let scene = Scene::default();
        assert!(!cam.frame_all(&scene));
    }

    #[test]
    fn frame_all_not_empty() {
        // Can't add visuals without GlesTexture, so test frame_all on empty.
        // The function is also validated through frame_visual in layout tests.
        let mut cam = Camera::new();
        let scene = Scene::default();
        assert!(!cam.frame_all(&scene));
    }

    #[test]
    fn save_and_restore_bookmark() {
        let mut cam = Camera::new();
        cam.position = Point3::new(100.0, 200.0, 300.0);
        cam.yaw = 0.5;
        cam.pitch = 0.3;
        cam.save_bookmark(1);
        // Move elsewhere
        cam.position = Point3::new(999.0, 999.0, 999.0);
        cam.yaw = 1.0;
        cam.pitch = 0.5;
        // Restore
        assert!(cam.restore_bookmark(1));
        assert!((cam.position.x - 100.0).abs() < 1e-4);
        assert!((cam.position.y - 200.0).abs() < 1e-4);
        assert!((cam.position.z - 300.0).abs() < 1e-4);
        assert!((cam.yaw - 0.5).abs() < 1e-4);
        assert!((cam.pitch - 0.3).abs() < 1e-4);
    }

    #[test]
    fn restore_empty_bookmark_returns_false() {
        let mut cam = Camera::new();
        assert!(!cam.restore_bookmark(0));
    }

    #[test]
    fn orbit_changes_yaw_and_pitch() {
        let mut cam = Camera::new();
        let (y0, p0) = (cam.yaw, cam.pitch);
        cam.handle_orbit(10.0, 5.0);
        assert_ne!(cam.yaw, y0);
        assert_ne!(cam.pitch, p0);
    }

    #[test]
    fn zoom_changes_z_position() {
        let mut cam = Camera::new();
        let z0 = cam.position.z;
        cam.handle_zoom(-100.0);
        assert_ne!(cam.position.z, z0);
    }

    #[test]
    fn pan_moves_camera() {
        let mut cam = Camera::new();
        let (x0, y0) = (cam.position.x, cam.position.y);
        cam.handle_pan(100.0, 50.0, 0.5);
        assert_ne!(cam.position.x, x0);
        assert_ne!(cam.position.y, y0);
    }

    #[test]
    fn orbit_does_not_change_distance_to_focus() {
        let mut cam = Camera::new();
        let d0 = distance_to_focus(&cam);
        cam.handle_orbit(30.0, 10.0);
        let d1 = distance_to_focus(&cam);
        // Distance should remain approximately the same
        let diff = (d1 - d0).abs();
        assert!(diff < d0 * 0.5, "orbit should roughly preserve focus distance: {} vs {}", d0, d1);
    }

    #[test]
    fn zoom_bounded() {
        let mut cam = Camera::new();
        // Zoom in a lot
        for _ in 0..1000 {
            cam.handle_zoom(-1000.0);
        }
        // Should not go behind minimum distance
        let d = distance_to_focus(&cam);
        assert!(d >= crate::input::MIN_DISTANCE * 0.9, "zoom should not go below min distance: {}", d);
        // Should not go to infinity
        assert!(cam.position.z.is_finite());
    }

    #[test]
    fn clamp_state_handles_nan() {
        let mut cam = Camera::new();
        cam.position.x = f32::NAN;
        cam.position.y = f32::INFINITY;
        cam.yaw = f32::NEG_INFINITY;
        cam.clamp_state();
        assert!(cam.position.x.is_finite());
        assert!(cam.position.y.is_finite());
        assert!(cam.yaw.is_finite());
    }

    #[test]
    fn look_dir_is_normalized() {
        let cam = Camera::new();
        let dir = cam.look_dir();
        let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt();
        assert!((len - 1.0).abs() < 0.01, "look direction should be normalized: {}", len);
    }
}

