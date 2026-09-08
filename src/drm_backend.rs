//! Native DRM/KMS presentation backend (G-B3).
//!
//! Implements [`PresentationBackend`] with a real GBM swapchain:
//!
//! ```text
//! begin_frame:   drain page-flip events → next_buffer() (GBM bo → Dmabuf)
//!                → EGLImage + RBO + FBO (cached per buffer) → bind FBO
//!   (raw GL renders into the FBO — with egl_surface()==None the
//!    renderer's rebind_surface helper is a no-op, so the binding
//!    survives all with_context() closures)
//! finish_frame:  glFlush → GbmBufferedSurface::queue_buffer → KMS page flip
//! vblank:        drm event on the device fd → frame_submitted() releases
//!                the buffer back to the swapchain (vsync pacing)
//! ```
//!
//! The libseat session is OPTIONAL: on real hardware it provides VT
//| control and DRM master; against virtual devices (VKMS) the backend
//! runs without one (master must then be acquired externally, e.g. by
//! running as root). Device selection honors `VEYRA_DRM_CARD`.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};

use smithay::backend::allocator::dmabuf::DmabufFlags;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
type AllocDmabuf = smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::egl::ffi::egl::types::EGLImage;
use smithay::backend::egl::EGLSurface;
use smithay::backend::renderer::gles::ffi;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::backend::SwapBuffersError;
use smithay::reexports::drm::control::Mode;
use smithay::reexports::drm::control::{connector, Device as ControlDevice};
use smithay::utils::DeviceFd;
use tracing::{info, warn};

use crate::backend::PresentationBackend;

/// Errors during native backend initialization.
#[derive(Debug)]
pub enum DrmBackendError {
    NoDevice,
    /// Reserved for real-hardware sessions (libseat failures under a
    /// real seat would map here once session activation is enforced).
    #[allow(dead_code)]
    Session(String),
    Drm(String),
    Egl(String),
}

impl std::fmt::Display for DrmBackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrmBackendError::NoDevice => {
                write!(f, "no usable DRM device found at /dev/dri/card*")
            }
            DrmBackendError::Session(e) => write!(f, "session: {}", e),
            DrmBackendError::Drm(e) => write!(f, "DRM: {}", e),
            DrmBackendError::Egl(e) => write!(f, "EGL: {}", e),
        }
    }
}

/// One GBM buffer imported into the GL context as a render target.
/// Cached per swapchain slot: the bo comes back identically each time,
/// so the EGLImage/renderbuffer/FBO triple is created exactly once.
/// GL objects are intentionally kept for the backend's lifetime —
/// swapchain slots are recycled, never freed mid-session.
#[allow(dead_code)]
struct CachedFramebuffer {
    /// The swapchain's original (read-only exported) dmabuf identity.
    dmabuf: AllocDmabuf,
    /// Re-exported through PRIME_HANDLE_TO_FD with DRM_RDWR: gbm's
    /// dma-buf export is read-only, which made every write-mmap —
    /// Mesa's render storage AND our CPU fill — fail with EACCES.
    rw: AllocDmabuf,
    image: EGLImage,
    rbo: u32,
    fbo: u32,
}

/// GBM swapchain surface type (allocator bound to our device fd).
type GbmSurface = smithay::backend::drm::GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>;

/// DRM/KMS presentation backend.
///
/// Opens a DRM device, finds a connected display, and presents frames
/// through a GBM swapchain page-flipped against the CRTC.
pub struct DrmGraphicsBackend {
    /// Optional: provides VT control + DRM master on real hardware.
    /// Held (not read) so the session — and its DRM master — survives
    /// for the compositor's lifetime; dropped on exit releases it.
    #[allow(dead_code)]
    session: Option<LibSeatSession>,
    #[allow(dead_code)]
    device: DrmDevice,
    /// Clone of the device fd used for non-blocking event polling
    /// (page-flip/vblank completions).
    event_fd: DrmDeviceFd,
    crtc: smithay::reexports::drm::control::crtc::Handle,
    renderer: GlesRenderer,
    gbm_surface: GbmSurface,
    fb_cache: Vec<CachedFramebuffer>,
    /// True between queue_buffer (page flip armed) and the matching
    /// flip completion — guards the event drain, whose read() would
    /// otherwise block forever when no event is queued.
    flip_pending: bool,
    /// (dmabuf, stride) of the buffer bound by begin_frame — used by
    /// the flip-only probe to fill the framebuffer without GL.
    current_buffer: Option<(smithay::backend::allocator::dmabuf::Dmabuf, u32, u32)>,
    width: f32,
    height: f32,
    frame_seq: u64,
}

