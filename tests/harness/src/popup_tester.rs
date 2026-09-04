//! Raw-protocol popup lifecycle tester (`client-kit popups`).
//!
//! Creates a raw xdg_toplevel parent (no sctk Window: `get_xdg_popup`
//! needs the parent's own xdg_surface object, which sctk keeps private),
//! then runs popup cycles:
//!
//!   configure parent → positioner + popup surface + commit
//!   → popup configure → commit content → (popup_done from compositor)
//!   → destroy popup → recreate
//!
//! Every observable step is logged as JSONL; the runner asserts on
//! those lines — popup_created / popup_committed / popup_done /
//! popup_destroyed repeating per cycle.

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
    protocol::{wl_output, wl_surface, wl_registry},
    Connection, Dispatch, QueueHandle,
};
use smithay_client_toolkit::reexports::client::protocol::wl_shm::Format;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};

const POPUP_W: i32 = 240;
const POPUP_H: i32 = 140;

struct PopupCycle {
    surface: wl_surface::WlSurface,
    #[allow(dead_code)]
    xdg_surface: xdg_surface::XdgSurface,
    popup: Option<xdg_popup::XdgPopup>,
    pool: Option<SlotPool>,
    buffer: Option<Buffer>,
    configured: bool,
}

pub struct PopupTester {
    wanted_cycles: u32,
    /// Keep the LAST cycle's popup mapped (skip its destroy) so the
    /// harness can drag the parent while a popup is attached (J2).
    hold_last: bool,
    pub cycle: u32,
    #[allow(dead_code)]
    compositor: CompositorState,
    wm_base: xdg_wm_base::XdgWmBase,
    shm: Shm,
    registry_state: RegistryState,
    output_state: OutputState,

    parent_surface: wl_surface::WlSurface,
    parent_xdg_surface: xdg_surface::XdgSurface,
    #[allow(dead_code)]
    parent_toplevel: xdg_toplevel::XdgToplevel,
    parent_pool: Option<SlotPool>,
    parent_buffer: Option<Buffer>,
    parent_configured: bool,
    pw: u32,
    ph: u32,

    current: Option<PopupCycle>,
    committed_cycle: Option<u32>,
    committed_at: Option<std::time::Instant>,
    exit: bool,
}

pub fn run_popups(cycles: u32, duration_ms: u64) -> i32 {
    run_popups_opts(cycles, duration_ms, false)
}

