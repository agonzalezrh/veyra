//! Native DRM/KMS presentation backend.
//!
//! Implements [`PresentationBackend`] using Smithay's DRM infrastructure.
//! This backend replaces the winit nested backend when running directly on hardware.

use std::os::unix::io::OwnedFd;

use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmSurface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::SwapBuffersError;
use smithay::reexports::drm::control::{connector, crtc, Device as ControlDevice};
use smithay::reexports::drm::control::Mode;
use smithay::utils::DeviceFd;
use tracing::{error, info, warn};

use crate::backend::PresentationBackend;

/// Errors during native backend initialization.
#[derive(Debug)]
pub enum DrmBackendError {
    NoDevice,
    Session(String),
    Drm(String),
}

impl std::fmt::Display for DrmBackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrmBackendError::NoDevice => {
                write!(f, "no DRM device found at /dev/dri/card*")
            }
            DrmBackendError::Session(e) => write!(f, "session: {}", e),
            DrmBackendError::Drm(e) => write!(f, "DRM: {}", e),
        }
    }
}

/// DRM/KMS presentation backend.
///
/// Opens a DRM device, creates a session, finds a connected display,
/// and sets up rendering.
pub struct DrmGraphicsBackend {
    #[allow(dead_code)]
    session: LibSeatSession,
    #[allow(dead_code)]
    device: DrmDevice,
    #[allow(dead_code)]
    surface: DrmSurface,
    renderer: GlesRenderer,
    width: f32,
    height: f32,
}

impl DrmGraphicsBackend {
    /// Attempt to create a native DRM/KMS backend.
    pub fn try_new() -> Result<Self, DrmBackendError> {
        info!("Initializing native DRM/KMS backend");

        let path = find_drm_device().ok_or(DrmBackendError::NoDevice)?;
        info!(?path, "opening DRM device");

        // Open and wrap the device fd
        let file = std::fs::File::open(&path)
            .map_err(|e| DrmBackendError::Drm(format!("open {}: {}", path.display(), e)))?;
        let owned: OwnedFd = file.into();
        let device_fd = DeviceFd::from(owned);
        let drm_fd = DrmDeviceFd::new(device_fd);

        // Create session
        let (session, _notifier) = LibSeatSession::new()
            .map_err(|e| DrmBackendError::Session(format!("{}", e)))?;

        // Create DRM device (disable_connectors = false to keep them active)
        let (mut device, _notifier) =
            DrmDevice::new(drm_fd, false).map_err(|e| DrmBackendError::Drm(format!("{:?}", e)))?;

        // Find first CRTC
        let crtcs = device.crtcs().to_vec();
        if crtcs.is_empty() {
            return Err(DrmBackendError::Drm("no CRTCs available".into()));
        }
        let first_crtc = crtcs[0];

        // Find connected connector with its mode
        let (conn_handle, mode) = find_connector_with_mode(&device)
            .ok_or_else(|| DrmBackendError::Drm("no connected connector found".into()))?;

        let (w, h) = (mode.size().0 as f32, mode.size().1 as f32);
        info!(crtc = ?first_crtc, width = w, height = h, "found connected display");

        // Create a DRM surface for this CRTC/connector
        let surface = device
            .create_surface(first_crtc, mode, &[conn_handle])
            .map_err(|e| DrmBackendError::Drm(format!("surface: {:?}", e)))?;

        // Set up EGL/GLES for rendering
        let egl_display = unsafe {
            smithay::backend::egl::display::EGLDisplay::new(
                smithay::backend::egl::native::EGLSurfacelessDisplay,
            )
        }
        .map_err(|e| DrmBackendError::Drm(format!("EGL display: {}", e)))?;
        let egl_context = smithay::backend::egl::context::EGLContext::new(&egl_display)
            .map_err(|e| DrmBackendError::Drm(format!("EGL context: {}", e)))?;
        let renderer = unsafe { GlesRenderer::new(egl_context) }
            .map_err(|e| DrmBackendError::Drm(format!("GLES: {}", e)))?;

        info!(
            "Native DRM/KMS backend initialized: {}x{}",
            w, h
        );

        Ok(DrmGraphicsBackend {
            session,
            device,
            surface,
            renderer,
            width: w,
            height: h,
        })
    }
}

impl PresentationBackend for DrmGraphicsBackend {
    fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    fn begin_frame(&mut self) -> Result<(), SwapBuffersError> {
        Ok(())
    }

    fn finish_frame(&mut self) -> Result<(), SwapBuffersError> {
        Ok(())
    }

    fn size(&self) -> (f32, f32) {
        (self.width, self.height)
    }
}

/// Find the first available DRM device node.
fn find_drm_device() -> Option<std::path::PathBuf> {
    for n in 0..4 {
        let p = std::path::PathBuf::from(format!("/dev/dri/card{}", n));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Find the first connected connector and its mode.
fn find_connector_with_mode(
    device: &DrmDevice,
) -> Option<(connector::Handle, Mode)> {
    let fd = device.device_fd();
    let res_handles = fd.resource_handles().ok()?;
    for conn_handle in res_handles.connectors() {
        if let Ok(info) = fd.get_connector(*conn_handle, true) {
            if info.state() == connector::State::Connected {
                if let Some(mode) = info.modes().first() {
                    return Some((*conn_handle, mode.clone()));
                }
            }
        }
    }
    None
}
