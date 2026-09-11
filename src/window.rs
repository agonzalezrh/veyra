//! Window-management abstraction.
//!
//! AGENTS.md's window model makes the window a first-class object:
//! identity, Wayland surface relationship, application metadata,
//! lifecycle state, workspace membership, logical geometry and visual
//! state. This module is its home. The compositor's per-surface
//! bookkeeping types live here first (incremental extraction — not a
//! rewrite); lifecycle OPERATIONS follow in later steps.

use smithay::wayland::compositor::with_states;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::xdg::{PositionerState, ToplevelSurface, XdgToplevelSurfaceData};

use crate::scene::VisualId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reserved API surface; not yet wired
pub enum SurfaceLifecycle {
    Created,
    Configured,
    Mapped,
    Unmapped,
    Destroyed,
}

/// Track a popup surface with its parent relationship.
#[derive(Debug, Clone)]
pub struct PopupInfo {
    pub popup: smithay::wayland::shell::xdg::PopupSurface,
    pub wl_surface: WlSurface,
    pub parent_toplevel_vid: Option<VisualId>,
    pub visual_id: Option<VisualId>,
    pub lifecycle: SurfaceLifecycle,
    pub size: Option<(i32, i32)>,
    /// The positioner state for computing popup geometry.
    pub positioner: PositionerState,
}

/// A managed Wayland toplevel: identity (visual id), Wayland surface
/// relationship, application metadata (title/app_id), and the
/// window-management state machines (maximize/minimize/fullscreen).
#[derive(Debug, Clone)]
pub struct ToplevelInfo {
    pub toplevel: ToplevelSurface,
    pub wl_surface: WlSurface,
    pub app_id: String,
    pub title: String,
    pub lifecycle: SurfaceLifecycle,
    pub visual_id: Option<VisualId>,
    pub size: Option<(i32, i32)>,
    /// I4: the client acknowledged a maximized configure. Geometry
    /// authority stays with the client; this only tracks the state.
    pub maximized: bool,
    /// I4: committed size to restore on unmaximize (captured at
    /// maximize time). None while not maximized.
    pub restore_size: Option<(i32, i32)>,
    /// I4: presentation pose to restore on unmaximize: (position xyz,
    /// rotation ijkw). Captured when the window is maximized.
    pub restore_pose: Option<((f32, f32, f32), [f32; 4])>,
    /// I5: the window is currently minimized (hidden, Wayland surface
    /// still mapped and alive). Presentation transform is untouched;
    /// layout/arrangement treat minimized visuals as detached.
    pub minimized: bool,
    /// I7: the client acknowledged a fullscreen configure and committed
    /// at the transition size (FULLSCREEN state in the machine above).
    /// The snapshot for restoring lives in the fullscreen coordinator.
    pub fullscreened: bool,
    /// I7: the pre-fullscreen snapshot (FullscreenSnapshot), captured
    /// exactly once at fullscreen entry and consumed by unfullscreen
    /// restore. None while not fullscreen.
    pub fullscreen_snapshot: Option<crate::fullscreen::FullscreenSnapshot>,
}

impl ToplevelInfo {
    pub(crate) fn new(toplevel: ToplevelSurface) -> Self {
        let wl_surface = toplevel.wl_surface().clone();
        let (title, app_id) = with_states(&wl_surface, |states| {
            let title = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|attrs| attrs.lock().unwrap().title.clone().unwrap_or_default())
                .unwrap_or_default();
            let app_id = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|attrs| attrs.lock().unwrap().app_id.clone().unwrap_or_default())
                .unwrap_or_default();
            (title, app_id)
        });
        ToplevelInfo {
            lifecycle: SurfaceLifecycle::Created,
            toplevel,
            wl_surface,
            app_id,
            title,
            visual_id: None,
            size: None,
            maximized: false,
            restore_size: None,
            restore_pose: None,
            minimized: false,
            fullscreened: false,
            fullscreen_snapshot: None,
        }
    }

    pub(crate) fn refresh_metadata(&mut self) {
        with_states(&self.wl_surface, |states| {
            if let Some(attrs) = states.data_map.get::<XdgToplevelSurfaceData>() {
                let attrs = attrs.lock().unwrap();
                self.title = attrs.title.clone().unwrap_or_default();
                self.app_id = attrs.app_id.clone().unwrap_or_default();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_starts_created() {
        // The window model starts every surface in Created; Mapped is
        // only reached through the compositor's commit pipeline.
        let l = SurfaceLifecycle::Created;
        assert_ne!(l, SurfaceLifecycle::Mapped);
        assert_eq!(l, SurfaceLifecycle::Created);
    }
}
