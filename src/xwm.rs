//! XWayland integration (G-C4): the X11 window manager (XWM) and the
//! xwayland-shell-v1 association hooks.
//!
//! XWayland is just another Wayland client: its X11 windows associate
//! with wl_surfaces via xwayland-shell-v1, and their buffers commit
//! through the SAME compositor commit path as native toplevels (see
//! `handle_commit`'s x11 branch). This module implements the X11-side
//! lifecycle (map/configure/destroy requests) and the clipboard/primary
//! selection bridge in both directions.

use smithay::delegate_xwayland_shell;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::Rectangle;
use smithay::wayland::compositor::SurfaceAttributes;
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, X11Surface, X11Wm, XwmHandler, XwmId};
use smithay::xwayland::{XWayland, XWaylandEvent};
use tracing::{debug, info, warn};

use crate::compositor::{LookingGlass, SelectionOwner};

/// Spawn the XWayland server. Missing `Xwayland` binary or socket
/// exhaustion degrades cleanly: the compositor runs native-only and the
/// caller logs the reason (M079-style capability discipline).
pub fn spawn_xwayland(dh: &DisplayHandle) -> Option<(XWayland, Client)> {
    if std::env::var_os("VEYRA_NO_XWAYLAND").is_some() {
        info!("XWayland disabled via VEYRA_NO_XWAYLAND");
        return None;
    }
    if std::process::Command::new("Xwayland")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        info!("XWayland not available (Xwayland binary missing) — running native-only");
        return None;
    }
    match XWayland::spawn(
        dh,
        None,
        // VEYRA_XWAYLAND_DEBUG=1 forwards WAYLAND_DEBUG into the
        // spawned server (stderr is inherited below) — protocol-level
        // diagnosis of the Xwayland↔compositor feed.
        std::env::var_os("VEYRA_XWAYLAND_DEBUG")
            .map(|_| ("WAYLAND_DEBUG".to_owned(), "1".to_owned())),
        false,
        std::process::Stdio::null(),
        // Inherit stderr only with VEYRA_XWAYLAND_DEBUG (goes to the
        // compositor log); null otherwise.
        if std::env::var_os("VEYRA_XWAYLAND_DEBUG").is_some() {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        },
        |_| {},
    ) {
        Ok(pair) => {
            info!("XWayland spawned");
            Some(pair)
        }
        Err(e) => {
            warn!(?e, "XWayland spawn failed — running native-only");
            None
        }
    }
}

/// Wire the spawned instance into the event loop; on Ready the XWM
/// starts and takes over the X11 connection.
pub fn insert_xwayland_source(
    state: &mut LookingGlass,
    xwayland: XWayland,
    client: Client,
    handle: &LoopHandle<'static, LookingGlass>,
) {
    state.xwayland_client = Some(client);
    let h = handle.clone();
    let res = handle.insert_source(xwayland, move |event, _, state| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => {
            info!(
                display = display_number,
                "XWayland ready; starting X11 window manager"
            );
            state.x11_display = Some(display_number);
            let Some(client) = state.xwayland_client.clone() else {
                warn!("XWayland ready but the client handle is gone");
                return;
            };
            match X11Wm::start_wm::<LookingGlass>(h.clone(), x11_socket, client) {
                Ok(wm) => {
                    state.x11_wm = Some(wm);
                }
                Err(e) => {
                    warn!(?e, "X11 window manager failed to start");
                }
            }
        }
        XWaylandEvent::Error => {
            warn!("XWayland failed during startup");
        }
    });
    if let Err(e) = res {
        warn!(?e, "failed to register XWayland event source");
    }
}

impl XWaylandShellHandler for LookingGlass {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm_id: XwmId, wl_surface: WlSurface, surface: X11Surface) {
        // xwayland-shell-v1: the X11 window is now bound to this
        // wl_surface. Buffers committed on the surface flow through the
        // regular commit path; the X11 window provides title/class and
        // the X-side lifecycle.
        info!(class = %surface.class(), "x11 window associated with wl_surface");
        self.x11_windows.insert(wl_surface.clone(), surface);
        // RACE: the association arrives via the X11 event source while
        // the first buffer commit arrives via the Wayland dispatch —
        // either order is legal. If the commit already happened it was
        // dropped as an unknown surface; process it now.
        let has_buffer = smithay::wayland::compositor::with_states(&wl_surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            matches!(
                cached.current().buffer,
                Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(_))
            )
        });
        if has_buffer {
            debug!("x11 association arrived after the first commit; processing it now");
            self.handle_commit(&wl_surface);
        }
    }
}

