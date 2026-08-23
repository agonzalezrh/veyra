mod backend;
mod compositor;
mod config;
mod focus;
mod input;
mod input_router;
mod interaction;
mod kvmfr;
mod layout;
mod perf;
mod persist;
mod producer;
mod renderer;
mod scene;
mod snap;
#[cfg(test)]
mod stress_tests;
mod window;
mod workspace;

use std::sync::Arc;

use compositor::{ClientState, LookingGlass};
use producer::{HostileCheckerboard, StaticColor};
use smithay::backend::input::{AbsolutePositionEvent, Axis, InputEvent, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::PostAction;
use smithay::reexports::calloop::Interest;
use smithay::reexports::calloop::Mode;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "veyra=info,warn".into()),
        )
        .init();

    tracing::info!("Veyra starting");

    let mut event_loop: EventLoop<'static, LookingGlass> =
        EventLoop::try_new().expect("Failed to create event loop");
    let handle = event_loop.handle();

    let display: Display<LookingGlass> = Display::new().expect("Failed to create Wayland display");
    let display_handle = display.handle();

    // Initialize the winit backend
    let (backend, winit_source) =
        winit::init::<GlesRenderer>().expect("Failed to initialize winit backend");

    let mut state = LookingGlass::new(&display_handle, backend);

    // Register the animated checkerboard frame producer
    // This proves the external frame producer pipeline works with continuous updates.
    // A Looking Glass KVMFR producer would be registered the same way.
    // Register benchmark producers if BENCHMARK_VISUALS is set
    let bench_count: usize = std::env::var("BENCHMARK_VISUALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // Always add the standard producers
    if let Some(prod) = HostileCheckerboard::new(
        state.backend.as_mut().map(|b| b.renderer()).unwrap(),
    ) {
        state.add_producer(Box::new(prod));
    }
    state.add_producer(Box::new(kvmfr::KvmfrFrameProducer::new()));

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
                state.render();
                Ok(PostAction::Continue)
            },
        )
        .expect("Failed to init wayland server source");

    // Periodic render timer (~60 fps)
    use smithay::reexports::calloop::timer::{Timer, TimeoutAction};
    let render_timer = Timer::from_duration(std::time::Duration::from_millis(16));
    handle
        .insert_source(render_timer, |_, _, state| {
            state.render();
            TimeoutAction::ToDuration(std::time::Duration::from_millis(16))
        })
        .expect("Failed to register render timer");

    // Winit event source
    handle
        .insert_source(winit_source, |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                tracing::debug!("Window resized to {:?}", size);
                state.window_size = (size.w as f32, size.h as f32);
                state.render();
            }
            WinitEvent::Input(event) => {
                match event {
                    InputEvent::Keyboard { event } => {
                        let key = event.key_code();
                        let pressed = event.state() == smithay::backend::input::KeyState::Pressed;
                        state.handle_key(u32::from(key), pressed);
                    }
                    InputEvent::PointerMotionAbsolute { event } => {
                        let x = event.x();
                        let y = event.y();
                        // During right/middle dray, pan/orbit instead of moving content
                        // (button state tracked via has_active_drag flag)
                        state.handle_pointer_move(x, y);
                    }
                    InputEvent::PointerButton { event } => {
                        let pressed = event.state() == smithay::backend::input::ButtonState::Pressed;
                        let (mx, my) = state.last_mouse;
                        // Encode button as u32: 1=Left, 2=Middle, 3=Right
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
                                    state.handle_pointer_down(mx, my, false, false, false);
                                } else {
                                    state.handle_pointer_up(mx, my);
                                }
                            }
                            2 | 3 => {
                                // Middle (pan) and Right (orbit): handled in pointer_move
                            }
                            _ => {}
                        }
                    }
                    InputEvent::PointerAxis { event } => {
                        let v = event.amount(Axis::Vertical).unwrap_or(0.0);
                        let h = event.amount(Axis::Horizontal).unwrap_or(0.0);
                        if v.abs() > h.abs() {
                            state.handle_zoom(v);
                        } else {
                            state.handle_zoom(h);
                        }
                    }
                    _ => {}
                }
                state.render();
            }
            WinitEvent::CloseRequested => {
                tracing::info!("Close requested, saving workspace state");
                state.save_state();
                state.backend.take();
            }
            _ => {}
        })
        .expect("Failed to register winit event source");

    tracing::info!("Veyra running on {}", socket_name);

    let _ = event_loop.run(None, &mut state, |_| {});
}
