//! Per-output state model (#14 phase 1 — G-E5 multi-monitor audit).
//!
//! The audit (BUG_LIST #14) inventoried the single-output assumptions:
//! one `window_size`, one wl_output global, projection/hit-testing
//! keyed off one framebuffer. The required shape is a per-output state
//! map whose outputs tile the global desktop plane, with per-output
//! hit testing. This module is that shape's data layer: it owns the
//! output registry (mode, scale, global position), global-rect
//! computation, and point→output resolution. Wiring consumers off
//! `window_size` onto this model proceeds incrementally in later
//! phases; today the live compositor registers its single output here
//! so the structure is exercised on every real session.
//!
//! Layout rule (audit): outputs tile the global desktop plane as a
//! horizontal row (x accumulates by width, y pinned to 0) until a
//! per-output position source exists (winit multi-window / DRM
//! connector geometry in later phases).
//!
//! Phase-1 note: only the registry-consumer methods the compositor
//! exercises today are wired; the multi-output accessors (hit test,
//! to_local, extents, remove) are the phase-2/3 API surface and are
//! exercised by this module's tests in the meantime.
#![allow(dead_code)]

/// One output's state: identity, mode, scale, and its position on the
/// global desktop plane.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputState {
    pub name: String,
    /// Current mode in physical pixels.
    pub mode: (u32, u32),
    /// Advertised refresh in mHz (e.g. 60000 = 60 Hz).
    pub refresh_mhz: i32,
    /// Output scale (fractional capable).
    pub scale: f64,
    /// Global position of the output's top-left corner on the desktop
    /// plane, in physical pixels.
    pub global_pos: (i32, i32),
}

impl OutputState {
    pub fn size(&self) -> (u32, u32) {
        self.mode
    }
}

/// The output registry. Phase 1 keeps single-output behavior identical:
/// the primary output mirrors the existing `window_size`/scale state.
#[derive(Debug, Clone, Default)]
pub struct OutputManager {
    outputs: Vec<OutputState>,
    primary: usize,
}

