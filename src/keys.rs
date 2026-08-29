/// XKB keycodes (Linux evdev + 8) for compositor shortcuts.
///
/// The winit backend delivers keycodes in XKB format (evdev + 8).
/// All compositor shortcut bindings MUST use these XKB constants.
///
/// Physical key → evdev → XKB (= evdev + 8)
#[allow(dead_code)]

// Modifiers
pub const CTRL_L: u32 = 37;   // evdev 29
pub const CTRL_R: u32 = 105;  // evdev 97
pub const SHIFT_L: u32 = 50;  // evdev 42
pub const SHIFT_R: u32 = 62;  // evdev 54
pub const ALT_L: u32 = 64;    // evdev 56
pub const ALT_R: u32 = 108;   // evdev 100
pub const META_L: u32 = 133;  // evdev 125
pub const META_R: u32 = 134;  // evdev 126

// Function keys
pub const F1: u32 = 67;
pub const F2: u32 = 68;
pub const F3: u32 = 69;
pub const F5: u32 = 71;
pub const F6: u32 = 72;

// Navigation
pub const TAB: u32 = 23;
pub const ENTER: u32 = 36;
pub const ESCAPE: u32 = 9;
pub const SPACE: u32 = 65;
pub const BACKSPACE: u32 = 22;
pub const UP: u32 = 111;
pub const DOWN: u32 = 116;
pub const LEFT: u32 = 113;
pub const RIGHT: u32 = 114;
pub const HOME: u32 = 110;
pub const MENU: u32 = 135;

// Number row
pub const K1: u32 = 10;
pub const K2: u32 = 11;
pub const K3: u32 = 12;
pub const K4: u32 = 13;
pub const K5: u32 = 14;
pub const K6: u32 = 15;
pub const K7: u32 = 16;
pub const K8: u32 = 17;
pub const K9: u32 = 18;
pub const K0: u32 = 19;

// Letters used in shortcuts
pub const A: u32 = 38;
pub const D: u32 = 40;
pub const F: u32 = 41;
pub const M: u32 = 66;
pub const O: u32 = 32;
pub const P: u32 = 33;
pub const Q: u32 = 24;
pub const T: u32 = 28;
pub const W: u32 = 25;
pub const SLASH: u32 = 61;
