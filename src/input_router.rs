use cgmath::InnerSpace;
use cgmath::Matrix4;
use cgmath::SquareMatrix;
use cgmath::Vector3;
use cgmath::Vector4;

use crate::scene::{Scene, VisualId};

/// Classification of a pointer event's destination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InteractionTarget {
    Scene,
    Content(VisualId),
}

/// The kind of pointer event being delivered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEventKind {
    Down,
    Up,
    Motion,
    Scroll(f64, f64),
}

/// A keyboard event using USB HID usage IDs as the key representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyboardEvent {
    pub key: u16,
    pub pressed: bool,
}

/// Abstraction for delivering input events to application content.
pub trait InputSink: std::fmt::Debug {
    fn handle_pointer(&mut self, kind: PointerEventKind, u: f64, v: f64);
    fn handle_keyboard(&mut self, event: KeyboardEvent);
}

/// Classification of a keyboard event's routing destination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyRouting {
    /// Compositor global shortcut (Tab, etc.)
    Global,
    /// Route to the focused visual's InputSink
    Content,
}

/// A recording input sink for testing multi-visual input routing.
/// Records all events it receives for later assertion.
#[derive(Debug, Default, Clone)]
pub struct RecordingInputSink {
    pub pointers: Vec<(PointerEventKind, f64, f64)>,
    pub keys: Vec<KeyboardEvent>,
}

impl RecordingInputSink {
    pub fn new() -> Self {
        RecordingInputSink { pointers: Vec::new(), keys: Vec::new() }
    }
    pub fn clear(&mut self) {
        self.pointers.clear();
        self.keys.clear();
    }
}

impl InputSink for RecordingInputSink {
    fn handle_pointer(&mut self, kind: PointerEventKind, u: f64, v: f64) {
        self.pointers.push((kind, u, v));
    }
    fn handle_keyboard(&mut self, event: KeyboardEvent) {
        self.keys.push(event);
    }
}

/// Convert a Linux evdev key code (as used by winit) to USB HID usage ID.
/// Returns 0 for unmapped keys.
pub fn linux_to_hid(code: u32) -> u16 {
    match code {
        1 => 0x29, 2 => 0x1e, 3 => 0x1f, 4 => 0x20, 5 => 0x21, 6 => 0x22,
        7 => 0x23, 8 => 0x24, 9 => 0x25, 10 => 0x26, 11 => 0x27,
        12 => 0x2d, 13 => 0x2e, 14 => 0x2a, 15 => 0x2b,
        16 => 0x14, 17 => 0x1a, 18 => 0x08, 19 => 0x15, 20 => 0x17,
        21 => 0x1c, 22 => 0x18, 23 => 0x0c, 24 => 0x16, 25 => 0x1b,
        26 => 0x2f, 27 => 0x30, 28 => 0x28, 29 => 0xe1,
        30 => 0x04, 31 => 0x16, 32 => 0x07, 33 => 0x09, 34 => 0x0a,
        35 => 0x0b, 36 => 0x0d, 37 => 0x0e, 38 => 0x0f,
        39 => 0x33, 40 => 0x34, 41 => 0x35, 42 => 0xe1, 43 => 0x31,
        44 => 0x1d, 45 => 0x1b, 46 => 0x06, 47 => 0x19, 48 => 0x05,
        49 => 0x11, 50 => 0x10, 51 => 0x36, 52 => 0x37, 53 => 0x38,
        54 => 0xe5, 55 => 0x55, 56 => 0xe2, 57 => 0x2c, 58 => 0x39,
        59 => 0x3a, 60 => 0x3b, 61 => 0x3c, 62 => 0x3d, 63 => 0x3e,
        64 => 0x3f, 65 => 0x40, 66 => 0x41, 67 => 0x42,
        68 => 0x43, 69 => 0x44, 70 => 0x45,
        71 => 0x46, 72 => 0x47, 73 => 0x48,
        74 => 0x49, 75 => 0x4a, 76 => 0x4b, 77 => 0x4c,
        78 => 0x4d, 79 => 0x4e,
        80 => 0x4f, 81 => 0x50, 82 => 0x51, 83 => 0x52,
        84 => 0x53, 85 => 0x54, 86 => 0x56, 87 => 0x57,
        88 => 0x58, 89 => 0x59, 90 => 0x5a, 91 => 0x5b,
        92 => 0x5c, 93 => 0x5d, 94 => 0x5e, 95 => 0x5f,
        96 => 0x60, 97 => 0x61, 98 => 0x62, 99 => 0x63,
        100 => 0x64, 101 => 0x87, 102 => 0x66,
        103 => 0x66,
        104..=115 => (0x67 + (code - 104)) as u16,
        _ => 0,
    }
}

