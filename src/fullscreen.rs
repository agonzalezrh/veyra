//! Fullscreen coordination (I7).
//!
//! Fullscreen is a SURFACE STATE TRANSITION, not a geometry operation:
//! the same invariant as maximize (I4) applies — the client's committed
//! buffers remain the geometry authority; Veyra never scales or rotates
//! a visual to make it "fit". The presentation pose change (a fullscreen
//! window is presented centered on the workspace view) happens only
//! after the client ACKs and commits at the configured size.
//!
//! State machine:
//!
//! ```text
//! NORMAL or MAXIMIZED
//!   │ set_fullscreen (client or compositor)
//!   ▼
//! PENDING_FULLSCREEN   (snapshot captured exactly once)
//!   │ configure(Fullscreen, presentation area) sent + ACK + commit
//!   ▼
//! FULLSCREEN
//! ```
//!
//! Exit is the mirror image; unfullscreen restores the state existed
//! immediately before fullscreen was entered (MAXIMIZED → FULLSCREEN →
//! MAXIMIZED, NORMAL → FULLSCREEN → NORMAL).
//!
//! Snapshot discipline: `FullscreenSnapshot` is captured ONCE when the
//! pending transition begins — never while fullscreen (so a stray commit
//! or layout pass cannot rot the restore point).

use smithay::utils::Serial;

use crate::scene::VisualId;

/// Who asked for the fullscreen change. Reuses the maximize/minimize
/// taxonomy: client request vs compositor key/menu.
pub type FullscreenSource = crate::maximize::MaximizeSource;

/// The compositor's presentation area: the surface Veyra renders into,
/// expressed in logical (surface-relative) pixels. Fullscreen configures
/// a client to this size; nothing may hardcode a display size here.
/// Multi-output Expansion (J5) replaces the single-rect model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationArea {
    pub logical_w: i32,
    pub logical_h: i32,
}

impl PresentationArea {
    /// Derive from the compositor's window framebuffer size (logical).
    pub fn for_window_size(window_size: (f32, f32)) -> Self {
        PresentationArea {
            logical_w: (window_size.0.round() as i32).max(1),
            logical_h: (window_size.1.round() as i32).max(1),
        }
    }

    pub fn size(&self) -> (i32, i32) {
        (self.logical_w, self.logical_h)
    }
}

/// The pre-fullscreen presentation + state snapshot. Captured exactly
/// once at entry into PENDING_FULLSCREEN; consumed (taken) on
/// unfullscreen restore.
#[derive(Debug, Clone, PartialEq)]
pub struct FullscreenSnapshot {
    /// Pre-fullscreen committed size to hand back to the client.
    pub restore_size: (i32, i32),
    /// Pre-fullscreen presentation pose: (position xyz, rotation ijkw).
    pub restore_pos: (f32, f32, f32),
    pub restore_rot: [f32; 4],
    /// Whether the window was MAXIMIZED immediately before fullscreen.
    /// Unfullscreen returns to that state instead of NORMAL.
    pub was_maximized: bool,
}

