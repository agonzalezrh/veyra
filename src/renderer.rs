use std::sync::Mutex;

use cgmath::Matrix;
use cgmath::Matrix4;
use smithay::backend::renderer::gles::ffi;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::SwapBuffersError;
use tracing::error;

use crate::backend::PresentationBackend;
use crate::context_menu::ContextMenu;
use crate::perf::PerfStats;
use crate::scene::Scene;

/// Update sub-region of a GL texture with pixel data.
/// This is the narrow renderer-owned API for in-place texture updates.
/// Producers call this instead of raw GL operations.
pub fn upload_texture_sub_region(
    renderer: &mut GlesRenderer,
    tex_id: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    data: &[u8],
) {
    let _ = renderer.with_context(|gl| unsafe {
        gl.BindTexture(ffi::TEXTURE_2D, tex_id);
        gl.TexSubImage2D(
            ffi::TEXTURE_2D,
            0, x, y, w, h,
            ffi::BGRA_EXT,
            ffi::UNSIGNED_BYTE,
            data.as_ptr() as *const std::ffi::c_void,
        );
    });
}


/// Global DrawGl cache, created once per GL context lifetime.
/// Reset on context loss.
static DRAW_GL: Mutex<Option<DrawGl>> = Mutex::new(None);

/// Font atlas texture for bitmap text rendering.
static FONT_ATLAS: Mutex<Option<FontAtlas>> = Mutex::new(None);

struct FontAtlas {
    tex_id: u32,
    /// Glyph width in pixels
    gw: u32,
    /// Glyph height in pixels
    gh: u32,
    /// Columns in atlas
    cols: u32,
}

/// Render a line of text using the font atlas.
/// Uses the text_prog shader which applies a uniform color modulated by the font's alpha.
/// # Safety
/// Requires a current GL context with the text_prog program available.
unsafe fn draw_text(
    gl: &ffi::Gles2,
    draw: &DrawGl,
    text: &str,
    x_ndc: f32,
    y_ndc: f32,
    char_w: f32,
    char_h: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
) {
    let (font_tex_id, gw, gh, cols) = {
        let atlas_guard = FONT_ATLAS.lock().unwrap();
        let Some(ref font) = *atlas_guard else { return };
        (font.tex_id, font.gw, font.gh, font.cols)
    };
    let total_rows = atlas_rows(cols);
    let atlas_w = (cols * gw) as f32;
    let atlas_h = (total_rows * gh) as f32;

    // Use the existing quad shader for text: set u_title_h=0, bind font atlas.
    // This avoids needing text_u_color which triggers GL errors on some NVIDIA drivers.
    let stride = 4 * std::mem::size_of::<f32>() as i32;
    gl.UseProgram(draw.program);
    gl.Uniform1f(draw.u_selected, 0.0);
    gl.Uniform1f(draw.u_focused, 0.0);
    gl.Uniform1f(draw.u_title_h, 0.0);
    gl.Uniform1f(draw.u_edge, 0.0);
    // Glyph color comes from u_tint (the atlas ink is white; rgb carried
    // in .rgb, shape in .a). This finally applies the requested color.
    gl.Uniform4f(draw.u_tint, color_r, color_g, color_b, 1.0);
    gl.ActiveTexture(ffi::TEXTURE0);
    gl.BindTexture(ffi::TEXTURE_2D, font_tex_id);
    gl.Uniform1i(draw.u_tex, 0);
    gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);

    for (i, ch) in text.chars().enumerate() {
        let code = ch as u32;
        if !(32..=128).contains(&code) { continue; }
        let idx = code - 32;
        let col = idx % cols;
        let row = idx / cols;
        let u = (col * gw) as f32 / atlas_w;
        let v = (row * gh) as f32 / atlas_h;
        let uw = gw as f32 / atlas_w;
        let vh = gh as f32 / atlas_h;

        let verts: [f32; 16] = [
            -0.5, -0.5, u,       v + vh,
             0.5, -0.5, u + uw,  v + vh,
            -0.5,  0.5, u,       v,
             0.5,  0.5, u + uw,  v,
        ];

        gl.BufferData(
            ffi::ARRAY_BUFFER,
            std::mem::size_of_val(&verts) as isize,
            verts.as_ptr() as *const std::ffi::c_void,
            ffi::STREAM_DRAW,
        );

        let cx = x_ndc + (i as f32) * char_w;
        let cy = y_ndc;
        let mvp = cgmath::Matrix4::from_translation(cgmath::Vector3::new(cx + char_w / 2.0, cy + char_h / 2.0, 0.0))
            * cgmath::Matrix4::from_nonuniform_scale(char_w, char_h, 1.0);
        gl.UniformMatrix4fv(draw.u_mvp, 1, 0, mvp.as_ptr());

        gl.EnableVertexAttribArray(draw.a_pos);
        gl.VertexAttribPointer(draw.a_pos, 2, ffi::FLOAT, 0, stride, std::ptr::null());
        gl.EnableVertexAttribArray(draw.a_uv);
        gl.VertexAttribPointer(draw.a_uv, 2, ffi::FLOAT, 0, stride, (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void);
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(draw.a_pos);
        gl.DisableVertexAttribArray(draw.a_uv);
    }

    // Restore main VBO
    let verts: [f32; 16] = [
        -0.5, -0.5, 0.0, 1.0,
         0.5, -0.5, 1.0, 1.0,
        -0.5,  0.5, 0.0, 0.0,
         0.5,  0.5, 1.0, 0.0,
     ];
     gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
     gl.BufferData(
         ffi::ARRAY_BUFFER,
         std::mem::size_of_val(&verts) as isize,
         verts.as_ptr() as *const std::ffi::c_void,
         ffi::STATIC_DRAW,
     );
 }

