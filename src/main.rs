mod anchor;
mod app_switcher;
mod arrange;
mod bench;
mod backend;
mod keys;
mod capabilities;
mod compositor;
mod config;
mod context_menu;
mod client_resize;
mod closed;
mod debug_journal;
mod dmabuf;
mod drm_backend;
mod focus;
mod group;
mod input;
mod input_router;
mod interaction;
mod launcher;
mod layout;
mod maximize;
mod fullscreen;
mod focus_history;
mod chrome;
mod shell;
mod native_backend;
mod navigation;
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
mod simulated;
mod snap;
#[cfg(test)]
mod stress_tests;
mod window;
mod workspace;

use std::sync::Arc;

use compositor::{ClientState, LookingGlass};
use config::Config;
use producer::{HostileCheckerboard, StaticColor};
use smithay::backend::input::{AbsolutePositionEvent, Axis, InputEvent, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent};

use crate::backend::{PresentationBackend, WinitPresentationBackend};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::PostAction;
use smithay::reexports::calloop::Interest;
use smithay::reexports::calloop::Mode;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use tracing_subscriber::EnvFilter;

fn main() {
    crate::debug_journal::init_from_env();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "veyra=info,warn".into()),
        )
        .init();

    tracing::info!("Veyra starting");

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

    // Initialize the winit backend
    let (backend, winit_source) =
        winit::init::<GlesRenderer>().expect("Failed to initialize winit backend");

    // Clamp the nested window to the output. Smithay's default winit
    // window is 1280x800; on smaller outputs (e.g. the harness's
    // 1280x720 Xvfb screen) the overflow renders the shell taskbar —
    // and the bottom of the framebuffer — off-screen, while the
    // compositor's default window_size (1280x720) silently disagrees
    // with the actual GL viewport.
    if let Some(monitor) = backend.window().current_monitor() {
        let ms = monitor.size();
        let win = backend.window().inner_size();
        if win.width > ms.width || win.height > ms.height {
            backend
                .window()
                .request_inner_size(smithay::reexports::winit::dpi::PhysicalSize::new(
                    ms.width, ms.height,
                ));
            tracing::info!(monitor_w = ms.width, monitor_h = ms.height, "clamped nested window to output size");
        }
    }
    let initial_size = backend.window_size();

    let mut state = LookingGlass::new(&display_handle, Box::new(WinitPresentationBackend(backend)), config.clone());
    // Trust the actual winit window over the struct default: without a
    // WM (raw Xvfb) no Resized event may arrive, leaving window_size
    // stale and desynchronizing projection, input mapping, and the
    // shell plane from the real framebuffer.
    state.window_size = (initial_size.w as f32, initial_size.h as f32);
    tracing::info!(window_size = ?state.window_size, "render size");
    // R11: the advertised output mode follows the actual backend size
    // from the start — clients see the real monitor, not a fixed mode.
    state.sync_output_mode(initial_size.w as i32, initial_size.h as i32, 60000);

    // Handle --native flag: construct DrmGraphicsBackend instead
    if use_native {
        tracing::info!("Starting native DRM/KMS backend");
        match crate::drm_backend::DrmGraphicsBackend::try_new() {
            Ok(drm_backend) => {
                state = LookingGlass::new(&display_handle, Box::new(drm_backend), config.clone());
                tracing::info!("Native backend initialized successfully");
                // R11: a native state is fresh — adopt the KMS mode as
                // both the framebuffer size and the advertised output
                // mode (the winit initial size no longer applies).
                let (w, h) = state.backend.as_ref().unwrap().size();
                state.window_size = (w as f32, h as f32);
                state.sync_output_mode(w as i32, h as i32, 60000);
                tracing::info!(window_size = ?state.window_size, "render size");
            }
            Err(e) => {
                tracing::error!(?e, "Failed to initialize native backend, falling back to winit");
                // Keep the winit backend already set up in `state`
            }
        }
    }

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
        tracing::info!(total = %(bench_count + 2), "benchmark scene ready");
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
    state.render_ping = Some(render_ping);
    let loop_handle_for_pump = handle.clone();
    handle
        .insert_source(render_ping_source, move |_, _, state| {
            state.pump_render_loop(&loop_handle_for_pump);
        })
        .expect("Failed to register render ping source");
    // Ensure the initial frame renders
    state.schedule_render();

    // Winit event source
    handle
        .insert_source(winit_source, |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                state.window_size = (size.w as f32, size.h as f32);
                // Same greppable shape as the startup log so consumers
                // (harness) always see the CURRENT render size.
                tracing::info!(window_size = ?state.window_size, "render size");
                // R11: the advertised output mode follows the resize.
                state.sync_output_mode(size.w as i32, size.h as i32, 60000);
                state.schedule_render();
            }
            WinitEvent::Input(event) => {
                match event {
                    InputEvent::Keyboard { event } => {
                        let key = event.key_code();
                        let pressed = event.state() == smithay::backend::input::KeyState::Pressed;
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
                        let pressed = event.state() == smithay::backend::input::ButtonState::Pressed;
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
                                if pressed {
                                    state.handle_context_menu(mx, my);
                                }
                            }
                            2 => {}
                            _ => {}
                        }
                        state.schedule_render();
                    }
                    InputEvent::PointerAxis { event } => {
                        let v = event.amount(Axis::Vertical).unwrap_or(0.0);
                        let h = event.amount(Axis::Horizontal).unwrap_or(0.0);
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

    tracing::info!(window_size = ?state.window_size, "render size");
    tracing::info!("Veyra running on {}", socket_name);

    let _ = event_loop.run(None, &mut state, |_| {});
}
