//! Raw-protocol drag-and-drop tester (`client-kit dnd`, G-B2).
//!
//! sctk 0.20 has no data_device module, so wl_data_device /
//! wl_data_source / wl_data_offer are driven by hand on top of a raw
//! xdg_toplevel window (same scaffolding as `popups`).
//!
//! Roles:
//!   --role source  on the FIRST pointer button press: create a
//!                  wl_data_source, offer --mime, start_drag with the
//!                  press serial. Logs dnd_drag_started / dnd_target /
//!                  dnd_send / dnd_drop_performed / dnd_finished /
//!                  dnd_cancelled. Writes --payload to the send fd.
//!   --role dest    logs dnd_offer / dnd_mime / dnd_enter / dnd_motion /
//!                  dnd_leave / dnd_drop; on enter accepts the first
//!                  matching mime + negotiates Copy actions (v3); on
//!                  drop: receive → read fd → dnd_data → offer.finish.
//!
//! The runner asserts on these JSONL lines — never on timing.

use std::collections::HashMap;
use std::os::fd::BorrowedFd;
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
        wl_pointer, wl_registry, wl_seat, wl_surface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use smithay_client_toolkit::reexports::client::protocol::wl_shm::Format;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
    xdg_surface, xdg_toplevel, xdg_wm_base,
};

const W: i32 = 320;
const H: i32 = 220;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role { Source, Dest }

#[derive(Default)]
struct OfferState {
    mimes: Vec<String>,
    entered: bool,
    #[allow(dead_code)]
    source_actions: Option<u32>,
}

pub struct DndTester {
    role: Role,
    mime: String,
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

    pointer: Option<wl_pointer::WlPointer>,
    /// Serial of the latest wl_pointer.button(press) — the implicit
    /// grab serial the source echoes into start_drag. Every press
    /// begins a fresh drag (the harness performs several drags per
    /// client lifetime).
    press_serial: Option<u32>,

    device: Option<wl_data_device::WlDataDevice>,
    offers: HashMap<wl_data_offer::WlDataOffer, OfferState>,
    /// Offer carried by the most recent dnd enter (the active target).
    active_offer: Option<wl_data_offer::WlDataOffer>,
    /// Serial of the most recent dnd enter (for wl_data_offer.accept).
    enter_serial: u32,
    exit: bool,
}

pub fn run_dnd(role: Role, mime: String, payload: String, duration_ms: u64) -> i32 {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("connect to wayland: NoCompositor");
            return 1;
        }
    };
    let (globals, mut event_queue) = match registry_queue_init::<DndTester>(&conn) {
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
        Err(_) => {
            eprintln!("xdg_wm_base not available");
            return 2;
        }
    };
    let ddm: wl_data_device_manager::WlDataDeviceManager =
        match globals.bind(&qh, 1..=3, ()) {
            Ok(d) => d,
            Err(_) => {
                eprintln!("wl_data_device_manager not available");
                return 2;
            }
        };
    let seat: wl_seat::WlSeat = match globals.bind(&qh, 1..=7, ()) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("wl_seat not available");
            return 2;
        }
    };

    let surface = compositor.create_surface(&qh);
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    let (title, app_id) = match role {
        Role::Source => ("client-kit dnd-source", "dnd-source"),
        Role::Dest => ("client-kit dnd-dest", "dnd-dest"),
    };
    toplevel.set_title(title.into());
    toplevel.set_app_id(app_id.into());
    surface.commit();

    let device = ddm.get_data_device(&seat, &qh, ());
    seat.get_pointer(&qh, ());

    let mut tester = DndTester {
        role,
        mime,
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
        pointer: None,
        press_serial: None,
        device: Some(device),
        offers: HashMap::new(),
        active_offer: None,
        enter_serial: 0,
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
    crate::log_kv(&[("ev", "exit".into()), ("role", format!("{role:?}").into())]);
    0
}

