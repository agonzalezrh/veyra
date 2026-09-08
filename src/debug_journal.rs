//! Debug journal (e2e support): optional JSONL state log.
//!
//! Enabled with `VEYRA_DEBUG=<path>`. Every state transition the shell
//! and window management perform is appended as one JSON line, plus a
//! full `snapshot` record after interesting events. The file lets a
//! remote session reconstruct exactly what the user saw: window tables
//! (id, app, title, position, size, workspace, minimized/maximized/
//! fullscreen), focus, taskbar layout, camera.
//!
//! Failure to write is silently ignored — diagnostics must never take
//! the compositor down.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::scene::VisualId;

static JOURNAL: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Install the journal target from the environment. Called once at
/// startup.
pub fn init_from_env() {
    if let Ok(path) = std::env::var("VEYRA_DEBUG") {
        if !path.is_empty() {
            // Truncate stale content so a run maps 1:1 to a file.
            let _ = std::fs::write(&path, b"");
            if let Ok(mut guard) = JOURNAL.lock() {
                *guard = Some(PathBuf::from(path));
            }
        }
    }
}

pub fn enabled() -> bool {
    JOURNAL.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Append one event: `{"ev": "<kind>", ...fields}`.
pub fn event(kind: &str, fields: &[(&str, String)]) {
    let Ok(guard) = JOURNAL.lock() else { return };
    let Some(path) = guard.as_ref() else { return };
    let mut line = format!("{{\"ev\":\"{}\"", kind);
    for (k, v) in fields {
        // Values are pre-formatted (Debug of ids/vectors or quoted strings).
        line.push_str(&format!(",\"{}\":{}", k, v));
    }
    line.push_str("}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Convenience: quote a string for JSON.
pub fn s(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Convenience: Debug-format any value.
pub fn d<T: std::fmt::Debug>(v: T) -> String {
    format!("{:?}", v)
}

/// A compact per-window row for snapshots.
pub struct WindowRow {
    pub vid: VisualId,
    pub app_id: String,
    pub title: String,
    pub workspace: Option<usize>,
    pub pos: (f32, f32, f32),
    pub size: (f32, f32),
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

/// Full state snapshot (call after state-changing events).
pub fn snapshot(rows: &[WindowRow], focused: Option<VisualId>, active_ws: usize, camera_z: f32) {
    let windows: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{{\"vid\":{},\"app\":{},\"title\":{},\"ws\":{},\"pos\":[{:.1},{:.1},{:.1}],\"size\":[{:.1},{:.1}],\"focused\":{},\"min\":{},\"max\":{},\"fs\":{}}}",
                r.vid.0,
                s(&r.app_id),
                s(&r.title),
                r.workspace.map(|w| w.to_string()).unwrap_or_else(|| "null".into()),
                r.pos.0, r.pos.1, r.pos.2,
                r.size.0, r.size.1,
                r.focused, r.minimized, r.maximized, r.fullscreen,
            )
        })
        .collect();
    event(
        "snapshot",
        &[
            ("windows", format!("[{}]", windows.join(","))),
            ("focused", focused.map(|v| v.0.to_string()).unwrap_or_else(|| "null".into())),
            ("active_ws", active_ws.to_string()),
            ("camera_z", format!("{:.1}", camera_z)),
        ],
    );
}
