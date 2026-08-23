use std::sync::Mutex;

use cgmath::Matrix;
use cgmath::Matrix3;
use cgmath::Matrix4;
use smithay::backend::renderer::gles::ffi;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::Texture;
use smithay::backend::SwapBuffersError;
use tracing::error;

use crate::perf::PerfStats;
use crate::scene::Scene;

/// Global DrawGl cache, created once per GL context lifetime.
/// Reset on context loss.
static DRAW_GL: Mutex<Option<DrawGl>> = Mutex::new(None);

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
void main() {
    vec2 uv = v_uv;
    float b = 0.05;
    float title_border = 0.02;
    bvec4 edge = bvec4(
        uv.x < b || uv.x > 1.0 - b,
        uv.y < b || uv.y > 1.0 - b,
        false,
        false
    );
    if (uv.y < u_title_h) {
        // Title bar area
        bool title_edge = uv.x < title_border || uv.x > 1.0 - title_border ||
                          uv.y < title_border || uv.y > u_title_h - title_border;
        if (title_edge) {
            if (u_selected > 0.5) {
                gl_FragColor = vec4(0.8, 0.64, 0.0, 1.0);
            } else if (u_focused > 0.5) {
                gl_FragColor = vec4(0.2, 0.7, 0.2, 1.0);
            } else {
                gl_FragColor = vec4(0.0, 0.7, 0.7, 1.0);
            }
        } else {
            if (u_selected > 0.5) {
                gl_FragColor = vec4(0.45, 0.35, 0.1, 0.9);
            } else if (u_focused > 0.5) {
                gl_FragColor = vec4(0.15, 0.35, 0.15, 0.9);
            } else {
                gl_FragColor = vec4(0.1, 0.25, 0.25, 0.9);
            }
        }
    } else {
        // Content area
        vec2 content_uv = vec2(uv.x, (uv.y - u_title_h) / (1.0 - u_title_h));
        if (any(edge)) {
            if (u_selected > 0.5) {
                gl_FragColor = vec4(1.0, 0.84, 0.0, 1.0);
            } else if (u_focused > 0.5) {
                gl_FragColor = vec4(0.3, 0.9, 0.3, 1.0);
            } else {
                gl_FragColor = vec4(0.0, 1.0, 1.0, 1.0);
            }
        } else {
            gl_FragColor = texture2D(u_tex, content_uv);
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
    vbo: u32,
}

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
        DrawGl { program, a_pos, a_uv, u_mvp, u_tex, u_selected, u_focused, u_title_h, vbo }
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
/// Reset the cache if the GL context was lost (returns None on error).
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
) {
    unsafe {
        gl.UseProgram(draw.program);
        gl.UniformMatrix4fv(draw.u_mvp, 1, 0, mvp.as_ptr());
        gl.Uniform1f(draw.u_selected, if selected { 1.0 } else { 0.0 });
        gl.Uniform1f(draw.u_focused, if focused { 1.0 } else { 0.0 });
        gl.Uniform1f(draw.u_title_h, title_h);
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

fn do_render(
    backend: &mut smithay::backend::winit::WinitGraphicsBackend<GlesRenderer>,
    scene: &Scene,
    view: &Matrix4<f32>,
    proj: &Matrix4<f32>,
    window_size: smithay::utils::Size<i32, smithay::utils::Physical>,
    _w: f32, _h: f32,
) -> Result<(), SwapBuffersError> {
    let (renderer, mut target) = backend.bind()?;
    let mut frame = match renderer.render(&mut target, window_size, smithay::utils::Transform::Normal) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };

    // Initialize DrawGl once per GL context lifetime
    let _ = frame.with_context(|gl| { get_draw_gl(gl); });
    let draw_guard = DRAW_GL.lock().unwrap();
    let draw = match draw_guard.as_ref() {
        Some(d) => d,
        None => {
            error!("DrawGl not initialized");
            return Ok(());
        }
    };

    let _ = frame.with_context(|gl| unsafe {
        gl.ClearColor(0.15, 0.15, 0.15, 1.0);
        gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
    });
    let _ = frame.with_context(|gl| unsafe {
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.Enable(ffi::DEPTH_TEST);
        gl.DepthFunc(ffi::LESS);
    });

    for visual in scene.iter() {
        if visual.window_state == crate::scene::WindowState::Minimized { continue; }
        let Some(texture) = visual.texture() else { continue };
        let tex_id = texture.tex_id();
        let gw = visual.total_width();
        let gh = visual.total_height();
        let title_h = visual.decoration.title_bar_height / (1.0 + visual.decoration.title_bar_height);

        // Use world-space position and rotation (scale stays local)
        let world = scene.world_matrix(visual.id);
        let wx = world[3][0]; let wy = world[3][1]; let wz = world[3][2];
        let m3 = cgmath::Matrix3::new(
            world[0][0], world[0][1], world[0][2],
            world[1][0], world[1][1], world[1][2],
            world[2][0], world[2][1], world[2][2],
        );
        let rot = cgmath::Quaternion::from(m3);
        let model = Matrix4::from_translation(cgmath::Vector3::new(wx, wy, wz))
            * Matrix4::from(rot)
            * Matrix4::from_nonuniform_scale(gw, gh, 1.0);
        let mvp = proj * view * model;
        let _ = frame.with_context(|gl| draw_textured_quad(gl, draw, &mvp, tex_id, visual.selected, visual.focused, title_h));
    }

    drop(frame);
    drop(target);
    backend.submit(None)
}

pub fn render_scene(
    backend: &mut smithay::backend::winit::WinitGraphicsBackend<GlesRenderer>,
    scene: &Scene,
    view: &Matrix4<f32>,
    proj: &Matrix4<f32>,
    perf: &mut PerfStats,
    visible_ids: Option<&[crate::scene::VisualId]>,
) -> Result<(), SwapBuffersError> {
    use crate::perf::PipelineStage;

    let window_size = backend.window_size();
    let w = window_size.w as f32;
    let h = window_size.h as f32;

    let t_bind = std::time::Instant::now();
    let (renderer, mut target) = match backend.bind() {
        Ok(pair) => pair,
        Err(e) => { error!(?e, "bind failed"); return Ok(()); }
    };
    let mut frame = match renderer.render(&mut target, window_size, smithay::utils::Transform::Normal) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };
    perf.record_stage(PipelineStage::RenderBind, t_bind.elapsed().as_nanos() as u64);

    let _ = frame.with_context(|gl| { get_draw_gl(gl); });
    let draw_guard = DRAW_GL.lock().unwrap();
    let draw = match draw_guard.as_ref() {
        Some(d) => d,
        None => { error!("DrawGl not initialized"); return Ok(()); }
    };

    let _ = frame.with_context(|gl| unsafe {
        gl.ClearColor(0.15, 0.15, 0.15, 1.0);
        gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
    });
    let _ = frame.with_context(|gl| unsafe {
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.Enable(ffi::DEPTH_TEST);
        gl.DepthFunc(ffi::LESS);
    });

    // Draw all visuals
    let t_draw = std::time::Instant::now();
    for visual in scene.iter() {
        if visual.window_state == crate::scene::WindowState::Minimized { continue; }
        if let Some(ids) = visible_ids {
            if !ids.contains(&visual.id) { continue; }
        }
        let Some(texture) = visual.texture() else { continue };
        let tex_id = texture.tex_id();
        let gw = visual.total_width();
        let gh = visual.total_height();
        let title_h = visual.decoration.title_bar_height / (1.0 + visual.decoration.title_bar_height);
        let world = scene.world_matrix(visual.id);
        let wx = world[3][0]; let wy = world[3][1]; let wz = world[3][2];
        let m3 = cgmath::Matrix3::new(
            world[0][0], world[0][1], world[0][2],
            world[1][0], world[1][1], world[1][2],
            world[2][0], world[2][1], world[2][2],
        );
        let rot = cgmath::Quaternion::from(m3);
        let model = Matrix4::from_translation(cgmath::Vector3::new(wx, wy, wz))
            * Matrix4::from(rot)
            * Matrix4::from_nonuniform_scale(gw, gh, 1.0);
        let mvp = proj * view * model;
        let _ = frame.with_context(|gl| draw_textured_quad(gl, draw, &mvp, tex_id, visual.selected, visual.focused, title_h));
    }
    perf.record_stage(PipelineStage::RenderDraw, t_draw.elapsed().as_nanos() as u64);

    drop(frame);
    drop(target);

    let t_submit = std::time::Instant::now();
    let r = backend.submit(None);
    perf.record_stage(PipelineStage::RenderSubmit, t_submit.elapsed().as_nanos() as u64);

    if let Err(SwapBuffersError::ContextLost(e)) = r {
        error!(?e, "Context lost");
    }
    Ok(())
}
