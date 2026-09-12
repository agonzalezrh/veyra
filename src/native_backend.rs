//! Native DRM/KMS backend for Veyra.
//!
//! Replaces the winit nested backend when running directly on hardware:
//! a libseat session owns the DRM device (and DRM master), libinput over
//! udev feeds the same LookingGlass input methods the winit backend uses,
//! and the shared calloop loop drives presentation through
//! `DrmGraphicsBackend` (GBM swapchain + page flips, G-B3).
//!
//! Input path consistency:
//! ALL input events are routed to the same LookingGlass methods as the
//! winit backend:
//!
//!   handle_key(key, pressed)
//!   handle_pointer_move(x, y)
//!   handle_pointer_down(x, y, shift, ctrl, alt)
//!   handle_pointer_up(x, y)
//!   handle_axis(x, y, h, v)
//!
//! This ensures the compositor behaves identically regardless of
//! whether input comes from nested (winit) or native (DRM + libinput).

use smithay::backend::input::{self as backend, ButtonState, InputEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::Session as _;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_server::DisplayHandle;
use tracing::info;

use crate::compositor::LookingGlass;
use crate::config::Config;
use crate::drm_backend::DrmGraphicsBackend;

/// The session objects a native state needs before its event sources
/// can be wired into the loop (split construction: the state must exist
/// before calloop sources that dispatch into it).
pub struct NativeStack {
    session: LibSeatSession,
    notifier: LibSeatSessionNotifier,
}

/// Failure modes of the native startup path. Every variant is a CLEAN
/// refusal — main.rs falls back to the nested winit backend.
#[derive(Debug)]
pub enum NativeError {
    /// libseat could not provide a seat (headless/containers).
    Session(String),
    /// DRM/KMS presentation setup failed (device, mode, M079 gate).
    Drm(String),
    /// libinput over udev could not be initialized.
    Libinput(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Session(e) => write!(f, "session: {e}"),
            NativeError::Drm(e) => write!(f, "drm: {e}"),
            NativeError::Libinput(e) => write!(f, "libinput: {e}"),
        }
    }
}

/// P1 #2, phase 1: build the native compositor state — libseat session
/// first (device ownership + DRM master), then the session-owned DRM
/// presentation backend. NO winit anywhere on this path. Returns the
/// session objects for phase 2 (input wiring).
pub fn create_native_state(
    display_handle: &DisplayHandle,
    config: &Config,
) -> Result<(LookingGlass, NativeStack), NativeError> {
    info!("initializing native backend: session → drm");

    // Session: seat + VT control + DRM master lifetime.
    let (session, notifier) =
        LibSeatSession::new().map_err(|e| NativeError::Session(e.to_string()))?;
    info!(seat = %session.seat(), "libseat session acquired");
    let session_paused = !session.is_active();

    // DRM/KMS presentation through the session-owned device.
    let drm = DrmGraphicsBackend::try_new_with_session(&session)
        .map_err(|e| NativeError::Drm(e.to_string()))?;

    let mut state = LookingGlass::new(display_handle, Box::new(drm), config.clone());
    // P1 (audit): remember how this backend was built so a lost GL
    // context can genuinely be recovered — the session-owned device is
    // re-opened through a clone of the libseat session.
    state.backend_origin = Some(crate::compositor::BackendOrigin::Drm);
    state.drm_session = Some(session.clone());
    state.session_paused = session_paused;
    let (w, h) = state.backend.as_ref().expect("backend just set").size();
    state.window_size = (w, h);
    // R11: the advertised output mode follows the KMS mode from the start.
    state.sync_output_mode(w as i32, h as i32, 60000);
    tracing::info!(window_size = ?state.window_size, "render size");

    Ok((state, NativeStack { session, notifier }))
}

