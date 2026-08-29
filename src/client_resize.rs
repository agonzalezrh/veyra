//! Client-driven geometry coordination (I3a).
//!
//! Wayland protocol state (configures, ACK serials, committed state) is
//! owned by Smithay: `XdgToplevelSurfaceRoleAttributes` publicly exposes
//! `pending_configures()`, `configure_serial`, `current_serial` and the
//! committed `current` state. This module holds only VEYRA'S INTENT:
//! which size we asked a client to become, for which configure serial,
//! and whether the client has confirmed it yet.
//!
//! Governing invariant: Veyra requests geometry; the client decides
//! geometry by what it commits. If a client commits a different size
//! than requested, the committed size wins and the request is abandoned
//! — Veyra must never overwrite committed geometry.

use smithay::utils::Serial;

use crate::scene::VisualId;

/// Veyra's outstanding request for a client surface to resize.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientResizeRequest {
    pub vid: VisualId,
    /// Serial of the configure Veyra sent for this request.
    pub serial: Serial,
    /// Logical size (width, height) Veyra requested.
    pub requested: (i32, i32),
    /// Whether the client acknowledged this specific serial.
    pub acknowledged: bool,
}

/// Result of reporting a client commit to the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The committed buffer matches the requested size: request fulfilled.
    Fulfilled,
    /// The client committed a different size than requested: the client
    /// wins, the request is abandoned and committed geometry is adopted.
    ClientOverride,
    /// No resize request is in flight for this visual.
    NotResizing,
}

/// Tracks at most one outstanding geometry request per visual.
///
/// Multiple windows resize independently: entries are keyed by VisualId.
#[derive(Debug, Default)]
pub struct ClientResizeCoordinator {
    entries: Vec<ClientResizeRequest>,
}

impl ClientResizeCoordinator {
    /// Record that a configure with `serial` was sent to the visual,
    /// requesting `size`. Replaces any previous entry for the visual
    /// (at most one outstanding request per surface).
    pub fn mark_sent(&mut self, vid: VisualId, serial: Serial, size: (i32, i32)) {
        self.entries.retain(|e| e.vid != vid);
        self.entries.push(ClientResizeRequest {
            vid,
            serial,
            requested: size,
            acknowledged: false,
        });
    }

    /// Report an ACK from a client. Only completes the request whose
    /// serial matches; ACKs for older or unknown serials are ignored.
    /// Returns true when a request was marked acknowledged.
    pub fn note_ack(&mut self, serial: Serial) -> bool {
        let matched = self
            .entries
            .iter_mut()
            .find(|e| e.serial == serial && !e.acknowledged);
        match matched {
            Some(entry) => {
                entry.acknowledged = true;
                true
            }
            None => false,
        }
    }

    /// Report a client commit with the given buffer size (w, h).
    ///
    /// A matching buffer fulfills the request; any other size means the
    /// client overrode us and the request is abandoned. Either way the
    /// entry is cleared — geometry adoption happens in the commit path,
    /// which always follows the committed buffer.
    pub fn note_commit(&mut self, vid: VisualId, buffer_size: (i32, i32)) -> CommitOutcome {
        let Some(pos) = self.entries.iter().position(|e| e.vid == vid) else {
            return CommitOutcome::NotResizing;
        };
        let entry = self.entries.remove(pos);
        if entry.requested == buffer_size {
            CommitOutcome::Fulfilled
        } else {
            CommitOutcome::ClientOverride
        }
    }

    /// Whether the visual has an outstanding (unacknowledged) request.
    /// While true, the compositor must not send another configure.
    pub fn awaiting_ack(&self, vid: VisualId) -> bool {
        self.entries
            .iter()
            .any(|e| e.vid == vid && !e.acknowledged)
    }

    /// The outstanding request for a visual, if any.
    pub fn entry(&self, vid: VisualId) -> Option<&ClientResizeRequest> {
        self.entries.iter().find(|e| e.vid == vid)
    }

    /// Drop the request for a visual (pointer release, resize abort).
    pub fn abort(&mut self, vid: VisualId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.vid != vid);
        before != self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of outstanding requests (test helper).
    pub fn entries_len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(n: u64) -> VisualId {
        VisualId(n)
    }