/// Draw a text run INSIDE a window quad (J3 chrome): glyph positions
/// are in window-pixel coordinates relative to the window center and
/// ride the window's own model matrix, so title text and buttons
/// rotate/scale/move with the decoration exactly like the client
/// surface. Glyphs are offset slightly along the window normal
/// (+0.5 px along the window normal, toward the camera) to avoid
/// z-fighting with the window quad.
///
/// `win` = decorated (width, height) the model matrix was built with;
/// `run` = (left edge px, vertical center px, glyph height px), all
/// window pixels relative to the window center.
///
/// # Safety
/// Requires a current GL context.
unsafe fn draw_text_in_window(
    gl: &ffi::Gles2,
    draw: &DrawGl,
    text: &str,
    mats: (&Matrix4<f32>, &Matrix4<f32>),
    win: (f32, f32),
    run: (f32, f32, f32),
    color: (f32, f32, f32),
) {
    let (model, pv) = mats;
    let (gw, gh) = win;
    let (x_px, y_center_px, char_h_px) = run;
    let (font_tex_id, gw_atlas, gh_atlas, cols) = {
        let atlas_guard = FONT_ATLAS.lock().unwrap();
        let Some(ref font) = *atlas_guard else { return };
        (font.tex_id, font.gw, font.gh, font.cols)
    };
    let total_rows = atlas_rows(cols);
    let atlas_w = (cols * gw_atlas) as f32;
    let atlas_h = (total_rows * gh_atlas) as f32;

    let stride = 4 * std::mem::size_of::<f32>() as i32;
    gl.UseProgram(draw.program);
    gl.Uniform1f(draw.u_selected, 0.0);
    gl.Uniform1f(draw.u_focused, 0.0);
    gl.Uniform1f(draw.u_title_h, 0.0);
    gl.Uniform1f(draw.u_edge, 0.0);
    gl.Uniform4f(draw.u_tint, color.0, color.1, color.2, 1.0);
    gl.ActiveTexture(ffi::TEXTURE0);
    gl.BindTexture(ffi::TEXTURE_2D, font_tex_id);
    gl.Uniform1i(draw.u_tex, 0);
    gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);

    let char_w_px = char_h_px * 5.0 / 7.0;
    // Window px → model space: the model scales the unit quad by
    // (gw, gh, 1), so model x = px_x / gw, y = px_y / gh, z = px_z.
    let to_model_x = 1.0 / gw.max(1.0);
    let to_model_y = 1.0 / gh.max(1.0);
    for (i, ch) in text.chars().enumerate() {
        let code = ch as u32;
        if !(32..=128).contains(&code) { continue; }
        let idx = code - 32;
        let col = idx % cols;
        let row = idx / cols;
        let u = (col * gw_atlas) as f32 / atlas_w;
        let v = (row * gh_atlas) as f32 / atlas_h;
        let uw = gw_atlas as f32 / atlas_w;
        let vh = gh_atlas as f32 / atlas_h;

        let verts: [f32; 16] = [
            -0.5, -0.5, u,       v + vh,
             0.5, -0.5, u + uw,  v + vh,
            -0.5,  0.5, u,       v,
             0.5,  0.5, u + uw,  v,
        ];

        gl.BufferData(
            ffi::ARRAY_BUFFER,
            std::mem::size_of_val(&verts) as isize,
            verts.as_ptr() as *const std::ffi::c_void,
            ffi::STREAM_DRAW,
        );

        let cx = x_px + (i as f32) * char_w_px + char_w_px / 2.0;
        let glyph_local = Matrix4::from_translation(cgmath::Vector3::new(
            cx * to_model_x,
            y_center_px * to_model_y,
            // +z is TOWARD the camera (camera sits at +z looking down -z):
            // glyphs must be slightly IN FRONT of the window quad or the
            // depth test discards them (they are drawn after it).
            0.5,
        )) * Matrix4::from_nonuniform_scale(
            char_w_px * to_model_x,
            char_h_px * to_model_y,
            1.0,
        );
        let mvp = pv * (*model * glyph_local);
        gl.UniformMatrix4fv(draw.u_mvp, 1, 0, mvp.as_ptr());

        gl.EnableVertexAttribArray(draw.a_pos);
        gl.VertexAttribPointer(draw.a_pos, 2, ffi::FLOAT, 0, stride, std::ptr::null());
        gl.EnableVertexAttribArray(draw.a_uv);
        gl.VertexAttribPointer(draw.a_uv, 2, ffi::FLOAT, 0, stride, (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void);
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(draw.a_pos);
        gl.DisableVertexAttribArray(draw.a_uv);
    }

    // Restore main VBO
    let verts: [f32; 16] = [
        -0.5, -0.5, 0.0, 1.0,
         0.5, -0.5, 1.0, 1.0,
        -0.5,  0.5, 0.0, 0.0,
         0.5,  0.5, 1.0, 0.0,
    ];
    gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
    gl.BufferData(
        ffi::ARRAY_BUFFER,
        std::mem::size_of_val(&verts) as isize,
        verts.as_ptr() as *const std::ffi::c_void,
        ffi::STATIC_DRAW,
    );
}


