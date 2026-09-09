//! Scriptable Wayland test clients for the Veyra headless harness.
//!
//! All clients log protocol observations as JSON lines on stdout:
//! configure/ACK/commit cycles, frame callbacks, keyboard events
//! (with keysyms), pointer events, and close requests. The runner
//! asserts on these logs — never on screenshots or timing sleeps.
//!
//! Subcommands: probe, resizer, keyboard, pointer, quitter. The probe/
//! resizer clients also support client-requested maximize sequencing via
//! --maximize-after / --unmaximize-after (I4),
//! --minimize-after (I5), --fullscreen-after / --unfullscreen-after (I7).

use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::csd_frame::WindowState,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{
        wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_subcompositor, wl_subsurface,
        wl_surface,
    },
    Connection, QueueHandle,
};
use xkeysym::Keysym;

// ── JSON event log ───────────────────────────────────────────────────

fn log(ev: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", ev);
    let _ = out.flush();
}

mod clip_tester;
mod dnd_tester;
mod popup_tester;
use clip_tester::{run_clip, Mode as ClipMode};
use dnd_tester::{run_dnd, Role};
use popup_tester::run_popups_opts;

fn opt_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn log_kv(pairs: &[(&str, serde_json::Value)]) {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    log(serde_json::Value::Object(map));
}

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Policy {
    /// Commit buffers matching the configured size (well-behaved client).
    Match,
    /// Ignore configured sizes and always commit a fixed size
    /// (tests the "client decides geometry" ownership rule).
    Ignore,
}

#[derive(Clone)]
struct Opts {
    cmd: String,
    duration_ms: u64,
    app_id: String,
    exit_after_commits: Option<u32>,
    policy: Policy,
    fixed_size: (u32, u32),
    min_size: Option<(u32, u32)>,
    max_size: Option<(u32, u32)>,
    expect: Option<String>,
    /// Probe only: after this many commits, switch to committing this size
    /// (client-driven geometry change — tests compositor adoption).
    resize_to: Option<(u32, u32)>,
    after_commits: u32,
    /// Maximize test: after this many commits, send xdg_toplevel
    /// set_maximized (client-requested maximize).
    maximize_after: Option<u32>,
    /// Maximize test: after this many commits, send unset_maximized.
    unmaximize_after: Option<u32>,
    /// Minimize test: after this many commits, send xdg_toplevel
    /// set_minimized (client-requested minimize, I5).
    minimize_after: Option<u32>,
    /// Fullscreen test (I7): after this many commits, send
    /// xdg_toplevel.set_fullscreen (client-requested).
    fullscreen_after: Option<u32>,
    /// Fullscreen test (I7): after this many commits, send
    /// xdg_toplevel.unset_fullscreen.
    unfullscreen_after: Option<u32>,
    /// G-C3: wl_surface.set_buffer_scale — buffers are committed at
    /// scale× dimensions while the logical size stays fixed. The
    /// compositor must adopt the logical size (buffer / scale).
    scale: i32,
    /// #11: after the first commit, create a 100x60 wl_subsurface at
    /// offset (50, 50) with a distinct color — verifies subsurface
    /// mapping on the compositor side.
    subsurface: bool,
}

fn parse_size(s: &str) -> (u32, u32) {
    let (w, h) = s.split_once('x').expect("size must be WxH");
    (w.parse().expect("width"), h.parse().expect("height"))
}

