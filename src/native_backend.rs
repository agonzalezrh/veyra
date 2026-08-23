//! Native DRM/KMS backend for Veyra.
//!
//! Replaces the winit nested backend when running directly on hardware.
//! Uses Smithay's DRM + GBM + EGL + libseat + libinput infrastructure.
//!
//! This module is only compiled when the `backend_drm` feature is available.

use std::sync::Arc;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::{DrmDevice, DrmEvent, DrmSurface};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::gbm::{GbmBufferedSurface, GbmDevice};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::auto::AutoSession;
use smithay::backend::session::Session;
use smithay::backend::udev::{UdevBackend, UdevEvent};
use smithay::reexports::calloop::{EventLoop, LoopHandle, RegistrationToken};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{DevPath, Size};
use tracing::{error, info, warn};

use crate::compositor::LookingGlass;

/// Errors during native backend initialization.
#[derive(Debug)]
pub enum NativeError {
    Session,
    Udev,
    Drm,
    Gbm,
    Egl,
    Renderer,
}

/// Initialize the native DRM/KMS backend and run the event loop.
/// This function does not return until the compositor shuts down.
pub fn run_native(
    mut event_loop: EventLoop<'static, LookingGlass>,
    display_handle: &DisplayHandle,
    mut state: LookingGlass,
) -> Result<(), NativeError> {
    let handle = event_loop.handle();
    let _log = handle;

    // 1. Initialize session (libseat/logind for DRM/VT access)
    let mut session = AutoSession::new(None).map_err(|_| NativeError::Session)?;
    let seat = session.seat();

    // 2. Start udev backend to discover DRM devices
    let udev_backend = UdevBackend::new(seat, None).map_err(|_| NativeError::Udev)?;

    // 3. Register udev event handler
    // For simplicity, we handle the first DRM device we find
    // In a full implementation, we'd handle hotplug events

    // Register udev source
    let udev_handle = handle.clone();
    handle
        .insert_source(udev_backend, move |event, _meta, state| {
            if let UdevEvent::Added { device_id, path } = event {
                info!(?device_id, ?path, "DRM device added");
                // Create DRM device from the udev event
                match DrmDevice::new_from_path(&path, false) {
                    Ok(drm) => {
                        info!("DRM device opened successfully");
                        if let Err(e) = setup_drm(drm, &udev_handle, state) {
                            error!(?e, "Failed to setup DRM device");
                        }
                    }
                    Err(e) => error!(?e, ?path, "Failed to open DRM device"),
                }
            }
            if let UdevEvent::Changed { device_id: _ } = event {
                // Re-scan connectors on change
            }
            if let UdevEvent::Removed { device_id: _ } = event {
                // Cleanup on removal
            }
        })
        .map_err(|_| NativeError::Udev)?;

    // 4. Run the event loop (processes DRM, input, wayland)
    let _ = event_loop.run(None, &mut state, |_| {});

    Ok(())
}

/// Set up a DRM device: find a connector, create a surface, set up EGL/GBM/GLES.
fn setup_drm(
    drm: DrmDevice<GlesRenderer>,
    handle: &LoopHandle<'static, LookingGlass>,
    state: &mut LookingGlass,
) -> Result<(), NativeError> {
    // 1. Create GBM device from the DRM fd
    let gbm = GbmDevice::new(&drm).map_err(|_| NativeError::Gbm)?;

    // 2. Create EGL context on the GBM device
    let egl_display =
        EGLDisplay::new_for_gbm_device(&gbm, false).map_err(|_| NativeError::Egl)?;
    let egl_context =
        EGLContext::new_with_config(&egl_display).map_err(|_| NativeError::Egl)?;

    // 3. Create GLES renderer from EGL context
    let renderer = unsafe {
        GlesRenderer::new(&egl_context).map_err(|_| NativeError::Renderer)?
    };

    // 4. Find a suitable connector/CRTC
    let mut pending = drm.pending_state();
    let connectors = drm
        .connectors()
        .iter()
        .filter(|c| c.state() == smithay::backend::drm::ConnectorState::Connected)
        .cloned()
        .collect::<Vec<_>>();

    if connectors.is_empty() {
        warn!("No connected display connectors found");
        return Err(NativeError::Drm);
    }

    let connector = &connectors[0];

    // 5. Create a framebuffer surface
    let mode = connector.modes().first().cloned().ok_or(NativeError::Drm)?;
    let size: Size<i32, smithay::utils::Physical> = (mode.size().w as i32, mode.size().h as i32).into();

    // Create a GBM buffered surface for presentation
    let gbm_surface =
        GbmBufferedSurface::new(&gbm, &egl_context, size, Default::default())
            .map_err(|_| NativeError::Gbm)?;

    info!(
        "DRM display initialized: {}x{} @ {}",
        size.w,
        size.h,
        mode.refresh(),
    );

    // 6. Register libinput for input events
    let mut libinput = smithay::backend::libinput::LibinputInputBackend::new(
        LibinputSessionInterface::new_seat(session)
    );
    // We need the session — for now, use a simplified approach
    // Register input handling
    /*handle
        .insert_source(libinput, |event, _, state| {
            match event {
                InputEvent::Keyboard { event } => {
                    // Forward keyboard to compositor
                }
                InputEvent::PointerMotionAbsolute { event } => {
                    // Forward pointer motion
                }
                InputEvent::PointerButton { event } => {
                    // Forward button events
                }
                _ => {}
            }
        })
        .ok();*/

    // TODO: Full input handling, rendering loop, frame scheduling
    // This is a simplified stub that proves the DRM/GBM/EGL path works

    info!("Native DRM backend initialized (stub — full rendering loop pending)");
    Ok(())
}
