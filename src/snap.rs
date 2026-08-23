//! Spatial snapping engine for drag interactions.
//!
//! Pure-data: operates on (position, size) tuples — no Scene/Visual references.
//! Camera-independent: works in workspace coordinates.
//! Only suggests a position; the InteractionController decides whether to apply it.

use cgmath::Vector3;

/// A snap candidate describing an aligned position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapCandidate {
    /// The suggested snapped position.
    pub position: Vector3<f32>,
    /// The type of alignment that triggered this snap.
    pub kind: SnapKind,
    /// The strength of the snap (e.g. alignment distance).
    pub strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SnapKind {
    Left,
    Right,
    Top,
    Bottom,
    CenterH,
    CenterV,
    Origin,
}

/// Configuration for snap detection.
#[derive(Debug, Clone)]
pub struct SnapConfig {
    /// Maximum distance in world units for a snap to activate.
    pub threshold: f32,
}

impl Default for SnapConfig {
    fn default() -> Self {
        SnapConfig { threshold: 30.0 }
    }
}

/// The snapping engine — a pure function.
///
/// Given a moving visual's current position and size, and a list of
/// anchor visuals' (position, width, height) tuples, returns the best
/// snap candidate (if any).
pub fn snap_position(
    moving_pos: Vector3<f32>,
    moving_w: f32,
    moving_h: f32,
    anchors: &[(Vector3<f32>, f32, f32)],
    config: &SnapConfig,
) -> Option<SnapCandidate> {
    let mut best: Option<SnapCandidate> = None;

    // Check each anchor for edge/center alignment
    for &(apos, aw, ah) in anchors {
        let left = pos_after_snap(moving_pos, moving_w, moving_h, apos, aw, ah, config);
        if let Some(candidate) = left {
            if best.as_ref().map_or(true, |b| candidate.strength < b.strength) {
                best = Some(candidate);
            }
        }
    }

    // Check workspace origin
    if let Some(candidate) = snap_to_origin(moving_pos, moving_w, moving_h, config) {
        if best.as_ref().map_or(true, |b| candidate.strength < b.strength) {
            best = Some(candidate);
        }
    }

    best
}

/// Check if the moving visual's edges align with an anchor visual.
fn pos_after_snap(
    mpos: Vector3<f32>,
    mw: f32,
    mh: f32,
    apos: Vector3<f32>,
    aw: f32,
    ah: f32,
    config: &SnapConfig,
) -> Option<SnapCandidate> {
    let t = config.threshold;

    // Compute moving bounds
    let ml = mpos.x - mw / 2.0;
    let mr = mpos.x + mw / 2.0;
    let mt = mpos.y - mh / 2.0; // top (y decreases = up in our scene)
    let mb = mpos.y + mh / 2.0;

    // Compute anchor bounds
    let al = apos.x - aw / 2.0;
    let ar = apos.x + aw / 2.0;
    let at = apos.y - ah / 2.0;
    let ab = apos.y + ah / 2.0;

    // Test each alignment
    // Left edge snap: moving's left aligns with anchor's right
    let d_left = (ml - ar).abs();
    let y_overlap = (mt < ab && mb > at);
    if d_left < t && y_overlap {
        let new_x = ar + mw / 2.0;
        return Some(SnapCandidate {
            position: Vector3::new(new_x, mpos.y, mpos.z),
            kind: SnapKind::Left,
            strength: d_left,
        });
    }

    // Right edge snap: moving's right aligns with anchor's left
    let d_right = (mr - al).abs();
    if d_right < t && y_overlap {
        let new_x = al - mw / 2.0;
        return Some(SnapCandidate {
            position: Vector3::new(new_x, mpos.y, mpos.z),
            kind: SnapKind::Right,
            strength: d_right,
        });
    }

    // Top edge snap: moving's top aligns with anchor's bottom
    let d_top = (mt - ab).abs();
    let x_overlap = (ml < ar && mr > al);
    if d_top < t && x_overlap {
        let new_y = ab + mh / 2.0;
        return Some(SnapCandidate {
            position: Vector3::new(mpos.x, new_y, mpos.z),
            kind: SnapKind::Top,
            strength: d_top,
        });
    }

    // Bottom edge snap: moving's bottom aligns with anchor's top
    let d_bot = (mb - at).abs();
    if d_bot < t && x_overlap {
        let new_y = at - mh / 2.0;
        return Some(SnapCandidate {
            position: Vector3::new(mpos.x, new_y, mpos.z),
            kind: SnapKind::Bottom,
            strength: d_bot,
        });
    }

    // Horizontal center snap (vertical alignment)
    let d_cx = (mpos.x - apos.x).abs();
    if d_cx < t && y_overlap {
        return Some(SnapCandidate {
            position: Vector3::new(apos.x, mpos.y, mpos.z),
            kind: SnapKind::CenterH,
            strength: d_cx,
        });
    }

    // Vertical center snap (horizontal alignment)
    let d_cy = (mpos.y - apos.y).abs();
    if d_cy < t && x_overlap {
        return Some(SnapCandidate {
            position: Vector3::new(mpos.x, apos.y, mpos.z),
            kind: SnapKind::CenterV,
            strength: d_cy,
        });
    }

    None
}

