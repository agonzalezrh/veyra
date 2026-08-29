//! Closed-window history: tombstones that allow reopening a recently
//! closed window by relaunching its application and reattaching the
//! saved 3D transform and workspace.

use crate::scene::Transform3D;

/// A tombstone recorded when a window is closed/destroyed.
#[derive(Debug, Clone)]
pub struct ClosedWindow {
    pub app_id: String,
    pub title: String,
    /// Workspace index the visual belonged to.
    pub workspace: usize,
    /// The visual's 3D transform at close time.
    pub transform: Transform3D,
    pub closed_at_ms: u64,
}

/// A reopen in progress: the app was relaunched, waiting for its
/// toplevel to map so the saved transform and workspace can be applied.
#[derive(Debug, Clone)]
pub struct PendingReopen {
    pub app_id: String,
    pub transform: Transform3D,
    pub workspace: usize,
}

/// Bounded history of closed windows, most recent at the end.
#[derive(Debug)]
pub struct ClosedWindowHistory {
    entries: Vec<ClosedWindow>,
    cap: usize,
}

impl ClosedWindowHistory {
    pub fn new(cap: usize) -> Self {
        ClosedWindowHistory { entries: Vec::new(), cap: cap.max(1) }
    }

    /// Record a closed window, trimming to the capacity (oldest dropped).
    pub fn record(&mut self, window: ClosedWindow) {
        self.entries.push(window);
        while self.entries.len() > self.cap {
            self.entries.remove(0);
        }
    }

    /// Most recently closed window.
    pub fn most_recent(&self) -> Option<&ClosedWindow> {
        self.entries.last()
    }

    /// Remove and return the most recently closed window.
    pub fn take_most_recent(&mut self) -> Option<ClosedWindow> {
        self.entries.pop()
    }

    /// Most recently closed window for the given app id, if any.
    pub fn most_recent_for_app(&self, app_id: &str) -> Option<&ClosedWindow> {
        self.entries
            .iter()
            .rev()
            .find(|w| w.app_id == app_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Determine whether a closed window's app id corresponds to a launcher
/// entry so it can be relaunched. The launcher derives entry app ids from
/// the Exec field's first token (which may be a full path). Matching is
/// case-insensitive against the entry name, the exec basename, or the
/// last dot-separated segment of the basename (e.g. window app_id
/// "firefox" for desktop Exec "/usr/lib/firefox/firefox").
pub fn app_id_matches_entry(app_id: &str, entry_app_id: &str, entry_name: &str) -> bool {
    if app_id.is_empty() || entry_app_id.is_empty() {
        return false;
    }
    let app_lower = app_id.to_lowercase();
    if !entry_name.is_empty() && app_lower == entry_name.to_lowercase() {
        return true;
    }
    let owned = entry_app_id.to_lowercase();
    let base = owned.rsplit('/').next().unwrap_or("");
    base == app_lower.as_str()
        || base.rsplit('.').next() == Some(app_lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgmath::Quaternion;

    fn sample(app_id: &str, workspace: usize) -> ClosedWindow {
        ClosedWindow {
            app_id: app_id.to_string(),
            title: "window".to_string(),
            workspace,
            transform: Transform3D {
                position: cgmath::Vector3::new(1.0, 2.0, 3.0),
                rotation: Quaternion::new(1.0, 0.0, 0.0, 0.0),
                scale: cgmath::Vector3::new(1.0, 1.0, 1.0),
            },
            closed_at_ms: 42,
        }
    }

    #[test]
    fn history_is_lifo() {
        let mut h = ClosedWindowHistory::new(5);
        h.record(sample("foot", 0));
        h.record(sample("gedit", 1));
        assert_eq!(h.len(), 2);
        assert_eq!(h.most_recent().unwrap().app_id, "gedit");
        let taken = h.take_most_recent().unwrap();
        assert_eq!(taken.app_id, "gedit");
        assert_eq!(taken.workspace, 1);
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn history_trims_to_capacity() {
        let mut h = ClosedWindowHistory::new(2);
        h.record(sample("a", 0));
        h.record(sample("b", 0));
        h.record(sample("c", 0));
        assert_eq!(h.len(), 2);
        assert_eq!(h.most_recent().unwrap().app_id, "c");
        // oldest ("a") was dropped
        assert!(h.most_recent_for_app("a").is_none());
    }

    #[test]
    fn most_recent_for_app_skips_other_apps() {
        let mut h = ClosedWindowHistory::new(5);
        h.record(sample("foot", 2));
        h.record(sample("gedit", 0));
        h.record(sample("foot", 1));
        let m = h.most_recent_for_app("foot").unwrap();
        assert_eq!(m.workspace, 1);
        assert!(h.most_recent_for_app("nonexistent").is_none());
    }

    #[test]
    fn app_id_matching_rules() {
        assert!(app_id_matches_entry("foot", "foot", "Foot"));
        assert!(app_id_matches_entry("Foot", "foot", "foot terminal"));
        assert!(app_id_matches_entry("firefox", "/usr/lib/firefox/firefox", "Firefox"));
        assert!(app_id_matches_entry("firefox", "org.mozilla.firefox", "Firefox"));
        // substring suffixes must NOT match
        assert!(!app_id_matches_entry("fox", "firefox", "Firefox"));
        assert!(!app_id_matches_entry("", "foot", "Foot"));
        assert!(!app_id_matches_entry("gedit", "other", "Other"));
    }
}
