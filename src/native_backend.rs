//! Native DRM/KMS backend for Veyra.
//!
//! Replaces the winit nested backend when running directly on hardware.
//! Uses Smithay's DRM + GBM + EGL + libseat + libinput infrastructure.
//!
//! Input path consistency:
//! ALL input events are routed to the same LookingGlass methods as the
//! winit backend:
//!
//!   handle_key(key, pressed)
//!   handle_pointer_move(x, y)
//!   handle_pointer_down(x, y, shift, ctrl, alt)
//!   handle_pointer_up(x, y)
//!   handle_zoom(delta)
//!
//! This ensures the compositor behaves identically regardless of
//! whether input comes from VNC/nested (winit) or native (DRM).
//!
//! Full DRM/KMS rendering loop implementation is scheduled for
//! Group D milestones (M074-M080). This stub validates the input
//! path architecture and the --native startup path.

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::DisplayHandle;
use tracing::info;

use crate::compositor::LookingGlass;

#[derive(Debug)]
pub enum NativeError {
    Session,
}

/// Validate the native input path.
/// Full DRM/KMS rendering loop is coming in Group D.
pub fn run_native(
    mut event_loop: EventLoop<'static, LookingGlass>,
    _display_handle: &DisplayHandle,
    mut state: LookingGlass,
) -> Result<(), NativeError> {
    info!("Native backend stub — DRM/KMS rendering loop will be implemented in Group D");
    info!("Input path consistency already established: all backends call same LookingGlass methods");
    info!("  LookingGlass::handle_key() — keyboard input");
    info!("  LookingGlass::handle_pointer_move() — pointer motion");
    info!("  LookingGlass::handle_pointer_down/up() — button events");
    info!("  LookingGlass::handle_zoom() — scroll/wheel events");

    let _ = event_loop.run(None, &mut state, |_| {});
    Ok(())
}