impl FullscreenSnapshot {
    pub fn capture(
        size: (i32, i32),
        pos: cgmath::Vector3<f32>,
        rot: cgmath::Quaternion<f32>,
        was_maximized: bool,
    ) -> Self {
        FullscreenSnapshot {
            restore_size: size,
            restore_pos: (pos.x, pos.y, pos.z),
            // Quaternion memory layout: [i, j, k, w]
            restore_rot: [rot.v.x, rot.v.y, rot.v.z, rot.s],
            was_maximized,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenKind {
    Fullscreen,
    Unfullscreen,
}

/// One outstanding fullscreen/unfullscreen transaction per surface.
#[derive(Debug, Clone, PartialEq)]
pub struct FullscreenIntent {
    pub vid: VisualId,
    pub kind: FullscreenKind,
    pub source: FullscreenSource,
    /// Serial of the configure Veyra sent for this transition.
    pub serial: Serial,
    /// Logical size configured for this transition (presentation area
    /// on entry; restored size on exit).
    pub target: (i32, i32),
    /// Committed size before the transition (draining detection).
    pub previous: (i32, i32),
    /// Snapshot for Unfullscreen (the pose/values to restore). None for
    /// Fullscreen: entering takes the snapshot into ToplevelInfo once.
    pub snapshot: Option<FullscreenSnapshot>,
}

/// Tracks Veyra's outstanding fullscreen intents: at most one per
/// surface, one deferred request overall (last wins). Mirrors
/// `MaximizeCoordinator` exactly.
#[derive(Debug, Default)]
pub struct FullscreenCoordinator {
    intents: Vec<FullscreenIntent>,
    deferred: Option<(VisualId, FullscreenKind, FullscreenSource)>,
}

impl FullscreenCoordinator {
    /// Record a new intent for a visual, replacing any previous one.
    pub fn begin(&mut self, intent: FullscreenIntent) {
        self.intents.retain(|i| i.vid != intent.vid);
        self.intents.push(intent);
    }

    pub fn intent(&self, vid: VisualId) -> Option<&FullscreenIntent> {
        self.intents.iter().find(|i| i.vid == vid)
    }

    /// Complete the transaction: ownership transfer to the caller.
    pub fn take_intent(&mut self, vid: VisualId) -> Option<FullscreenIntent> {
        let pos = self.intents.iter().position(|i| i.vid == vid)?;
        Some(self.intents.remove(pos))
    }

    /// Queue a request that could not be configured immediately
    /// (last request wins).
    pub fn defer(&mut self, vid: VisualId, kind: FullscreenKind, source: FullscreenSource) {
        self.deferred = Some((vid, kind, source));
    }

    pub fn take_deferred(&mut self) -> Option<(VisualId, FullscreenKind, FullscreenSource)> {
        self.deferred.take()
    }

    pub fn abort(&mut self, vid: VisualId) {
        self.intents.retain(|i| i.vid != vid);
        if self.deferred.as_ref().is_some_and(|(dvid, _, _)| *dvid == vid) {
            self.deferred = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgmath::Quaternion;
    use crate::scene::VisualId;

    const AREA: (f32, f32) = (1280.0, 720.0);

    fn snap(maximized: bool) -> FullscreenSnapshot {
        FullscreenSnapshot::capture(
            (800, 600),
            cgmath::Vector3::new(1.0, 2.0, 3.0),
            Quaternion::new(1.0, 0.0, 0.0, 0.0),
            maximized,
        )
    }

    #[test]
    fn presentation_area_scales_with_window() {
        // Nothing is hardcoded: the area derives from the actual window.
        assert_eq!(PresentationArea::for_window_size(AREA).size(), (1280, 720));
        assert_eq!(PresentationArea::for_window_size((1000.4, 800.6)).size(), (1000, 801));
        assert_eq!(PresentationArea::for_window_size((0.0, 0.0)).size(), (1, 1));
    }

    #[test]
    fn snapshot_layout_matches_quaternion_convention() {
        // cgmath Quaternion::new is scalar-first (w, x, y, z); the
        // snapshot stores ijkw so restore can map straight back.
        let q = Quaternion::new(0.9, 0.1, 0.2, 0.3); // (w, i, j, k)
        let s = FullscreenSnapshot::capture((10, 20), cgmath::Vector3::new(1.0, 2.0, 3.0), q, false);
        assert_eq!(s.restore_rot, [0.1, 0.2, 0.3, 0.9]);
        assert_eq!(s.restore_pos, (1.0, 2.0, 3.0));
        assert_eq!(s.restore_size, (10, 20));
        assert!(!s.was_maximized);
    }

    #[test]
    fn intent_lifecycle_replaces_and_aborts() {
        let mut fc = FullscreenCoordinator::default();
        let vid = VisualId(1);
        fc.begin(FullscreenIntent {
            vid, kind: FullscreenKind::Fullscreen,
            source: FullscreenSource::Client, serial: smithay::utils::Serial::from(1),
            target: (1280, 720), previous: (800, 600), snapshot: Some(snap(false)),
        });
        // A newer request for the same surface supersedes the old one.
        fc.begin(FullscreenIntent {
            vid, kind: FullscreenKind::Unfullscreen,
            source: FullscreenSource::Compositor, serial: smithay::utils::Serial::from(2),
            target: (800, 600), previous: (1280, 720), snapshot: None,
        });
        assert_eq!(fc.intents.len(), 1);
        assert!(fc.intent(vid).unwrap().kind == FullscreenKind::Unfullscreen);
        fc.abort(vid);
        assert!(fc.intent(vid).is_none());
        assert!(fc.take_deferred().is_none());
    }

    #[test]
    fn deferred_last_wins_and_aborts_with_surface() {
        let mut fc = FullscreenCoordinator::default();
        let vid = VisualId(2);
        fc.defer(vid, FullscreenKind::Fullscreen, FullscreenSource::Client);
        fc.defer(vid, FullscreenKind::Unfullscreen, FullscreenSource::Compositor);
        assert_eq!(
            fc.take_deferred(),
            Some((vid, FullscreenKind::Unfullscreen, FullscreenSource::Compositor))
        );
        fc.defer(vid, FullscreenKind::Fullscreen, FullscreenSource::Client);
        fc.abort(vid);
        assert!(fc.take_deferred().is_none());
        assert!(fc.intent(vid).is_none());
    }

    #[test]
    fn take_intent_gives_ownership() {
        let mut fc = FullscreenCoordinator::default();
        let vid = VisualId(3);
        fc.begin(FullscreenIntent {
            vid, kind: FullscreenKind::Fullscreen,
            source: FullscreenSource::Compositor, serial: smithay::utils::Serial::from(5),
            target: (640, 480), previous: (320, 240), snapshot: None,
        });
        let taken = fc.take_intent(vid).expect("intent owned");
        assert_eq!(taken.snapshot, None);
        assert!(fc.intent(vid).is_none());
    }
}
