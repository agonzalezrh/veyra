//! Pointer-driven client resize (I3b).
//!
//! Resize hit testing and session math. The session freezes the visual's
//! plane frame at drag start; every later update is computed from that
//! frozen frame plus the current pointer, so results are deterministic
//! regardless of how the visual's geometry evolves while the client
//! re-renders.
//!
//! Resize changes CLIENT geometry (buffer size in logical pixels).
//! Spatial transforms are owned by the scene: `Transform3D.scale` is
//! never touched, and `Transform3D.position` only shifts so the edge
//! opposite the grabbed one stays visually anchored.

use cgmath::Vector3;

use crate::scene::VisualId;

/// Which edges of a window are being grabbed. Corners set two flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeEdges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl ResizeEdges {
    pub const NORTH: ResizeEdges = ResizeEdges { left: false, right: false, top: true, bottom: false };
    pub const SOUTH: ResizeEdges = ResizeEdges { left: false, right: false, top: false, bottom: true };
    pub const EAST: ResizeEdges = ResizeEdges { left: false, right: true, top: false, bottom: false };
    pub const WEST: ResizeEdges = ResizeEdges { left: true, right: false, top: false, bottom: false };
    pub const NORTH_WEST: ResizeEdges = ResizeEdges { left: true, right: false, top: true, bottom: false };
    pub const NORTH_EAST: ResizeEdges = ResizeEdges { left: false, right: true, top: true, bottom: false };
    pub const SOUTH_WEST: ResizeEdges = ResizeEdges { left: true, right: false, top: false, bottom: true };
    pub const SOUTH_EAST: ResizeEdges = ResizeEdges { left: false, right: true, top: false, bottom: true };

    pub fn is_corner(&self) -> bool {
        (self.left || self.right) && (self.top || self.bottom)
    }
}

/// Hit test a point in window UV space (u: 0=left, v: 0=top) against the
/// resize bands along the decorated window border.
///
/// `band_u` / `band_v` are the band thickness as a fraction of the window
/// size on each axis. Returns the grabbed edges, or None when the point
/// is in the interior (content or title bar away from the border).
pub fn hit_test_resize_zone(u: f64, v: f64, band_u: f64, band_v: f64) -> Option<ResizeEdges> {
    if band_u <= 0.0 || band_v <= 0.0 {
        return None;
    }
    let at_left = u <= band_u;
    let at_right = u >= 1.0 - band_u;
    let at_top = v <= band_v;
    let at_bottom = v >= 1.0 - band_v;
    if !at_left && !at_right && !at_top && !at_bottom {
        return None;
    }
    Some(ResizeEdges {
        left: at_left,
        right: at_right,
        top: at_top,
        bottom: at_bottom,
    })
}

/// An in-progress pointer resize of one client surface.
#[derive(Debug, Clone)]
pub struct ResizeSession {
    pub vid: VisualId,
    pub edges: ResizeEdges,
    /// Cursor position in the visual's frozen local frame at drag start,
    /// in [-0.5, 0.5] (x right, y up).
    pub start_local: (f32, f32),
    /// Decorated world size (width, height) at drag start.
    pub start_total: (f32, f32),
    /// Client logical size (width, height) at drag start.
    pub start_size: (i32, i32),
    /// Frozen visual transform (position + rotation) at drag start.
    pub start_transform: crate::scene::Transform3D,
    /// The visual's local right axis in world space.
    pub right_axis: Vector3<f32>,
    /// The visual's local up axis in world space.
    pub up_axis: Vector3<f32>,
    /// Client-requested minimum size (logical px).
    pub min_size: (i32, i32),
    /// Client-requested maximum size (logical px), None = unconstrained.
    pub max_size: Option<(i32, i32)>,
    /// Latest desired size while a configure is outstanding.
    pub desired: (i32, i32),
}

/// Result of updating a resize session with a new cursor position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeUpdate {
    /// New desired client size in logical pixels.
    pub size: (i32, i32),
    /// World-space delta to apply to the visual position so the edge
    /// opposite the grabbed one stays anchored.
    pub position_delta: Vector3<f32>,
}

impl ResizeSession {
    /// px-per-world-unit ratios.
    fn ratio_w(&self) -> f32 {
        self.start_size.0 as f32 / self.start_total.0.max(1e-6)
    }

    fn ratio_h(&self) -> f32 {
        self.start_size.1 as f32 / self.start_total.1.max(1e-6)
    }