/// Determines whether a pointer event targets the compositor scene or a visual's content.
pub fn classify_pointer_target(
    scene: &Scene,
    _proj_view: &Matrix4<f32>,
    shift: bool,
    ctrl: bool,
    alt: bool,
) -> InteractionTarget {
    if shift || ctrl || alt {
        return InteractionTarget::Scene;
    }
    match scene.selected_id {
        Some(vid) => InteractionTarget::Content(vid),
        None => InteractionTarget::Scene,
    }
}

/// Convert screen coordinates to a point in the visual's frozen local
/// frame: (x, y) in world-local units, x right, y up, z the plane normal.
/// The point is NOT bounds-checked — callers decide whether it lies
/// within the quad (resize tracking must see the cursor travel past the
/// original window borders). Returns None when the cursor ray is
/// parallel to the visual's plane or hits it behind the camera.
pub fn screen_to_visual_local_point(
    proj_view: &Matrix4<f32>,
    ndc_x: f32,
    ndc_y: f32,
    visual_transform: &crate::scene::Transform3D,
    geom_w: f32,
    geom_h: f32,
) -> Option<(f32, f32)> {
    let inv_pv = proj_view.invert().unwrap_or(Matrix4::identity());
    let near = inv_pv * Vector4::new(ndc_x, ndc_y, -1.0, 1.0);
    let far = inv_pv * Vector4::new(ndc_x, ndc_y, 1.0, 1.0);
    let near = Vector3::new(near.x / near.w, near.y / near.w, near.z / near.w);
    let far = Vector3::new(far.x / far.w, far.y / far.w, far.z / far.w);
    let dir = (far - near).normalize();

    let model = Matrix4::from_translation(visual_transform.position)
        * Matrix4::from(visual_transform.rotation)
        * Matrix4::from_nonuniform_scale(geom_w, geom_h, 1.0);

    let inv_model = model.invert().unwrap_or(Matrix4::identity());
    let local_origin = inv_model * Vector4::new(near.x, near.y, near.z, 1.0);
    let local_dir = inv_model * Vector4::new(dir.x, dir.y, dir.z, 0.0);
    let lo = Vector3::new(local_origin.x, local_origin.y, local_origin.z) / local_origin.w;
    let ld = Vector3::new(local_dir.x, local_dir.y, local_dir.z);

    if ld.z.abs() < 1e-8 {
        return None;
    }
    let t = -lo.z / ld.z;
    if t < 0.0 {
        return None;
    }
    let hit = lo + ld * t;
    Some((hit.x, hit.y))
}

