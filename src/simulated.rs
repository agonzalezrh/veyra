//! Simulated frame producer and input sink for testing.
//!
//! Provides a simulated external frame that animates in place
//! as a placeholder for real frame producers (e.g. remote desktop,
//! screen capture, virtual machines).

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportMem;
use tracing::info;

use crate::input_router::{InputSink, KeyboardEvent, PointerEventKind};
use crate::producer::{FrameProducer, FrameResult};

/// A simulated external frame producer that generates animated checkerboard frames.
pub struct SimulatedFrameProducer {
    texture: GlesTexture,
    width: u32,
    height: u32,
    frame_count: u64,
}

impl SimulatedFrameProducer {
    pub fn new(renderer: &mut GlesRenderer) -> Option<Self> {
        let w = 256u32;
        let h = 256u32;
        let pixels = Self::generate(w, h, 0);
        let tex = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (w as i32, h as i32).into(), false)
            .ok()?;
        Some(SimulatedFrameProducer {
            texture: tex,
            width: w,
            height: h,
            frame_count: 0,
        })
    }

    fn generate(w: u32, h: u32, phase: u64) -> Vec<u8> {
        let shift = (phase % 12) as u8;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let bright = ((x / 32) + (y / 32) + (shift as u32)) % 2 == 0;
                if bright {
                    let r = 200u8.wrapping_add(shift.wrapping_mul(5));
                    let g = 60u8.wrapping_add(shift.wrapping_mul(3));
                    let b = 240u8.wrapping_sub(shift.wrapping_mul(8));
                    pixels.extend_from_slice(&[r, g, b, 255]);
                } else {
                    pixels.extend_from_slice(&[15, 8, 20, 255]);
                }
            }
        }
        pixels
    }
}

impl FrameProducer for SimulatedFrameProducer {
    fn update(&mut self, renderer: &mut GlesRenderer) -> FrameResult {
        self.frame_count += 1;
        if self.frame_count % 5 != 0 {
            return FrameResult::Unchanged;
        }
        let pixels = Self::generate(self.width, self.height, self.frame_count / 5);
        match renderer.import_memory(
            &pixels,
            Fourcc::Abgr8888,
            (self.width as i32, self.height as i32).into(),
            false,
        ) {
            Ok(tex) => {
                self.texture = tex;
                FrameResult::Updated
            }
            Err(e) => FrameResult::Error(format!("tex: {:?}", e)),
        }
    }

    fn texture(&self) -> &GlesTexture {
        &self.texture
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn create_input_sink(&mut self) -> Option<Box<dyn InputSink>> {
        #[derive(Debug)]
        struct LogInputSink;
        impl InputSink for LogInputSink {
            fn handle_pointer(&mut self, kind: PointerEventKind, u: f64, v: f64) {
                info!(?kind, u, v, "simulated input (pointer)");
            }
            fn handle_keyboard(&mut self, event: KeyboardEvent) {
                info!(key = event.key, pressed = event.pressed, "simulated input (keyboard)");
            }
        }
        Some(Box::new(LogInputSink))
    }

    fn capabilities(&self) -> crate::producer::ProviderCapabilities {
        crate::producer::ProviderCapabilities {
            pointer_input: true,
            keyboard_input: true,
            resize: true,
            close: false,
            reconnect: false,
        }
    }
}