/// Snap to workspace origin (0, 0) — useful for centering or grid alignment.
fn snap_to_origin(
    mpos: Vector3<f32>,
    _mw: f32,
    _mh: f32,
    config: &SnapConfig,
) -> Option<SnapCandidate> {
    let t = config.threshold;
    let d = (mpos.x * mpos.x + mpos.y * mpos.y + mpos.z * mpos.z).sqrt();
    if d < t {
        Some(SnapCandidate {
            position: Vector3::new(0.0, 0.0, mpos.z),
            kind: SnapKind::Origin,
            strength: d,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec(x: f32, y: f32) -> Vector3<f32> {
        Vector3::new(x, y, 0.0)
    }

    #[test]
    fn snap_left_edge_within_threshold() {
        let cfg = SnapConfig { threshold: 60.0 };
        // Moving at (120, 0) size 100x50
        // Left edge = 120 - 50 = 70
        // Anchor at (0,0) size 100x50, right edge = 50
        // Distance = |70 - 50| = 20, within 60
        let anchors = [(vec(0.0, 0.0), 100.0, 50.0)];
        let result = snap_position(vec(120.0, 0.0), 100.0, 50.0, &anchors, &SnapConfig { threshold: 60.0 });
        assert!(result.is_some(), "should snap left edge within threshold");
        if let Some(snap) = result {
            // Snapped left edge = anchor right = 50
            // New center = 50 + 50 = 100
            assert!((snap.position.x - 100.0).abs() < 0.01, "snapped x should be 100, got {}", snap.position.x);
        }
    }

    #[test]
    fn no_snap_far_away() {
        let cfg = SnapConfig { threshold: 30.0 };
        let anchors = [(vec(0.0, 0.0), 100.0, 50.0)];
        let result = snap_position(vec(500.0, 0.0), 100.0, 50.0, &anchors, &cfg);
        assert!(result.is_none(), "should not snap when far away");
    }

    #[test]
    fn snap_right_edge() {
        let cfg = SnapConfig { threshold: 30.0 };
        // Moving right edge = -100 + 50 = -50
        // Anchor left edge = 0 - 50 = -50
        // Distance = 0 -> should snap right edge perfectly
        let anchors = [(vec(0.0, 0.0), 100.0, 50.0)];
        let result = snap_position(vec(-100.0, 0.0), 100.0, 50.0, &anchors, &cfg);
        assert!(result.is_some(), "right edge should snap");
        if let Some(snap) = result {
            // Snapped x = anchor_left - moving_half = -50 - 50 = -100
            assert!((snap.position.x - (-100.0)).abs() < 0.01);
        }
    }

    #[test]
    fn snap_top_edge() {
        let cfg = SnapConfig { threshold: 30.0 };
        // Moving below anchor: moving top = 100 - 25 = 75
        // Anchor bottom = 0 + 25 = 25
        // Distance = 50 > 30 -> no snap
        // Make it closer:
        let anchors = [(vec(0.0, 0.0), 100.0, 50.0)];
        let result = snap_position(vec(0.0, 50.0), 100.0, 50.0, &anchors, &SnapConfig { threshold: 60.0 });
        assert!(result.is_some(), "top edge should snap with larger threshold");
        if let Some(snap) = result {
            // Moving top = anchor bottom = 25
            // New center = 25 + 25 = 50
            assert!((snap.position.y - 50.0).abs() < 0.01, "snapped y should be 50, got {}", snap.position.y);
        }
    }

    #[test]
    fn snap_to_origin() {
        let cfg = SnapConfig { threshold: 30.0 };
        let anchors: [(Vector3<f32>, f32, f32); 0] = [];
        let result = snap_position(vec(10.0, 15.0), 100.0, 50.0, &anchors, &cfg);
        assert!(result.is_some(), "should snap to origin when close");
        if let Some(snap) = result {
            assert!((snap.position.x).abs() < 0.01);
            assert!((snap.position.y).abs() < 0.01);
        }
    }

    #[test]
    fn no_snap_to_origin_when_far() {
        let cfg = SnapConfig { threshold: 30.0 };
        let anchors: [(Vector3<f32>, f32, f32); 0] = [];
        let result = snap_position(vec(500.0, 500.0), 100.0, 50.0, &anchors, &cfg);
        assert!(result.is_none(), "should not snap to origin when far");
    }

    #[test]
    fn snap_bottom_edge() {
        let cfg = SnapConfig { threshold: 30.0 };
        // Moving above anchor: moving bottom = -100 + 25 = -75
        // Anchor top = 0 - 25 = -25
        // Distance = 50 > 30
        // Make closer:
        let anchors = [(vec(0.0, 0.0), 100.0, 50.0)];
        let result = snap_position(vec(0.0, -50.0), 100.0, 50.0, &anchors, &SnapConfig { threshold: 60.0 });
        assert!(result.is_some(), "bottom edge should snap with larger threshold");
        if let Some(snap) = result {
            // Moving bottom = anchor top = -25
            // New center = -25 - 25 = -50
            assert!((snap.position.y - (-50.0)).abs() < 0.01, "snapped y should be -50, got {}", snap.position.y);
        }
    }

    #[test]
    fn no_overlap_no_snap() {
        // Moving top = 200 - 25 = 175
        // Anchor bottom = 0 + 25 = 25
        // Y overlap fails, so no snap even though X edges are close
        let anchors = [(vec(0.0, 0.0), 100.0, 50.0)];
        let result = snap_position(vec(110.0, 200.0), 100.0, 50.0, &anchors, &SnapConfig { threshold: 100.0 });
        // Y gap is large -> no overlap -> no snap
        assert!(result.is_none() || result.unwrap().kind == SnapKind::Origin);
    }
}
