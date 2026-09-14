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

/// G-E5.5.1: one output's rectangle inside the simulation framebuffer.
/// The mapping global-rect → viewport is explicit and reusable by both
/// rendering and tests — never derived ad hoc in the render path.
///
/// Hierarchy: simulation framebuffer → OutputViewport[] → OutputState[]
/// → per-output camera/projection. `window_size` remains a property of
/// the underlying Winit framebuffer, not the desktop geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputViewport {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl OutputViewport {
    /// Viewport for an output: its global rect translated by the
    /// desktop origin (the min corner across all outputs), so
    /// negative-origin outputs land inside the simulation framebuffer.
    pub fn from_state(state: &OutputState, desktop_origin: (i32, i32)) -> Self {
        OutputViewport {
            x: state.global_pos.0 - desktop_origin.0,
            y: state.global_pos.1 - desktop_origin.1,
            width: state.mode.0,
            height: state.mode.1,
        }
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && px < self.x + self.width as i32
            && py >= self.y
            && py < self.y + self.height as i32
    }
}

/// G-E5.5.2: one output's plan for the current frame — identity, where
/// it renders inside the simulation framebuffer, and ITS view of the
/// SHARED scene. Built by `build_frame_plans` (pure, testable);
/// render_scene consumes plans and must not derive geometry itself.
#[derive(Debug, Clone)]
pub struct OutputFramePlan {
    pub output_id: OutputId,
    pub viewport: OutputViewport,
    /// The output's own view of the shared scene.
    pub view: cgmath::Matrix4<f32>,
    /// Projection from the output's OWN viewport size — never the
    /// underlying framebuffer size.
    pub proj: cgmath::Matrix4<f32>,
}

