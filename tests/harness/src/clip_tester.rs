//! Raw-protocol clipboard tester (`client-kit clip`, G-B1).
//!
//! Same raw-wl_data_device scaffolding as `dnd_tester`, but SELECTION
//! semantics (distinct from drag semantics):
//!
//!   --mode set   --mimes a,b,c --payload P
//!       create a wl_data_source, offer every mime, set_selection.
//!       Logs clip_set_done / clip_target / clip_send / clip_cancelled.
//!       The source writes --payload to the send fd.
//!
//!   --mode paste --mime M
//!       logs clip_offer / clip_mime / clip_selection / clip_cleared,
//!       then receive(--mime) → read fd → clip_data(mime, len, payload).
//!       Requesting an unsupported mime yields no send (source never
//!       sees it) and an empty read → clip_data with len 0.
//!
//! G-D5: `--primary` switches the whole flow to the PRIMARY selection
//! (zwp_primary_selection_device_manager_v1) — used to verify the X11
//! selection bridge end-to-end (xterm sets PRIMARY; the clip client
//! pastes it).
//!
//! NOTE: smithay denies set_selection from clients without keyboard
//! focus (device.rs SetSelection handler) — the harness must ensure
//! the setter window has keyboard focus before setting.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::client::protocol::wl_shm::Format;
use smithay_client_toolkit::reexports::client::{
    protocol::{wl_data_offer, wl_data_source, wl_surface},
    Dispatch,
};
use smithay_client_toolkit::reexports::protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_manager_v1;
use smithay_client_toolkit::reexports::protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
    zwp_primary_selection_source_v1,
};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
    xdg_surface, xdg_toplevel, xdg_wm_base,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shm::{slot::Buffer, slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_data_device, wl_data_device_manager, wl_output, wl_registry, wl_seat},
    Connection, QueueHandle,
};

const W: i32 = 240;
const H: i32 = 160;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Set,
    Paste,
}

#[derive(Default)]
struct OfferState {
    mimes: Vec<String>,
}

pub struct ClipTester {
    mode: Mode,
    mimes: Vec<String>,
    payload: String,
    /// G-D5: operate on the PRIMARY selection instead of the clipboard.
    primary: bool,
    shm: Shm,
    registry_state: RegistryState,
    output_state: OutputState,
    ddm: wl_data_device_manager::WlDataDeviceManager,
    /// G-D5: raw primary selection manager (bound only with --primary).
    primary_manager:
        Option<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>,

    surface: wl_surface::WlSurface,
    _xdg_surface: xdg_surface::XdgSurface,
    _toplevel: xdg_toplevel::XdgToplevel,
    pool: Option<SlotPool>,
    _buffer: Option<Buffer>,
    configured: bool,

    device: Option<wl_data_device::WlDataDevice>,
    /// G-D5: primary selection device (--primary mode).
    primary_device: Option<zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1>,
    /// G-D5: primary selection source (set mode) — kept alive for
    /// send/cancel events.
    primary_source: Option<zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1>,
    /// G-D5: active primary offer (paste mode) + its mimes.
    primary_offer: Option<zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1>,
    primary_offer_mimes: Vec<String>,
    source: Option<wl_data_source::WlDataSource>,
    offers: HashMap<wl_data_offer::WlDataOffer, OfferState>,
    active_offer: Option<wl_data_offer::WlDataOffer>,
    set_done: bool,
    configured_at: Option<Instant>,
    exit: bool,
}

