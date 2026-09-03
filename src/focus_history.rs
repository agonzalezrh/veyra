//! Application focus history / MRU (J1).
//!
//! Owns the most-recently-focused ordering of application windows —
//! the data model behind Alt+Tab and focus replacement. Protocol- and
//! camera-free: this is window-management state only.
//!
//! Invariants:
//! - MRU is updated ONLY on actual focus transitions (mind: not on
//!   every input event, not on mere pointer hover).
//! - A window that is not focusable (destroyed, minimized, a popup, or
//!   on another workspace when cycling) never becomes the focus
//!   replacement candidate.
//! - Popups never enter the application MRU: registration happens on
//!   focus transitions of real toplevels only.
//!
//! `focused` itself remains in the Scene (single source of truth for
//! presentation chrome); this module is the ordering policy.

use crate::scene::VisualId;

/// Focusability predicate the compositor supplies at query time:
/// live, visible (= not minimized), on one of the workspaces being
/// cycled, not the excluded window itself.
pub type Focusable<'a> = dyn Fn(VisualId) -> bool + 'a;

/// Why a focus is changing is recorded in compositor log lines, not in
/// the model — only transitions matter here, not their source.
///
/// Most-recently-focused ordering. Most recent FIRST.
#[derive(Debug, Default, Clone)]
pub struct FocusHistory {
    order: Vec<VisualId>,
}

impl FocusHistory {
    pub fn new() -> Self {
        FocusHistory { order: Vec::new() }
    }

    /// Record a focus transition: `vid` becomes most recent.
    pub fn touch(&mut self, vid: VisualId) {
        self.order.retain(|v| *v != vid);
        self.order.insert(0, vid);
    }

    /// Drop a window from the history entirely (destroy / external).
    pub fn remove(&mut self, vid: VisualId) {
        self.order.retain(|v| *v != vid);
    }

    /// Most recent entries, most recent first.
    pub fn order(&self) -> &[VisualId] {
        &self.order
    }

    /// Next candidate after `current` in MRU order (wraps), skipping
    /// everything `focusable` rejects. Used for Alt+Tab / Super+Tab.
    pub fn next_after(
        &self,
        current: Option<VisualId>,
        focusable: &Focusable,
    ) -> Option<VisualId> {
        self.cycle_from(current, 1, focusable)
    }

    /// Previous candidate before `current` in MRU order (wraps).
    pub fn previous_before(
        &self,
        current: Option<VisualId>,
        focusable: &Focusable,
    ) -> Option<VisualId> {
        self.cycle_from(current, -1, focusable)
    }

    /// Best replacement for the CURRENTLY FOCUSED window (it just
    /// closed or minimized): the most recent OTHER focusable window.
    pub fn focus_replacement(
        &self,
        excluded: Option<VisualId>,
        focusable: &Focusable,
    ) -> Option<VisualId> {
        self.cycle_from(excluded, 1, focusable)
    }