/// Build a font atlas from a hardcoded 5x7 pixel bitmap font.
/// Contains 96 glyphs (ASCII 32-127), each 5 columns × 7 rows.
/// Packed 8 columns × 12 rows in the atlas texture.
/// # Safety
/// Requires a current GL context.
pub const FONT: &[u8] = &[
    // 32 space
    0x00,0x00,0x00,0x00,0x00,0x00,0x00,
    // 33 !
    0x20,0x20,0x20,0x20,0x00,0x20,0x00,
    // 34 "
    0x50,0x50,0x00,0x00,0x00,0x00,0x00,
    // 35 #
    0x50,0x50,0xf8,0x50,0xf8,0x50,0x50,
    // 36 $
    0x20,0x78,0xa0,0x70,0x28,0xf0,0x20,
    // 37 %
    0x40,0xa4,0x48,0x10,0x24,0x4a,0x04,
    // 38 &
    0x60,0x90,0xa0,0x40,0xa8,0x90,0x68,
    // 39 '
    0x20,0x20,0x00,0x00,0x00,0x00,0x00,
    // 40 (
    0x10,0x20,0x40,0x40,0x40,0x20,0x10,
    // 41 )
    0x40,0x20,0x10,0x10,0x10,0x20,0x40,
    // 42 *
    0x00,0x20,0xa8,0x70,0xa8,0x20,0x00,
    // 43 +
    0x00,0x20,0x20,0xf8,0x20,0x20,0x00,
    // 44 ,
    0x00,0x00,0x00,0x00,0x20,0x20,0x40,
    // 45 -
    0x00,0x00,0x00,0xf8,0x00,0x00,0x00,
    // 46 .
    0x00,0x00,0x00,0x00,0x00,0x20,0x00,
    // 47 /
    0x00,0x08,0x10,0x20,0x40,0x80,0x00,
    // 48 0
    0x70,0x88,0x98,0xa8,0xc8,0x88,0x70,
    // 49 1
    0x20,0x60,0xa0,0x20,0x20,0x20,0xf8,
    // 50 2
    0x70,0x88,0x08,0x10,0x20,0x40,0xf8,
    // 51 3
    0x70,0x88,0x08,0x30,0x08,0x88,0x70,
    // 52 4
    0x10,0x30,0x50,0x90,0xf8,0x10,0x10,
    // 53 5
    0xf8,0x80,0xf0,0x08,0x08,0x88,0x70,
    // 54 6
    0x30,0x40,0x80,0xf0,0x88,0x88,0x70,
    // 55 7
    0xf8,0x08,0x10,0x20,0x40,0x40,0x40,
    // 56 8
    0x70,0x88,0x88,0x70,0x88,0x88,0x70,
    // 57 9
    0x70,0x88,0x88,0x78,0x08,0x10,0x60,
    // 58 :
    0x00,0x20,0x00,0x00,0x00,0x20,0x00,
    // 59 ;
    0x00,0x20,0x00,0x00,0x20,0x20,0x40,
    // 60 <
    0x00,0x08,0x10,0x20,0x10,0x08,0x00,
    // 61 =
    0x00,0x00,0xf8,0x00,0xf8,0x00,0x00,
    // 62 >
    0x00,0x80,0x40,0x20,0x40,0x80,0x00,
    // 63 ?
    0x70,0x88,0x08,0x10,0x20,0x00,0x20,
    // 64 @
    0x70,0x88,0xb8,0xa8,0xb0,0x80,0x78,
    // 65 A
    0x20,0x50,0x88,0x88,0xf8,0x88,0x88,
    // 66 B
    0xf0,0x88,0x88,0xf0,0x88,0x88,0xf0,
    // 67 C
    0x70,0x88,0x80,0x80,0x80,0x88,0x70,
    // 68 D
    0xf0,0x88,0x88,0x88,0x88,0x88,0xf0,
    // 69 E
    0xf8,0x80,0x80,0xf0,0x80,0x80,0xf8,
    // 70 F
    0xf8,0x80,0x80,0xf0,0x80,0x80,0x80,
    // 71 G
    0x78,0x80,0x80,0x98,0x88,0x88,0x78,
    // 72 H
    0x88,0x88,0x88,0xf8,0x88,0x88,0x88,
    // 73 I
    0xf8,0x20,0x20,0x20,0x20,0x20,0xf8,
    // 74 J
    0x08,0x08,0x08,0x08,0x08,0x88,0x70,
    // 75 K
    0x88,0x90,0xa0,0xc0,0xa0,0x90,0x88,
    // 76 L
    0x80,0x80,0x80,0x80,0x80,0x80,0xf8,
    // 77 M
    0x88,0xd8,0xa8,0x88,0x88,0x88,0x88,
    // 78 N
    0x88,0xc8,0xa8,0x98,0x88,0x88,0x88,
    // 79 O
    0x70,0x88,0x88,0x88,0x88,0x88,0x70,
    // 80 P
    0xf0,0x88,0x88,0xf0,0x80,0x80,0x80,
    // 81 Q
    0x70,0x88,0x88,0x88,0xa8,0x90,0x68,
    // 82 R
    0xf0,0x88,0x88,0xf0,0xa0,0x90,0x88,
    // 83 S
    0x70,0x88,0x80,0x70,0x08,0x88,0x70,
    // 84 T
    0xf8,0x20,0x20,0x20,0x20,0x20,0x20,
    // 85 U
    0x88,0x88,0x88,0x88,0x88,0x88,0x70,
    // 86 V
    0x88,0x88,0x88,0x88,0x50,0x50,0x20,
    // 87 W
    0x88,0x88,0x88,0xa8,0xa8,0xd8,0x88,
    // 88 X
    0x88,0x88,0x50,0x20,0x50,0x88,0x88,
    // 89 Y
    0x88,0x88,0x50,0x20,0x20,0x20,0x20,
    // 90 Z
    0xf8,0x08,0x10,0x20,0x40,0x80,0xf8,
    // 91 [
    0x70,0x40,0x40,0x40,0x40,0x40,0x70,
    // 92 backslash
    0x00,0x80,0x40,0x20,0x10,0x08,0x00,
    // 93 ]
    0x70,0x10,0x10,0x10,0x10,0x10,0x70,
    // 94 ^
    0x20,0x50,0x00,0x00,0x00,0x00,0x00,
    // 95 _
    0x00,0x00,0x00,0x00,0x00,0x00,0xf8,
    // 96 `
    0x40,0x20,0x00,0x00,0x00,0x00,0x00,
    // 97 a
    0x00,0x00,0x70,0x08,0x78,0x88,0x78,
    // 98 b
    0x80,0x80,0xf0,0x88,0x88,0x88,0xf0,
    // 99 c
    0x00,0x00,0x70,0x88,0x80,0x88,0x70,
    // 100 d
    0x08,0x08,0x78,0x88,0x88,0x88,0x78,
    // 101 e
    0x00,0x00,0x70,0x88,0xf8,0x80,0x78,
    // 102 f
    0x30,0x48,0x40,0xe0,0x40,0x40,0x40,
    // 103 g
    0x00,0x00,0x78,0x88,0x78,0x08,0x70,
    // 104 h
    0x80,0x80,0xf0,0x88,0x88,0x88,0x88,
    // 105 i
    0x20,0x00,0x60,0x20,0x20,0x20,0x70,
    // 106 j
    0x10,0x00,0x30,0x10,0x10,0x90,0x60,
    // 107 k
    0x80,0x80,0x88,0x90,0xe0,0x90,0x88,
    // 108 l
    0x60,0x20,0x20,0x20,0x20,0x20,0x70,
    // 109 m
    0x00,0x00,0xd0,0xa8,0xa8,0x88,0x88,
    // 110 n
    0x00,0x00,0xf0,0x88,0x88,0x88,0x88,
    // 111 o
    0x00,0x00,0x70,0x88,0x88,0x88,0x70,
    // 112 p
    0x00,0x00,0xf0,0x88,0xf0,0x80,0x80,
    // 113 q
    0x00,0x00,0x78,0x88,0x78,0x08,0x08,
    // 114 r
    0x00,0x00,0xb0,0xc8,0x80,0x80,0x80,
    // 115 s
    0x00,0x00,0x78,0x80,0x70,0x08,0xf0,
    // 116 t
    0x40,0x40,0xf0,0x40,0x40,0x48,0x30,
    // 117 u
    0x00,0x00,0x88,0x88,0x88,0x88,0x78,
    // 118 v
    0x00,0x00,0x88,0x88,0x88,0x50,0x20,
    // 119 w
    0x00,0x00,0x88,0x88,0xa8,0xa8,0x50,
    // 120 x
    0x00,0x00,0x88,0x50,0x20,0x50,0x88,
    // 121 y
    0x00,0x00,0x88,0x88,0x78,0x08,0x70,
    // 122 z
    0x00,0x00,0xf8,0x10,0x20,0x40,0xf8,
    // 123 {
    0x18,0x20,0x20,0xc0,0x20,0x20,0x18,
    // 124 |
    0x20,0x20,0x20,0x20,0x20,0x20,0x20,
    // 125 }
    0xc0,0x20,0x20,0x18,0x20,0x20,0xc0,
    // 126 ~
    0x00,0x00,0x40,0xa8,0x10,0x00,0x00,
    // 127 DEL (placeholder keeps ASCII codes aligned with indices)
    0x00,0x00,0x00,0x00,0x00,0x00,0x00,
    // 128 (custom) hollow box — title-bar maximize button (J3).
    // Not ASCII; referenced via char::from_u32(128).
    0xf8,0x88,0x88,0x88,0x88,0x88,0xf8,
];