pub fn run_clip(
    mode: Mode,
    mimes: Vec<String>,
    payload: String,
    duration_ms: u64,
    primary: bool,
) -> i32 {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("connect to wayland: NoCompositor");
            return 1;
        }
    };
    let (globals, mut event_queue) = match registry_queue_init::<ClipTester>(&conn) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("registry init failed");
            return 2;
        }
    };
    let qh = event_queue.handle();
    let compositor = match CompositorState::bind(&globals, &qh) {
        Ok(c) => c,
        Err(_) => return 2,
    };
    let shm = match Shm::bind(&globals, &qh) {
        Ok(s) => s,
        Err(_) => return 2,
    };
    let wm_base: xdg_wm_base::XdgWmBase = match globals.bind(&qh, 1..=5, ()) {
        Ok(w) => w,
        Err(_) => return 2,
    };
    let ddm: wl_data_device_manager::WlDataDeviceManager = match globals.bind(&qh, 1..=3, ()) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("wl_data_device_manager not available");
            return 2;
        }
    };
    let seat: wayland_client::protocol::wl_seat::WlSeat = match globals.bind(&qh, 1..=7, ()) {
        Ok(s) => s,
        Err(_) => return 2,
    };

    // G-D5: bind the primary selection manager when --primary is set.
    let primary_manager = if primary {
        match globals.bind(&qh, 1..=1, ()) {
            Ok(m) => Some(m),
            Err(_) => {
                eprintln!("zwp_primary_selection_device_manager_v1 not available");
                return 2;
            }
        }
    } else {
        None
    };

    let surface = compositor.create_surface(&qh);
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    let (title, app_id) = match (mode, primary) {
        (Mode::Set, false) => ("client-kit clip-set", "clip-set"),
        (Mode::Paste, false) => ("client-kit clip-paste", "clip-paste"),
        (Mode::Set, true) => ("client-kit clip-set-primary", "clip-set-primary"),
        (Mode::Paste, true) => ("client-kit clip-paste-primary", "clip-paste-primary"),
    };
    toplevel.set_title(title.into());
    toplevel.set_app_id(app_id.into());
    surface.commit();

    let device = ddm.get_data_device(&seat, &qh, ());
    let primary_device = primary_manager.as_ref().map(
        |m: &zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1| {
            m.get_device(&seat, &qh, ())
        },
    );

    let mut tester = ClipTester {
        mode,
        mimes,
        payload,
        primary,
        shm,
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        ddm,
        primary_manager,
        surface,
        _xdg_surface: xdg_surface,
        _toplevel: toplevel,
        pool: None,
        _buffer: None,
        configured: false,
        device: Some(device),
        primary_device,
        primary_source: None,
        primary_offer: None,
        primary_offer_mimes: Vec::new(),
        source: None,
        offers: HashMap::new(),
        active_offer: None,
        set_done: false,
        configured_at: None,
        exit: false,
    };

    let deadline = Instant::now() + Duration::from_millis(duration_ms);
    if event_queue.roundtrip(&mut tester).is_err() {
        eprintln!("roundtrip error");
        return 2;
    }
    loop {
        if tester.exit || Instant::now() >= deadline {
            break;
        }
        // Set the selection 300ms after configure: smithay denies
        // set_selection from clients without keyboard focus, and the
        // keyboard-enter must have been processed first (focus-on-map).
        if !tester.set_done
            && tester.mode == Mode::Set
            && tester
                .configured_at
                .map(|t| t.elapsed() >= Duration::from_millis(300))
                .unwrap_or(false)
        {
            let qh = qh.clone();
            tester.set_selection(&qh);
        }
        let _ = conn.flush();
        use std::os::fd::AsRawFd as _;
        let fd = conn.backend().poll_fd().as_raw_fd();
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 50) };
        if ret > 0 {
            if let Some(guard) = conn.prepare_read() {
                let _ = guard.read();
            }
            if pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                let _ = event_queue.dispatch_pending(&mut tester);
                break;
            }
        }
        if event_queue.dispatch_pending(&mut tester).is_err() {
            eprintln!("dispatch error");
            break;
        }
    }
    let _ = conn.flush();
    crate::log_kv(&[("ev", "exit".into()), ("mode", format!("{mode:?}").into())]);
    0
}