fn parse_args() -> Opts {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| {
        eprintln!("usage: client-kit <probe|resizer|keyboard|pointer|quitter> [options]");
        std::process::exit(2);
    });
    let mut opts = Opts {
        cmd: cmd.clone(),
        duration_ms: 4000,
        app_id: format!("client-kit-{}", cmd),
        exit_after_commits: None,
        policy: Policy::Match,
        fixed_size: (800, 600),
        min_size: None,
        max_size: None,
        expect: None,
        resize_to: None,
        after_commits: 0,
        maximize_after: None,
        unmaximize_after: None,
        minimize_after: None,
        fullscreen_after: None,
        unfullscreen_after: None,
        scale: 1,
        subsurface: false,
    };
    let mut i = 1;
    while i < args.len() {
        let next = |i: &mut usize| {
            *i += 1;
            args.get(*i).cloned().unwrap_or_default()
        };
        match args[i].as_str() {
            "--duration" => opts.duration_ms = next(&mut i).parse().unwrap_or(4000),
            "--app-id" => opts.app_id = next(&mut i),
            "--exit-after-commits" => opts.exit_after_commits = next(&mut i).parse().ok(),
            "--policy" => {
                opts.policy = if next(&mut i) == "ignore" {
                    Policy::Ignore
                } else {
                    Policy::Match
                };
            }
            "--fixed" => opts.fixed_size = parse_size(&next(&mut i)),
            "--min" => opts.min_size = Some(parse_size(&next(&mut i))),
            "--max" => opts.max_size = Some(parse_size(&next(&mut i))),
            "--expect" => opts.expect = Some(next(&mut i)),
            "--resize-to" => opts.resize_to = Some(parse_size(&next(&mut i))),
            "--after-commits" => opts.after_commits = next(&mut i).parse().unwrap_or(0),
            "--maximize-after" => opts.maximize_after = next(&mut i).parse().ok(),
            "--unmaximize-after" => opts.unmaximize_after = next(&mut i).parse().ok(),
            "--minimize-after" => opts.minimize_after = next(&mut i).parse().ok(),
            "--fullscreen-after" => opts.fullscreen_after = next(&mut i).parse().ok(),
            "--unfullscreen-after" => opts.unfullscreen_after = next(&mut i).parse().ok(),
            "--scale" => opts.scale = next(&mut i).parse().unwrap_or(1),
            "--subsurface" => opts.subsurface = true,
            other => {
                eprintln!("unknown option: {}", other);
                std::process::exit(2);
            }
        }
        i += 1;
    }
    opts
}

// ── Client state ─────────────────────────────────────────────────────

struct TestClient {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,

    opts: Opts,
    window: Window,

    pool: Option<SlotPool>,
    pool_size: usize,
    buffer: Option<Buffer>,
    buffer_size: (u32, u32),
    width: u32,
    height: u32,

    first_configure: bool,
    configures: u32,
    commits: u32,
    /// #11: the client's own subsurface (created once with --subsurface).
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    sub_surface: Option<wl_surface::WlSurface>,
    subsurface: Option<(wl_subsurface::WlSubsurface, wl_surface::WlSurface)>,
    sub_pool: Option<SlotPool>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    got_keys: String,
    exit: bool,
}

