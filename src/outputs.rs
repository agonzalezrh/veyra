//! Per-output state model (#14 / G-E5 — multi-monitor architecture).
//!
//! The audit (BUG_LIST #14) inventoried the single-output assumptions:
//! one `window_size`, one wl_output global, projection/hit-testing
//! keyed off one framebuffer. The required shape is a per-output state
//! map whose outputs tile the global desktop plane, with per-output
//! hit testing. This module is that shape's data layer:
//!
//! - `OutputId` is the stable identity (never reused, survives mode
//!   changes and removal/re-add) — consumers key off it, never off
//!   slot indices.
//! - `OutputManager` is the `HashMap<OutputId, OutputState>` registry
//!   plus an insertion-ordered id list that defines the horizontal row
//!   tiling (x accumulates by width, y pinned to 0) until a per-output
//!   position source exists (winit multi-window / DRM connector
//!   geometry in a later phase).
//!
//! Phase-1 note: only the registry-consumer methods the compositor
//! exercises today are wired; the multi-output accessors (hit test,
//! to_local, extents, remove) are the phase-2/3 API surface and are
//! exercised by this module's tests in the meantime.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::input::Camera;

/// Stable output identity. Assigned from a monotonic counter — ids are
/// never recycled, so a removed and re-added output is a DIFFERENT
/// output (matching X11/Wayland semantics where a hotplugged monitor
/// is a new wl_output).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u64);

/// One output's state: identity, mode, scale, its position on the
/// global desktop plane, and its PRESENTATION VIEW.
///
/// G-E5.3: the camera is per-output. Workspace/world state (scene,
/// visual transforms, workspace membership) is shared; the view is
/// output-local. A workspace's SAVED camera (WorkspaceState.camera)
/// seeds an output's live view when that workspace activates.
#[derive(Debug, Clone)]
pub struct OutputState {
    /// Current mode in physical pixels.
    pub mode: (u32, u32),
    /// Advertised refresh in mHz (e.g. 60000 = 60 Hz).
    pub refresh_mhz: i32,
    /// Output scale (fractional capable).
    pub scale: f64,
    /// Global position of the output's top-left corner on the desktop
    /// plane, in physical pixels.
    pub global_pos: (i32, i32),
    /// wl_output name (e.g. "DP-1") — diagnostic/UX label.
    pub name: String,
    /// This output's live presentation view (G-E5.3).
    pub camera: Camera,
}

impl OutputState {
    pub fn size(&self) -> (u32, u32) {
        self.mode
    }
}

/// The output registry: `HashMap<OutputId, OutputState>` + insertion
/// order. Phase 1 keeps single-output behavior identical: the primary
/// output mirrors the existing `window_size`/scale state.
#[derive(Debug, Default)]
pub struct OutputManager {
    states: HashMap<OutputId, OutputState>,
    /// Insertion order — defines the row tiling and iteration order.
    order: Vec<OutputId>,
    primary: Option<OutputId>,
    next_id: u64,
}