impl ClipTester {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.pool.is_none() {
            self.pool = SlotPool::new(2 * W as usize * H as usize, &self.shm).ok();
        }
        let Some(pool) = &mut self.pool else { return };
        let (buffer, canvas) = match pool.create_buffer(W, H, W * 4, Format::Argb8888) {
            Ok(b) => b,
            Err(_) => return,
        };
        // Setter: orange; paster: purple.
        let color: [u8; 4] = match self.mode {
            Mode::Set => [0x10, 0x90, 0xF0, 0xFF],
            Mode::Paste => [0xF0, 0x10, 0x90, 0xFF],
        };
        for chunk in canvas.as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&color);
        }
        buffer.attach_to(&self.surface).expect("clip attach");
        self._buffer = Some(buffer);
        self.surface.commit();
    }

    /// Paste mode: request the wanted mime from the active offer as
    /// soon as BOTH preconditions are known — smithay may deliver the
    /// mime list and the selection event in either order.
    fn maybe_receive(&self, offer: &wl_data_offer::WlDataOffer) {
        if self.mode != Mode::Paste || self.active_offer.as_ref() != Some(offer) {
            return;
        }
        let Some(want) = self.mimes.first() else {
            return;
        };
        let Some(os) = self.offers.get(offer) else {
            return;
        };
        if os.mimes.iter().any(|m| m == want) {
            receive_and_log(offer, want);
        }
    }

    /// Set mode: advertise the payload under every configured mime.
    /// Requires keyboard focus (smithay denies otherwise) — the harness
    /// relies on focus-on-map. G-D5: with --primary the source is a
    /// zwp_primary_selection_source on the primary device.
    fn set_selection(&mut self, qh: &QueueHandle<Self>) {
        if self.mode != Mode::Set {
            return;
        }
        if self.primary {
            use smithay_client_toolkit::reexports::protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1;
            let Some(manager) = self.primary_manager.as_ref() else {
                return;
            };
            let Some(device) = self.primary_device.as_ref() else {
                return;
            };
            let source: zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1 =
                manager.create_source(qh, ());
            for m in &self.mimes {
                source.offer(m.clone());
            }
            device.set_selection(Some(&source), 0);
            self.primary_source = Some(source);
            self.set_done = true;
            crate::log_kv(&[
                ("ev", "clip_set_done".into()),
                ("selection", "primary".into()),
                ("mimes", self.mimes.join(",").into()),
            ]);
            return;
        }
        let Some(device) = self.device.clone() else {
            return;
        };
        let source = self.ddm.create_data_source(qh, ());
        for m in &self.mimes {
            source.offer(m.clone());
        }
        device.set_selection(Some(&source), 0);
        self.source = Some(source);
        self.set_done = true;
        crate::log_kv(&[
            ("ev", "clip_set_done".into()),
            ("mimes", self.mimes.join(",").into()),
        ]);
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ClipTester {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No pointer needed for clipboard testing; capabilities ignored.
    }
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for ClipTester {
    fn event(
        _: &mut Self,
        _: &wl_data_device_manager::WlDataDeviceManager,
        _: wl_data_device_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for ClipTester {
    fn event(
        state: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.offers.insert(id, OfferState::default());
            }
            wl_data_device::Event::Selection { id } => match id {
                Some(offer) => {
                    crate::log_kv(&[("ev", "clip_selection".into()), ("has_offer", true.into())]);
                    state.active_offer = Some(offer.clone());
                    state.maybe_receive(&offer);
                }
                None => {
                    crate::log_kv(&[("ev", "clip_selection".into()), ("has_offer", false.into())]);
                    crate::log_kv(&[("ev", "clip_cleared".into())]);
                    state.active_offer = None;
                }
            },
            _ => {}
        }
    }

    // wl_data_device.data_offer creates a wl_data_offer child object.
    wayland_client::event_created_child!(ClipTester, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
    ]);
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for ClipTester {
    fn event(
        state: &mut Self,
        offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            if let Some(o) = state.offers.get_mut(offer) {
                o.mimes.push(mime_type.clone());
            }
            crate::log_kv(&[
                ("ev", "clip_mime".into()),
                ("mime", mime_type.clone().into()),
            ]);
            state.maybe_receive(offer);
        }
    }
}

/// Read the transferred payload over a pipe and log it.
fn receive_and_log(offer: &wl_data_offer::WlDataOffer, mime: &str) {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        crate::log_kv(&[("ev", "clip_data_error".into()), ("why", "pipe".into())]);
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    offer.receive(mime.to_string(), unsafe {
        std::os::fd::BorrowedFd::borrow_raw(write_fd)
    });
    // The backend dup'd the fd into the message; close our copy.
    unsafe { libc::close(write_fd) };
    let mime_owned = mime.to_string();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(read_fd, chunk.as_mut_ptr() as _, chunk.len()) };
            if n <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n as usize]);
        }
        unsafe { libc::close(read_fd) };
        let text = String::from_utf8_lossy(&buf).to_string();
        crate::log_kv(&[
            ("ev", "clip_data".into()),
            ("mime", mime_owned.into()),
            ("len", buf.len().into()),
            ("payload", text.into()),
        ]);
    });
}

impl Dispatch<wl_data_source::WlDataSource, ()> for ClipTester {
    fn event(
        state: &mut Self,
        _: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Target { mime_type } => {
                crate::log_kv(&[("ev", "clip_target".into()), ("mime", mime_type.into())]);
            }
            wl_data_source::Event::Send { mime_type, fd } => {
                let payload = state.payload.clone();
                let len = payload.len();
                std::thread::spawn(move || {
                    use std::io::Write as _;
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(payload.as_bytes());
                    let _ = f.flush();
                });
                crate::log_kv(&[
                    ("ev", "clip_send".into()),
                    ("mime", mime_type.into()),
                    ("len", len.into()),
                ]);
            }
            wl_data_source::Event::Cancelled => {
                crate::log_kv(&[("ev", "clip_cancelled".into())]);
            }
            _ => {}
        }
    }
}

