//! Desktop shell (J4): the 2D taskbar plane over the 3D desktop.
//!
//! The shell is presentation + input routing, NOT scene state: it owns
//! no windows, no surfaces, no transforms. The model below is rebuilt
//! each frame from compositor state (toplevels, workspaces, focus
//! history, launcher), the renderer draws it as a screen-space overlay
//! (camera-independent — the shell never participates in the 3D world),
//! and clicks route through the SAME coordinators everything else uses
//! (set_keyboard_focus, begin_minimize, restore_minimized,
//! switch_workspace, launcher).

use crate::scene::VisualId;

/// What a click on a taskbar region means. The compositor maps these
/// to existing coordinators.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskbarHit {
    /// Activate (or minimize) a window button.
    Window(VisualId),
    /// Switch to workspace `usize`.
    Workspace(usize),
    /// Launch the application at launcher index `usize`.
    Launch(usize),
}

/// One clickable region of the bar, in screen pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskbarItem {
    pub hit: TaskbarHit,
    pub x: f32,
    pub w: f32,
    pub label: String,
    /// Visual state for drawing: focused window / active workspace.
    pub active: bool,
    /// Dimmed (minimized window).
    pub dim: bool,
}

/// The full taskbar layout for one frame: bar geometry + items.
#[derive(Debug, Clone, Default)]
pub struct TaskbarLayout {
    pub bar_h: f32,
    pub items: Vec<TaskbarItem>,
}

const WS_BTN_W: f32 = 30.0;
const WS_BTN_GAP: f32 = 2.0;
const WS_ZONE_LEFT: f32 = 6.0;
const LAUNCH_BTN_W: f32 = 104.0;
const LAUNCH_GAP: f32 = 4.0;
const WIN_BTN_MAX_W: f32 = 170.0;
const WIN_BTN_MIN_W: f32 = 84.0;
const WIN_BTN_GAP: f32 = 4.0;
const SECTION_PAD: f32 = 8.0;

impl TaskbarLayout {
    /// Bar height for a framebuffer height (DPI-proportional, same
    /// scaling family as MenuMetrics).
    pub fn bar_height(fb_h: f32) -> f32 {
        let sv = (fb_h / 720.0).clamp(1.0, 2.5);
        (36.0 * sv).round()
    }

    /// Build the frame's layout.
    ///
    /// `windows`: (vid, label, focused, minimized) in DISPLAY order —
    /// most recently focused first (the compositor passes MRU order).
    /// `launches`: (launcher_index, label) to pin on the right.
    pub fn build(
        fb_w: f32,
        fb_h: f32,
        windows: &[(VisualId, String, bool, bool)],
        ws_count: usize,
        ws_active: usize,
        launches: &[(usize, String)],
    ) -> Self {
        let bar_h = Self::bar_height(fb_h);
        let mut items = Vec::new();

        // ── Left section: workspace buttons ──
        let mut cursor = WS_ZONE_LEFT;
        for i in 0..ws_count {
            items.push(TaskbarItem {
                hit: TaskbarHit::Workspace(i),
                x: cursor,
                w: WS_BTN_W - WS_BTN_GAP,
                label: (i + 1).to_string(),
                active: i == ws_active,
                dim: false,
            });
            cursor += WS_BTN_W;
        }

        // ── Right section: pinned launcher entries ──
        let mut r_cursor = fb_w - SECTION_PAD;
        let mut launch_items = Vec::new();
        for (idx, label) in launches.iter().rev() {
            let x = r_cursor - LAUNCH_BTN_W;
            if x < cursor + SECTION_PAD {
                break; // no room left for more launcher pins
            }
            launch_items.push(TaskbarItem {
                hit: TaskbarHit::Launch(*idx),
                x,
                w: LAUNCH_BTN_W - LAUNCH_GAP,
                label: crate::chrome::fit_title(label, LAUNCH_BTN_W - LAUNCH_GAP - 10.0, 13.0),
                active: false,
                dim: false,
            });
            r_cursor = x - LAUNCH_GAP;
        }
        items.extend(launch_items);

        // ── Middle section: window buttons, MRU order, left to right ──
        let win_zone_l = cursor + SECTION_PAD;
        let win_zone_r = r_cursor - LAUNCH_GAP - SECTION_PAD;
        let zone_w = (win_zone_r - win_zone_l).max(0.0);
        let n = windows.len() as f32;
        let mut w_cursor = win_zone_l;
        for (i, (vid, label, focused, minimized)) in windows.iter().enumerate() {
            // Fair share of the zone, clamped to [min, max].
            let share = ((zone_w - WIN_BTN_GAP * (n - 1.0).max(0.0)) / n)
                .clamp(WIN_BTN_MIN_W, WIN_BTN_MAX_W);
            if w_cursor + share > win_zone_r {
                break; // out of room; remaining windows are not shown
            }
            items.push(TaskbarItem {
                hit: TaskbarHit::Window(*vid),
                x: w_cursor,
                w: share,
                label: crate::chrome::fit_title(label, share - 12.0, 13.0),
                active: *focused,
                dim: *minimized,
            });
            w_cursor += share + WIN_BTN_GAP;
            let _ = i;
        }

        TaskbarLayout { bar_h, items }
    }