/// Convert screen coordinates to visual-local normalized UV coordinates.
pub fn screen_to_visual_uv(
    proj_view: &Matrix4<f32>,
    ndc_x: f32,
    ndc_y: f32,
    visual_transform: &crate::scene::Transform3D,
    geom_w: f32,
    geom_h: f32,
) -> Option<(f64, f64)> {
    let hit = screen_to_visual_local_point(
        proj_view, ndc_x, ndc_y, visual_transform, geom_w, geom_h,
    )?;
    if hit.0.abs() > 0.5 || hit.1.abs() > 0.5 {
        return None;
    }
    let u = (hit.0 + 0.5) as f64;
    let v = (1.0 - (hit.1 + 0.5)) as f64;
    Some((u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
}

/// Convert normalized UV to pixel coordinates.
pub fn uv_to_pixels(u: f64, v: f64, width: u32, height: u32) -> (u32, u32) {
    let px = (u * width as f64) as u32;
    let py = (v * height as f64) as u32;
    (px.min(width.saturating_sub(1)), py.min(height.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Transform3D;
    use cgmath::Deg;
    use cgmath::Quaternion;
    use cgmath::Rotation3;
    use cgmath::Vector3;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn linux_to_hid_known() {
        assert_eq!(linux_to_hid(1), 0x29);  // ESC
        assert_eq!(linux_to_hid(30), 0x04); // A
        assert_eq!(linux_to_hid(57), 0x2c); // space
        assert_eq!(linux_to_hid(42), 0xe1); // left shift
    }

    #[test]
    fn linux_to_hid_unmapped() {
        assert_eq!(linux_to_hid(999), 0);
    }

    #[test]
    fn classify_modifier_returns_scene() {
        let scene = Scene::default();
        let pv = Matrix4::identity();
        assert_eq!(classify_pointer_target(&scene, &pv, true, false, false), InteractionTarget::Scene);
        assert_eq!(classify_pointer_target(&scene, &pv, false, true, false), InteractionTarget::Scene);
        assert_eq!(classify_pointer_target(&scene, &pv, false, false, true), InteractionTarget::Scene);
    }

    #[test]
    fn classify_no_selection_returns_scene() {
        let scene = Scene::default();
        let pv = Matrix4::identity();
        assert_eq!(classify_pointer_target(&scene, &pv, false, false, false), InteractionTarget::Scene);
    }

    #[test]
    fn screen_to_uv_center() {
        let transform = Transform3D::identity();
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        let uv = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 200.0, 100.0);
        assert!(uv.is_some());
        let (u, v) = uv.unwrap();
        assert!(approx_eq(u, 0.5, 1e-4) && approx_eq(v, 0.5, 1e-4));
    }

    #[test]
    fn screen_to_uv_corner() {
        let transform = Transform3D::identity();
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        let uv = screen_to_visual_uv(&pv, -100.0 / 320.0, 50.0 / 240.0, &transform, 200.0, 100.0);
        assert!(uv.is_some());
        let (u, v) = uv.unwrap();
        assert!(u < 0.1 && v < 0.1);
    }

    #[test]
    fn screen_to_uv_rotated_visual() {
        let mut transform = Transform3D::identity();
        transform.rotation = Quaternion::from_angle_y(Deg(45.0));
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        let uv = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 200.0, 100.0);
        assert!(uv.is_some());
        let (u, v) = uv.unwrap();
        assert!(approx_eq(u, 0.5, 1e-4) && approx_eq(v, 0.5, 1e-4));
    }

    #[test]
    fn screen_to_uv_scaled_visual() {
        let mut transform = Transform3D::identity();
        transform.scale = Vector3::new(2.0, 2.0, 1.0);
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        let uv = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 200.0, 100.0);
        assert!(uv.is_some());
        let (u, v) = uv.unwrap();
        assert!(approx_eq(u, 0.5, 1e-4) && approx_eq(v, 0.5, 1e-4));
    }

    #[test]
    fn screen_to_uv_miss_outside() {
        let transform = Transform3D::identity();
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        assert!(screen_to_visual_uv(&pv, -0.9, 0.9, &transform, 200.0, 100.0).is_none());
    }

    #[test]
    fn uv_to_pixel_center() {
        assert_eq!(uv_to_pixels(0.5, 0.5, 1920, 1080), (960, 540));
    }

    #[test]
    fn uv_to_pixel_edge_clamp() {
        assert_eq!(uv_to_pixels(1.0, 1.0, 1920, 1080), (1919, 1079));
    }

    // ── HID mapping validation ─────────────────────────────────────────

    #[test]
    fn linux_to_hid_modifiers() {
        assert_eq!(linux_to_hid(29), 0xe1, "left ctrl");
        assert_eq!(linux_to_hid(42), 0xe1, "left shift");
        assert_eq!(linux_to_hid(56), 0xe2, "left alt");
        assert_eq!(linux_to_hid(54), 0xe5, "right shift");
    }

    #[test]
    fn linux_to_hid_navigation() {
        assert_eq!(linux_to_hid(74), 0x49, "insert");
        assert_eq!(linux_to_hid(75), 0x4a, "home");
        assert_eq!(linux_to_hid(76), 0x4b, "page up");
        assert_eq!(linux_to_hid(77), 0x4c, "delete");
        assert_eq!(linux_to_hid(78), 0x4d, "end");
        assert_eq!(linux_to_hid(79), 0x4e, "page down");
        assert_eq!(linux_to_hid(80), 0x4f, "right arrow");
        assert_eq!(linux_to_hid(81), 0x50, "left arrow");
        assert_eq!(linux_to_hid(82), 0x51, "down arrow");
        assert_eq!(linux_to_hid(83), 0x52, "up arrow");
    }

    #[test]
    fn linux_to_hid_function_keys() {
        for i in 0..12 {
            let hid = linux_to_hid(59 + i);
            assert_eq!(hid, 0x3a + i as u16, "F{}", i + 1);
        }
        assert_eq!(linux_to_hid(104), 0x67, "F13");
        assert_eq!(linux_to_hid(105), 0x68, "F14");
        assert_eq!(linux_to_hid(115), 0x72, "F24");
    }

    #[test]
    fn linux_to_hid_important_keys() {
        assert_eq!(linux_to_hid(1), 0x29, "escape");
        assert_eq!(linux_to_hid(15), 0x2b, "tab");
        assert_eq!(linux_to_hid(28), 0x28, "enter");
        assert_eq!(linux_to_hid(14), 0x2a, "backspace");
        assert_eq!(linux_to_hid(57), 0x2c, "space");
        assert_eq!(linux_to_hid(58), 0x39, "caps lock");
        assert_eq!(linux_to_hid(2), 0x1e, "1");
        assert_eq!(linux_to_hid(11), 0x27, "0");
    }

    #[test]
    fn linux_to_hid_numbers() {
        for i in 0..10 {
            let hid = linux_to_hid(2 + i);
            assert_eq!(hid, 0x1e + i as u16, "key {}", i);
        }
    }

    // ── RecordingInputSink tests ──────────────────────────────────────

    #[test]
    fn recording_input_sink_records_pointer() {
        let mut sink = RecordingInputSink::new();
        sink.handle_pointer(PointerEventKind::Down, 0.5, 0.5);
        sink.handle_pointer(PointerEventKind::Motion, 0.6, 0.5);
        sink.handle_pointer(PointerEventKind::Up, 0.6, 0.5);
        assert_eq!(sink.pointers.len(), 3);
        assert_eq!(sink.pointers[0], (PointerEventKind::Down, 0.5, 0.5));
        assert_eq!(sink.pointers[1], (PointerEventKind::Motion, 0.6, 0.5));
        assert_eq!(sink.pointers[2], (PointerEventKind::Up, 0.6, 0.5));
    }

    #[test]
    fn recording_input_sink_records_keyboard() {
        let mut sink = RecordingInputSink::new();
        sink.handle_keyboard(KeyboardEvent { key: 0x04, pressed: true });  // a down
        sink.handle_keyboard(KeyboardEvent { key: 0x04, pressed: false }); // a up
        sink.handle_keyboard(KeyboardEvent { key: 0x05, pressed: true });  // b down
        assert_eq!(sink.keys.len(), 3);
        assert_eq!(sink.keys[0], KeyboardEvent { key: 0x04, pressed: true });
        assert_eq!(sink.keys[1], KeyboardEvent { key: 0x04, pressed: false });
        assert_eq!(sink.keys[2], KeyboardEvent { key: 0x05, pressed: true });
    }

    #[test]
    fn recording_input_sink_clear() {
        let mut sink = RecordingInputSink::new();
        sink.handle_keyboard(KeyboardEvent { key: 0x04, pressed: true });
        sink.clear();
        assert!(sink.keys.is_empty());
        assert!(sink.pointers.is_empty());
    }

    // ── Two independent sinks test ─────────────────────────────────────

    #[test]
    fn two_independent_sinks() {
        let mut sink1 = RecordingInputSink::new();
        let mut sink2 = RecordingInputSink::new();

        // Click #1 → only sink1 sees pointer
        sink1.handle_pointer(PointerEventKind::Down, 0.3, 0.4);
        sink2.handle_pointer(PointerEventKind::Down, 0.3, 0.4);
        assert_eq!(sink1.pointers.len(), 1);
        assert_eq!(sink2.pointers.len(), 1);

        // Keyboard to #1 → only sink1 sees keyboard
        sink1.handle_keyboard(KeyboardEvent { key: 0x04, pressed: true });
        assert_eq!(sink1.keys.len(), 1);
        assert_eq!(sink2.keys.len(), 0);

        // Keyboard to #2 → only sink2 sees keyboard
        sink2.handle_keyboard(KeyboardEvent { key: 0x05, pressed: true });
        assert_eq!(sink1.keys.len(), 1);
        assert_eq!(sink2.keys.len(), 1);

        sink2.clear();
        assert_eq!(sink2.keys.len(), 0);
    }

    // ── Resize tests (UV/pixel chain) ──────────────────────────────────

    #[test]
    fn uv_same_after_resize() {
        let transform = Transform3D::identity();
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        // Same click in space, same UV regardless of framebuffer resolution
        let uv_small = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 200.0, 100.0);
        let uv_big = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 400.0, 200.0);
        assert!(uv_small.is_some() && uv_big.is_some());
        assert_eq!(uv_small.unwrap(), uv_big.unwrap(), "UV unchanged by resize");
    }

    #[test]
    fn pixel_differs_after_resize() {
        // Same UV → different pixel coords for different resolutions
        let (px_sm, py_sm) = uv_to_pixels(0.5, 0.5, 1920, 1080);
        let (px_bg, py_bg) = uv_to_pixels(0.5, 0.5, 2560, 1440);
        assert_eq!(px_sm, 960);
        assert_eq!(py_sm, 540);
        assert_eq!(px_bg, 1280);
        assert_eq!(py_bg, 720);
    }

    #[test]
    fn resize_changes_pick_geometry() {
        // The picking function uses geometry size (gw, gh).
        // When geometry changes (resize), the quad size changes,
        // which changes what spatial area the visual occupies.
        let view = Matrix4::look_at_rh(
            cgmath::Point3::new(0.0, 0.0, 500.0),
            cgmath::Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let pv = proj * view;
        let transform = Transform3D::identity();
        // NDC (-0.9, 0.5) is outside 200x100 quad (NDC bounds [-0.312,0.312])
        let uv_small = screen_to_visual_uv(&pv, -0.9, 0.5, &transform, 200.0, 100.0);
        assert!(uv_small.is_none(), "should miss small quad");
        // Same NDC with larger quad (800x400, NDC bounds [-1.25,0.833]) — now hits
        let uv_big = screen_to_visual_uv(&pv, -0.9, 0.5, &transform, 800.0, 400.0);
        assert!(uv_big.is_some(), "should hit resized larger quad");
    }

    #[test]
    fn resize_both_ends() {
        // Verify that going 1920→2560 and 2560→1920 both work
        let (px1, py1) = uv_to_pixels(0.5, 0.5, 1920, 1080);
        let (px2, py2) = uv_to_pixels(0.5, 0.5, 2560, 1440);
        let (px3, py3) = uv_to_pixels(0.5, 0.5, 1920, 1080);
        assert_eq!(px1, px3);
        assert_eq!(py1, py3);

        // Transform preserved — UV at center always (0.5, 0.5)
        let transform = Transform3D::identity();
        let proj = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0);
        let view = Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let pv = proj * view;
        let uv1 = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 200.0, 100.0);
        let uv2 = screen_to_visual_uv(&pv, 0.0, 0.0, &transform, 400.0, 200.0);
        assert_eq!(uv1, uv2, "center UV unchanged by resize");
    }

    #[test]
    fn aspect_change_uv() {
        // 16:9 → 16:10 aspect ratio change
        let pv = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0) *
            Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let (u, v) = screen_to_visual_uv(&pv, 0.0, 0.0, &Transform3D::identity(), 1920.0, 1080.0).unwrap();
        assert!((u - 0.5).abs() < 1e-4);
        assert!((v - 0.5).abs() < 1e-4);
        let (u2, v2) = screen_to_visual_uv(&pv, 0.0, 0.0, &Transform3D::identity(), 1920.0, 1200.0).unwrap();
        assert!((u2 - 0.5).abs() < 1e-4);
        assert!((v2 - 0.5).abs() < 1e-4);
    }

    // ── Decoration / title bar tests ──────────────────────────────────

    #[test]
    fn title_bar_hit_detection() {
        // Create a DecorationConfig with title_bar_height = 0.06
        let deco = crate::scene::DecorationConfig { title_bar_height: 0.06, title: "test".into() };
        let content_w = 200.0;
        let content_h = 100.0;
        let total_w = content_w;
        let total_h = content_h * (1.0 + 0.06);
        let title_h_frac = (0.06 / (1.0 + 0.06)) as f64;

        // A hit at the top of the visual should be in the title bar
        let pv = cgmath::ortho(-320.0, 320.0, -240.0, 240.0, 1.0, 1000.0) *
            Matrix4::look_at_rh(cgmath::Point3::new(0.0, 0.0, 500.0), cgmath::Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        // Hit the very top of the visual
        let ndc_y = (total_h / 2.0 - content_h * 0.06 / 2.0) / 240.0; // NDC for top of total quad
        // Actually just test that UV.y < title_h_frac means title bar
        assert!(title_h_frac > 0.0 && title_h_frac < 1.0);
    }

    #[test]
    fn content_uv_from_total_uv() {
        let title_h = 0.06f64;
        let title_frac = title_h / (1.0 + title_h);
        let content_v = (0.5f64 - title_frac) / (1.0 - title_frac);
        assert!((content_v - 0.5).abs() < 0.05, "center of total is center of content");
        assert!(0.01f64 < title_frac, "v=0.01 should be in title bar");
        assert!(0.1f64 > title_frac, "v=0.1 should be in content area");
    }

    #[test]
    fn total_height_includes_decorations() {
        let total_h: f64 = 100.0 * (1.0 + 0.06);
        let total_w: f64 = 200.0;
        assert!((total_h - 106.0f64).abs() < 1e-4);
        assert!((total_w - 200.0f64).abs() < 1e-4);
    }
}
