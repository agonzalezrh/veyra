use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::SwapBuffersError;

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
}

/// Wrapper implementing PresentationBackend for Smithay's WinitGraphicsBackend.
pub struct WinitPresentationBackend(pub smithay::backend::winit::WinitGraphicsBackend<GlesRenderer>);

impl PresentationBackend for WinitPresentationBackend {
    fn renderer(&mut self) -> &mut GlesRenderer {
        self.0.renderer()
    }

    fn begin_frame(&mut self) -> Result<(), SwapBuffersError> {
        let (_renderer, _target) = self.0.bind()?;
        // The EGL surface is now current for rendering.
        // We drop the target immediately — the surface stays current
        // because EGL doesn't unbind on target drop.
        Ok(())
    }

    fn finish_frame(&mut self) -> Result<(), SwapBuffersError> {
        self.0.submit(None)
    }

    fn size(&self) -> (f32, f32) {
        let s = self.0.window_size();
        (s.w as f32, s.h as f32)
    }
}
