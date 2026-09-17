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
    /// Pointer is over this button right now (draw state only).
    pub hover: bool,
}

/// The full taskbar layout for one frame: bar geometry + items.
#[derive(Debug, Clone, Default)]
pub struct TaskbarLayout {
    pub bar_h: f32,
    pub items: Vec<TaskbarItem>,
    /// X of the hairline separator between the workspace zone and the
    /// window zone (None when the two zones touch).
    pub sep_ws: Option<f32>,
    /// X of the hairline separator between the window zone and the
    /// launcher zone.
    pub sep_launch: Option<f32>,
}

const WS_BTN_W: f32 = 34.0;
const WS_BTN_GAP: f32 = 4.0;
const WS_ZONE_LEFT: f32 = 8.0;
const LAUNCH_BTN_W: f32 = 104.0;
const LAUNCH_GAP: f32 = 4.0;
const WIN_BTN_MAX_W: f32 = 170.0;
const WIN_BTN_MIN_W: f32 = 84.0;
const WIN_BTN_GAP: f32 = 5.0;
const SECTION_PAD: f32 = 10.0;

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
    /// `hover`: current pointer position in screen px (draw state).
    pub fn build(
        fb_w: f32,
        fb_h: f32,
        windows: &[(VisualId, String, bool, bool)],
        ws_count: usize,
        ws_active: usize,
        launches: &[(usize, String)],
        hover: Option<(f64, f64)>,
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
                hover: Self::hovered(hover, cursor, bar_h, WS_BTN_W - WS_BTN_GAP),
            });
            cursor += WS_BTN_W;
        }
        let ws_zone_end = cursor;

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
                hover: Self::hovered(hover, x, bar_h, LAUNCH_BTN_W - LAUNCH_GAP),
            });
            r_cursor = x - LAUNCH_GAP;
        }
        let launch_zone_start = launch_items
            .last()
            .map(|it| it.x - LAUNCH_GAP)
            .unwrap_or(fb_w - SECTION_PAD);
        items.extend(launch_items);

        // ── Middle section: window buttons, MRU order, left to right ──
        let win_zone_l = cursor + SECTION_PAD;
        let win_zone_r = r_cursor - LAUNCH_GAP - SECTION_PAD;
        let zone_w = (win_zone_r - win_zone_l).max(0.0);
        let n = windows.len() as f32;
        let mut w_cursor = win_zone_l;
        for (vid, label, focused, minimized) in windows.iter() {
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
                hover: Self::hovered(hover, w_cursor, bar_h, share),
            });
            w_cursor += share + WIN_BTN_GAP;
        }

        // Separators only where the zones actually have neighbors.
        let sep_ws = if ws_count > 0 && (!windows.is_empty() || !launches.is_empty()) {
            Some(ws_zone_end + (SECTION_PAD - WS_BTN_GAP) * 0.5)
        } else {
            None
        };
        let sep_launch = if !launches.is_empty() {
            Some(launch_zone_start - SECTION_PAD * 0.5)
        } else {
            None
        };

        TaskbarLayout {
            bar_h,
            items,
            sep_ws,
            sep_launch,
        }
    }

    /// Pointer-over test for one button. `hover` is bar-scoped: the
    /// caller only passes a position when the pointer is inside the bar
    /// strip, so this is an X-range check.
    fn hovered(hover: Option<(f64, f64)>, x: f32, _bar_h: f32, w: f32) -> bool {
        match hover {
            Some((hx, _)) => {
                let hx = hx as f32;
                hx >= x && hx <= x + w
            }
            None => false,
        }
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
        let l = TaskbarLayout::build(1280.0, 720.0, &[], 3, 0, &[], None);
        assert_eq!(l.bar_h, 36.0);
        assert_eq!(l.bar_top(720.0), 684.0);
    }

    #[test]
    fn workspace_buttons_left_aligned() {
        let l = TaskbarLayout::build(1280.0, 720.0, &[], 3, 1, &[], None);
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
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 2, 0, &[], None);
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
        let l = TaskbarLayout::build(1280.0, 720.0, &[], 2, 0, &launches, None);
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
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 2, 0, &launches, None);
        let top = l.bar_top(720.0);
        // Above the bar: no hit.
        assert!(l.hit(720.0, 640.0, (top - 5.0) as f64).is_none());
        // Workspace button 0.
        let ws0 = l
            .items
            .iter()
            .find(|it| matches!(it.hit, TaskbarHit::Workspace(0)))
            .unwrap();
        let h = l
            .hit(720.0, (ws0.x + 5.0) as f64, (top + 10.0) as f64)
            .unwrap();
        assert_eq!(h.hit, TaskbarHit::Workspace(0));
        // Window button.
        let win = l
            .items
            .iter()
            .find(|it| matches!(it.hit, TaskbarHit::Window(_)))
            .unwrap();
        let h = l
            .hit(720.0, (win.x + win.w / 2.0) as f64, (top + 10.0) as f64)
            .unwrap();
        assert_eq!(h.hit, TaskbarHit::Window(vid(1)));
        // Launcher pin.
        let ln = l
            .items
            .iter()
            .find(|it| matches!(it.hit, TaskbarHit::Launch(_)))
            .unwrap();
        let h = l
            .hit(720.0, (ln.x + 10.0) as f64, (top + 10.0) as f64)
            .unwrap();
        assert_eq!(h.hit, TaskbarHit::Launch(3));
    }

    #[test]
    fn minimized_window_is_dimmed() {
        let wins = [(vid(1), "A".to_string(), false, true)];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 1, 0, &[], None);
        let win = l
            .items
            .iter()
            .find(|it| matches!(it.hit, TaskbarHit::Window(_)))
            .unwrap();
        assert!(win.dim);
        assert!(!win.active);
    }

    #[test]
    fn hover_marks_only_the_button_under_the_pointer() {
        let wins = [(vid(1), "A".to_string(), false, false)];
        let launches = [(0, "Foot".to_string())];
        let probe = TaskbarLayout::build(1280.0, 720.0, &wins, 1, 0, &launches, None);
        let win = probe
            .items
            .iter()
            .find(|it| matches!(it.hit, TaskbarHit::Window(_)))
            .unwrap()
            .clone();
        // Pointer at the window button's center.
        let l = TaskbarLayout::build(
            1280.0,
            720.0,
            &wins,
            1,
            0,
            &launches,
            Some(((win.x + win.w / 2.0) as f64, 700.0)),
        );
        let hovered: Vec<bool> = l.items.iter().map(|it| it.hover).collect();
        assert_eq!(hovered.iter().filter(|h| **h).count(), 1);
        assert!(
            l.items
                .iter()
                .find(|it| matches!(it.hit, TaskbarHit::Window(_)))
                .unwrap()
                .hover
        );
        // Pointer elsewhere: nothing hovered.
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 1, 0, &launches, Some((640.0, 700.0)));
        assert!(l.items.iter().all(|it| !it.hover));
    }

    #[test]
    fn separators_sit_between_zones() {
        let wins = [(vid(1), "A".to_string(), false, false)];
        let launches = [(0, "Foot".to_string())];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 1, 0, &launches, None);
        let ws_right = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Workspace(_)))
            .map(|it| it.x + it.w)
            .fold(0.0f32, f32::max);
        let win_left = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Window(_)))
            .map(|it| it.x)
            .fold(f32::MAX, f32::min);
        let launch_left = l
            .items
            .iter()
            .filter(|it| matches!(it.hit, TaskbarHit::Launch(_)))
            .map(|it| it.x)
            .fold(f32::MAX, f32::min);
        let sep_ws = l.sep_ws.expect("ws separator present");
        let sep_launch = l.sep_launch.expect("launcher separator present");
        assert!(sep_ws > ws_right && sep_ws < win_left);
        assert!(sep_launch > ws_right && sep_launch < launch_left);
    }

    #[test]
    fn many_windows_shrink_but_never_overlap_launcher() {
        let wins: Vec<(VisualId, String, bool, bool)> = (0..12u64)
            .map(|i| (vid(i + 1), format!("Window {}", i), i == 0, false))
            .collect();
        let launches = [(0, "App".to_string())];
        let l = TaskbarLayout::build(1280.0, 720.0, &wins, 2, 0, &launches, None);
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
            assert!(
                w.w >= WIN_BTN_MIN_W - 0.5,
                "window button too narrow: {}",
                w.w
            );
        }
    }
}

