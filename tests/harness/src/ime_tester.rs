//! Raw-protocol input-method tester (`client-kit ime`, #6).
//!
//! Acts as the IME side of zwp_input_method_v2 against veyra:
//!
//!   bind zwp_input_method_manager_v2 + wl_seat → get_input_method
//!   → wait for activate → grab_keyboard → on each 'a' key PRESS
//!   commit_string("あ") (and on 'k' commit_string("漢")), applying the
//!   state with commit(). Logs ime_activate / ime_deactivate /
//!   ime_grabbed / ime_key(code,state) / ime_committed.
//!
//! Together with `client-kit probe --text-input` (the client side of
//! zwp_text_input_v3) this verifies the full compositor loop:
//! focused text field → IME grab receives keys → commit_string →
//! committed_string delivered to the focused surface.

use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{
    globals::registry_queue_init, protocol::wl_keyboard, Connection, Dispatch, QueueHandle,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
};

pub fn run_ime(duration_ms: u64) -> i32 {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("connect to wayland: NoCompositor");
            return 1;
        }
    };
    let (globals, mut event_queue) = match registry_queue_init::<ImeTester>(&conn) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("registry init failed");
            return 2;
        }
    };
    let qh = event_queue.handle();
    let manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 =
        match globals.bind(&qh, 1..=1, ()) {
            Ok(m) => m,
            Err(_) => {
                eprintln!("zwp_input_method_manager_v2 not advertised");
                return 3;
            }
        };
    let seat: wl_seat::WlSeat = match globals.bind(&qh, 1..=7, ()) {
        Ok(s) => s,
        Err(_) => return 3,
    };
    let mut state = ImeTester {
        running: true,
        method: None,
        grab: None,
        commit_serial: 0u32,
    };
    let method = manager.get_input_method(&seat, &qh, ());
    state.method = Some(method);
    // Initial roundtrip so the get_input_method object is created
    // before the main loop.
    let _ = event_queue.roundtrip(&mut state);

    let start = Instant::now();
    while state.running && start.elapsed() < Duration::from_millis(duration_ms) {
        let mut pfd = libc::pollfd {
            fd: conn.backend().poll_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 50) };
        if ret > 0 {
            if let Some(guard) = conn.prepare_read() {
                let _ = guard.read();
            }
        }
        if event_queue.dispatch_pending(&mut state).is_err() {
            break;
        }
        let _ = event_queue.flush();
    }
    if let Some(grab) = state.grab.take() {
        grab.release();
    }
    if let Some(method) = state.method.take() {
        method.destroy();
    }
    0
}

struct ImeTester {
    running: bool,
    method: Option<zwp_input_method_v2::ZwpInputMethodV2>,
    grab: Option<zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2>,
    commit_serial: u32,
}

impl ImeTester {
    fn commit_text(&mut self, qh: &QueueHandle<Self>, text: &str) {
        let Some(method) = &self.method else { return };
        method.commit_string(text.to_string());
        self.commit_serial += 1;
        method.commit(self.commit_serial);
        crate::log_kv(&[
            ("ev", "ime_committed".into()),
            ("text", text.to_string().into()),
        ]);
        // Keep the queue handle alive for symmetry with other testers.
        let _ = qh;
    }
}

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for ImeTester {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ImeTester {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_input_method_manager_v2::ZwpInputMethodManagerV2, ()> for ImeTester {
    fn event(
        _: &mut Self,
        _: &zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        _: zwp_input_method_manager_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for ImeTester {
    fn event(
        state: &mut Self,
        method: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                crate::log_kv(&[("ev", "ime_activate".into())]);
                // Acknowledge the activation with an empty state commit,
                // then take the keyboard grab so injected keys arrive.
                state.commit_serial += 1;
                method.commit(state.commit_serial);
                let grab = method.grab_keyboard(qh, ());
                crate::log_kv(&[("ev", "ime_grabbed".into())]);
                state.grab = Some(grab);
            }
            zwp_input_method_v2::Event::Deactivate => {
                crate::log_kv(&[("ev", "ime_deactivate".into())]);
                if let Some(grab) = state.grab.take() {
                    grab.release();
                }
            }
            zwp_input_method_v2::Event::Unavailable => {
                crate::log_kv(&[("ev", "ime_unavailable".into())]);
                state.running = false;
            }
            _ => {}
        }
        let _ = conn;
    }
}

impl Dispatch<zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2, ()> for ImeTester {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let zwp_input_method_keyboard_grab_v2::Event::Key {
            key,
            state: key_state,
            ..
        } = event
        {
            let pressed = key_state
                .into_result()
                .map(|s| s == wl_keyboard::KeyState::Pressed)
                .unwrap_or(false);
            crate::log_kv(&[
                ("ev", "ime_key".into()),
                ("code", key.into()),
                ("pressed", pressed.into()),
            ]);
            // evdev keycode 30 = 'a', 37 = 'k' (raw X keycode - 8).
            if pressed {
                match key {
                    30 => state.commit_text(qh, "あ"),
                    37 => state.commit_text(qh, "漢"),
                    _ => {}
                }
            }
        }
    }
}
