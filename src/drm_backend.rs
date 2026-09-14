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

use crate::drm_topology::{
    ConnectorState, DrmTopology, TopologyConnector, TopologyCrtc, TopologyMode,
};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::backend::SwapBuffersError;
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
/// G-E5.6.3: one output's frame lifecycle state. Transitions are pure
/// functions (unit-tested); the backend applies them per output.
///
/// Submitted vs FlipPending: queue_buffer both submits the frame and
/// arms the page flip in one call today, so the path is
/// Rendering → FlipPending. `Submitted` is reserved for backends where
/// submission and flip-arming split (fence-based future paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFrameState {
    /// No frame in flight.
    Idle,
    /// begin_output succeeded; the renderer is drawing into the output's buffer.
    Rendering,
    /// The frame was handed to KMS (submission without armed flip — reserved).
    Submitted,
    /// Page flip armed; awaiting the vblank completion event.
    FlipPending,
}

/// Illegal lifecycle transition — always a backend-internal bug or a
/// missed drain, never a client-visible condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStateError {
    DoubleBegin,
    BeginDuringFlip,
    SubmitWithoutBegin,
}

impl OutputFrameState {
    pub fn begin(self) -> Result<Self, FrameStateError> {
        match self {
            OutputFrameState::Idle | OutputFrameState::Submitted => {
                Ok(OutputFrameState::Rendering)
            }
            OutputFrameState::Rendering => Err(FrameStateError::DoubleBegin),
            OutputFrameState::FlipPending => Err(FrameStateError::BeginDuringFlip),
        }
    }

    pub fn submit(self) -> Result<Self, FrameStateError> {
        match self {
            OutputFrameState::Rendering => Ok(OutputFrameState::Submitted),
            _ => Err(FrameStateError::SubmitWithoutBegin),
        }
    }

    pub fn arm_flip(self) -> Result<Self, FrameStateError> {
        match self {
            OutputFrameState::Submitted => Ok(OutputFrameState::FlipPending),
            _ => Err(FrameStateError::SubmitWithoutBegin),
        }
    }

    /// A flip-completion event for THIS output. Returns whether the
    /// state changed (a spurious event with nothing pending is a no-op —
    /// the swapchain bookkeeping is one-shot per queue).
    pub fn flip_complete(self) -> bool {
        matches!(
            self,
            OutputFrameState::FlipPending | OutputFrameState::Submitted
        )
    }

    /// G-E5.6.6 hook: a disconnected output's lifecycle ends forcibly —
    /// no flip event will ever arrive for it.
    pub fn force_idle(self) -> Self {
        OutputFrameState::Idle
    }
}

/// G-E5.6.2: one output's presentation state. The GL context
/// (`renderer`) stays per DEVICE; everything that must be output-scoped
/// lives here. With N outputs this vec holds N entries — frame
/// lifecycle generalization is G-E5.6.3/6.4; today one entry is active.
struct OutputPresentation {
    crtc: smithay::reexports::drm::control::crtc::Handle,
    gbm_surface: GbmSurface,
    fb_cache: Vec<CachedFramebuffer>,
    /// G-E5.6.3: the output's frame lifecycle state. Guards the event
    /// drain, whose read() would otherwise block forever when no event
    /// is queued.
    frame_state: OutputFrameState,
    /// (dmabuf, stride) of the buffer bound by begin_frame — used by
    /// the flip-only probe to fill the framebuffer without GL.
    current_buffer: Option<(smithay::backend::allocator::dmabuf::Dmabuf, u32, u32)>,
    width: f32,
    height: f32,
}

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
    renderer: GlesRenderer,
    /// G-E5.6.2: one presentation state per assigned output.
    outputs: Vec<OutputPresentation>,
    /// G-E5.6.5: the topology assignment this backend was built from —
    /// the authoritative OutputId→connector/CRTC/mode record.
    assignment: crate::drm_topology::TopologyAssignment,
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
        if backend.outputs[0].frame_state != OutputFrameState::FlipPending {
            drained += 1;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if backend.outputs[0].frame_state == OutputFrameState::FlipPending {
        return Err("last flip never completed".into());
    }
    info!(flips = drained, "flip probe complete");
    Ok(())
}

