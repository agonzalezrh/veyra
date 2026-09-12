use smithay::backend::egl::EGLSurface;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::SwapBuffersError;
use smithay::utils::Physical;
use smithay::utils::Size;

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
    /// Access the underlying EGL surface, if available.
    fn egl_surface(&self) -> Option<&EGLSurface>;
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
        let surface_ptr: *const EGLSurface = self.0.egl_surface() as *const EGLSurface;
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

    fn egl_surface(&self) -> Option<&EGLSurface> {
        Some(self.0.egl_surface())
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
