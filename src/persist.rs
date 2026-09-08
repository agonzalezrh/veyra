//! Workspace state persistence v2.
//!
//! Persists user spatial state (transforms, camera) to disk.
//! Uses atomic writes (tmp + fsync + rename) for crash safety.
//!
//! v2 format: multi-workspace, each workspace has its own visuals, camera,
//! layout_mode, and detached set.
//! v1 format (single workspace) is detected and converted on load.
//!
//! Persisted: Visual transform, scale, rotation, app_id, detached flag, camera.
//! NOT persisted: Wayland object IDs, hover, focus, drag state, transient data.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::input::Camera;
use crate::layout::LayoutMode;
use crate::scene::{Scene, VisualId};

use serde::{Deserialize, Serialize};
use tracing::info;

pub const CURRENT_VERSION: u32 = 2;
pub const VERSION_1: u32 = 1;

/// Persisted camera state.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CameraState {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub pitch: f32,
}

/// Persisted state for one visual.
#[derive(Debug, Serialize, Deserialize, Clone)]
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

/// Single workspace in the v2 format.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkspaceEntry {
    pub visuals: Vec<VisualState>,
    pub camera: CameraState,
    pub layout_mode: String, // "freeform", "flat", "grid"
    pub detached: Vec<u64>,  // VisualId raw values that are detached
}

/// V1 format (single workspace, used for backward compatibility).
#[derive(Debug, Deserialize)]
struct WorkspaceStateV1 {
    version: u32,
    visuals: Vec<VisualState>,
    camera: CameraState,
}

/// The on-disk workspace state format.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub version: u32,
    pub workspaces: Vec<WorkspaceEntry>,
}

impl WorkspaceState {
    /// Capture all workspace states from the workspace manager, scene, and camera.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn capture(
        scene: &Scene,
        camera: &Camera,
        layout_mode: LayoutMode,
        detached_set: &[VisualId],
        _workspace_visuals: &[VisualId],
    ) -> Self {
        let visuals: Vec<VisualState> = scene
            .visuals
            .iter()
            .filter_map(|v| {
                // R3: the identity is the Wayland app_id (stable launch
                // key) — `decoration.title` was never populated, which
                // silently dropped every visual from saved state. Only
                // Wayland toplevels carry an app_id and are restorable;
                // producer visuals are re-created by their producers.
                let app_id = v.chrome.app_id.clone();
                if app_id.is_empty() {
                    return None;
                }
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
                    detached: detached_set.contains(&v.id),
                })
            })
            .collect();

        let detached: Vec<u64> = detached_set.iter().map(|id| id.0).collect();

        WorkspaceState {
            version: CURRENT_VERSION,
            workspaces: vec![WorkspaceEntry {
                visuals,
                camera: CameraState {
                    x: camera.position.x,
                    y: camera.position.y,
                    z: camera.position.z,
                    yaw: camera.yaw,
                    pitch: camera.pitch,
                },
                layout_mode: layout_mode_to_string(layout_mode),
                detached,
            }],
        }
    }

    /// Capture multi-workspace state from the full scene and workspace manager.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn capture_multi(
        scene: &Scene,
        workspace_visuals: &[Vec<VisualId>],
        workspace_cameras: &[Camera],
        workspace_layouts: &[LayoutMode],
        workspace_detached: &[Vec<VisualId>],
    ) -> Self {
        let count = workspace_visuals
            .len()
            .min(workspace_cameras.len())
            .min(workspace_layouts.len())
            .min(workspace_detached.len());
        let mut workspaces = Vec::with_capacity(count);

        for i in 0..count {
            let ws_visuals = &workspace_visuals[i];
            let ws_camera = &workspace_cameras[i];
            let ws_layout = workspace_layouts[i];
            let ws_detached = &workspace_detached[i];

            let visuals: Vec<VisualState> = scene
                .visuals
                .iter()
                .filter(|v| ws_visuals.contains(&v.id))
                .filter_map(|v| {
                    // R3: identity is chrome.app_id (see capture above).
                    let app_id = v.chrome.app_id.clone();
                    if app_id.is_empty() {
                        return None;
                    }
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
                        detached: ws_detached.contains(&v.id),
                    })
                })
                .collect();

            let detached: Vec<u64> = ws_detached.iter().map(|id| id.0).collect();

            workspaces.push(WorkspaceEntry {
                visuals,
                camera: CameraState {
                    x: ws_camera.position.x,
                    y: ws_camera.position.y,
                    z: ws_camera.position.z,
                    yaw: ws_camera.yaw,
                    pitch: ws_camera.pitch,
                },
                layout_mode: layout_mode_to_string(ws_layout),
                detached,
            });
        }

        WorkspaceState {
            version: CURRENT_VERSION,
            workspaces,
        }
    }

    /// Get the first workspace (for backward compat with single-workspace usage).
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn first_workspace(&self) -> Option<&WorkspaceEntry> {
        self.workspaces.first()
    }

    /// Get a workspace by index.
    pub fn workspace(&self, idx: usize) -> Option<&WorkspaceEntry> {
        self.workspaces.get(idx)
    }

    /// Get the number of workspaces.
    pub fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    /// Try to restore a visual's state by matching `app_id` in all workspaces.
    /// Returns `Some((workspace_index, &VisualState))` if a match was found.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn find_visual(&self, app_id: &str) -> Option<(usize, &VisualState)> {
        for (i, ws) in self.workspaces.iter().enumerate() {
            if let Some(vs) = ws.visuals.iter().find(|vs| vs.app_id == app_id) {
                return Some((i, vs));
            }
        }
        None
    }

    /// R3: consuming variant of [`find_visual`] — removes the matched
    /// entry so duplicate app_ids restore in capture order (the Nth
    /// window of an app takes the Nth saved transform) instead of every
    /// instance matching the first entry. Returns the workspace index
    /// the visual was saved in, so membership is restored faithfully.
    pub fn take_visual(&mut self, app_id: &str) -> Option<(usize, VisualState)> {
        for (i, ws) in self.workspaces.iter_mut().enumerate() {
            if let Some(pos) = ws.visuals.iter().position(|vs| vs.app_id == app_id) {
                let vs = ws.visuals.remove(pos);
                return Some((i, vs));
            }
        }
        None
    }

    /// Apply saved camera state from the first workspace to a camera.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn apply_camera(&self, camera: &mut Camera) {
        if let Some(ws) = self.workspaces.first() {
            camera.position.x = ws.camera.x;
            camera.position.y = ws.camera.y;
            camera.position.z = ws.camera.z;
            camera.yaw = ws.camera.yaw;
            camera.pitch = ws.camera.pitch;
        }
    }
}

