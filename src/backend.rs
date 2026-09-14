use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::SwapBuffersError;
use smithay::utils::Physical;
use smithay::utils::Size;

/// G-F3: the frame's drawing target — OPAQUE to the renderer.
///
/// The renderer never sees EGL surfaces, GBM surfaces, DRM fds, or
/// CRTC handles; it asks the target to make itself current for GL work
/// and the presentation backend owns every detail of how that happens.
/// This is a compile-time boundary, not a convention: `renderer.rs`
/// cannot name a presentation type.
pub enum FrameTarget {
    /// Nested winit: an EGL window surface. The raw context/surface
    /// pair is the irreducible make-current input (smithay binds
    /// NO_SURFACE on every with_context cycle) — contained HERE,
    /// invisible to renderer.rs.
    EglWindow {
        ctx: *const smithay::backend::egl::EGLContext,
        surface: *const smithay::backend::egl::EGLSurface,
    },
    /// DRM path: `begin_output` already bound the output's FBO; the
    /// surface dance is a no-op (FBO bindings survive NO_SURFACE
    /// cycles — the binding survives all with_context closures).
    BoundFbo,
}

impl FrameTarget {
    /// Presentation-backend invariant: make this target current AND
    /// re-assert the ACTIVE VIEWPORT. A fresh context→surface bind
    /// resets viewport/scissor to the SURFACE size (the E5.5 live
    /// finding) — every rebind must re-assert the renderer's active
    /// viewport or multi-output quads render at full-framebuffer scale.
    /// Returns Err(ContextLost) on bind failure (the recovery path
    /// takes over).
    pub fn make_current(
        &self,
        gl: &smithay::backend::renderer::gles::ffi::Gles2,
        viewport: [i32; 4],
    ) -> Result<(), SwapBuffersError> {
        match self {
            FrameTarget::EglWindow { ctx, surface } => unsafe {
                let ctx = &**ctx;
                let surface = &**surface;
                ctx.make_current_with_surface(surface).map_err(|_| {
                    SwapBuffersError::ContextLost(
                        "target rebind failed (context lost)".into(),
                    )
                })?;
                gl.BindFramebuffer(
                    smithay::backend::renderer::gles::ffi::FRAMEBUFFER,
                    0,
                );
            },
            FrameTarget::BoundFbo => {
                // The FBO is already current; nothing to rebind.
            }
        }
        // Invariant: a fresh bind resets viewport/scissor — re-assert.
        unsafe {
            gl.Viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
        Ok(())
    }

    /// G-G6: query-only swap-behavior preservation probe. READ-ONLY:
    /// EGL_SWAP_BEHAVIOR is a property of the surface's EGLConfig —
    /// setting it post-hoc is spec-invalid (and breaks llvmpipe's first
    /// swap with BadAlloc). Drivers that natively preserve get partial
    /// presents; everyone else falls back to full-frame (always
    /// correct). Honors the backend's probe gate.
    pub fn natively_preserves_buffers(&self, probe_allowed: bool) -> bool {
        if !probe_allowed {
            return false;
        }
        match self {
            FrameTarget::EglWindow { ctx, surface } => unsafe {
                use smithay::backend::egl::ffi::egl as eglffi;
                let ctx = &**ctx;
                let display = ctx.display().get_display_handle().handle;
                let surf = (&**surface).get_surface_handle();
                let mut v = 0i32;
                eglffi::QuerySurface(display, surf, eglffi::SWAP_BEHAVIOR as i32, &mut v)
                    == eglffi::TRUE
                    && v == eglffi::BUFFER_PRESERVED as i32
            },
            FrameTarget::BoundFbo => false,
        }
    }
}

/// A presentation backend that owns a GlesRenderer.
/// WinitGraphicsBackend and DrmGraphicsBackend both implement this.
pub trait PresentationBackend {
    fn renderer(&mut self) -> &mut GlesRenderer;
    /// Begin a new frame: make the rendering surface current.
    /// Must be called before rendering.
    fn begin_frame(&mut self) -> Result<(), SwapBuffersError>;
    /// Finish the current frame and present it to the display.
    fn finish_frame(&mut self) -> Result<(), SwapBuffersError>;
    /// Output size in logical pixels.
    fn size(&self) -> (f32, f32);
    /// G-F3: the frame's drawing target, opaque to the renderer. The
    /// renderer asks it to make itself current; it never learns what
    /// the target physically is.
    fn frame_target(&mut self) -> FrameTarget;
    /// Downcast hook for backend-specific plumbing (BUG_LIST #4: the
    /// DRM flip-event source reaches DrmGraphicsBackend through this).
    fn as_any(&mut self) -> &mut dyn std::any::Any;
    /// G-G6: whether the renderer may query the EGL swap behavior on
    /// this backend's surface. Default: NO. Rationale: on the nested
    /// winit/llvmpipe stack even a read-only eglQuerySurface between
    /// make_current cycles corrupts the next eglSwapBuffers (BadAlloc,
    /// observed deterministically); nested is a development backend and
    /// presents full-frame. The native DRM path (qualified drivers,
    /// buffer-age aware) opts in.
    fn preservation_probe_allowed(&self) -> bool {
        false
    }
}

/// Wrapper implementing PresentationBackend for Smithay's WinitGraphicsBackend.
pub struct WinitPresentationBackend(
    pub smithay::backend::winit::WinitGraphicsBackend<GlesRenderer>,
);

impl PresentationBackend for WinitPresentationBackend {
    fn renderer(&mut self) -> &mut GlesRenderer {
        self.0.renderer()
    }

    fn begin_frame(&mut self) -> Result<(), SwapBuffersError> {
        // Make the EGL surface current so subsequent GL operations have a
        // valid draw target. We stash a raw pointer to the surface to avoid
        // borrow conflicts between renderer() (mutable) and egl_surface()
        // (immutable) on self.0.
        //
        // This replaces self.0.bind() which does NOT call make_current_with_surface.
        // Without this, with_context() calls later use EGL_NO_SURFACE, causing
        // GL_INVALID_FRAMEBUFFER_OPERATION.
        let window_size: Size<i32, Physical> = self.0.window_size();
        self.0
            .egl_surface()
            .resize(window_size.w, window_size.h, 0, 0);
        let surface_ptr: *const smithay::backend::egl::EGLSurface =
            self.0.egl_surface() as *const _;
        let ctx_ptr: *const smithay::backend::egl::EGLContext =
            self.0.renderer().egl_context() as *const _;
        unsafe {
            (*ctx_ptr)
                .make_current_with_surface(&*surface_ptr)
                .map_err(|_| SwapBuffersError::ContextLost("make_current_with_surface".into()))?;
        }
        Ok(())
    }

    fn finish_frame(&mut self) -> Result<(), SwapBuffersError> {
        self.0.submit(None)
    }

    fn size(&self) -> (f32, f32) {
        let s = self.0.window_size();
        (s.w as f32, s.h as f32)
    }

    fn frame_target(&mut self) -> FrameTarget {
        // The nested window's EGL pair, stashed as opaque pointers. The
        // renderer asks FrameTarget::make_current to use them; it never
        // names the types.
        let surface_ptr: *const smithay::backend::egl::EGLSurface =
            self.0.egl_surface() as *const _;
        let ctx_ptr: *const smithay::backend::egl::EGLContext =
            self.0.renderer().egl_context() as *const _;
        FrameTarget::EglWindow {
            ctx: ctx_ptr,
            surface: surface_ptr,
        }
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