/// G-E5.5.5: per-output presentation bookkeeping for one presented
/// frame. The simulated swap is global (one swap_buffers presents all
/// views), but the bookkeeping already understands that a frame
/// contains N output views.
#[derive(Debug, Clone)]
pub struct OutputFrameReport {
    pub output_id: OutputId,
    pub viewport: OutputViewport,
    pub presented: bool,
}

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

    /// G-E5.6.6: hotplug unplug — the logical output LEAVES the
    /// desktop plane (registry, row tiling, camera access) while
    /// everything that is not presentation state survives untouched:
    /// scene, workspaces, windows, other outputs' cameras. The caller
    /// applies the returned effect to the bindings (detach + compact)
    /// and drives the backend's Draining → Removed presentation
    /// teardown separately.
    pub fn unplug_output(&mut self, id: OutputId) -> Option<HotplugEffect> {
        let was_primary = self.primary == Some(id);
        self.remove(id)?;
        let promoted = if was_primary {
            self.primary
        } else {
            None
        };
        Some(HotplugEffect {
            unplugged: id,
            was_primary,
            promoted,
        })
    }

    /// G-E5.6.6: hotplug replug — a NEW output (ids are never
    /// recycled). The caller presents it at the end of the row.
    pub fn replug_output(&mut self, mut state: OutputState) -> OutputId {
        state.camera = Camera::new();
        self.add(state)
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

    /// G-E5.5.3: the desktop plane's min corner across all outputs.
    /// (0,0) for the default row tiling; negative when an output sits
    /// left of / above the origin. The identity input↔render mapping
    /// relies on this being the SINGLE offset between the global
    /// desktop plane and the simulation framebuffer.
    pub fn desktop_origin(&self) -> (i32, i32) {
        let mut it = self.states.values();
        let Some(first) = it.next() else {
            return (0, 0);
        };
        let mut min_x = first.global_pos.0;
        let mut min_y = first.global_pos.1;
        for o in it {
            min_x = min_x.min(o.global_pos.0);
            min_y = min_y.min(o.global_pos.1);
        }
        (min_x, min_y)
    }

    /// G-E5.5.3: simulation framebuffer pixel → global desktop plane.
    /// Exact inverse of `global_to_fb`. With the default row tiling
    /// (origin at (0,0)) this is the identity — today's behavior.
    pub fn fb_to_global(&self, x: i32, y: i32) -> (i32, i32) {
        let (ox, oy) = self.desktop_origin();
        (x + ox, y + oy)
    }

    /// G-E5.5.3: global desktop plane → simulation framebuffer pixel.
    pub fn global_to_fb(&self, x: i32, y: i32) -> (i32, i32) {
        let (ox, oy) = self.desktop_origin();
        (x - ox, y - oy)
    }

    /// G-E5.5.2: build one OutputFramePlan per output, in registry
    /// order. Pure so tests can assert the mapping without a backend.
    /// `projector` produces the per-output projection from the
    /// output's OWN viewport size (the compositor supplies its
    /// spatial/ortho policy).
    pub fn build_frame_plans(
        &self,
        projector: &dyn Fn(bool, f32, f32) -> cgmath::Matrix4<f32>,
        spatial_mode: bool,
    ) -> Vec<OutputFramePlan> {
        let origin = self.desktop_origin();
        self.order
            .iter()
            .filter_map(|id| {
                let state = self.states.get(id)?;
                let viewport = OutputViewport::from_state(state, origin);
                // Normal mode: the ortho projection maps the output's
                // OWN pixel space, but the scene lives on the GLOBAL
                // desktop plane — the view carries the output's global
                // offset so each output renders ITS slice of the shared
                // desktop (a window at global (440,210) is on output A,
                // not on B). Spatial mode: one shared 3D space, every
                // output's camera is just a different viewpoint — no
                // translation.
                let view = if spatial_mode {
                    state.camera.view_matrix()
                } else {
                    state.camera.view_matrix()
                        * cgmath::Matrix4::from_translation(cgmath::Vector3::new(
                            -(state.global_pos.0 - origin.0) as f32,
                            -(state.global_pos.1 - origin.1) as f32,
                            0.0,
                        ))
                };
                Some(OutputFramePlan {
                    output_id: *id,
                    viewport,
                    view,
                    proj: projector(
                        spatial_mode,
                        viewport.width as f32,
                        viewport.height as f32,
                    ),
                })
            })
            .collect()
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

/// G-E5.5: the simulated multi-output layout spec — shared by the
/// registry seeder (LookingGlass::new) and the nested window sizing
/// (main.rs), so the simulation framebuffer and the underlying winit
/// surface can never disagree. Output 0 is the primary (1280x720);
/// outputs 1..n are 1024x768 with a 64 px gap, row-tiled.
/// One simulated output's spec: (name, physical mode, global position).
pub type SimulatedOutputSpec = (String, (u32, u32), (i32, i32));

/// One adopted native output's spec: (name, mode, refresh mHz, global position).
pub type NativeOutputSpec = (String, (u32, u32), i32, (i32, i32));

pub fn simulated_layout(n: u32) -> Vec<SimulatedOutputSpec> {
    let mut v = vec![("default".to_string(), (1280u32, 720u32), (0i32, 0i32))];
    for i in 1..n {
        v.push((
            format!("SIM-{i}"),
            (1024, 768),
            ((1280 + 64) * i as i32, 0),
        ));
    }
    v
}

/// Simulation framebuffer extents for N simulated outputs.
pub fn simulated_extents(n: u32) -> (u32, u32) {
    let mut w = 0u32;
    let mut h = 0u32;
    for (_, (mw, mh), (x, y)) in simulated_layout(n) {
        w = w.max(x as u32 + mw);
        h = h.max(y as u32 + mh);
    }
    (w, h)
}

/// G-E5.6.5: the explicit OutputId → backend-output-index binding.
///
/// The DRM backend stores its presentation states in a Vec; the
/// compositor addresses outputs by OutputId. Positional assumptions
/// (`outputs[id]`, `outputs[position]`) are exactly the class of bug
/// the E5.6.4 attribution tests exposed — this mapping is the ONLY
/// link between the two worlds, and it survives reordering, removal,
/// and non-contiguous ids.
#[derive(Debug, Default, Clone)]
pub struct OutputBindings {
    map: std::collections::HashMap<OutputId, usize>,
}

impl OutputBindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind an output to its backend index.
    pub fn bind(&mut self, id: OutputId, backend_index: usize) {
        self.map.insert(id, backend_index);
    }

    /// The backend index presenting this output, if bound.
    pub fn index_for(&self, id: OutputId) -> Option<usize> {
        self.map.get(&id).copied()
    }

    /// Unbind (hotplug removal). The OutputId stays valid in the
    /// registry if windows remain on it — only the PRESENTATION link
    /// is severed.
    pub fn unbind(&mut self, id: OutputId) -> bool {
        self.map.remove(&id).is_some()
    }

    /// A backend output was removed: detach the binding that pointed
    /// at it and compact the indices of everything past it. Returns
    /// the affected OutputIds (their presentation is gone).
    pub fn backend_removed(&mut self, removed_index: usize) -> Vec<OutputId> {
        let mut affected = Vec::new();
        for (id, idx) in self.map.iter_mut() {
            if *idx == removed_index {
                affected.push(*id);
                *idx = usize::MAX; // detached — presentation gone
            } else if *idx > removed_index {
                *idx -= 1;
            }
        }
        affected
    }

    /// Bind an output's presentation again after a backend-side index
    /// change (replug).
    pub fn rebind(&mut self, id: OutputId, backend_index: usize) -> bool {
        match self.map.get_mut(&id) {
            Some(slot) => {
                *slot = backend_index;
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// G-E5.6.6: what an unplugged output leaves behind — the effect a
/// caller must apply (bindings detach, primary may move).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotplugEffect {
    /// The unplugged output's identity. Ids are NEVER recycled: a
    /// re-added connector is a NEW output (Wayland/X11 semantics).
    pub unplugged: OutputId,
    /// True when the unplugged output was primary.
    pub was_primary: bool,
    /// The promoted primary, if reassignment happened.
    pub promoted: Option<OutputId>,
}

/// G-E5.6.6: one output's lifecycle. Logical removal (the registry
/// forgets an output) and physical presentation teardown (GBM surfaces,
/// pending flips, buffer caches) are SEPARATE states — a monitor
/// disappearing must never imply the destruction of the windows that
/// were visible on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLifecycle {
    /// Connector gone (or never seen). Not presented.
    Disconnected,
    /// Topology assigned the connector to a CRTC; no presentation yet.
    Discovered,
    /// Presentation bound; frames flowing.
    Active,
    /// Presentation detaching: the pending flip completes or is
    /// cancelled (force_idle) BEFORE resources are destroyed.
    Draining,
    /// Fully torn down. The registry entry may persist if windows
    /// remain on the (now unbound) logical output.
    Removed,
}

impl OutputLifecycle {
    /// A connector appeared / was assigned.
    pub fn discover(self) -> Result<Self, &'static str> {
        match self {
            OutputLifecycle::Disconnected => Ok(OutputLifecycle::Discovered),
            _ => Err("discover requires Disconnected"),
        }
    }

    /// Presentation bound.
    pub fn activate(self) -> Result<Self, &'static str> {
        match self {
            OutputLifecycle::Discovered => Ok(OutputLifecycle::Active),
            _ => Err("activate requires Discovered"),
        }
    }

    /// Unplug: presentation starts detaching. Resources are NOT freed
    /// yet — the pending flip (if any) must resolve first.
    pub fn begin_drain(self) -> Result<Self, &'static str> {
        match self {
            OutputLifecycle::Active => Ok(OutputLifecycle::Draining),
            _ => Err("drain requires Active"),
        }
    }

    /// Teardown completes. Only legal once the frame state is quiesced.
    pub fn finish_drain(self) -> Result<Self, &'static str> {
        match self {
            OutputLifecycle::Draining => Ok(OutputLifecycle::Removed),
            _ => Err("finish_drain requires Draining"),
        }
    }

    pub fn is_presented(self) -> bool {
        matches!(self, OutputLifecycle::Active | OutputLifecycle::Draining)
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

    // ---- G-E5.5: simulated multi-output viewport mapping.
    // Contract: simulation framebuffer → OutputViewport[] →
    // OutputState[] → per-output camera/projection. The fb↔global
    // conversions are exact inverses; viewports derive from global
    // rects via the desktop origin (never ad hoc in the render path).

    #[test]
    fn viewport_mapping_identity_for_row_tiling() {
        let m = side_by_side();
        assert_eq!(m.desktop_origin(), (0, 0));
        // Identity round trip on the default row tiling.
        for (gx, gy) in [(0, 0), (1919, 1079), (1920, 0), (3199, 719)] {
            let (fx, fy) = m.global_to_fb(gx, gy);
            assert_eq!((fx, fy), (gx, gy));
            assert_eq!(m.fb_to_global(fx, fy), (gx, gy));
        }
        // Viewports tile the framebuffer exactly.
        let plans = m.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].viewport, OutputViewport { x: 0, y: 0, width: 1920, height: 1080 });
        assert_eq!(plans[1].viewport, OutputViewport { x: 1920, y: 0, width: 1280, height: 720 });
    }

    #[test]
    fn viewport_mapping_negative_origin_translates() {
        let mut m = OutputManager::new();
        m.add(out("L", 1920, 1080));
        m.add(out("R", 1920, 1080));
        let ids = m.order.clone();
        // L occupies [-1920, 0), R occupies [0, 1920) — adjacent, with
        // the desktop origin at L's top-left.
        m.states.get_mut(&ids[0]).unwrap().global_pos = (-1920, 0);
        m.states.get_mut(&ids[1]).unwrap().global_pos = (0, 0);
        // Desktop origin is L's top-left; the framebuffer starts there.
        assert_eq!(m.desktop_origin(), (-1920, 0));
        let plans = m.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        assert_eq!(plans[0].viewport, OutputViewport { x: 0, y: 0, width: 1920, height: 1080 });
        assert_eq!(plans[1].viewport, OutputViewport { x: 1920, y: 0, width: 1920, height: 1080 });
        // fb(0,0) IS global (-1920,0) — and the round trip closes.
        assert_eq!(m.fb_to_global(0, 0), (-1920, 0));
        assert_eq!(m.global_to_fb(-1920, 0), (0, 0));
        assert_eq!(m.global_to_fb(-1000, 400), (920, 400));
        assert_eq!(m.fb_to_global(920, 400), (-1000, 400));
    }

    #[test]
    fn viewport_mapping_stacked_translates_y() {
        let mut m = OutputManager::new();
        m.add(out("top", 1920, 1080));
        m.add(out("bot", 2560, 1440));
        let ids = m.order.clone();
        m.states.get_mut(&ids[1]).unwrap().global_pos = (0, 1080);
        let plans = m.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        assert_eq!(plans[0].viewport, OutputViewport { x: 0, y: 0, width: 1920, height: 1080 });
        assert_eq!(plans[1].viewport, OutputViewport { x: 0, y: 1080, width: 2560, height: 1440 });
        // Framebuffer is the AABB: 2560 wide (bottom is wider), 2520 tall.
        assert_eq!(m.global_extents(), (2560, 2520));
        assert_eq!(m.fb_to_global(100, 1200), (100, 1200));
    }

    #[test]
    fn viewport_plans_use_output_camera_and_size() {
        // THE core E5.5 property: one scene, N views. Each plan carries
        // ITS output's camera and ITS viewport-size projection — the
        // same builder input produces different view matrices when the
        // cameras differ.
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1280, 720));
        let ids = m.order.clone();
        m.states.get_mut(&ids[1]).unwrap().global_pos = (1920, 0);
        // Rotate camera B 90° about Y (camera A stays default).
        m.states.get_mut(&ids[1]).unwrap().camera.yaw = 1.5707964; // ~90°
        let plans = m.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        assert_ne!(plans[0].view, plans[1].view, "different cameras → different views");
        // Projections derive from each output's OWN size.
        let pa = plans[0].proj;
        let pb = plans[1].proj;
        assert_ne!(pa, pb);
        // And the projection scale matches the viewport (marker matrix
        // is from_nonuniform_scale(w, h, 1)).
        assert_eq!(pa.x.x, 1920.0);
        assert_eq!(pb.x.x, 1280.0);
    }

    #[test]
    fn viewport_removal_drops_plan_and_remaps() {
        let mut m = OutputManager::new();
        m.add(out("A", 1920, 1080));
        m.add(out("B", 1280, 720));
        let ids = m.order.clone();
        m.states.get_mut(&ids[1]).unwrap().global_pos = (1920, 0);
        m.remove(ids[0]);
        // B remains. remove() re-tiles the row (placement policy), so
        // B lands at the row start and the simulation framebuffer
        // shrinks to exactly B's viewport.
        let plans = m.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].output_id, ids[1]);
        assert_eq!(plans[0].viewport, OutputViewport { x: 0, y: 0, width: 1280, height: 720 });
        assert_eq!(m.desktop_origin(), (0, 0));
    }

    #[test]
    fn viewport_identity_is_registry_order_independent() {
        // Rendering order may vary; logical identity (OutputId) and
        // geometry must not.
        let mut m1 = OutputManager::new();
        let a1 = m1.add(out("A", 1920, 1080));
        let b1 = m1.add(out("B", 1280, 720));
        m1.states.get_mut(&b1).unwrap().global_pos = (1920, 0);
        let mut m2 = OutputManager::new();
        let b2 = m2.add(out("B", 1280, 720));
        let a2 = m2.add(out("A", 1920, 1080));
        m2.states.get_mut(&a2).unwrap().global_pos = (0, 0);
        m2.states.get_mut(&b2).unwrap().global_pos = (1920, 0);
        m2.set_primary(a2);
        let plans1 = m1.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        let plans2 = m2.build_frame_plans(&|_, w, h| {
            cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0)
        }, false);
        let by_id = |plans: &[OutputFramePlan], id: OutputId| {
            plans.iter().find(|p| p.output_id == id).unwrap().viewport
        };
        assert_eq!(by_id(&plans1, a1), by_id(&plans2, a2));
        assert_eq!(by_id(&plans1, b1), by_id(&plans2, b2));
    }

    // ---- G-E5.6.5: OutputId ↔ backend index binding + KMS geometry
    // agreement with the E5.5 mapping (the simulation is the oracle).

    #[test]
    fn bindings_map_ids_to_indices() {
        let mut b = OutputBindings::new();
        b.bind(OutputId(100), 0);
        b.bind(OutputId(207), 1);
        // Non-contiguous OutputIds, positional backend indices.
        assert_eq!(b.index_for(OutputId(100)), Some(0));
        assert_eq!(b.index_for(OutputId(207)), Some(1));
        assert_eq!(b.index_for(OutputId(999)), None, "unknown OutputId");
    }

    #[test]
    fn bindings_survive_reordering() {
        // The same ids bound to SWAPPED backend indices — the binding
        // must follow whatever the real device reports, not the
        // registry order.
        let mut b = OutputBindings::new();
        b.bind(OutputId(100), 1);
        b.bind(OutputId(207), 0);
        assert_eq!(b.index_for(OutputId(100)), Some(1));
        assert_eq!(b.index_for(OutputId(207)), Some(0));
    }

    #[test]
    fn bindings_compact_on_backend_removal() {
        let mut b = OutputBindings::new();
        b.bind(OutputId(100), 0);
        b.bind(OutputId(207), 1);
        b.bind(OutputId(305), 2);
        // Backend output 1 (B) unplugged.
        let affected = b.backend_removed(1);
        assert_eq!(affected, vec![OutputId(207)]);
        assert_eq!(b.index_for(OutputId(207)), Some(usize::MAX), "detached");
        assert_eq!(b.index_for(OutputId(305)), Some(1), "compacted down");
        assert_eq!(b.index_for(OutputId(100)), Some(0), "untouched");
    }

    #[test]
    fn bindings_unbind_is_explicit() {
        let mut b = OutputBindings::new();
        b.bind(OutputId(100), 0);
        assert!(b.unbind(OutputId(100)));
        assert!(!b.unbind(OutputId(100)), "already gone");
        assert_eq!(b.index_for(OutputId(100)), None);
    }

    #[test]
    fn drm_assignment_geometry_agrees_with_e55_oracle() {
        // The bridge contract: a DRM topology assignment turned into
        // OutputStates (positions from global_positions — the E5.5
        // row tiling) must produce frame plans whose viewports tile
        // the global desktop exactly like the nested simulation does.
        use crate::drm_topology::{
            global_positions, AssignedOutput, ConnectorState, DrmTopology, TopologyConnector,
            TopologyCrtc, TopologyMode,
        };
        let mut t = DrmTopology::new();
        t.add_connector(TopologyConnector {
            id: 31,
            state: ConnectorState::Connected,
            modes: vec![TopologyMode { width: 1920, height: 1080, refresh_mhz: 60000, preferred: true }],
            encoder_candidates: vec![10],
        });
        t.add_connector(TopologyConnector {
            id: 34,
            state: ConnectorState::Connected,
            modes: vec![TopologyMode { width: 2560, height: 1440, refresh_mhz: 144000, preferred: true }],
            encoder_candidates: vec![11],
        });
        t.add_crtc(TopologyCrtc { id: 50, encoder_candidates: vec![10] });
        t.add_crtc(TopologyCrtc { id: 51, encoder_candidates: vec![11] });
        let assignment = t.assign();
        assert_eq!(assignment.outputs.len(), 2);

        // 1. The compositor registers one OutputState per assigned
        //    output (positions from the SAME global_positions the
        //    simulation uses).
        let positions = global_positions(&assignment.outputs);
        let mut m = OutputManager::new();
        for ((conn_id, pos), assigned) in positions.iter().zip(&assignment.outputs) {
            m.add(OutputState {
                name: format!("DP-{conn_id}"),
                mode: (assigned.mode.width, assigned.mode.height),
                refresh_mhz: assigned.mode.refresh_mhz,
                scale: 1.0,
                global_pos: *pos,
                camera: Camera::new(),
                wl: None,
            });
        }
        // 2. Frame plans through the E5.5 mapping.
        let plans = m.build_frame_plans(
            &|_, w, h| cgmath::Matrix4::from_nonuniform_scale(w, h, 1.0),
            false,
        );
        assert_eq!(plans.len(), 2);
        assert_eq!(
            plans[0].viewport,
            OutputViewport { x: 0, y: 0, width: 1920, height: 1080 }
        );
        assert_eq!(
            plans[1].viewport,
            OutputViewport { x: 1920, y: 0, width: 2560, height: 1440 }
        );
        // 3. The bindings link each output to its backend index
        //    (assignment order = backend vec order).
        let mut bindings = OutputBindings::new();
        for (i, assigned) in assignment.outputs.iter().enumerate() {
            let oid = m
                .outputs()
                .iter()
                .find(|(_, s)| s.name == format!("DP-{}", assigned.connector_id))
                .map(|(id, _)| *id)
                .unwrap();
            bindings.bind(oid, i);
        }
        assert_eq!(bindings.index_for(plans[0].output_id), Some(0));
        assert_eq!(bindings.index_for(plans[1].output_id), Some(1));
    }

    // ---- G-E5.6.6: hotplug lifecycle — logical removal ≠ physical
    // teardown. THE invariant: a physical output disappearing never
    // implies the destruction of the windows visible on it.

    fn two_outputs() -> (OutputManager, OutputId, OutputId) {
        let mut m = OutputManager::new();
        let a = m.add(out("A", 1920, 1080));
        let b = m.add(out("B", 1280, 720));
        (m, a, b)
    }

    #[test]
    fn lifecycle_transitions_are_ordered() {
        use OutputLifecycle::*;
        assert_eq!(Disconnected.discover(), Ok(Discovered));
        assert_eq!(Discovered.activate(), Ok(Active));
        assert_eq!(Active.begin_drain(), Ok(Draining));
        assert_eq!(Draining.finish_drain(), Ok(Removed));
        // Illegal jumps.
        assert!(Disconnected.activate().is_err());
        assert!(Discovered.begin_drain().is_err());
        assert!(Active.finish_drain().is_err());
        assert!(Removed.discover().is_err());
    }

    #[test]
    fn draining_outputs_are_still_presented_until_quiesced() {
        use OutputLifecycle::*;
        assert!(Active.is_presented());
        assert!(Draining.is_presented(), "pending flip still owns buffers");
        assert!(!Removed.is_presented());
    }

    #[test]
    fn unplug_secondary_primary_continues() {
        let (mut m, a, b) = two_outputs();
        let eff = m.unplug_output(b).expect("unplug");
        assert!(!eff.was_primary);
        assert_eq!(eff.promoted, None);
        assert_eq!(m.primary_id(), Some(a), "A continues rendering");
        assert_eq!(m.outputs().len(), 1);
    }

    #[test]
    fn unplug_primary_promotes_successor() {
        let (mut m, _a, b) = two_outputs();
        let eff = m.unplug_output(_a).expect("unplug primary");
        assert!(eff.was_primary);
        assert_eq!(eff.promoted, Some(b), "B becomes primary");
        assert_eq!(m.primary_id(), Some(b));
    }

    #[test]
    fn unplug_does_not_touch_scene_or_workspaces() {
        // The single most important lifecycle invariant, at the level
        // testable here: the registry's unplug path never receives —
        // let alone mutates — scene/workspace state. The visual_ids
        // live on WorkspaceState, keyed by WORKSPACE, not by output.
        // This assertion pins the structural fact that OutputManager
        // holds no window references to destroy.
        let (mut m, a, _b) = two_outputs();
        m.unplug_output(a).expect("unplug");
        // Nothing to assert on windows BECAUSE there is no window field
        // on OutputManager — the type system enforces the invariant.
        assert_eq!(m.outputs().len(), 1);
    }

    #[test]
    fn pending_flip_resolution_is_a_draining_concern() {
        // A pending flip (E5.6.3 FlipPending) must resolve BEFORE the
        // presentation is Removed — force_idle cancels it, then
        // finish_drain is legal. The ordering is the lifecycle's job.
        use crate::drm_backend::OutputFrameState;
        use OutputLifecycle::*;
        let frame = OutputFrameState::FlipPending;
        // Presentation teardown starts (Draining) while the flip pends:
        let lc = Active.begin_drain().expect("drain");
        assert!(lc.is_presented());
        // The frame state quiesces (force_idle — no event will arrive).
        let frame = frame.force_idle();
        assert_eq!(frame, OutputFrameState::Idle);
        // NOW teardown may complete.
        assert_eq!(lc.finish_drain(), Ok(Removed));
    }

    #[test]
    fn replug_creates_a_new_identity() {
        let (mut m, a, b) = two_outputs();
        let _ = a;
        m.unplug_output(b).expect("unplug");
        // Reconnect B's connector: a NEW OutputId (ids never recycled —
        // matching wl_output/X11 hotplug semantics).
        let b2 = m.replug_output(out("B", 1280, 720));
        assert_ne!(b2, b, "re-added output is a new identity");
        assert_eq!(m.outputs().len(), 2);
        assert_ne!(m.primary_id(), Some(b));
    }

    #[test]
    fn hotplug_binding_detach_and_compact() {
        // The full disconnect dance at the binding level: A(idx0)+B(idx1)
        // active → B unplugged → B's binding detached, A untouched →
        // reconnect: B2 binds at the next index.
        let (mut m, a, b) = two_outputs();
        let mut bindings = OutputBindings::new();
        bindings.bind(a, 0);
        bindings.bind(b, 1);
        let eff = m.unplug_output(b).expect("unplug");
        let _ = eff;
        let affected = bindings.backend_removed(1);
        assert_eq!(affected, vec![b]);
        assert_eq!(bindings.index_for(a), Some(0));
        assert_eq!(
            bindings.index_for(b),
            Some(usize::MAX),
            "presentation link severed (detached marker)"
        );
        // Logical output B is gone from the registry; the binding map
        // no longer references it (unplug_output removed the id, so
        // replug gets a fresh id and a fresh binding).
        let b2 = m.replug_output(out("B", 1280, 720));
        bindings.bind(b2, 1);
        assert_eq!(bindings.index_for(b2), Some(1));
        assert_eq!(bindings.index_for(a), Some(0));
    }

    #[test]
    fn camera_state_of_survivors_is_untouched() {
        // Unplugging B must not disturb A's presentation view.
        let (mut m, a, b) = two_outputs();
        if let Some(o) = m.primary_mut() {
            o.camera.yaw = 0.5;
        }
        let yaw_before = m.outputs()[0].1.camera.yaw;
        m.unplug_output(b).expect("unplug");
        assert_eq!(m.outputs()[0].1.camera.yaw, yaw_before, "A's camera preserved");
        let _ = a;
    }
}