#[allow(dead_code)] // reserved API surface; not yet wired
fn layout_mode_to_string(mode: LayoutMode) -> String {
    match mode {
        LayoutMode::Freeform => "freeform".into(),
        LayoutMode::Flat => "flat".into(),
        LayoutMode::Grid { columns } => format!("grid:{}", columns),
    }
}

#[allow(dead_code)] // reserved API surface; not yet wired
fn string_to_layout_mode(s: &str) -> LayoutMode {
    match s {
        "flat" => LayoutMode::Flat,
        s if s.starts_with("grid:") => {
            let cols = s[5..].parse().unwrap_or(3);
            LayoutMode::Grid { columns: cols }
        }
        _ => LayoutMode::Freeform,
    }
}

/// Default path for workspace state file.
fn state_path() -> PathBuf {
    let mut path =
        PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string()));
    path.push("veyra-state.json");
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

    // Fsync directory
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }

    // Atomic rename
    fs::rename(&tmp_path, &path).map_err(|e| format!("rename: {}", e))?;

    Ok(())
}

/// Load workspace state from disk.
/// Handles v1 -> v2 migration and corrupt-state recovery.
pub fn load() -> Result<WorkspaceState, String> {
    let path = state_path();
    let data = fs::read_to_string(&path).map_err(|e| format!("read: {}", e))?;

    // Try parsing as v2 first
    if let Ok(state) = serde_json::from_str::<WorkspaceState>(&data) {
        return Ok(state);
    }

    // Try parsing as v1 and convert
    if let Ok(v1) = serde_json::from_str::<WorkspaceStateV1>(&data) {
        if v1.version == VERSION_1 {
            let ws = WorkspaceEntry {
                visuals: v1.visuals,
                camera: v1.camera,
                layout_mode: "freeform".into(),
                detached: Vec::new(),
            };
            return Ok(WorkspaceState {
                version: CURRENT_VERSION,
                workspaces: vec![ws],
            });
        }
    }

    Err("invalid workspace state format".into())
}

/// Check if a saved state exists on disk.
#[allow(dead_code)] // reserved API surface; not yet wired
pub fn exists() -> bool {
    state_path().exists()
}