    fn cycle_from(
        &self,
        current: Option<VisualId>,
        dir: isize,
        focusable: &Focusable,
    ) -> Option<VisualId> {
        if self.order.is_empty() {
            return None;
        }
        // Rotation start: the entry after `current` (or the most recent
        // window when current is gone from the list).
        let start = match current {
            Some(c) if self.order.contains(&c) => {
                let pos = self.order.iter().position(|v| *v == c).unwrap_or(0);
                let len = self.order.len() as isize;
                let next = (pos as isize + dir).rem_euclid(len);
                next as usize
            }
            _ => {
                // current missing: start scanning from the front.
                if dir >= 0 { 0 } else { self.order.len() - 1 }
            }
        };
        for step in 0..self.order.len() {
            let idx = (start + (step as isize * dir).rem_euclid(self.order.len() as isize) as usize)
                % self.order.len();
            let cand = self.order[idx];
            if Some(cand) == current {
                continue;
            }
            if focusable(cand) {
                return Some(cand);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The acceptance sequence from the J1 brief, end to end:
    ///
    /// A → B → C (mapped+focused in order)
    /// focus B → MRU = B, C, A
    /// focus A → MRU = A, B, C
    /// minimize A → focused = B, MRU = B, C
    /// restore A, focus A → MRU = A, B, C
    /// destroy B → MRU = A, C
    #[test]
    fn j1_acceptance_sequence() {
        let a = VisualId(1);
        let b = VisualId(2);
        let c = VisualId(3);
        let mut mru = FocusHistory::new();
        let minimized: RefCell<Vec<VisualId>> = RefCell::new(Vec::new());
        let focusable = |v: VisualId| !minimized.borrow().contains(&v);

        // A → B → C: three focus transitions in creation order
        mru.touch(a);
        mru.touch(b);
        mru.touch(c);
        assert_eq!(mru.order(), &[c, b, a]);

        // focus B
        mru.touch(b);
        assert_eq!(mru.order(), &[b, c, a]);

        // focus A
        mru.touch(a);
        assert_eq!(mru.order(), &[a, b, c]);

        // minimize A: focused replacement must be B (most recent other).
        // Same removal discipline as compositor begin_minimize: drop the
        // minimized window from the MRU immediately.
        minimized.borrow_mut().push(a);
        mru.remove(a);
        assert_eq!(mru.order(), &[b, c]);
        let replacement = mru.focus_replacement(Some(a), &focusable);
        assert_eq!(replacement, Some(b));

        // restore A + focus A
        minimized.borrow_mut().retain(|v| *v != a);
        mru.touch(a);
        assert_eq!(mru.order(), &[a, b, c]);

        // destroy B
        mru.remove(b);
        assert_eq!(mru.order(), &[a, c]);
    }

    #[test]
    fn alt_tab_wraps_and_skips_minimized() {
        let a = VisualId(1);
        let b = VisualId(2);
        let c = VisualId(3);
        let mut mru = FocusHistory::new();
        mru.touch(a);
        mru.touch(b);
        mru.touch(c); // MRU = [c, b, a]; focused = c

        // next after c = b
        assert_eq!(mru.next_after(Some(c), &|_v| true), Some(b));
        // previous before c = a
        assert_eq!(mru.previous_before(Some(c), &|_v| true), Some(a));

        // minimize b: Alt+Tab from c skips b and lands on a
        let focusable = |v: VisualId| v != b;
        assert_eq!(mru.next_after(Some(c), &focusable), Some(a));
    }

    #[test]
    fn popups_and_externals_are_not_registered() {
        // Registration is the compositor's job (only toplevels get
        // touched); this documents the guarantee: a vid never touched
        // can never be returned.
        let mru = FocusHistory::new();
        assert!(mru.next_after(Some(VisualId(9)), &|_v| true).is_none());
        assert!(mru.focus_replacement(None, &|_v| true).is_none());
    }

    #[test]
    fn stale_current_falls_back_to_extremes() {
        let a = VisualId(1);
        let b = VisualId(2);
        let c = VisualId(3);
        let mut mru = FocusHistory::new();
        mru.touch(a);
        mru.touch(b);
        mru.touch(c);
        // current already destroyed: next scans from the most recent,
        // previous scans from the least recent (the anchor is gone, so
        // both degrade to deterministic extremes of the MRU order).
        let gone = VisualId(99);
        assert_eq!(mru.next_after(Some(gone), &|_v| true), Some(c));
        assert_eq!(mru.previous_before(Some(gone), &|_v| true), Some(a));
    }

    #[test]
    fn single_window_cycles_to_none() {
        let mut mru = FocusHistory::new();
        let a = VisualId(1);
        mru.touch(a);
        // Nothing else to focus: cycling keeps the window focused, no
        // bogus wrap to itself is returned.
        assert_eq!(mru.next_after(Some(a), &|_v| true), None);
        assert_eq!(mru.previous_before(Some(a), &|_v| true), None);
    }

    #[test]
    fn replacement_excludes_the_dead_window() {
        let a = VisualId(1);
        let b = VisualId(2);
        let mut mru = FocusHistory::new();
        mru.touch(a);
        mru.touch(b); // focused = b, MRU = [b, a]
        // b destroyed: replacement for b is a, never b itself
        assert_eq!(mru.focus_replacement(Some(b), &|_v| true), Some(a));
    }
}