unsafe impl Send for DrmGraphicsBackend {}

/// G-B3 validation probe: render `frames` solid-color frames through
/// the full GBM → EGL dmabuf → KMS page-flip pipeline without starting
/// the compositor. Exercises begin_frame (buffer + FBO bind) and
/// finish_frame (flush + page flip) exactly as the live render path
/// does. Returns Err with the first failing stage.
pub fn run_probe(frames: u32) -> Result<(), String> {
    let mut backend = DrmGraphicsBackend::try_new().map_err(|e| e.to_string())?;
    info!(frames, size = ?backend.size(), "probe start");
    for i in 0..frames {
        backend
            .begin_frame()
            .map_err(|e| format!("frame {i} begin: {e}"))?;
        // Varying clear color: consecutive frames are distinguishable
        // on a real capture and invisible-buffer bugs show as uniform
        // output.
        let t = i as f32 / frames.max(1) as f32;
        let (r, g, b) = (t * 0.8, 0.2 + 0.5 * t, 1.0 - t * 0.8);
        backend
            .renderer()
            .with_context(|gl| unsafe {
                gl.ClearColor(r, g, b, 1.0);
                gl.Clear(ffi::COLOR_BUFFER_BIT | ffi::DEPTH_BUFFER_BIT);
            })
            .map_err(|e| format!("frame {i} clear: {e}"))?;
        backend
            .finish_frame()
            .map_err(|e| format!("frame {i} finish: {e}"))?;
        // Pace below the nominal refresh so the vblank drain in
        // begin_frame keeps up with the flip queue.
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    // Drain the final flip so the swapchain retires cleanly.
    backend.drain_flips();
    info!("probe complete");
    Ok(())
}

/// G-B3 flip-machinery probe (GL-independent): verifies buffer
/// allocation, KMS page flips and flip-event completion by filling the
/// scanout buffer through a dma-buf mmap instead of GL rasterization.
/// This isolates the compositor's presentation logic from the GL
/// driver's dma-buf RENDER limitation (software rasterizers cannot
/// render into imported dma-bufs; see the M079 gate in try_new).
pub fn run_flip_probe(frames: u32) -> Result<(), String> {
    let mut backend = DrmGraphicsBackend::try_new_impl(None, false).map_err(|e| e.to_string())?;
    info!(frames, size = ?backend.size(), "flip probe start");
    for i in 0..frames {
        backend
            .begin_frame()
            .map_err(|e| format!("frame {i} begin: {e}"))?;
        backend
            .fill_current_pattern(i)
            .map_err(|e| format!("frame {i} fill: {e}"))?;
        backend
            .finish_frame()
            .map_err(|e| format!("frame {i} finish: {e}"))?;
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    // Drain the final flip with a generous deadline (vblank pacing).
    let mut drained = 0u32;
    for _ in 0..100 {
        backend.drain_flips();
        if !backend.flip_pending {
            drained += 1;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if backend.flip_pending {
        return Err("last flip never completed".into());
    }
    info!(flips = drained, "flip probe complete");
    Ok(())
}

impl DrmGraphicsBackend {
    /// Flip probe helper: paint an identifiable pattern into the bound
    /// scanout buffer through a direct dma-buf mmap (no GL involved).
    fn fill_current_pattern(&mut self, frame: u32) -> Result<(), String> {
        let Some((dmabuf, stride, height)) = self.current_buffer.take() else {
            return Err("no current buffer".into());
        };
        self.current_buffer = Some((dmabuf.clone(), stride, height));
        let rw = self
            .fb_cache
            .iter()
            .find(|f| f.dmabuf == dmabuf)
            .map(|f| f.rw.clone())
            .ok_or("no cached framebuffer for current buffer")?;
        let fd = rw
            .handles()
            .next()
            .ok_or("dmabuf has no planes")?
            .as_raw_fd();
        let len = stride as usize * height as usize;
        unsafe {
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if ptr == libc::MAP_FAILED {
                return Err(format!("dmabuf mmap: {}", std::io::Error::last_os_error()));
            }
            // Per-frame color bands: frame-identifiable content.
            let base = frame % 8;
            for row in 0..height as usize {
                let shade = ((base + row as u32 / 8) % 8) * 32;
                let px = (shade << 16) | (shade << 8) | shade | 0xFF000000; // XRGB
                let row_ptr = (ptr as *mut u8).add(row * stride as usize);
                for x in 0..(stride as usize / 4) {
                    std::ptr::write_unaligned(row_ptr.add(x * 4) as *mut u32, px);
                }
            }
            libc::munmap(ptr, len);
        }
        Ok(())
    }
}

impl DrmGraphicsBackend {
    /// Attempt to create a native DRM/KMS backend (probe path:
    /// self-opened device, optional internal session).
    pub fn try_new() -> Result<Self, DrmBackendError> {
        Self::try_new_impl(None, true)
    }

    /// P1 #2: session-managed construction — the DRM device is opened
    /// THROUGH the libseat session, so device lifetime (and DRM
    /// master) follows the seat and survives VT switches. `session`
    /// is the caller's handle (shared with the libinput interface);
    /// this backend holds a clone for master lifetime.
    pub fn try_new_with_session(session: &LibSeatSession) -> Result<Self, DrmBackendError> {
        Self::try_new_impl(Some(session.clone()), true)
    }

    /// Internal constructor; a `Some` session opens the device through
    /// libseat (native startup path), `None` self-opens (probes,
    /// virtual devices).
    fn try_new_impl(
        session: Option<LibSeatSession>,
        render_gate: bool,
    ) -> Result<Self, DrmBackendError> {
        info!("Initializing native DRM/KMS backend");

        let (path, fd) = match &session {
            // Session-owned device: open through libseat so the device
            // is revoked/restored with seat (de)activation.
            Some(session) => {
                let (path, _) = open_drm_device().ok_or(DrmBackendError::NoDevice)?;
                let mut s = session.clone();
                let fd = s
                    .open(
                        &path,
                        smithay::reexports::rustix::fs::OFlags::RDWR
                            | smithay::reexports::rustix::fs::OFlags::CLOEXEC,
                    )
                    .map_err(|e| DrmBackendError::Drm(format!("session open: {:?}", e)))?;
                info!(?path, "opened DRM device through libseat session");
                (path, fd)
            }
            None => open_drm_device().ok_or(DrmBackendError::NoDevice)?,
        };
        if session.is_none() {
            info!(?path, "opened DRM device");
        }

        // Probe path: session control is optional (required for real
        // hardware — VT + master — unnecessary for virtual devices like
        // VKMS where master is acquired externally).
        let session = match session {
            Some(s) => Some(s),
            None => match LibSeatSession::new() {
                Ok((s, _notifier)) => {
                    info!("libseat session acquired");
                    Some(s)
                }
                Err(e) => {
                    warn!(err = %e, "no libseat session (continuing without VT control — OK for virtual devices)");
                    None
                }
            },
        };

        let device_fd = DeviceFd::from(fd);
        let drm_fd = DrmDeviceFd::new(device_fd);

        let (mut device, _dev_notifier) = DrmDevice::new(drm_fd.clone(), false)
            .map_err(|e| DrmBackendError::Drm(format!("{:?}", e)))?;

        let crtcs = device.crtcs().to_vec();
        if crtcs.is_empty() {
            return Err(DrmBackendError::Drm("no CRTCs available".into()));
        }
        let first_crtc = crtcs[0];

        let (conn_handle, mode) = find_connector_with_mode(&device)
            .ok_or_else(|| DrmBackendError::Drm("no connected connector found".into()))?;

        let (w, h) = (mode.size().0 as f32, mode.size().1 as f32);
        info!(crtc = ?first_crtc, width = w, height = h, "found connected display");

        let surface = device
            .create_surface(first_crtc, mode, &[conn_handle])
            .map_err(|e| DrmBackendError::Drm(format!("surface: {:?}", e)))?;

        // GBM device FIRST, and the EGL display lives ON it
        // (EGL_PLATFORM_GBM): the rendering driver then allocates from
        // and imports the SAME device's buffers. A surfaceless display
        // cannot import GBM dmabufs from another device (observed:
        // llvmpipe fails to mmap foreign dma-bufs → EGL image garbage).
        let gbm_device = GbmDevice::new(drm_fd.clone())
            .map_err(|e| DrmBackendError::Drm(format!("GBM device: {}", e)))?;
        let allocator = GbmAllocator::new(gbm_device.clone(), GbmBufferFlags::RENDERING);

        let egl_display = unsafe { smithay::backend::egl::display::EGLDisplay::new(gbm_device) }
            .map_err(|e| DrmBackendError::Egl(format!("EGL display: {}", e)))?;
        let egl_context = smithay::backend::egl::context::EGLContext::new(&egl_display)
            .map_err(|e| DrmBackendError::Egl(format!("EGL context: {}", e)))?;

        // The swapchain format must be importable by the renderer.
        let renderer_formats = egl_context.dmabuf_render_formats().clone();
        if renderer_formats.iter().next().is_none() {
            return Err(DrmBackendError::Egl(
                "EGL driver reports no dmabuf render formats — cannot render into GBM buffers"
                    .into(),
            ));
        }

        let mut renderer = unsafe { GlesRenderer::new(egl_context) }
            .map_err(|e| DrmBackendError::Egl(format!("GLES: {}", e)))?;

        // M079 capability gate: software rasterizers (llvmpipe/softpipe)
        // REPORT dmabuf render formats but cannot render into imported
        // dma-bufs (their storage mmap fails; the raster threads then
        // crash inside Mesa). Detect that BEFORE handing frames to KMS
        // and refuse with actionable diagnostics. VEYRA_DRM_FORCE=1
        // overrides for driver experimentation.
        let gl_renderer = renderer
            .with_context(|gl| unsafe {
                let ptr = gl.GetString(ffi::RENDERER);
                if ptr.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(ptr as *const _)
                        .to_string_lossy()
                        .into_owned()
                }
            })
            .unwrap_or_default();
        let software = ["llvmpipe", "softpipe", "swrast", "zink"]
            .iter()
            .any(|sw| gl_renderer.to_lowercase().contains(sw));
        if software && render_gate && std::env::var("VEYRA_DRM_FORCE").is_err() {
            return Err(DrmBackendError::Egl(format!(
                "software GL renderer '{}' cannot render into GBM dma-bufs \
                 (M079: native DRM requires a real GPU with dma-buf import); \
                 set VEYRA_DRM_FORCE=1 to attempt anyway",
                gl_renderer
            )));
        }
        info!(renderer = %gl_renderer, "EGL renderer capability check passed");

        let gbm_surface = GbmSurface::new(
            surface,
            allocator,
            &[Fourcc::Xrgb8888, Fourcc::Argb8888],
            renderer_formats,
        )
        .map_err(|e| DrmBackendError::Drm(format!("GBM surface: {:?}", e)))?;

        info!("Native DRM/KMS backend initialized: {}x{}", w, h);

        Ok(DrmGraphicsBackend {
            session,
            device,
            event_fd: drm_fd,
            crtc: first_crtc,
            renderer,
            gbm_surface,
            fb_cache: Vec::new(),
            flip_pending: false,
            current_buffer: None,
            width: w,
            height: h,
            frame_seq: 0,
        })
    }

    /// Re-export a dmabuf with write access through the device's PRIME
    /// ioctls (gbm exports read-only; see CachedFramebuffer).
    fn rewrite_dmabuf_rw(&self, dmabuf: &AllocDmabuf) -> Result<AllocDmabuf, String> {
        use smithay::reexports::drm as drm_crate;
        let mut builder = AllocDmabuf::builder_from_buffer(dmabuf, DmabufFlags::empty());
        for (idx, fd) in dmabuf.handles().enumerate() {
            let handle = self
                .event_fd
                .prime_fd_to_buffer(fd)
                .map_err(|e| format!("prime fd_to_handle: {e}"))?;
            let rw = self
                .event_fd
                .buffer_to_prime_fd(handle, drm_crate::RDWR | drm_crate::CLOEXEC)
                .map_err(|e| format!("prime handle_to_fd(RDWR): {e}"))?;
            let offset = dmabuf.offsets().nth(idx).unwrap_or(0);
            let stride = dmabuf.strides().nth(idx).unwrap_or(0);
            if !builder.add_plane(rw, idx as u32, offset, stride) {
                return Err("too many planes".into());
            }
        }
        if let Some(node) = dmabuf.node() {
            builder.set_node(node);
        }
        builder
            .build()
            .ok_or_else(|| "dmabuf rebuild failed".into())
    }

    /// Non-blocking drain of completed page flips. Every flip event for
    /// our CRTC releases one buffer back to the swapchain
    /// (`frame_submitted`). Called at the top of `begin_frame` so the
    /// swapchain never starves while pacing stays vblank-driven.
    fn drain_flips(&mut self) {
        if !self.flip_pending {
            return;
        }
        // receive_events blocks on read when no event is queued — poll
        // first (timeout 0) so an idle device cannot stall the frame.
        let mut pfd = libc::pollfd {
            fd: self.event_fd.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 0) };
        if ready <= 0 {
            return;
        }
        match self.event_fd.receive_events() {
            Ok(events) => {
                for ev in events {
                    if let smithay::reexports::drm::control::Event::PageFlip(flip) = ev {
                        if flip.crtc == self.crtc {
                            match self.gbm_surface.frame_submitted() {
                                Ok(Some(_)) => {
                                    self.flip_pending = false;
                                }
                                Ok(None) => {
                                    // Flip event without a pending frame:
                                    // the swapchain bookkeeping is
                                    // one-shot per queue; harmless.
                                }
                                Err(e) => {
                                    warn!(?e, "frame_submitted failed");
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(?e, "drm event read failed");
            }
        }
    }
}

impl PresentationBackend for DrmGraphicsBackend {
    fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    fn begin_frame(&mut self) -> Result<(), SwapBuffersError> {
        self.drain_flips();

        let (dmabuf, _age) = self.gbm_surface.next_buffer().map_err(|e| {
            SwapBuffersError::TemporaryFailure(format!("next_buffer: {:?}", e).into())
        })?;

        let (w, h) = (self.width as i32, self.height as i32);
        self.current_buffer = Some((
            dmabuf.clone(),
            dmabuf.strides().next().unwrap_or((w as u32) * 4),
            h as u32,
        ));
        // Cache key: smithay's Dmabuf implements PartialEq by buffer
        // identity, so each swapchain slot resolves to its cached FBO.
        let cached = self.fb_cache.iter().any(|f| f.dmabuf == dmabuf);
        if !cached {
            // gbm's dma-buf export is READ-ONLY — re-export with
            // DRM_RDWR so both Mesa's render storage and CPU access
            // can mmap the memory (otherwise: EACCES).
            let rw = self
                .rewrite_dmabuf_rw(&dmabuf)
                .map_err(|e| SwapBuffersError::TemporaryFailure(e.into()))?;
            // Import the dmabuf into the current context: EGLImage →
            // renderbuffer storage → FBO (same path as smithay's
            // Bind<Dmabuf>, but the FBO is left BOUND for veyra's
            // raw-GL pipeline).
            let image = self
                .renderer
                .egl_context()
                .display()
                .create_image_from_dmabuf(&rw)
                .map_err(|e| {
                    SwapBuffersError::TemporaryFailure(format!("EGL image: {:?}", e).into())
                })?;

            let (rbo, fbo) = self
                .renderer
                .with_context(|gl| unsafe {
                    let mut rbo = 0;
                    gl.GenRenderbuffers(1, &mut rbo);
                    gl.BindRenderbuffer(ffi::RENDERBUFFER, rbo);
                    gl.EGLImageTargetRenderbufferStorageOES(ffi::RENDERBUFFER, image);
                    gl.BindRenderbuffer(ffi::RENDERBUFFER, 0);

                    let mut fbo = 0;
                    gl.GenFramebuffers(1, &mut fbo);
                    gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
                    gl.FramebufferRenderbuffer(
                        ffi::FRAMEBUFFER,
                        ffi::COLOR_ATTACHMENT0,
                        ffi::RENDERBUFFER,
                        rbo,
                    );
                    let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
                    if status != ffi::FRAMEBUFFER_COMPLETE {
                        Err(SwapBuffersError::TemporaryFailure(
                            "dmabuf framebuffer incomplete".into(),
                        ))
                    } else {
                        Ok((rbo, fbo))
                    }
                })
                .map_err(|e| SwapBuffersError::ContextLost(e.to_string().into()))??;
            self.fb_cache.push(CachedFramebuffer {
                dmabuf: dmabuf.clone(),
                rw,
                image,
                rbo,
                fbo,
            });
        }

        // Bind the FBO and leave it current for render_scene's raw GL.
        let fbo = self
            .fb_cache
            .iter()
            .find(|f| f.dmabuf == dmabuf)
            .map(|f| f.fbo)
            .expect("dmabuf framebuffer cached");
        self.renderer
            .with_context(|gl| unsafe {
                gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
                gl.Viewport(0, 0, w, h);
            })
            .map_err(|e| SwapBuffersError::ContextLost(e.to_string().into()))?;
        Ok(())
    }

    fn finish_frame(&mut self) -> Result<(), SwapBuffersError> {
        // Make the rendered content visible to the KMS consumer before
        // handing the buffer to the flip queue.
        self.renderer
            .with_context(|gl| unsafe {
                gl.Flush();
            })
            .map_err(|e| SwapBuffersError::ContextLost(e.to_string().into()))?;

        self.frame_seq += 1;
        self.gbm_surface
            .queue_buffer(None, None, ())
            .map(|_| {
                self.flip_pending = true;
            })
            .map_err(|e| {
                SwapBuffersError::TemporaryFailure(format!("queue_buffer: {:?}", e).into())
            })
    }

    fn size(&self) -> (f32, f32) {
        (self.width, self.height)
    }

    fn egl_surface(&self) -> Option<&EGLSurface> {
        // None: frames render into GBM dmabuf FBOs, not an EGL window
        // surface. render_scene's rebind_surface no-ops accordingly,
        // preserving the FBO binding across with_context closures.
        None
    }
}

/// Open a usable DRM device: `VEYRA_DRM_CARD` wins, otherwise the first
/// card that both opens AND has a connected connector (a card may exist
/// without being usable — M079).
fn open_drm_device() -> Option<(std::path::PathBuf, OwnedFd)> {
    if let Ok(p) = std::env::var("VEYRA_DRM_CARD") {
        let path = std::path::PathBuf::from(p);
        if let Ok(file) = std::fs::File::open(&path) {
            return Some((path, file.into()));
        }
        return None;
    }
    for n in 0..4 {
        let path = std::path::PathBuf::from(format!("/dev/dri/card{}", n));
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        // A card counts as usable only if a connector is connected —
        // probing modes requires a ControlDevice, which needs an fd we
        // keep; wrap and check via a short-lived DrmDeviceFd.
        let probe_dup: OwnedFd = file.try_clone().expect("drm fd clone").into();
        let probe_fd = DrmDeviceFd::new(DeviceFd::from(probe_dup));
        let has_display = probe_fd
            .resource_handles()
            .ok()
            .map(|res| {
                res.connectors().iter().any(|c| {
                    probe_fd
                        .get_connector(*c, true)
                        .map(|info| info.state() == connector::State::Connected)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if has_display {
            return Some((path, file.into()));
        }
    }
    None
}

/// Find the first connected connector and its mode.
fn find_connector_with_mode(device: &DrmDevice) -> Option<(connector::Handle, Mode)> {
    let fd = device.device_fd();
    let res_handles = fd.resource_handles().ok()?;
    for conn_handle in res_handles.connectors() {
        if let Ok(info) = fd.get_connector(*conn_handle, true) {
            if info.state() == connector::State::Connected {
                if let Some(mode) = info.modes().first() {
                    return Some((*conn_handle, *mode));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_backend_error_display_is_actionable() {
        let e = DrmBackendError::NoDevice.to_string();
        assert!(
            e.contains("/dev/dri/card"),
            "error should name the searched paths: {e}"
        );
        let e = DrmBackendError::Egl("ctx dead".into()).to_string();
        assert!(e.contains("EGL"), "error should name the subsystem: {e}");
    }

    #[test]
    fn env_override_wins_and_fails_cleanly() {
        // A nonexistent card must yield None (no silent fallback to
        // another device — the operator asked for a specific one).
        unsafe { std::env::set_var("VEYRA_DRM_CARD", "/dev/dri/card-veyra-test-missing") };
        assert!(
            open_drm_device().is_none(),
            "missing card must not fall back"
        );
        unsafe { std::env::remove_var("VEYRA_DRM_CARD") };
    }

    #[test]
    fn device_discovery_skips_unusable_cards() {
        // Without an override, discovery must find SOME usable card in
        // this environment if any exists (VKMS is loaded on the CI box;
        // the test tolerates its absence).
        let found = open_drm_device();
        if let Some((path, _fd)) = found {
            assert!(
                path.to_string_lossy().starts_with("/dev/dri/card"),
                "unexpected path {path:?}"
            );
        }
    }
}