/// Remove saved state file (for testing).
#[allow(dead_code)] // reserved API surface; not yet wired
pub fn remove() {
    let path = state_path();
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(path.with_extension("json.tmp"));
}

/// Return the state path (exposed for testing).
pub fn state_path_for_test() -> PathBuf {
    state_path()
}

/// Back up a potentially corrupt state file.
/// Renames the file to `.json.bak` with a timestamp.
pub fn backup() {
    let path = state_path();
    if path.exists() {
        let bak = path.with_extension("json.bak");
        let _ = fs::rename(&path, &bak);
        info!("backed up state file to {:?}", bak);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_v2_state() -> WorkspaceState {
        WorkspaceState {
            version: CURRENT_VERSION,
            workspaces: vec![
                WorkspaceEntry {
                    visuals: vec![VisualState {
                        app_id: "foot".into(),
                        x: 100.0,
                        y: 200.0,
                        z: 0.0,
                        rotation: [1.0, 0.0, 0.0, 0.0],
                        scale: [1.0, 1.0, 1.0],
                        detached: true,
                    }],
                    camera: CameraState {
                        x: 0.0,
                        y: 0.0,
                        z: 500.0,
                        yaw: 0.0,
                        pitch: 0.0,
                    },
                    layout_mode: "freeform".into(),
                    detached: vec![1],
                },
                WorkspaceEntry {
                    visuals: vec![VisualState {
                        app_id: "firefox".into(),
                        x: -200.0,
                        y: 50.0,
                        z: 100.0,
                        rotation: [1.0, 0.0, 0.0, 0.0],
                        scale: [1.0, 1.0, 1.0],
                        detached: false,
                    }],
                    camera: CameraState {
                        x: 100.0,
                        y: 0.0,
                        z: 600.0,
                        yaw: 0.5,
                        pitch: 0.2,
                    },
                    layout_mode: "flat".into(),
                    detached: vec![],
                },
            ],
        }
    }

    fn make_v1_state_json() -> String {
        serde_json::json!({
            "version": 1,
            "visuals": [
                {
                    "app_id": "foot",
                    "x": 100.0, "y": 200.0, "z": 0.0,
                    "rotation": [1.0, 0.0, 0.0, 0.0],
                    "scale": [1.0, 1.0, 1.0],
                    "detached": true
                }
            ],
            "camera": {
                "x": 0.0, "y": 0.0, "z": 500.0,
                "yaw": 0.0, "pitch": 0.0
            }
        })
        .to_string()
    }

    #[test]
    fn serialize_round_trip_v2() {
        let state = make_v2_state();
        let json = serde_json::to_string(&state).unwrap();
        let restored: WorkspaceState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.version, CURRENT_VERSION);
        assert_eq!(restored.workspaces.len(), 2);
        assert_eq!(restored.workspaces[0].visuals.len(), 1);
        assert_eq!(restored.workspaces[0].visuals[0].app_id, "foot");
        assert_eq!(restored.workspaces[1].visuals[0].app_id, "firefox");
        assert_eq!(restored.workspaces[1].layout_mode, "flat");
    }

    #[test]
    fn v1_migration() {
        let json = make_v1_state_json();
        let state: WorkspaceState = serde_json::from_str(&json).unwrap_or_else(|_| {
            // Should fail v2 parse, then we try v1
            let v1: WorkspaceStateV1 = serde_json::from_str(&json).unwrap();
            WorkspaceState {
                version: CURRENT_VERSION,
                workspaces: vec![WorkspaceEntry {
                    visuals: v1.visuals,
                    camera: v1.camera,
                    layout_mode: "freeform".into(),
                    detached: Vec::new(),
                }],
            }
        });
        assert_eq!(state.version, CURRENT_VERSION);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].visuals[0].app_id, "foot");
    }

    #[test]
    fn save_and_load_atomic() {
        let state = make_v2_state();
        assert!(save(&state).is_ok());
        assert!(exists());
        let loaded = load().unwrap();
        assert_eq!(loaded.workspaces.len(), 2);
        assert_eq!(loaded.workspaces[0].visuals[0].app_id, "foot");
        // Clean up
        remove();
    }

    #[test]
    fn camera_apply_round_trip() {
        let state = make_v2_state();
        let mut camera = Camera::new();
        state.apply_camera(&mut camera);
        assert_eq!(camera.position.z, 500.0);
    }

    #[test]
    fn find_visual_by_app_id() {
        let state = make_v2_state();
        let r = state.find_visual("foot");
        assert!(r.is_some());
        let (idx, vs) = r.unwrap();
        assert_eq!(idx, 0);
        assert_eq!(vs.x, 100.0);
        assert!(state.find_visual("nonexistent").is_none());
    }

    #[test]
    fn corrupt_file_recovery() {
        // Write corrupt data
        let path = state_path();
        fs::write(&path, "not valid json}{").unwrap();
        let result = load();
        assert!(result.is_err());
        remove();
    }

    #[test]
    fn v2_round_trip_all_fields() {
        let state = make_v2_state();
        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: WorkspaceState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.workspaces.len(), 2);
        // Check workspace 1
        let ws1 = &restored.workspaces[1];
        assert_eq!(ws1.layout_mode, "flat");
        assert_eq!(ws1.camera.yaw, 0.5);
        assert_eq!(ws1.visuals[0].app_id, "firefox");
    }

    #[test]
    fn layout_mode_round_trip() {
        // Test string conversion
        assert_eq!(string_to_layout_mode("freeform"), LayoutMode::Freeform);
        assert_eq!(string_to_layout_mode("flat"), LayoutMode::Flat);
        assert_eq!(
            string_to_layout_mode("grid:3"),
            LayoutMode::Grid { columns: 3 }
        );
        assert_eq!(string_to_layout_mode("unknown"), LayoutMode::Freeform);
    }

    /// R3: capture keys identity off chrome.app_id — the display title
    /// may change at any time and decoration.title was never populated
    /// (every visual used to be silently dropped from saved state).
    #[test]
    fn capture_uses_chrome_app_id_not_title() {
        let mut scene = crate::scene::Scene::default();
        let mut v = crate::scene::Visual::new(
            crate::scene::VisualContent::Test,
            smithay::utils::Rectangle::new(
                smithay::utils::Point::new(0, 0),
                smithay::utils::Size::new(320, 200),
            ),
        );
        v.chrome.app_id = "foot".into();
        v.chrome.title = "user renamed this window".into();
        scene.add(v);

        let state = WorkspaceState::capture_multi(
            &scene,
            &[vec![scene.visuals[0].id]],
            &[crate::input::Camera::new()],
            &[crate::layout::LayoutMode::Freeform],
            &[Vec::new()],
        );
        assert_eq!(
            state.workspaces[0].visuals.len(),
            1,
            "visual must be captured"
        );
        assert_eq!(state.workspaces[0].visuals[0].app_id, "foot");
    }

    /// R3: visuals without an app_id (producer content, recreated by
    /// the producers themselves) are not persisted.
    #[test]
    fn capture_skips_visuals_without_app_id() {
        let mut scene = crate::scene::Scene::default();
        let v = crate::scene::Visual::new(
            crate::scene::VisualContent::Test,
            smithay::utils::Rectangle::new(
                smithay::utils::Point::new(0, 0),
                smithay::utils::Size::new(320, 200),
            ),
        );
        scene.add(v);
        let state = WorkspaceState::capture_multi(
            &scene,
            &[vec![scene.visuals[0].id]],
            &[crate::input::Camera::new()],
            &[crate::layout::LayoutMode::Freeform],
            &[Vec::new()],
        );
        assert!(state.workspaces[0].visuals.is_empty());
    }

    /// R3: duplicate app_ids restore in capture order — the consuming
    /// take hands out the Nth saved transform to the Nth window.
    #[test]
    fn take_visual_consumes_duplicates_in_order() {
        let mut state = make_v2_state();
        // The same app saved in two workspaces (two instances).
        state.workspaces[0].visuals[0].x = 10.0;
        state.workspaces[1].visuals[0].app_id = "foot".into();
        state.workspaces[1].visuals[0].x = 20.0;

        let first = state.take_visual("foot").expect("first entry");
        let second = state.take_visual("foot").expect("second entry");
        let third = state.take_visual("foot");
        assert_eq!(first.0, 0, "first take comes from workspace 0");
        assert_eq!(first.1.x, 10.0);
        assert_eq!(second.0, 1, "second take comes from workspace 1");
        assert_eq!(second.1.x, 20.0);
        assert!(third.is_none(), "entries are consumed exactly once");
        assert!(
            state.find_visual("foot").is_none(),
            "find no longer matches consumed entries"
        );
    }
}
