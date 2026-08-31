//! Maximize coordination (I4).
//!
//! Architectural invariant: maximize changes the client's ALLOCATED/
//! CONFIGURED geometry (xdg_toplevel state + size), never the visual's
//! spatial transform (position/rotation/scale). The scene quad grows
//! because the client commits bigger buffers and the commit path adopts
//! committed geometry (I3a) — Veyra never scales, rotates or moves a
//! visual as part of maximizing.
//!
//! Division of ownership mirrors `client_resize` (I3a):
//! - Smithay owns the protocol state (configures, ACK serials, states).
//! - `ClientResizeCoordinator` owns the outstanding size request, so
//!   maximize reuses its pacing (at most one unacknowledged configure).
//! - This module holds only VEYRA'S INTENT: which maximize transition is
//!   in flight for which surface, for which serial, and what to restore.
//!
//! Governing rules:
//! - The client decides geometry by what it commits. If a client acks a
//!   maximized configure but commits a different size, the committed
//!   size wins and the maximized STATE is still recorded (the state bit
//!   was acknowledged).
//! - A request that cannot be configured immediately (surface still has
//!   an unacknowledged configure) is deferred and flushed from the
//!   ack/commit paths.

use smithay::utils::Serial;

use crate::scene::VisualId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaximizeKind {
    Maximize,
    Unmaximize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaximizeSource {
    /// The client sent xdg_toplevel.set_maximized / unset_maximized.
    Client,
    /// The compositor initiated the change (key binding).
    Compositor,
}

/// One outstanding maximize/unmaximize transaction per surface.
#[derive(Debug, Clone, PartialEq)]
pub struct MaximizeIntent {
    pub vid: VisualId,
    pub kind: MaximizeKind,
    pub source: MaximizeSource,
    /// Serial of the configure Veyra sent for this transition.
    pub serial: Serial,
    /// Logical size Veyra configured the client to.
    pub target: (i32, i32),
    /// The pre-maximize committed size to restore on unmaximize.
    pub restore: (i32, i32),
    /// The committed size before the transition started. A commit at
    /// this size is a DRAINING buffer (acked but not yet redrawn at the
    /// new size) — it does not complete the transaction.
    pub previous: (i32, i32),
    /// Spatial transform captured when the transition began. A maximized
    /// window is presented centered on the workspace view; these values
    /// are restored verbatim when the window is unmaximized.
    pub restore_pos: (f32, f32, f32),
    pub restore_rot: [f32; 4], // quaternion (i, j, k, w)
}

/// Tracks Veyra's outstanding maximize intents. At most one intent per
/// surface and at most one deferred request overall (last wins).
#[derive(Debug, Default)]
pub struct MaximizeCoordinator {
    intents: Vec<MaximizeIntent>,
    deferred: Option<(VisualId, MaximizeKind, MaximizeSource)>,
}

impl MaximizeCoordinator {
    /// Record a new intent for a visual, replacing any previous one
    /// (a newer configure supersedes the older transition).
    pub fn begin(&mut self, intent: MaximizeIntent) {
        self.intents.retain(|i| i.vid != intent.vid);
        self.intents.push(intent);
    }

    /// The outstanding intent for a visual, if any.
    pub fn intent(&self, vid: VisualId) -> Option<&MaximizeIntent> {
        self.intents.iter().find(|i| i.vid == vid)
    }

    /// Complete the outstanding intent for a visual: ownership transfer
    /// to the caller, which applies the state change to the toplevel.
    pub fn take_intent(&mut self, vid: VisualId) -> Option<MaximizeIntent> {
        let pos = self.intents.iter().position(|i| i.vid == vid)?;
        Some(self.intents.remove(pos))
    }

    /// Queue a request that could not be configured immediately.
    /// Last request wins — a newer one replaces an older deferred one.
    pub fn defer(&mut self, vid: VisualId, kind: MaximizeKind, source: MaximizeSource) {
        self.deferred = Some((vid, kind, source));
    }

    /// Pop the deferred request (caller retries it).
    pub fn take_deferred(&mut self) -> Option<(VisualId, MaximizeKind, MaximizeSource)> {
        self.deferred.take()
    }

    /// Drop all state for a visual (surface destroyed / aborted).
    pub fn abort(&mut self, vid: VisualId) {
        self.intents.retain(|i| i.vid != vid);
        if self.deferred.map_or(false, |(dvid, _, _)| dvid == vid) {
            self.deferred = None;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.intents.is_empty() && self.deferred.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(n: u64) -> VisualId {
        VisualId(n)
    }

    fn intent(v: VisualId, serial: u32, target: (i32, i32)) -> MaximizeIntent {
        MaximizeIntent {
            vid: v,
            kind: MaximizeKind::Maximize,
            source: MaximizeSource::Client,
            serial: Serial::from(serial),
            target,
            restore: (640, 480),
            previous: (640, 480),
            restore_pos: (0.0, 0.0, 0.0),
            restore_rot: [0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn begin_replaces_previous_intent_for_same_visual() {
        let mut c = MaximizeCoordinator::default();
        c.begin(intent(vid(1), 10, (1280, 720)));
        c.begin(intent(vid(1), 11, (1920, 1080)));
        assert_eq!(c.intent(vid(1)).map(|i| i.serial), Some(Serial::from(11u32)));
        assert_eq!(c.take_intent(vid(1)).map(|i| i.target), Some((1920, 1080)));
        assert!(c.intent(vid(1)).is_none());
    }

    #[test]
    fn intents_are_independent_per_visual() {
        let mut c = MaximizeCoordinator::default();
        c.begin(intent(vid(1), 10, (1280, 720)));
        c.begin(intent(vid(2), 11, (800, 600)));
        // Completing visual 2 leaves visual 1 untouched.
        let taken = c.take_intent(vid(2)).expect("intent for vid 2");
        assert_eq!(taken.target, (800, 600));
        assert!(c.intent(vid(1)).is_some());
        assert!(c.take_intent(vid(2)).is_none(), "intent consumed");
    }

    #[test]
    fn unmaximize_intent_replaces_maximize_intent() {
        let mut c = MaximizeCoordinator::default();
        c.begin(intent(vid(1), 10, (1280, 720)));
        let mut un = intent(vid(1), 12, (640, 480));
        un.kind = MaximizeKind::Unmaximize;
        c.begin(un);
        let taken = c.take_intent(vid(1)).expect("unmaximize intent");
        assert_eq!(taken.kind, MaximizeKind::Unmaximize);
        assert_eq!(taken.target, (640, 480));
    }

    #[test]
    fn defer_is_last_wins() {
        let mut c = MaximizeCoordinator::default();
        c.defer(vid(1), MaximizeKind::Maximize, MaximizeSource::Client);
        c.defer(vid(1), MaximizeKind::Unmaximize, MaximizeSource::Compositor);
        let (dvid, kind, source) = c.take_deferred().expect("deferred request");
        assert_eq!(dvid, vid(1));
        assert_eq!(kind, MaximizeKind::Unmaximize);
        assert_eq!(source, MaximizeSource::Compositor);
        assert!(c.take_deferred().is_none());
    }

    #[test]
    fn abort_clears_intent_and_deferred_for_visual() {
        let mut c = MaximizeCoordinator::default();
        c.begin(intent(vid(1), 10, (1280, 720)));
        c.begin(intent(vid(2), 11, (800, 600)));
        c.defer(vid(1), MaximizeKind::Maximize, MaximizeSource::Client);
        c.abort(vid(1));
        assert!(c.intent(vid(1)).is_none());
        assert!(c.take_deferred().is_none(), "deferred request for vid dropped");
        assert!(c.intent(vid(2)).is_some(), "other visual unaffected");
        assert!(!c.is_empty());
        c.abort(vid(2));
        assert!(c.is_empty());
    }

    #[test]
    fn empty_coordinator_completes_nothing() {
        let mut c = MaximizeCoordinator::default();
        assert!(c.intent(vid(9)).is_none());
        assert!(c.take_intent(vid(9)).is_none());
        assert!(c.take_deferred().is_none());
    }
}
