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
            context_menu: ContextMenu::new(),
            config: config.clone(),
            session: Session::new(config.clone()),
            recovery: Recovery::new(),
            scheduler: RenderScheduler::new(),
            pointer_constraints: crate::pointer_constraints::PointerConstraints::new(display_handle),
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
                                if restored.is_none() {
                                    let pos = layout::place_new_visual(
                                        tex_size.w as f32 * visual.transform.scale.x,
                                        tex_size.h as f32 * visual.transform.scale.y,
                                        &self.scene,
                                    );
                                    visual.transform.position = pos;
                                    visual.transform.rotation = cgmath::Quaternion::from_angle_y(Deg(angle_y));
                                }
                                let visual_id = visual.id;
                                self.toplevels[idx].visual_id = Some(visual_id);
                                self.wayland_surfaces.insert(visual_id, surface.clone());
                                self.scene.add(visual);
                                self.workspace_manager.active_mut().add(visual_id);
                                self.scene.focus(Some(visual_id));
                                self.app_switcher.register_visual(
                                    &self.toplevels[idx].app_id,
                                    visual_id,
                                );
                                info!(?visual_id, app_id = %self.toplevels[idx].app_id, "surface mapped");
                            }
                        }
                    } else if let Some(vid) = existing_vid {
                        if let Some(visual) = self.scene.get_mut(vid) {
                            if let Some(dst) = visual.texture_mut() {
                                *dst = texture;
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

        // Check if a render is actually needed
        if !self.scheduler.needs_render() {
            self.perf.record_dropped();
            self.perf.record_idle();
            return;
        }

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
        let detached = self.scene.detached_set.clone();
        let layout_mode = self.workspace_manager.active().layout_mode;
        layout::apply_layout(
            &mut self.scene,
            layout_mode,
            &layout::LayoutConfig::default(),
            &detached,
            world_w,
            world_h,
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
            (v.transform.clone(), v.total_width(), v.total_height(), v.decoration.title_bar_height)
        });
        let Some((transform, gw, gh, title_h)) = data else { return ContentRouting::NoTarget };

        if let Some((u, v)) = input_router::screen_to_visual_uv(
            &pv, ndc_x, ndc_y, &transform, gw, gh,
        ) {
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
                            ph.motion(self, Some((wl_surface.clone(), pos)), &mot_ev);
                            ph.frame(self);
                        }
                        PointerEventKind::Down | PointerEventKind::Up => {
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
        let ws_ids = self.workspace_manager.active().visual_ids.as_slice();
        let picked = self.scene.pick_visible(&pv, ndc_x, ndc_y, ws_ids);

        if let Some((vid, _)) = picked {
            let ws_count = self.workspace_manager.len();
            self.context_menu.show(x, y, vid, ws_count);
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
                let detached = self.scene.detached_set.clone();
                let (ww, wh) = self.window_size;
                layout::apply_layout(&mut self.scene, mode, &layout::LayoutConfig::default(), &detached, ww, wh);
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
        let menu_width = 220.0;
        let item_height = 24.0;
        if let Some(idx) = self.context_menu.item_at(x, y, menu_width, item_height) {
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

    /// Route hover (pointer motion without button) to the visual under cursor.
    /// Picks the visual via 3D ray cast, computes UV, emits Wayland pointer
    /// motion events. Correctly handles enter/leave transitions via Smithay's
    /// PointerHandle::motion (changing the focus surface triggers leave/enter).
    /// Route hover (pointer motion without button) to Wayland surfaces.
    /// Updates pointer focus based on 3D ray hit testing. Only emits
    /// enter/leave transitions when the hovered surface ACTUALLY changes
    /// (debounces against last_wayland_focus). Never clears focus from
    /// hover alone — that avoids flickering with non-Wayland visuals.
    fn route_hover(&mut self, x: f64, y: f64) {
        let (w, h) = self.window_size;
        if w <= 0.0 || h <= 0.0 { return; }
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();

        let ph = match self.pointer_handle.clone() {
            Some(ph) => ph,
            None => return,
        };

        // Pick the visual under cursor via 3D ray cast, filtered by active workspace
        let ws_visible = self.workspace_manager.active().visual_ids.as_slice();
        let picked = self.scene.pick_visible(&pv, ndc_x, ndc_y, ws_visible);
        if let Some((vid, _)) = picked {
            self.scene.hovered_id = Some(vid);
        } else {
            self.scene.hovered_id = None;
        }

        let (vid, wl_surface, pos) = match picked {
            Some((vid, _)) if self.scene.is_active(vid) => {
                if let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() {
                    if let Some(v) = self.scene.visuals.iter().find(|v| v.id == vid) {
                        let transform = v.transform.clone();
                        let total_w = v.total_width();
                        let total_h = v.total_height();
                        if let Some((u, uv)) = input_router::screen_to_visual_uv(
                            &pv, ndc_x, ndc_y, &transform, total_w, total_h,
                        ) {
                            let title_frac = 0.06f64 / 1.06f64;
                            if uv < title_frac {
                                self.scene.hovered_id = None;
                                return;
                            } // title bar
                            let content_v = (uv - title_frac) / (1.0 - title_frac);
                            let px = u.clamp(0.0, 1.0) * v.geometry.size.w as f64;
                            let py = content_v.clamp(0.0, 1.0) * v.geometry.size.h as f64;
                            let pos: smithay::utils::Point<f64, smithay::utils::Logical> = (px, py).into();
                            (vid, wl_surface, pos)
                        } else { return; }
                    } else { return; }
                } else { return; }
            }
            _ => return, // no visual, no Wayland surface, or inactive
        };

        // Track last surface for later reference. PointerHandle::motion
        // handles enter/leave internally — same surface = motion;
        // different surface = leave old + enter new. We always call
        // motion to update cursor position within the current surface.
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
        // Track modifier keys (Linux keycodes: 29=Left Ctrl, 97=Right Ctrl,
        // 42=Left Shift, 54=Right Shift, 56=Left Alt, 100=Right Alt,
        // 125=Left Meta/Super, 126=Right Meta/Super)
        match linux_key {
            29 | 97 => { self.ctrl_pressed = pressed; }
            42 | 54 => { self.shift_pressed = pressed; }
            56 | 100 => {
                self.alt_pressed = pressed;
                if !pressed && self.alt_tab_active {
                    // Alt released — commit the Alt+Tab selection
                    self.alt_tab_active = false;
                    // Alt released after Alt+Tab — nothing else to do
                }
            }
            125 | 126 => {
                self.meta_pressed = pressed;
            }
            _ => {}
        }

        if pressed {
            self.workspace_manager.active_mut().auto_orbit = false;
        }

        // Track Alt+Tab state: while Alt is held, keep cycling
        if self.alt_pressed && linux_key == 23 && pressed {
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
            // Linux keycodes: 103=Up, 108=Down, 28=Enter, 1=Escape
            match linux_key {
                103 => { self.context_menu.select_prev(); return; }
                108 => { self.context_menu.select_next(); return; }
                28 => {
                    if let Some(action) = self.context_menu.confirm_selection() {
                        self.execute_menu_action(action);
                    }
                    return;
                }
                _ => {}
            }
        }

        if pressed {
            // F1/F2/F3 -> switch workspaces 0/1/2 (X11 keycodes 67=F1, 68=F2, 69=F3)
            match linux_key {
                67 => { self.activate_workspace(0); return; }
                68 => { self.activate_workspace(1); return; }
                69 => { self.activate_workspace(2); return; }
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
                return;
            }
        }

        // Camera keyboard controls only when no visual has focus
        if self.scene.focused_id.is_none() {
            self.camera.handle_key(linux_key, pressed, 1.0);
        }

        if pressed {
            // 1-9 — save bookmark; 0 — save slot 9
            if linux_key >= 2 && linux_key <= 10 {
                let slot = (linux_key - 2) as usize;
                if self.scene.selected_id.is_some() {
                    self.save_bookmark(slot);
                } else {
                    self.restore_bookmark(slot);
                }
                return;
            }
            if linux_key == 11 {
                if self.scene.selected_id.is_some() {
                    self.save_bookmark(9);
                } else {
                    self.restore_bookmark(9);
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
                self.spatial_mode = !self.spatial_mode;
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

    fn ack_configure(&mut self, _surface: WlSurface, configure: Configure) {
        info!(?configure, "configure acknowledged");
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
                self.app_switcher.unregister_visual(&info.app_id, vid);
                self.shelf.remove(vid);
                cleanup_visual_permanently(self, vid);
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
    // Remove from all workspaces
    for i in 0..state.workspace_manager.len() {
        if let Some(ws) = state.workspace_manager.get_mut(i) {
            ws.remove(vid);
        }
    }
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
    let config = smithay::input::keyboard::XkbConfig {
        rules: "",
        model: "",
        layout: "us",
        variant: "",
        options: None,
    };
    // Dump compiled keymap for debugging
    #[cfg(feature = "wayland_frontend")]
    {
        use xkbcommon::xkb;
        let context = xkb::Context::new();
        if let Ok(keymap) = xkb::Keymap::new_from_names(
            &context, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            if let Ok(text) = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1) {
                info!("veyra keymap: {} bytes", text.len());
                let _ = std::fs::write("/tmp/veyra-km.xkb", &text);
            }
        }
    }
    config
}

delegate_shm!(LookingGlass);
delegate_output!(LookingGlass);
delegate_data_device!(LookingGlass);
delegate_primary_selection!(LookingGlass);
delegate_pointer_constraints!(LookingGlass);
delegate_relative_pointer!(LookingGlass);
delegate_dmabuf!(LookingGlass);