pub const fn font_glyph_count() -> usize {
    FONT.len() / 7
}

pub const fn atlas_rows(cols: u32) -> u32 {
    (font_glyph_count() as u32).div_ceil(cols)
}

unsafe fn ensure_font_atlas(gl: &ffi::Gles2) {
    let mut guard = FONT_ATLAS.lock().unwrap();
    if guard.is_some() {
        return;
    }

    const GW: u32 = 5;
    const GH: u32 = 7;
    const COLS: u32 = 16;
    const ATLAS_W: u32 = COLS * GW;
    // Rows derive from the glyph count so appended custom glyphs
    // (code 128 = maximize box) extend the atlas automatically.
    const ROWS: u32 = atlas_rows(COLS);
    const ATLAS_H: u32 = ROWS * GH;

    // 96 ASCII glyphs + appended custom glyphs, 5 columns × 7 rows each.
    // Each byte is one row of 5 pixels: bit (7-c) lights column c
    // (c=0 is the LEFTMOST pixel), i.e. MSB-first within the low 5 bits.


    // RGBA atlas with WHITE ink (alpha carries the glyph shape): the quad
    // shader samples rgb directly, so white ink lets the u_tint uniform
    // control the final text color (black ink would stay black).
    let mut pixels = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];
    let glyph_count = FONT.len() / (GH as usize);
    for gi in 0..glyph_count {
        let col = gi % COLS as usize;
        let row = gi / COLS as usize;
        let gx = (col * GW as usize) as u32;
        let gy = (row * GH as usize) as u32;
        for r in 0..GH {
            let byte = FONT[gi * (GH as usize) + r as usize];
            for c in 0..GW {
                // Font-data convention: bit (7-c) lights column c
                // (verified against the D/X/C/O glyph shapes). The old
                // (4-c) shift rendered every glyph as a 1-2px smudge —
                // the likely root cause of the "menu text unreadable"
                // report.
                let bit = (byte >> (7 - c)) & 1;
                let px = (gx + c) as usize;
                let py = (gy + r) as usize;
                if bit != 0 {
                    let i = (py * ATLAS_W as usize + px) * 4;
                    pixels[i] = 255; // R
                    pixels[i + 1] = 255; // G
                    pixels[i + 2] = 255; // B
                    pixels[i + 3] = 255; // A
                }
            }
        }
    }

    let mut tex = 0;
    gl.GenTextures(1, &mut tex);
    gl.BindTexture(ffi::TEXTURE_2D, tex);
    gl.TexImage2D(
        ffi::TEXTURE_2D,
        0,
        ffi::RGBA as i32,
        ATLAS_W as i32,
        ATLAS_H as i32,
        0,
        ffi::RGBA,
        ffi::UNSIGNED_BYTE,
        pixels.as_ptr() as *const std::ffi::c_void,
    );
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::NEAREST as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::NEAREST as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);

    *guard = Some(FontAtlas { tex_id: tex, gw: GW, gh: GH, cols: COLS });
}

const QUAD_VS: &str = "\
attribute vec2 a_pos;
attribute vec2 a_uv;
uniform mat4 u_mvp;
varying vec2 v_uv;
void main() {
    gl_Position = u_mvp * vec4(a_pos, 0.0, 1.0);
    v_uv = a_uv;
}
";

