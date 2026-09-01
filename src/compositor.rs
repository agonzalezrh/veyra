//! Wayland protocol integration and central compositor state.

use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportAll;
use smithay::backend::renderer::Texture;
use smithay::backend::SwapBuffersError;

use crate::backend::PresentationBackend;
use smithay::delegate_compositor;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::delegate_data_device;
use smithay::delegate_output;
use smithay::delegate_primary_selection;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_xdg_shell;
use smithay::backend::input::{KeyState, Keycode};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, LedState};
use smithay::input::pointer::{ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle};
use smithay::input::Seat;
use smithay::input::SeatHandler;
use smithay::input::SeatState;
use smithay::wayland::selection::data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler};
use smithay::wayland::selection::primary_selection::{PrimarySelectionHandler, PrimarySelectionState};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::{SelectionHandler, SelectionTarget};
use smithay::delegate_dmabuf;
use smithay::delegate_pointer_constraints;
use smithay::delegate_relative_pointer;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::with_states;
use smithay::wayland::compositor::BufferAssignment;
use smithay::wayland::compositor::CompositorClientState;
use smithay::wayland::compositor::CompositorHandler;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::compositor::SurfaceAttributes;
use smithay::wayland::shell::xdg::Configure;
use smithay::wayland::shell::xdg::PositionerState;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::XdgShellHandler;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use smithay::wayland::shm::ShmHandler;
use smithay::wayland::shm::ShmState;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use cgmath::Matrix4;

use crate::app_switcher::ApplicationSwitcher;
use crate::config::Config;
use crate::context_menu::{ContextMenu, MenuAction};
use crate::focus::{CameraMode, FocusManager};
use crate::input::Camera;
use crate::input_router::{self, InputSink, KeyboardEvent, PointerEventKind};
use crate::interaction::InteractionController;
use crate::launcher::Launcher;
use crate::layout;
use crate::navigation::{EscapeAction, NavigationModel};
use crate::perf::PerfStats;
use crate::recovery::Recovery;
use crate::scheduler::RenderScheduler;
use crate::session::Session;
use crate::shelf::SpatialShelf;
use crate::workspace::WorkspaceManager;
use crate::producer::{FrameProducer, FrameResult};
use crate::scene::{DamageKind, Scene, Visual, VisualContent, VisualId};
use crate::renderer;
use tracing::error;
use tracing::info;
use tracing::warn;

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        info!(?client_id, "client connected");
    }
    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        info!(?client_id, ?reason, "client disconnected");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceLifecycle {
    Created,
    Configured,
    Mapped,
    Unmapped,
    Destroyed,
}

/// Track a popup surface with its parent relationship.
#[derive(Debug, Clone)]
pub struct PopupInfo {
    pub popup: smithay::wayland::shell::xdg::PopupSurface,
    pub wl_surface: WlSurface,
    pub parent_toplevel_vid: Option<VisualId>,
    pub visual_id: Option<VisualId>,
    pub lifecycle: SurfaceLifecycle,
    pub size: Option<(i32, i32)>,
    /// The positioner state for computing popup geometry.
    pub positioner: PositionerState,
}

#[derive(Debug, Clone)]
pub struct ToplevelInfo {
    pub toplevel: ToplevelSurface,
    pub wl_surface: WlSurface,
    pub app_id: String,
    pub title: String,
    pub lifecycle: SurfaceLifecycle,
    pub visual_id: Option<VisualId>,
    pub size: Option<(i32, i32)>,
    /// I4: the client acknowledged a maximized configure. Geometry
    /// authority stays with the client; this only tracks the state.
    pub maximized: bool,
    /// I4: committed size to restore on unmaximize (captured at
    /// maximize time). None while not maximized.
    pub restore_size: Option<(i32, i32)>,
    /// I4: presentation pose to restore on unmaximize: (position xyz,
    /// rotation ijkw). Captured when the window is maximized.
    pub restore_pose: Option<((f32, f32, f32), [f32; 4])>,
    /// I5: the window is currently minimized (hidden, Wayland surface
    /// still mapped and alive). Presentation transform is untouched;
    /// layout/arrangement treat minimized visuals as detached.
    pub minimized: bool,
}

impl ToplevelInfo {
    fn new(toplevel: ToplevelSurface) -> Self {
        let wl_surface = toplevel.wl_surface().clone();
        let (title, app_id) = with_states(&wl_surface, |states| {
            let title = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|attrs| attrs.lock().unwrap().title.clone().unwrap_or_default())
                .unwrap_or_default();
            let app_id = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|attrs| attrs.lock().unwrap().app_id.clone().unwrap_or_default())
                .unwrap_or_default();
            (title, app_id)
        });
        ToplevelInfo {
            lifecycle: SurfaceLifecycle::Created,
            toplevel,
            wl_surface,
            app_id,
            title,
            visual_id: None,
            size: None,
            maximized: false,
            restore_size: None,
            restore_pose: None,
            minimized: false,
        }
    }

    fn refresh_metadata(&mut self) {
        with_states(&self.wl_surface, |states| {
            if let Some(attrs) = states.data_map.get::<XdgToplevelSurfaceData>() {
                let attrs = attrs.lock().unwrap();
                self.title = attrs.title.clone().unwrap_or_default();
                self.app_id = attrs.app_id.clone().unwrap_or_default();
            }
        });
    }
}

pub struct LookingGlass {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub seat_state: SeatState<Self>,
    /// Track the Smithay Seat handle for data device and selection operations.
    pub seat: Option<smithay::input::Seat<Self>>,
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub backend: Option<Box<dyn PresentationBackend>>,
    pub toplevels: Vec<ToplevelInfo>,
    pub popups: Vec<PopupInfo>,
    pub scene: Scene,
    pub camera: Camera,
    pub spatial_mode: bool,
    /// Spatial camera pose saved when entering normal (2D) mode: normal
    /// pins the camera to the ortho pose for 1:1 world↔screen mapping;
    /// re-entering spatial restores this pose instead of starting from
    /// the ortho distance (which made windows appear hugely zoomed).
    spatial_cam_pose: Option<(cgmath::Point3<f32>, f32, f32)>,
    /// One-shot spatial camera fit: at startup the camera distance must
    /// cover the workspace view (the perspective frustum at z=0 must
    /// include the full ortho rectangle, otherwise windows placed along
    /// the spiral are invisible in spatial mode).
    spatial_cam_adapted: bool,
    pub workspace_manager: WorkspaceManager,
    /// Registered frame producers
    producers: Vec<(VisualId, Box<dyn FrameProducer>)>,
    pub perf: PerfStats,
    pub output: Option<Output>,
    pub window_size: (f32, f32),
    pub last_mouse: (f64, f64),
    pub last_dx: f64,
    pub last_dy: f64,
    pub press_pos: (f64, f64),
    pub nav_button: u32,
    pub event_serial: u32,
    pub last_down_vid: Option<VisualId>,
    // auto_orbit is now per-workspace via workspace_manager.active().auto_orbit
    pub saved_state: Option<crate::persist::WorkspaceState>,
    pub focus_manager: FocusManager,
    pub interaction: InteractionController,
    input_sinks: HashMap<VisualId, Box<dyn InputSink>>,
    /// Track Wayland WlSurface per VisualId for direct seat input.
    pub wayland_surfaces: HashMap<VisualId, WlSurface>,
    pub pointer_handle: Option<PointerHandle<Self>>,
    pub keyboard_handle: Option<KeyboardHandle<Self>>,
    /// Track the last Wayland surface that received pointer focus
    /// for proper enter/leave event sequencing.
    last_wayland_focus: Option<WlSurface>,
    /// Render scheduling (dirty/animating state instead of fixed 16ms timer).
    pub scheduler: RenderScheduler,
    /// Modifier key state for keyboard shortcuts.
    ctrl_pressed: bool,
    shift_pressed: bool,
    alt_pressed: bool,
    meta_pressed: bool,
    /// Application switcher (Alt+Tab).
    pub app_switcher: ApplicationSwitcher,
    /// Launcher (desktop file based application launcher).
    pub launcher: Launcher,
    /// Spatial shelf (de-emphasized visuals at bottom of workspace).
    pub shelf: SpatialShelf,
    /// Navigation model (key binding dispatch).
    pub navigation: NavigationModel,
    /// Alt+Tab was active (releasing Alt commits selection).
    alt_tab_active: bool,
    /// Key whose press was consumed by the compositor (binding or context
    /// menu); its release must be swallowed instead of leaking an unpaired
    /// release to the focused client.
    swallow_release: Option<u32>,
    /// Context menu (right-click popup).
    pub context_menu: ContextMenu,
    /// Configuration (loaded at startup, no live reload).
    pub config: Config,
    /// Session lifecycle management.
    pub session: Session,
    /// Recovery operations for destroyed focus, corrupt state, etc.
    pub recovery: Recovery,
    /// Pointer constraints (lock/confine) state.
    pub pointer_constraints: crate::pointer_constraints::PointerConstraints,
    /// Tombstones of recently closed windows for reopen support (I1).
    pub closed_windows: crate::closed::ClosedWindowHistory,
    /// A reopen in progress: waiting for the relaunched app to map.
    pub pending_reopen: Option<crate::closed::PendingReopen>,
    /// Veyra-owned intent for client geometry changes (I3a). Protocol
    /// state (configure serials, ACKs) remains owned by Smithay.
    pub client_resizes: crate::client_resize::ClientResizeCoordinator,
    /// Veyra-owned intent for maximize transitions (I4). The client's
    /// configured/committed geometry changes; the spatial transform never
    /// does (see crate::maximize).
    pub maximize: crate::maximize::MaximizeCoordinator,
    /// In-progress pointer resize session (I3b), None when idle.
    pub resize_session: Option<crate::resize::ResizeSession>,
    /// Relative pointer manager for sending relative motion deltas.
    pub relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState,
    /// DMA-BUF buffer import state.
    pub dmabuf_manager: crate::dmabuf::DmabufManager,
}

/// Result of routing a pointer event to the selected visual's content.
enum ContentRouting { Routed, TitleBarHit, NoTarget }

/// Monotonic milliseconds timestamp for input events.
fn now_ms() -> u32 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32
}

impl LookingGlass {
    pub fn new(
        display_handle: &DisplayHandle,
        backend: Box<dyn PresentationBackend>,
        config: Config,
    ) -> Self {
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(display_handle);
        let shm_state = ShmState::new::<Self>(display_handle, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(display_handle);
        let mut seat_state = SeatState::new();
        let primary_selection_state = PrimarySelectionState::new::<Self>(display_handle);

        // Create a seat and pointer/keyboard handles for Wayland input routing
        // Use new_wl_seat to register the wl_seat global (new_seat doesn't register it)
        let mut seat_actual = seat_state.new_wl_seat(display_handle, "default");
        let seat_handle = seat_actual.clone();
        let pointer_handle = Some(seat_actual.add_pointer());
        // Load system keyboard configuration for proper non-US layout support
        let xkb_config = load_system_xkb_config();
        info!(layout = %xkb_config.layout, "keyboard: using xkb config");
        let keyboard_result = seat_actual.add_keyboard(xkb_config, 250, 50);
        if let Err(e) = &keyboard_result {
            warn!(?e, "keyboard setup failed (xkb keymap may not be loaded)");
        }
        let keyboard_handle = seat_actual.get_keyboard();

        // Create a wl_output global so clients see a monitor
        let output = Output::new(
            "LG-NG".into(),
            PhysicalProperties {
                size: (338, 270).into(),
                subpixel: Subpixel::Unknown,
                make: "Veyra".into(),
                model: "Display".into(),
            },
        );
        let _output_global = output.create_global::<Self>(display_handle);
        output.change_current_state(
            Some(Mode { size: (1280, 720).into(), refresh: 60000 }),
            None,
            Some(Scale::Integer(1)),
            None,
        );
        output.set_preferred(Mode {
            size: (1280, 720).into(),
            refresh: 60000,
        });

        LookingGlass {
            display_handle: display_handle.clone(),
            compositor_state,
            xdg_shell_state,
            seat_state,
            shm_state,
            data_device_state,
            primary_selection_state,
            seat: Some(seat_handle),
            backend: Some(backend),
            toplevels: Vec::new(),
            popups: Vec::new(),
            scene: Scene::default(),
            camera: Camera::new(),
            spatial_mode: true,
            spatial_cam_pose: None,
            spatial_cam_adapted: false,
            workspace_manager: WorkspaceManager::new(config.workspace.count),
            producers: Vec::new(),
            perf: PerfStats::new(),
            output: None,
            window_size: (1280.0, 720.0),
            last_mouse: (0.0, 0.0),
            last_dx: 0.0,
            last_dy: 0.0,
            press_pos: (0.0, 0.0),
            nav_button: 0,
            event_serial: 0,
            last_down_vid: None,
            saved_state: None,
            focus_manager: FocusManager::new(),
            interaction: InteractionController::new(),
            input_sinks: HashMap::new(),
            wayland_surfaces: HashMap::new(),
            pointer_handle,
            keyboard_handle,
            last_wayland_focus: None,
            ctrl_pressed: false,
            shift_pressed: false,
            alt_pressed: false,
            meta_pressed: false,
            app_switcher: ApplicationSwitcher::new(),
            launcher: Launcher::new(),
            shelf: SpatialShelf::new(),
            navigation: NavigationModel::new(),
            alt_tab_active: false,
            swallow_release: None,
            context_menu: ContextMenu::new(),
            config: config.clone(),
            session: Session::new(config.clone()),
            recovery: Recovery::new(),
            scheduler: RenderScheduler::new(),
            pointer_constraints: crate::pointer_constraints::PointerConstraints::new(display_handle),
            closed_windows: crate::closed::ClosedWindowHistory::new(10),
            pending_reopen: None,
            client_resizes: crate::client_resize::ClientResizeCoordinator::default(),
            maximize: crate::maximize::MaximizeCoordinator::default(),
            resize_session: None,
            relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState::new::<LookingGlass>(display_handle),
            dmabuf_manager: crate::dmabuf::DmabufManager::new(display_handle),
        }
    }

    /// Load saved workspace state from disk and apply to workspaces.
    ///
    /// Precedence:
    /// 1. Built-in defaults (from Config)
    /// 2. Config file overrides
    /// 3. Saved state (overrides config for runtime values like camera/transforms)
    ///
    /// This method applies saved state for each workspace (camera, layout mode)
    /// and stores visual state for later remapping when surfaces appear.
    pub fn load_saved_state(&mut self) {
        use crate::layout::LayoutMode;

        if !crate::persist::exists() {
            info!("no saved workspace state found, using config defaults");
            return;
        }

        match crate::persist::load() {
            Ok(state) => {
                let count = state.workspace_count();
                info!(workspaces = count, "workspace state loaded");

                // Apply per-workspace state: camera and layout mode
                for i in 0..count {
                    if let Some(ws_entry) = state.workspace(i) {
                        if let Some(ws) = self.workspace_manager.get_mut(i) {
                            // Restore workspace camera (overrides config default)
                            ws.camera.position.x = ws_entry.camera.x;
                            ws.camera.position.y = ws_entry.camera.y;
                            ws.camera.position.z = ws_entry.camera.z;
                            ws.camera.yaw = ws_entry.camera.yaw;
                            ws.camera.pitch = ws_entry.camera.pitch;

                            // Restore layout mode from state, fall back to config default
                            ws.layout_mode = match ws_entry.layout_mode.as_str() {
                                "flat" => LayoutMode::Flat,
                                s if s.starts_with("grid:") => {
                                    let cols = s[5..].parse().unwrap_or(3);
                                    LayoutMode::Grid { columns: cols }
                                }
                                _ => LayoutMode::Freeform,
                            };
                        }
                    }
                }

                // Apply camera from first workspace to the compositor's active camera
                if let Some(first) = state.workspace(0) {
                    self.camera.position.x = first.camera.x;
                    self.camera.position.y = first.camera.y;
                    self.camera.position.z = first.camera.z;
                    self.camera.yaw = first.camera.yaw;
                    self.camera.pitch = first.camera.pitch;
                }

                // Store saved visual state for surface remapping
                self.saved_state = Some(state);

                // Validate version mismatch
                if self.saved_state.as_ref().map_or(false, |s| s.version > crate::persist::CURRENT_VERSION) {
                    warn!("saved state version {} > current version {}, attempt load",
                        self.saved_state.as_ref().unwrap().version,
                        crate::persist::CURRENT_VERSION);
                }
            }
            Err(e) => {
                // Corrupt state: back up file and start fresh
                if crate::persist::exists() {
                    warn!(?e, "corrupt saved state, backing up and starting fresh");
                    crate::persist::backup();
                } else {
                    info!(?e, "no saved workspace state to load");
                }
            }
        }
    }

    /// Save current workspace state to disk (multi-workspace).
    pub fn save_state(&self) {
        // Collect workspace data
        let n = self.workspace_manager.len();
        let mut ws_visuals: Vec<Vec<VisualId>> = Vec::with_capacity(n);
        let mut ws_cameras: Vec<Camera> = Vec::with_capacity(n);
        let mut ws_layouts: Vec<crate::layout::LayoutMode> = Vec::with_capacity(n);
        let mut ws_detached: Vec<Vec<VisualId>> = Vec::with_capacity(n);

        for i in 0..n {
            if let Some(ws) = self.workspace_manager.get(i) {
                ws_visuals.push(ws.visual_ids.clone());
                ws_cameras.push(ws.camera.clone());
                ws_layouts.push(ws.layout_mode);
                ws_detached.push(ws.detached_set.clone());
            }
        }

        let state = crate::persist::WorkspaceState::capture_multi(
            &self.scene,
            &self.camera,
            &ws_visuals,
            &ws_cameras,
            &ws_layouts,
            &ws_detached,
        );
        match crate::persist::save(&state) {
            Ok(()) => info!("multi-workspace state saved"),
            Err(e) => warn!(?e, "failed to save workspace state"),
        }
    }

    pub fn cleanup(&mut self) {
        self.toplevels.retain(|t| t.toplevel.alive());
    }

    fn find_toplevel(&mut self, surface: &WlSurface) -> Option<&mut ToplevelInfo> {
        self.toplevels
            .iter_mut()
            .find(|t| t.toplevel.wl_surface() == surface)
    }

    fn find_surface_visual_id(&self, surface: &WlSurface) -> Option<VisualId> {
        // Check toplevels
        for t in &self.toplevels {
            if t.toplevel.wl_surface() == surface {
                return t.visual_id;
            }
        }
        // Check popups
        for p in &self.popups {
            if p.wl_surface == *surface {
                return p.visual_id;
            }
        }
        None
    }

    /// Clamp client damage rectangles to the last committed buffer size.
    ///
    /// Clients occasionally race their own resize: damage is reported in
    /// the coordinates of the previous buffer while a smaller/larger
    /// buffer is committed. smithay's shm import uploads damage regions
    /// unclamped, so out-of-bounds rects produce GL_INVALID_VALUE spam
    /// and a visibly stuck window (the upload fails each frame). Damage
    /// that exceeds the buffer is clamped or dropped; with no known size
    /// the import becomes a full upload (damage empty).
    fn sanitize_damage(
        damage: Vec<smithay::utils::Rectangle<i32, smithay::utils::Buffer>>,
        last_size: Option<(i32, i32)>,
    ) -> Vec<smithay::utils::Rectangle<i32, smithay::utils::Buffer>> {
        use smithay::utils::{Point, Rectangle, Size};
        let Some((w, h)) = last_size else { return Vec::new() };
        let buf = Rectangle::new(Point::new(0, 0), Size::new(w, h));
        damage.into_iter()
            .filter_map(|r| r.intersection(buf))
            .filter(|r| r.size.w > 0 && r.size.h > 0)
            .collect()
    }

    fn handle_commit(&mut self, surface: &WlSurface) {
        // Determine if this is a toplevel or popup commit
        let is_popup = self.popups.iter().any(|p| p.wl_surface == *surface);

        let existing_vid = self.find_surface_visual_id(surface);

        // Find the lifecycle state for this surface
        let lifecycle = if is_popup {
            self.popups.iter().find(|p| p.wl_surface == *surface)
                .map(|p| p.lifecycle)
                .unwrap_or(SurfaceLifecycle::Destroyed)
        } else {
            self.toplevels.iter().find(|t| t.toplevel.wl_surface() == surface)
                .map(|t| t.lifecycle)
                .unwrap_or(SurfaceLifecycle::Destroyed)
        };

        if lifecycle == SurfaceLifecycle::Destroyed {
            return;
        }

        let is_first_map = lifecycle != SurfaceLifecycle::Mapped;
        let is_remap = is_first_map && existing_vid.is_some();

        // Extract buffer + damage (shared path for toplevels and popups)
        let (wl_buffer, damage): (Option<_>, Vec<_>) = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let attrs = cached.current();
            let buf = match &attrs.buffer {
                Some(BufferAssignment::NewBuffer(b)) => Some(b.clone()),
                _ => None,
            };
            let dmg = attrs.damage.iter().filter_map(|d| match d {
                smithay::wayland::compositor::Damage::Buffer(r) => Some(*r),
                smithay::wayland::compositor::Damage::Surface(r) => {
                    let bs = attrs.buffer_scale.max(1);
                    Some(smithay::utils::Rectangle::new(
                        smithay::utils::Point::new(r.loc.x * bs, r.loc.y * bs),
                        smithay::utils::Size::new(r.size.w * bs, r.size.h * bs),
                    ))
                }
            }).collect();
            (buf, dmg)
        });
        let Some(wl_buffer) = wl_buffer else { return };
        // Damage must fit the buffer being imported: race-resized clients
        // report damage in previous-buffer coordinates (see sanitize_damage).
        let last_size = if is_popup {
            self.popups.iter().find(|p| p.wl_surface == *surface).and_then(|p| p.size)
        } else {
            self.toplevels.iter().find(|t| t.toplevel.wl_surface() == surface).and_then(|t| t.size)
        };
        let damage = Self::sanitize_damage(damage, last_size);