    /// Compute the new desired size and position delta from a cursor
    /// position in the frozen local frame.
    pub fn update(&self, local: (f32, f32)) -> ResizeUpdate {
        // Cursor travel in world units along the visual's local axes.
        let dx_world = (local.0 - self.start_local.0) * self.start_total.0;
        let dy_world = (local.1 - self.start_local.1) * self.start_total.1;

        // Unclamped size deltas in client pixels per grabbed axis.
        let dw_px = if self.edges.right {
            dx_world * self.ratio_w()
        } else if self.edges.left {
            -dx_world * self.ratio_w()
        } else {
            0.0
        };
        let dh_px = if self.edges.bottom {
            -dy_world * self.ratio_h()
        } else if self.edges.top {
            dy_world * self.ratio_h()
        } else {
            0.0
        };

        let min_w = self.min_size.0.max(1);
        let min_h = self.min_size.1.max(1);
        let (max_w, max_h) = self.max_size.unwrap_or((i32::MAX, i32::MAX));

        let w = (self.start_size.0 as f32 + dw_px).round().clamp(min_w as f32, max_w as f32) as i32;
        let h = (self.start_size.1 as f32 + dh_px).round().clamp(min_h as f32, max_h as f32) as i32;

        // Actual world-size change after clamping; the grabbed edge follows
        // the cursor only as far as the clamp allows.
        let dt_w = (w - self.start_size.0) as f32 / self.ratio_w();
        let dt_h = (h - self.start_size.1) as f32 / self.ratio_h();

        // Keep the edge opposite the grabbed one anchored: the center moves
        // by half the size change toward the grabbed side.
        let mut pos_delta = Vector3::new(0.0, 0.0, 0.0);
        if self.edges.right {
            pos_delta += self.right_axis * (dt_w / 2.0);
        } else if self.edges.left {
            pos_delta -= self.right_axis * (dt_w / 2.0);
        }
        if self.edges.top {
            pos_delta += self.up_axis * (dt_h / 2.0);
        } else if self.edges.bottom {
            pos_delta -= self.up_axis * (dt_h / 2.0);
        }

        ResizeUpdate { size: (w, h), position_delta: pos_delta }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgmath::Quaternion;

    const START_W: i32 = 800;
    const START_H: i32 = 600;
    const TOTAL_W: f32 = 400.0; // scale 2.0 horizontally
    const TOTAL_H: f32 = 300.0; // scale 2.0 vertically (incl. title bar)

    fn session(edges: ResizeEdges) -> ResizeSession {
        ResizeSession {
            vid: VisualId(1),
            edges,
            start_local: (0.0, 0.0),
            start_total: (TOTAL_W, TOTAL_H),
            start_size: (START_W, START_H),
            start_transform: crate::scene::Transform3D {
                position: Vector3::new(10.0, 20.0, 30.0),
                rotation: Quaternion::new(1.0, 0.0, 0.0, 0.0),
                scale: Vector3::new(2.0, 2.0, 1.0),
            },
            right_axis: Vector3::new(1.0, 0.0, 0.0),
            up_axis: Vector3::new(0.0, 1.0, 0.0),
            min_size: (0, 0),
            max_size: None,
            desired: (START_W, START_H),
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.6
    }

    #[test]
    fn hit_test_zones_all_eight_directions() {
        let bu = 0.05;
        let bv = 0.05;
        // Edges
        assert_eq!(hit_test_resize_zone(0.01, 0.5, bu, bv), Some(ResizeEdges::WEST));
        assert_eq!(hit_test_resize_zone(0.99, 0.5, bu, bv), Some(ResizeEdges::EAST));
        assert_eq!(hit_test_resize_zone(0.5, 0.01, bu, bv), Some(ResizeEdges::NORTH));
        assert_eq!(hit_test_resize_zone(0.5, 0.99, bu, bv), Some(ResizeEdges::SOUTH));
        // Corners
        assert_eq!(hit_test_resize_zone(0.01, 0.01, bu, bv), Some(ResizeEdges::NORTH_WEST));
        assert_eq!(hit_test_resize_zone(0.99, 0.01, bu, bv), Some(ResizeEdges::NORTH_EAST));
        assert_eq!(hit_test_resize_zone(0.01, 0.99, bu, bv), Some(ResizeEdges::SOUTH_WEST));
        assert_eq!(hit_test_resize_zone(0.99, 0.99, bu, bv), Some(ResizeEdges::SOUTH_EAST));
        // Interior (content) is not a resize zone; a point inside the top
        // band IS (resize wins over title bar at the border).
        assert_eq!(hit_test_resize_zone(0.5, 0.5, bu, bv), None);
        assert_eq!(hit_test_resize_zone(0.5, 0.06, bu, bv), None);
        assert_eq!(hit_test_resize_zone(0.5, 0.03, 0.0, 0.0), None);
    }

    #[test]
    fn east_edge_grows_width_only() {
        let s = session(ResizeEdges::EAST);
        // +50 world units right = +100 client px (ratio 2)
        let up = s.update((0.125, 0.0));
        assert_eq!(up.size, (START_W + 100, START_H));
        // Center shifts right by half the world growth.
        assert!(approx(up.position_delta.x, 25.0), "center +dx/2, got {}", up.position_delta.x);
        assert!(approx(up.position_delta.y, 0.0));
        assert!(approx(up.position_delta.z, 0.0));
    }

    #[test]
    fn west_edge_shrinks_width_and_shifts_center() {
        let s = session(ResizeEdges::WEST);
        // Cursor moves right by 50 world units: left edge follows, window
        // shrinks by 100 px, center moves right by 25 world units.
        let up = s.update((0.125, 0.0));
        assert_eq!(up.size, (START_W - 100, START_H));
        assert!(approx(up.position_delta.x, 25.0));
        assert!(approx(up.position_delta.y, 0.0));
    }

    #[test]
    fn west_edge_leftward_grows() {
        let s = session(ResizeEdges::WEST);
        let up = s.update((-0.125, 0.0));
        assert_eq!(up.size, (START_W + 100, START_H));
        assert!(approx(up.position_delta.x, -25.0), "center follows left edge");
    }

    #[test]
    fn south_edge_cursor_down_grows_height() {
        let s = session(ResizeEdges::SOUTH);
        // Cursor down = local y decreases by 0.125 → 37.5 world → 75 px.
        let up = s.update((0.0, -0.125));
        assert_eq!(up.size, (START_W, START_H + 75));
        // Top anchored: center moves down (−y) by half the world growth.
        assert!(approx(up.position_delta.y, -18.75));
        assert!(approx(up.position_delta.x, 0.0));
    }

    #[test]
    fn north_edge_cursor_down_shrinks_height() {
        let s = session(ResizeEdges::NORTH);
        let up = s.update((0.0, -0.125));
        assert_eq!(up.size, (START_W, START_H - 75));
        // Bottom anchored: center moves down.
        assert!(approx(up.position_delta.y, -18.75));
    }

    #[test]
    fn north_edge_cursor_up_grows_height() {
        let s = session(ResizeEdges::NORTH);
        let up = s.update((0.0, 0.125));
        assert_eq!(up.size, (START_W, START_H + 75));
        assert!(approx(up.position_delta.y, 18.75));
    }

    #[test]
    fn corners_combine_both_axes() {
        let s = session(ResizeEdges::SOUTH_EAST);
        let up = s.update((0.125, -0.125));
        assert_eq!(up.size, (START_W + 100, START_H + 75));
        assert!(approx(up.position_delta.x, 25.0));
        assert!(approx(up.position_delta.y, -18.75));

        let s = session(ResizeEdges::NORTH_WEST);
        let up = s.update((-0.125, 0.125));
        assert_eq!(up.size, (START_W + 100, START_H + 75));
        assert!(approx(up.position_delta.x, -25.0));
        assert!(approx(up.position_delta.y, 18.75));
    }

    #[test]
    fn non_grabbed_axis_is_untouched() {
        let s = session(ResizeEdges::EAST);
        let up = s.update((0.125, -0.5));
        assert_eq!(up.size, (START_W + 100, START_H), "vertical ignored for E edge");
        assert!(approx(up.position_delta.y, 0.0));
    }

    #[test]
    fn min_size_clamps_and_position_stays_consistent() {
        let mut s = session(ResizeEdges::WEST);
        s.min_size = (700, 100);
        // Shrink far past the minimum.
        let up = s.update((0.25, 0.0)); // would be 800-200=600 → clamped to 700
        assert_eq!(up.size, (700, START_H));
        // Position delta reflects the CLAMPED change: 100 px shrunk at
        // ratio 2 = 50 world units, center shifts by half = 25.
        assert!(approx(up.position_delta.x, 25.0));
    }

    #[test]
    fn max_size_clamps() {
        let mut s = session(ResizeEdges::EAST);
        s.max_size = Some((900, 900));
        let up = s.update((0.25, 0.0)); // would be 1000 → clamped to 900
        assert_eq!(up.size, (900, START_H));
        assert!(approx(up.position_delta.x, 25.0));
    }

    #[test]
    fn updates_are_deterministic_from_frozen_start() {
        let s = session(ResizeEdges::SOUTH_EAST);
        let a = s.update((0.1, -0.1));
        let b = s.update((0.1, -0.1));
        assert_eq!(a, b, "session math is pure: no accumulation drift");
    }

    #[test]
    fn rotated_visual_axes_rotate_position_delta() {
        // 90° around Z: local +x (right) becomes world +y.
        let mut s = session(ResizeEdges::EAST);
        s.right_axis = Vector3::new(0.0, 1.0, 0.0);
        let up = s.update((0.125, 0.0));
        assert!(approx(up.position_delta.y, 25.0));
        assert!(approx(up.position_delta.x, 0.0));
        let _ = Quaternion::new(1.0, 0.0, 0.0, 0.0);
    }
}
