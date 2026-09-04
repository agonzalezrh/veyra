//! Window decoration chrome (J3): title text fitting, title-bar button
//! layout and hit-testing.
//!
//! The decoration is part of the window's own quad (title strip = top
//! `title_frac` of the decorated height), so it automatically shares
//! the Visual/world transform with the client surface. This module is
//! the pure math on top: where buttons sit in title-strip UV space and
//! which button (if any) a click hits. Button actions are dispatched
//! by the compositor through the SAME handlers the context menu uses
//! (begin_minimize / toggle_maximize_for / toplevel send_close) — no
//! second semantics path.

/// The three title-bar buttons, right-aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleButton {
    Minimize,
    Maximize,
    Close,
}

impl TitleButton {
    /// Atlas code for the glyph: '-' (ASCII 45), the custom hollow-box
    /// glyph appended to the font atlas (code 128), 'x' (ASCII 120).
    pub fn glyph_code(self) -> u32 {
        match self {
            TitleButton::Minimize => 45,
            TitleButton::Maximize => 128,
            TitleButton::Close => 120,
        }
    }

    /// Label for log lines.
    pub fn name(self) -> &'static str {
        match self {
            TitleButton::Minimize => "minimize",
            TitleButton::Maximize => "maximize",
            TitleButton::Close => "close",
        }
    }
}

/// A button's hit zone in title-strip UV space (u range; v spans the
/// whole strip).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonZone {
    pub button: TitleButton,
    pub u_lo: f32,
    pub u_hi: f32,
}

/// Button geometry for a window whose decorated quad is `gw` wide and
/// `gh` tall (world units), with the title strip occupying the top
/// `title_frac` of the quad height.
///
/// Buttons are square (`side_px`), sized from the strip height, laid
/// out right-aligned with a margin and a gap — all in window pixels,
/// converted to UV-x by dividing by `gw`.
#[derive(Debug, Clone, Copy)]
pub struct ButtonLayout {
    /// Button side in UV-x units.
    pub side_u: f32,
    /// Right margin in UV-x units.
    pub margin_u: f32,
    /// Gap between buttons in UV-x units.
    pub gap_u: f32,
}

impl ButtonLayout {
    pub fn for_window(gw: f32, gh: f32, title_frac: f32) -> Self {
        let strip_px = title_frac * gh.max(1.0);
        let side_px = (strip_px * 0.72).max(6.0);
        let gap_px = side_px * 0.30;
        let margin_px = side_px * 0.40;
        ButtonLayout {
            side_u: side_px / gw.max(1.0),
            margin_u: margin_px / gw.max(1.0),
            gap_u: gap_px / gw.max(1.0),
        }
    }

    /// The three zones, right to left: Close, Maximize, Minimize.
    pub fn zones(&self) -> [ButtonZone; 3] {
        let b = self.side_u;
        let g = self.gap_u;
        let m = self.margin_u;
        [
            ButtonZone {
                button: TitleButton::Close,
                u_lo: 1.0 - m - b,
                u_hi: 1.0 - m,
            },
            ButtonZone {
                button: TitleButton::Maximize,
                u_lo: 1.0 - m - 2.0 * b - g,
                u_hi: 1.0 - m - b - g,
            },
            ButtonZone {
                button: TitleButton::Minimize,
                u_lo: 1.0 - m - 3.0 * b - 2.0 * g,
                u_hi: 1.0 - m - 2.0 * b - 2.0 * g,
            },
        ]
    }

    /// Button glyph center positions in UV space, matching `zones`
    /// (used by the renderer to place the glyphs).
    pub fn centers(&self) -> [(TitleButton, f32); 3] {
        self.zones().map(|z| (z.button, (z.u_lo + z.u_hi) * 0.5))
    }
}

/// Which button (if any) does a title-bar hit at UV (u, v) land on?
/// `v` is the full-quad UV (0 = top edge), `title_frac` the strip
/// height in the same units.
pub fn hit_button(
    gw: f32,
    gh: f32,
    title_frac: f32,
    u: f64,
    v: f64,
) -> Option<TitleButton> {
    if v < 0.0 || v as f32 > title_frac {
        return None;
    }
    let layout = ButtonLayout::for_window(gw, gh, title_frac);
    layout
        .zones()
        .iter()
        .find(|z| (u as f32) >= z.u_lo && (u as f32) <= z.u_hi)
        .map(|z| z.button)
}