    pub fn bar_top(&self, fb_h: f32) -> f32 {
        fb_h - self.bar_h
    }

    /// Hit-test a pointer position (screen px, y down).
    pub fn hit(&self, fb_h: f32, x: f64, y: f64) -> Option<&TaskbarItem> {
        let top = self.bar_top(fb_h);
        if (y as f32) < top {
            return None;
        }
        self.items
            .iter()
            .find(|it| x as f32 >= it.x && x as f32 <= it.x + it.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(n: u64) -> VisualId {
        VisualId(n)
    }

    #[test]
    fn bar_geometry_is_bottom_strip() {
        let l = TaskbarLayout::build(1280.0, 720.0, &[], 3, 0, &[]);
        assert_eq!(l.bar_h, 36.0);
        assert_eq!(l.bar_top(720.0), 684.0);
    }

    #[test]
    fn workspace_buttons_left_aligned() {
        let l = TaskbarLayout::build(1280.0, 720.0, &[], 3, 1, &[]);
        let ws: Vec<_> = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Workspace(_)))
            .collect();
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].x, WS_ZONE_LEFT);
        assert_eq!(ws[1].x, WS_ZONE_LEFT + WS_BTN_W);
        assert!(ws[1].active);
        assert!(!ws[0].active);
    }

    #[test]
    fn window_buttons_mru_order_left_to_right() {
        let wins = [
            (vid(2), "B".to_string(), true, false),
            (vid(1), "A".to_string(), false, false),
        ];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 2, 0, &[]);
        let wins_items: Vec<_> = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Window(_)))
            .collect();
        assert_eq!(wins_items.len(), 2);
        assert_eq!(wins_items[0].hit, TaskbarHit::Window(vid(2)));
        assert!(wins_items[0].active, "focused window highlighted");
        assert!(wins_items[0].x < wins_items[1].x, "MRU first (leftmost)");
        assert!(wins_items[0].w <= WIN_BTN_MAX_W + 0.5);
    }

    #[test]
    fn launcher_pins_right_aligned() {
        let launches = [(0, "Foot".to_string()), (1, "Weston Terminal".to_string())];
        let l = TaskbarLayout::build(1280.0, 720.0, &[], 2, 0, &launches);
        let mut ls: Vec<_> = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Launch(_)))
            .collect();
        assert_eq!(ls.len(), 2);
        // Items are built right-to-left; sort for display order.
        ls.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());
        assert!(ls[0].x < ls[1].x);
        let rightmost = ls.iter().map(|it| it.x + it.w).fold(0.0f32, f32::max);
        // Right edge sits one gap inside the section pad (w excludes it).
        assert!((rightmost - (1280.0 - SECTION_PAD - LAUNCH_GAP)).abs() < 1.0);
    }

    #[test]
    fn hit_resolves_regions() {
        let wins = [(vid(1), "A".to_string(), false, false)];
        let launches = [(3, "Foot".to_string())];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 2, 0, &launches);
        let top = l.bar_top(720.0);
        // Above the bar: no hit.
        assert!(l.hit(720.0, 640.0, (top - 5.0) as f64).is_none());
        // Workspace button 0.
        let ws0 = l.items.iter().find(|it| matches!(it.hit, TaskbarHit::Workspace(0))).unwrap();
        let h = l.hit(720.0, (ws0.x + 5.0) as f64, (top + 10.0) as f64).unwrap();
        assert_eq!(h.hit, TaskbarHit::Workspace(0));
        // Window button.
        let win = l.items.iter().find(|it| matches!(it.hit, TaskbarHit::Window(_))).unwrap();
        let h = l.hit(720.0, (win.x + win.w / 2.0) as f64, (top + 10.0) as f64).unwrap();
        assert_eq!(h.hit, TaskbarHit::Window(vid(1)));
        // Launcher pin.
        let ln = l.items.iter().find(|it| matches!(it.hit, TaskbarHit::Launch(_))).unwrap();
        let h = l.hit(720.0, (ln.x + 10.0) as f64, (top + 10.0) as f64).unwrap();
        assert_eq!(h.hit, TaskbarHit::Launch(3));
    }

    #[test]
    fn minimized_window_is_dimmed() {
        let wins = [(vid(1), "A".to_string(), false, true)];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 1, 0, &[]);
        let win = l.items.iter().find(|it| matches!(it.hit, TaskbarHit::Window(_))).unwrap();
        assert!(win.dim);
        assert!(!win.active);
    }

    #[test]
    fn many_windows_shrink_but_never_overlap_launcher() {
        let wins: Vec<(VisualId, String, bool, bool)> = (0..12u64)
            .map(|i| (vid(i + 1), format!("Window {}", i), i == 0, false))
            .collect();
        let launches = [(0, "App".to_string())];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 2, 0, &launches);
        let wins_items: Vec<_> = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Window(_)))
            .collect();
        // Some windows may be hidden for room, but shown ones never
        // cross into the launcher zone.
        let launch_left = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Launch(_)))
            .map(|it| it.x)
            .fold(f32::MAX, f32::min);
        for w in &wins_items {
            assert!(w.x + w.w <= launch_left, "window button overlaps launcher");
            assert!(w.w >= WIN_BTN_MIN_W - 0.5, "window button too narrow: {}", w.w);
        }
    }
}