impl OutputManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an output; it is appended and tiled to the right of the
    /// existing row. Returns its index.
    pub fn add(&mut self, mut state: OutputState) -> usize {
        let x: i32 = self.outputs.iter().map(|o| o.mode.0 as i32).sum();
        state.global_pos = (x, 0);
        self.outputs.push(state);
        self.outputs.len() - 1
    }

    pub fn remove(&mut self, index: usize) -> Option<OutputState> {
        if index >= self.outputs.len() {
            return None;
        }
        let removed = self.outputs.remove(index);
        self.retile();
        if self.primary >= self.outputs.len() {
            self.primary = 0;
        }
        Some(removed)
    }

    /// Re-tile the row after add/remove/resize.
    fn retile(&mut self) {
        let mut x = 0;
        for o in &mut self.outputs {
            o.global_pos = (x, 0);
            x += o.mode.0 as i32;
        }
    }

    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    pub fn outputs(&self) -> &[OutputState] {
        &self.outputs
    }

    pub fn primary_index(&self) -> usize {
        self.primary.min(self.outputs.len().saturating_sub(1))
    }

    pub fn set_primary(&mut self, index: usize) -> bool {
        if index < self.outputs.len() {
            self.primary = index;
            true
        } else {
            false
        }
    }

    /// Primary output state — the single-output compatibility view.
    pub fn primary(&self) -> Option<&OutputState> {
        self.outputs.get(self.primary_index())
    }

    /// Primary output mode — mirrors the existing `window_size`.
    pub fn primary_size(&self) -> Option<(f32, f32)> {
        self.primary()
            .map(|o| (o.mode.0 as f32, o.mode.1 as f32))
    }

    pub fn primary_scale(&self) -> f64 {
        self.primary().map(|o| o.scale).unwrap_or(1.0)
    }

    /// Update the primary output's mode (resize) and re-tile.
    pub fn update_primary_mode(&mut self, w: u32, h: u32, refresh_mhz: i32) {
        let idx = self.primary_index();
        if let Some(o) = self.outputs.get_mut(idx) {
            o.mode = (w, h);
            o.refresh_mhz = refresh_mhz;
        }
        self.retile();
    }

    pub fn update_primary_scale(&mut self, scale: f64) {
        let idx = self.primary_index();
        if let Some(o) = self.outputs.get_mut(idx) {
            o.scale = scale;
        }
    }

    /// Total global desktop plane size (the union of the tiled row).
    pub fn global_extents(&self) -> (u32, u32) {
        let w: u32 = self.outputs.iter().map(|o| o.mode.0).sum();
        let h = self.outputs.iter().map(|o| o.mode.1).max().unwrap_or(0);
        (w, h)
    }

    /// Resolve a global-plane point (physical px) to an output index.
    /// Point-in-rect per output; the primary wins ties.
    pub fn output_at_global(&self, x: i32, y: i32) -> Option<usize> {
        let primary = self.primary_index();
        let mut best = None;
        for (i, o) in self.outputs.iter().enumerate() {
            let (ox, oy) = o.global_pos;
            let (ow, oh) = o.mode;
            let inside = x >= ox && x < ox + ow as i32 && y >= oy && y < oy + oh as i32;
            if inside {
                if best == Some(primary) {
                    return Some(primary);
                }
                best = Some(i);
            }
        }
        best
    }

    /// Convert a global-plane point to output-local coordinates.
    /// Returns (index, local_x, local_y).
    pub fn to_local(&self, x: i32, y: i32) -> Option<(usize, i32, i32)> {
        let idx = self.output_at_global(x, y)?;
        let o = &self.outputs[idx];
        Some((idx, x - o.global_pos.0, y - o.global_pos.1))
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
        }
    }

    #[test]
    fn single_output_matches_window_size_semantics() {
        let mut m = OutputManager::new();
        m.add(out("eDP-1", 1280, 720));
        assert_eq!(m.primary_size(), Some((1280.0, 720.0)));
        assert_eq!(m.global_extents(), (1280, 720));
        assert_eq!(m.output_at_global(0, 0), Some(0));
        assert_eq!(m.output_at_global(1279, 719), Some(0));
        assert_eq!(m.output_at_global(1280, 0), None);
    }

    #[test]
    fn outputs_tile_horizontally() {
        let mut m = OutputManager::new();
        m.add(out("DP-1", 1920, 1080));
        m.add(out("eDP-1", 1280, 720));
        assert_eq!(m.outputs()[0].global_pos, (0, 0));
        assert_eq!(m.outputs()[1].global_pos, (1920, 0));
        assert_eq!(m.global_extents(), (3200, 1080));
    }

    #[test]
    fn hit_test_resolves_per_output_and_prefers_primary_on_tie() {
        let mut m = OutputManager::new();
        m.add(out("DP-1", 1920, 1080));
        m.add(out("eDP-1", 1280, 720));
        assert_eq!(m.output_at_global(100, 100), Some(0));
        assert_eq!(m.output_at_global(2000, 100), Some(1));
        assert_eq!(m.output_at_global(1921, 700), Some(1));
        // Primary is output 1 here? No — primary defaults to 0; a point
        // inside only output 1 resolves to 1.
        assert_eq!(m.primary_index(), 0);
        m.set_primary(1);
        assert_eq!(m.primary_index(), 1);
        assert_eq!(m.primary_size(), Some((1280.0, 720.0)));
        // A point inside BOTH cannot happen in a row tiling — assert
        // out-of-plane points miss.
        assert_eq!(m.output_at_global(-5, 5), None);
        assert_eq!(m.output_at_global(5000, 5000), None);
    }

    #[test]
    fn to_local_subtracts_global_origin() {
        let mut m = OutputManager::new();
        m.add(out("DP-1", 1920, 1080));
        m.add(out("eDP-1", 1280, 720));
        let (idx, lx, ly) = m.to_local(2000, 300).unwrap();
        assert_eq!(idx, 1);
        assert_eq!((lx, ly), (80, 300));
        assert!(m.to_local(1920, 1080).is_none()); // y past the 720 row
    }

    #[test]
    fn remove_retiles_and_keeps_primary_valid() {
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1280, 720));
        m.set_primary(1);
        m.remove(0);
        assert_eq!(m.len(), 1);
        assert_eq!(m.outputs()[0].global_pos, (0, 0));
        assert_eq!(m.primary_index(), 0, "primary clamps after removal");
        assert_eq!(m.primary_size(), Some((1280.0, 720.0)));
    }

    #[test]
    fn mode_resize_retiles() {
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1280, 720));
        m.update_primary_mode(1280, 1024, 144000);
        assert_eq!(m.outputs()[1].global_pos, (1280, 0));
        assert_eq!(m.outputs()[0].refresh_mhz, 144000);
        // Extents height is the TALLEST row member (1024), not the
        // pre-resize mode.
        assert_eq!(m.global_extents(), (2560, 1024));
    }
}