        if let Some(backend) = self.backend.as_mut() {
            let renderer = backend.renderer();
            // Use ImportAll::import_buffer to handle SHM, EGL, and DMA-BUF buffers
            let result = with_states(surface, |states| {
                renderer.import_buffer(&wl_buffer, Some(states), &damage)
            });
            match result {
                Some(Ok(texture)) => {
                    use smithay::backend::renderer::Texture;
                    if is_first_map {
                        let tex_size = texture.size();

                        if is_popup {
                            // ── Popup commit ──
                            let popup_idx = self.popups.iter()
                                .position(|p| p.wl_surface == *surface)
                                .unwrap();
                            self.popups[popup_idx].lifecycle = SurfaceLifecycle::Mapped;
                            self.popups[popup_idx].size = Some((tex_size.w, tex_size.h));

                            if is_remap {
                                if let Some(vid) = existing_vid {
                                    // Compute position before mutable borrow of scene
                                    let new_pos = self.popups[popup_idx].positioner.get_geometry();
                                    let parent_pos = self.popups[popup_idx].parent_toplevel_vid
                                        .and_then(|pvid| {
                                            self.scene.visuals.iter().find(|v| v.id == pvid).map(|parent| {
                                                (parent.transform.position, parent.total_width(), parent.total_height())
                                            })
                                        });
                                    if let Some(visual) = self.scene.get_mut(vid) {
                                        if let Some(dst) = visual.texture_mut() {
                                            *dst = texture;
                                        }
                                        visual.geometry = smithay::utils::Rectangle::new(
                                            smithay::utils::Point::new(0, 0),
                                            smithay::utils::Size::new(tex_size.w, tex_size.h),
                                        );
                                        // Recompute position from updated positioner
                                        if let Some((p_pos, p_total_w, p_total_h)) = parent_pos {
                                            let popup_w = new_pos.size.w as f32;
                                            let popup_h = new_pos.size.h as f32;
                                            let local_x = new_pos.loc.x as f32 + popup_w * 0.5 - p_total_w * 0.5;
                                            let local_y = -(new_pos.loc.y as f32 + popup_h * 0.5 - p_total_h * 0.5);
                                            visual.transform.position = p_pos
                                                + cgmath::Vector3::new(local_x, local_y, 10.0);
                                        }
                                        self.workspace_manager.active_mut().add(vid);
                                        info!(?vid, "popup remapped");
                                    }
                                }
                            } else {
                                let parent_vid = self.popups[popup_idx].parent_toplevel_vid;
                                let positioner = self.popups[popup_idx].positioner;
                                // Compute popup position from xdg_positioner before creating visual
                                let popup_geometry = positioner.get_geometry();
                                // Find parent position info before mutable borrow
                                let parent_info = parent_vid.and_then(|pvid| {
                                    self.scene.visuals.iter().find(|v| v.id == pvid).map(|parent| {
                                        (parent.transform.position, parent.total_width(), parent.total_height(), pvid)
                                    })
                                });
                                let mut visual = Visual::new(
                                    VisualContent::WaylandSurface(texture),
                                    smithay::utils::Rectangle::new(
                                        smithay::utils::Point::new(0, 0),
                                        smithay::utils::Size::new(tex_size.w, tex_size.h),
                                    ),
                                );
                                if let Some((p_pos, p_total_w, p_total_h, pvid)) = parent_info {
                                    let popup_w = popup_geometry.size.w as f32;
                                    let popup_h = popup_geometry.size.h as f32;
                                    let local_x = popup_geometry.loc.x as f32 + popup_w * 0.5 - p_total_w * 0.5;
                                    let local_y = -(popup_geometry.loc.y as f32 + popup_h * 0.5 - p_total_h * 0.5);
                                    visual.transform.position = p_pos
                                        + cgmath::Vector3::new(local_x, local_y, 10.0);
                                    visual.parent = Some(pvid);
                                }
                                let visual_id = visual.id;
                                self.popups[popup_idx].visual_id = Some(visual_id);
                                self.wayland_surfaces.insert(visual_id, surface.clone());
                                self.scene.add(visual);
                                // Add to the same workspace as the parent (or active workspace)
                                if let Some((_, _, _, pvid)) = parent_info {
                                    for i in 0..self.workspace_manager.len() {
                                        if let Some(ws) = self.workspace_manager.get_mut(i) {
                                            if ws.visual_ids.contains(&pvid) {
                                                ws.add(visual_id);
                                                break;
                                            }
                                        }
                                    }
                                } else {
                                    self.workspace_manager.active_mut().add(visual_id);
                                }
                                info!(?visual_id, ?parent_vid, ?popup_geometry, "popup mapped");
                            }
                        } else {
                            // ── Toplevel commit ──
                            let idx = self.toplevels.iter()
                                .position(|t| t.toplevel.wl_surface() == surface)
                                .unwrap();
                            self.toplevels[idx].lifecycle = SurfaceLifecycle::Mapped;
                            self.toplevels[idx].size = Some((tex_size.w, tex_size.h));

                            if is_remap {
                                if let Some(vid) = existing_vid {
                                    if let Some(visual) = self.scene.get_mut(vid) {
                                        if let Some(dst) = visual.texture_mut() {
                                            *dst = texture;
                                        }
                                        visual.geometry = smithay::utils::Rectangle::new(
                                            smithay::utils::Point::new(0, 0),
                                            smithay::utils::Size::new(tex_size.w, tex_size.h),
                                        );
                                        self.workspace_manager.active_mut().add(vid);
                                        info!(?vid, app_id = %self.toplevels[idx].app_id, "surface remapped");
                                    }
                                }
                            } else {
                                let z_off = [-200.0, 0.0, 200.0];
                                let y_ang = [5.0, 0.0, -5.0];
                                let n = self.toplevels.len();
                                let angle_y = if idx < y_ang.len() { y_ang[idx] } else { 0.0 };
                                let mut visual = Visual::new(
                                    VisualContent::WaylandSurface(texture),
                                    smithay::utils::Rectangle::new(
                                        smithay::utils::Point::new(0, 0),
                                        smithay::utils::Size::new(tex_size.w, tex_size.h),
                                    ),
                                );
                                use cgmath::Deg;
                                use cgmath::Rotation3;
                                visual.chrome.title = self.toplevels[idx].title.clone();
                                visual.chrome.app_id = self.toplevels[idx].app_id.clone();
                                let app_id = &self.toplevels[idx].app_id;
                                // Pending reopen (I1): reattach saved transform
                                // when the relaunched app's toplevel maps.
                                let mut reopened: Option<crate::closed::PendingReopen> = None;
                                if self.pending_reopen.as_ref().map_or(false, |pr| pr.app_id == *app_id) {
                                    reopened = self.pending_reopen.take();
                                    if let Some(pr) = &reopened {
                                        visual.transform = pr.transform.clone();
                                        info!(app_id = %app_id, workspace = pr.workspace, "pending reopen applied");
                                    }
                                }
                                let restored = self.saved_state.as_ref().and_then(|s| {
                                    s.find_visual(app_id).map(|(_, vs)| {
                                        visual.transform.position.x = vs.x;
                                        visual.transform.position.y = vs.y;
                                        visual.transform.position.z = vs.z;
                                        visual.transform.rotation.s = vs.rotation[0];
                                        visual.transform.rotation.v.x = vs.rotation[1];
                                        visual.transform.rotation.v.y = vs.rotation[2];
                                        visual.transform.rotation.v.z = vs.rotation[3];
                                        visual.transform.scale.x = vs.scale[0];
                                        visual.transform.scale.y = vs.scale[1];
                                        visual.transform.scale.z = vs.scale[2];
                                        if vs.detached {
                                            self.scene.detached_set.push(visual.id);
                                        }
                                    })
                                });
                                if restored.is_none() && reopened.is_none() {
                                    let pos = layout::place_new_visual(
                                        tex_size.w as f32 * visual.transform.scale.x,
                                        tex_size.h as f32 * visual.transform.scale.y,
                                        &self.scene,
                                    );
                                    visual.transform.position = pos;
                                    visual.transform.rotation = cgmath::Quaternion::from_angle_y(Deg(angle_y));
                                }
                                let visual_id = visual.id;
                                let map_pos = visual.transform.position;
                                let map_rot = visual.transform.rotation;
                                let map_total_w = visual.total_width();
                                let map_total_h = visual.total_height();
                                let map_scale = visual.transform.scale;
                                let reopen_workspace = reopened.as_ref().map(|pr| pr.workspace);
                                self.toplevels[idx].visual_id = Some(visual_id);
                                self.wayland_surfaces.insert(visual_id, surface.clone());
                                self.scene.add(visual);
                                self.workspace_manager.active_mut().add(visual_id);
                                // Reopen targets a specific workspace: move the
                                // visual there if it differs from the active one.
                                if let Some(ws_idx) = reopen_workspace {
                                    if ws_idx < self.workspace_manager.len() && ws_idx != self.workspace_manager.active_id() {
                                        if let Some(ws) = self.workspace_manager.get_mut(ws_idx) {
                                            ws.add(visual_id);
                                        }
                                        self.workspace_manager.active_mut().remove(visual_id);
                                        info!(?visual_id, workspace = ws_idx, "reopened window restored to workspace");
                                    }
                                }
                                self.scene.focus(Some(visual_id));
                                self.app_switcher.register_visual(
                                    &self.toplevels[idx].app_id,
                                    visual_id,
                                );
                                info!(?visual_id, app_id = %self.toplevels[idx].app_id,
                                       pos = ?map_pos,
                                       rot = ?map_rot,
                                       total_w = map_total_w,
                                       total_h = map_total_h,
                                       scale = ?map_scale,
                                       "surface mapped");
                            }
                        }
                    } else if let Some(vid) = existing_vid {
                        use smithay::backend::renderer::Texture;
                        let tex_size = texture.size();
                        // Resolve any outstanding geometry request (I3a).
                        // A mismatched buffer means the client overrode us —
                        // committed geometry always wins.
                        let outcome = self.client_resizes.note_commit(vid, (tex_size.w as i32, tex_size.h as i32));
                        match outcome {
                            crate::client_resize::CommitOutcome::Fulfilled => {
                                info!(?vid, w = tex_size.w, h = tex_size.h, "client resize fulfilled");
                                // Client pacing (I3b): continue with the
                                // latest desired size, if the session moved on.
                                self.flush_resize_desired(vid);
                            }
                            crate::client_resize::CommitOutcome::ClientOverride => {
                                info!(?vid, w = tex_size.w, h = tex_size.h, "client overrode requested size; adopting committed geometry");
                            }
                            crate::client_resize::CommitOutcome::NotResizing => {}
                        }
                        // I4: a committed buffer completes any outstanding
                        // maximize/unmaximize transition for this surface.
                        self.complete_maximize_intent(vid, (tex_size.w as i32, tex_size.h as i32));
                        if let Some(visual) = self.scene.get_mut(vid) {
                            if let Some(dst) = visual.texture_mut() {
                                *dst = texture;
                            }
                            // Adopt committed buffer dimensions: the client
                            // decides geometry. Transform (position/rotation/
                            // scale) is spatial state and is never touched.
                            if visual.geometry.size.w != tex_size.w as i32
                                || visual.geometry.size.h != tex_size.h as i32
                            {
                                visual.geometry = smithay::utils::Rectangle::new(
                                    smithay::utils::Point::new(0, 0),
                                    smithay::utils::Size::new(tex_size.w as i32, tex_size.h as i32),
                                );
                                info!(?vid, w = tex_size.w, h = tex_size.h, "visual geometry adopted from client buffer");
                            }
                            visual.damage = DamageKind::Content;
                        }
                    }
                }
                Some(Err(e)) => warn!(?e, "buffer import failed"),
                None => warn!("buffer type not recognized by renderer"),
            }
            // After processing a commit, schedule a render so any pending
            // frame callbacks are completed promptly. Without this, the
            // client waits for callback.done() before rendering the next
            // frame, creating a latency bubble.
            self.schedule_render();
        }
    }

    /// Create a visual from external (non-Wayland) pixel data.
    pub fn add_external_visual(&mut self, pixels: Vec<u8>, width: u32, height: u32) {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::renderer::ImportMem;

        let Some(backend) = self.backend.as_mut() else { return };
        let renderer = backend.renderer();
        if let Ok(texture) = renderer.import_memory(
            &pixels,
            Fourcc::Abgr8888,
            (width as i32, height as i32).into(),
            false,
        ) {
            let visual = Visual::new(
                VisualContent::ExternalTexture(texture),
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::new(0, 0),
                    smithay::utils::Size::new(width as i32, height as i32),
                ),
            );
            info!(visual_id = ?visual.id, width, height, "external visual created");
            self.scene.add(visual);
        }
    }

    /// Add a benchmark visual at a grid position
    /// Register an InputSink for a visual.
    pub fn register_input_sink(&mut self, vid: VisualId, sink: Box<dyn InputSink>) {
        self.input_sinks.insert(vid, sink);
        info!(?vid, "input sink registered");
    }

    pub fn add_benchmark_visual(&mut self, mut producer: Box<dyn FrameProducer>, index: usize, total: usize) {
        let Some(backend) = self.backend.as_mut() else { return };
        let renderer = backend.renderer();
        if !matches!(producer.update(renderer), FrameResult::Unchanged) { return; }
        let (w, h) = producer.size();
        let cols = (total as f32).sqrt().ceil() as i32;
        let spacing = 160;
        let gx = (index as i32 % cols) * spacing - (cols * spacing) / 2;
        let gy = (index as i32 / cols) * spacing - (total as i32 / cols * spacing) / 2;

        let mut visual = Visual::new(
            VisualContent::ExternalTexture(producer.texture().clone()),
            smithay::utils::Rectangle::new(
                smithay::utils::Point::new(0, 0),
                smithay::utils::Size::new(w as i32, h as i32),
            ),
        );
        use cgmath::Deg;
        use cgmath::Rotation3;
        visual.transform.position = cgmath::Vector3::new(gx as f32, gy as f32, 0.0);
        // Rotate odd rows slightly for 3D variety
        if (index / cols as usize) % 2 == 1 {
            visual.transform.rotation = cgmath::Quaternion::from_angle_y(Deg(10.0));
        }
        let vid = visual.id;
        self.scene.add(visual);
        self.producers.push((vid, producer));
    }

    /// Register a frame producer and create its Visual in the scene.
    /// If the producer fails on its first update, it is not added.
    /// Returns the VisualId if the producer was registered successfully.
    pub fn add_producer(&mut self, mut producer: Box<dyn FrameProducer>) -> Option<VisualId> {
        let Some(backend) = self.backend.as_mut() else { return None };
        let renderer = backend.renderer();
        let result = producer.update(renderer);
        let is_ok = matches!(result, FrameResult::Updated | FrameResult::Unchanged | FrameResult::Resized(_, _));
        if !is_ok {
            match result {
                FrameResult::Error(msg) => warn!(?msg, "frame producer not added: initial update failed"),
                FrameResult::Finished => info!("frame producer finished before registration"),
                _ => {}
            }
            return None;
        }

        let (w, h) = producer.size();
        let tex = producer.texture().clone();
        let mut visual = Visual::new(
            VisualContent::ExternalTexture(tex),
            smithay::utils::Rectangle::new(
                smithay::utils::Point::new(0, 0),
                smithay::utils::Size::new(w as i32, h as i32),
            ),
        );
        visual.transform.position = layout::place_new_visual(w as f32, h as f32, &self.scene);
        let vid = visual.id;

        // Try to create an InputSink from the producer before moving it
        if let Some(sink) = producer.create_input_sink() {
            self.input_sinks.insert(vid, sink);
            info!(?vid, "input sink registered from producer");
        }

        self.scene.add(visual);
        self.workspace_manager.active_mut().add(vid);
        self.producers.push((vid, producer));
        info!(visual_id = ?vid, width = w, height = h, "frame producer registered");
        Some(vid)
    }

    /// Schedule a render and record the request in perf stats.
    pub fn schedule_render(&mut self) {
        self.perf.record_requested();
        self.scheduler.schedule_render();
    }

    pub fn render(&mut self) {
        use crate::perf::PipelineStage;

        // Always render to complete pending wl_surface.frame callbacks.
        // If nothing changed, begin_frame/render_scene/finish_frame are
        // still required to send callback.done() to waiting clients.
        // Without this, foot and other clients stall waiting for callbacks.

        // Clear stale focus: if the focused visual has been destroyed, clean up
        self.clear_stale_focus();
        self.perf.record_rendered();
        self.scheduler.clear();

        let t_frame = std::time::Instant::now();
        self.perf.begin_frame();

        // Step 1: Update frame producers (measure each)
        let mut updates: Vec<(VisualId, GlesTexture)> = Vec::new();
        {
            let backend = match self.backend.as_mut() {
                Some(b) => b,
                None => return,
            };
            let renderer = backend.renderer();
            let mut i = 0;
            while i < self.producers.len() {
                let (vid, producer) = &mut self.producers[i];
                let t0 = std::time::Instant::now();
                let result = producer.update(renderer);
                let dt = t0.elapsed().as_nanos() as u64;
                match result {
                    FrameResult::Updated => {
                        self.perf.record_stage(PipelineStage::ProducerUpdate, dt);
                        updates.push((*vid, producer.texture().clone()));
                        i += 1;
                    }
                    FrameResult::Unchanged => {
                        self.perf.record_stage(PipelineStage::ProducerUpdate, dt);
                        self.perf.record_dropped();
                        i += 1;
                    }
                    FrameResult::Resized(w, h) => {
                        // Update visual geometry to match new framebuffer size.
                        // The transform.scale is NOT modified — it's the user's spatial scale.
                        if let Some(visual) = self.scene.get_mut(*vid) {
                            visual.geometry = smithay::utils::Rectangle::new(
                                smithay::utils::Point::new(0, 0),
                                smithay::utils::Size::new(w as i32, h as i32),
                            );
                            info!(?vid, new_w = w, new_h = h, "visual resized");
                        }
                        self.perf.record_stage(PipelineStage::ProducerUpdate, dt);
                        updates.push((*vid, producer.texture().clone()));
                        i += 1;
                    }
                    FrameResult::Error(msg) => {
                        warn!(?vid, ?msg, "producer error");
                        i += 1;
                    }
                    FrameResult::Finished => {
                        info!(?vid, "producer finished, disconnecting visual");
                        self.scene.disconnect(*vid);
                        self.producers.swap_remove(i);
                    }
                }
            }
        }

        // Step 2: Copy updated textures to Visuals
        let t_tex_start = std::time::Instant::now();
        for (vid, tex) in &updates {
            if let Some(visual) = self.scene.get_mut(*vid) {
                if let Some(dst) = visual.texture_mut() {
                    *dst = tex.clone();
                }
            }
        }
        self.perf.record_stage(PipelineStage::TexCopy, t_tex_start.elapsed().as_nanos() as u64);

        // Step 3: Apply layout
        let (world_w, world_h) = self.window_size;
        let detached = self.layout_detached();
        // Layout only speaks for the active workspace (audit: foreign
        // workspace transforms must not be rearranged every frame).
        let eligible = self.workspace_manager.active().visual_ids.clone();
        let layout_mode = self.workspace_manager.active().layout_mode;
        layout::apply_layout(
            &mut self.scene,
            layout_mode,
            &layout::LayoutConfig::default(),
            &detached,
            world_w,
            world_h,
            &eligible,
        );

        // Apply shelf transforms to shelved visuals (overrides layout)
        if self.shelf.visible {
            self.shelf.apply_shelf_transforms(&mut self.scene);
        }

        // Step 4: Camera + render
        let back: &mut dyn PresentationBackend = match self.backend.as_mut() {
            Some(b) => b.as_mut(),
            None => return,
        };
        if !self.spatial_mode {
            self.camera.position = cgmath::Point3::new(0.0, 0.0, 500.0);
            self.camera.yaw = 0.0;
            self.camera.pitch = 0.0;
        } else if self.workspace_manager.active().auto_orbit {
            let t = (self.perf.frame_count as f32) * 0.003;
            self.camera.yaw = t.cos() * 0.8;
            self.camera.pitch = (t * 0.5).sin() * 0.3 + 0.2;
        } else if !self.spatial_cam_adapted && self.spatial_cam_pose.is_none()
                  && self.focus_manager.transition.is_none() {
            // One-shot frustum fit: with fov_y=45° and aspect w/h, a
            // camera at distance 1.2071*h sees exactly the ortho view
            // rectangle (±w/2, ±h/2) on the z=0 plane. Camera::new's
            // fixed z=800 leaves windows along the placement spiral
            // outside the frustum (invisible in spatial mode).
            self.spatial_cam_adapted = true;
            let d = (self.window_size.1 * 1.2071f32).max(600.0);
            self.camera.position = cgmath::Point3::new(0.0, 0.0, d);
            self.camera.yaw = 0.0;
            self.camera.pitch = 0.0;
            info!(distance = d, "spatial camera fitted to view");
        }
        // Focus/overview mode interpolates the camera toward the target
        let render_camera = self.focus_manager.interpolated_camera(&self.camera, &self.scene);
        let (w, h) = self.window_size;
        let view = render_camera.view_matrix();
        let proj = if self.spatial_mode {
            cgmath::perspective(cgmath::Deg(45.0), w / h, 1.0, 10000.0)
        } else {
            cgmath::ortho(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        };
        // In workspace overview mode, show all workspaces' visuals
        let ws_visible = match self.focus_manager.camera_mode {
            CameraMode::WorkspaceOverview => None, // show all
            _ => Some(self.workspace_manager.active().visual_ids.as_slice()),
        };
        // Keep animating if focus/overview transition is active
        if self.focus_manager.transition.is_some() {
            self.scheduler.set_animating(true);
        } else if self.workspace_manager.active().auto_orbit {
            self.scheduler.set_animating(true);
        } else {
            self.scheduler.set_animating(false);
        }
        let context_menu = if self.context_menu.visible { Some(&self.context_menu) } else { None };
        // Bind the EGL surface before rendering (makes rendering context current)
        if let Err(e) = back.begin_frame() {
            error!(?e, "begin_frame failed");
            self.scheduler.clear();
            self.perf.record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
            self.perf.record_frame();
            return;
        }
        let context_lost = match renderer::render_scene(back, &self.scene, &view, &proj, &mut self.perf, ws_visible, context_menu) {
            Err(SwapBuffersError::ContextLost(e)) => {
                error!(?e, "Context lost");
                true
            }
            _ => false,
        };
        if context_lost {
            self.backend = None;
            self.scheduler.clear();
            self.perf.record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
            self.perf.record_frame();
            return;
        }
        if let Err(e) = back.finish_frame() {
            error!(?e, "finish_frame failed");
        } else {
            self.perf.record_presented();
            if !updates.is_empty() {
                self.perf.record_damage();
            }
        }

        // Complete pending frame callbacks for all mapped Wayland surfaces.
        // Without this, clients that request wl_surface.frame() wait forever
        // and never render their initial content.
        let time = now_ms();
        for toplevel in &self.toplevels {
            if toplevel.lifecycle == SurfaceLifecycle::Mapped || toplevel.lifecycle == SurfaceLifecycle::Configured {
                let surface = toplevel.toplevel.wl_surface();
                with_states(surface, |states| {
                    let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                    let current = attrs.current();
                    for cb in &current.frame_callbacks {
                        cb.done(time);
                    }
                    current.frame_callbacks.clear();
                });
            }
        }

        // Flush protocol events generated during this frame (frame callbacks,
        // input forwarding, configure events). libwayland buffers them; without
        // an explicit flush they are only delivered when the client itself
        // sends traffic, so clients pacing rendering with wl_surface.frame()
        // (e.g. foot) stall until the next input event.
        let _ = self.display_handle.flush_clients();

        self.scene.clear_damage();

        self.perf.record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
        self.perf.record_frame();
    }

    /// Compute proj × view matrix for the current camera.
    fn proj_view(&self) -> Matrix4<f32> {
        let (w, h) = self.window_size;
        let proj = if self.spatial_mode {
            cgmath::perspective(cgmath::Deg(45.0), w / h, 1.0, 10000.0)
        } else {
            cgmath::ortho(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        };
        proj * self.camera.view_matrix()
    }

    /// Route a pointer event to the selected visual's InputSink.
    /// Focus follows click: sets focused visual to the selected one.
    /// Title bar hits are NOT routed to content — caller should start a drag.
    /// Authoritative keyboard focus setter.
    /// Updates scene focus, Wayland keyboard focus, data device focus, FocusManager, and SpatialChrome consistently.
    /// Unlocks pointer if focus changes to a different surface than the locked one.
    fn set_keyboard_focus(&mut self, vid: Option<VisualId>) {
        // Unlock pointer on focus change (unless the same visual)
        if self.pointer_constraints.pointer_locked {
            let locked_surface = self.pointer_constraints.locked_surface.clone();
            let is_same_surface = vid.and_then(|v| self.wayland_surfaces.get(&v)).map_or(false, |s| {
                locked_surface.as_ref().map_or(false, |ls| ls == s)
            });
            if !is_same_surface {
                self.pointer_constraints.unlock();
            }
        }

        // Update scene focus
        self.scene.focus(vid);

        // Update Wayland keyboard focus
        if let (Some(vid), Some(kh)) = (vid, self.keyboard_handle.clone()) {
            if let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() {
                let serial = self.next_serial();
                kh.set_focus(self, Some(wl_surface), serial);
            }
        } else if let Some(kh) = self.keyboard_handle.clone() {
            let serial = self.next_serial();
            kh.set_focus(self, None, serial);
        }

        // Update data device focus so clipboard selection is offered
        self.update_data_device_focus(vid);

        // Update SpatialChrome on visuals
        for visual in &mut self.scene.visuals {
            visual.chrome.focused = Some(visual.id) == vid;
        }
    }

    /// Update the data device focus to match the keyboard focus.
    /// This ensures clipboard/primary selection is offered to the correct client.
    fn update_data_device_focus(&mut self, vid: Option<VisualId>) {
        let client = vid.and_then(|vid| {
            self.wayland_surfaces.get(&vid)
                .and_then(|s| Resource::client(s))
        });
        if let Some(ref seat) = self.seat {
            let dh = &self.display_handle;
            smithay::wayland::selection::data_device::set_data_device_focus::<Self>(dh, seat, client.clone());
            smithay::wayland::selection::primary_selection::set_primary_focus::<Self>(dh, seat, client);
        }
    }

    fn route_to_content(&mut self, kind: PointerEventKind, x: f64, y: f64) -> ContentRouting {
        let Some(vid) = self.scene.selected_id else { return ContentRouting::NoTarget };
        if !self.scene.is_active(vid) { return ContentRouting::NoTarget }

        if kind == PointerEventKind::Down {
            self.set_keyboard_focus(Some(vid));
            self.scene.bring_to_front(vid);
            info!(?vid, "focus set, brought to front");
        }

        let (w, h) = self.window_size;
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();

        let data = self.scene.visuals.iter().find(|v| v.id == vid).map(|v| {
            (v.transform.clone(), v.total_width(), v.total_height(), v.decoration.title_bar_height, v.geometry.size)
        });
        let Some((transform, gw, gh, title_h, geom_size)) = data else { return ContentRouting::NoTarget };

        if let Some((u, v)) = input_router::screen_to_visual_uv(
            &pv, ndc_x, ndc_y, &transform, gw, gh,
        ) {
            // Resize zones win over title-bar/content routing on pointer
            // down (I3b). An 8 logical-px band along the decorated border.
            if kind == PointerEventKind::Down && self.resize_session.is_none() {
                let band_u = 8.0 / geom_size.w.max(1) as f64;
                let band_v = 8.0 / (geom_size.h.max(1) as f64 * (1.0 + title_h as f64));
                let zone = crate::resize::hit_test_resize_zone(u, v, band_u, band_v);
                info!(u, v, band_u, band_v, zone = ?zone, "resize zone check");
                if let Some(edges) = zone {
                    let is_toplevel = self.toplevels.iter().any(|t| t.visual_id == Some(vid));
                    if is_toplevel {
                        if self.is_maximized(vid) {
                            info!(?vid, ?edges, "resize refused: window is maximized");
                        } else {
                            let start_local = ((u - 0.5) as f32, (0.5 - v) as f32);
                            if self.begin_resize_session(vid, edges, start_local) {
                                return ContentRouting::Routed;
                            }
                        }
                    }
                }
            }
            let title_frac = (title_h / (1.0 + title_h)) as f64;
            if v < title_frac {
                return ContentRouting::TitleBarHit;
            }
            let content_u = u.clamp(0.0, 1.0);
            let content_v = (v - title_frac) / (1.0 - title_frac);
            let content_v = content_v.clamp(0.0, 1.0);

            // Check if this is a Wayland surface — if so, emit Smithay seat events
            // Clone handles first to avoid borrow conflicts with ph.motion(self,...)
            let wl_surface = self.wayland_surfaces.get(&vid).cloned();
            let pointer_handle = self.pointer_handle.clone();
            let geom_w = self.scene.visuals.iter().find(|v| v.id == vid).map(|v| v.geometry.size.w as f64);

            if let (Some(wl_surface), Some(ph)) = (wl_surface, pointer_handle) {
                if let Some(gw) = geom_w {
                    let geom_h = self.scene.visuals.iter().find(|v| v.id == vid).map(|v| v.geometry.size.h as f64).unwrap_or(1.0);
                    let px = content_u * gw;
                    let py = content_v * geom_h;
                    let pos: smithay::utils::Point<f64, smithay::utils::Logical> = (px, py).into();
                    let serial = self.next_serial();
                    let time = now_ms();
                    let btn_ev = ButtonEvent {
                        serial,
                        time,
                        button: 0x110,
                        state: match kind {
                            PointerEventKind::Down => smithay::backend::input::ButtonState::Pressed,
                            PointerEventKind::Up => smithay::backend::input::ButtonState::Released,
                            _ => smithay::backend::input::ButtonState::Pressed,
                        },
                    };
                    let global_pos: smithay::utils::Point<f64, smithay::utils::Logical> = (x, y).into();
                    let mot_ev = MotionEvent {
                        location: global_pos,
                        serial,
                        time,
                    };
                    match kind {
                        PointerEventKind::Motion => {
                            self.last_wayland_focus = Some(wl_surface.clone());
                            ph.motion(self, Some((wl_surface.clone(), pos)), &mot_ev);
                            ph.frame(self);
                        }
                        PointerEventKind::Down | PointerEventKind::Up => {
                            self.last_wayland_focus = Some(wl_surface.clone());
                            ph.motion(self, Some((wl_surface.clone(), pos)), &mot_ev);
                            ph.button(self, &btn_ev);
                            ph.frame(self);
                            info!(?vid, ?pos, ?kind, "wl_pointer.enter + button + frame");
                        }
                        PointerEventKind::Scroll(_, _) => {}
                    }
                    return ContentRouting::Routed;
                }
            }

            let Some(sink) = self.input_sinks.get_mut(&vid) else { return ContentRouting::NoTarget };
            sink.handle_pointer(kind, content_u, content_v);
            return ContentRouting::Routed;
        }
        ContentRouting::NoTarget
    }

    /// Route a keyboard event to the focused visual's InputSink.
    /// key: winit platform key code (X11 keycodes when under X11, offset +8 from evdev).
    /// The offset is subtracted to get raw evdev codes for HID mapping.
    fn route_keyboard(&mut self, key: u32, pressed: bool) {
        let Some(vid) = self.scene.focused_id else { return };
        if !self.scene.is_active(vid) { return }

        // For Wayland surfaces, set keyboard focus and deliver key event
        let wl_focus = self.wayland_surfaces.get(&vid).cloned();
        let kh = self.keyboard_handle.clone();
        if let (Some(wl_surface), Some(ref kh_handle)) = (wl_focus, kh) {
            let serial = self.next_serial();
            let time = now_ms();
            let state = if pressed { KeyState::Pressed } else { KeyState::Released };
            // Ensure keyboard focus is on the right surface
            kh_handle.set_focus(self, Some(wl_surface), serial);
            let xkb_keycode = Keycode::new(key);
            let _ = kh_handle.input::<(), _>(
                self,
                xkb_keycode,
                state,
                serial,
                time,
                |_, mods, sym| {
                    let sym_val: u32 = sym.modified_sym().into();
                    let raw_keycode: u32 = key;
                    let wl_keycode: u32 = raw_keycode - 8;
                    if let Some(ch) = char::from_u32(sym_val) {
                        info!(raw_code = %raw_keycode, wl_code = %wl_keycode, sym = %ch, hex = %format!("{:x}", sym_val), pressed, mods = ?mods, "KEY");
                    } else {
                        info!(raw_code = %raw_keycode, wl_code = %wl_keycode, hex = %format!("{:x}", sym_val), pressed, mods = ?mods, "KEY (no char)");
                    }
                    FilterResult::Forward
                },
            );
            let _ = self.display_handle.flush_clients();
            return;
        }

        // For external producers (non-Wayland), use InputSink path
        let Some(sink) = self.input_sinks.get_mut(&vid) else { return };
        let evdev = if key > 8 { key - 8 } else { key };
        let hid = input_router::linux_to_hid(evdev);
        if hid == 0 {
            return; // unmapped key
        }
        sink.handle_keyboard(KeyboardEvent { key: hid, pressed });
    }

    /// Find which workspace contains a visual.
    fn workspace_for_visual(&self, vid: VisualId) -> Option<usize> {
        for i in 0..self.workspace_manager.len() {
            if let Some(ws) = self.workspace_manager.get(i) {
                if ws.contains(vid) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// After the focused/selected visual was destroyed, transfer keyboard
    /// focus and selection to the topmost remaining visual in the active
    /// workspace so keyboard input keeps flowing to a real client.
    fn refocus_after_close(&mut self) {
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        let replacement = self.scene.pick_focus_replacement(&ws_ids);
        info!(?replacement, "refocusing after close");
        self.scene.select(replacement);
        self.set_keyboard_focus(replacement);
    }

    /// Request a client surface to resize to the given logical size (I3a).
    ///
    /// Sends one xdg_toplevel.configure(size, Resizing) and records the
    /// intent in `client_resizes`. Refuses while any configure — ours or
    /// Smithay's queue — is unacknowledged, so at most one configure per
    /// surface is ever outstanding. The client decides geometry by what
    /// it commits; see `ClientResizeCoordinator`.
    pub fn begin_client_resize(&mut self, vid: VisualId, w: i32, h: i32) -> Option<smithay::utils::Serial> {
        if w <= 0 || h <= 0 {
            return None;
        }
        if self.client_resizes.awaiting_ack(vid) {
            return None;
        }
        let wl_surface = self.wayland_surfaces.get(&vid).cloned()?;
        let toplevel = self.toplevels.iter()
            .find(|t| t.toplevel.wl_surface() == &wl_surface)
            .map(|t| t.toplevel.clone())?;
        if !toplevel.alive() {
            return None;
        }

        // Never stack a second configure while Smithay's queue still holds
        // unacknowledged configures for this surface.
        let unacked = with_states(&wl_surface, |states| {
            let attrs = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .expect("toplevel surface lacks xdg data")
                .lock()
                .unwrap();
            !attrs.pending_configures().is_empty()
        });
        if unacked {
            return None;
        }

        toplevel.with_pending_state(|state| {
            state.size = Some(smithay::utils::Size::new(w, h));
            state.states.set(xdg_toplevel::State::Resizing);
        });
        let serial = toplevel.send_configure();
        self.client_resizes.mark_sent(vid, serial, (w, h));
        info!(?vid, w, h, ?serial, "client resize requested");
        Some(serial)
    }

    /// End a client resize (pointer release).
    ///
    /// Clears Veyra's intent and always withdraws the Resizing state bit
    /// via a follow-up configure (size=None: the client keeps its current
    /// size) so the client does not remain in interactive-resize state.
    pub fn abort_client_resize(&mut self, vid: VisualId) {
        let had_request = self.client_resizes.abort(vid);
        let wl_surface = self.wayland_surfaces.get(&vid).cloned();
        let toplevel = wl_surface.and_then(|wl_surface| {
            self.toplevels.iter()
                .find(|t| t.toplevel.wl_surface() == &wl_surface)
                .map(|t| t.toplevel.clone())
        });
        if let Some(toplevel) = toplevel {
            if toplevel.alive() {
                toplevel.with_pending_state(|state| {
                    state.size = None;
                    state.states.unset(xdg_toplevel::State::Resizing);
                });
                let _ = toplevel.send_configure();
            }
        }
        if had_request {
            info!(?vid, "client resize aborted");
        }
    }

    /// Begin a pointer resize session on a toplevel (I3b).
    ///
    /// Freezes the visual's plane frame, axes and client size constraints
    /// so every later update is deterministic. The visual is detached from
    /// automatic layout for the duration, like drags are.
    pub fn begin_resize_session(
        &mut self,
        vid: VisualId,
        edges: crate::resize::ResizeEdges,
        start_local: (f32, f32),
    ) -> bool {
        if self.resize_session.is_some() {
            return false;
        }
        // I4: maximized windows do not participate in interactive resize.
        if self.is_maximized(vid) {
            info!(?vid, "resize session refused: window is maximized");
            return false;
        }
        let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() else { return false };
        if !self.toplevels.iter().any(|t| t.toplevel.wl_surface() == &wl_surface) {
            return false; // popups are transient and not resizable
        }
        let Some(visual) = self.scene.get(vid) else { return false };
        let start_transform = visual.transform.clone();
        let start_total = (visual.total_width(), visual.total_height());
        let start_size = (visual.geometry.size.w, visual.geometry.size.h);
        if start_total.0 <= 0.0 || start_total.1 <= 0.0 || start_size.0 <= 0 || start_size.1 <= 0 {
            return false;
        }
        let right_axis = start_transform.rotation * cgmath::Vector3::new(1.0, 0.0, 0.0);
        let up_axis = start_transform.rotation * cgmath::Vector3::new(0.0, 1.0, 0.0);

        // Client-requested size constraints from the surface's cached state.
        let (min_w, min_h, max_w, max_h) = with_states(&wl_surface, |states| {
            let attrs = states.cached_state.get::<SurfaceCachedState>().current().clone();
            (
                attrs.min_size.w.max(1),
                attrs.min_size.h.max(1),
                if attrs.max_size.w > 0 { attrs.max_size.w } else { i32::MAX },
                if attrs.max_size.h > 0 { attrs.max_size.h } else { i32::MAX },
            )
        });
        let max_size = if max_w == i32::MAX && max_h == i32::MAX {
            None
        } else {
            Some((max_w, max_h))
        };

        if !self.scene.detached_set.contains(&vid) {
            self.scene.detached_set.push(vid);
        }
        self.resize_session = Some(crate::resize::ResizeSession {
            vid,
            edges,
            start_local,
            start_total,
            start_size,
            start_transform,
            right_axis,
            up_axis,
            min_size: (min_w, min_h),
            max_size,
            desired: start_size,
        });
        info!(?vid, ?edges, "resize session started");
        true
    }

    /// Update the active resize session from a pointer motion event.
    ///
    /// Computes the desired size from the frozen session frame, applies the
    /// anchor-preserving position delta, and sends a configure only when the
    /// previous transaction has completed (client pacing).
    fn update_resize_session(&mut self, x: f64, y: f64) {
        let Some(session) = self.resize_session.clone() else { return };
        let (w, h) = self.window_size;
        if w <= 0.0 || h <= 0.0 { return; }
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();
        // Unproject against the FROZEN start transform: the session frame
        // does not follow the visual as its geometry evolves.
        let Some(local) = input_router::screen_to_visual_local_point(
            &pv, ndc_x, ndc_y,
            &session.start_transform,
            session.start_total.0,
            session.start_total.1,
        ) else { return };
        let upd = session.update(local);
        let vid = session.vid;

        // Anchor-preserving position update from the frozen start position.
        let base = session.start_transform.position;
        if let Some(visual) = self.scene.get_mut(vid) {
            visual.transform.position = base + upd.position_delta;
        }

        if !self.client_resizes.awaiting_ack(vid) {
            let outstanding = self.client_resizes.entry(vid).map(|e| e.requested);
            if outstanding != Some(upd.size) {
                self.begin_client_resize(vid, upd.size.0, upd.size.1);
            }
        }
        if let Some(s) = self.resize_session.as_mut() {
            s.desired = upd.size;
        }
    }

    /// Send the session's pending desired size once the previous configure
    /// transaction completed (called from ack_configure and handle_commit).
    fn flush_resize_desired(&mut self, vid: VisualId) {
        let Some(session) = self.resize_session.as_ref() else { return };
        if session.vid != vid { return; }
        if self.client_resizes.awaiting_ack(vid) { return; }
        let desired = session.desired;
        let outstanding = self.client_resizes.entry(vid).map(|e| e.requested);
        if outstanding != Some(desired) {
            self.begin_client_resize(vid, desired.0, desired.1);
        }
    }

    /// Terminate the resize session on pointer release.
    /// Returns true when a session was active.
    pub fn finish_resize_session(&mut self) -> bool {
        let Some(session) = self.resize_session.take() else { return false };
        self.abort_client_resize(session.vid);
        info!(vid = ?session.vid, "resize session finished");
        true
    }

    // ── Maximize/unmaximize (I4) ─────────────────────────────────────────

    /// The configured size for a maximized surface: the current view size.
    /// The window quad grows around its spatial position when the client
    /// commits bigger buffers — the transform itself is never touched.
    fn maximize_target(&self) -> (i32, i32) {
        let (w, h) = self.window_size;
        ((w.round() as i32).max(1), (h.round() as i32).max(1))
    }

    /// Whether the toplevel for `vid` is currently maximized (I4).
    pub fn is_maximized(&self, vid: VisualId) -> bool {
        self.toplevels.iter().any(|t| t.visual_id == Some(vid) && t.maximized)
    }

    fn toplevel_for_vid(&self, vid: VisualId) -> Option<ToplevelSurface> {
        self.toplevels.iter()
            .find(|t| t.visual_id == Some(vid))
            .map(|t| t.toplevel.clone())
            .filter(|t| t.alive())
    }

    /// Begin a maximize transition (I4).
    ///
    /// Sends one xdg_toplevel.configure(view size, Maximized). The client
    /// decides geometry by what it commits; the visual's spatial transform
    /// (position/rotation/scale) is never modified. Reuses the I3a
    /// coordinator so pacing stays "at most one unacknowledged configure".
    pub fn begin_maximize(&mut self, vid: VisualId, source: crate::maximize::MaximizeSource) {
        use crate::maximize::{MaximizeIntent, MaximizeKind};
        let Some(toplevel) = self.toplevel_for_vid(vid) else { return };
        if self.is_maximized(vid) {
            info!(?vid, ?source, "maximize ignored: already maximized");
            return;
        }
        if self.resize_session.as_ref().map_or(false, |s| s.vid == vid) {
            info!(?vid, ?source, "maximize refused: resize session in progress");
            return;
        }
        let Some(visual) = self.scene.get(vid) else { return };
        let restore = (visual.geometry.size.w, visual.geometry.size.h);
        if restore.0 <= 0 || restore.1 <= 0 {
            info!(?vid, ?source, "maximize refused: no committed geometry yet");
            return;
        }
        // Presentation transform to restore on unmaximize: a maximized
        // window is centered on the view, so the pre-maximize pose must
        // be captured up front.
        let restore_pos = {
            let p = visual.transform.position;
            (p.x, p.y, p.z)
        };
        let r = visual.transform.rotation;
        let restore_rot = [r.v.x, r.v.y, r.v.z, r.s];
        let target = self.maximize_target();

        // Defer while the surface still owes an ACK for a previous configure.
        let wl_surface = self.wayland_surfaces.get(&vid).cloned();
        let unacked = wl_surface.map_or(false, |wl_surface| {
            with_states(&wl_surface, |states| {
                states.data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|attrs| !attrs.lock().unwrap().pending_configures().is_empty())
                    .unwrap_or(false)
            })
        });
        if unacked || self.client_resizes.awaiting_ack(vid) {
            self.maximize.defer(vid, MaximizeKind::Maximize, source);
            info!(?vid, ?source, "maximize deferred: configure outstanding");
            return;
        }

        toplevel.with_pending_state(|state| {
            state.size = Some(smithay::utils::Size::new(target.0, target.1));
            state.states.set(xdg_toplevel::State::Maximized);
        });
        let serial = toplevel.send_configure();
        self.client_resizes.mark_sent(vid, serial, target);
        self.maximize.begin(MaximizeIntent {
            vid, kind: MaximizeKind::Maximize, source, serial, target, restore,
            previous: restore, restore_pos, restore_rot,
        });
        info!(?vid, ?source, ?serial, target_w = target.0, target_h = target.1, restore_w = restore.0, restore_h = restore.1, "maximize requested");
    }

    /// Begin an unmaximize transition (I4): configure the client back to
    /// its pre-maximize committed size and clear the Maximized state bit.
    pub fn begin_unmaximize(&mut self, vid: VisualId, source: crate::maximize::MaximizeSource) {
        use crate::maximize::{MaximizeIntent, MaximizeKind};
        let Some(toplevel) = self.toplevel_for_vid(vid) else { return };
        if !self.is_maximized(vid) {
            info!(?vid, ?source, "unmaximize ignored: not maximized");
            return;
        }
        let restore = self.toplevels.iter()
            .find(|t| t.visual_id == Some(vid))
            .and_then(|t| t.restore_size)
            .unwrap_or_else(|| self.maximize_target());
        let wl_surface = self.wayland_surfaces.get(&vid).cloned();
        let unacked = wl_surface.map_or(false, |wl_surface| {
            with_states(&wl_surface, |states| {
                states.data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|attrs| !attrs.lock().unwrap().pending_configures().is_empty())
                    .unwrap_or(false)
            })
        });
        if unacked || self.client_resizes.awaiting_ack(vid) {
            self.maximize.defer(vid, MaximizeKind::Unmaximize, source);
            info!(?vid, ?source, "unmaximize deferred: configure outstanding");
            return;
        }

        toplevel.with_pending_state(|state| {
            state.size = Some(smithay::utils::Size::new(restore.0, restore.1));
            state.states.unset(xdg_toplevel::State::Maximized);
        });
        let previous = self.scene.get(vid)
            .map(|v| (v.geometry.size.w, v.geometry.size.h))
            .unwrap_or(restore);
        // Unmaximize restores the captured pre-maximize presentation pose
        // (position + rotation); the size restore goes to the client.
        let (restore_pos, restore_rot) = self.toplevels.iter()
            .find(|t| t.visual_id == Some(vid))
            .and_then(|t| t.restore_pose)
            .map(|(p, r)| (p, r))
            .unwrap_or_else(|| {
                match self.scene.get(vid) {
                    Some(v) => {
                        let p = v.transform.position;
                        let r = v.transform.rotation;
                        ((p.x, p.y, p.z), [r.v.x, r.v.y, r.v.z, r.s])
                    }
                    None => ((0.0, 0.0, 0.0), [0.0, 0.0, 0.0, 1.0]),
                }
            });
        let serial = toplevel.send_configure();
        self.client_resizes.mark_sent(vid, serial, restore);
        self.maximize.begin(MaximizeIntent {
            vid, kind: MaximizeKind::Unmaximize, source, serial, target: restore, restore,
            previous, restore_pos, restore_rot,
        });
        info!(?vid, ?source, ?serial, restore_w = restore.0, restore_h = restore.1, "unmaximize requested");
    }

    /// Toggle maximize on the focused (or selected) visual.
    pub fn toggle_maximize_selected(&mut self, source: crate::maximize::MaximizeSource) {
        let Some(vid) = self.scene.focused_id.or(self.scene.selected_id) else {
            info!(?source, "maximize toggle ignored: no focused visual");
            return;
        };
        self.toggle_maximize_for(vid, source);
    }

    /// Toggle maximize on a specific visual (also the context-menu path).
    pub fn toggle_maximize_for(&mut self, vid: VisualId, source: crate::maximize::MaximizeSource) {
        if self.is_maximized(vid) {
            self.begin_unmaximize(vid, source);
        } else {
            self.begin_maximize(vid, source);
        }
    }

    // ── I5: minimize / restore ────────────────────────────────────────

    pub fn is_minimized(&self, vid: VisualId) -> bool {
        self.toplevels.iter().any(|t| t.visual_id == Some(vid) && t.minimized)
    }

    /// Minimize a window (I5).
    ///
    /// The visual is hidden from the scene (renderer + picking skip it)
    /// while the Wayland surface stays mapped and alive: the client keeps
    /// receiving frame callbacks and can keep committing — the commit path
    /// still adopts its buffers so a restore shows the latest content.
    /// The 3D transform and workspace membership are untouched.
    ///
    /// The focused window loses keyboard focus immediately; the best
    /// remaining window (workspace stack order, skipping minimized) is
    /// focused instead.
    pub fn begin_minimize(&mut self, vid: VisualId, source: crate::maximize::MinimizeSource) {
        if self.is_minimized(vid) {
            info!(?vid, ?source, "minimize ignored: already minimized");
            return;
        }
        if self.resize_session.as_ref().is_some_and(|s| s.vid == vid) {
            info!(?vid, ?source, "minimize refused: resize session in progress");
            return;
        }
        if self.scene.get(vid).is_none() {
            info!(?vid, ?source, "minimize ignored: no visual");
            return;
        }
        // Audit fix (P1): only real toplevels can minimize. Popups and
        // external visuals have no ToplevelInfo row, so restoring from
        // `restore_last_minimized` (which scans toplevels) could never
        // find them — they'd hide forever.
        if !self.toplevels.iter().any(|t| t.visual_id == Some(vid)) {
            info!(?vid, ?source, "minimize refused: not a toplevel (popups/transients stay visible)");
            return;
        }
        // An in-flight maximize/unmaximize transition would fight the
        // hidden state (center-on-view at commit). Drop it: the surface
        // just stops being visible.
        self.maximize.abort(vid);

        let was_focused = self.scene.focused_id == Some(vid);
        if let Some(info) = self.toplevels.iter_mut().find(|t| t.visual_id == Some(vid)) {
            info.minimized = true;
        }
        self.scene.set_minimized(vid, true);
        info!(?vid, ?source, "minimize applied");

        if was_focused {
            // Focus the best remaining window on this workspace.
            let ws_ids = self.workspace_manager.active().visual_ids.clone();
            let replacement = self.scene.pick_focus_replacement(&ws_ids)
                .filter(|r| *r != vid);
            info!(?replacement, "refocusing after minimize");
            self.scene.select(replacement);
            self.set_keyboard_focus(replacement);
        }
        self.schedule_render();
    }

    /// Restore a minimized window (I5): visible again at exactly its
    /// previous transform, raised above the stack and keyboard-focused.
    pub fn restore_minimized(&mut self, vid: VisualId, source: crate::maximize::MinimizeSource) {
        if !self.is_minimized(vid) {
            info!(?vid, ?source, "restore ignored: not minimized");
            return;
        }
        // Audit fix (P1): a minimized window restored while the user has
        // switched to another workspace would be focused but INVISIBLE
        // (the renderer only draws the active workspace). Switch to the
        // owning workspace first so the restore is actually seen.
        if let Some(ws_idx) = self.workspace_for_visual(vid) {
            if ws_idx != self.workspace_manager.active_id() {
                info!(?vid, workspace = ws_idx, "restoring across workspace switch");
                self.switch_workspace(ws_idx);
            }
        }
        if let Some(info) = self.toplevels.iter_mut().find(|t| t.visual_id == Some(vid)) {
            info.minimized = false;
        }
        self.scene.set_minimized(vid, false);
        self.scene.raise_to_top(vid);
        self.scene.focus(Some(vid));
        self.set_keyboard_focus(Some(vid));
        info!(?vid, ?source, "minimize restored");
        self.schedule_render();
    }

    /// Restore the most recently minimized window (F10 / shell path).
    /// The scan runs in reverse toplevel order, so the latest-created
    /// minimized window wins.
    pub fn restore_last_minimized(&mut self, source: crate::maximize::MinimizeSource) {
        let target = self.toplevels.iter().rev()
            .find(|t| t.minimized)
            .and_then(|t| t.visual_id);
        match target {
            Some(vid) => self.restore_minimized(vid, source),
            None => info!(?source, "restore ignored: nothing minimized"),
        }
    }

    /// Minimize the focused (or selected) visual.
    pub fn minimize_selected(&mut self, source: crate::maximize::MinimizeSource) {
        let Some(vid) = self.scene.focused_id.or(self.scene.selected_id) else {
            info!(?source, "minimize ignored: no focused visual");
            return;
        };
        self.begin_minimize(vid, source);
    }

    /// Visuals that arrangement must not move: detached pins, maximized
    /// windows (kept centered on the view) and minimized windows (hidden;
    /// their transform must be preserved for restore).
    fn layout_detached(&self) -> Vec<VisualId> {
        let mut d = self.scene.detached_set.clone();
        d.extend(
            self.toplevels.iter()
                .filter(|t| t.maximized || t.minimized)
                .filter_map(|t| t.visual_id)
        );
        d
    }

    /// Complete a maximize/unmaximize transaction after a client commit.
    ///
    /// `committed` is the client's buffer size. Completion rules:
    /// - committed == target: the client complied (client_matched=true).
    /// - committed == previous: a DRAINING buffer (acked our configure but
    ///   not yet redrawn at the new size) — the intent stays armed until
    ///   the client's next commit.
    /// - anything else: the client explicitly committed a third size —
    ///   the acknowledged STATE applies, committed geometry wins
    ///   (client_matched=false).
    ///
    /// The visual transform is applied per the presentation rule: maximize
    /// centers it on the view, unmaximize restores the captured pose. The
    /// fulfilled log records the post-transition transform as evidence.
    fn complete_maximize_intent(&mut self, vid: VisualId, committed: (i32, i32)) {
        use crate::maximize::MaximizeKind;
        let Some(intent) = self.maximize.intent(vid) else { return };
        if committed == intent.previous && committed != intent.target {
            return; // draining commit — keep the intent armed
        }
        let intent = self.maximize.take_intent(vid).expect("intent peeked above");
        let client_matched = committed == intent.target;
        if let Some(info) = self.toplevels.iter_mut().find(|t| t.visual_id == Some(vid)) {
            match intent.kind {
                MaximizeKind::Maximize => {
                    info.maximized = true;
                    info.restore_size = Some(intent.restore);
                    // Presentation: a maximized window covers the view.
                    // Park the pre-maximize pose on the toplevel and center
                    // the visual (workspace origin), rotation identity so
                    // the quad fills the viewport edge to edge.
                    if let Some(v) = self.scene.get_mut(vid) {
                        let p = v.transform.position;
                        info.restore_pose = Some(((p.x, p.y, p.z),
                            [intent.restore_rot[0], intent.restore_rot[1],
                             intent.restore_rot[2], intent.restore_rot[3]]));
                        v.transform.position = cgmath::Vector3::new(0.0, 0.0, p.z);
                        v.transform.rotation = cgmath::Quaternion::new(1.0, 0.0, 0.0, 0.0);
                    }
                }
                MaximizeKind::Unmaximize => {
                    info.maximized = false;
                    if let Some(v) = self.scene.get_mut(vid) {
                        // Restore the exact pre-maximize pose captured at
                        // maximize time (fall back to the transition pose).
                        let pose = info.restore_pose.take().unwrap_or((intent.restore_pos, intent.restore_rot));
                        let (px, py, pz) = pose.0;
                        let [ri, rj, rk, rw] = pose.1;
                        v.transform.position = cgmath::Vector3::new(px, py, pz);
                        // cgmath Quaternion::new is scalar-first (w, x, y, z)
                        v.transform.rotation = cgmath::Quaternion::new(rw, ri, rj, rk);
                    }
                    info.restore_size = None;
                }
            }
        }
        let state_msg = match intent.kind {
            MaximizeKind::Maximize => "maximize fulfilled",
            MaximizeKind::Unmaximize => "unmaximize fulfilled",
        };
        if let Some(v) = self.scene.get(vid) {
            let pos = v.transform.position;
            let rot = v.transform.rotation;
            let scale = v.transform.scale;
            info!(?vid, source = ?intent.source, client_matched,
                  w = intent.target.0, h = intent.target.1, ?pos, ?rot, ?scale,
                  "{}", state_msg);
        } else {
            info!(?vid, source = ?intent.source, client_matched,
                  w = intent.target.0, h = intent.target.1,
                  "{}", state_msg);
        }
        self.flush_deferred_maximize();
    }

    /// Retry a deferred maximize request once the surface is free of
    /// unacknowledged configures (called from ack/commit paths).
    fn flush_deferred_maximize(&mut self) {
        use crate::maximize::MaximizeKind;
        let Some((vid, kind, source)) = self.maximize.take_deferred() else { return };
        match kind {
            MaximizeKind::Maximize => self.begin_maximize(vid, source),
            MaximizeKind::Unmaximize => self.begin_unmaximize(vid, source),
        }
    }

    /// Show a context menu at the given screen position for the selected visual.
    /// Returns true if a menu was shown.
    pub fn handle_context_menu(&mut self, x: f64, y: f64) -> bool {
        // Dismiss any existing menu first
        self.context_menu.dismiss();

        // Pick the visual under cursor
        let (w, h) = self.window_size;
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();
        let ws_ids: Vec<VisualId> = self.workspace_manager.active().visual_ids.iter()
            .copied()
            .filter(|id| self.scene.is_visible(*id))
            .collect();
        let picked = self.scene.pick_visible(&pv, ndc_x, ndc_y, &ws_ids);

        if let Some((vid, _)) = picked {
            let ws_count = self.workspace_manager.len();
            self.context_menu.show(x, y, vid, ws_count);
            self.context_menu.set_maximize_label(self.is_maximized(vid));
            let m = crate::context_menu::MenuMetrics::for_framebuffer(self.window_size.0, self.window_size.1);
            info!(menu_width = m.menu_width, item_height = m.item_height, glyph_scale = m.glyph_scale,
                  fb_w = self.window_size.0, fb_h = self.window_size.1, "context menu metrics");
            info!(?vid, "context menu opened");
            true
        } else {
            false
        }
    }

    /// Execute the action for a context menu item.
    fn execute_menu_action(&mut self, action: MenuAction) {
        let Some(target) = self.context_menu.target else {
            self.context_menu.dismiss();
            return;
        };
        match action {
            MenuAction::Dismiss => {
                self.context_menu.dismiss();
            }
            MenuAction::Focus => {
                self.focus_manager.enter(&self.camera, target, &self.scene);
                info!(?target, "context menu: focus");
            }
            MenuAction::Arrange => {
                let ws = self.workspace_manager.active_mut();
                let mode = ws.layout_mode;
                let detached = self.layout_detached();
                let eligible = self.workspace_manager.active().visual_ids.clone();
                let (ww, wh) = self.window_size;
                layout::apply_layout(&mut self.scene, mode, &layout::LayoutConfig::default(), &detached, ww, wh, &eligible);
                info!(?target, "context menu: arrange");
            }
            MenuAction::MoveToWorkspace(ws_idx) => {
                if ws_idx < self.workspace_manager.len() {
                    if let Some(ws) = self.workspace_manager.get_mut(ws_idx) {
                        ws.add(target);
                    }
                    let current_ws = self.workspace_manager.active_id();
                    if let Some(ws) = self.workspace_manager.get_mut(current_ws) {
                        ws.remove(target);
                    }
                    info!(?target, workspace = ws_idx, "context menu: move to workspace");
                }
            }
            MenuAction::Group => {
                self.scene.create_group(vec![target]);
                info!(?target, "context menu: group");
            }
            MenuAction::Ungroup => {
                let gid = self.scene.find_group_containing(target);
                if let Some(gid) = gid {
                    self.scene.remove_group(gid);
                    info!(?target, "context menu: ungroup");
                }
            }
            MenuAction::DeEmphasize => {
                self.scene.de_emphasize(target);
                info!(?target, "context menu: de-emphasize");
            }
            MenuAction::Restore => {
                self.scene.restore_from_de_emphasis(target);
                info!(?target, "context menu: restore");
            }
            MenuAction::ResetTransform => {
                self.scene.reset_transform(target);
                info!(?target, "context menu: reset transform");
            }
            MenuAction::Maximize => {
                // Same path as Meta+Up: intent -> configure -> commit.
                self.toggle_maximize_for(target, crate::maximize::MaximizeSource::Compositor);
                info!(?target, "context menu: maximize toggle");
            }
            MenuAction::Minimize => {
                // Non-Meta permanent minimize path (right-click). The
                // minimize flow drops keyboard focus and hides the visual
                // while the client stays alive.
                self.begin_minimize(target, crate::maximize::MinimizeSource::Compositor);
                info!(?target, "context menu: minimize");
            }
            MenuAction::Close => {
                if let Some(wl_surface) = self.wayland_surfaces.get(&target).cloned() {
                    // Send close to the client via XDG toplevel
                    for t in &self.toplevels {
                        if t.toplevel.wl_surface() == &wl_surface {
                            t.toplevel.send_close();
                            info!(?target, "context menu: close sent");
                            break;
                        }
                    }
                }
            }
        }
        self.context_menu.dismiss();
    }

    /// Handle a left-click on the context menu. Returns true if the click was handled by the menu.
    pub fn handle_menu_click(&mut self, x: f64, y: f64) -> bool {
        // Must match the renderer's metrics (MenuMetrics::for_framebuffer)
        let m = crate::context_menu::MenuMetrics::for_framebuffer(self.window_size.0, self.window_size.1);
        if let Some(idx) = self.context_menu.item_at(x, y, m.menu_width as f64, m.item_height as f64) {
            if idx < self.context_menu.items.len() {
                let action = self.context_menu.items[idx].action;
                self.execute_menu_action(action);
                return true;
            }
        }
        false
    }

    /// Public entry point for a pointer button press.
    pub fn handle_pointer_down(&mut self, x: f64, y: f64, shift: bool, ctrl: bool, alt: bool) {
        self.press_pos = (x, y);
        self.event_serial = self.event_serial.wrapping_add(1);
        self.interaction.window_size = self.window_size;
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        let mode = self.interaction.handle_pointer_down(
            x, y, &mut self.scene, &self.camera, self.spatial_mode, shift, ctrl, alt,
            Some(ws_ids),
        );
        // In overview mode, clicking a visual should focus it
        if matches!(self.focus_manager.camera_mode, CameraMode::Overview) {
            if let Some(vid) = self.scene.selected_id {
                self.focus_manager.enter(&self.camera, vid, &self.scene);
                info!(?vid, "overview click -> focus");
            }
            return;
        }
        // In workspace overview, clicking a visual switches to its workspace
        if matches!(self.focus_manager.camera_mode, CameraMode::WorkspaceOverview) {
            if let Some(vid) = self.scene.selected_id {
                if let Some(ws_id) = self.workspace_for_visual(vid) {
                    let _ = self.activate_workspace(ws_id);
                    self.focus_manager.exit_overview(&mut self.camera);
                    self.set_keyboard_focus(Some(vid));
                    info!(?vid, workspace = ws_id, "workspace overview click -> switch");
                }
            }
            return;
        }
        // If the clicked visual doesn't belong to the active workspace, deselect it
        if let Some(vid) = self.scene.selected_id {
            let in_workspace = self.workspace_manager.active().contains(vid);
            if !in_workspace {
                self.scene.selected_id = None;
                self.scene.focus(None);
            }
        }
        self.last_down_vid = self.scene.selected_id;
        match mode {
            Some(_) => {}
            None => {
                // Route to content; title bar hits start a title-bar drag
                match self.route_to_content(PointerEventKind::Down, x, y) {
                    ContentRouting::TitleBarHit => {
                        // Start a translate drag from the title bar
                        let ws_ids = self.workspace_manager.active().visual_ids.clone();
                        self.interaction.handle_pointer_down(
                            x, y, &mut self.scene, &self.camera,
                            self.spatial_mode, false, false, false, Some(ws_ids),
                        );
                        // Force translate even though no modifier
                        self.interaction.force_translate(x, y, &mut self.scene,
                            &self.camera, self.spatial_mode);
                    }
                    _ => {}
                }
            }
        }
        let _ = self.display_handle.flush_clients();
    }

    /// Public entry point for pointer button release.
    pub fn handle_pointer_up(&mut self, x: f64, y: f64) {
        self.event_serial = self.event_serial.wrapping_add(1);
        self.last_down_vid = None;
        // Finish a pointer resize before any content routing (I3b).
        if self.finish_resize_session() {
            self.schedule_render();
            return;
        }
        let has_active = self.interaction.is_dragging();
        self.interaction.handle_pointer_up();
        if !has_active {
            self.route_to_content(PointerEventKind::Up, x, y);
        }
        let _ = self.display_handle.flush_clients();
    }

    /// Public entry point for pointer motion.
    pub fn handle_pointer_move(&mut self, x: f64, y: f64) {
        let dx = x - self.last_mouse.0;
        let dy = y - self.last_mouse.1;
        self.last_mouse = (x, y);

        // When pointer is locked, route relative motion to the locked client
        // and skip all spatial interaction.
        if self.pointer_constraints.pointer_locked {
            if let (Some(ph), Some(surface)) = (self.pointer_handle.clone(), self.pointer_constraints.locked_surface.clone()) {
                let serial = self.next_serial();
                let time = now_ms();
                let pos: smithay::utils::Point<f64, smithay::utils::Logical> = (x, y).into();
                let mot_ev = MotionEvent {
                    location: pos,
                    serial,
                    time,
                };
                let rel_ev = smithay::input::pointer::RelativeMotionEvent {
                    delta: (dx, dy).into(),
                    delta_unaccel: (dx, dy).into(),
                    utime: time as u64 * 1000,
                };
                ph.motion(self, Some((surface.clone(), pos)), &mot_ev);
                ph.relative_motion(self, Some((surface.clone(), pos)), &rel_ev);
            }
            return;
        }
        // Navigation buttons (right=mouse 3, middle=mouse 2)
        if self.nav_button == 3 {
            self.workspace_manager.active_mut().auto_orbit = false;
            self.handle_orbit(dx, dy);
            return;
        }
        if self.nav_button == 2 {
            self.workspace_manager.active_mut().auto_orbit = false;
            self.handle_pan(dx, dy);
            return;
        }
        // Pointer resize session (I3b): suppress drag/hover routing while
        // resizing so pointer focus stays on the resized surface.
        if self.resize_session.is_some() {
            self.update_resize_session(x, y);
            self.schedule_render();
            return;
        }
        self.interaction.window_size = self.window_size;
        let was_dragging = self.interaction.is_dragging();
        self.interaction.handle_pointer_move(x, y, &mut self.scene, &self.camera, self.spatial_mode);

        // Snap correction: if currently dragging, snap dragged visual to nearby edges
        if self.interaction.is_dragging() {
            if let Some(vid) = self.scene.selected_id {
                if let Some(visual) = self.scene.visuals.iter().find(|v| v.id == vid) {
                    let mpos = visual.transform.position;
                    let mw = visual.total_width();
                    let mh = visual.total_height();
                    // Build anchor list from non-selected, non-detached, same-workspace visuals only
                    let ws_ids = self.workspace_manager.active().visual_ids.as_slice();
                    let anchors: Vec<_> = self.scene.visuals.iter()
                        .filter(|v| v.id != vid && !self.scene.detached_set.contains(&v.id) && ws_ids.contains(&v.id))
                        .map(|v| (v.transform.position, v.total_width(), v.total_height()))
                        .collect();
                    if let Some(snap) = crate::snap::snap_position(mpos, mw, mh, &anchors, &Default::default()) {
                        if let Some(v) = self.scene.get_mut(vid) {
                            v.transform.position = snap.position;
                        }
                    }
                }
            }
        }

        // If left button is held and we're not already dragging,
        // start a content-area spatial drag on the selected visual.
        if !was_dragging && !self.interaction.is_dragging() && self.nav_button == 1 {
            if let Some(vid) = self.scene.selected_id {
                if self.scene.is_active(vid) {
                    let threshold = 5.0;
                    if (x - self.press_pos.0).abs() > threshold || (y - self.press_pos.1).abs() > threshold {
                        self.interaction.force_translate(x, y, &mut self.scene, &self.camera, self.spatial_mode);
                    }
                }
            }
        }
        // If still not dragging after all checks, route hover events
        if !was_dragging && !self.interaction.is_dragging() {
            self.route_hover(x, y);
        }
    }

    /// Pick the Wayland surface under the given screen position via 3D ray cast.
    /// Returns (visual id, surface, surface-local content position).
    /// Returns None when the cursor is over empty space, a title bar, or a
    /// non-Wayland visual.
    fn pick_wayland_target(
        &self,
        x: f64,
        y: f64,
    ) -> Option<(VisualId, WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        let (w, h) = self.window_size;
        if w <= 0.0 || h <= 0.0 { return None; }
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();

        let ws_visible: Vec<VisualId> = self.workspace_manager.active().visual_ids.iter()
            .copied()
            .filter(|id| self.scene.is_visible(*id))
            .collect();
        let (vid, _) = self.scene.pick_visible(&pv, ndc_x, ndc_y, &ws_visible)?;
        if !self.scene.is_active(vid) { return None; }
        let wl_surface = self.wayland_surfaces.get(&vid).cloned()?;
        let v = self.scene.visuals.iter().find(|v| v.id == vid)?;
        let transform = v.transform.clone();
        let total_w = v.total_width();
        let total_h = v.total_height();
        let (u, uv) = input_router::screen_to_visual_uv(
            &pv, ndc_x, ndc_y, &transform, total_w, total_h,
        )?;
        let title_frac = 0.06f64 / 1.06f64;
        if uv < title_frac {
            return None; // title bar — not content
        }
        let content_v = ((uv - title_frac) / (1.0 - title_frac)).clamp(0.0, 1.0);
        let px = u.clamp(0.0, 1.0) * v.geometry.size.w as f64;
        let py = content_v * v.geometry.size.h as f64;
        Some((vid, wl_surface, (px, py).into()))
    }

    /// Route hover (pointer motion without button) to Wayland surfaces.
    /// Updates pointer focus based on 3D ray hit testing. Only emits
    /// enter/leave transitions when the hovered surface ACTUALLY changes.
    /// Sends a pointer leave when the cursor moves off all surfaces so
    /// clients do not believe the pointer is still inside them.
    fn route_hover(&mut self, x: f64, y: f64) {
        let ph = match self.pointer_handle.clone() {
            Some(ph) => ph,
            None => return,
        };

        let target = self.pick_wayland_target(x, y);
        match &target {
            Some((vid, _, _)) => self.scene.hovered_id = Some(*vid),
            None => self.scene.hovered_id = None,
        }

        let Some((vid, wl_surface, pos)) = target else {
            // Cursor left all client surfaces — emit pointer leave.
            if self.last_wayland_focus.take().is_some() {
                let global_pos: smithay::utils::Point<f64, smithay::utils::Logical> = (x, y).into();
                let mot_ev = MotionEvent {
                    location: global_pos,
                    serial: self.next_serial(),
                    time: now_ms(),
                };
                ph.motion(self, None, &mot_ev);
            }
            return;
        };

        // PointerHandle::motion handles enter/leave internally — same
        // surface = motion; different surface = leave old + enter new.
        self.last_wayland_focus = Some(wl_surface.clone());
        let global_pos: smithay::utils::Point<f64, smithay::utils::Logical> = (x, y).into();
        let mot_ev = MotionEvent {
            location: global_pos,
            serial: self.next_serial(),
            time: now_ms(),
        };
        ph.motion(self, Some((wl_surface, pos)), &mot_ev);
    }

    /// Center the camera on the currently selected visual.
    pub fn frame_selected(&mut self) -> bool {
        let Some(vid) = self.scene.selected_id else { return false };
        let result = self.camera.frame_visual(vid, &self.scene);
        if result {
            info!(?vid, "camera framed on selected");
        }
        result
    }

    /// Frame all visuals in view.
    pub fn frame_all(&mut self) -> bool {
        let result = self.camera.frame_all(&self.scene);
        if result {
            info!("camera framed all visuals");
        }
        result
    }

    /// Toggle focus mode: enter or exit camera framing of the focused visual.
    pub fn toggle_focus_mode(&mut self) {
        match self.focus_manager.camera_mode {
            CameraMode::Focus(_) | CameraMode::Overview | CameraMode::WorkspaceOverview => {
                // Exit — restore previous camera
                match self.focus_manager.camera_mode {
                    CameraMode::Focus(_) => self.focus_manager.exit(&mut self.camera, &self.scene),
                    _ => self.focus_manager.exit_overview(&mut self.camera),
                }
                info!("focus mode off");
            }
            CameraMode::Normal => {
                // Enter focus mode — save camera, target focused visual
                let Some(vid) = self.scene.focused_id else {
                    info!("no focused visual to focus on");
                    return;
                };
                self.focus_manager.enter(&self.camera, vid, &self.scene);
                info!(?vid, "focus mode on");
            }
        }
    }

    /// Enter overview mode: show all visuals in the active workspace.
    pub fn enter_overview(&mut self) {
        let ws = self.workspace_manager.active();
        if let Some(overview_cam) = crate::focus::overview_camera(&self.scene, &ws.visual_ids) {
            self.focus_manager.enter_overview(&self.camera, overview_cam);
            info!("overview mode on");
        }
    }

    /// Enter workspace overview: show all workspaces.
    pub fn enter_workspace_overview(&mut self) {
        // Compute a camera that shows all workspace cameras' positions
        // Simplified: pull way back to show all 3 workspaces
        let overview_cam = Camera {
            position: cgmath::Point3::new(0.0, 0.0, 3000.0),
            yaw: 0.0,
            pitch: -0.3,
            ..Camera::new()
        };
        self.focus_manager.enter_workspace_overview(&self.camera, overview_cam);
        info!("workspace overview mode on");
    }

    /// Get the next serial number for input events.
    fn next_serial(&mut self) -> smithay::utils::Serial {
        self.event_serial = self.event_serial.wrapping_add(1);
        smithay::utils::Serial::from(self.event_serial)
    }

    /// Orbit camera (right-drag).
    pub fn handle_orbit(&mut self, dx: f64, dy: f64) {
        self.camera.handle_orbit(dx, dy);
    }

    /// Pan camera (middle-drag).
    pub fn handle_pan(&mut self, dx: f64, dy: f64) {
        self.camera.handle_pan(dx, dy, 0.05);
    }

    /// Zoom camera (scroll).
    pub fn handle_zoom(&mut self, delta: f64) {
        self.camera.handle_zoom(delta);
    }

    /// Handle a pointer axis (scroll) event at the given screen position.
    ///
    /// Scroll is routed to the Wayland surface under the cursor (terminals,
    /// browsers scroll their content). Only when no client surface is under
    /// the cursor does the camera zoom (the pre-existing global behavior).
    pub fn handle_axis(&mut self, x: f64, y: f64, dx: f64, dy: f64) {
        let Some(ph) = self.pointer_handle.clone() else {
            self.camera.handle_zoom(dy);
            return;
        };

        if let Some((_, wl_surface, pos)) = self.pick_wayland_target(x, y) {
            // Ensure pointer focus is on the target surface before axis events.
            self.last_wayland_focus = Some(wl_surface.clone());
            let global_pos: smithay::utils::Point<f64, smithay::utils::Logical> = (x, y).into();
            let mot_ev = MotionEvent {
                location: global_pos,
                serial: self.next_serial(),
                time: now_ms(),
            };
            ph.motion(self, Some((wl_surface, pos)), &mot_ev);

            let time = now_ms();
            let frame = smithay::input::pointer::AxisFrame::new(time)
                .source(smithay::backend::input::AxisSource::Wheel)
                .value(smithay::backend::input::Axis::Horizontal, dx)
                .value(smithay::backend::input::Axis::Vertical, dy);
            ph.axis(self, frame);
            ph.frame(self);
        } else {
            if dx.abs() > dy.abs() {
                self.camera.handle_zoom(dx);
            } else {
                self.camera.handle_zoom(dy);
            }
        }
    }

    /// Save camera bookmark.
    pub fn save_bookmark(&mut self, slot: usize) {
        self.camera.save_bookmark(slot);
        info!(slot, "camera bookmark saved");
    }

    /// Restore camera bookmark.
    pub fn restore_bookmark(&mut self, slot: usize) -> bool {
        let result = self.camera.restore_bookmark(slot);
        if result {
            info!(slot, "camera bookmark restored");
        }
        result
    }

    /// Convenience: get the layout mode from the active workspace.
    pub fn layout_mode(&self) -> layout::LayoutMode {
        self.workspace_manager.active().layout_mode
    }

    /// Switch to a workspace by ID.
    /// Saves the current workspace state and restores the target workspace state.
    /// Uses set_keyboard_focus() to ensure Wayland keyboard focus stays in sync.
    /// Returns true if the switch occurred.
    pub fn switch_workspace(&mut self, idx: usize) -> bool {
        // Unlock pointer on workspace switch
        if self.pointer_constraints.pointer_locked {
            self.pointer_constraints.unlock();
        }
        // Terminate any in-progress drag: the dragged visual may not belong
        // to the target workspace, leaving stale drag state behind.
        if self.interaction.is_dragging() {
            self.interaction.handle_pointer_up();
        }
        // Same for an in-progress resize session (I3b).
        if let Some(session) = self.resize_session.take() {
            self.abort_client_resize(session.vid);
        }

        let old_id = self.workspace_manager.active_id();
        // Save current state into the old workspace
        {
            let ws = self.workspace_manager.active_mut();
            ws.camera = self.camera.clone();
            ws.focused_id = self.scene.focused_id;
            ws.detached_set = self.scene.detached_set.clone();
            ws.focus_manager_state = self.focus_manager.clone();
        }
        if !self.workspace_manager.switch(idx, &mut self.scene) {
            return false;
        }
        // Sync camera, layout, focus from saved workspace state
        let ws = self.workspace_manager.active();
        self.camera = ws.camera.clone();
        self.scene.detached_set = ws.detached_set.clone();
        // Sync focus manager state
        self.focus_manager = ws.focus_manager_state.clone();
        // Reset camera mode on workspace switch (each workspace has its own view)
        self.focus_manager.camera_mode = CameraMode::Normal;
        self.focus_manager.transition = None;
        self.focus_manager.saved_camera = None;
        // Use authoritative focus setter for Wayland keyboard focus sync
        self.set_keyboard_focus(ws.focused_id);
        info!(workspace = idx, old = old_id, "switched workspace");
        true
    }

    /// Create a new workspace and return its ID.
    pub fn create_workspace(&mut self) -> usize {
        let id = self.workspace_manager.add();
        info!(workspace = id, "workspace created");
        id
    }

    /// Destroy a workspace by ID.
    /// Fails if it's the last workspace.
    /// Wayland surfaces survive — only their workspace membership is cleaned up.
    pub fn destroy_workspace(&mut self, id: usize) -> Result<(), String> {
        if self.workspace_manager.len() <= 1 {
            return Err("cannot destroy the last workspace".into());
        }
        // If destroying the active workspace, save current state and switch to 0 first
        if id == self.workspace_manager.active_id() {
            {
                let ws = self.workspace_manager.active_mut();
                ws.camera = self.camera.clone();
                ws.focused_id = self.scene.focused_id;
                ws.detached_set = self.scene.detached_set.clone();
                ws.focus_manager_state = self.focus_manager.clone();
            }
            self.switch_workspace(0);
        }
        // Remove all Visual references from the workspace (but DON'T destroy Wayland surfaces)
        // The workspace_manager.remove() handles visual state cleanup via save_transforms
        self.workspace_manager.remove(id, &mut self.scene)?;
        info!(workspace = id, "workspace destroyed");
        Ok(())
    }

    /// Returns the number of workspaces.
    pub fn workspace_count(&self) -> usize {
        self.workspace_manager.len()
    }

    /// Cycle to the next workspace (wraps around).
    pub fn next_workspace(&mut self) -> bool {
        let current = self.workspace_manager.active_id();
        let next = (current + 1) % self.workspace_manager.len();
        self.switch_workspace(next)
    }

    /// Cycle to the previous workspace (wraps around).
    pub fn previous_workspace(&mut self) -> bool {
        let current = self.workspace_manager.active_id();
        let prev = if current == 0 {
            self.workspace_manager.len() - 1
        } else {
            current - 1
        };
        self.switch_workspace(prev)
    }

    /// Activate a specific workspace by ID. No-op if the ID is invalid.
    pub fn activate_workspace(&mut self, id: usize) -> bool {
        if id >= self.workspace_manager.len() {
            return false;
        }
        self.switch_workspace(id)
    }

    /// Public entry point for keyboard events.
    /// Routes to the focused visual's InputSink.
    /// Uses NavigationModel for binding dispatch.
    pub fn handle_key(&mut self, linux_key: u32, pressed: bool) {
        use crate::keys;
        // A press consumed by the compositor must not leak an unpaired
        // release to the focused client.
        if !pressed && self.swallow_release == Some(linux_key) {
            self.swallow_release = None;
            return;
        }
        match linux_key {
            keys::CTRL_L | keys::CTRL_R => { self.ctrl_pressed = pressed; }
            keys::SHIFT_L | keys::SHIFT_R => { self.shift_pressed = pressed; }
            keys::ALT_L | keys::ALT_R => {
                self.alt_pressed = pressed;
                if !pressed && self.alt_tab_active {
                    self.alt_tab_active = false;
                }
            }
            keys::META_L | keys::META_R => {
                self.meta_pressed = pressed;
            }
            _ => {}
        }

        if pressed {
            self.workspace_manager.active_mut().auto_orbit = false;
        }

        // Track Alt+Tab state: while Alt is held, keep cycling
        if self.alt_pressed && linux_key == crate::keys::TAB && pressed {
            if self.shift_pressed {
                self.alt_tab_active = true;
                if let Some(app_id) = self.app_switcher.previous() {
                    info!(app = %app_id.as_str(), "alt+shift+tab: previous app");
                }
            } else {
                self.alt_tab_active = true;
                if let Some(app_id) = self.app_switcher.next() {
                    info!(app = %app_id.as_str(), "alt+tab: next app");
                }
            }
            return;
        }

        tracing::debug!(?linux_key, pressed, ctrl = self.ctrl_pressed, shift = self.shift_pressed, alt = self.alt_pressed, meta = self.meta_pressed, "KEY EVENT");

        // If context menu is visible, route keyboard navigation to it
        if self.context_menu.visible && pressed {
            use crate::keys;
            match linux_key {
                keys::UP => { self.context_menu.select_prev(); self.swallow_release = Some(linux_key); return; }
                keys::DOWN => { self.context_menu.select_next(); self.swallow_release = Some(linux_key); return; }
                keys::ENTER => {
                    if let Some(action) = self.context_menu.confirm_selection() {
                        self.execute_menu_action(action);
                    }
                    self.swallow_release = Some(linux_key);
                    return;
                }
                _ => {}
            }
        }

        if pressed {
            use crate::keys;
            match linux_key {
                keys::F1 => { self.activate_workspace(0); return; }
                keys::F2 => { self.activate_workspace(1); return; }
                keys::F3 => { self.activate_workspace(2); return; }
                _ => {}
            }

            // Dispatch key bindings through NavigationModel
            let binding = self.navigation.match_binding(
                linux_key,
                self.ctrl_pressed,
                self.shift_pressed,
                self.alt_pressed,
                self.meta_pressed,
            );

            if let Some(b) = binding {
                self.handle_binding(b);
                self.swallow_release = Some(linux_key);
                return;
            }
        }

        // Camera keyboard controls only when no visual has focus
        if self.scene.focused_id.is_none() {
            self.camera.handle_key(linux_key, pressed, 1.0);
        }

        if pressed {
            // Meta+1..9 — save bookmark (with selection); Meta+1..0 — restore
            if let Some(slot) = crate::navigation::bookmark_slot(linux_key, self.meta_pressed) {
                if self.scene.selected_id.is_some() {
                    self.save_bookmark(slot);
                } else {
                    self.restore_bookmark(slot);
                }
                return;
            }
        }
        // Route ALL keyboard events (down AND up) to focused visual
        self.route_keyboard(linux_key, pressed);
    }

    /// Dispatch a key binding to the appropriate handler.
    fn handle_binding(&mut self, binding: crate::navigation::Binding) {
        use crate::navigation::Binding::*;
        match binding {
            ToggleSpatial => {
                if self.spatial_mode {
                    // Leaving spatial: remember the pose, then let the
                    // render loop pin the ortho camera.
                    self.spatial_cam_pose = Some((
                        self.camera.position,
                        self.camera.yaw,
                        self.camera.pitch,
                    ));
                    self.spatial_mode = false;
                } else {
                    // Re-entering spatial: restore the saved pose so the
                    // desktop looks exactly like before the toggle.
                    self.spatial_mode = true;
                    if let Some((pos, yaw, pitch)) = self.spatial_cam_pose.take() {
                        self.camera.position = pos;
                        self.camera.yaw = yaw;
                        self.camera.pitch = pitch;
                    } else if !self.spatial_cam_adapted {
                        // First spatial entry through the toggle: fit the
                        // frustum to the workspace view.
                        let d = (self.window_size.1 * 1.2071f32).max(600.0);
                        self.camera.position = cgmath::Point3::new(0.0, 0.0, d);
                        self.camera.yaw = 0.0;
                        self.camera.pitch = 0.0;
                    }
                    self.spatial_cam_adapted = true;
                }
                tracing::info!(spatial_mode = self.spatial_mode, "spatial mode toggled");
            }
            ToggleFocus => {
                self.toggle_focus_mode();
            }
            ToggleOverview => {
                match self.focus_manager.camera_mode {
                    CameraMode::Overview | CameraMode::WorkspaceOverview => {
                        self.focus_manager.exit_overview(&mut self.camera);
                        info!("overview mode off");
                    }
                    _ => {
                        self.enter_overview();
                    }
                }
            }
            ToggleWorkspaceOverview => {
                match self.focus_manager.camera_mode {
                    CameraMode::WorkspaceOverview => {
                        self.focus_manager.exit_overview(&mut self.camera);
                        info!("workspace overview off");
                    }
                    _ => {
                        self.enter_workspace_overview();
                    }
                }
            }
            WorkspaceNext => {
                self.next_workspace();
            }
            WorkspacePrev => {
                self.previous_workspace();
            }
            AppNext | AppPrev => {
                // Alt+Tab is already handled above, but this catches
                // any other key bound to app switching
            }
            DeEmphasize => {
                if let Some(vid) = self.scene.selected_id {
                    if self.scene.is_de_emphasized(vid) {
                        self.scene.restore_from_de_emphasis(vid);
                        info!(?vid, "restored from de-emphasis");
                    } else {
                        self.scene.de_emphasize(vid);
                        info!(?vid, "de-emphasized");
                    }
                }
            }
            FrameSelected => {
                self.frame_selected();
            }
            FrameAll => {
                self.frame_all();
            }
            ResetCamera => {
                self.reset_camera();
            }
            Escape => {
                self.handle_escape();
            }
            ToggleShelf => {
                self.shelf.toggle_visibility();
                info!(visible = self.shelf.visible, "shelf toggled");
            }
            SendToShelf => {
                if let Some(vid) = self.scene.selected_id {
                    if self.shelf.contains(vid) {
                        self.shelf.restore_from_shelf(&mut self.scene, vid);
                        info!(?vid, "restored from shelf");
                    } else {
                        self.shelf.send_to_shelf(&mut self.scene, vid);
                        info!(?vid, "sent to shelf");
                    }
                }
            }
            Launcher => {
                info!("launcher triggered");
            }
            CloseApp => {
                self.close_focused_app();
            }
            ToggleMaximize => {
                self.toggle_maximize_selected(crate::maximize::MaximizeSource::Compositor);
            }
            MinimizeSelected => {
                self.minimize_selected(crate::maximize::MinimizeSource::Compositor);
            }
            RestoreSelected => {
                self.restore_last_minimized(crate::maximize::MinimizeSource::Compositor);
            }
            ReopenClosed => {
                self.reopen_last_closed();
            }
            CycleVisuals => {
                self.cycle_visuals();
            }
            OpenContextMenu => {
                self.open_context_menu_on_focused();
            }
            HelpOverlay => {
                self.shelf.toggle_visibility();
                info!("help overlay toggled (using shelf for now)");
            }
        }
    }

    /// Handle the Escape key with deterministic priority.
    fn handle_escape(&mut self) {
        // If pointer is locked, unlock it first
        if self.pointer_constraints.pointer_locked {
            self.pointer_constraints.unlock();
            return;
        }

        use crate::focus::CameraMode;
        let in_workspace_overview = matches!(self.focus_manager.camera_mode, CameraMode::WorkspaceOverview);
        let in_overview = matches!(self.focus_manager.camera_mode, CameraMode::Overview);
        let in_focus = matches!(self.focus_manager.camera_mode, CameraMode::Focus(_));

        let action = crate::navigation::escape_chain(
            self.interaction.is_dragging(),
            in_workspace_overview,
            in_overview,
            in_focus,
        );
        info!(?action, "escape chain");
        match action {
            EscapeAction::CancelDrag => {
                self.interaction.handle_pointer_up();
            }
            EscapeAction::ExitWorkspaceOverview => {
                self.focus_manager.exit_overview(&mut self.camera);
            }
            EscapeAction::ExitOverview => {
                self.focus_manager.exit_overview(&mut self.camera);
            }
            EscapeAction::ExitFocus => {
                self.focus_manager.exit(&mut self.camera, &self.scene);
            }
            EscapeAction::ResetCamera => {
                self.reset_camera();
            }
        }
    }

    /// Open context menu on the focused visual (triggered by Menu key).
    pub fn open_context_menu_on_focused(&mut self) {
        if let Some(vid) = self.scene.focused_id {
            let (x, y) = (self.window_size.0 as f64 * 0.5, self.window_size.1 as f64 * 0.5);
            let ws_count = self.workspace_manager.len();
            self.context_menu.show(x, y, vid, ws_count);
            self.context_menu.set_maximize_label(self.is_maximized(vid));
            let m = crate::context_menu::MenuMetrics::for_framebuffer(self.window_size.0, self.window_size.1);
            info!(menu_width = m.menu_width, item_height = m.item_height, glyph_scale = m.glyph_scale,
                  fb_w = self.window_size.0, fb_h = self.window_size.1, "context menu metrics");
            info!(?vid, "context menu opened via keyboard");
        }
    }

    /// Close the focused application.
    pub fn close_focused_app(&mut self) {
        let Some(vid) = self.scene.focused_id else { return };
        if let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() {
            for t in &self.toplevels {
                if t.toplevel.wl_surface() == &wl_surface {
                    t.toplevel.send_close();
                    info!(?vid, "close sent to focused app");
                    return;
                }
            }
        }
    }

    /// Reopen the most recently closed window (I1).
    ///
    /// Relaunches the application via the launcher (matched by app id) and
    /// arms a pending reopen. When the new toplevel maps, its saved 3D
    /// transform and workspace are reattached (see handle_commit).
    pub fn reopen_last_closed(&mut self) -> bool {
        let Some(entry) = self.closed_windows.take_most_recent() else {
            info!("no closed window to reopen");
            return false;
        };
        // Re-scan desktop files if the launcher cache is empty.
        if self.launcher.applications.is_empty() {
            self.launcher.discover();
        }
        let match_idx = self.launcher.applications.iter().position(|e| {
            crate::closed::app_id_matches_entry(&entry.app_id, &e.app_id, &e.name)
        });
        let Some(idx) = match_idx else {
            info!(app_id = %entry.app_id, "no desktop file matches closed window, cannot reopen");
            return false;
        };
        let app_id = entry.app_id.clone();
        self.pending_reopen = Some(crate::closed::PendingReopen {
            app_id: app_id.clone(),
            transform: entry.transform.clone(),
            workspace: entry.workspace,
        });
        match self.launcher.launch(idx) {
            Some(_child) => {
                info!(app_id = %app_id, workspace = entry.workspace, "reopen launched");
                true
            }
            None => {
                warn!(app_id = %app_id, "reopen launch failed");
                self.pending_reopen = None;
                false
            }
        }
    }

    /// Cycle through visuals in the current workspace (Super+Tab).
    pub fn cycle_visuals(&mut self) -> bool {
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        if ws_ids.is_empty() {
            return false;
        }
        let current = self.scene.focused_id;
        let next = if let Some(cid) = current {
            // Find the current visual's position and pick the next one
            if let Some(pos) = ws_ids.iter().position(|id| *id == cid) {
                ws_ids[(pos + 1) % ws_ids.len()]
            } else {
                ws_ids[0]
            }
        } else {
            ws_ids[0]
        };
        self.set_keyboard_focus(Some(next));
        self.scene.select(Some(next));
        info!(?next, "cycled to visual");
        true
    }

    /// Reset the camera to its default position.
    pub fn reset_camera(&mut self) {
        self.camera.position = cgmath::Point3::new(0.0, 0.0, 800.0);
        self.camera.yaw = 0.0;
        self.camera.pitch = 0.0;
        info!("camera reset");
    }

    /// Recover from destroyed focus — if the focused visual no longer exists,
    /// clear focus state cleanly.
    pub fn recover_from_destroyed_focus(&mut self) {
        if let Some(vid) = self.scene.focused_id {
            if !self.scene.visuals.iter().any(|v| v.id == vid) {
                info!(?vid, "recovering from destroyed focus");
                self.set_keyboard_focus(None);
                self.scene.selected_id = None;
            }
        }
    }

    /// Cancel any active drag or grab interaction.
    pub fn cancel_interaction(&mut self) {
        if self.interaction.is_dragging() {
            self.interaction.handle_pointer_up();
            info!("interaction cancelled");
        }
    }

    /// Run full recovery: cancel interaction → exit focus → exit overview → reset camera.
    pub fn recover(&mut self) {
        use crate::focus::CameraMode;
        info!("full recovery sequence");

        self.cancel_interaction();

        if matches!(self.focus_manager.camera_mode, CameraMode::Focus(_)) {
            self.focus_manager.exit(&mut self.camera, &self.scene);
            info!("recovery: exited focus mode");
        }

        if matches!(self.focus_manager.camera_mode, CameraMode::Overview)
            || matches!(self.focus_manager.camera_mode, CameraMode::WorkspaceOverview)
        {
            self.focus_manager.exit_overview(&mut self.camera);
            info!("recovery: exited overview");
        }

        self.reset_camera();
    }

    /// Clear stale focus: verify focused visual still exists.
    /// Called after every render.
    pub fn clear_stale_focus(&mut self) {
        if let Some(focused) = self.scene.focused_id {
            if !self.scene.visuals.iter().any(|v| v.id == focused) {
                info!(?focused, "stale focus cleared after render");
                self.set_keyboard_focus(None);
            }
        }
    }
}

impl CompositorHandler for LookingGlass {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(
        &self,
        client: &'a Client,
    ) -> &'a CompositorClientState {
        let state: &ClientState = client.get_data().unwrap();
        &state.compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.handle_commit(surface);
    }
}

delegate_compositor!(LookingGlass);

impl XdgShellHandler for LookingGlass {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.cleanup();
        let mut info = ToplevelInfo::new(surface);
        info.toplevel.send_configure();
        info.lifecycle = SurfaceLifecycle::Configured;
        info!(
            app_id = %info.app_id,
            title = %info.title,
            "toplevel created"
        );
        self.toplevels.push(info);
    }

    fn new_popup(
        &mut self,
        surface: smithay::wayland::shell::xdg::PopupSurface,
        positioner: PositionerState,
    ) {
        let wl_surface = surface.wl_surface().clone();
        let parent_vid = find_parent_toplevel_vid(&self.toplevels, &self.popups, &surface);

        // Send initial configure to the popup
        let _ = surface.send_configure();

        let mut info = PopupInfo {
            popup: surface,
            wl_surface,
            parent_toplevel_vid: parent_vid,
            visual_id: None,
            lifecycle: SurfaceLifecycle::Created,
            size: None,
            positioner,
        };
        info!("popup created");
        self.popups.push(info);
    }

    fn grab(
        &mut self,
        _surface: smithay::wayland::shell::xdg::PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: Serial,
    ) {
        // For now, popup grabs are accepted. In a full implementation,
        // we'd validate the serial. But the popup is already created
        // by the client at this point.
        info!("popup grab accepted");
    }

    fn reposition_request(
        &mut self,
        surface: smithay::wayland::shell::xdg::PopupSurface,
        positioner: PositionerState,
        _token: u32,
    ) {
        // Update stored positioner state for this popup
        if let Some(info) = self.popups.iter_mut().find(|p| p.popup.wl_surface() == surface.wl_surface()) {
            info.positioner = positioner;
        }
        // Accept reposition requests by sending a configure
        let _ = surface.send_configure();
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>) {
        let vid = self.find_toplevel(surface.wl_surface()).and_then(|t| t.visual_id);
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Fullscreen);
        });
        let _ = surface.send_configure();
        info!(?vid, "fullscreen requested");
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let vid = self.find_toplevel(surface.wl_surface()).and_then(|t| t.visual_id);
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
        });
        let _ = surface.send_configure();
        info!(?vid, "unfullscreen requested");
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        // Client-initiated minimize (xdg_toplevel.set_minimized): same
        // compositor-side flow as Meta+Down — hide the visual, keep the
        // surface mapped, focus the next window. No state bit exists for
        // minimized (xdg-shell), so no configure is sent.
        let vid = self.find_toplevel(surface.wl_surface()).and_then(|t| t.visual_id);
        if let Some(vid) = vid {
            self.begin_minimize(vid, crate::maximize::MinimizeSource::Client);
        } else {
            info!("minimize request for unknown toplevel ignored");
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let vid = self.find_toplevel(surface.wl_surface()).and_then(|t| t.visual_id);
        match vid {
            Some(vid) => self.begin_maximize(vid, crate::maximize::MaximizeSource::Client),
            None => {
                info!("maximize request before the surface mapped; ignored");
            }
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let vid = self.find_toplevel(surface.wl_surface()).and_then(|t| t.visual_id);
        match vid {
            Some(vid) => self.begin_unmaximize(vid, crate::maximize::MaximizeSource::Client),
            None => {
                info!("unmaximize request before the surface mapped; ignored");
            }
        }
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        // I3a: mark our outstanding geometry request acknowledged when the
        // serial matches. Smithay remains authoritative for the protocol
        // queue; this only updates Veyra's intent.
        if let Configure::Toplevel(configure) = &configure {
            if self.client_resizes.note_ack(configure.serial) {
                info!(serial = ?configure.serial, "client resize acknowledged");
            }
        }
        info!(?configure, "configure acknowledged");
        // Client pacing (I3b): the completed transaction frees the surface
        // for the next configure when the session's desired size moved on.
        if let Configure::Toplevel(_) = &configure {
            let vid = self.toplevels.iter()
                .find(|t| t.toplevel.wl_surface() == &surface)
                .and_then(|t| t.visual_id);
            if let Some(vid) = vid {
                self.flush_resize_desired(vid);
                // I4: the freed surface may now take a deferred maximize.
                self.flush_deferred_maximize();
            }
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        // Extract values before mutable borrows to avoid borrow conflicts
        let (title, vid) = {
            let info = match self.find_toplevel(surface.wl_surface()) {
                Some(i) => i,
                None => return,
            };
            let old = info.title.clone();
            info.refresh_metadata();
            if info.title == old {
                return;
            }
            (info.title.clone(), info.visual_id)
        };
        info!(title = %title, "title changed");
        if let Some(vid) = vid {
            if let Some(visual) = self.scene.get_mut(vid) {
                visual.chrome.title = title;
            }
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        let (app_id, vid) = {
            let info = match self.find_toplevel(surface.wl_surface()) {
                Some(i) => i,
                None => return,
            };
            let old = info.app_id.clone();
            info.refresh_metadata();
            if info.app_id == old {
                return;
            }
            (info.app_id.clone(), info.visual_id)
        };
        info!(app_id = %app_id, "app_id changed");
        if let Some(vid) = vid {
            if let Some(visual) = self.scene.get_mut(vid) {
                visual.chrome.app_id = app_id.clone();
            }
            // Register with the application switcher
            self.app_switcher.register_visual(&app_id, vid);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        if let Some(idx) = self
            .toplevels
            .iter()
            .position(|t| t.toplevel.wl_surface() == wl_surface)
        {
            let mut info = self.toplevels.remove(idx);
            info.lifecycle = SurfaceLifecycle::Destroyed;
            if let Some(vid) = info.visual_id {
                let was_focused = self.scene.focused_id == Some(vid)
                    || self.scene.selected_id == Some(vid);
                // Drop any outstanding geometry request for the dead surface (I3a).
                self.client_resizes.abort(vid);
                // Drop any outstanding maximize intent for the dead surface (I4).
                self.maximize.abort(vid);
                // Record a tombstone before cleanup so the window can be
                // reopened with its transform and workspace (I1).
                let transform = self.scene.get_mut(vid).map(|v| v.transform.clone());
                if let Some(transform) = transform {
                    let ws_idx = self.workspace_for_visual(vid)
                        .unwrap_or_else(|| self.workspace_manager.active_id());
                    self.closed_windows.record(crate::closed::ClosedWindow {
                        app_id: info.app_id.clone(),
                        title: info.title.clone(),
                        workspace: ws_idx,
                        transform,
                        closed_at_ms: now_ms() as u64,
                    });
                }
                self.app_switcher.unregister_visual(&info.app_id, vid);
                self.shelf.remove(vid);
                cleanup_visual_permanently(self, vid);
                if was_focused {
                    self.refocus_after_close();
                }
            }
            info!(
                app_id = %info.app_id,
                title = %info.title,
                "surface destroyed"
            );
        }
    }
}

delegate_xdg_shell!(LookingGlass);

impl OutputHandler for LookingGlass {}

/// Find which toplevel (or popup) visual is the parent of a given popup surface.
fn find_parent_toplevel_vid(
    toplevels: &[ToplevelInfo],
    popups: &[PopupInfo],
    popup: &smithay::wayland::shell::xdg::PopupSurface,
) -> Option<VisualId> {
    let parent_surface = popup.get_parent_surface()?;
    for t in toplevels {
        if t.toplevel.wl_surface() == &parent_surface {
            return t.visual_id;
        }
    }
    for p in popups {
        if p.wl_surface == parent_surface {
            return p.visual_id;
        }
    }
    None
}

/// Clean up any popup info entries whose visual_id matches the given vid.
fn cleanup_popups_by_vid(state: &mut LookingGlass, vid: VisualId) {
    // Find popups that reference this vid as parent or have this vid
    let popup_ids: Vec<VisualId> = state.popups.iter()
        .filter(|p| p.visual_id == Some(vid) || p.parent_toplevel_vid == Some(vid))
        .filter_map(|p| p.visual_id)
        .collect();
    // Remove their visuals from scene
    for pvid in &popup_ids {
        state.scene.remove(*pvid);
        state.wayland_surfaces.remove(pvid);
    }
    // Remove from tracking list
    state.popups.retain(|p| {
        p.visual_id != Some(vid) && p.parent_toplevel_vid != Some(vid)
    });
}

/// Clean up a visual from ALL workspaces, focus, interaction, snap, and scene state.
/// This is used by the XdgShellHandler when a toplevel is destroyed.
fn cleanup_visual_permanently(state: &mut LookingGlass, vid: VisualId) {
    // Clean up child popups first
    cleanup_popups_by_vid(state, vid);
    // Drop any outstanding client geometry request (I3a)
    state.client_resizes.abort(vid);
    // Drop any outstanding maximize intent (I4)
    state.maximize.abort(vid);
    // Drop a resize session targeting the removed visual (I3b)
    if state.resize_session.as_ref().map_or(false, |s| s.vid == vid) {
        state.resize_session = None;
    }
    // Remove from all workspaces
    for i in 0..state.workspace_manager.len() {
        if let Some(ws) = state.workspace_manager.get_mut(i) {
            ws.remove(vid);
        }
    }
    // Audit fix (P2): drop the InputSink registration; leaving it behind
    // leaked the sink (and the Wayland proxies it holds) on every
    // open/close cycle in long sessions.
    state.input_sinks.remove(&vid);
    // Clean up focus state
    state.scene.remove(vid);
    state.wayland_surfaces.remove(&vid);
    // Clean up interaction state
    if state.interaction.is_dragging_visual(vid) {
        state.interaction.handle_pointer_up();
    }
    if state.scene.selected_id == Some(vid) {
        state.scene.selected_id = None;
    }
    // Clean up focus manager
    if state.focus_manager.focus_target == Some(vid) {
        let mut saved = Camera::new();
        std::mem::swap(&mut saved, &mut state.camera);
        state.focus_manager.exit(&mut saved, &state.scene);
        std::mem::swap(&mut saved, &mut state.camera);
    }
    // Clean up overview if focused on that visual
    if matches!(state.focus_manager.camera_mode, CameraMode::Focus(t) if t == vid) {
        state.focus_manager.camera_mode = CameraMode::Normal;
        state.focus_manager.transition = None;
    }
    // Remove from all groups
    state.scene.remove_from_all_groups(vid);
}

impl SeatHandler for LookingGlass {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    fn led_state_changed(&mut self, _seat: &Seat<Self>, _led_state: LedState) {}
}

delegate_seat!(LookingGlass);

impl ShmHandler for LookingGlass {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SelectionHandler for LookingGlass {
    type SelectionUserData = ();
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<smithay::wayland::selection::SelectionSource>, _seat: Seat<Self>) {
        // Collect MIME types from the source so clients can negotiate formats
        let mime_types: Vec<String> = source
            .as_ref()
            .map(|s| s.mime_types().clone())
            .unwrap_or_default();
        match ty {
            SelectionTarget::Clipboard => {
                if let Some(ref seat) = self.seat {
                    let dh = &self.display_handle;
                    smithay::wayland::selection::data_device::set_data_device_selection::<Self>(
                        dh, seat, mime_types, (),
                    );
                }
            }
            SelectionTarget::Primary => {
                if let Some(ref seat) = self.seat {
                    let dh = &self.display_handle;
                    smithay::wayland::selection::primary_selection::set_primary_selection::<Self>(
                        dh, seat, mime_types, (),
                    );
                }
            }
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: std::os::unix::io::OwnedFd,
        _seat: Seat<Self>,
        _user_data: &Self::SelectionUserData,
    ) {
        // When a client requests clipboard data, forward the request
        // to the currently active selection source via Smithay's free functions.
        if let Some(ref seat) = self.seat {
            match ty {
                SelectionTarget::Clipboard => {
                    let _ = smithay::wayland::selection::data_device::request_data_device_client_selection::<Self>(
                        seat, mime_type, fd,
                    );
                }
                SelectionTarget::Primary => {
                    let _ = smithay::wayland::selection::primary_selection::request_primary_client_selection::<Self>(
                        seat, mime_type, fd,
                    );
                }
            }
        }
    }
}

impl ClientDndGrabHandler for LookingGlass {
    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        info!("DnG grab ended");
        // Clear any spatial interaction state if a DnD operation was in progress
        if self.interaction.is_dragging() {
            self.interaction.handle_pointer_up();
        }
        self.schedule_render();
    }
}

impl ServerDndGrabHandler for LookingGlass {
    fn dropped(&mut self, _seat: Seat<Self>) {
        info!("server DnG operation ended");
        self.schedule_render();
    }
}

impl DataDeviceHandler for LookingGlass {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for LookingGlass {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl BufferHandler for LookingGlass {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

/// Load system keyboard configuration from /etc/default/keyboard or environment.
/// Falls back to XkbConfig::default() which uses XKB_DEFAULT_* env vars.
fn load_system_xkb_config() -> smithay::input::keyboard::XkbConfig<'static> {
    // Try /etc/default/keyboard first (Debian/Ubuntu)
    let etc_path = Path::new("/etc/default/keyboard");
    if etc_path.exists() {
        if let Ok(content) = fs::read_to_string(etc_path) {
            let mut layout = String::new();
            let mut variant = String::new();
            let mut options = String::new();
            let mut model = String::new();
            for line in content.lines() {
                let line = line.trim();
                if let Some(val) = line.strip_prefix("XKBLAYOUT=") {
                    layout = val.trim_matches('"').to_string();
                } else if let Some(val) = line.strip_prefix("XKBVARIANT=") {
                    variant = val.trim_matches('"').to_string();
                } else if let Some(val) = line.strip_prefix("XKBOPTIONS=") {
                    options = val.trim_matches('"').to_string();
                } else if let Some(val) = line.strip_prefix("XKBMODEL=") {
                    model = val.trim_matches('"').to_string();
                }
            }
            if !layout.is_empty() {
                return smithay::input::keyboard::XkbConfig {
                    rules: "",
                    model: Box::leak(model.into_boxed_str()),
                    layout: Box::leak(layout.into_boxed_str()),
                    variant: Box::leak(variant.into_boxed_str()),
                    options: if options.is_empty() { None } else { Some(options) },
                };
            }
        }
    }
    // Fallback to US layout if nothing else is configured
    smithay::input::keyboard::XkbConfig {
        rules: "",
        model: "",
        layout: "us",
        variant: "",
        options: None,
    }
}

delegate_shm!(LookingGlass);
delegate_output!(LookingGlass);
delegate_data_device!(LookingGlass);
delegate_primary_selection!(LookingGlass);
delegate_pointer_constraints!(LookingGlass);
delegate_relative_pointer!(LookingGlass);
delegate_dmabuf!(LookingGlass);

#[cfg(test)]
mod damage_tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> smithay::utils::Rectangle<i32, smithay::utils::Buffer> {
        smithay::utils::Rectangle::new(
            smithay::utils::Point::new(x, y),
            smithay::utils::Size::new(w, h),
        )
    }

    #[test]
    fn no_last_size_means_full_upload() {
        let d = vec![rect(0, 0, 100, 100)];
        assert!(LookingGlass::sanitize_damage(d, None).is_empty());
    }

    #[test]
    fn damage_within_buffer_is_kept() {
        let d = vec![rect(10, 10, 100, 50)];
        let out = LookingGlass::sanitize_damage(d, Some((696, 432)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size.w, 100);
        assert_eq!(out[0].size.h, 50);
    }

    #[test]
    fn oversized_damage_is_clamped() {
        // Client raced its resize: 1280-wide damage against a 696-wide buffer
        let d = vec![rect(0, 0, 1280, 768)];
        let out = LookingGlass::sanitize_damage(d, Some((696, 432)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size.w, 696);
        assert_eq!(out[0].size.h, 432);
    }

    #[test]
    fn negative_offset_damage_clamps_origin() {
        let d = vec![rect(-4, -8, 100, 100)];
        let out = LookingGlass::sanitize_damage(d, Some((696, 432)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].loc.x, 0);
        assert_eq!(out[0].loc.y, 0);
        assert_eq!(out[0].size.w, 96);
        assert_eq!(out[0].size.h, 92);
    }

    #[test]
    fn fully_outside_damage_is_dropped() {
        // x beyond width; y beyond height — both no-overlap rects vanish
        let d = vec![rect(700, 0, 100, 100), rect(0, 500, 696, 464)];
        let out = LookingGlass::sanitize_damage(d, Some((696, 432)));
        assert!(out.is_empty());
    }

    #[test]
    fn partially_overlapping_damage_is_clamped() {
        // Real-world case: xoffset 1 + width 1278 > 696
        let d = vec![rect(1, 0, 1278, 432)];
        let out = LookingGlass::sanitize_damage(d, Some((696, 432)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].loc.x, 1);
        assert_eq!(out[0].size.w, 695);
    }

    #[test]
    fn empty_damage_rects_are_dropped() {
        // Real-world case: yoffset 464 + height 0 > 432 GL spam
        let d = vec![rect(0, 464, 0, 0), rect(1, 1, 10, 10)];
        let out = LookingGlass::sanitize_damage(d, Some((696, 432)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size.w, 10);
    }

    #[test]
    fn all_invalid_damage_degenerates_to_full_upload() {
        let d = vec![rect(700, 0, 100, 100)];
        assert!(LookingGlass::sanitize_damage(d, Some((696, 432))).is_empty());
    }
}
