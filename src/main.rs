mod anchor;
mod app_switcher;
mod arrange;
mod backend;
mod bench;
mod capabilities;
mod chrome;
mod client_resize;
mod closed;
mod compositor;
mod config;
mod drm_regression;
mod drm_topology;
mod context_menu;
mod debug_journal;
mod dmabuf;
mod drm_backend;
mod focus;
mod focus_history;
mod fullscreen;
mod group;
mod input;
mod input_router;
mod interaction;
mod keys;
mod launcher;
mod layout;
mod maximize;
mod native_backend;
mod navigation;
mod outputs;
mod perf;
mod persist;
mod pointer_constraints;
mod producer;
mod recovery;
mod renderer;
mod resize;
mod scene;
mod scheduler;
mod session;
mod shelf;
mod shell;
mod simulated;
mod snap;
#[cfg(test)]
mod stress_tests;
mod window;
mod workspace;
mod xwm;

use std::sync::Arc;

use crate::backend::WinitPresentationBackend;
use compositor::{ClientState, LookingGlass};
use config::Config;
use producer::StaticColor;
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, InputEvent, KeyboardKeyEvent, MouseButton, PointerAxisEvent,
    PointerButtonEvent,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent};

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::Interest;
use smithay::reexports::calloop::Mode;
use smithay::reexports::calloop::PostAction;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Nested (winit) startup: window clamp, state construction, and the
/// advertised-mode sync. Returns the source to insert into the loop.
/// #9: runtime config reload — own the inotify fd, watch the config
/// file for writes, and reload + apply on change. Event-driven: the
/// watch source only fires when the file actually changes.
struct InotifyFd(i32);

impl std::os::unix::io::AsRawFd for InotifyFd {
    fn as_raw_fd(&self) -> i32 {
        self.0
    }
}

impl std::os::unix::io::AsFd for InotifyFd {
    fn as_fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        unsafe { std::os::unix::io::BorrowedFd::borrow_raw(self.0) }
    }
}

impl Drop for InotifyFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