// ── G-H1: first-run / empty-state hints ─────────────────────────────
//
// A small, dismissible card that answers a new user's first questions
// ("what am I looking at? how do I move?") without a tutorial or a
// modal. One-time-ever: dismissed by the first camera gesture, the
// first application opening, or a click anywhere on the card.

#[derive(Debug, Clone)]
pub struct HintLayout {
    /// Card rect in framebuffer px (x, y, w, h), y = top.
    pub rect: (f32, f32, f32, f32),
    pub lines: Vec<String>,
}

impl HintLayout {
    pub fn for_framebuffer(w: f32, h: f32) -> Self {
        let lines = vec![
            "Veyra — your desktop, in space".to_string(),
            String::new(),
            "Scroll · approach the desktop".to_string(),
            "Left-drag · move (pan the world)".to_string(),
            "Right-drag · look around".to_string(),
            "Click a window · focus it".to_string(),
            "Right-click a window · actions".to_string(),
            String::new(),
            "Open applications from the taskbar below".to_string(),
        ];
        let card_w = (w * 0.44).clamp(420.0, 640.0);
        let line_h = 24.0f32;
        let card_h = lines.len() as f32 * line_h + 36.0;
        let x = (w - card_w) * 0.5;
        let y = (h - card_h - 90.0).max(24.0); // above the taskbar strip
        HintLayout {
            rect: (x, y, card_w, card_h),
            lines,
        }
    }

    pub fn contains(&self, px: f64, py: f64) -> bool {
        let (x, y, w, h) = self.rect;
        px >= x as f64 && px <= (x + w) as f64 && py >= y as f64 && py <= (y + h) as f64
    }
}

/// One-time-ever dismissal flag: a tiny marker file in the user's
/// state directory.
pub fn hints_seen() -> bool {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        let p = std::path::PathBuf::from(dir).join("veyra/hints-seen");
        p.exists()
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home)
            .join(".local/state/veyra/hints-seen")
            .exists()
    } else {
        false
    }
}

pub fn mark_hints_seen() {
    let dir = match std::env::var("XDG_STATE_HOME") {
        Ok(d) => std::path::PathBuf::from(d).join("veyra"),
        Err(_) => match std::env::var("HOME") {
            Ok(h) => std::path::PathBuf::from(h).join(".local/state/veyra"),
            Err(_) => return,
        },
    };
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join("hints-seen"), b"1");
    }
}