    #[test]
    fn configure_ack_matching_buffer_is_fulfilled() {
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);
        assert!(!c.awaiting_ack(v));
        c.mark_sent(v, Serial::from(10u32), (800, 600));
        assert!(c.awaiting_ack(v));
        assert!(c.note_ack(Serial::from(10u32)), "ack marks the request");
        assert!(!c.awaiting_ack(v), "acked — new configures may be sent");
        assert_eq!(c.note_commit(v, (800, 600)), CommitOutcome::Fulfilled);
        assert!(c.is_empty(), "request cleared after commit");
    }

    #[test]
    fn wrong_sized_buffer_means_client_wins() {
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);
        c.mark_sent(v, Serial::from(11u32), (800, 600));
        c.note_ack(Serial::from(11u32));
        // Client commits a size we did not request (or an old buffer).
        assert_eq!(c.note_commit(v, (640, 480)), CommitOutcome::ClientOverride);
        assert!(c.is_empty(), "override abandons the request");
        // The coordinator never stores a size to enforce afterwards.
    }

    #[test]
    fn no_duplicate_configure_while_unacked() {
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);
        c.mark_sent(v, Serial::from(12u32), (800, 600));
        assert!(
            c.awaiting_ack(v),
            "second motion must not produce another configure"
        );
        // Even a second mark_sent for the same visual replaces, never queues.
        c.mark_sent(v, Serial::from(13u32), (900, 700));
        assert_eq!(c.entry(v).map(|e| e.serial), Some(Serial::from(13u32)));
        assert_eq!(c.entries_len(), 1);
    }

    #[test]
    fn ack_for_older_serial_does_not_complete_current_request() {
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);
        // An older configure was outstanding and then superseded.
        c.mark_sent(v, Serial::from(20u32), (700, 500));
        c.mark_sent(v, Serial::from(21u32), (800, 600));
        assert!(!c.note_ack(Serial::from(20u32)), "stale ack ignored");
        assert!(c.awaiting_ack(v), "current request still outstanding");
        assert!(c.note_ack(Serial::from(21u32)));
        assert!(!c.awaiting_ack(v));
    }

    #[test]
    fn request_survives_multiple_commit_cycles() {
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);

        // Cycle 1: request 800x600, ack, commit.
        c.mark_sent(v, Serial::from(30u32), (800, 600));
        c.note_ack(Serial::from(30u32));
        assert_eq!(c.note_commit(v, (800, 600)), CommitOutcome::Fulfilled);

        // Cycle 2: a new request with a new serial works independently.
        c.mark_sent(v, Serial::from(31u32), (1000, 700));
        assert!(c.awaiting_ack(v));
        c.note_ack(Serial::from(31u32));
        assert_eq!(c.note_commit(v, (1000, 700)), CommitOutcome::Fulfilled);
        assert!(c.is_empty());
    }

    #[test]
    fn pointer_release_while_configure_outstanding_aborts() {
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);
        c.mark_sent(v, Serial::from(40u32), (800, 600));
        assert!(c.abort(v), "outstanding request dropped");
        assert!(c.is_empty());
        // A late ack/commit afterwards touches nothing.
        assert!(!c.note_ack(Serial::from(40u32)));
        assert_eq!(c.note_commit(v, (800, 600)), CommitOutcome::NotResizing);
    }

    #[test]
    fn surface_destruction_while_configure_outstanding() {
        let mut c = ClientResizeCoordinator::default();
        let a = vid(1);
        let b = vid(2);
        c.mark_sent(a, Serial::from(50u32), (800, 600));
        c.mark_sent(b, Serial::from(51u32), (400, 300));
        c.abort(a); // destruction cleanup path
        assert_eq!(c.entry(a), None);
        assert_eq!(c.entry(b).map(|e| e.requested), Some((400, 300)));
        assert!(c.note_ack(Serial::from(51u32)));
        assert_eq!(c.note_commit(b, (400, 300)), CommitOutcome::Fulfilled);
    }

    #[test]
    fn multiple_windows_resize_independently() {
        let mut c = ClientResizeCoordinator::default();
        let a = vid(1);
        let b = vid(2);
        c.mark_sent(a, Serial::from(60u32), (800, 600));
        c.mark_sent(b, Serial::from(61u32), (500, 400));
        assert!(c.awaiting_ack(a) && c.awaiting_ack(b));
        c.note_ack(Serial::from(61u32));
        assert!(c.awaiting_ack(a), "a unaffected by b's ack");
        assert!(!c.awaiting_ack(b));
        assert_eq!(c.note_commit(a, (800, 600)), CommitOutcome::Fulfilled);
        assert_eq!(c.entry(b).map(|e| e.requested), Some((500, 400)));
    }

    #[test]
    fn commits_without_requests_are_normal() {
        let mut c = ClientResizeCoordinator::default();
        assert_eq!(c.note_commit(vid(9), (1920, 1080)), CommitOutcome::NotResizing);
        assert!(!c.note_ack(Serial::from(70u32)));
    }

    #[test]
    fn client_may_commit_without_acking_and_keeps_geometry() {
        // A client that ignores our size entirely: ack never arrives,
        // it commits its own size. The request must be abandoned and
        // nothing may force the requested size afterwards.
        let mut c = ClientResizeCoordinator::default();
        let v = vid(1);
        c.mark_sent(v, Serial::from(80u32), (1600, 900));
        assert_eq!(c.note_commit(v, (640, 480)), CommitOutcome::ClientOverride);
        assert!(c.is_empty());
        assert!(!c.awaiting_ack(v));
        assert_eq!(c.entry(v), None);
    }
}