/// Truncate a window title so it fits the given pixel budget, using
/// ".." as the ellipsis. Never returns a title wider than the budget
/// (measured in glyph cells: each char is 5/7 of the char height).
pub fn fit_title(title: &str, avail_px: f32, char_h_px: f32) -> String {
    let char_w = char_h_px * 5.0 / 7.0;
    let fits = |s: &str| (s.chars().count() as f32) * char_w <= avail_px;
    if fits(title) {
        return title.to_string();
    }
    let ellipsis = "..";
    // Drop characters from the end until title + ".." fits (at least
    // the ellipsis itself must fit).
    let chars: Vec<char> = title.chars().collect();
    let mut end = chars.len();
    while end > 0 {
        let candidate: String = chars[..end].iter().collect();
        if fits(&format!("{}{}", candidate, ellipsis)) {
            return format!("{}{}", candidate, ellipsis);
        }
        end -= 1;
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 640×480 client with the 6% title bar: decorated height 508.8,
    /// strip ≈ 28.8 px, button side ≈ 20.7 px.
    fn sample() -> (f32, f32, f32) {
        (640.0, 508.8, 0.06 / 1.06)
    }

    #[test]
    fn zones_are_ordered_and_disjoint() {
        let (gw, gh, tf) = sample();
        let layout = ButtonLayout::for_window(gw, gh, tf);
        let [close, max, min] = layout.zones();
        // Right to left: close is rightmost, then maximize, then minimize.
        assert!(close.u_hi > close.u_lo);
        assert!(max.u_hi < close.u_lo);
        assert!(min.u_hi < max.u_lo);
        assert!(min.u_lo > 0.0);
        // All zones sit inside the quad.
        assert!(close.u_hi <= 1.0);
        // Side is the strip height scaled, converted to u by gw.
        let strip_px = tf * gh;
        assert!((layout.side_u - strip_px * 0.72 / gw).abs() < 1e-5);
    }

    #[test]
    fn hit_hits_each_button_center() {
        let (gw, gh, tf) = sample();
        let layout = ButtonLayout::for_window(gw, gh, tf);
        for z in layout.zones() {
            let mid = ((z.u_lo + z.u_hi) / 2.0) as f64;
            assert_eq!(
                hit_button(gw, gh, tf, mid, (tf * 0.5) as f64),
                Some(z.button),
                "center of {:?} zone must hit it",
                z.button
            );
        }
    }

    #[test]
    fn hit_outside_strip_is_none() {
        let (gw, gh, tf) = sample();
        // Below the strip (content area).
        assert_eq!(hit_button(gw, gh, tf, 0.97, (tf + 0.01) as f64), None);
        // Above the quad (v < 0).
        assert_eq!(hit_button(gw, gh, tf, 0.97, -0.01), None);
        // Left of all buttons.
        assert_eq!(hit_button(gw, gh, tf, 0.5, (tf * 0.5) as f64), None);
    }

    #[test]
    fn hit_gaps_return_none() {
        let (gw, gh, tf) = sample();
        let layout = ButtonLayout::for_window(gw, gh, tf);
        let [close, max, _] = layout.zones();
        // Midpoint of the gap between close and maximize.
        let gap_mid = ((close.u_lo + max.u_hi) / 2.0) as f64;
        assert_eq!(hit_button(gw, gh, tf, gap_mid, (tf * 0.5) as f64), None);
    }

    #[test]
    fn fit_title_passes_short_titles() {
        assert_eq!(fit_title("term", 500.0, 20.0), "term");
    }

    #[test]
    fn fit_title_truncates_with_ellipsis() {
        let t = "a very long window title that will not fit";
        let out = fit_title(t, 100.0, 20.0);
        // 100px / (20*5/7) = 7 cells: 5 chars + ".."
        assert!(out.ends_with(".."));
        assert!(out.len() <= 5 + 2);
        assert!(out.starts_with("a ver"));
    }

    #[test]
    fn fit_title_empty_budget() {
        let out = fit_title("title", 0.0, 20.0);
        assert_eq!(out, "");
    }

    #[test]
    fn glyph_codes_match_atlas() {
        assert_eq!(TitleButton::Minimize.glyph_code(), 45); // '-'
        assert_eq!(TitleButton::Maximize.glyph_code(), 128); // custom box
        assert_eq!(TitleButton::Close.glyph_code(), 120); // 'x'
    }
}
