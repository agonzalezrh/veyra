use std::sync::Mutex;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Format;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportDma;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use tracing::warn;

pub struct DmabufManager {
    pub state: DmabufState,
    pub global: DmabufGlobal,
    pub textures: Mutex<Vec<(Dmabuf, GlesTexture)>>,
}

impl DmabufManager {
    pub fn new(display: &DisplayHandle) -> Self {
        let mut state = DmabufState::new();
        let formats = vec![
            Format {
                code: smithay::backend::allocator::Fourcc::Argb8888,
                modifier: smithay::backend::allocator::Modifier::Linear,
            },
            Format {
                code: smithay::backend::allocator::Fourcc::Xrgb8888,
                modifier: smithay::backend::allocator::Modifier::Linear,
            },
        ];
        let global = state.create_global::<crate::compositor::LookingGlass>(display, formats);

        DmabufManager {
            state,
            global,
            textures: Mutex::new(Vec::new()),
        }
    }
}

impl DmabufHandler for crate::compositor::LookingGlass {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_manager.state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if let Some(backend) = self.backend.as_mut() {
            let renderer = backend.renderer();
            match renderer.import_dmabuf(&dmabuf, None) {
                Ok(texture) => {
                    if let Ok(mut cache) = self.dmabuf_manager.textures.lock() {
                        cache.push((dmabuf, texture));
                    }
                    if notifier.successful::<Self>().is_err() {
                        warn!("dmabuf import notification failed");
                    }
                }
                Err(e) => {
                    warn!(?e, "dmabuf import failed, notifying client");
                    notifier.failed();
                }
            }
        } else {
            warn!("no backend available for dmabuf import");
            notifier.failed();
        }
    }

    fn new_surface_feedback(
        &mut self,
        _surface: &WlSurface,
        _global: &DmabufGlobal,
    ) -> Option<smithay::wayland::dmabuf::DmabufFeedback> {
        None
    }
}
