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
#[allow(dead_code)] // reserved API surface; not yet wired
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
            0,
            x,
            y,
            w,
            h,
            ffi::BGRA_EXT,
            ffi::UNSIGNED_BYTE,
            data.as_ptr() as *const std::ffi::c_void,
        );
    });
}

/// Per-GL-context render caches (P2 #9): the shader programs/VAOs and
/// the font atlas are owned by the state that owns the GL context and
/// reset together with it on context loss — a fresh context can no
/// longer observe stale object IDs from a previous one.
#[derive(Default)]
pub struct RenderCaches {
    draw: Option<DrawGl>,
    font_atlas: Option<FontAtlas>,
    /// G-G6: EGL buffer preservation, probed once per context (caches
    /// die with the context, so a recreated context re-probes).
    preserved: Option<bool>,
    /// G-G6: previous frame's per-visual state for the scene diff.
    prev_frame: Option<PrevFrameState>,
}

/// G-G6: per-visual snapshot used to detect what changed since the
/// last presented frame. Appearance-affecting fields are all included:
/// a visual that changed in ANY of these must be redrawn (and its old
/// footprint damaged, else partial present leaves ghosts).
#[derive(Clone)]
struct PrevEntry {
    matrix: cgmath::Matrix4<f32>,
    gw: f32,
    gh: f32,
    drawn: bool,
    selected: bool,
    focused: bool,
    window_state: crate::scene::WindowState,
}