pub fn run_popups_opts(cycles: u32, duration_ms: u64, hold_last: bool) -> i32 {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("connect to wayland: NoCompositor");
            return 1;
        }
    };
    let (globals, mut event_queue) = match registry_queue_init::<PopupTester>(&conn) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("registry init failed");
            return 1;
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

    let surface = compositor.create_surface(&qh);
    let parent_xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let parent_toplevel = parent_xdg_surface.get_toplevel(&qh, ());
    parent_toplevel.set_title("client-kit popups".into());
    parent_toplevel.set_app_id("client-kit-popups".into());
    surface.commit();

    let mut tester = PopupTester {
        wanted_cycles: cycles,
        hold_last,
        cycle: 0,
        compositor,
        wm_base,
        shm,
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        parent_surface: surface,
        parent_xdg_surface,
        parent_toplevel,
        parent_pool: None,
        parent_buffer: None,
        parent_configured: false,
        pw: 640,
        ph: 480,
        current: None,
        committed_cycle: None,
        committed_at: None,
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
        // Kill + recreate the popup 300ms after its content commit —
        // wall-clock driven so pacing differences between backends
        // (weston-headless vs X11) do not stall the cycle chain.
        if let Some(t) = tester.committed_at {
            let holding = tester.hold_last && tester.cycle >= tester.wanted_cycles;
            if t.elapsed() >= Duration::from_millis(300) && !holding {
                let qh = qh.clone();
                tester.kill_and_continue(&qh);
            }
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
    crate::log_kv(&[("ev", "exit".into()), ("cycles_done", tester.cycle.into())]);
    if tester.exit { 0 } else { 3 }
}

impl PopupTester {
    fn maybe_start_cycle(&mut self, qh: &QueueHandle<Self>) {
        if self.current.is_none() && self.cycle < self.wanted_cycles && self.parent_configured {
            self.cycle += 1;
            crate::log_kv(&[
                ("ev", "popup_cycle_start".into()),
                ("cycle", self.cycle.into()),
            ]);
            self.create_popup(qh);
        }
    }

    fn create_popup(&mut self, qh: &QueueHandle<Self>) {
        // Anchor in the parent's top-left region, growing down-right;
        // slide adjustment lets the compositor keep it on-screen.
        let pos: xdg_positioner::XdgPositioner = self.wm_base.create_positioner(qh, ());
        pos.set_size(POPUP_W, POPUP_H);
        pos.set_anchor_rect(0, 0, 20, 20);
        pos.set_anchor(xdg_positioner::Anchor::TopLeft);
        pos.set_gravity(xdg_positioner::Gravity::BottomRight);
        pos.set_constraint_adjustment(
            xdg_positioner::ConstraintAdjustment::SlideX
                | xdg_positioner::ConstraintAdjustment::SlideY,
        );

        let psurface = self.compositor.create_surface(qh);
        let pxdgs = self.wm_base.get_xdg_surface(&psurface, qh, ());
        let popup = pxdgs.get_popup(Some(&self.parent_xdg_surface), &pos, qh, ());
        psurface.commit();

        self.current = Some(PopupCycle {
            surface: psurface,
            xdg_surface: pxdgs,
            popup: Some(popup),
            pool: None,
            buffer: None,
            configured: false,
        });
        crate::log_kv(&[("ev", "popup_created".into()), ("cycle", self.cycle.into())]);
    }

    /// Ack the popup configure, then attach + commit popup content.
    fn ack_and_draw_popup(&mut self, _qh: &QueueHandle<Self>) {
        let Some(cyc) = &mut self.current else { return };
        if cyc.pool.is_none() {
            cyc.pool = SlotPool::new(
                POPUP_W as usize * POPUP_H as usize * 4,
                &self.shm,
            )
            .ok();
        }
        let Some(pool) = &mut cyc.pool else { return };
        let (buffer, canvas) = match pool.create_buffer(
            POPUP_W, POPUP_H, POPUP_W * 4, Format::Argb8888,
        ) {
            Ok(b) => b,
            Err(_) => return,
        };
        for chunk in canvas.chunks_exact_mut(4) {
            chunk[0] = 0x30;
            chunk[1] = 0xC0;
            chunk[2] = 0x60;
            chunk[3] = 0xFF;
        }
        buffer.attach_to(&cyc.surface).expect("popup attach");
        cyc.buffer = Some(buffer);
        cyc.surface.commit();
        crate::log_kv(&[("ev", "popup_committed".into()), ("cycle", self.cycle.into())]);
        self.committed_cycle = Some(self.cycle);
        self.committed_at = Some(Instant::now());
    }

    fn kill_and_continue(&mut self, qh: &QueueHandle<Self>) {
        if let Some(cyc) = self.current.take() {
            if let Some(p) = cyc.popup {
                let _ = p.destroy();
            }
            crate::log_kv(&[
                ("ev", "popup_destroyed".into()),
                ("cycle", self.cycle.into()),
            ]);
        }
        self.committed_cycle = None;
        self.committed_at = None;
        if self.cycle >= self.wanted_cycles {
            self.exit = true;
            return;
        }
        self.maybe_start_cycle(qh);
        if self.current.is_none() {
            // Restart the parent frame pump: the destroyed popup may have
            // had the last pending callback, leaving no frame to resume
            // from (observed in the nested-weston harness).
            self.draw_parent(qh);
        }
    }

    fn draw_parent(&mut self, qh: &QueueHandle<Self>) {
        let (w, h) = (self.pw, self.ph);
        if self.parent_pool.is_none() {
            self.parent_pool = SlotPool::new(2 * w as usize * h as usize, &self.shm).ok();
        }
        let Some(pool) = &mut self.parent_pool else { return };
        let (buffer, canvas) = match pool.create_buffer(
            w as i32, h as i32, w as i32 * 4, Format::Argb8888,
        ) {
            Ok(b) => b,
            Err(_) => return,
        };
        for chunk in canvas.chunks_exact_mut(4) {
            chunk[0] = 0xF0;
            chunk[1] = 0x20;
            chunk[2] = 0x40;
            chunk[3] = 0xFF;
        }
        buffer.attach_to(&self.parent_surface).expect("parent attach");
        self.parent_buffer = Some(buffer);
        // Frame callbacks drive the popup cycles (destroy after a couple
        // of presented frames).
        self.parent_surface.frame(qh, self.parent_surface.clone());
        self.parent_surface.commit();
        crate::log_kv(&[("ev", "parent_commit".into()), ("w", w.into()), ("h", h.into())]);
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for PopupTester {
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

impl Dispatch<wl_surface::WlSurface, ()> for PopupTester {
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

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for PopupTester {
    fn event(
        state: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
        let _ = state;
        let _ = qh;
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for PopupTester {
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
            if s == &state.parent_xdg_surface {
                if !state.parent_configured {
                    state.parent_configured = true;
                    crate::log_kv(&[("ev", "parent_configured".into())]);
                    state.draw_parent(qh);
                    state.maybe_start_cycle(qh);
                }
            } else {
                // popup xdg_surface configure
                if let Some(cyc) = &mut state.current {
                    if cyc.configured {
                        return;
                    }
                    cyc.configured = true;
                }
                crate::log_kv(&[("ev", "popup_configured".into()), ("cycle", state.cycle.into())]);
                state.ack_and_draw_popup(qh);
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for PopupTester {
    fn event(
        _: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            crate::log_kv(&[("ev", "parent_close".into())]);
        }
    }
}

impl Dispatch<xdg_popup::XdgPopup, ()> for PopupTester {
    fn event(
        state: &mut Self,
        _p: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            xdg_popup::Event::Configure { x, y, width, height } => {
                crate::log_kv(&[
                    ("ev", "popup_popup_configure".into()),
                    ("x", x.into()),
                    ("y", y.into()),
                    ("w", width.into()),
                    ("h", height.into()),
                ]);
            }
            xdg_popup::Event::PopupDone => {
                crate::log_kv(&[("ev", "popup_done".into()), ("cycle", state.cycle.into())]);
                let qh = _qh.clone();
                state.kill_and_continue(&qh);
            }
            xdg_popup::Event::Repositioned { token } => {
                let _ = token;
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_positioner::XdgPositioner, ()> for PopupTester {
    fn event(
        _: &mut Self,
        _: &xdg_positioner::XdgPositioner,
        _: xdg_positioner::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl CompositorHandler for PopupTester {
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
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        // Parent frames drive the cycle: after the popup commits, wait a
        // couple of frames (configures delivered, popup live) then the
        // Parent frames drive the parent commit chain; the popup cycle
        // kill happens on the wall-clock timer in the run loop.
        if surface != &self.parent_surface {
            return;
        }
        // Keep committing the parent so frame callbacks keep flowing.
        self.draw_parent(qh);
    }
}

impl OutputHandler for PopupTester {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for PopupTester {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for PopupTester {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState,];
}

delegate_compositor!(PopupTester);
delegate_shm!(PopupTester);
delegate_output!(PopupTester);
delegate_registry!(PopupTester);