impl TestClient {
    fn choose_size(&self, configure: &WindowConfigure) -> (u32, u32) {
        match self.opts.policy {
            Policy::Match => (
                configure.new_size.0.map(|v| v.get()).unwrap_or(640),
                configure.new_size.1.map(|v| v.get()).unwrap_or(480),
            ),
            Policy::Ignore => self.opts.fixed_size,
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        self.commits += 1;
        let mut width = self.width;
        let mut height = self.height;
        if let (Some(target), true) = (self.opts.resize_to, self.commits > self.opts.after_commits)
        {
            width = target.0;
            height = target.1;
        }
        if width == 0 || height == 0 {
            return;
        }
        // G-C3: with --scale N, buffers carry N× pixels while the
        // logical size stays width×height (wl_surface.set_buffer_scale).
        let scale = self.opts.scale.max(1);
        let bw = width * scale as u32;
        let bh = height * scale as u32;
        let stride = (bw as i32) * 4;
        let need = (bw * bh * 4) as usize;
        if self.pool_size < need {
            self.pool = Some(SlotPool::new(need, &self.shm).expect("create pool"));
            self.pool_size = need;
            self.buffer = None;
        }
        let pool = self.pool.as_mut().expect("pool");
        let buffer = self.buffer.get_or_insert_with(|| {
            pool.create_buffer(bw as i32, bh as i32, stride, wl_shm::Format::Argb8888)
                .expect("create buffer")
                .0
        });
        let stale = self.buffer_size != (bw, bh);
        let canvas = if stale {
            let (second, canvas) = pool
                .create_buffer(bw as i32, bh as i32, stride, wl_shm::Format::Argb8888)
                .expect("create replacement buffer");
            *buffer = second;
            canvas
        } else {
            match pool.canvas(buffer) {
                Some(c) => c,
                None => {
                    let (second, canvas) = pool
                        .create_buffer(bw as i32, bh as i32, stride, wl_shm::Format::Argb8888)
                        .expect("create double-buffer");
                    *buffer = second;
                    canvas
                }
            }
        };

        // Solid color with a per-commit count encoded in green for debuggability.
        let green = canvas.len() as u8;
        for chunk in canvas.as_chunks_mut::<4>().0 {
            chunk[0] = 0x30;
            chunk[1] = green;
            chunk[2] = 0x80;
            chunk[3] = 0xFF;
        }

        if scale > 1 {
            self.window.wl_surface().set_buffer_scale(scale);
        }
        self.window
            .wl_surface()
            .damage_buffer(0, 0, bw as i32, bh as i32);
        self.window
            .wl_surface()
            .frame(qh, self.window.wl_surface().clone());
        buffer
            .attach_to(self.window.wl_surface())
            .expect("buffer attach");
        self.buffer_size = (bw, bh);
        self.window.commit();
        log_kv(&[
            ("ev", "commit".into()),
            ("w", width.into()),
            ("h", height.into()),
            ("bw", bw.into()),
            ("bh", bh.into()),
            ("scale", (scale as u32).into()),
        ]);
        // #11: after the first commit, create a 100x60 subsurface at
        // offset (50, 50) with a distinct color. Synchronized by
        // default: it applies with this parent commit per protocol.
        if self.opts.subsurface && self.subsurface.is_none() {
            if let (Some(subcomp), Some(sub_surface)) =
                (self.subcompositor.as_ref(), self.sub_surface.clone())
            {
                let sub = subcomp.get_subsurface(&sub_surface, self.window.wl_surface(), qh, ());
                sub.set_position(50, 50);
                let pool = self.sub_pool.get_or_insert_with(|| {
                    SlotPool::new(100 * 60 * 4, &self.shm).expect("sub pool")
                });
                let (sbuf, scanvas) = pool
                    .create_buffer(100, 60, 100 * 4, wl_shm::Format::Argb8888)
                    .expect("sub buffer");
                for chunk in scanvas.as_chunks_mut::<4>().0 {
                    chunk[0] = 0xE0;
                    chunk[1] = 0x60;
                    chunk[2] = 0x10;
                    chunk[3] = 0xFF;
                }
                sbuf.attach_to(&sub_surface).expect("sub attach");
                sub_surface.damage_buffer(0, 0, 100, 60);
                sub_surface.commit();
                log_kv(&[
                    ("ev", "subsurface_created".into()),
                    ("w", 100.into()),
                    ("h", 60.into()),
                ]);
                self.subsurface = Some((sub, sub_surface));
            }
        }
        // Client-requested maximize transitions (I4). Requests are sent
        // right after a commit; the compositor answers with a configure
        // carrying the Maximized state bit (and a size for well-behaved
        // compositors).
        if self.opts.maximize_after == Some(self.commits) {
            log_kv(&[("ev", "request_maximize".into())]);
            self.window.set_maximized();
        }
        if self.opts.unmaximize_after == Some(self.commits) {
            log_kv(&[("ev", "request_unmaximize".into())]);
            self.window.unset_maximized();
        }
        // Client-requested minimize (I5). No protocol answer is expected:
        // xdg-shell has no Minimized state bit; the compositor simply
        // hides the visual. The client keeps committing (log lines prove
        // liveness) until the run duration expires.
        if self.opts.minimize_after == Some(self.commits) {
            log_kv(&[("ev", "request_minimize".into())]);
            self.window.set_minimized();
        }
        // Client-requested fullscreen transitions (I7). The compositor
        // answers with a configure carrying the Fullscreen state bit
        // (plus a size for well-behaved compositors).
        if self.opts.fullscreen_after == Some(self.commits) {
            log_kv(&[("ev", "request_fullscreen".into())]);
            self.window.set_fullscreen(None);
        }
        if self.opts.unfullscreen_after == Some(self.commits) {
            log_kv(&[("ev", "request_unfullscreen".into())]);
            self.window.unset_fullscreen();
        }
        if let Some(n) = self.opts.exit_after_commits {
            if self.commits >= n {
                self.exit = true;
            }
        }
    }
}

// ── Handlers ─────────────────────────────────────────────────────────

impl CompositorHandler for TestClient {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        log_kv(&[("ev", "frame".into())]);
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for TestClient {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.log_output(output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // G-D1: log mode + scale so harness tests can assert that
        // output changes propagate to clients.
        self.log_output(output);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl TestClient {
    fn log_output(&mut self, output: wl_output::WlOutput) {
        if let Some(info) = self.output_state.info(&output) {
            let mode = info
                .modes
                .iter()
                .find(|m| m.current)
                .map(|m| m.dimensions)
                .unwrap_or((0, 0));
            log_kv(&[
                ("ev", "outmode".into()),
                ("w", (mode.0).into()),
                ("h", (mode.1).into()),
                ("scale", (info.scale_factor).into()),
            ]);
        }
    }
}

impl WindowHandler for TestClient {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        log_kv(&[("ev", "close".into())]);
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        serial: u32,
    ) {
        let first = self.first_configure;
        self.first_configure = false;
        self.configures += 1;
        self.buffer = None;
        let (w, h) = self.choose_size(&configure);
        self.width = w;
        self.height = h;
        log_kv(&[
            ("ev", "config".into()),
            ("serial", serial.into()),
            ("w", configure.new_size.0.map(|v| v.get() as u64).into()),
            ("h", configure.new_size.1.map(|v| v.get() as u64).into()),
            (
                "resizing",
                configure.state.contains(WindowState::RESIZING).into(),
            ),
            (
                "maximized",
                configure.state.contains(WindowState::MAXIMIZED).into(),
            ),
            (
                "fullscreen",
                configure.state.contains(WindowState::FULLSCREEN).into(),
            ),
            (
                "activated",
                configure.state.contains(WindowState::ACTIVATED).into(),
            ),
            ("first", first.into()),
        ]);
        self.draw(qh);
    }
}

impl SeatHandler for TestClient {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("keyboard capability");
            self.keyboard = Some(keyboard);
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let pointer = self
                .seat_state
                .get_pointer(qh, &seat)
                .expect("pointer capability");
            self.pointer = Some(pointer);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_some() {
            self.keyboard.take().unwrap().release();
        }
        if capability == Capability::Pointer && self.pointer.is_some() {
            self.pointer.take().unwrap().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for TestClient {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        if self.window.wl_surface() == surface {
            log_kv(&[("ev", "kb_enter".into())]);
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.window.wl_surface() == surface {
            log_kv(&[("ev", "kb_leave".into())]);
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(ch) = &event.utf8 {
            self.got_keys.push_str(ch);
        }
        let sym_name = event.keysym.name().map(|s| s.to_string());
        log_kv(&[
            ("ev", "key".into()),
            ("code", (event.raw_code).into()),
            ("sym", sym_name.into()),
            ("char", event.utf8.clone().into()),
            ("pressed", true.into()),
        ]);
        if let Some(expect) = &self.opts.expect {
            if self.got_keys.contains(expect.as_str()) {
                log_kv(&[
                    ("ev", "expect_matched".into()),
                    ("got", self.got_keys.clone().into()),
                ]);
                self.exit = true;
            }
        }
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        log_kv(&[
            ("ev", "key_repeat".into()),
            ("code", (event.raw_code).into()),
            ("sym", event.keysym.name().map(|s| s.to_string()).into()),
        ]);
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        log_kv(&[
            ("ev", "key".into()),
            ("code", (event.raw_code).into()),
            ("sym", event.keysym.name().map(|s| s.to_string()).into()),
            ("char", serde_json::Value::Null),
            ("pressed", false.into()),
        ]);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        log_kv(&[
            ("ev", "mods".into()),
            ("ctrl", modifiers.ctrl.into()),
            ("shift", modifiers.shift.into()),
            ("alt", modifiers.alt.into()),
            ("logo", modifiers.logo.into()),
        ]);
    }
}

impl PointerHandler for TestClient {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    log_kv(&[
                        ("ev", "ptr_enter".into()),
                        ("x", event.position.0.into()),
                        ("y", event.position.1.into()),
                    ]);
                }
                PointerEventKind::Leave { .. } => {
                    log_kv(&[("ev", "ptr_leave".into())]);
                }
                PointerEventKind::Motion { .. } => {
                    log_kv(&[
                        ("ev", "motion".into()),
                        ("x", event.position.0.into()),
                        ("y", event.position.1.into()),
                    ]);
                }
                PointerEventKind::Press { button, serial, .. } => {
                    log_kv(&[
                        ("ev", "button".into()),
                        ("button", (button).into()),
                        ("pressed", true.into()),
                        ("serial", (serial).into()),
                    ]);
                }
                PointerEventKind::Release { button, serial, .. } => {
                    log_kv(&[
                        ("ev", "button".into()),
                        ("button", (button).into()),
                        ("pressed", false.into()),
                        ("serial", (serial).into()),
                    ]);
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    log_kv(&[
                        ("ev", "axis".into()),
                        ("v", vertical.absolute.into()),
                        ("h", horizontal.absolute.into()),
                    ]);
                }
            }
        }
    }
}

impl ShmHandler for TestClient {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(TestClient);
delegate_output!(TestClient);
delegate_shm!(TestClient);
delegate_seat!(TestClient);
delegate_keyboard!(TestClient);
delegate_pointer!(TestClient);
delegate_xdg_shell!(TestClient);
delegate_xdg_window!(TestClient);
delegate_registry!(TestClient);

// #11: wl_subcompositor / wl_subsurface carry no client-side events.
impl wayland_client::Dispatch<wl_subcompositor::WlSubcompositor, ()> for TestClient {
    fn event(
        _: &mut Self,
        _: &wl_subcompositor::WlSubcompositor,
        _: wl_subcompositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<wl_subsurface::WlSubsurface, ()> for TestClient {
    fn event(
        _: &mut Self,
        _: &wl_subsurface::WlSubsurface,
        _: wl_subsurface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl ProvidesRegistryState for TestClient {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState,];
}

// ── Event loop ───────────────────────────────────────────────────────

fn run_until(
    conn: &Connection,
    eq: &mut wayland_client::EventQueue<TestClient>,
    state: &mut TestClient,
    deadline: Instant,
) -> i32 {
    loop {
        if state.exit {
            return 0;
        }
        if Instant::now() >= deadline {
            return 3; // timed out
        }
        let _ = conn.flush();
        let fd = conn.backend().poll_fd().as_raw_fd();
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 50) };
        if ret > 0 {
            if let Some(guard) = conn.prepare_read() {
                // EAGAIN here is benign (data already consumed by a flush);
                // real connection loss surfaces via dispatch_pending.
                let _ = guard.read();
            }
            // Socket hangup = the compositor closed the connection
            // (shutdown while clients are still connected). Drain any
            // events still in flight, then treat it as a clean
            // disconnect — this mirrors what real Wayland clients do
            // when the compositor dies.
            if pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                if let Err(e) = eq.dispatch_pending(state) {
                    eprintln!("dispatch error after hangup: {}", e);
                }
                return 0;
            }
        }
        if let Err(e) = eq.dispatch_pending(state) {
            // Compositor died / connection lost — treat as clean disconnect.
            eprintln!("dispatch error: {}", e);
            return 0;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_default();
    // Raw popup lifecycle tester has its own connection flow.
    if cmd == "popups" {
        let cycles = opt_value(&args, "--cycles")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let duration = opt_value(&args, "--duration")
            .and_then(|v| v.parse().ok())
            .unwrap_or(8000);
        let hold = args.iter().any(|a| a == "--hold");
        let code = run_popups_opts(cycles, duration, hold);
        std::process::exit(code);
    }
    // Raw drag-and-drop tester (G-B2) has its own connection flow.
    if cmd == "dnd" {
        let role = match opt_value(&args, "--role").as_deref() {
            Some("source") => Role::Source,
            Some("dest") => Role::Dest,
            _ => {
                eprintln!("dnd: --role source|dest required");
                std::process::exit(1);
            }
        };
        let mime = opt_value(&args, "--mime").unwrap_or_else(|| "text/plain".into());
        let payload = opt_value(&args, "--payload").unwrap_or_else(|| "veyra-dnd".into());
        let duration = opt_value(&args, "--duration")
            .and_then(|v| v.parse().ok())
            .unwrap_or(15000);
        let code = run_dnd(role, mime, payload, duration);
        std::process::exit(code);
    }
    // Raw clipboard tester (G-B1) has its own connection flow.
    if cmd == "clip" {
        let mode = match opt_value(&args, "--mode").as_deref() {
            Some("set") => ClipMode::Set,
            Some("paste") => ClipMode::Paste,
            _ => {
                eprintln!("clip: --mode set|paste required");
                std::process::exit(1);
            }
        };
        let mimes: Vec<String> = opt_value(&args, "--mimes")
            .unwrap_or_else(|| "text/plain".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let payload = opt_value(&args, "--payload").unwrap_or_else(|| "veyra-clip".into());
        let duration = opt_value(&args, "--duration")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10000);
        let primary = args.iter().any(|a| a == "--primary");
        let code = run_clip(mode, mimes, payload, duration, primary);
        std::process::exit(code);
    }
    let opts = parse_args();
    let conn = Connection::connect_to_env().expect("connect to wayland");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("registry init");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg shell");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title(format!("client-kit {}", opts.cmd));
    window.set_app_id(opts.app_id.clone());
    if let Some(min) = opts.min_size {
        window.set_min_size(Some(min));
    }
    if let Some(max) = opts.max_size {
        window.set_max_size(Some(max));
    }
    window.commit();

    // #11: bind the subcompositor and create the sub surface up front
    // when --subsurface is requested.
    let (subcompositor_opt, sub_surface) = if opts.subsurface {
        let sc: wl_subcompositor::WlSubcompositor =
            globals.bind(&qh, 1..=1, ()).expect("bind wl_subcompositor");
        let sub_surface = compositor.create_surface(&qh);
        (Some(sc), Some(sub_surface))
    } else {
        (None, None)
    };

    let mut state = TestClient {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        opts: opts.clone(),
        window,
        pool: None,
        pool_size: 0,
        buffer: None,
        buffer_size: (0, 0),
        width: 640,
        height: 480,
        first_configure: true,
        configures: 0,
        commits: 0,
        subcompositor: subcompositor_opt,
        sub_surface,
        subsurface: None,
        sub_pool: None,
        keyboard: None,
        pointer: None,
        got_keys: String::new(),
        exit: false,
    };

    let deadline = Instant::now() + Duration::from_millis(opts.duration_ms);
    // Initial roundtrip: process the initial configure + first draw.
    if let Err(e) = event_queue.roundtrip(&mut state) {
        eprintln!("roundtrip error: {}", e);
        std::process::exit(2);
    }
    let code = run_until(&conn, &mut event_queue, &mut state, deadline);
    // Deliver pending requests (e.g. the final commit) before exiting.
    let _ = conn.flush();
    log_kv(&[("ev", "exit".into()), ("code", code.into())]);
    std::process::exit(code);
}