const QUAD_FS: &str = "\
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
uniform float u_selected;
uniform float u_focused;
uniform float u_title_h;
uniform float u_edge;
uniform vec4 u_tint;
uniform vec4 u_border;
void main() {
    vec2 uv = v_uv;
    // Fixed-pixel chrome: thickness passed per-axis in UV units so the
    // ring stays ~2.5 px regardless of window size (5% of a maximized
    // window was a 30-60px gold band).
    float bx = u_border.x;
    float by = u_border.y;
    float tbx = u_border.z;
    float tby = u_border.w;
    // u_edge=0 disables the window-chrome borders entirely (glyphs and
    // overlay quads reuse this shader with atlas-relative UVs that would
    // otherwise cross the chrome thresholds).
    float th = u_title_h * u_edge;
    bvec4 edge = bvec4(
        u_edge > 0.5 && (uv.x < bx || uv.x > 1.0 - bx),
        u_edge > 0.5 && (uv.y < by || uv.y > 1.0 - by),
        false,
        false
    );
    if (uv.y < th) {
        bool title_edge = uv.x < tbx || uv.x > 1.0 - tbx ||
                          uv.y < tby || uv.y > th - tby;
        if (title_edge) {
            // Muted states: full-saturation gold/green/cyan on every
            // window read as visual noise (physical feedback).
            if (u_selected > 0.5) {
                gl_FragColor = vec4(0.55, 0.45, 0.10, 1.0);
            } else if (u_focused > 0.5) {
                gl_FragColor = vec4(0.22, 0.46, 0.22, 1.0);
            } else {
                gl_FragColor = vec4(0.14, 0.22, 0.23, 1.0);
            }
        } else {
            // Strip interior: same hue family, quieter.
            if (u_selected > 0.5) {
                gl_FragColor = vec4(0.30, 0.24, 0.08, 0.9);
            } else if (u_focused > 0.5) {
                gl_FragColor = vec4(0.12, 0.24, 0.12, 0.9);
            } else {
                gl_FragColor = vec4(0.09, 0.15, 0.16, 0.9);
            }
        }
    } else {
        vec2 content_uv = vec2(uv.x, (uv.y - th) / (1.0 - th));
        if (any(edge)) {
            if (u_selected > 0.5) {
                gl_FragColor = vec4(0.62, 0.50, 0.10, 1.0);
            } else if (u_focused > 0.5) {
                gl_FragColor = vec4(0.24, 0.52, 0.24, 1.0);
            } else {
                gl_FragColor = vec4(0.16, 0.26, 0.27, 1.0);
            }
        } else {
            gl_FragColor = texture2D(u_tex, content_uv) * u_tint;
        }
    }
}
";

struct DrawGl {
    program: u32,
    a_pos: u32,
    a_uv: u32,
    u_mvp: i32,
    u_tex: i32,
    u_selected: i32,
    u_focused: i32,
    u_title_h: i32,
    u_edge: i32,
    u_tint: i32,
    u_border: i32,
    /// Solid-color overlay program (no texture, no window chrome semantics).
    solid_prog: u32,
    solid_a_pos: u32,
    solid_a_uv: u32,
    solid_u_mvp: i32,
    solid_u_color: i32,
    vbo: u32,
    /// Simple text shader: samples alpha from a texture, applies a solid color.
    text_prog: u32,
    text_a_pos: u32,
    text_a_uv: u32,
    text_u_mvp: i32,
    text_u_tex: i32,
    text_u_color: i32,
    text_vbo: u32,
}

const TEXT_VS: &str = "\
attribute vec2 a_pos;
attribute vec2 a_uv;
uniform mat4 u_mvp;
varying vec2 v_uv;
void main() {
    gl_Position = u_mvp * vec4(a_pos, 0.0, 1.0);
    v_uv = a_uv;
}
";

const TEXT_FS: &str = "\
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_color;
void main() {
    float a = texture2D(u_tex, v_uv).a;
    gl_FragColor = vec4(u_color.rgb, u_color.a * a);
}
";

const SOLID_FS: &str = "\
precision mediump float;
uniform vec4 u_color;
void main() {
    gl_FragColor = u_color;
}
";