impl OutputManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an output; it is appended to the row (tiled right of
    /// the existing outputs). Returns its stable id.
    pub fn add(&mut self, mut state: OutputState) -> OutputId {
        let id = OutputId(self.next_id);
        self.next_id += 1;
        let x: i32 = self
            .order
            .iter()
            .filter_map(|i| self.states.get(i))
            .map(|o| o.mode.0 as i32)
            .sum();
        state.global_pos = (x, 0);
        self.states.insert(id, state);
        self.order.push(id);
        if self.primary.is_none() {
            self.primary = Some(id);
        }
        id
    }

    pub fn remove(&mut self, id: OutputId) -> Option<OutputState> {
        let removed = self.states.remove(&id)?;
        self.order.retain(|i| *i != id);
        self.retile();
        if self.primary == Some(id) {
            self.primary = self.order.first().copied();
        }
        Some(removed)
    }

    /// Re-tile the row after add/remove/resize.
    fn retile(&mut self) {
        let mut x = 0;
        for id in &self.order {
            if let Some(o) = self.states.get_mut(id) {
                o.global_pos = (x, 0);
                x += o.mode.0 as i32;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Outputs in tiling (insertion) order.
    pub fn outputs(&self) -> Vec<(OutputId, &OutputState)> {
        self.order
            .iter()
            .filter_map(|id| self.states.get(id).map(|s| (*id, s)))
            .collect()
    }

    pub fn get(&self, id: OutputId) -> Option<&OutputState> {
        self.states.get(&id)
    }

    pub fn get_mut(&mut self, id: OutputId) -> Option<&mut OutputState> {
        self.states.get_mut(&id)
    }

    pub fn primary_id(&self) -> Option<OutputId> {
        self.primary
    }

    pub fn set_primary(&mut self, id: OutputId) -> bool {
        if self.states.contains_key(&id) {
            self.primary = Some(id);
            true
        } else {
            false
        }
    }

    /// Primary output state — the single-output compatibility view.
    pub fn primary(&self) -> Option<&OutputState> {
        self.primary.and_then(|id| self.states.get(&id))
    }

    /// Mutable primary output state.
    pub fn primary_mut(&mut self) -> Option<&mut OutputState> {
        self.primary.and_then(|id| self.states.get_mut(&id))
    }

    /// Update the primary output's wl_output name (first real sync).
    pub fn rename_primary(&mut self, name: String) {
        if let Some(o) = self.primary_mut() {
            o.name = name;
        }
    }

    /// Primary output mode — mirrors the existing `window_size`.
    pub fn primary_size(&self) -> Option<(f32, f32)> {
        self.primary().map(|o| (o.mode.0 as f32, o.mode.1 as f32))
    }

    pub fn primary_scale(&self) -> f64 {
        self.primary().map(|o| o.scale).unwrap_or(1.0)
    }

    /// Update the primary output's mode (resize) and re-tile.
    pub fn update_primary_mode(&mut self, w: u32, h: u32, refresh_mhz: i32) {
        if let Some(o) = self.primary.and_then(|id| self.states.get_mut(&id)) {
            o.mode = (w, h);
            o.refresh_mhz = refresh_mhz;
        }
        self.retile();
    }

    pub fn update_primary_scale(&mut self, scale: f64) {
        if let Some(o) = self.primary.and_then(|id| self.states.get_mut(&id)) {
            o.scale = scale;
        }
    }

    /// Total global desktop plane size (the union of the tiled row).
    pub fn global_extents(&self) -> (u32, u32) {
        let w: u32 = self.states.values().map(|o| o.mode.0).sum();
        let h = self.states.values().map(|o| o.mode.1).max().unwrap_or(0);
        (w, h)
    }

    /// Resolve a global-plane point (physical px) to an output id.
    /// Point-in-rect per output; the primary wins ties.
    pub fn output_at_global(&self, x: i32, y: i32) -> Option<OutputId> {
        let mut best = None;
        for id in &self.order {
            let o = &self.states[id];
            let (ox, oy) = o.global_pos;
            let (ow, oh) = o.mode;
            let inside = x >= ox && x < ox + ow as i32 && y >= oy && y < oy + oh as i32;
            if inside {
                if best == self.primary {
                    return self.primary;
                }
                best = Some(*id);
            }
        }
        best
    }

    /// Convert a global-plane point to output-local coordinates.
    /// Returns (id, local_x, local_y).
    pub fn to_local(&self, x: i32, y: i32) -> Option<(OutputId, i32, i32)> {
        let id = self.output_at_global(x, y)?;
        let o = &self.states[&id];
        Some((id, x - o.global_pos.0, y - o.global_pos.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, w: u32, h: u32) -> OutputState {
        OutputState {
            name: name.into(),
            mode: (w, h),
            refresh_mhz: 60000,
            scale: 1.0,
            global_pos: (0, 0),
            camera: Camera::new(),
        }
    }

    #[test]
    fn single_output_matches_window_size_semantics() {
        let mut m = OutputManager::new();
        m.add(out("eDP-1", 1280, 720));
        assert_eq!(m.primary_size(), Some((1280.0, 720.0)));
        assert_eq!(m.global_extents(), (1280, 720));
        assert_eq!(m.output_at_global(0, 0), Some(OutputId(0)));
        assert_eq!(m.output_at_global(1279, 719), Some(OutputId(0)));
        assert_eq!(m.output_at_global(1280, 0), None);
    }

    #[test]
    fn ids_are_stable_and_never_reused() {
        let mut m = OutputManager::new();
        let a = m.add(out("A", 1920, 1080));
        let b = m.add(out("B", 1280, 720));
        assert_ne!(a, b);
        m.remove(a);
        let c = m.add(out("C", 1920, 1080));
        assert_ne!(c, a, "removed id must not be recycled");
        assert_ne!(c, b);
    }

    #[test]
    fn outputs_tile_horizontally() {
        let mut m = OutputManager::new();
        m.add(out("DP-1", 1920, 1080));
        m.add(out("eDP-1", 1280, 720));
        let outs = m.outputs();
        assert_eq!(outs[0].1.global_pos, (0, 0));
        assert_eq!(outs[1].1.global_pos, (1920, 0));
        assert_eq!(m.global_extents(), (3200, 1080));
    }

    #[test]
    fn hit_test_resolves_per_output_and_prefers_primary_on_tie() {
        let mut m = OutputManager::new();
        let a = m.add(out("DP-1", 1920, 1080));
        let b = m.add(out("eDP-1", 1280, 720));
        assert_eq!(m.output_at_global(100, 100), Some(a));
        assert_eq!(m.output_at_global(2000, 100), Some(b));
        assert_eq!(m.primary_id(), Some(a));
        m.set_primary(b);
        assert_eq!(m.primary_id(), Some(b));
        assert_eq!(m.primary_size(), Some((1280.0, 720.0)));
        assert_eq!(m.output_at_global(-5, 5), None);
        assert_eq!(m.output_at_global(5000, 5000), None);
    }

    #[test]
    fn to_local_subtracts_global_origin() {
        let mut m = OutputManager::new();
        m.add(out("DP-1", 1920, 1080));
        m.add(out("eDP-1", 1280, 720));
        let (id, lx, ly) = m.to_local(2000, 300).unwrap();
        assert_eq!(id, OutputId(1));
        assert_eq!((lx, ly), (80, 300));
        assert!(m.to_local(1920, 1080).is_none()); // y past the 720 row
    }

    #[test]
    fn remove_retiles_and_rehomes_primary() {
        let mut m = OutputManager::new();
        let a = m.add(out("A", 1920, 1080));
        let b = m.add(out("B", 1280, 720));
        m.set_primary(b);
        m.remove(a);
        assert_eq!(m.len(), 1);
        assert_eq!(m.outputs()[0].1.global_pos, (0, 0));
        assert_eq!(
            m.primary_id(),
            Some(b),
            "unrelated primary survives removal"
        );
        m.remove(b);
        assert_eq!(m.primary_id(), None, "all removed → no primary");
        assert_eq!(m.primary_size(), None);
        assert_eq!(m.primary_scale(), 1.0);
    }

    #[test]
    fn mode_resize_retiles() {
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1280, 720));
        m.update_primary_mode(1280, 1024, 144000);
        assert_eq!(m.outputs()[1].1.global_pos, (1280, 0));
        assert_eq!(m.outputs()[0].1.refresh_mhz, 144000);
        // Extents height is the TALLEST row member (1024), not the
        // pre-resize mode.
        assert_eq!(m.global_extents(), (2560, 1024));
    }
}