impl DndTester {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.pool.is_none() {
            self.pool = SlotPool::new(2 * W as usize * H as usize, &self.shm).ok();
        }
        let Some(pool) = &mut self.pool else { return };
        let (buffer, canvas) = match pool.create_buffer(W, H, W * 4, Format::Argb8888) {
            Ok(b) => b,
            Err(_) => return,
        };
        // Source: blue; dest: green.
        let color: [u8; 4] = match self.role {
            Role::Source => [0xC0, 0x10, 0x10, 0xFF],
            Role::Dest => [0x10, 0xC0, 0x10, 0xFF],
        };
        for chunk in canvas.as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&color);
        }
        buffer.attach_to(&self.surface).expect("dnd attach");
        self._buffer = Some(buffer);
        self.surface.commit();
        crate::log_kv(&[
            ("ev", "dnd_window_drawn".into()),
            ("role", format!("{:?}", self.role).into()),
        ]);
    }

    /// Accept the wanted mime + negotiate Copy actions on the active
    /// offer. Called from BOTH the enter and mime handlers: smithay
    /// sends offer.mime events BEFORE device.enter, so either handler
    /// may be the first to see both preconditions satisfied.
    fn maybe_negotiate(&self, offer: &wl_data_offer::WlDataOffer) {
        if self.active_offer.as_ref() != Some(offer) {
            return;
        }
        let Some(os) = self.offers.get(offer) else { return };
        if !os.mimes.iter().any(|m| m == &self.mime) {
            return;
        }
        offer.accept(self.enter_serial, Some(self.mime.clone()));
        if offer.version() >= 3 {
            offer.set_actions(
                wl_data_device_manager::DndAction::Copy,
                wl_data_device_manager::DndAction::Copy,
            );
        }
    }

    /// Source role: begin the drag in response to the button press.
    fn start_drag(&mut self, qh: &QueueHandle<Self>) {
        if self.role != Role::Source {
            return;
        }
        let Some(serial) = self.press_serial else { return };
        let Some(device) = self.device.clone() else { return };
        let source = self.ddm.create_data_source(qh, ());
        source.offer(self.mime.clone());
        if source.version() >= 3 {
            // Source-side action mask for v3 negotiation.
            source.set_actions(wl_data_device_manager::DndAction::Copy);
        }
        device.start_drag(Some(&source), &self.surface, None, serial);
        crate::log_kv(&[
            ("ev", "dnd_drag_started".into()),
            ("serial", serial.into()),
            ("mime", self.mime.clone().into()),
        ]);
    }
}

// ── seat / pointer ───────────────────────────────────────────────────

impl Dispatch<wl_seat::WlSeat, ()> for DndTester {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if matches!(capabilities, wayland_client::WEnum::Value(c) if c.contains(wl_seat::Capability::Pointer))
                && state.pointer.is_none()
            {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for DndTester {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { serial, button, state: btn_state, .. } = event {
            if btn_state == wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed)
                && button == 0x110
            {
                // Dedupe identical-serial presses: the same wl_pointer
                // press event has been observed to arrive twice (see
                // BUG_LIST P2 #15); one physical press = one drag.
                if state.press_serial == Some(serial) {
                    return;
                }
                state.press_serial = Some(serial);
                crate::log_kv(&[("ev", "dnd_press".into()), ("serial", serial.into())]);
                state.start_drag(qh);
            }
        }
    }
}

