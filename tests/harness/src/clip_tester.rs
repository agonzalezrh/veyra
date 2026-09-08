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
//! NOTE: smithay denies set_selection from clients without keyboard
//! focus (device.rs SetSelection handler) — the harness must ensure
//! the setter window has keyboard focus before setting.

use std::collections::HashMap;
use std::time::{Duration, Instant};

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
    protocol::{
        wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_output,
        wl_registry, wl_seat, wl_surface,
    },
    Connection, Dispatch, QueueHandle,
};
use smithay_client_toolkit::reexports::client::protocol::wl_shm::Format;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
    xdg_surface, xdg_toplevel, xdg_wm_base,
};

const W: i32 = 240;
const H: i32 = 160;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode { Set, Paste }

#[derive(Default)]
struct OfferState {
    mimes: Vec<String>,
}

pub struct ClipTester {
    mode: Mode,
    mimes: Vec<String>,
    payload: String,
    shm: Shm,
    registry_state: RegistryState,
    output_state: OutputState,
    ddm: wl_data_device_manager::WlDataDeviceManager,

    surface: wl_surface::WlSurface,
    _xdg_surface: xdg_surface::XdgSurface,
    _toplevel: xdg_toplevel::XdgToplevel,
    pool: Option<SlotPool>,
    _buffer: Option<Buffer>,
    configured: bool,

    device: Option<wl_data_device::WlDataDevice>,
    source: Option<wl_data_source::WlDataSource>,
    offers: HashMap<wl_data_offer::WlDataOffer, OfferState>,
    active_offer: Option<wl_data_offer::WlDataOffer>,
    set_done: bool,
    configured_at: Option<Instant>,
    exit: bool,
}

pub fn run_clip(mode: Mode, mimes: Vec<String>, payload: String, duration_ms: u64) -> i32 {
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
    let ddm: wl_data_device_manager::WlDataDeviceManager =
        match globals.bind(&qh, 1..=3, ()) {
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

    let surface = compositor.create_surface(&qh);
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    let (title, app_id) = match mode {
        Mode::Set => ("client-kit clip-set", "clip-set"),
        Mode::Paste => ("client-kit clip-paste", "clip-paste"),
    };
    toplevel.set_title(title.into());
    toplevel.set_app_id(app_id.into());
    surface.commit();

    let device = ddm.get_data_device(&seat, &qh, ());

    let mut tester = ClipTester {
        mode,
        mimes,
        payload,
        shm,
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        ddm,
        surface,
        _xdg_surface: xdg_surface,
        _toplevel: toplevel,
        pool: None,
        _buffer: None,
        configured: false,
        device: Some(device),
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
            && tester.configured_at.map(|t| t.elapsed() >= Duration::from_millis(300)).unwrap_or(false)
        {
            let qh = qh.clone();
            tester.set_selection(&qh);
        }
        let _ = conn.flush();
        use std::os::fd::AsRawFd as _;
        let fd = conn.backend().poll_fd().as_raw_fd();
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
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
        let Some(want) = self.mimes.first() else { return };
        let Some(os) = self.offers.get(offer) else { return };
        if os.mimes.iter().any(|m| m == want) {
            receive_and_log(offer, want);
        }
    }

    /// Set mode: advertise the payload under every configured mime.
    /// Requires keyboard focus (smithay denies otherwise) — the harness
    /// relies on focus-on-map.
    fn set_selection(&mut self, qh: &QueueHandle<Self>) {
        if self.mode != Mode::Set {
            return;
        }
        let Some(device) = self.device.clone() else { return };
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
            wl_data_device::Event::Selection { id } => {
                match id {
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
                }
            }
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
            crate::log_kv(&[("ev", "clip_mime".into()), ("mime", mime_type.clone().into())]);
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
                crate::log_kv(&[
                    ("ev", "clip_target".into()),
                    ("mime", mime_type.into()),
                ]);
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

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
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