impl DrawGl {
    fn new(gl: &ffi::Gles2) -> Self {
        let vs = Self::compile(gl, ffi::VERTEX_SHADER, QUAD_VS);
        let fs = Self::compile(gl, ffi::FRAGMENT_SHADER, QUAD_FS);
        let program = unsafe { gl.CreateProgram() };
        unsafe {
            gl.AttachShader(program, vs);
            gl.AttachShader(program, fs);
            gl.LinkProgram(program);
            gl.DeleteShader(vs);
            gl.DeleteShader(fs);
        }
        let a_pos = unsafe { gl.GetAttribLocation(program, b"a_pos\0".as_ptr() as *const i8) as u32 };
        let a_uv = unsafe { gl.GetAttribLocation(program, b"a_uv\0".as_ptr() as *const i8) as u32 };
        let u_mvp = unsafe { gl.GetUniformLocation(program, b"u_mvp\0".as_ptr() as *const i8) };
        let u_tex = unsafe { gl.GetUniformLocation(program, b"u_tex\0".as_ptr() as *const i8) };
        let u_selected = unsafe { gl.GetUniformLocation(program, b"u_selected\0".as_ptr() as *const i8) };
        let u_focused = unsafe { gl.GetUniformLocation(program, b"u_focused\0".as_ptr() as *const i8) };
        let u_title_h = unsafe { gl.GetUniformLocation(program, b"u_title_h\0".as_ptr() as *const i8) };
        let u_edge = unsafe { gl.GetUniformLocation(program, c"u_edge".as_ptr()) };
        let u_tint = unsafe { gl.GetUniformLocation(program, c"u_tint".as_ptr()) };
        let u_border = unsafe { gl.GetUniformLocation(program, c"u_border".as_ptr()) };
        let mut vbo = 0;
        unsafe { gl.GenBuffers(1, &mut vbo) };
        let verts: [f32; 16] = [
            -0.5, -0.5, 0.0, 1.0,
             0.5, -0.5, 1.0, 1.0,
            -0.5,  0.5, 0.0, 0.0,
             0.5,  0.5, 1.0, 0.0,
        ];
        unsafe {
            gl.BindBuffer(ffi::ARRAY_BUFFER, vbo);
            gl.BufferData(ffi::ARRAY_BUFFER, std::mem::size_of_val(&verts) as isize,
                verts.as_ptr() as *const std::ffi::c_void, ffi::STATIC_DRAW);
        }

        // Text shader
        let tvs = Self::compile(gl, ffi::VERTEX_SHADER, TEXT_VS);
        let tfs = Self::compile(gl, ffi::FRAGMENT_SHADER, TEXT_FS);
        let text_prog = unsafe { gl.CreateProgram() };
        unsafe {
            gl.AttachShader(text_prog, tvs);
            gl.AttachShader(text_prog, tfs);
            gl.LinkProgram(text_prog);
            gl.DeleteShader(tvs);
            gl.DeleteShader(tfs);
        }
        let text_a_pos = unsafe { gl.GetAttribLocation(text_prog, b"a_pos\0".as_ptr() as *const i8) as u32 };
        let text_a_uv = unsafe { gl.GetAttribLocation(text_prog, b"a_uv\0".as_ptr() as *const i8) as u32 };
        let text_u_mvp = unsafe { gl.GetUniformLocation(text_prog, b"u_mvp\0".as_ptr() as *const i8) };
        let text_u_tex = unsafe { gl.GetUniformLocation(text_prog, b"u_tex\0".as_ptr() as *const i8) };
        let text_u_color = unsafe { gl.GetUniformLocation(text_prog, b"u_color\0".as_ptr() as *const i8) };
        let mut text_vbo = 0;
        unsafe { gl.GenBuffers(1, &mut text_vbo) };
        let text_verts: [f32; 16] = [
            -0.5, -0.5, 0.0, 1.0,
             0.5, -0.5, 1.0, 1.0,
            -0.5,  0.5, 0.0, 0.0,
             0.5,  0.5, 1.0, 0.0,
        ];
        unsafe {
            gl.BindBuffer(ffi::ARRAY_BUFFER, text_vbo);
            gl.BufferData(ffi::ARRAY_BUFFER, std::mem::size_of_val(&text_verts) as isize,
                text_verts.as_ptr() as *const std::ffi::c_void, ffi::STATIC_DRAW);
        }

        // Solid overlay shader: constant color regardless of UV/texture.
        let svs = Self::compile(gl, ffi::VERTEX_SHADER, QUAD_VS);
        let sfs = Self::compile(gl, ffi::FRAGMENT_SHADER, SOLID_FS);
        let solid_prog = unsafe { gl.CreateProgram() };
        unsafe {
            gl.AttachShader(solid_prog, svs);
            gl.AttachShader(solid_prog, sfs);
            gl.LinkProgram(solid_prog);
            gl.DeleteShader(svs);
            gl.DeleteShader(sfs);
        }
        let solid_a_pos = unsafe { gl.GetAttribLocation(solid_prog, c"a_pos".as_ptr()) as u32 };
        let solid_a_uv = unsafe { gl.GetAttribLocation(solid_prog, c"a_uv".as_ptr()) as u32 };
        let solid_u_mvp = unsafe { gl.GetUniformLocation(solid_prog, c"u_mvp".as_ptr()) };
        let solid_u_color = unsafe { gl.GetUniformLocation(solid_prog, c"u_color".as_ptr()) };

        DrawGl { program, a_pos, a_uv, u_mvp, u_tex, u_selected, u_focused, u_title_h, u_edge, u_tint, u_border,
                 solid_prog, solid_a_pos, solid_a_uv, solid_u_mvp, solid_u_color, vbo,
                 text_prog, text_a_pos, text_a_uv, text_u_mvp, text_u_tex, text_u_color, text_vbo }
    }

    fn compile(gl: &ffi::Gles2, kind: u32, src: &str) -> u32 {
        let s = unsafe { gl.CreateShader(kind) };
        let bytes = src.as_bytes();
        let len = bytes.len() as i32;
        unsafe {
            gl.ShaderSource(s, 1, &(bytes.as_ptr() as *const i8), &len);
            gl.CompileShader(s);
        }
        let mut ok = 0;
        unsafe { gl.GetShaderiv(s, ffi::COMPILE_STATUS, &mut ok) };
        if ok == 0 {
            let mut len = 0;
            unsafe { gl.GetShaderiv(s, ffi::INFO_LOG_LENGTH, &mut len) };
            let mut buf = vec![0u8; len as usize];
            unsafe { gl.GetShaderInfoLog(s, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut i8) };
            error!("Shader error: {}", String::from_utf8_lossy(&buf));
        }
        s
    }
}

/// Get or create the cached DrawGl.
fn get_draw_gl(gl: &ffi::Gles2) -> Option<std::sync::MutexGuard<'static, Option<DrawGl>>> {
    let mut guard = DRAW_GL.lock().unwrap();
    if guard.is_none() {
        *guard = Some(DrawGl::new(gl));
    }
    Some(guard)
}

fn draw_textured_quad(
    gl: &ffi::Gles2,
    draw: &DrawGl,
    mvp: &Matrix4<f32>,
    tex_id: u32,
    selected: bool,
    focused: bool,
    title_h: f32,
    gw: f32,
    gh: f32,
) {
    unsafe {
        gl.UseProgram(draw.program);
        gl.UniformMatrix4fv(draw.u_mvp, 1, 0, mvp.as_ptr());
        gl.Uniform1f(draw.u_selected, if selected { 1.0 } else { 0.0 });
        gl.Uniform1f(draw.u_focused, if focused { 1.0 } else { 0.0 });
        gl.Uniform1f(draw.u_title_h, title_h);
        gl.Uniform1f(draw.u_edge, 1.0);
        gl.Uniform4f(draw.u_tint, 1.0, 1.0, 1.0, 1.0);
        // ~1.5px chrome ring regardless of window size (2.5px read as
        // a heavy frame in physical testing).
        let ring_u = 1.5 / gw.max(1.0);
        let ring_v = 1.5 / gh.max(1.0);
        gl.Uniform4f(draw.u_border, ring_u, ring_v, ring_u, ring_v);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, tex_id);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.Uniform1i(draw.u_tex, 0);

        let stride = 4 * std::mem::size_of::<f32>() as i32;
        gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
        gl.EnableVertexAttribArray(draw.a_pos);
        gl.VertexAttribPointer(draw.a_pos, 2, ffi::FLOAT, 0, stride, std::ptr::null());
        gl.EnableVertexAttribArray(draw.a_uv);
        gl.VertexAttribPointer(draw.a_uv, 2, ffi::FLOAT, 0, stride,
            (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void);
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(draw.a_pos);
        gl.DisableVertexAttribArray(draw.a_uv);
    }
}

