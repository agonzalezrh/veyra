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
    /// G-E5.2 completion: the smithay protocol handle (the wl_output
    /// global clients bind). One per output — with multi-output, every
    /// output advertises its own mode/scale to the clients that bind
    /// it. The registry is the single source of truth; this is the
    /// protocol mirror, not a second source.
    pub wl: Option<smithay::output::Output>,
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

    /// G-E5.2 completion: attach the smithay protocol handle to the
    /// primary output (called once at construction when the wl_output
    /// global is created).
    pub fn set_primary_wl(&mut self, wl: smithay::output::Output) {
        if let Some(o) = self.primary_mut() {
            o.wl = Some(wl);
        }
    }

    /// G-E5.2 completion: the primary output's protocol handle. The
    /// handle is a cheap clone (Arc internally).
    pub fn primary_wl(&self) -> Option<smithay::output::Output> {
        self.primary().and_then(|o| o.wl.clone())
    }

    pub fn update_primary_scale(&mut self, scale: f64) {
        if let Some(o) = self.primary.and_then(|id| self.states.get_mut(&id)) {
            o.scale = scale;
        }
    }

    /// Total global desktop plane size (the union of the tiled row).
    pub fn global_extents(&self) -> (u32, u32) {
        // AABB of all output rects measured from the desktop origin —
        // respects actual placements (stacked outputs, mixed sizes),
        // not just the default row tiling. Negative-origin outputs
        // extend the plane leftward/upward but do not shrink the
        // origin-anchored extents.
        let w = self
            .states
            .values()
            .map(|o| o.global_pos.0.max(0) as u64 + o.mode.0 as u64)
            .max()
            .unwrap_or(0);
        let h = self
            .states
            .values()
            .map(|o| o.global_pos.1.max(0) as u64 + o.mode.1 as u64)
            .max()
            .unwrap_or(0);
        (w.min(u32::MAX as u64) as u32, h.min(u32::MAX as u64) as u32)
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
            wl: None,
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

    // ---- G-E5.4 remainder: synthetic multi-output regression battery.
    // These tests pin the INPUT geometry contract that multi-output
    // presentation will rely on: global-plane hit testing, per-event
    // output-local conversion, and the straddling-window rule (the hit
    // output is the POINTER's output, not the window-origin output).
    // Manual placement is applied by writing global_pos directly after
    // add() — retile() owns placement until a persistent position
    // source exists, and these tests never trigger it afterwards.

    /// Two outputs side by side with different resolutions:
    /// A(1920x1080) at (0,0), B(1280x720) at (1920,0).
    fn side_by_side() -> OutputManager {
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1280, 720));
        let ids = m.order.clone();
        m.states.get_mut(&ids[1]).unwrap().global_pos = (1920, 0);
        m
    }

    #[test]
    fn battery_hit_testing_mixed_resolutions() {
        let m = side_by_side();
        let a = m.order[0];
        let b = m.order[1];
        assert_eq!(m.output_at_global(0, 0), Some(a));
        assert_eq!(m.output_at_global(1919, 1079), Some(a));
        // Boundary: the first pixel of B belongs to B, the last pixel
        // of A to A (x = 1920 is exclusive on A's [0,1920) range).
        assert_eq!(m.output_at_global(1920, 0), Some(b));
        assert_eq!(m.output_at_global(1919, 0), Some(a));
        assert_eq!(m.output_at_global(3199, 719), Some(b));
        assert_eq!(m.output_at_global(3200, 0), None, "past the right edge");
        assert_eq!(m.output_at_global(100, 1080), None, "below A's bottom");
        assert_eq!(m.output_at_global(2000, 1080), None, "below B's bottom");
    }

    #[test]
    fn battery_to_local_converts_per_output() {
        let m = side_by_side();
        let b = m.order[1];
        // Global (2000, 100) → B-local (80, 100).
        assert_eq!(m.to_local(2000, 100), Some((b, 80, 100)));
        let a = m.order[0];
        assert_eq!(m.to_local(500, 900), Some((a, 500, 900)));
        assert_eq!(m.to_local(-1, 0), None, "negative global: outside");
    }

    #[test]
    fn battery_negative_origins() {
        let mut m = OutputManager::new();
        m.add(out("L", 1920, 1080));
        m.add(out("R", 1920, 1080));
        let ids = m.order.clone();
        // L sits to the LEFT of the origin: [-1920, 0).
        m.states.get_mut(&ids[0]).unwrap().global_pos = (-1920, 0);
        m.states.get_mut(&ids[1]).unwrap().global_pos = (0, 0);
        let l = ids[0];
        let r = ids[1];
        assert_eq!(m.output_at_global(-1920, 0), Some(l));
        assert_eq!(m.output_at_global(-1, 500), Some(l));
        assert_eq!(m.output_at_global(0, 0), Some(r));
        // Local conversion crosses the negative boundary correctly.
        assert_eq!(m.to_local(-1000, 400), Some((l, 920, 400)));
        assert_eq!(m.output_at_global(-1921, 0), None);
    }

    #[test]
    fn battery_outputs_stacked_vertically() {
        let mut m = OutputManager::new();
        m.add(out("top", 1920, 1080));
        m.add(out("bot", 2560, 1440));
        let ids = m.order.clone();
        m.states.get_mut(&ids[1]).unwrap().global_pos = (0, 1080);
        let t = ids[0];
        let b = ids[1];
        assert_eq!(m.output_at_global(100, 1079), Some(t));
        assert_eq!(m.output_at_global(100, 1080), Some(b));
        assert_eq!(m.to_local(1500, 2000), Some((b, 1500, 920)));
        assert_eq!(m.global_extents(), (2560, 2520));
    }

    #[test]
    fn battery_scale_does_not_shift_input_plane() {
        // Input geometry is the PHYSICAL plane: per-output scale affects
        // client surface rendering, never output hit testing or
        // output-local input coordinates.
        let mut m = side_by_side();
        let ids = m.order.clone();
        m.states.get_mut(&ids[1]).unwrap().scale = 2.0;
        let b = ids[1];
        assert_eq!(m.to_local(2000, 100), Some((b, 80, 100)));
        assert_eq!(m.output_at_global(2500, 500), Some(b));
    }

    #[test]
    fn battery_overlapping_outputs_prefer_primary() {
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1920, 1080));
        let ids = m.order.clone();
        // B fully overlaps A (mirrored clone). A is primary.
        m.states.get_mut(&ids[1]).unwrap().global_pos = (0, 0);
        let a = ids[0];
        assert_eq!(m.output_at_global(100, 100), Some(a), "primary wins overlap");
        // Primary B: now B wins.
        let mut m2 = OutputManager::new();
        m2.add(out("A", 1920, 1080));
        m2.add(out("B", 1920, 1080));
        let ids2 = m2.order.clone();
        m2.states.get_mut(&ids2[1]).unwrap().global_pos = (0, 0);
        m2.set_primary(ids2[1]);
        assert_eq!(m2.output_at_global(100, 100), Some(ids2[1]));
    }

    #[test]
    fn battery_straddling_window_hits_pointer_output() {
        // A window spans global x [1800, 2100] across the A|B boundary.
        // The same window must receive events in DIFFERENT output-local
        // spaces depending on where the pointer is: the hit output is
        // the pointer's output, NOT the window-origin output.
        let m = side_by_side();
        let a = m.order[0];
        let b = m.order[1];
        // Pointer over the window's left part (on A):
        assert_eq!(m.to_local(1850, 300), Some((a, 1850, 300)));
        // Pointer over the window's right part (on B) — the same
        // window, different coordinate space:
        assert_eq!(m.to_local(2050, 300), Some((b, 130, 300)));
        // And the event just OUTSIDE the window on B still hits B:
        assert_eq!(m.to_local(2200, 300), Some((b, 280, 300)));
    }
}
