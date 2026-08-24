use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportMem;

use crate::input_router::InputSink;

/// Capabilities a content provider can advertise.
/// The compositor uses these to determine which operations are available
/// without knowing the concrete provider implementation.
#[derive(Debug, Clone, Default)]
pub struct ProviderCapabilities {
    pub pointer_input: bool,
    pub keyboard_input: bool,
    pub resize: bool,
    pub close: bool,
    pub reconnect: bool,
}

/// Result of a frame update.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameResult {
    /// A new frame was imported and the texture changed.
    Updated,
    /// No new frame available; use existing texture.
    Unchanged,
    /// The producer changed size; new texture has different dimensions.
    Resized(u32, u32),
    /// The producer encountered a non-fatal error.
    Error(String),
    /// The producer has shut down and should be removed.
    Finished,
}

/// A producer of GPU textures for the scene.
///
/// Each frame the compositor calls `update()` on every registered producer.
/// The producer imports new pixel data into the renderer and returns
/// a FrameResult indicating what changed. The compositor then updates
/// the corresponding Visual's texture if needed.
///
/// Producers manage their own GPU resource lifecycle — old textures are
/// dropped when new ones are imported, and the compositor never holds
/// stale references.
pub trait FrameProducer {
    /// Attempt to produce a new frame. Returns a FrameResult.
    fn update(&mut self, renderer: &mut GlesRenderer) -> FrameResult;
    /// Get the current texture (valid after the first successful update).
    fn texture(&self) -> &GlesTexture;
    /// Get the current dimensions.
    fn size(&self) -> (u32, u32);
    /// Optionally create an InputSink for this producer's content.
    /// Returns None if the producer doesn't support input routing.
    fn create_input_sink(&mut self) -> Option<Box<dyn InputSink>> { None }
    /// Report provider capabilities.
    fn capabilities(&self) -> ProviderCapabilities { ProviderCapabilities::default() }
}

/// An animated checkerboard that deliberately tests edge cases.
///
/// - Occasionally drops frames (returns Unchanged)
/// - Occasionally resizes
/// - Occasionally "fails" (returns Error)
/// - Eventually finishes (returns Finished)
///
/// This is a hostile test for the FrameProducer lifecycle.
pub struct HostileCheckerboard {
    texture: GlesTexture,
    width: u32,
    height: u32,
    frame_count: u64,
    max_frames: u64,
}

impl HostileCheckerboard {
    pub fn new(renderer: &mut GlesRenderer) -> Option<Self> {
        let w = 256u32;
        let h = 256u32;
        let pixels = Self::generate(w, h, 0);
        let tex = renderer.import_memory(&pixels, Fourcc::Abgr8888, (w as i32, h as i32).into(), false).ok()?;
        Some(HostileCheckerboard {
            texture: tex,
            width: w,
            height: h,
            frame_count: 0,
            max_frames: 300,
        })
    }

    fn generate(w: u32, h: u32, phase: u64) -> Vec<u8> {
        let shift = (phase % 24) as u8;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let bright = ((x / 32) + (y / 32) + (shift as u32)) % 2 == 0;
                if bright {
                    let r = 128u8.wrapping_add(shift.wrapping_mul(10));
                    let g = 200u8.wrapping_sub(shift.wrapping_mul(8));
                    let b = 255u8.wrapping_sub(shift.wrapping_mul(6));
                    pixels.extend_from_slice(&[r, g, b, 255]);
                } else {
                    pixels.extend_from_slice(&[20, 20, 40, 255]);
                }
            }
        }
        pixels
    }
}

/// A static colored quad that never updates. Ideal for scaling benchmarks.
pub struct StaticColor {
    texture: GlesTexture,
    width: u32,
    height: u32,
}

impl StaticColor {
    pub fn new(renderer: &mut GlesRenderer, r: u8, g: u8, b: u8) -> Option<Self> {
        let w = 128u32;
        let h = 128u32;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for _x in 0..w {
                pixels.extend_from_slice(&[b, g, r, 255]);
            }
        }
        let tex = renderer.import_memory(
            &pixels, smithay::backend::allocator::Fourcc::Abgr8888,
            (w as i32, h as i32).into(), false,
        ).ok()?;
        Some(StaticColor { texture: tex, width: w, height: h })
    }
}

impl FrameProducer for StaticColor {
    fn update(&mut self, _renderer: &mut GlesRenderer) -> FrameResult {
        FrameResult::Unchanged
    }
    fn texture(&self) -> &GlesTexture { &self.texture }
    fn size(&self) -> (u32, u32) { (self.width, self.height) }
}

impl FrameProducer for HostileCheckerboard {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            pointer_input: false,
            keyboard_input: false,
            resize: true,
            close: true,
            reconnect: false,
        }
    }

    fn update(&mut self, renderer: &mut GlesRenderer) -> FrameResult {
        self.frame_count += 1;

        // After max_frames, shut down
        if self.frame_count > self.max_frames {
            return FrameResult::Finished;
        }

        // Every 20th frame: simulate an error
        if self.frame_count % 20 == 0 {
            return FrameResult::Error("simulated glitch".into());
        }

        // Every 15th frame: drop (no new frame)
        if self.frame_count % 15 == 0 {
            return FrameResult::Unchanged;
        }

        // Every 30th frame: resize (alternate between two sizes)
        if self.frame_count % 30 == 0 {
            let new_w = if self.width == 256 { 192 } else { 256 };
            let new_h = if self.height == 256 { 192 } else { 256 };
            self.width = new_w;
            self.height = new_h;
            let pixels = Self::generate(new_w, new_h, self.frame_count / 3);
            if let Ok(tex) = renderer.import_memory(
                &pixels, Fourcc::Abgr8888, (new_w as i32, new_h as i32).into(), false,
            ) {
                self.texture = tex;
                return FrameResult::Resized(new_w, new_h);
            }
        }

        // Every 3 frames: update texture in-place using glTexSubImage2D
        // to avoid full texture creation cost (alloc + upload vs just upload).
        if self.frame_count % 3 != 0 {
            return FrameResult::Unchanged;
        }
        let pixels = Self::generate(self.width, self.height, self.frame_count / 3);
        let w = self.width as i32;
        let h = self.height as i32;
        let tex_id = self.texture.tex_id();
        crate::renderer::upload_texture_sub_region(renderer, tex_id, 0, 0, w, h, &pixels);
        FrameResult::Updated
    }

    fn texture(&self) -> &GlesTexture {
        &self.texture
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}