// ── data device ──────────────────────────────────────────────────────

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for DndTester {
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

impl Dispatch<wl_data_device::WlDataDevice, ()> for DndTester {
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
            }            wl_data_device::Event::Enter { serial, surface: _, x, y, id } => {
                let Some(offer) = id else { return };
                crate::log_kv(&[
                    ("ev", "dnd_enter".into()),
                    ("serial", serial.into()),
                    ("x", x.into()),
                    ("y", y.into()),
                ]);
                if let Some(o) = state.offers.get_mut(&offer) {
                    o.entered = true;
                }
                state.enter_serial = serial;
                state.active_offer = Some(offer.clone());
                state.maybe_negotiate(&offer);
            }
            wl_data_device::Event::Leave => {
                crate::log_kv(&[("ev", "dnd_leave".into())]);
                if let Some(o) = state.active_offer.take() {
                    if let Some(os) = state.offers.get_mut(&o) {
                        os.entered = false;
                    }
                }
            }
            wl_data_device::Event::Motion { time, x, y } => {
                crate::log_kv(&[
                    ("ev", "dnd_motion".into()),
                    ("time", time.into()),
                    ("x", x.into()),
                    ("y", y.into()),
                ]);
            }
            wl_data_device::Event::Drop => {
                crate::log_kv(&[("ev", "dnd_drop".into())]);
                if let Some(offer) = state.active_offer.clone() {
                    let have_mime = state
                        .offers
                        .get(&offer)
                        .map(|o| o.mimes.contains(&state.mime))
                        .unwrap_or(false);
                    if have_mime {
                        receive_and_log(&offer, &state.mime);
                        if offer.version() >= 3 {
                            offer.finish();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // wl_data_device.data_offer creates a wl_data_offer child object;
    // its userdata is () (offers are tracked in the state HashMap).
    wayland_client::event_created_child!(DndTester, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
    ]);
}

/// Read the transferred payload over a pipe and log it. The source
/// writes concurrently; the reader thread only touches the log.
/// writes concurrently; the reader thread only touches the log.
fn receive_and_log(offer: &wl_data_offer::WlDataOffer, mime: &str) {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        crate::log_kv(&[("ev", "dnd_data_error".into()), ("why", "pipe".into())]);
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // The backend dups the fd into the message; dropping our OwnedFd
    // afterwards keeps the pipe accounting clean.
    offer.receive(mime.to_string(), unsafe {
        BorrowedFd::borrow_raw(write_fd)
    });
    // The backend dup'd the fd into the message; close our copy so the
    // read end sees EOF once the source closes its dup.
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
            ("ev", "dnd_data".into()),
            ("mime", mime_owned.into()),
            ("len", buf.len().into()),
            ("payload", text.into()),
        ]);
    });
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for DndTester {
    fn event(
        state: &mut Self,
        offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_offer::Event::Offer { mime_type } => {
                if let Some(o) = state.offers.get_mut(offer) {
                    o.mimes.push(mime_type.clone());
                }
                crate::log_kv(&[("ev", "dnd_mime".into()), ("mime", mime_type.into())]);
                state.maybe_negotiate(offer);
            }
            wl_data_offer::Event::SourceActions { source_actions } => {
                if let Some(o) = state.offers.get_mut(offer) {
                    o.source_actions = Some(u32::from(source_actions));
                }
            }
            wl_data_offer::Event::Action { dnd_action } => {
                crate::log_kv(&[
                    ("ev", "dnd_action".into()),
                    ("action", format!("{dnd_action:?}").into()),
                ]);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for DndTester {
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
                    ("ev", "dnd_target".into()),
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
                    ("ev", "dnd_send".into()),
                    ("mime", mime_type.into()),
                    ("len", len.into()),
                ]);
            }
            wl_data_source::Event::Cancelled => {
                crate::log_kv(&[("ev", "dnd_cancelled".into())]);
            }
            wl_data_source::Event::DndDropPerformed => {
                crate::log_kv(&[("ev", "dnd_drop_performed".into())]);
            }
            wl_data_source::Event::DndFinished => {
                crate::log_kv(&[("ev", "dnd_finished".into())]);
            }
            _ => {}
        }
    }
}

// ── window scaffolding (same shape as popups) ────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for DndTester {
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

impl Dispatch<wl_surface::WlSurface, ()> for DndTester {
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

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for DndTester {
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

impl Dispatch<xdg_surface::XdgSurface, ()> for DndTester {
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
                crate::log_kv(&[("ev", "dnd_configured".into())]);
            }
            state.draw(qh);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for DndTester {
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

impl CompositorHandler for DndTester {
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

impl OutputHandler for DndTester {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for DndTester {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for DndTester {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState,];
}

delegate_compositor!(DndTester);
delegate_shm!(DndTester);
delegate_output!(DndTester);
delegate_registry!(DndTester);
