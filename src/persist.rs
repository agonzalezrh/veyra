//! Workspace state persistence.
//!
//! Persists user spatial state (transforms, camera) to disk.
//! Uses atomic writes (tmp + fsync + rename) for crash safety.
//!
//! Persisted: Visual transform, scale, rotation, app_id, detached flag, camera.
//! NOT persisted: Wayland object IDs, hover, focus, drag state, transient data.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::scene::{Scene, Visual, VisualId};
use crate::input::Camera;

use serde::{Deserialize, Serialize};

const STATE_VERSION: u32 = 1;

/// The on-disk workspace state format.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub version: u32,
    pub visuals: Vec<VisualState>,
    pub camera: CameraState,
}

/// Persisted state for one visual.
#[derive(Debug, Serialize, Deserialize)]
pub struct VisualState {
    /// Stable identity for matching on restore.
    pub app_id: String,
    /// The visual's spatial transform.
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation: [f32; 4], // quaternion (w, x, y, z)
    pub scale: [f32; 3],    // (x, y, z)
    /// Whether the user manually moved this visual.
    pub detached: bool,
}

/// Persisted camera state.
#[derive(Debug, Serialize, Deserialize)]
pub struct CameraState {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl WorkspaceState {
    /// Capture the current workspace state from the scene and camera.
    pub fn capture(scene: &Scene, camera: &Camera) -> Self {
        let visuals: Vec<VisualState> = scene
            .visuals
            .iter()
            .filter_map(|v| {
                let app_id = v.decoration.title.clone();
                if app_id.is_empty() { return None; }
                Some(VisualState {
                    app_id,
                    x: v.transform.position.x,
                    y: v.transform.position.y,
                    z: v.transform.position.z,
                    rotation: [
                        v.transform.rotation.s,
                        v.transform.rotation.v.x,
                        v.transform.rotation.v.y,
                        v.transform.rotation.v.z,
                    ],
                    scale: [
                        v.transform.scale.x,
                        v.transform.scale.y,
                        v.transform.scale.z,
                    ],
                    detached: scene.detached_set.contains(&v.id),
                })
            })
            .collect();

        WorkspaceState {
            version: STATE_VERSION,
            visuals,
            camera: CameraState {
                x: camera.position.x,
                y: camera.position.y,
                z: camera.position.z,
                yaw: camera.yaw,
                pitch: camera.pitch,
            },
        }
    }

    /// Try to restore a visual's state by matching `app_id`.
    /// Returns `Some(VisualState)` if a match was found.
    pub fn find_visual(&self, app_id: &str) -> Option<&VisualState> {
        self.visuals.iter().find(|vs| vs.app_id == app_id)
    }

    /// Apply a saved visual state to a visual.
    pub fn apply_visual(&self, visual: &mut Visual, app_id: &str) -> bool {
        if let Some(vs) = self.find_visual(app_id) {
            visual.transform.position.x = vs.x;
            visual.transform.position.y = vs.y;
            visual.transform.position.z = vs.z;
            visual.transform.rotation.s = vs.rotation[0];
            visual.transform.rotation.v.x = vs.rotation[1];
            visual.transform.rotation.v.y = vs.rotation[2];
            visual.transform.rotation.v.z = vs.rotation[3];
            visual.transform.scale.x = vs.scale[0];
            visual.transform.scale.y = vs.scale[1];
            visual.transform.scale.z = vs.scale[2];
            true
        } else {
            false
        }
    }

    /// Apply saved camera state to a camera.
    pub fn apply_camera(&self, camera: &mut Camera) {
        camera.position.x = self.camera.x;
        camera.position.y = self.camera.y;
        camera.position.z = self.camera.z;
        camera.yaw = self.camera.yaw;
        camera.pitch = self.camera.pitch;
    }
}

/// Default path for workspace state file.
fn state_path() -> PathBuf {
    let mut path = PathBuf::from(
        std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| "/tmp".to_string()),
    );
    path.push("looking-glass-ng-state.json");
    path
}

/// Save workspace state to disk atomically.
pub fn save(state: &WorkspaceState) -> Result<(), String> {
    let path = state_path();
    let tmp_path = path.with_extension("json.tmp");

    let json = serde_json::to_string_pretty(state).map_err(|e| format!("serialize: {}", e))?;

    // Write to temp file
    let mut file = fs::File::create(&tmp_path).map_err(|e| format!("create tmp: {}", e))?;
    file.write_all(json.as_bytes())
        .map_err(|e| format!("write: {}", e))?;
    file.sync_all().map_err(|e| format!("fsync: {}", e))?;
    drop(file);

    // Atomic rename
    fs::rename(&tmp_path, &path).map_err(|e| format!("rename: {}", e))?;

    Ok(())
}

/// Load workspace state from disk.
pub fn load() -> Result<WorkspaceState, String> {
    let path = state_path();
    let data = fs::read_to_string(&path).map_err(|e| format!("read: {}", e))?;
    let state: WorkspaceState =
        serde_json::from_str(&data).map_err(|e| format!("parse: {}", e))?;

    if state.version != STATE_VERSION {
        return Err(format!(
            "version mismatch: expected {}, got {}",
            STATE_VERSION, state.version
        ));
    }
    Ok(state)
}

/// Check if a saved state exists on disk.
pub fn exists() -> bool {
    state_path().exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> WorkspaceState {
        WorkspaceState {
            version: STATE_VERSION,
            visuals: vec![
                VisualState {
                    app_id: "foot".into(),
                    x: 100.0,
                    y: 200.0,
                    z: 0.0,
                    rotation: [1.0, 0.0, 0.0, 0.0],
                    scale: [1.0, 1.0, 1.0],
                    detached: true,
                },
            ],
            camera: CameraState {
                x: 0.0, y: 0.0, z: 500.0,
                yaw: 0.0, pitch: 0.0,
            },
        }
    }

    #[test]
    fn serialize_round_trip() {
        let state = make_state();
        let json = serde_json::to_string(&state).unwrap();
        let restored: WorkspaceState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        assert_eq!(restored.visuals.len(), 1);
        assert_eq!(restored.visuals[0].app_id, "foot");
        assert_eq!(restored.visuals[0].x, 100.0);
    }

    #[test]
    fn find_visual_by_app_id() {
        let state = make_state();
        assert!(state.find_visual("foot").is_some());
        assert!(state.find_visual("nonexistent").is_none());
    }

    #[test]
    fn save_and_load_atomic() {
        let state = make_state();
        assert!(save(&state).is_ok());
        assert!(exists());
        let loaded = load().unwrap();
        assert_eq!(loaded.visuals.len(), 1);
        assert_eq!(loaded.visuals[0].app_id, "foot");
        // Clean up
        let _ = fs::remove_file(state_path());
        let _ = fs::remove_file(state_path().with_extension("json.tmp"));
    }

    #[test]
    fn camera_apply_round_trip() {
        let state = make_state();
        let mut camera = Camera::new();
        state.apply_camera(&mut camera);
        assert_eq!(camera.position.z, 500.0);
    }
}