fn watch_config_for_reload(
    handle: &smithay::reexports::calloop::LoopHandle<'static, LookingGlass>,
) -> Result<(), String> {
    let path = crate::config::config_path();
    if !path.exists() {
        return Err(format!("config file not present: {}", path.display()));
    }
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        return Err("inotify_init1 failed".into());
    }
    let watch = std::ffi::CString::new(path.to_str().ok_or("config path not utf-8")?.to_owned())
        .map_err(|_| "config path contains NUL".to_owned())?;
    let wd = unsafe {
        libc::inotify_add_watch(
            fd,
            watch.as_ptr(),
            libc::IN_MODIFY | libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO,
        )
    };
    if wd < 0 {
        unsafe { libc::close(fd) };
        return Err(format!("inotify_add_watch failed: {}", path.display()));
    }
    let owned = InotifyFd(fd);
    let fd_for_cb = fd;
    handle
        .insert_source(
            Generic::new(owned, Interest::READ, Mode::Level),
            move |_, _, state| {
                // Drain the event queue (the event contents don't
                // matter — any write to the config file is a reload
                // signal), then reload + apply.
                let mut buf = [0u8; 4096];
                loop {
                    let n = unsafe {
                        libc::read(fd_for_cb, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                    };
                    if n <= 0 {
                        break;
                    }
                }
                let config = crate::config::Config::load();
                info!(scale = config.appearance.output_scale, "config reloaded");
                state.apply_config_changes(config);
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| format!("failed to register config watch: {e}"))?;
    info!(path = %path.display(), "config reload watcher active (write to reload)");
    Ok(())
}

fn start_winit_state(
    display_handle: &smithay::reexports::wayland_server::DisplayHandle,
    config: &Config,
) -> (LookingGlass, winit::WinitEventLoop) {
    let (backend, winit_source) =
        winit::init::<GlesRenderer>().expect("Failed to initialize winit backend");

    // Clamp the nested window to the output. Smithay's default winit
    // window is 1280x800; on smaller outputs (e.g. the harness's
    // 1280x720 Xvfb screen) the overflow renders the shell taskbar —
    // and the bottom of the framebuffer — off-screen, while the
    // compositor's default window_size (1280x720) silently disagrees
    // with the actual GL viewport.
    // G-E5.5: the simulation framebuffer sizes the nested window — the
    // surface must contain every output's viewport. Same spec as the
    // registry seeder (outputs::simulated_layout).
    if let Ok(n) = std::env::var("VEYRA_SIM_OUTPUTS").ok().unwrap_or_default().parse::<u32>() {
        if n > 1 {
            let (ew, eh) = crate::outputs::simulated_extents(n);
            let _ = backend.window().request_inner_size(
                smithay::reexports::winit::dpi::PhysicalSize::new(ew, eh),
            );
            tracing::info!(w = ew, h = eh, "nested window sized to simulation extents");
        }
    }
    if let Some(monitor) = backend.window().current_monitor() {
        let ms = monitor.size();
        let win = backend.window().inner_size();
        if win.width > ms.width || win.height > ms.height {
            let _ = backend.window().request_inner_size(
                smithay::reexports::winit::dpi::PhysicalSize::new(ms.width, ms.height),
            );
            tracing::info!(
                monitor_w = ms.width,
                monitor_h = ms.height,
                "clamped nested window to output size"
            );
        }
    }
    let initial_size = backend.window_size();

    let mut state = LookingGlass::new(
        display_handle,
        Box::new(WinitPresentationBackend(backend)),
        config.clone(),
    );
    // P1 (audit): winit contexts cannot be recreated mid-session — a
    // lost GL context must fail loudly instead of silently no-oping.
    state.backend_origin = Some(compositor::BackendOrigin::Winit);
    // Trust the actual winit window over the struct default: without a
    // WM (raw Xvfb) no Resized event may arrive, leaving window_size
    // stale and desynchronizing projection, input mapping, and the
    // shell plane from the real framebuffer.
    state.window_size = (initial_size.w as f32, initial_size.h as f32);
    tracing::info!(window_size = ?state.window_size, "render size");
    // R11: the advertised output mode follows the actual backend size
    // from the start — clients see the real monitor, not a fixed mode.
    state.sync_output_mode(initial_size.w, initial_size.h, 60000);
    (state, winit_source)
}

fn main() {
    crate::debug_journal::init_from_env();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "veyra=info,warn".into()),
        )
        .init();

    tracing::info!(
        profile = option_env!("VEYRA_PROFILE").unwrap_or("unknown"),
        built = option_env!("VEYRA_BUILD_TS").unwrap_or("unknown"),
        "Veyra starting"
    );

    // Load configuration
    let config = Config::load();
    tracing::info!(workspaces = config.workspace.count, "config loaded");

    // Check for --native flag to use DRM backend
    let use_native = std::env::args().any(|a| a == "--native");
    // Check for --normal/--2d flag to start in normal (2D, ortho) mode.
    // Deterministic mode pinning: the harness (and users) must not depend
    // on injected F5 keypresses, which proved unreliable across setups.
    let start_normal = std::env::args().any(|a| a == "--normal" || a == "--2d");

    // G-B3 validation probe: render N frames through the full
    // GBM → EGL dmabuf → KMS page-flip pipeline without starting the
    // compositor (no wayland socket, no winit). Run against a virtual
    // device with: sudo -E VEYRA_DRM_CARD=/dev/dri/card1 \
    //               VEYRA_DRM_PROBE=90 ./veyra
    if let Ok(mode) = std::env::var("VEYRA_DRM_PROBE") {
        let result = if mode == "flip" {
            crate::drm_backend::run_flip_probe(90)
        } else {
            let frames: u32 = mode.parse().unwrap_or(90);
            tracing::info!(frames, "running DRM presentation probe");
            crate::drm_backend::run_probe(frames)
        };
        match result {
            Ok(()) => {
                tracing::info!("drm probe OK");
                std::process::exit(0);
            }
            Err(e) => {
                tracing::error!(e, "drm probe FAILED");
                std::process::exit(1);
            }
        }
    }

    let mut event_loop: EventLoop<'static, LookingGlass> =
        EventLoop::try_new().expect("Failed to create event loop");
    let handle = event_loop.handle();

    let display: Display<LookingGlass> = Display::new().expect("Failed to create Wayland display");
    let display_handle = display.handle();

    // P1 #2: --native NEVER initializes winit — the session owns the
    // DRM device and libinput replaces the winit event source. Winit is
    // started only for the nested path (or as a fallback when the
    // native stack refuses cleanly).
    let mut winit_source: Option<winit::WinitEventLoop> = None;
    let mut state = if use_native {
        tracing::info!("Starting native DRM/KMS backend");
        match native_backend::create_native_state(&display_handle, &config) {
            Ok((mut native_state, stack)) => {
                if let Err(e) = native_backend::wire_native_input(&handle, &mut native_state, stack)
                {
                    tracing::error!(e = %e, "native input setup failed");
                    std::process::exit(1);
                }
                tracing::info!("Native backend initialized successfully");
                native_state
            }
            Err(e) => {
                tracing::error!(
                    e = %e,
                    "Failed to initialize native backend, falling back to winit"
                );
                let (s, src) = start_winit_state(&display_handle, &config);
                winit_source = Some(src);
                s
            }
        }
    } else {
        let (s, src) = start_winit_state(&display_handle, &config);
        winit_source = Some(src);
        s
    };

    // Start in normal (2D, ortho) mode when requested.
    if start_normal {
        state.spatial_mode = false;
        tracing::info!("starting in normal (2D) mode");
    }

    // Run session startup sequence
    match state.session.startup_sequence(&mut state.workspace_manager) {
        Ok(()) => tracing::info!("session startup complete"),
        Err(e) => tracing::warn!(?e, "session startup issue (non-fatal)"),
    }

    // Load saved workspace state (applies on top of config defaults)
    state.load_saved_state();

    // Register frame producers
    let bench_count: usize = std::env::var("BENCHMARK_VISUALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // Standard producers are disabled when Wayland clients are connected.
    // They were useful during development but add unnecessary rendering
    // overhead (texture imports, scene changes, producer glitch errors)
    // when real Wayland applications are running.
    // Enable with environment variables if needed.

    // Create benchmark producers first, then add them to the state
    let mut bench_producers: Vec<Box<dyn producer::FrameProducer>> = Vec::new();
    if bench_count > 0 {
        let renderer = state.backend.as_mut().map(|b| b.renderer()).unwrap();
        tracing::info!(count = bench_count, "benchmark: creating visuals");
        for i in 0..bench_count {
            let r = ((i * 37) % 256) as u8;
            let g = ((i * 71) % 256) as u8;
            let b = ((i * 113) % 256) as u8;
            if let Some(p) = StaticColor::new(renderer, r, g, b) {
                bench_producers.push(Box::new(p));
            }
        }
    }
    for (i, p) in bench_producers.into_iter().enumerate() {
        state.add_benchmark_visual(p, i, bench_count);
    }
    if bench_count > 0 {
        // G-G4: continuous frame demand — static producers return
        // Unchanged and the demand-driven loop would go idle, measuring
        // nothing. Auto-orbit keeps the scheduler animating so the
        // PROFILE lines capture the steady-state draw path at N
        // visuals (culling included).
        state.workspace_manager.active_mut().auto_orbit = true;
        tracing::info!(total = %(bench_count + 2), "benchmark scene ready (auto-orbit on)");
    }

    // Wayland socket listener
    let source = ListeningSocketSource::new_auto().expect("Failed to create listening socket");
    let socket_name = source.socket_name().to_string_lossy().into_owned();
    handle
        .insert_source(source, |client_stream, _, state| {
            if let Err(err) = state
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                tracing::warn!("Error adding wayland client: {}", err);
            };
        })
        .expect("Failed to init wayland socket source");
    tracing::info!("Listening on wayland socket: {}", socket_name);

    // G-C4: XWayland — spawn the server and register its event source.
    // Missing Xwayland binary degrades cleanly to native-only.
    state.loop_handle = Some(handle.clone());
    if let Some((xwayland, xwayland_client)) = crate::xwm::spawn_xwayland(&display_handle) {
        crate::xwm::insert_xwayland_source(&mut state, xwayland, xwayland_client, &handle);
    }

    // #9: runtime config reload — watch the config file with inotify
    // (event-driven: an idle compositor wakes for nothing). A write to
    // the file triggers a reload; the output scale is applied live
    // (wl_output scale + preferred fractional scale to clients).
    if let Err(e) = watch_config_for_reload(&handle) {
        tracing::info!(?e, "config reload watcher unavailable (reload disabled)");
    }

    // Wayland display dispatch source
    handle
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, state| {
                let inner = unsafe { display.get_mut() };
                let _ = inner.dispatch_clients(state);
                let _ = inner.flush_clients();
                state.schedule_render();
                Ok(PostAction::Continue)
            },
        )
        .expect("Failed to init wayland server source");

    // R6: demand-driven render scheduling — no fixed-rate polling.
    // Dirty state (commits, input, resizes, interaction) pings the loop
    // and renders immediately. Continuous work (camera/focus animation,
    // pending client frame callbacks) keeps a self-rescheduling pacing
    // timer armed; when the compositor goes idle the timer source is
    // dropped entirely — no render calls, no timer wakeups.
    use smithay::reexports::calloop::ping;
    let (render_ping, render_ping_source) =
        ping::make_ping().expect("Failed to create render ping");
    let ping_for_signal = render_ping.clone();
    state.render_ping = Some(render_ping);
    let loop_handle_for_pump = handle.clone();
    handle
        .insert_source(render_ping_source, move |_, _, state| {
            state.pump_render_loop(&loop_handle_for_pump);
        })
        .expect("Failed to register render ping source");

    // G-H0.9: a session manager (and any sane supervisor) stops the
    // compositor with SIGTERM; Ctrl+C is SIGINT. Neither is a crash —
    // both are CLEAN SHUTDOWN paths and must persist workspace state
    // exactly like an ICCCM window close. Block the signals process-
    // wide, then sigwait on a dedicated thread and wake the render
    // loop; the pump (main-thread state access) performs the save.
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let sig_flag = shutdown_flag.clone();
        unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::sigaddset(&mut mask, libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        }
        std::thread::spawn(move || unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::sigaddset(&mut mask, libc::SIGINT);
            let mut sig: libc::c_int = 0;
            if libc::sigwait(&mask, &mut sig) == 0 {
                tracing::info!(signal = sig, "shutdown signal received — saving state");
                sig_flag.store(true, Ordering::SeqCst);
                ping_for_signal.ping();
            }
        });
        state.shutdown_requested = Some(shutdown_flag);
    }
    // Ensure the initial frame renders
    state.schedule_render();

    // Winit event source (nested mode only — native mode takes input
    // from libinput instead; the winit backend/window are discarded)
    if let Some(winit_source) = winit_source.take() {
        handle
            .insert_source(winit_source, |event, _, state| match event {
                WinitEvent::Resized { size, .. } => {
                    // P1 (audit): winit reports 0×N on some minimize/resize
                    // transitions; storing it verbatim poisons projection,
                    // picking, and the shell plane (aspect ∞). Clamp at the
                    // single storage point.
                    state.window_size = ((size.w as f32).max(1.0), (size.h as f32).max(1.0));
                    // Same greppable shape as the startup log so consumers
                    // (harness) always see the CURRENT render size.
                    tracing::info!(window_size = ?state.window_size, "render size");
                    // R11: the advertised output mode follows the resize.
                    state.sync_output_mode(size.w, size.h, 60000);
                    state.schedule_render();
                }
                WinitEvent::Input(event) => {
                    match event {
                        InputEvent::Keyboard { event } => {
                            let key = event.key_code();
                            let pressed =
                                event.state() == smithay::backend::input::KeyState::Pressed;
                            state.handle_key(u32::from(key), pressed);
                            state.schedule_render();
                        }
                        InputEvent::PointerMotionAbsolute { event } => {
                            let x = event.x();
                            let y = event.y();
                            state.handle_pointer_move(x, y);
                            state.schedule_render();
                        }
                        InputEvent::PointerButton { event } => {
                            let pressed =
                                event.state() == smithay::backend::input::ButtonState::Pressed;
                            let (mx, my) = state.last_mouse;
                            let btn_code = match event.button() {
                                Some(MouseButton::Left) => 1u32,
                                Some(MouseButton::Middle) => 2u32,
                                Some(MouseButton::Right) => 3u32,
                                _ => 0u32,
                            };
                            if pressed {
                                state.nav_button = btn_code;
                            } else {
                                state.nav_button = 0;
                            }
                            match btn_code {
                                1 => {
                                    if pressed {
                                        // If context menu is open, clicking outside dismisses it
                                        if state.context_menu.visible {
                                            if !state.handle_menu_click(mx, my) {
                                                state.context_menu.dismiss();
                                            }
                                            state.schedule_render();
                                            return;
                                        }
                                        state.handle_pointer_down(mx, my, false, false, false);
                                    } else {
                                        state.handle_pointer_up(mx, my);
                                    }
                                }
                                3 => {
                                    // H0.5.3: right press on a window arms
                                    // per-window rotation (release below the
                                    // threshold opens the context menu);
                                    // on empty background nav_button=3
                                    // drives camera orbit as before.
                                    if pressed {
                                        state.handle_right_press(mx, my);
                                    } else {
                                        state.handle_right_release(mx, my);
                                    }
                                }
                                2 => {}
                                _ => {}
                            }
                            state.schedule_render();
                        }
                        InputEvent::PointerAxis { event } => {
                            // LineDelta wheels (XTEST button 4/5, physical
                            // wheel notches) carry their value in the
                            // v120 convention — amount() is None for them.
                            let v = event
                                .amount(Axis::Vertical)
                                .or_else(|| event.amount_v120(Axis::Vertical))
                                .unwrap_or(0.0);
                            let h = event
                                .amount(Axis::Horizontal)
                                .or_else(|| event.amount_v120(Axis::Horizontal))
                                .unwrap_or(0.0);
                            let (mx, my) = state.last_mouse;
                            state.handle_axis(mx, my, h, v);
                            state.schedule_render();
                        }
                        _ => {}
                    }
                }
                WinitEvent::CloseRequested => {
                    tracing::info!("Close requested");
                    state.save_state();
                    let _ = state.session.shutdown_sequence(|| {
                        // State already saved above
                    });
                    std::process::exit(0);
                }
                _ => {}
            })
            .expect("Failed to register winit event source");
    }

    tracing::info!(window_size = ?state.window_size, "render size");
    tracing::info!("Veyra running on {}", socket_name);

    let _ = event_loop.run(None, &mut state, |_| {});
}