impl XwmHandler for LookingGlass {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.x11_wm
            .as_mut()
            .expect("xwm_state called without a running X11Wm")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        info!(class = %window.class(), title = %window.title(), "x11 window created");
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Override-redirect windows (X11 popups, menus) are not managed
        // by the XWM. Known limitation: they do not become scene
        // visuals in this milestone.
        debug!(class = %window.class(), "x11 override-redirect window created (unmanaged)");
    }

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // Grant the map. The wl_surface buffer commit (via the x11
        // branch of handle_commit) creates the visual.
        let _ = window.set_mapped(true);
        info!(class = %window.class(), "x11 window mapped");
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        debug!(class = %window.class(), "x11 override-redirect window mapped (unmanaged)");
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // The X client withdrew the window but may remap later: drop
        // the visual, keep the association alive.
        if let Some(vid) = self.x11_visual_for(&window) {
            self.destroy_x11_visual(vid);
            info!(?vid, "x11 window unmapped; visual removed");
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(vid) = self.x11_visual_for(&window) {
            self.destroy_x11_visual(vid);
            info!(?vid, "x11 window destroyed; visual removed");
        }
        self.x11_windows
            .retain(|_, w| w.window_id() != window.window_id());
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // Client decides geometry (I3): accept the requested values by
        // reconfiguring the X window with them.
        let current = window.geometry();
        let rect = Rectangle::new(
            smithay::utils::Point::new(x.unwrap_or(current.loc.x), y.unwrap_or(current.loc.y)),
            smithay::utils::Size::new(
                w.unwrap_or(current.size.w as u32) as i32,
                h.unwrap_or(current.size.h as u32) as i32,
            ),
        );
        let _ = window.configure(Some(rect));
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, smithay::utils::Logical>,
        _above: Option<smithay::reexports::x11rb::protocol::xproto::Window>,
    ) {
        debug!(class = %window.class(), ?geometry, "x11 window reconfigured");
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // X11 windows have no xdg coordinator: configure to the view
        // size directly and mark the state on the X window.
        let _ = window.set_maximized(true);
        let (w, h) = self.window_size;
        let _ = window.configure(Some(Rectangle::new(
            smithay::utils::Point::new(0, 0),
            smithay::utils::Size::new(w as i32, h as i32),
        )));
        info!("x11 window maximize request granted");
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_maximized(false);
        // Restore to the size hints' base size, falling back to a sane
        // default (no saved pre-maximize pose is tracked for X11).
        let (w, h) = window
            .size_hints()
            .and_then(|hints| hints.base_size.map(|b| (b.0, b.1)))
            .unwrap_or((640, 480));
        let _ = window.configure(Some(Rectangle::new(
            smithay::utils::Point::new(0, 0),
            smithay::utils::Size::new(w, h),
        )));
        info!("x11 window unmaximize request granted");
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(true);
        let (w, h) = self.window_size;
        let _ = window.configure(Some(Rectangle::new(
            smithay::utils::Point::new(0, 0),
            smithay::utils::Size::new(w as i32, h as i32),
        )));
        info!("x11 window fullscreen request granted");
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(false);
        let geometry = window.geometry();
        let _ = window.configure(Some(geometry));
        info!("x11 window unfullscreen request granted");
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // I5 semantics: hide the visual, keep the surface alive. The
        // X window stays mapped but has no visual while minimized;
        // unminimize re-maps it through the commit path.
        if let Some(vid) = self.x11_visual_for(&window) {
            self.destroy_x11_visual(vid);
        }
        info!("x11 window minimize request: visual hidden");
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // The next wl_surface commit re-creates the visual.
        let _ = window;
        info!("x11 window unminimize request");
    }

    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, _edge: ResizeEdge) {
        // Interactive X11 resize/move grabs are a known limitation of
        // this milestone; the request is logged, not dropped silently.
        let _ = window;
        debug!("x11 resize request ignored (limitation)");
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        let _ = window;
        debug!("x11 move request ignored (limitation)");
    }

    fn allow_selection_access(&mut self, _xwm: XwmId, selection: SelectionTarget) -> bool {
        // X clients may read the Wayland-side selections.
        info!(?selection, "x11 client requests selection access");
        true
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: std::os::unix::io::OwnedFd,
    ) {
        info!(?selection, ?mime_type, "x11 selection read started");
        // An X client is reading a selection. If the current owner is
        // an X client too, round-trip back through the X selection
        // owner; otherwise pull from the Wayland data-device source.
        let x11_owned = match selection {
            SelectionTarget::Clipboard => self.x11_owns_clipboard,
            SelectionTarget::Primary => self.x11_owns_primary,
        };
        if x11_owned {
            if let (Some(wm), Some(lh)) = (self.x11_wm.as_mut(), self.loop_handle.clone()) {
                if let Err(e) = wm.send_selection(selection, mime_type, fd, lh) {
                    warn!(?e, "x11 selection transfer failed");
                }
            }
            return;
        }
        if let Some(ref seat) = self.seat {
            match selection {
                SelectionTarget::Clipboard => {
                    if let Err(e) = smithay::wayland::selection::data_device::request_data_device_client_selection::<Self>(
                        seat, mime_type, fd,
                    ) {
                        warn!(?e, "x11 clipboard request found no Wayland source");
                    }
                }
                SelectionTarget::Primary => {
                    if let Err(e) = smithay::wayland::selection::primary_selection::request_primary_client_selection::<Self>(
                        seat, mime_type, fd,
                    ) {
                        warn!(?e, "x11 primary request found no Wayland source");
                    }
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        // An X client became the selection owner: publish to Wayland
        // clients via the server-side data-device/primary selection.
        match selection {
            SelectionTarget::Clipboard => self.x11_owns_clipboard = true,
            SelectionTarget::Primary => self.x11_owns_primary = true,
        }
        info!(?selection, mimes = ?mime_types, "x11 selection published to wayland clients");
        if let Some(ref seat) = self.seat {
            let dh = self.display_handle.clone();
            match selection {
                SelectionTarget::Clipboard => {
                    smithay::wayland::selection::data_device::set_data_device_selection::<Self>(
                        &dh,
                        seat,
                        mime_types,
                        SelectionOwner,
                    );
                }
                SelectionTarget::Primary => {
                    smithay::wayland::selection::primary_selection::set_primary_selection::<Self>(
                        &dh,
                        seat,
                        mime_types,
                        SelectionOwner,
                    );
                }
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => self.x11_owns_clipboard = false,
            SelectionTarget::Primary => self.x11_owns_primary = false,
        }
        if let Some(ref seat) = self.seat {
            let dh = self.display_handle.clone();
            match selection {
                SelectionTarget::Clipboard => {
                    smithay::wayland::selection::data_device::set_data_device_selection::<Self>(
                        &dh,
                        seat,
                        Vec::new(),
                        SelectionOwner,
                    );
                }
                SelectionTarget::Primary => {
                    smithay::wayland::selection::primary_selection::set_primary_selection::<Self>(
                        &dh,
                        seat,
                        Vec::new(),
                        SelectionOwner,
                    );
                }
            }
        }
        info!(?selection, "x11 selection cleared");
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        // The X server is gone: drop every X11 visual and bookkeeping.
        let vids: Vec<_> = self
            .x11_windows
            .keys()
            .filter_map(|s| self.find_vid_for_surface(s))
            .collect();
        for vid in vids {
            self.destroy_x11_visual(vid);
        }
        self.x11_windows.clear();
        self.x11_wm = None;
        self.x11_display = None;
        info!("x11 window manager disconnected; x11 visuals removed");
    }
}

delegate_xwayland_shell!(LookingGlass);