// ── window scaffolding (same shape as dnd_tester) ────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for ClipTester {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for ClipTester {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for ClipTester {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for ClipTester {
    fn event(
        state: &mut Self,
        s: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            s.ack_configure(serial);
            if !state.configured {
                state.configured = true;
                state.configured_at = Some(Instant::now());
                crate::log_kv(&[("ev", "clip_configured".into())]);
                state.draw(qh);
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for ClipTester {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            state.exit = true;
        }
    }
}

impl CompositorHandler for ClipTester {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
}

impl OutputHandler for ClipTester {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for ClipTester {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for ClipTester {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState,];
}

delegate_compositor!(ClipTester);
delegate_shm!(ClipTester);
delegate_output!(ClipTester);
delegate_registry!(ClipTester);

// ── G-D5: raw primary selection plumbing ─────────────────────────────
// Same shape as the wl_data_device flow above, on the
// zwp_primary_selection* protocol objects.

impl Dispatch<zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1, ()>
    for ClipTester
{
    fn event(
        _: &mut Self,
        _: &zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
        _: zwp_primary_selection_device_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1, ()> for ClipTester {
    // DataOffer creates a primary selection offer child object.
    wayland_client::event_created_child!(ClipTester, zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1, ()),
    ]);
    fn event(
        state: &mut Self,
        _: &zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_primary_selection_device_v1::Event as PSEvent;
        match event {
            PSEvent::DataOffer { offer } => {
                state.primary_offer = Some(offer);
                state.primary_offer_mimes.clear();
            }
            PSEvent::Selection { id } => match id {
                Some(offer) => {
                    crate::log_kv(&[("ev", "clip_selection".into()), ("has_offer", true.into())]);
                    state.primary_offer = Some(offer);
                    state.maybe_receive_primary();
                }
                None => {
                    crate::log_kv(&[("ev", "clip_selection".into()), ("has_offer", false.into())]);
                    crate::log_kv(&[("ev", "clip_cleared".into())]);
                    state.primary_offer = None;
                    state.primary_offer_mimes.clear();
                }
            },
            _ => {}
        }
    }
}

impl Dispatch<zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1, ()> for ClipTester {
    fn event(
        state: &mut Self,
        offer: &zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event {
            if state.primary_offer.as_ref() == Some(offer) {
                state.primary_offer_mimes.push(mime_type.clone());
            }
            crate::log_kv(&[("ev", "clip_mime".into()), ("mime", mime_type.into())]);
            state.maybe_receive_primary();
        }
    }
}

impl Dispatch<zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1, ()> for ClipTester {
    fn event(
        state: &mut Self,
        _: &zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_primary_selection_source_v1::Event as PSSourceEvent;
        match event {
            PSSourceEvent::Send { mime_type, fd } => {
                let payload = state.payload.clone();
                let len = payload.len();
                std::thread::spawn(move || {
                    use std::io::Write as _;
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(payload.as_bytes());
                    let _ = f.flush();
                });
                crate::log_kv(&[
                    ("ev", "clip_send".into()),
                    ("mime", mime_type.into()),
                    ("len", len.into()),
                ]);
            }
            PSSourceEvent::Cancelled => {
                crate::log_kv(&[("ev", "clip_cancelled".into())]);
            }
            _ => {}
        }
    }
}

impl ClipTester {
    /// G-D5: paste mode — receive the wanted mime from the active
    /// primary offer once both the offer and its mimes are known.
    fn maybe_receive_primary(&mut self) {
        if self.mode != Mode::Paste {
            return;
        }
        let Some(offer) = self.primary_offer.clone() else {
            return;
        };
        let Some(want) = self.mimes.first().cloned() else {
            return;
        };
        if !self.primary_offer_mimes.iter().any(|m| m == &want) {
            return;
        }
        // No once-latch: a selection replacement (e.g. the second click
        // of a double-click) cancels pending transfers per protocol —
        // the surviving offer must be received again.
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            crate::log_kv(&[("ev", "clip_data_error".into()), ("why", "pipe".into())]);
            return;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        offer.receive(want.clone(), unsafe {
            std::os::fd::BorrowedFd::borrow_raw(write_fd)
        });
        unsafe { libc::close(write_fd) };
        let mime_owned = want.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = unsafe { libc::read(read_fd, chunk.as_mut_ptr() as _, chunk.len()) };
                if n <= 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n as usize]);
            }
            unsafe { libc::close(read_fd) };
            let text = String::from_utf8_lossy(&buf).to_string();
            crate::log_kv(&[
                ("ev", "clip_data".into()),
                ("mime", mime_owned.into()),
                ("len", buf.len().into()),
                ("payload", text.into()),
            ]);
        });
    }
}