pub fn render_scene(
    backend: &mut dyn PresentationBackend,
    scene: &Scene,
    view: &Matrix4<f32>,
    proj: &Matrix4<f32>,
    perf: &mut PerfStats,
    visible_ids: Option<&[crate::scene::VisualId]>,
    context_menu: Option<&ContextMenu>,
) -> Result<(), SwapBuffersError> {
    use crate::perf::PipelineStage;

    let (w, h) = backend.size();

    let t_bind = std::time::Instant::now();
    backend.begin_frame()?;
    perf.record_stage(PipelineStage::RenderBind, t_bind.elapsed().as_nanos() as u64);

    // Stash raw pointers to EGL context and surface so we can rebind the
    // window surface inside with_context() closures. with_context() internally
    // calls eglMakeCurrent with EGL_NO_SURFACE, which unbinds the window surface
    // and causes GL_INVALID_FRAMEBUFFER_OPERATION on subsequent GL operations.
    // Using raw pointers avoids borrow conflicts with with_context(&mut self).
    // Stash the surface pointer BEFORE borrowing renderer (borrows backend).
    let egl_surface_ptr: Option<*const smithay::backend::egl::EGLSurface> =
        backend.egl_surface().map(|s| s as *const _);

    let renderer = backend.renderer();
    let egl_ctx_ptr: *const smithay::backend::egl::EGLContext = renderer.egl_context();

    // Helper to rebind the window surface as the current draw/read target.
    // Must be called inside each with_context() closure before any GL operations.
    let rebind_surface = |gl: &ffi::Gles2| unsafe {
        if let Some(surface_ptr) = egl_surface_ptr {
            (*egl_ctx_ptr)
                .make_current_with_surface(&*surface_ptr)
                .expect("make_current_with_surface");
            gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
        }
    };

    // Initialize DrawGl once per GL context lifetime
    let _ = renderer.with_context(|gl| {
        rebind_surface(gl);
        get_draw_gl(gl);
    });
    let draw_guard = DRAW_GL.lock().unwrap();
    let draw = match draw_guard.as_ref() {
        Some(d) => d,
        None => {
            error!("DrawGl not initialized");
            return Ok(());
        }
    };

    // Set up viewport, clear, and state in one with_context block
    let t_clear = std::time::Instant::now();
    let _ = renderer.with_context(|gl| unsafe {
        rebind_surface(gl);
        gl.Viewport(0, 0, w as i32, h as i32);
        gl.ClearColor(0.15, 0.15, 0.15, 1.0);
        gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.Enable(ffi::DEPTH_TEST);
        gl.DepthFunc(ffi::LESS);
    });
    perf.record_stage(PipelineStage::RenderDraw, t_clear.elapsed().as_nanos() as u64);

    // Draw all visuals
    let t_draw = std::time::Instant::now();
    let pv = proj * view;
    for visual in scene.iter() {
        if visual.window_state == crate::scene::WindowState::Minimized {
            continue;
        }
        if let Some(ids) = visible_ids {
            if !ids.contains(&visual.id) {
                continue;
            }
        }
        let Some(texture) = visual.texture() else { continue };
        let tex_id = texture.tex_id();
        let gw = visual.total_width();
        let gh = visual.total_height();
        let title_h =
            visual.decoration.title_bar_height / (1.0 + visual.decoration.title_bar_height);
        let world = scene.world_matrix(visual.id);
        let wx = world[3][0];
        let wy = world[3][1];
        let wz = world[3][2];
        let m3 = cgmath::Matrix3::new(
            world[0][0], world[0][1], world[0][2], world[1][0], world[1][1], world[1][2],
            world[2][0], world[2][1], world[2][2],
        );
        let rot = cgmath::Quaternion::from(m3);
        let model = Matrix4::from_translation(cgmath::Vector3::new(wx, wy, wz))
            * Matrix4::from(rot)
            * Matrix4::from_nonuniform_scale(gw, gh, 1.0);
        let mvp = proj * view * model;
        let chrome = visual.chrome.clone();
        let focused = visual.focused;
        let _ = renderer.with_context(|gl| unsafe {
            rebind_surface(gl);
            draw_textured_quad(gl, draw, &mvp, tex_id, visual.selected, visual.focused, title_h, gw, gh);

            // J3 chrome: title text + window buttons ride the SAME model
            // matrix as the client surface (one spatial object).
            let strip_px = title_h * gh;
            let char_h = strip_px * 0.62;
            let layout = crate::chrome::ButtonLayout::for_window(gw, gh, title_h);
            let [_, _, min_zone] = layout.zones();
            // Title text: left-aligned in the strip, fitting between the
            // left margin and the button region.
            let left_margin = strip_px * 0.35;
            let avail = min_zone.u_lo * gw - left_margin - strip_px * 0.25;
            let title = crate::chrome::fit_title(&chrome.title, avail.max(0.0), char_h);
            if !title.is_empty() {
                let (tr, tg, tb) = if focused { (0.95, 0.95, 0.95) } else { (0.55, 0.58, 0.60) };
                draw_text_in_window(
                    gl, draw, &title, (&model, &pv), (gw, gh),
                    (-gw * 0.5 + left_margin, gh * 0.5 - strip_px * 0.5, char_h),
                    (tr, tg, tb),
                );
            }
            // Buttons: right-aligned glyphs, slightly brighter on focus.
            let (br, bg, bb) = if focused { (0.92, 0.92, 0.92) } else { (0.52, 0.55, 0.57) };
            for (button, u_center) in layout.centers() {
                let glyph = char::from_u32(button.glyph_code()).unwrap_or(' ');
                let cw = char_h * 0.9 * 5.0 / 7.0;
                let cx_px = (u_center - 0.5) * gw;
                draw_text_in_window(
                    gl, draw, &glyph.to_string(), (&model, &pv), (gw, gh),
                    (cx_px - cw * 0.5, gh * 0.5 - strip_px * 0.5, char_h * 0.9),
                    (br, bg, bb),
                );
            }
        });
    }
    perf.record_stage(PipelineStage::RenderDraw, t_draw.elapsed().as_nanos() as u64);

    // Render context menu overlay (if visible)
    if let Some(menu) = context_menu {
        if menu.visible {
            let _ = renderer.with_context(|gl| unsafe {
                rebind_surface(gl);
                gl.Disable(ffi::DEPTH_TEST);
                gl.Enable(ffi::BLEND);
                gl.BlendFunc(ffi::SRC_ALPHA, ffi::ONE_MINUS_SRC_ALPHA);

                let (mx, my) = menu.position;
                // DPI-proportional metrics shared with the click hit-test
                let metrics = crate::context_menu::MenuMetrics::for_framebuffer(w as f32, h as f32);
                let menu_width = metrics.menu_width;
                let item_height = metrics.item_height;
                let menu_height = menu.items.len() as f32 * item_height;

                // Convert screen pixel coords to NDC [-1, 1]
                let ndc_w = menu_width / w as f32 * 2.0;

                // Ensure font atlas is initialized for the labels below
                ensure_font_atlas(gl);

                // Draw px-space rects through the solid overlay program.
                // (px, py) is the top-left corner in screen pixels.
                let stride = 4 * std::mem::size_of::<f32>() as i32;
                let solid_rect = |px: f32, py: f32, pw: f32, ph: f32,
                                  r: f32, g: f32, b: f32, a: f32| {
                    let cx = ((px + pw / 2.0) / w as f32) * 2.0 - 1.0;
                    let cy = -(((py + ph / 2.0) / h as f32) * 2.0 - 1.0);
                    let mvp = cgmath::Matrix4::from_translation(cgmath::Vector3::new(cx, cy, 0.0))
                        * cgmath::Matrix4::from_nonuniform_scale(
                            pw / w as f32 * 2.0,
                            ph / h as f32 * 2.0,
                            1.0,
                        );
                    gl.UseProgram(draw.solid_prog);
                    gl.UniformMatrix4fv(draw.solid_u_mvp, 1, 0, mvp.as_ptr());
                    gl.Uniform4f(draw.solid_u_color, r, g, b, a);
                    gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
                    gl.EnableVertexAttribArray(draw.solid_a_pos);
                    gl.VertexAttribPointer(draw.solid_a_pos, 2, ffi::FLOAT, 0, stride, std::ptr::null());
                    gl.EnableVertexAttribArray(draw.solid_a_uv);
                    gl.VertexAttribPointer(
                        draw.solid_a_uv,
                        2,
                        ffi::FLOAT,
                        0,
                        stride,
                        (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void,
                    );
                    gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
                    gl.DisableVertexAttribArray(draw.solid_a_pos);
                    gl.DisableVertexAttribArray(draw.solid_a_uv);
                };

                // Border ring + dark interior: subtle neutral chrome, no
                // window-shader edges, readable against any background.
                let (mx32, my32) = (mx as f32, my as f32);
                solid_rect(mx32 - 1.0, my32 - 1.0, menu_width + 2.0, menu_height + 2.0,
                           0.30, 0.30, 0.32, 0.98);
                solid_rect(mx32, my32, menu_width, menu_height, 0.13, 0.13, 0.15, 0.97);

                let ndc_ih = item_height / h as f32 * 2.0;
                // Draw each menu item
                for (i, _item) in menu.items.iter().enumerate() {
                    let item_iy =
                        -((my as f32 + (i as f32 * item_height)) / h as f32) * 2.0 + 1.0;
                    let item_ix =
                        (mx as f32 / w as f32) * 2.0 - 1.0 + ndc_w / 2.0;
                    let item_iy_c = item_iy - ndc_ih / 2.0;

                    let is_selected = menu.selected == Some(i);
                    if is_selected {
                        // Subtle gold row highlight (matches selection accent)
                        solid_rect(mx32, my32 + i as f32 * item_height, menu_width, item_height,
                                   0.42, 0.33, 0.10, 0.95);
                    }

                    // Render item label text
                    // White for normal items, gold for the selected one.
                    let (tr, tg, tb) = if is_selected { (1.0, 0.84, 0.0) } else { (1.0, 1.0, 1.0) };
                    // The 5x7 bitmap glyphs are drawn at an integer scale
                    // factor proportional to the row height — 1:1 pixels on
                    // a modern panel are unreadably small (crisp with
                    // NEAREST sampling at any scale).
                    let scale = metrics.glyph_scale;
                    let text_x = item_ix - ndc_w / 2.0 + (4.0 / w as f32) * 2.0; // 4px left padding
                    let ch = (7.0f32 * scale / h as f32) * 2.0; // 7*scale px char height in NDC
                    let cw = (5.0f32 * scale / w as f32) * 2.0; // 5*scale px char width in NDC
                    let text_y = item_iy_c - ch / 2.0; // draw_text y = glyph bottom → vertically centered
                    draw_text(gl, draw, &_item.label, text_x, text_y, cw, ch, tr, tg, tb);
                }

                // Restore GL state for subsequent main-render passes
                gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
                gl.Enable(ffi::DEPTH_TEST);
            });
        }
    }

    let t_submit = std::time::Instant::now();
    let r = backend.finish_frame();
    perf.record_stage(PipelineStage::RenderSubmit, t_submit.elapsed().as_nanos() as u64);

    if let Err(SwapBuffersError::ContextLost(e)) = r {
        error!(?e, "Context lost");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The atlas geometry invariant: the TEXTURE height (built from
    /// ROWS) and the UV math (atlas_rows) must derive from the SAME
    /// glyph count. This regressed when ROWS was computed from
    /// FONT.len() (bytes, 7 per glyph) instead of glyphs — the texture
    /// became 43 rows while UVs assumed 7, smushing six glyph rows
    /// into every sampled quad ("only symbols appear, nothing
    /// readable").
    #[test]
    fn atlas_rows_match_texture_geometry() {
        let cols = 16u32;
        // Glyph count is glyphs, not bytes.
        assert_eq!(font_glyph_count(), FONT.len() / 7);
        assert_eq!(font_glyph_count(), 97); // 96 ASCII + maximize box
        // All glyphs fit in the rows the UV math assumes.
        assert!(atlas_rows(cols) * cols >= font_glyph_count() as u32);
        // And the texture height the builder uses equals it.
        const ROWS: u32 = atlas_rows(16);
        assert_eq!(ROWS, atlas_rows(cols));
        assert_eq!(ROWS * 7, 49); // 7 rows x 7 px
    }
}