impl DrmGraphicsBackend {
    /// Flip probe helper: paint an identifiable pattern into the bound
    /// scanout buffer through a direct dma-buf mmap (no GL involved).
    fn fill_current_pattern(&mut self, frame: u32) -> Result<(), String> {
        let Some((dmabuf, stride, height)) = self.outputs[0].current_buffer.take() else {
            return Err("no current buffer".into());
        };
        self.outputs[0].current_buffer = Some((dmabuf.clone(), stride, height));
        let rw = self
            .outputs[0]
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

        // G-E5.6.2: topology DISCOVERY + deterministic ASSIGNMENT —
        // the backend no longer implicitly means "the first connected
        // display". The full assignment is recorded; outputs[0] drives
        // the (single-output) frame path until G-E5.6.3/6.4 generalize
        // the per-output lifecycle.
        let crtcs = device.crtcs().to_vec();
        if crtcs.is_empty() {
            return Err(DrmBackendError::Drm("no CRTCs available".into()));
        }
        let topology = discover_topology(&drm_fd)
            .ok_or_else(|| DrmBackendError::Drm("topology discovery failed".into()))?;
        let assignment = topology.assign();
        for u in &assignment.unassigned {
            info!(connector = u.connector_id, reason = ?u.reason, "connector unassigned");
        }
        let first = assignment
            .outputs
            .first()
            .ok_or_else(|| DrmBackendError::Drm("no connected connector with a usable CRTC".into()))?;
        info!(
            outputs = assignment.outputs.len(),
            connector = first.connector_id,
            crtc = first.crtc_id,
            mode = ?(first.mode.width, first.mode.height, first.mode.refresh_mhz),
            "DRM topology assigned"
        );

        // Resolve the assignment back to KMS handles.
        let fd_for_handles = &drm_fd;
        let res_handles = fd_for_handles
            .resource_handles()
            .map_err(|e| DrmBackendError::Drm(format!("resources: {:?}", e)))?;
        use smithay::reexports::drm::control::Device as _;
        let conn_handle = *res_handles
            .connectors()
            .iter()
            .find(|c| u32::from(**c) == first.connector_id)
            .ok_or_else(|| DrmBackendError::Drm("assigned connector vanished".into()))?;
        let crtc_handle = *crtcs
            .iter()
            .find(|c| u32::from(**c) == first.crtc_id)
            .ok_or_else(|| DrmBackendError::Drm("assigned CRTC vanished".into()))?;
        let conn_info = fd_for_handles
            .get_connector(conn_handle, true)
            .map_err(|e| DrmBackendError::Drm(format!("connector info: {:?}", e)))?;
        let mode = *conn_info
            .modes()
            .iter()
            .find(|m| {
                m.size().0 as u32 == first.mode.width
                    && m.size().1 as u32 == first.mode.height
                    && (m.vrefresh() as i32) * 1000 == first.mode.refresh_mhz
            })
            .ok_or_else(|| DrmBackendError::Drm("assigned mode not found".into()))?;

        let (w, h) = (mode.size().0 as f32, mode.size().1 as f32);
        info!(crtc = ?crtc_handle, width = w, height = h, "found connected display");

        let surface = device
            .create_surface(crtc_handle, mode, &[conn_handle])
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
            renderer,
            outputs: vec![OutputPresentation {
                crtc: crtc_handle,
                gbm_surface,
                fb_cache: Vec::new(),
                frame_state: OutputFrameState::Idle,
                current_buffer: None,
                width: w,
                height: h,
            }],
            assignment: assignment.clone(),
            frame_seq: 0,
        })
    }

    /// G-E5.6.5: the topology assignment (connector/CRTC/mode per
    /// backend output index — index i of outputs[] corresponds to
    /// assignment.outputs[i]).
    pub fn assignment(&self) -> &crate::drm_topology::TopologyAssignment {
        &self.assignment
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

    /// G-E5.6.4: which output owns this CRTC. Flip events carry the
    /// CRTC — attribution by handle is the ONLY link between a KMS
    /// event and an output's buffer retirement.
    fn output_index_for_crtc(
        &self,
        crtc: smithay::reexports::drm::control::crtc::Handle,
    ) -> Option<usize> {
        self.outputs.iter().position(|o| o.crtc == crtc)
    }

    /// Drain completed page flips. Every flip event is ATTRIBUTED BY
    /// CRTC to exactly one output: flip(A) retires only A's buffer and
    /// never touches B's state (G-E5.6.4 — multi-output bugs here look
    /// like rendering corruption but are buffer-lifetime bugs).
    /// Returns true when any output's flip completed — the calloop flip
    /// source (BUG_LIST #4 step 1) uses that to wake the render loop
    /// event-driven instead of begin_frame polling per frame.
    pub fn handle_flip_events(&mut self) -> bool {
        if !self
            .outputs
            .iter()
            .any(|o| o.frame_state == OutputFrameState::FlipPending)
        {
            return false;
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
            return false;
        }
        let mut completed = false;
        match self.event_fd.receive_events() {
            Ok(events) => {
                for ev in events {
                    if let smithay::reexports::drm::control::Event::PageFlip(flip) = ev {
                        let Some(idx) = self.output_index_for_crtc(flip.crtc) else {
                            continue;
                        };
                        let o = &mut self.outputs[idx];
                        if !o.frame_state.flip_complete() {
                            // Flip event without a pending frame:
                            // the swapchain bookkeeping is
                            // one-shot per queue; harmless.
                            continue;
                        }
                        match o.gbm_surface.frame_submitted() {
                            Ok(Some(_)) => {
                                o.frame_state = OutputFrameState::Idle;
                                completed = true;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                warn!(?e, "frame_submitted failed");
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(?e, "drm event read failed");
            }
        }
        completed
    }

    /// Safety-net drain at the top of begin_frame: with the calloop
    /// flip source active this is normally a no-op (events are consumed
    /// event-driven the moment they arrive), but it guarantees the
    /// swapchain never starves if the source missed a wake.
    fn drain_flips(&mut self) {
        self.handle_flip_events();
    }

    /// Clone of the DRM event fd for the calloop flip-event source.
    pub fn event_device_fd(&self) -> DrmDeviceFd {
        self.event_fd.clone()
    }
}

impl DrmGraphicsBackend {
    /// G-E5.6.3: begin ONE output's frame — drain flips, take the next
    /// swapchain buffer, bind it, and transition Idle → Rendering. The
    /// state transition is explicit: begin during an outstanding flip
    /// is a backend bug (missed drain), never silently tolerated.
    fn begin_output(&mut self, idx: usize) -> Result<(), SwapBuffersError> {
        self.drain_flips();
        let o = self
            .outputs
            .get_mut(idx)
            .ok_or_else(|| SwapBuffersError::TemporaryFailure("no such output".into()))?;
        o.frame_state = o
            .frame_state
            .begin()
            .map_err(|e| SwapBuffersError::TemporaryFailure(format!("begin state: {e:?}").into()))?;
        let (dmabuf, _age) = o.gbm_surface.next_buffer().map_err(|e| {
            SwapBuffersError::TemporaryFailure(format!("next_buffer: {:?}", e).into())
        })?;

        let (w, h) = (o.width as i32, o.height as i32);
        o.current_buffer = Some((
            dmabuf.clone(),
            dmabuf.strides().next().unwrap_or((w as u32) * 4),
            h as u32,
        ));
        self.import_output_framebuffer(idx, &dmabuf, w, h)
    }

    /// Cache the output's next buffer as an EGL image + FBO (once per
    /// swapchain slot) and leave the FBO bound for render_scene's raw GL.
    fn import_output_framebuffer(
        &mut self,
        idx: usize,
        dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
        w: i32,
        h: i32,
    ) -> Result<(), SwapBuffersError> {
        let cached = self.outputs[idx].fb_cache.iter().any(|f| f.dmabuf == *dmabuf);
        if !cached {
            // gbm's dma-buf export is READ-ONLY — re-export with
            // DRM_RDWR so both Mesa's render storage and CPU access
            // can mmap the memory (otherwise: EACCES).
            let rw = self
                .rewrite_dmabuf_rw(dmabuf)
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
            self.outputs[idx].fb_cache.push(CachedFramebuffer {
                dmabuf: dmabuf.clone(),
                rw,
                image,
                rbo,
                fbo,
            });
        }

        // Bind the FBO and leave it current for render_scene's raw GL.
        let fbo = self.outputs[idx]
            .fb_cache
            .iter()
            .find(|f| f.dmabuf == *dmabuf)
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

    /// G-E5.6.3: finish ONE output's frame — flush, submit, arm the
    /// flip. State: Rendering → Submitted → FlipPending.
    fn finish_output(&mut self, idx: usize) -> Result<(), SwapBuffersError> {
        self.renderer
            .with_context(|gl| unsafe {
                gl.Flush();
            })
            .map_err(|e| SwapBuffersError::ContextLost(e.to_string().into()))?;
        self.frame_seq += 1;
        let o = self
            .outputs
            .get_mut(idx)
            .ok_or_else(|| SwapBuffersError::TemporaryFailure("no such output".into()))?;
        o.frame_state = o
            .frame_state
            .submit()
            .map_err(|e| SwapBuffersError::TemporaryFailure(format!("submit state: {e:?}").into()))?;
        o.frame_state = o
            .frame_state
            .arm_flip()
            .map_err(|e| SwapBuffersError::TemporaryFailure(format!("arm state: {e:?}").into()))?;
        o.gbm_surface
            .queue_buffer(None, None, ())
            .map(|_| ())
            .map_err(|e| {
                SwapBuffersError::TemporaryFailure(format!("queue_buffer: {:?}", e).into())
            })
    }

    /// G-E5.6.6 hook: an output left the topology (unplug) — its frame
    /// lifecycle ends forcibly; no flip event will ever arrive for it.
    /// Windows stay alive (they are scene state, not presentation state).
    /// Unused until the 6.6 hotplug wiring; the test suite exercises it.
    #[allow(dead_code)]
    pub fn force_output_idle(&mut self, idx: usize) {
        if let Some(o) = self.outputs.get_mut(idx) {
            o.frame_state = o.frame_state.force_idle();
        }
    }

    /// G-E5.6.6: hotplug unplug on the presentation side. ORDERING
    /// MATTERS: the frame lifecycle quiesces FIRST (force_idle — a
    /// removed output's flip event never arrives), THEN the
    /// presentation state is dropped. Logical output state (registry,
    /// scene, workspaces, other outputs' cameras) is untouched here.
    /// Returns false when the index is out of range.
    pub fn unplug_output(&mut self, idx: usize) -> bool {
        if idx >= self.outputs.len() {
            return false;
        }
        self.force_output_idle(idx);
        self.outputs.remove(idx);
        true
    }
}

impl PresentationBackend for DrmGraphicsBackend {
    fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    fn begin_frame(&mut self) -> Result<(), SwapBuffersError> {
        // Single active output until G-E5.6.5 wires OutputId → index.
        self.begin_output(0)
    }

    fn finish_frame(&mut self) -> Result<(), SwapBuffersError> {
        self.finish_output(0)
    }

    fn size(&self) -> (f32, f32) {
        (self.outputs[0].width, self.outputs[0].height)
    }

    fn egl_surface(&self) -> Option<&EGLSurface> {
        // None: frames render into GBM dmabuf FBOs, not an EGL window
        // surface. render_scene's rebind_surface no-ops accordingly,
        // preserving the FBO binding across with_context closures.
        None
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
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
/// G-E5.6.2: map the real device's connector/CRTC/encoder structure
/// onto the pure topology model (drm_topology.rs). The encoder→CRTC
/// adjacency comes from each encoder's `possible_crtcs` bitmask
/// (bit i = resource list index i).
fn discover_topology(fd: &DrmDeviceFd) -> Option<DrmTopology> {
    use smithay::reexports::drm::control::{self, Device as _};
    let res_handles = fd.resource_handles().ok()?;
    let crtc_handles = res_handles.crtcs();
    let mut topology = DrmTopology::new();
    // encoder handle id → possible_crtcs bitmask
    // encoder handle id → the CRTCs it can feed (via KMS's
    // possible_crtcs filter resolved against the resource list).
    let mut encoder_crtcs: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::new();
    for conn in res_handles.connectors() {
        let info = fd.get_connector(*conn, true).ok()?;
        let state = match info.state() {
            connector::State::Connected => ConnectorState::Connected,
            connector::State::Disconnected => ConnectorState::Disconnected,
            _ => ConnectorState::Unknown,
        };
        let modes = info
            .modes()
            .iter()
            .map(|m| TopologyMode {
                width: m.size().0 as u32,
                height: m.size().1 as u32,
                refresh_mhz: (m.vrefresh() as i32) * 1000,
                preferred: m.mode_type().contains(control::ModeTypeFlags::PREFERRED),
            })
            .collect();
        let mut enc_candidates = Vec::new();
        for eh in info.encoders() {
            if let Ok(ei) = fd.get_encoder(*eh) {
                let eid = u32::from(*eh);
                if !enc_candidates.contains(&eid) {
                    enc_candidates.push(eid);
                }
                let feeds: Vec<u32> = res_handles
                    .filter_crtcs(ei.possible_crtcs())
                    .iter()
                    .map(|c| u32::from(*c))
                    .collect();
                encoder_crtcs.entry(eid).or_insert(feeds);
            }
        }
        topology.add_connector(TopologyConnector {
            id: u32::from(*conn),
            state,
            modes,
            encoder_candidates: enc_candidates,
        });
    }
    // CRTC encoder candidates: every encoder whose possible_crtcs set
    // contains this CRTC.
    for ch in crtc_handles.iter() {
        let cid = u32::from(*ch);
        let mut enc_candidates = Vec::new();
        for (eid, feeds) in &encoder_crtcs {
            if feeds.contains(&cid) && !enc_candidates.contains(eid) {
                enc_candidates.push(*eid);
            }
        }
        topology.add_crtc(TopologyCrtc {
            id: cid,
            encoder_candidates: enc_candidates,
        });
    }
    Some(topology)
}

#[cfg(test)]
mod frame_lifecycle_tests {
    use super::*;
    use smithay::reexports::drm::control as drm_control;

    fn crtc_handle(raw: u32) -> drm_control::crtc::Handle {
        drm_control::from_u32::<drm_control::crtc::Handle>(raw)
            .expect("nonzero handle for test")
    }

    // ---- G-E5.6.3: the state machine.

    #[test]
    fn legal_chain_idle_to_flip_pending_to_idle() {
        let s = OutputFrameState::Idle;
        let s = s.begin().expect("begin from Idle");
        assert_eq!(s, OutputFrameState::Rendering);
        let s = s.submit().expect("submit from Rendering");
        assert_eq!(s, OutputFrameState::Submitted);
        let s = s.arm_flip().expect("arm from Submitted");
        assert_eq!(s, OutputFrameState::FlipPending);
        assert!(s.flip_complete(), "flip event completes FlipPending");
        assert_eq!(s.force_idle(), OutputFrameState::Idle);
    }

    #[test]
    fn begin_from_idle_directly_allowed() {
        // The common path: queue_buffer arms the flip in one call, so
        // the state chain is Idle → Rendering → FlipPending (submit +
        // arm collapse).
        let s = OutputFrameState::Idle.begin().expect("begin");
        let s = s.submit().expect("submit");
        let s = s.arm_flip().expect("arm");
        assert_eq!(s, OutputFrameState::FlipPending);
    }

    #[test]
    fn illegal_transitions_rejected() {
        // Double begin.
        assert_eq!(
            OutputFrameState::Rendering.begin(),
            Err(FrameStateError::DoubleBegin)
        );
        // Begin while a flip is outstanding.
        assert_eq!(
            OutputFrameState::FlipPending.begin(),
            Err(FrameStateError::BeginDuringFlip)
        );
        // Submit without begin.
        assert_eq!(
            OutputFrameState::Idle.submit(),
            Err(FrameStateError::SubmitWithoutBegin)
        );
        // Arm without submit.
        assert_eq!(
            OutputFrameState::Idle.arm_flip(),
            Err(FrameStateError::SubmitWithoutBegin)
        );
        assert_eq!(
            OutputFrameState::Rendering.arm_flip(),
            Err(FrameStateError::SubmitWithoutBegin)
        );
    }

    #[test]
    fn spurious_flip_event_is_noop() {
        assert!(!OutputFrameState::Idle.flip_complete());
        assert!(!OutputFrameState::Rendering.flip_complete());
    }

    #[test]
    fn force_idle_from_every_state() {
        for s in [
            OutputFrameState::Idle,
            OutputFrameState::Rendering,
            OutputFrameState::Submitted,
            OutputFrameState::FlipPending,
        ] {
            assert_eq!(s.force_idle(), OutputFrameState::Idle);
        }
    }

    // ---- G-E5.6.4: event attribution by CRTC (buffer ownership).
    // Synthetic output tables stand in for the real GBM surfaces — the
    // attribution logic under test only maps KMS events → output index.

    struct FakeOutput {
        crtc: drm_control::crtc::Handle,
        state: OutputFrameState,
    }

    fn fake_outputs() -> Vec<FakeOutput> {
        vec![
            FakeOutput {
                crtc: crtc_handle(37),
                state: OutputFrameState::FlipPending,
            },
            FakeOutput {
                crtc: crtc_handle(42),
                state: OutputFrameState::Idle,
            },
        ]
    }

    #[test]
    fn flip_a_does_not_retire_b() {
        let mut outs = fake_outputs();
        let ev_crtc = outs[0].crtc;
        let idx = outs.iter().position(|o| o.crtc == ev_crtc).unwrap();
        assert_eq!(idx, 0);
        // A completes.
        outs[0].state = if outs[0].state.flip_complete() {
            OutputFrameState::Idle
        } else {
            outs[0].state
        };
        // B is UNTOUCHED.
        assert_eq!(outs[0].state, OutputFrameState::Idle);
        assert_eq!(outs[1].state, OutputFrameState::Idle);
    }

    #[test]
    fn attribution_uses_crtc_not_position() {
        // The event's CRTC decides, regardless of vec order: swap the
        // order and A's event still resolves to the output with A's CRTC.
        let mut outs = fake_outputs();
        outs.reverse();
        let ev_crtc = crtc_handle(37);
        let idx = outs.iter().position(|o| o.crtc == ev_crtc).unwrap();
        assert_eq!(idx, 1, "A now sits at index 1");
        assert_eq!(outs[idx].state, OutputFrameState::FlipPending);
    }

    #[test]
    fn unknown_crtc_event_is_dropped() {
        let outs = fake_outputs();
        assert_eq!(outs.iter().position(|o| o.crtc == crtc_handle(999)), None);
    }

    #[test]
    fn mixed_pending_and_completed() {
        // A pending, B completed: A's event completes A; B already idle.
        let mut outs = fake_outputs();
        outs[0].state = OutputFrameState::FlipPending;
        outs[1].state = OutputFrameState::Idle;
        // Event for A:
        assert!(outs[0].state.flip_complete());
        // Event for B (spurious — B is idle):
        assert!(!outs[1].state.flip_complete());
    }

    #[test]
    fn disconnect_forces_idle_over_pending_flip() {
        // G-E5.6.6 hook: an output removed while a flip is outstanding
        // can never see its completion event — the lifecycle ends.
        let mut o = FakeOutput {
            crtc: crtc_handle(37),
            state: OutputFrameState::FlipPending,
        };
        o.state = o.state.force_idle();
        assert_eq!(o.state, OutputFrameState::Idle);
    }
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