/// P1 #2, phase 2: wire seat (de)activation and libinput into the
/// shared calloop loop. Device access goes through the SAME session
/// (open_restricted → session.open), so input devices are also
/// revoked/restored with VT switches.
pub fn wire_native_input(
    handle: &LoopHandle<'static, LookingGlass>,
    state: &mut LookingGlass,
    stack: NativeStack,
) -> Result<(), NativeError> {
    let NativeStack { session, notifier } = stack;

    // Seat (de)activation: stop rendering while the VT is backgrounded.
    handle
        .insert_source(notifier, |event, _, state: &mut LookingGlass| match event {
            smithay::backend::session::Event::PauseSession => {
                info!("session paused (VT switch) — presentation suspended");
                state.session_paused = true;
            }
            smithay::backend::session::Event::ActivateSession => {
                info!("session activated — presentation resumed");
                state.session_paused = false;
                state.schedule_render();
            }
        })
        .map_err(|e| NativeError::Session(format!("notifier: {e}")))?;

    let seat = session.seat();
    let mut libinput_ctx =
        smithay::reexports::input::Libinput::new_with_udev(LibinputSessionInterface::from(session));
    libinput_ctx
        .udev_assign_seat(&seat)
        .map_err(|_| NativeError::Libinput(format!("udev_assign_seat({seat}) failed")))?;
    let input_backend = LibinputInputBackend::new(libinput_ctx);
    handle
        .insert_source(input_backend, |event, _, state: &mut LookingGlass| {
            dispatch_input_event(state, event);
            state.schedule_render();
        })
        .map_err(|e| NativeError::Libinput(format!("source: {e}")))?;

    // BUG_LIST #4 (step 1): page-flip completions are event-driven via
    // a calloop source on the DRM event fd — no more poll(0) per frame
    // in begin_frame (it remains only as a safety-net drain there). A
    // completed flip wakes the render loop immediately, so the next
    // frame's queue_buffer does not race the swapchain, and the flip
    // completion becomes the vblank tick future pacing builds on.
    let flip_fd = state.backend.as_mut().and_then(|b| {
        b.as_any()
            .downcast_mut::<crate::drm_backend::DrmGraphicsBackend>()
            .map(|d| d.event_device_fd())
    });
    if let Some(fdfd) = flip_fd {
        handle
            .insert_source(
                Generic::new(fdfd, Interest::READ, Mode::Level),
                |_, _, state: &mut LookingGlass| {
                    let completed = state
                        .backend
                        .as_mut()
                        .and_then(|b| {
                            b.as_any()
                                .downcast_mut::<crate::drm_backend::DrmGraphicsBackend>()
                        })
                        .map(|d| d.handle_flip_events())
                        .unwrap_or(false);
                    if completed {
                        state.schedule_render();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| NativeError::Session(format!("flip source: {e}")))?;
        info!("drm flip-event source registered (BUG_LIST #4 step 1)");
    }

    info!("native backend ready (session + drm + libinput)");
    Ok(())
}

/// Translate libinput events into the SAME LookingGlass input methods
/// the winit backend feeds (input path consistency). Absolute pointer
/// devices are transformed into the current framebuffer size; relative
/// devices (touchpads) accumulate into the tracked cursor.
fn dispatch_input_event(state: &mut LookingGlass, event: InputEvent<LibinputInputBackend>) {
    use smithay::backend::input::{
        AbsolutePositionEvent as _, KeyboardKeyEvent as _, PointerAxisEvent as _,
        PointerButtonEvent as _, PointerMotionEvent as _,
    };
    match event {
        InputEvent::Keyboard { event } => {
            let key = u32::from(event.key_code());
            let pressed = event.state() == smithay::backend::input::KeyState::Pressed;
            state.handle_key(key, pressed);
        }
        InputEvent::PointerMotion { event } => {
            // Relative device (touchpad): accumulate the delta, clamped
            // to the framebuffer like the nested path's absolute coords.
            let (w, h) = (state.window_size.0 as f64, state.window_size.1 as f64);
            let dx = event.delta_x();
            let dy = event.delta_y();
            let (mx, my) = state.last_mouse;
            let x = (mx + dx).clamp(0.0, w);
            let y = (my + dy).clamp(0.0, h);
            state.handle_pointer_move(x, y);
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let (w, h) = state.window_size;
            let pos = event.position_transformed(smithay::utils::Size::new(w as i32, h as i32));
            state.handle_pointer_move(pos.x, pos.y);
        }
        InputEvent::PointerButton { event } => {
            let pressed = event.state() == ButtonState::Pressed;
            let (mx, my) = state.last_mouse;
            // libinput button codes are evdev BTN_* (0x110 left,
            // 0x112 middle, 0x111 right); map to the compositor's
            // 1=left/2=middle/3=right convention.
            let btn_code = match event.button_code() {
                0x110 => 1u32,
                0x112 => 2u32,
                0x111 => 3u32,
                _ => 0u32,
            };
            if pressed {
                state.nav_button = btn_code;
            } else {
                state.nav_button = 0;
            }
            match (btn_code, pressed) {
                (1, true) if state.context_menu.visible => {
                    if !state.handle_menu_click(mx, my) {
                        state.context_menu.dismiss();
                    }
                }
                (1, true) => {
                    state.handle_pointer_down(mx, my, false, false, false);
                }
                (1, false) => {
                    state.handle_pointer_up(mx, my);
                }
                (3, true) => {
                    state.handle_context_menu(mx, my);
                }
                _ => {}
            }
        }
        InputEvent::PointerAxis { event } => {
            let v = event.amount(backend::Axis::Vertical).unwrap_or(0.0);
            let h = event.amount(backend::Axis::Horizontal).unwrap_or(0.0);
            let (mx, my) = state.last_mouse;
            state.handle_axis(mx, my, h, v);
        }
        InputEvent::DeviceAdded { device } => {
            info!(name = %device.name(), "input device added");
        }
        InputEvent::DeviceRemoved { device } => {
            info!(name = %device.name(), "input device removed");
        }
        _ => {
            // Touch/tablet/gesture events are not routed yet (no
            // compositor semantics for them on the desktop path).
        }
    }
}