#[derive(Default)]
struct PrevFrameState {
    /// Per-output (view, fb w, fb h) — the partial-present camera/size
    /// guard is per output view, not per framebuffer.
    views: std::collections::HashMap<
        crate::outputs::OutputId,
        (cgmath::Matrix4<f32>, u32, u32),
    >,
    entries:
        std::collections::HashMap<(crate::outputs::OutputId, crate::scene::VisualId), PrevEntry>,
}

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
#[allow(clippy::too_many_arguments)] // wide GL/routing signatures are inherent
unsafe fn draw_text(
    gl: &ffi::Gles2,
    draw: &DrawGl,
    atlas: &FontAtlas,
    text: &str,
    x_ndc: f32,
    y_ndc: f32,
    char_w: f32,
    char_h: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
) {
    let (font_tex_id, gw, gh, cols) = (atlas.tex_id, atlas.gw, atlas.gh, atlas.cols);
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
    gl.Uniform4f(draw.u_src, 0.0, 0.0, 1.0, 1.0);
    // Glyph color comes from u_tint (the atlas ink is white; rgb carried
    // in .rgb, shape in .a). This finally applies the requested color.
    gl.Uniform4f(draw.u_tint, color_r, color_g, color_b, 1.0);
    gl.ActiveTexture(ffi::TEXTURE0);
    gl.BindTexture(ffi::TEXTURE_2D, font_tex_id);
    gl.Uniform1i(draw.u_tex, 0);
    gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);

    for (i, ch) in text.chars().enumerate() {
        let code = ch as u32;
        // Atlas holds 96 ASCII glyphs (32..=127) plus the custom
        // maximize-box sentinel at PUA U+E000. U+0080 and every other
        // non-ASCII codepoint is skipped — the old 32..=128 range
        // rendered client text containing U+0080 as the maximize box.
        if !((32..=127).contains(&code) || code == MAXIMIZE_GLYPH_CODE) {
            continue;
        }
        let idx = if code == MAXIMIZE_GLYPH_CODE {
            font_glyph_count() as u32 - 1
        } else {
            code - 32
        };
        let col = idx % cols;
        let row = idx / cols;
        let u = (col * gw) as f32 / atlas_w;
        let v = (row * gh) as f32 / atlas_h;
        let uw = gw as f32 / atlas_w;
        let vh = gh as f32 / atlas_h;

        let verts: [f32; 16] = [
            -0.5,
            -0.5,
            u,
            v + vh,
            0.5,
            -0.5,
            u + uw,
            v + vh,
            -0.5,
            0.5,
            u,
            v,
            0.5,
            0.5,
            u + uw,
            v,
        ];

        gl.BufferData(
            ffi::ARRAY_BUFFER,
            std::mem::size_of_val(&verts) as isize,
            verts.as_ptr() as *const std::ffi::c_void,
            ffi::STREAM_DRAW,
        );

        let cx = x_ndc + (i as f32) * char_w;
        let cy = y_ndc;
        let mvp = cgmath::Matrix4::from_translation(cgmath::Vector3::new(
            cx + char_w / 2.0,
            cy + char_h / 2.0,
            0.0,
        )) * cgmath::Matrix4::from_nonuniform_scale(char_w, char_h, 1.0);
        gl.UniformMatrix4fv(draw.u_mvp, 1, 0, mvp.as_ptr());

        gl.EnableVertexAttribArray(draw.a_pos);
        gl.VertexAttribPointer(draw.a_pos, 2, ffi::FLOAT, 0, stride, std::ptr::null());
        gl.EnableVertexAttribArray(draw.a_uv);
        gl.VertexAttribPointer(
            draw.a_uv,
            2,
            ffi::FLOAT,
            0,
            stride,
            (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void,
        );
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(draw.a_pos);
        gl.DisableVertexAttribArray(draw.a_uv);
    }

    // Restore main VBO
    let verts: [f32; 16] = [
        -0.5, -0.5, 0.0, 1.0, 0.5, -0.5, 1.0, 1.0, -0.5, 0.5, 0.0, 0.0, 0.5, 0.5, 1.0, 0.0,
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
#[allow(clippy::too_many_arguments)] // wide GL/routing signatures are inherent
unsafe fn draw_text_in_window(
    gl: &ffi::Gles2,
    draw: &DrawGl,
    atlas: &FontAtlas,
    text: &str,
    mats: (&Matrix4<f32>, &Matrix4<f32>),
    win: (f32, f32),
    run: (f32, f32, f32),
    color: (f32, f32, f32),
) {
    let (model, pv) = mats;
    let (gw, gh) = win;
    let (x_px, y_center_px, char_h_px) = run;
    let (font_tex_id, gw_atlas, gh_atlas, cols) = (atlas.tex_id, atlas.gw, atlas.gh, atlas.cols);
    let total_rows = atlas_rows(cols);
    let atlas_w = (cols * gw_atlas) as f32;
    let atlas_h = (total_rows * gh_atlas) as f32;

    let stride = 4 * std::mem::size_of::<f32>() as i32;
    gl.UseProgram(draw.program);
    // Depth-only pull toward the camera: glyph fragments beat their
    // own window quad on the depth tie (drawn after it at the same
    // plane), while genuinely-closer windows still occlude them —
    // a behind-window's title can no longer bleed through the window
    // in front. Position is untouched: chrome stays glued to the
    // window plane. Scope: the whole window-chrome run; the menu's
    // screen-space draw_text runs with depth disabled anyway.
    gl.Enable(ffi::POLYGON_OFFSET_FILL);
    gl.PolygonOffset(0.0, -1.0);
    gl.Uniform1f(draw.u_selected, 0.0);
    gl.Uniform1f(draw.u_focused, 0.0);
    gl.Uniform1f(draw.u_title_h, 0.0);
    gl.Uniform1f(draw.u_edge, 0.0);
    gl.Uniform4f(draw.u_src, 0.0, 0.0, 1.0, 1.0);
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
    // Chrome glyphs stay ON the window's plane (no z displacement — a
    // displaced glyph is depth-closer than every other window at that
    // plane, so titles bled through windows in front). Occlusion ties
    // are resolved with a depth-only polygon offset below.
    for (i, ch) in text.chars().enumerate() {
        let code = ch as u32;
        // Same atlas discipline as the overlay draw above: 96 ASCII
        // glyphs + the PUA maximize-box sentinel only.
        if !((32..=127).contains(&code) || code == MAXIMIZE_GLYPH_CODE) {
            continue;
        }
        let idx = if code == MAXIMIZE_GLYPH_CODE {
            font_glyph_count() as u32 - 1
        } else {
            code - 32
        };
        let col = idx % cols;
        let row = idx / cols;
        let u = (col * gw_atlas) as f32 / atlas_w;
        let v = (row * gh_atlas) as f32 / atlas_h;
        let uw = gw_atlas as f32 / atlas_w;
        let vh = gh_atlas as f32 / atlas_h;

        let verts: [f32; 16] = [
            -0.5,
            -0.5,
            u,
            v + vh,
            0.5,
            -0.5,
            u + uw,
            v + vh,
            -0.5,
            0.5,
            u,
            v,
            0.5,
            0.5,
            u + uw,
            v,
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
            0.0,
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
        gl.VertexAttribPointer(
            draw.a_uv,
            2,
            ffi::FLOAT,
            0,
            stride,
            (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void,
        );
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(draw.a_pos);
        gl.DisableVertexAttribArray(draw.a_uv);
    }

    gl.Disable(ffi::POLYGON_OFFSET_FILL);

    // Restore main VBO
    let verts: [f32; 16] = [
        -0.5, -0.5, 0.0, 1.0, 0.5, -0.5, 1.0, 1.0, -0.5, 0.5, 0.0, 0.0, 0.5, 0.5, 1.0, 0.0,
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
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 33 !
    0x20, 0x20, 0x20, 0x20, 0x00, 0x20, 0x00, // 34 "
    0x50, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // 35 #
    0x50, 0x50, 0xf8, 0x50, 0xf8, 0x50, 0x50, // 36 $
    0x20, 0x78, 0xa0, 0x70, 0x28, 0xf0, 0x20, // 37 %
    0x40, 0xa4, 0x48, 0x10, 0x24, 0x4a, 0x04, // 38 &
    0x60, 0x90, 0xa0, 0x40, 0xa8, 0x90, 0x68, // 39 '
    0x20, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, // 40 (
    0x10, 0x20, 0x40, 0x40, 0x40, 0x20, 0x10, // 41 )
    0x40, 0x20, 0x10, 0x10, 0x10, 0x20, 0x40, // 42 *
    0x00, 0x20, 0xa8, 0x70, 0xa8, 0x20, 0x00, // 43 +
    0x00, 0x20, 0x20, 0xf8, 0x20, 0x20, 0x00, // 44 ,
    0x00, 0x00, 0x00, 0x00, 0x20, 0x20, 0x40, // 45 -
    0x00, 0x00, 0x00, 0xf8, 0x00, 0x00, 0x00, // 46 .
    0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, // 47 /
    0x00, 0x08, 0x10, 0x20, 0x40, 0x80, 0x00, // 48 0
    0x70, 0x88, 0x98, 0xa8, 0xc8, 0x88, 0x70, // 49 1
    0x20, 0x60, 0xa0, 0x20, 0x20, 0x20, 0xf8, // 50 2
    0x70, 0x88, 0x08, 0x10, 0x20, 0x40, 0xf8, // 51 3
    0x70, 0x88, 0x08, 0x30, 0x08, 0x88, 0x70, // 52 4
    0x10, 0x30, 0x50, 0x90, 0xf8, 0x10, 0x10, // 53 5
    0xf8, 0x80, 0xf0, 0x08, 0x08, 0x88, 0x70, // 54 6
    0x30, 0x40, 0x80, 0xf0, 0x88, 0x88, 0x70, // 55 7
    0xf8, 0x08, 0x10, 0x20, 0x40, 0x40, 0x40, // 56 8
    0x70, 0x88, 0x88, 0x70, 0x88, 0x88, 0x70, // 57 9
    0x70, 0x88, 0x88, 0x78, 0x08, 0x10, 0x60, // 58 :
    0x00, 0x20, 0x00, 0x00, 0x00, 0x20, 0x00, // 59 ;
    0x00, 0x20, 0x00, 0x00, 0x20, 0x20, 0x40, // 60 <
    0x00, 0x08, 0x10, 0x20, 0x10, 0x08, 0x00, // 61 =
    0x00, 0x00, 0xf8, 0x00, 0xf8, 0x00, 0x00, // 62 >
    0x00, 0x80, 0x40, 0x20, 0x40, 0x80, 0x00, // 63 ?
    0x70, 0x88, 0x08, 0x10, 0x20, 0x00, 0x20, // 64 @
    0x70, 0x88, 0xb8, 0xa8, 0xb0, 0x80, 0x78, // 65 A
    0x20, 0x50, 0x88, 0x88, 0xf8, 0x88, 0x88, // 66 B
    0xf0, 0x88, 0x88, 0xf0, 0x88, 0x88, 0xf0, // 67 C
    0x70, 0x88, 0x80, 0x80, 0x80, 0x88, 0x70, // 68 D
    0xf0, 0x88, 0x88, 0x88, 0x88, 0x88, 0xf0, // 69 E
    0xf8, 0x80, 0x80, 0xf0, 0x80, 0x80, 0xf8, // 70 F
    0xf8, 0x80, 0x80, 0xf0, 0x80, 0x80, 0x80, // 71 G
    0x78, 0x80, 0x80, 0x98, 0x88, 0x88, 0x78, // 72 H
    0x88, 0x88, 0x88, 0xf8, 0x88, 0x88, 0x88, // 73 I
    0xf8, 0x20, 0x20, 0x20, 0x20, 0x20, 0xf8, // 74 J
    0x08, 0x08, 0x08, 0x08, 0x08, 0x88, 0x70, // 75 K
    0x88, 0x90, 0xa0, 0xc0, 0xa0, 0x90, 0x88, // 76 L
    0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0xf8, // 77 M
    0x88, 0xd8, 0xa8, 0x88, 0x88, 0x88, 0x88, // 78 N
    0x88, 0xc8, 0xa8, 0x98, 0x88, 0x88, 0x88, // 79 O
    0x70, 0x88, 0x88, 0x88, 0x88, 0x88, 0x70, // 80 P
    0xf0, 0x88, 0x88, 0xf0, 0x80, 0x80, 0x80, // 81 Q
    0x70, 0x88, 0x88, 0x88, 0xa8, 0x90, 0x68, // 82 R
    0xf0, 0x88, 0x88, 0xf0, 0xa0, 0x90, 0x88, // 83 S
    0x70, 0x88, 0x80, 0x70, 0x08, 0x88, 0x70, // 84 T
    0xf8, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, // 85 U
    0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x70, // 86 V
    0x88, 0x88, 0x88, 0x88, 0x50, 0x50, 0x20, // 87 W
    0x88, 0x88, 0x88, 0xa8, 0xa8, 0xd8, 0x88, // 88 X
    0x88, 0x88, 0x50, 0x20, 0x50, 0x88, 0x88, // 89 Y
    0x88, 0x88, 0x50, 0x20, 0x20, 0x20, 0x20, // 90 Z
    0xf8, 0x08, 0x10, 0x20, 0x40, 0x80, 0xf8, // 91 [
    0x70, 0x40, 0x40, 0x40, 0x40, 0x40, 0x70, // 92 backslash
    0x00, 0x80, 0x40, 0x20, 0x10, 0x08, 0x00, // 93 ]
    0x70, 0x10, 0x10, 0x10, 0x10, 0x10, 0x70, // 94 ^
    0x20, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, // 95 _
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, // 96 `
    0x40, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, // 97 a
    0x00, 0x00, 0x70, 0x08, 0x78, 0x88, 0x78, // 98 b
    0x80, 0x80, 0xf0, 0x88, 0x88, 0x88, 0xf0, // 99 c
    0x00, 0x00, 0x70, 0x88, 0x80, 0x88, 0x70, // 100 d
    0x08, 0x08, 0x78, 0x88, 0x88, 0x88, 0x78, // 101 e
    0x00, 0x00, 0x70, 0x88, 0xf8, 0x80, 0x78, // 102 f
    0x30, 0x48, 0x40, 0xe0, 0x40, 0x40, 0x40, // 103 g
    0x00, 0x00, 0x78, 0x88, 0x78, 0x08, 0x70, // 104 h
    0x80, 0x80, 0xf0, 0x88, 0x88, 0x88, 0x88, // 105 i
    0x20, 0x00, 0x60, 0x20, 0x20, 0x20, 0x70, // 106 j
    0x10, 0x00, 0x30, 0x10, 0x10, 0x90, 0x60, // 107 k
    0x80, 0x80, 0x88, 0x90, 0xe0, 0x90, 0x88, // 108 l
    0x60, 0x20, 0x20, 0x20, 0x20, 0x20, 0x70, // 109 m
    0x00, 0x00, 0xd0, 0xa8, 0xa8, 0x88, 0x88, // 110 n
    0x00, 0x00, 0xf0, 0x88, 0x88, 0x88, 0x88, // 111 o
    0x00, 0x00, 0x70, 0x88, 0x88, 0x88, 0x70, // 112 p
    0x00, 0x00, 0xf0, 0x88, 0xf0, 0x80, 0x80, // 113 q
    0x00, 0x00, 0x78, 0x88, 0x78, 0x08, 0x08, // 114 r
    0x00, 0x00, 0xb0, 0xc8, 0x80, 0x80, 0x80, // 115 s
    0x00, 0x00, 0x78, 0x80, 0x70, 0x08, 0xf0, // 116 t
    0x40, 0x40, 0xf0, 0x40, 0x40, 0x48, 0x30, // 117 u
    0x00, 0x00, 0x88, 0x88, 0x88, 0x88, 0x78, // 118 v
    0x00, 0x00, 0x88, 0x88, 0x88, 0x50, 0x20, // 119 w
    0x00, 0x00, 0x88, 0x88, 0xa8, 0xa8, 0x50, // 120 x
    0x00, 0x00, 0x88, 0x50, 0x20, 0x50, 0x88, // 121 y
    0x00, 0x00, 0x88, 0x88, 0x78, 0x08, 0x70, // 122 z
    0x00, 0x00, 0xf8, 0x10, 0x20, 0x40, 0xf8, // 123 {
    0x18, 0x20, 0x20, 0xc0, 0x20, 0x20, 0x18, // 124 |
    0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, // 125 }
    0xc0, 0x20, 0x20, 0x18, 0x20, 0x20, 0xc0, // 126 ~
    0x00, 0x00, 0x40, 0xa8, 0x10, 0x00, 0x00,
    // 127 DEL (placeholder keeps ASCII codes aligned with indices)
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // Custom hollow box — title-bar maximize button (J3). Lives at the
    // PUA sentinel MAXIMIZE_GLYPH_CODE (U+E000) so client text can never
    // collide with it; referenced via char::from_u32(MAXIMIZE_GLYPH_CODE).
    0xf8, 0x88, 0x88, 0x88, 0x88, 0x88, 0xf8,
];

/// Custom glyph sentinel: atlas index 96 (after the 96 ASCII glyphs
/// 32..=127). Private Use Area — real text never contains it.
pub const MAXIMIZE_GLYPH_CODE: u32 = 0xE000;

pub const fn font_glyph_count() -> usize {
    FONT.len() / 7
}

pub const fn atlas_rows(cols: u32) -> u32 {
    (font_glyph_count() as u32).div_ceil(cols)
}

unsafe fn new_font_atlas(gl: &ffi::Gles2) -> FontAtlas {
    const GW: u32 = 5;
    const GH: u32 = 7;
    const COLS: u32 = 16;
    const ATLAS_W: u32 = COLS * GW;
    // Rows derive from the glyph count so appended custom glyphs
    // (MAXIMIZE_GLYPH_CODE = maximize box) extend the atlas automatically.
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
    gl.TexParameteri(
        ffi::TEXTURE_2D,
        ffi::TEXTURE_MIN_FILTER,
        ffi::NEAREST as i32,
    );
    gl.TexParameteri(
        ffi::TEXTURE_2D,
        ffi::TEXTURE_MAG_FILTER,
        ffi::NEAREST as i32,
    );
    gl.TexParameteri(
        ffi::TEXTURE_2D,
        ffi::TEXTURE_WRAP_S,
        ffi::CLAMP_TO_EDGE as i32,
    );
    gl.TexParameteri(
        ffi::TEXTURE_2D,
        ffi::TEXTURE_WRAP_T,
        ffi::CLAMP_TO_EDGE as i32,
    );

    FontAtlas {
        tex_id: tex,
        gw: GW,
        gh: GH,
        cols: COLS,
    }
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
uniform vec4 u_src;
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
            // G-C3: wp_viewporter.src crop — normalized source window
            // over the client texture (identity when no viewport is set).
            vec2 suv = content_uv * u_src.zw + u_src.xy;
            if (any(edge)) {
                if (u_selected > 0.5) {
                    gl_FragColor = vec4(0.62, 0.50, 0.10, 1.0);
                } else if (u_focused > 0.5) {
                    gl_FragColor = vec4(0.24, 0.52, 0.24, 1.0);
                } else {
                    gl_FragColor = vec4(0.16, 0.26, 0.27, 1.0);
                }
            } else {
                gl_FragColor = texture2D(u_tex, suv) * u_tint;
            }
        }
}
";

#[allow(dead_code)] // reserved API surface; not yet wired
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
    u_src: i32,
    /// Solid-color overlay program (no texture, no window chrome semantics).
    solid_prog: u32,
    solid_a_pos: u32,
    solid_a_uv: u32,
    solid_u_mvp: i32,
    solid_u_color: i32,
    round_prog: u32,
    round_u_mvp: i32,
    round_u_color: i32,
    round_u_color2: i32,
    round_u_size: i32,
    round_u_radius: i32,
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

/// Rounded-rect fragment shader (SDF, px-space). The quad is the rect's
/// bounding box; `u_size` is the rect size in px, `u_radius` the corner
/// radius. Vertical gradient mixes u_color (top) into u_color2 (bottom).
/// 1px analytic AA without OES_standard_derivatives: the SDF distance is
/// already in px, so `0.5 - dist` is a one-pixel smooth edge.
const ROUND_FS: &str = "\
precision mediump float;
varying vec2 v_uv;
uniform vec4 u_color;
uniform vec4 u_color2;
uniform vec2 u_size;
uniform float u_radius;
void main() {
    vec2 p = v_uv * u_size;
    vec2 half_size = u_size * 0.5;
    vec2 corner = half_size - vec2(u_radius);
    vec2 d = abs(p - half_size) - corner;
    float dist = length(max(d, 0.0)) + min(max(d.x, d.y), 0.0) - u_radius;
    float alpha = clamp(0.5 - dist, 0.0, 1.0);
    vec3 col = mix(u_color.rgb, u_color2.rgb, v_uv.y);
    gl_FragColor = vec4(col, u_color.a * alpha);
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
        let a_pos = unsafe { gl.GetAttribLocation(program, c"a_pos".as_ptr()) as u32 };
        let a_uv = unsafe { gl.GetAttribLocation(program, c"a_uv".as_ptr()) as u32 };
        let u_mvp = unsafe { gl.GetUniformLocation(program, c"u_mvp".as_ptr()) };
        let u_tex = unsafe { gl.GetUniformLocation(program, c"u_tex".as_ptr()) };
        let u_selected = unsafe { gl.GetUniformLocation(program, c"u_selected".as_ptr()) };
        let u_focused = unsafe { gl.GetUniformLocation(program, c"u_focused".as_ptr()) };
        let u_title_h = unsafe { gl.GetUniformLocation(program, c"u_title_h".as_ptr()) };
        let u_edge = unsafe { gl.GetUniformLocation(program, c"u_edge".as_ptr()) };
        let u_tint = unsafe { gl.GetUniformLocation(program, c"u_tint".as_ptr()) };
        let u_border = unsafe { gl.GetUniformLocation(program, c"u_border".as_ptr()) };
        let u_src = unsafe { gl.GetUniformLocation(program, c"u_src".as_ptr()) };
        let mut vbo = 0;
        unsafe { gl.GenBuffers(1, &mut vbo) };
        let verts: [f32; 16] = [
            -0.5, -0.5, 0.0, 1.0, 0.5, -0.5, 1.0, 1.0, -0.5, 0.5, 0.0, 0.0, 0.5, 0.5, 1.0, 0.0,
        ];
        unsafe {
            gl.BindBuffer(ffi::ARRAY_BUFFER, vbo);
            gl.BufferData(
                ffi::ARRAY_BUFFER,
                std::mem::size_of_val(&verts) as isize,
                verts.as_ptr() as *const std::ffi::c_void,
                ffi::STATIC_DRAW,
            );
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
        let text_a_pos = unsafe { gl.GetAttribLocation(text_prog, c"a_pos".as_ptr()) as u32 };
        let text_a_uv = unsafe { gl.GetAttribLocation(text_prog, c"a_uv".as_ptr()) as u32 };
        let text_u_mvp = unsafe { gl.GetUniformLocation(text_prog, c"u_mvp".as_ptr()) };
        let text_u_tex = unsafe { gl.GetUniformLocation(text_prog, c"u_tex".as_ptr()) };
        let text_u_color = unsafe { gl.GetUniformLocation(text_prog, c"u_color".as_ptr()) };
        let mut text_vbo = 0;
        unsafe { gl.GenBuffers(1, &mut text_vbo) };
        let text_verts: [f32; 16] = [
            -0.5, -0.5, 0.0, 1.0, 0.5, -0.5, 1.0, 1.0, -0.5, 0.5, 0.0, 0.0, 0.5, 0.5, 1.0, 0.0,
        ];
        unsafe {
            gl.BindBuffer(ffi::ARRAY_BUFFER, text_vbo);
            gl.BufferData(
                ffi::ARRAY_BUFFER,
                std::mem::size_of_val(&text_verts) as isize,
                text_verts.as_ptr() as *const std::ffi::c_void,
                ffi::STATIC_DRAW,
            );
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

        // Rounded-rect overlay shader (taskbar buttons, pills).
        let rvs = Self::compile(gl, ffi::VERTEX_SHADER, QUAD_VS);
        let rfs = Self::compile(gl, ffi::FRAGMENT_SHADER, ROUND_FS);
        let round_prog = unsafe { gl.CreateProgram() };
        unsafe {
            gl.AttachShader(round_prog, rvs);
            gl.AttachShader(round_prog, rfs);
            gl.LinkProgram(round_prog);
            gl.DeleteShader(rvs);
            gl.DeleteShader(rfs);
        }
        let round_u_mvp = unsafe { gl.GetUniformLocation(round_prog, c"u_mvp".as_ptr()) };
        let round_u_color = unsafe { gl.GetUniformLocation(round_prog, c"u_color".as_ptr()) };
        let round_u_color2 = unsafe { gl.GetUniformLocation(round_prog, c"u_color2".as_ptr()) };
        let round_u_size = unsafe { gl.GetUniformLocation(round_prog, c"u_size".as_ptr()) };
        let round_u_radius = unsafe { gl.GetUniformLocation(round_prog, c"u_radius".as_ptr()) };

        DrawGl {
            program,
            a_pos,
            a_uv,
            u_mvp,
            u_tex,
            u_selected,
            u_focused,
            u_title_h,
            u_edge,
            u_tint,
            u_border,
            u_src,
            solid_prog,
            solid_a_pos,
            solid_a_uv,
            solid_u_mvp,
            solid_u_color,
            round_prog,
            round_u_mvp,
            round_u_color,
            round_u_color2,
            round_u_size,
            round_u_radius,
            vbo,
            text_prog,
            text_a_pos,
            text_a_uv,
            text_u_mvp,
            text_u_tex,
            text_u_color,
            text_vbo,
        }
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
            unsafe {
                gl.GetShaderInfoLog(s, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut i8)
            };
            error!("Shader error: {}", String::from_utf8_lossy(&buf));
        }
        s
    }
}

#[allow(clippy::too_many_arguments)] // wide GL/routing signatures are inherent
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
    src_uv: [f32; 4],
    edge_enabled: bool,
) {
    unsafe {
        gl.UseProgram(draw.program);
        gl.UniformMatrix4fv(draw.u_mvp, 1, 0, mvp.as_ptr());
        gl.Uniform1f(draw.u_selected, if selected { 1.0 } else { 0.0 });
        gl.Uniform1f(draw.u_focused, if focused { 1.0 } else { 0.0 });
        gl.Uniform1f(draw.u_title_h, title_h);
        // Parented visuals (subsurfaces, IME popups) are raw client
        // content: no veyra chrome ring, no title strip carve — the
        // texture fills the whole quad.
        gl.Uniform1f(draw.u_edge, if edge_enabled { 1.0 } else { 0.0 });
        gl.Uniform4f(draw.u_tint, 1.0, 1.0, 1.0, 1.0);
        gl.Uniform4f(draw.u_src, src_uv[0], src_uv[1], src_uv[2], src_uv[3]);
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
        gl.VertexAttribPointer(
            draw.a_uv,
            2,
            ffi::FLOAT,
            0,
            stride,
            (2 * std::mem::size_of::<f32>()) as *const std::ffi::c_void,
        );
        gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
        gl.DisableVertexAttribArray(draw.a_pos);
        gl.DisableVertexAttribArray(draw.a_uv);
    }
}

/// Screen-space overlay state for one frame (J4): the shell taskbar
/// and the context menu. Both are camera-independent 2D planes drawn
/// after the 3D world.
pub struct Overlays<'a> {
    pub context_menu: Option<&'a ContextMenu>,
    pub taskbar: Option<&'a crate::shell::TaskbarLayout>,
}

/// R1 contract: DRAW-ONLY. The frame lifecycle (begin_frame /
/// finish_frame) is owned by the caller — LookingGlass::render — so
/// each frame is made current and submitted exactly once, and
/// presentation errors propagate to it.
#[allow(clippy::too_many_arguments)] // wide GL/routing signatures are inherent
/// G-G6: framebuffer-space rect [x0, y0, x1, y1] union. Empty input →
/// None.
fn union_rect(rects: &[[f32; 4]]) -> Option<[f32; 4]> {
    let mut it = rects.iter();
    let first = *it.next()?;
    let mut u = first;
    for r in it {
        u[0] = u[0].min(r[0]);
        u[1] = u[1].min(r[1]);
        u[2] = u[2].max(r[2]);
        u[3] = u[3].max(r[3]);
    }
    Some(u)
}

/// G-G6: whether the frame can present partially. Requires preserved
/// buffers, a known previous frame at the same size with an unmoved
/// camera (view identical), actual damage, and conservative coverage.
fn decide_partial(
    preserved: bool,
    prev_view: Option<&(cgmath::Matrix4<f32>, u32, u32)>,
    view: &cgmath::Matrix4<f32>,
    fb: (u32, u32),
    damage: Option<[f32; 4]>,
) -> bool {
    if !preserved {
        return false;
    }
    let Some((prev_m, pw, ph)) = prev_view else {
        return false;
    };
    if (*pw, *ph) != fb || fb.0 == 0 || fb.1 == 0 {
        return false;
    }
    // Camera moved since the last present → parallax invalidates
    // everything: full frame.
    if prev_m != view {
        return false;
    }
    let Some(d) = damage else {
        return false;
    };
    let area = (d[2] - d[0]) * (d[3] - d[1]);
    let fb_area = (fb.0 as f32) * (fb.1 as f32);
    // Conservative: only bother below 70% coverage; above that the
    // scissor bookkeeping costs more than the fill it saves.
    fb_area > 0.0 && area / fb_area <= 0.7
}

/// G-G6/E5.5: screen-space AABB of a visual's quad under `mvp`, or
/// None when fully behind the camera. Coordinates land in the output's
/// VIEWPORT rect (vx, vy offset, vw/vh extent) — the mapping is the
/// exact inverse of the input path's fb→output-local conversion.
fn quad_screen_aabb(
    mvp: &cgmath::Matrix4<f32>,
    gw: f32,
    gh: f32,
    vx: i32,
    vy: i32,
    vw: f32,
    vh: f32,
) -> Option<[f32; 4]> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut any = false;
    for (cx, cy) in [
        (-gw / 2.0, -gh / 2.0),
        (gw / 2.0, -gh / 2.0),
        (gw / 2.0, gh / 2.0),
        (-gw / 2.0, gh / 2.0),
    ] {
        let p = mvp * cgmath::Vector4::new(cx, cy, 0.0, 1.0);
        if p.w <= 0.0 {
            continue;
        }
        let inv_w = 1.0 / p.w;
        let sx = vx as f32 + (p.x * inv_w * 0.5 + 0.5) * vw;
        let sy = vy as f32 + (-p.y * inv_w * 0.5 + 0.5) * vh;
        min_x = min_x.min(sx);
        min_y = min_y.min(sy);
        max_x = max_x.max(sx);
        max_y = max_y.max(sy);
        any = true;
    }
    if !any {
        return None;
    }
    Some([min_x, min_y, max_x, max_y])
}

/// G-G6: query EGL buffer preservation on the presentation surface.
/// READ-ONLY: the swap behavior is a property of the EGLConfig the
/// surface was created with (EGL_SWAP_BEHAVIOR_PRESERVED_BIT). We must
/// NOT eglSurfaceAttrib it on — setting it on an unqualified config is
/// spec-invalid and breaks llvmpipe's first swap with BadAlloc.
/// Drivers that natively preserve get partial presents; everyone else
/// falls back to full-frame (always correct).
fn probe_buffer_preserved(
    renderer: &mut GlesRenderer,
    surface_ptr: *const smithay::backend::egl::EGLSurface,
) -> bool {
    use smithay::backend::egl::ffi::egl as eglffi;
    unsafe {
        let ctx = renderer.egl_context();
        let display = ctx.display().get_display_handle().handle;
        let surface = (&*surface_ptr).get_surface_handle();
        let mut v = 0i32;
        let ok =
            eglffi::QuerySurface(display, surface, eglffi::SWAP_BEHAVIOR as i32, &mut v)
                == eglffi::TRUE;
        ok && v == eglffi::BUFFER_PRESERVED as i32
    }
}

#[allow(clippy::too_many_arguments)] // one frame's inputs; grouping would obscure the GL call sites
pub fn render_scene(
    backend: &mut dyn PresentationBackend,
    scene: &Scene,
    plans: &[crate::outputs::OutputFramePlan],
    perf: &mut PerfStats,
    visible_ids: Option<&[crate::scene::VisualId]>,
    overlays: &Overlays,
    caches: &mut RenderCaches,
    updated_ids: &[crate::scene::VisualId],
    reports: &mut Vec<crate::outputs::OutputFrameReport>,
) -> Result<(), SwapBuffersError> {
    use crate::perf::PipelineStage;

    let (w, h) = backend.size();

    // G-F3 (P2 #9 remainder): the EGL binding pair is owned by ONE
    // type with a documented invariant instead of two loose raw
    // pointers. The unsafety is irreducible with smithay's current
    // API: `GlesRenderer::with_context` makes current with
    // EGL_NO_SURFACE (backend/egl/context.rs:363), so every raw-GL
    // closure drawing to the default framebuffer must re-make-current
    // with the presentation surface — but the closure already holds
    // `&ffi::Gles2` borrowed from the renderer borrowed from the
    // backend that owns the surface. The pointers are valid for the
    // whole render_scene body (the backend outlives it) and never
    // stored. Rebind failures are ERRORS, not panics: a lost context
    // surfaces at frame submission and the G-E5 recovery path takes
    // over.
    struct SurfaceBinding {
        ctx: *const smithay::backend::egl::EGLContext,
        surface: Option<*const smithay::backend::egl::EGLSurface>,
    }
    impl SurfaceBinding {
        /// SAFETY: both pointers must outlive every `rebind` call —
        /// guaranteed by render_scene's borrow structure (the backend
        /// binding outlives the function; nothing mutates the EGL
        /// objects during rendering).
        fn rebind(&self, gl: &ffi::Gles2) -> Result<(), SwapBuffersError> {
            if let Some(surface_ptr) = self.surface {
                unsafe {
                    let surface = &*surface_ptr;
                    let ctx = &*self.ctx;
                    ctx.make_current_with_surface(surface).map_err(|_| {
                        SwapBuffersError::ContextLost(
                            "surface rebind failed (context lost)".into(),
                        )
                    })?;
                }
                unsafe { gl.BindFramebuffer(ffi::FRAMEBUFFER, 0) };
            }
            Ok(())
        }
    }

    // Stash the surface pointer BEFORE borrowing renderer (borrows backend).
    let surface_ptr: Option<*const smithay::backend::egl::EGLSurface> =
        backend.egl_surface().map(|s| s as *const _);
    // G-G6: read the probe gate before the renderer borrow (the flag
    // is backend-level state, needed later past the mutable borrow).
    let probe_allowed = backend.preservation_probe_allowed();
    let renderer = backend.renderer();
    let binding = SurfaceBinding {
        ctx: renderer.egl_context(),
        surface: surface_ptr,
    };

    // Helper to rebind the window surface as the current draw/read target.
    // Must be called inside each with_context() closure before any GL
    // operations. Failure is logged once per frame; the frame fails
    // downstream at submit (context-loss handling takes over).
    //
    // G-E5.5: each rebind is a fresh context→surface bind, and EGL
    // resets viewport/scissor to the SURFACE size on every fresh bind —
    // which silently reverted the active plan's viewport (multi-output
    // quads rendered at full-framebuffer scale). Re-assert the active
    // viewport after every rebind.
    let mut rebind_failed = false;
    let current_viewport: std::cell::Cell<[i32; 4]> = std::cell::Cell::new([0, 0, 0, 0]);
    let mut rebind_surface = |gl: &ffi::Gles2| {
        if binding.rebind(gl).is_err() && !rebind_failed {
            rebind_failed = true;
            tracing::error!("surface rebind failed — frame will not present");
        }
        let v = current_viewport.get();
        unsafe { gl.Viewport(v[0], v[1], v[2], v[3]) };
    };

    // Initialize the per-context caches inside the current GL context
    // (P2 #9): DrawGl programs/VAOs and the font atlas live and die
    // with the context that created them.
    let RenderCaches {
        draw,
        font_atlas,
        preserved,
        prev_frame,
    } = &mut *caches;
    let _ = renderer.with_context(|gl| {
        rebind_surface(gl);
        if draw.is_none() {
            *draw = Some(DrawGl::new(gl));
        }
        if font_atlas.is_none() {
            *font_atlas = Some(unsafe { new_font_atlas(gl) });
        }
    });
    // G-G6: probe buffer preservation once per context (needs the
    // context CURRENT? No — eglSurfaceAttrib/QuerySurface are context-
    // independent; run outside with_context to keep the borrow simple).
    if preserved.is_none() {
        // Probe only where the backend opts in (native DRM). On the
        // nested llvmpipe stack even read-only eglQuerySurface corrupts
        // the next swap (BadAlloc) — winit presents full-frame.
        let probed = if probe_allowed {
            surface_ptr
                .map(|ptr| probe_buffer_preserved(renderer, ptr))
                .unwrap_or(false)
        } else {
            false
        };
        *preserved = Some(probed);
        if probed {
            tracing::info!("G-G6: EGL buffer preservation available natively — partial presents enabled");
        } else {
            tracing::debug!("G-G6: EGL buffer preservation unavailable — full-frame presents");
        }
    }
    let preserved = preserved.unwrap_or(false);
    let (draw, atlas) = match (draw.as_ref(), font_atlas.as_ref()) {
        (Some(d), Some(a)) => (d, a),
        _ => {
            error!("render caches not initialized");
            return Ok(());
        }
    };

    // G-E5.5.2: one shared scene, N output views. Each plan sets its
    // own viewport (confines all drawing), clears ONLY its own rect
    // (scissored — one output's clear must never erase another), and
    // draws the scene through ITS camera and ITS viewport-size
    // projection. Viewports derive from the registry (OutputFramePlan);
    // nothing here derives geometry ad hoc.
    let mut any_partial = false;
    // Frame-level clear FIRST: regions between output viewports (gaps
    // in non-adjacent tiling) must never hold stale swapchain content.
    // Per-output scissored clears below still prove viewport
    // independence — draws happen after all clears.
    current_viewport.set([0, 0, w as i32, h as i32]);
    let _ = renderer.with_context(|gl| unsafe {
        rebind_surface(gl);
        gl.Disable(ffi::SCISSOR_TEST);
        gl.ClearColor(0.08, 0.08, 0.08, 1.0);
        gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
    });
    for plan in plans {
        // Set up viewport, clear, and state in one with_context block
        let t_clear = std::time::Instant::now();
        // G-G6 pre-pass: per-visual state diff against the previous frame.
        // The renderer-side diff is authoritative for partial presents —
        // it catches everything that changes appearance (transforms, size,
        // selection, focus, visibility, content swaps) without relying on
        // DamageKind discipline at every mutation site.
        let pv = plan.proj * plan.view;
        let (vx, vy, vw, vh) = (
            plan.viewport.x,
            plan.viewport.y,
            plan.viewport.width as f32,
            plan.viewport.height as f32,
        );
        // GL viewport/scissor origins are BOTTOM-LEFT; plan viewports
        // are TOP-LEFT framebuffer rects. (Also: EGL fresh binds reset
        // viewport/scissor — rebind_surface re-asserts these GL coords.)
        let gl_y = h as i32 - (vy + vh as i32);
        current_viewport.set([vx, gl_y, vw as i32, vh as i32]);
        let visible_set: Option<std::collections::HashSet<crate::scene::VisualId>> =
            visible_ids.map(|ids| ids.iter().copied().collect());
        let content_changed: std::collections::HashSet<crate::scene::VisualId> =
            updated_ids.iter().copied().collect();
        let prev = prev_frame.take();
        let mut entries: std::collections::HashMap<
            (crate::outputs::OutputId, crate::scene::VisualId),
            PrevEntry,
        > = std::collections::HashMap::with_capacity(scene.visuals.len());
        let mut pre: std::collections::HashMap<crate::scene::VisualId, cgmath::Matrix4<f32>> =
            std::collections::HashMap::with_capacity(scene.visuals.len());
        let mut damage_rects: Vec<[f32; 4]> = Vec::new();
        for visual in scene.iter() {
            let world = scene.world_matrix(visual.id);
            let gw = visual.total_width();
            let gh = visual.total_height();
            if std::env::var("VEYRA_DEBUG_VIEWPORT").is_ok() && visual.parent.is_none() {
                let pos = visual.transform.position;
                tracing::debug!(
                    plan_out = plan.output_id.0,
                    vp = ?(vx, vy, vw as u32, vh as u32),
                    world_pos = ?(pos.x, pos.y, pos.z),
                    gw, gh,
                    "E5.5 plan/visual geometry"
                );
            }
            let drawn = visual.window_state != crate::scene::WindowState::Minimized
                && visible_set.as_ref().is_none_or(|s| s.contains(&visual.id))
                && visual.texture().is_some();
            let cur_aabb = if drawn {
                quad_screen_aabb(&(pv * world), gw, gh, vx, vy, vw, vh)
            } else {
                None
            };
            if let Some(prev_state) = &prev {
                match prev_state.entries.get(&(plan.output_id, visual.id)) {
                    Some(old) => {
                        let changed = old.matrix != world
                            || old.gw != gw
                            || old.gh != gh
                            || old.selected != visual.selected
                            || old.focused != visual.focused
                            || old.window_state != visual.window_state
                            || old.drawn != drawn
                            || content_changed.contains(&visual.id);
                        if changed {
                            if old.drawn {
                                if let Some(old_aabb) =
                                    quad_screen_aabb(&(pv * old.matrix), old.gw, old.gh, vx, vy, vw, vh)
                                {
                                    damage_rects.push(old_aabb);
                                }
                            }
                            if let Some(a) = cur_aabb {
                                damage_rects.push(a);
                            }
                        }
                    }
                    None => {
                        if let Some(a) = cur_aabb {
                            damage_rects.push(a);
                        }
                    }
                }
            }
            entries.insert(
                (plan.output_id, visual.id),
                PrevEntry {
                    matrix: world,
                    gw,
                    gh,
                    drawn,
                    selected: visual.selected,
                    focused: visual.focused,
                    window_state: visual.window_state,
                },
            );
            pre.insert(visual.id, world);
        }
        // Visuals present last frame and gone now: damage their footprint.
        if let Some(prev_state) = &prev {
            for ((out_id, vid), old) in &prev_state.entries {
                if *out_id == plan.output_id && !entries.contains_key(&(*out_id, *vid)) && old.drawn {
                    if let Some(a) =
                        quad_screen_aabb(&(pv * old.matrix), old.gw, old.gh, vx, vy, vw, vh)
                    {
                        damage_rects.push(a);
                    }
                }
            }
        }
        let prev_view = prev
            .as_ref()
            .and_then(|p| p.views.get(&plan.output_id));
        let damage_union = if prev_view.is_some() {
            union_rect(&damage_rects)
        } else {
            None
        };
        let partial = decide_partial(
            preserved,
            prev_view,
            &plan.view,
            (vw as u32, vh as u32),
            damage_union,
        );
        let _ = renderer.with_context(|gl| unsafe {
            rebind_surface(gl);
            // E5.5.4: this output's viewport confines ALL of its
            // drawing (GL viewport); its clear is SCISSORED to the
            // same rect so one output's clear can never erase
            // another's pixels.
            gl.Viewport(vx, gl_y, vw as i32, vh as i32);
            gl.Enable(ffi::SCISSOR_TEST);
            gl.Scissor(vx, gl_y, vw as i32, vh as i32);
            gl.ClearColor(0.15, 0.15, 0.15, 1.0);
            if partial {
                // Clear and draw ONLY the damaged region; the preserved
                // back buffer retains the previous frame elsewhere.
                let d = damage_union.unwrap();
                // Damage rects are top-left fb coords → GL bottom-left.
                let x = d[0].floor().max(vx as f32) as i32;
                let top = d[1].floor().max(vy as f32) as i32;
                let right = d[2].ceil().min(vx as f32 + vw) as i32;
                let bottom = d[3].ceil().min(vy as f32 + vh) as i32;
                let y = h as i32 - bottom;
                let rw = (right - x).max(0);
                let rh = (bottom - top).max(0);
                if rw > 0 && rh > 0 {
                    gl.Scissor(x, y, rw, rh);
                }
            }
            gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
            // Drawing is confined by the VIEWPORT; the scissor only
            // governs the clear. Release it before the draw loop.
            gl.Disable(ffi::SCISSOR_TEST);
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
            gl.Enable(ffi::DEPTH_TEST);
            gl.DepthFunc(ffi::LESS);
        });
        if partial {
            perf.record_partial();
        }
        // G-G6: store this frame's per-output state as the next diff
        // baseline.
        let pf = prev_frame.get_or_insert_with(Default::default);
        pf.views
            .insert(plan.output_id, (plan.view, vw as u32, vh as u32));
        for ((oid, vid), entry) in entries {
            pf.entries.insert((oid, vid), entry);
        }
        perf.record_stage(
            PipelineStage::RenderDraw,
            t_clear.elapsed().as_nanos() as u64,
        );

        // Draw all visuals
        let t_draw = std::time::Instant::now();
        for visual in scene.iter() {
            if visual.window_state == crate::scene::WindowState::Minimized {
                continue;
            }
            if let Some(set) = &visible_set {
                if !set.contains(&visual.id) {
                    continue;
                }
            }
            let Some(texture) = visual.texture() else {
                continue;
            };
            let tex_id = texture.tex_id();
            let gw = visual.total_width();
            let gh = visual.total_height();
            // G-G2: conservative frustum cull. Transform the quad's corners
            // to clip space; cull only when ALL corners fall outside the
            // SAME plane — never culls a partially visible visual. The
            // selected/hovered visuals are exempt as belt-and-braces: a
            // math regression must degrade to "everything drawn", not hide
            // the focused window.
            {
                let world = scene.world_matrix(visual.id);
                let mvp = pv * world;
                let mut left = 0usize;
                let mut right = 0usize;
                let mut bottom = 0usize;
                let mut top = 0usize;
                let mut far = 0usize;
                let mut behind = 0usize;
                for (cx, cy) in [
                    (-gw / 2.0, -gh / 2.0),
                    (gw / 2.0, -gh / 2.0),
                    (gw / 2.0, gh / 2.0),
                    (-gw / 2.0, gh / 2.0),
                ] {
                    let p = mvp * cgmath::Vector4::new(cx, cy, 0.0, 1.0);
                    if p.w <= 0.0 {
                        behind += 1;
                        continue;
                    }
                    let inv_w = 1.0 / p.w;
                    let ndc_x = p.x * inv_w;
                    let ndc_y = p.y * inv_w;
                    // 5% slack so edge-hugging quads never flicker.
                    if ndc_x < -1.05 {
                        left += 1;
                    } else if ndc_x > 1.05 {
                        right += 1;
                    }
                    if ndc_y < -1.05 {
                        bottom += 1;
                    } else if ndc_y > 1.05 {
                        top += 1;
                    }
                    if p.z * inv_w > 1.05 {
                        far += 1;
                    }
                }
                let culled = left == 4
                    || right == 4
                    || bottom == 4
                    || top == 4
                    || far == 4
                    || behind == 4;
                if culled
                    && scene.selected_id != Some(visual.id)
                    && scene.hovered_id != Some(visual.id)
                {
                    perf.record_stage(PipelineStage::RenderDraw, 0);
                    continue;
                }
            }
            let title_h =
                visual.decoration.title_bar_height / (1.0 + visual.decoration.title_bar_height);
            let world = scene.world_matrix(visual.id);
            let wx = world[3][0];
            let wy = world[3][1];
            let wz = world[3][2];
            let m3 = cgmath::Matrix3::new(
                world[0][0],
                world[0][1],
                world[0][2],
                world[1][0],
                world[1][1],
                world[1][2],
                world[2][0],
                world[2][1],
                world[2][2],
            );
            let rot = cgmath::Quaternion::from(m3);
            let model = Matrix4::from_translation(cgmath::Vector3::new(wx, wy, wz))
                * Matrix4::from(rot)
                * Matrix4::from_nonuniform_scale(gw, gh, 1.0);
            let mvp = plan.proj * plan.view * model;
            // G-G3: borrow the title instead of cloning the whole chrome
            // state per visual per frame (the closure only reads it).
            let chrome_title: &str = &visual.chrome.title;
            let focused = visual.focused;
            let _ = renderer.with_context(|gl| unsafe {
                rebind_surface(gl);
                draw_textured_quad(
                    gl,
                    draw,
                    &mvp,
                    tex_id,
                    visual.selected,
                    visual.focused,
                    title_h,
                    gw,
                    gh,
                    visual.src_uv,
                    visual.parent.is_none(),
                );

                // J3 chrome: title text + window buttons ride the SAME model
                // matrix as the client surface (one spatial object). Scope:
                // toplevel visuals only — parented visuals (subsurfaces, IME
                // popups) are raw client content and must not grow veyra
                // chrome (a chrome strip carved from a 5px CSD border reads
                // as ghost buttons floating on the desktop).
                if visual.parent.is_none() {
                    let strip_px = title_h * gh;
                    let char_h = strip_px * 0.62;
                    let layout = crate::chrome::ButtonLayout::for_window(gw, gh, title_h);
                    let [_, _, min_zone] = layout.zones();
                    // Title text: left-aligned in the strip, fitting between the
                    // left margin and the button region.
                    let left_margin = strip_px * 0.35;
                    let avail = min_zone.u_lo * gw - left_margin - strip_px * 0.25;
                    let title = crate::chrome::fit_title(chrome_title, avail.max(0.0), char_h);
                    if !title.is_empty() {
                        let (tr, tg, tb) = if focused {
                            (0.95, 0.95, 0.95)
                        } else {
                            (0.55, 0.58, 0.60)
                        };
                        draw_text_in_window(
                            gl,
                            draw,
                            atlas,
                            &title,
                            (&model, &pv),
                            (gw, gh),
                            (-gw * 0.5 + left_margin, gh * 0.5 - strip_px * 0.5, char_h),
                            (tr, tg, tb),
                        );
                    }
                    // Buttons: right-aligned glyphs, slightly brighter on focus.
                    let (br, bg, bb) = if focused {
                        (0.92, 0.92, 0.92)
                    } else {
                        (0.52, 0.55, 0.57)
                    };
                    for (button, u_center) in layout.centers() {
                        let glyph = char::from_u32(button.glyph_code()).unwrap_or(' ');
                        let cw = char_h * 0.9 * 5.0 / 7.0;
                        let cx_px = (u_center - 0.5) * gw;
                        // G-G3: stack-encoded glyph — no String per button
                        // per window per frame.
                        let mut glyph_buf = [0u8; 4];
                        let glyph_str = glyph.encode_utf8(&mut glyph_buf);
                        draw_text_in_window(
                            gl,
                            draw,
                            atlas,
                            glyph_str,
                            (&model, &pv),
                            (gw, gh),
                            (cx_px - cw * 0.5, gh * 0.5 - strip_px * 0.5, char_h * 0.9),
                            (br, bg, bb),
                        );
                    }
                }
            });
        }
        perf.record_stage(
            PipelineStage::RenderDraw,
            t_draw.elapsed().as_nanos() as u64,
        );

        any_partial |= partial;
        reports.push(crate::outputs::OutputFrameReport {
            output_id: plan.output_id,
            viewport: plan.viewport,
            presented: true,
        });
    }

    // Render the desktop shell taskbar (J4): 2D screen-space plane at
    // the bottom of the framebuffer, camera-independent. Same overlay
    // discipline as the context menu: depth off, px-space rects.
    // Screen-space overlays draw OUTSIDE the damage scissor (G-G6):
    // they are opaque and repaint fully every frame.
    if let Some(tb) = overlays.taskbar {
        let _ = renderer.with_context(|gl| unsafe {
            rebind_surface(gl);
            gl.Disable(ffi::SCISSOR_TEST);
            gl.Disable(ffi::DEPTH_TEST);
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::SRC_ALPHA, ffi::ONE_MINUS_SRC_ALPHA);

            let stride = 4 * std::mem::size_of::<f32>() as i32;
            let solid_rect =
                |px: f32, py: f32, pw: f32, ph: f32, r: f32, g: f32, b: f32, a: f32| {
                    let cx = ((px + pw / 2.0) / w) * 2.0 - 1.0;
                    let cy = -(((py + ph / 2.0) / h) * 2.0 - 1.0);
                    let mvp = cgmath::Matrix4::from_translation(cgmath::Vector3::new(cx, cy, 0.0))
                        * cgmath::Matrix4::from_nonuniform_scale(pw / w * 2.0, ph / h * 2.0, 1.0);
                    gl.UseProgram(draw.solid_prog);
                    gl.UniformMatrix4fv(draw.solid_u_mvp, 1, 0, mvp.as_ptr());
                    gl.Uniform4f(draw.solid_u_color, r, g, b, a);
                    gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
                    gl.EnableVertexAttribArray(draw.solid_a_pos);
                    gl.VertexAttribPointer(
                        draw.solid_a_pos,
                        2,
                        ffi::FLOAT,
                        0,
                        stride,
                        std::ptr::null(),
                    );
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

            let bar_y = h - tb.bar_h;
            tracing::debug!(
                w,
                h,
                bar_h = tb.bar_h,
                items = tb.items.len(),
                bar_y,
                "taskbar draw"
            );
            // Rounded-rect overlay with a vertical gradient + 1px SDF AA.
            let round_rect = |px: f32,
                              py: f32,
                              pw: f32,
                              ph: f32,
                              radius: f32,
                              top: (f32, f32, f32, f32),
                              bottom: (f32, f32, f32, f32)| {
                let cx = ((px + pw / 2.0) / w) * 2.0 - 1.0;
                let cy = -(((py + ph / 2.0) / h) * 2.0 - 1.0);
                let mvp = cgmath::Matrix4::from_translation(cgmath::Vector3::new(cx, cy, 0.0))
                    * cgmath::Matrix4::from_nonuniform_scale(pw / w * 2.0, ph / h * 2.0, 1.0);
                let radius = radius.min(pw * 0.5).min(ph * 0.5);
                gl.UseProgram(draw.round_prog);
                gl.UniformMatrix4fv(draw.round_u_mvp, 1, 0, mvp.as_ptr());
                gl.Uniform4f(draw.round_u_color, top.0, top.1, top.2, top.3);
                gl.Uniform4f(draw.round_u_color2, bottom.0, bottom.1, bottom.2, bottom.3);
                gl.Uniform2f(draw.round_u_size, pw, ph);
                gl.Uniform1f(draw.round_u_radius, radius);
                gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
                gl.EnableVertexAttribArray(draw.solid_a_pos);
                gl.VertexAttribPointer(
                    draw.solid_a_pos,
                    2,
                    ffi::FLOAT,
                    0,
                    stride,
                    std::ptr::null(),
                );
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

            // ── Bar background: subtle top→bottom darkening gradient ──
            round_rect(
                0.0,
                bar_y,
                w,
                tb.bar_h,
                0.0,
                (0.135, 0.145, 0.165, 0.98),
                (0.075, 0.082, 0.095, 0.98),
            );
            // Top hairline: bright edge for separation from the scene.
            solid_rect(0.0, bar_y, w, 1.0, 0.42, 0.45, 0.50, 0.22);

            // DPI-scaled glyph metrics (same family as before).
            let scale = (((tb.bar_h - 6.0) * 0.58) / 7.0).round().clamp(2.0, 6.0);
            let ch = (7.0f32 * scale / h) * 2.0;
            let cw = (5.0f32 * scale / w) * 2.0;

            let draw_label =
                |it: &crate::shell::TaskbarItem, iy: f32, ih: f32, color: (f32, f32, f32)| {
                    let text_x = ((it.x + 8.0) / w) * 2.0 - 1.0;
                    let center_ndc = -(((iy + ih / 2.0) / h) * 2.0 - 1.0);
                    let text_y = center_ndc - ch / 2.0;
                    draw_text(
                        gl, draw, atlas, &it.label, text_x, text_y, cw, ch, color.0, color.1,
                        color.2,
                    );
                };

            for it in &tb.items {
                let iy = bar_y + 4.0;
                let ih = tb.bar_h - 8.0;
                // Button fill: accent for active, lift for hover, quiet
                // neutral otherwise, darker for minimized.
                let (top, bottom) = if it.active {
                    ((0.165, 0.28, 0.175, 0.97), (0.11, 0.20, 0.125, 0.97))
                } else if it.hover {
                    ((0.26, 0.28, 0.32, 0.95), (0.19, 0.205, 0.235, 0.95))
                } else if it.dim {
                    ((0.13, 0.14, 0.155, 0.8), (0.10, 0.105, 0.12, 0.8))
                } else {
                    ((0.205, 0.22, 0.25, 0.9), (0.15, 0.16, 0.185, 0.9))
                };
                round_rect(it.x, iy, it.w, ih, 5.0, top, bottom);

                // Active accent underline (focused window / active ws).
                if it.active {
                    round_rect(
                        it.x + 7.0,
                        iy + ih - 3.5,
                        (it.w - 14.0).max(8.0),
                        2.0,
                        1.0,
                        (0.36, 0.78, 0.44, 0.95),
                        (0.30, 0.66, 0.38, 0.95),
                    );
                }

                let text_color = if it.active {
                    (0.93, 0.97, 0.93)
                } else if it.dim {
                    (0.50, 0.52, 0.56)
                } else if it.hover {
                    (0.92, 0.93, 0.96)
                } else {
                    (0.76, 0.78, 0.82)
                };
                draw_label(it, iy, ih, text_color);
            }

            // Section separators: quiet vertical hairlines.
            let sep_h = tb.bar_h * 0.55;
            let sep_y = bar_y + (tb.bar_h - sep_h) * 0.5;
            for sep_x in [tb.sep_ws, tb.sep_launch].into_iter().flatten() {
                solid_rect(sep_x, sep_y, 1.0, sep_h, 1.0, 1.0, 1.0, 0.07);
            }

            let gl_err = gl.GetError();
            if gl_err != 0 {
                tracing::warn!(code = format!("{gl_err:x}"), "GL error after taskbar draw");
            }
            gl.Enable(ffi::DEPTH_TEST);
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        });
    }

    // Render context menu overlay (if visible)
    if let Some(menu) = overlays.context_menu {
        if menu.visible {
            let _ = renderer.with_context(|gl| unsafe {
                rebind_surface(gl);
                gl.Disable(ffi::DEPTH_TEST);
                gl.Enable(ffi::BLEND);
                gl.BlendFunc(ffi::SRC_ALPHA, ffi::ONE_MINUS_SRC_ALPHA);

                let (mx, my) = menu.position;
                // DPI-proportional metrics shared with the click hit-test
                let metrics = crate::context_menu::MenuMetrics::for_framebuffer(w, h);
                let menu_width = metrics.menu_width;
                let item_height = metrics.item_height;
                let menu_height = menu.items.len() as f32 * item_height;

                // Convert screen pixel coords to NDC [-1, 1]
                let ndc_w = menu_width / w * 2.0;

                // (The font atlas was initialized with the per-context
                // caches at the top of render_scene.)

                // Draw px-space rects through the solid overlay program.
                // (px, py) is the top-left corner in screen pixels.
                let stride = 4 * std::mem::size_of::<f32>() as i32;
                let solid_rect =
                    |px: f32, py: f32, pw: f32, ph: f32, r: f32, g: f32, b: f32, a: f32| {
                        let cx = ((px + pw / 2.0) / w) * 2.0 - 1.0;
                        let cy = -(((py + ph / 2.0) / h) * 2.0 - 1.0);
                        let mvp =
                            cgmath::Matrix4::from_translation(cgmath::Vector3::new(cx, cy, 0.0))
                                * cgmath::Matrix4::from_nonuniform_scale(
                                    pw / w * 2.0,
                                    ph / h * 2.0,
                                    1.0,
                                );
                        gl.UseProgram(draw.solid_prog);
                        gl.UniformMatrix4fv(draw.solid_u_mvp, 1, 0, mvp.as_ptr());
                        gl.Uniform4f(draw.solid_u_color, r, g, b, a);
                        gl.BindBuffer(ffi::ARRAY_BUFFER, draw.vbo);
                        gl.EnableVertexAttribArray(draw.solid_a_pos);
                        gl.VertexAttribPointer(
                            draw.solid_a_pos,
                            2,
                            ffi::FLOAT,
                            0,
                            stride,
                            std::ptr::null(),
                        );
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
                solid_rect(
                    mx32 - 1.0,
                    my32 - 1.0,
                    menu_width + 2.0,
                    menu_height + 2.0,
                    0.30,
                    0.30,
                    0.32,
                    0.98,
                );
                solid_rect(mx32, my32, menu_width, menu_height, 0.13, 0.13, 0.15, 0.97);

                let ndc_ih = item_height / h * 2.0;
                // Draw each menu item
                for (i, _item) in menu.items.iter().enumerate() {
                    let item_iy = -((my as f32 + (i as f32 * item_height)) / h) * 2.0 + 1.0;
                    let item_ix = (mx as f32 / w) * 2.0 - 1.0 + ndc_w / 2.0;
                    let item_iy_c = item_iy - ndc_ih / 2.0;

                    let is_selected = menu.selected == Some(i);
                    if is_selected {
                        // Subtle gold row highlight (matches selection accent)
                        solid_rect(
                            mx32,
                            my32 + i as f32 * item_height,
                            menu_width,
                            item_height,
                            0.42,
                            0.33,
                            0.10,
                            0.95,
                        );
                    }

                    // Render item label text
                    // White for normal items, gold for the selected one.
                    let (tr, tg, tb) = if is_selected {
                        (1.0, 0.84, 0.0)
                    } else {
                        (1.0, 1.0, 1.0)
                    };
                    // The 5x7 bitmap glyphs are drawn at an integer scale
                    // factor proportional to the row height — 1:1 pixels on
                    // a modern panel are unreadably small (crisp with
                    // NEAREST sampling at any scale).
                    let scale = metrics.glyph_scale;
                    let text_x = item_ix - ndc_w / 2.0 + (4.0 / w) * 2.0; // 4px left padding
                    let ch = (7.0f32 * scale / h) * 2.0; // 7*scale px char height in NDC
                    let cw = (5.0f32 * scale / w) * 2.0; // 5*scale px char width in NDC
                    let text_y = item_iy_c - ch / 2.0; // draw_text y = glyph bottom → vertically centered
                    draw_text(
                        gl,
                        draw,
                        atlas,
                        &_item.label,
                        text_x,
                        text_y,
                        cw,
                        ch,
                        tr,
                        tg,
                        tb,
                    );
                }

                // Restore GL state for subsequent main-render passes
                gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
                gl.Enable(ffi::DEPTH_TEST);
            });
        }
    }

    // G-G6: never leak scissor state into the next frame — a full
    // clear under a stale scissor would leave the screen stale. Gated
    // on `any_partial` so the dormant path issues no extra GL calls.
    if any_partial {
        let _ = renderer.with_context(|gl| unsafe {
            gl.Disable(ffi::SCISSOR_TEST);
        });
    }

    Ok(())
}

#[cfg(test)]
mod gg6_tests {
    use super::*;

    #[test]
    fn union_rect_unions_all() {
        assert_eq!(union_rect(&[]), None);
        let u = union_rect(&[[10.0, 10.0, 50.0, 40.0], [30.0, 0.0, 90.0, 20.0]]).unwrap();
        assert_eq!(u, [10.0, 0.0, 90.0, 40.0]);
    }

    fn prev_view(view: cgmath::Matrix4<f32>, fb: (u32, u32)) -> (cgmath::Matrix4<f32>, u32, u32) {
        (view, fb.0, fb.1)
    }

    fn ident() -> cgmath::Matrix4<f32> {
        cgmath::Matrix4::from_scale(1.0)
    }

    #[test]
    fn partial_requires_preservation() {
        let v = ident();
        assert!(!decide_partial(false, Some(&prev_view(v, (100, 100))), &v, (100, 100), Some([0.0, 0.0, 10.0, 10.0])));
        assert!(decide_partial(true, Some(&prev_view(v, (100, 100))), &v, (100, 100), Some([0.0, 0.0, 10.0, 10.0])));
    }

    #[test]
    fn partial_requires_matching_view_and_fb() {
        let v = ident();
        let v2 = cgmath::Matrix4::from_scale(2.0);
        // Camera moved.
        assert!(!decide_partial(true, Some(&prev_view(v, (100, 100))), &v2, (100, 100), Some([0.0, 0.0, 10.0, 10.0])));
        // First frame (no prev).
        assert!(!decide_partial(true, None, &v, (100, 100), Some([0.0, 0.0, 10.0, 10.0])));
        // Framebuffer resized.
        assert!(!decide_partial(true, Some(&prev_view(v, (200, 100))), &v, (100, 100), Some([0.0, 0.0, 10.0, 10.0])));
        // Degenerate fb.
        assert!(!decide_partial(true, Some(&prev_view(v, (0, 0))), &v, (0, 0), Some([0.0, 0.0, 10.0, 10.0])));
        // No damage → full (nothing to do anyway).
        assert!(!decide_partial(true, Some(&prev_view(v, (100, 100))), &v, (100, 100), None));
    }

    #[test]
    fn partial_rejects_excessive_coverage() {
        let v = ident();
        // 90% of a 100x100 frame.
        assert!(!decide_partial(true, Some(&prev_view(v, (100, 100))), &v, (100, 100), Some([0.0, 0.0, 90.0, 100.0])));
        // 25%.
        assert!(decide_partial(true, Some(&prev_view(v, (100, 100))), &v, (100, 100), Some([0.0, 0.0, 50.0, 50.0])));
    }

    #[test]
    fn quad_aabb_maps_through_ortho() {
        // Ortho projection mapping world (±100, ±50) to a 1280x720
        // framebuffer centered at the origin: expect a 200x100 px box
        // at the center.
        let proj = crate::compositor::LookingGlass::projection_for(false, 1280.0, 720.0);
        let mvp = proj * cgmath::Matrix4::from_translation(cgmath::Vector3::new(0.0, 0.0, -1.0));
        let aabb = quad_screen_aabb(&mvp, 200.0, 100.0, 0, 0, 1280.0, 720.0).unwrap();
        assert!(((aabb[2] - aabb[0]) - 200.0).abs() < 1.0, "{}", aabb[2] - aabb[0]);
        assert!(((aabb[3] - aabb[1]) - 100.0).abs() < 1.0, "{}", aabb[3] - aabb[1]);
        assert!((aabb[0] - 540.0).abs() < 1.5, "centered: {}", aabb[0]);
        assert!((aabb[1] - 310.0).abs() < 1.5, "centered: {}", aabb[1]);
    }

    #[test]
    fn quad_aabb_behind_camera_is_none() {
        // Perspective camera; the quad sits fully BEHIND the camera
        // plane (w <= 0 for every corner) → no screen footprint.
        let proj = crate::compositor::LookingGlass::projection_for(true, 1280.0, 720.0);
        let mvp = proj * cgmath::Matrix4::from_translation(cgmath::Vector3::new(0.0, 0.0, 50.0));
        assert_eq!(quad_screen_aabb(&mvp, 200.0, 100.0, 0, 0, 1280.0, 720.0), None);
    }
}

#[cfg(test)]
mod tests {
    /// R1 regression guard: `render_scene` is DRAW-ONLY — the frame
    /// lifecycle (begin_frame/finish_frame) belongs to
    /// LookingGlass::render, which calls each exactly once per frame.
    /// A second begin/submit here would re-make the surface, queue an
    /// unrendered DRM buffer, and double-swap on winit.
    #[test]
    fn render_scene_is_draw_only_no_frame_lifecycle() {
        let src = include_str!("renderer.rs");
        let start = src
            .find("pub fn render_scene")
            .expect("render_scene must exist");
        let end = src[start..]
            .find("#[cfg(test)]")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        for banned in ["begin_frame", "finish_frame"] {
            assert!(
                !body.contains(banned),
                "render_scene must not call {banned} — the frame lifecycle is owned by LookingGlass::render (R1)"
            );
        }
    }

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

    /// P2 #9 regression guard: the render caches are context-owned —
    /// LookingGlass::render must reset them on every context-loss path
    /// (begin, draw, submit) so a fresh context can never observe stale
    /// object IDs from a previous one.
    #[test]
    fn context_loss_resets_render_caches() {
        let src = include_str!("compositor.rs");
        // render(): the context-loss paths must clear the caches.
        let count = src
            .matches("self.render_caches = Default::default();")
            .count();
        assert!(
            count >= 3,
            "expected resets on all three context-loss paths (begin/draw/submit), found {count}"
        );
        // The statics must be gone — ownership lives in RenderCaches.
        assert!(
            !src.contains("static DRAW_GL") && !src.contains("static FONT_ATLAS"),
            "global GL caches must not return"
        );
    }
}
