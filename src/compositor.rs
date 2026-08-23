//! Wayland protocol integration and central compositor state.

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportMemWl;
use smithay::backend::renderer::Texture;
use smithay::backend::SwapBuffersError;
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::delegate_compositor;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::delegate_data_device;
use smithay::delegate_output;
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
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::{SelectionHandler, SelectionTarget};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::DisplayHandle;
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
use std::time::SystemTime;

use cgmath::Matrix4;

use crate::focus::FocusManager;
use crate::input::Camera;
use crate::input_router::{self, InputSink, KeyboardEvent, PointerEventKind};
use crate::interaction::InteractionController;
use crate::layout;
use crate::perf::PerfStats;
use crate::workspace::WorkspaceManager;
use crate::producer::{FrameProducer, FrameResult};
use crate::scene::{Scene, Visual, VisualContent, VisualId};
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
    pub shm_state: ShmState,
    pub data_device_state: DataDeviceState,
    pub backend: Option<WinitGraphicsBackend<GlesRenderer>>,
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
    /// Modifier key state for keyboard shortcuts.
    ctrl_pressed: bool,
    shift_pressed: bool,
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
        backend: WinitGraphicsBackend<GlesRenderer>,
    ) -> Self {
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(display_handle);
        let shm_state = ShmState::new::<Self>(display_handle, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(display_handle);
        let mut seat_state = SeatState::new();

        // Create a seat and pointer/keyboard handles for Wayland input routing
        // Use new_wl_seat to register the wl_seat global (new_seat doesn't register it)
        let mut seat_actual = seat_state.new_wl_seat(display_handle, "default");
        let pointer_handle = Some(seat_actual.add_pointer());
        // Keyboard handle may fail if no keymap is available; that's OK
        let _keyboard_result = seat_actual.add_keyboard(smithay::input::keyboard::XkbConfig::default(), 0, 0);
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
            backend: Some(backend),
            toplevels: Vec::new(),
            popups: Vec::new(),
            scene: Scene::default(),
            camera: Camera::new(),
            spatial_mode: true,
            workspace_manager: WorkspaceManager::new(3),
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
        }
    }

    /// Load saved workspace state from disk and apply camera.
    pub fn load_saved_state(&mut self) {
        if crate::persist::exists() {
            match crate::persist::load() {
                Ok(state) => {
                    let count = state.workspace_count();
                    self.saved_state = Some(state);
                    if let Some(ref s) = self.saved_state {
                        s.apply_camera(&mut self.camera);
                        info!(workspaces = count, "workspace state loaded");
                    }
                }
                Err(e) => info!(?e, "no saved workspace state to load"),
            }
        }
    }

    /// Save current workspace state to disk.
    pub fn save_state(&self) {
        let ws = self.workspace_manager.active();
        let state = crate::persist::WorkspaceState::capture(
            &self.scene,
            &self.camera,
            ws.layout_mode,
            &self.scene.detached_set,
            &ws.visual_ids,
        );
        match crate::persist::save(&state) {
            Ok(()) => info!("workspace state saved"),
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
            let result = with_states(surface, |states| {
                renderer.import_shm_buffer(&wl_buffer, Some(states), &damage)
            });
            match result {
                Ok(texture) => {
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
                                    if let Some(visual) = self.scene.get_mut(vid) {
                                        if let Some(dst) = visual.texture_mut() {
                                            *dst = texture;
                                        }
                                        visual.geometry = smithay::utils::Rectangle::new(
                                            smithay::utils::Point::new(0, 0),
                                            smithay::utils::Size::new(tex_size.w, tex_size.h),
                                        );
                                        self.workspace_manager.active_mut().add(vid);
                                        info!(?vid, "popup remapped");
                                    }
                                }
                            } else {
                                let parent_vid = self.popups[popup_idx].parent_toplevel_vid;
                                let mut visual = Visual::new(
                                    VisualContent::WaylandSurface(texture),
                                    smithay::utils::Rectangle::new(
                                        smithay::utils::Point::new(0, 0),
                                        smithay::utils::Size::new(tex_size.w, tex_size.h),
                                    ),
                                );
                                // Position popup relative to parent with a simple offset
                                if let Some(pvid) = parent_vid {
                                    if let Some(parent) = self.scene.visuals.iter().find(|v| v.id == pvid) {
                                        visual.transform.position = parent.transform.position
                                            + cgmath::Vector3::new(100.0, -50.0, 10.0); // offset right and slightly down, in front
                                        visual.parent = Some(pvid);
                                    }
                                }
                                let visual_id = visual.id;
                                self.popups[popup_idx].visual_id = Some(visual_id);
                                self.wayland_surfaces.insert(visual_id, surface.clone());
                                self.scene.add(visual);
                                // Add to the same workspace as the parent (or active workspace)
                                if let Some(pvid) = parent_vid {
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
                                info!(?visual_id, ?parent_vid, "popup mapped");
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
                                info!(?visual_id, app_id = %self.toplevels[idx].app_id, "surface mapped");
                            }
                        }
                    } else if let Some(vid) = existing_vid {
                        if let Some(visual) = self.scene.get_mut(vid) {
                            if let Some(dst) = visual.texture_mut() {
                                *dst = texture;
                            }
                        }
                    }
                }
                Err(e) => warn!(?e, "SHM import failed"),
            }
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

    pub fn render(&mut self) {
        use crate::perf::PipelineStage;
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
        let (w, h) = self.window_size;
        let world_w = w;
        let world_h = h;
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

        // Step 4: Camera + render
        let Some(backend) = self.backend.as_mut() else { return; };
        if !self.spatial_mode {
            self.camera.position = cgmath::Point3::new(0.0, 0.0, 500.0);
            self.camera.yaw = 0.0;
            self.camera.pitch = 0.0;
        } else if self.workspace_manager.active().auto_orbit {
            let t = (self.perf.frame_count as f32) * 0.003;
            self.camera.yaw = t.cos() * 0.8;
            self.camera.pitch = (t * 0.5).sin() * 0.3 + 0.2;
        }
        // Focus mode interpolates the camera toward the focused visual
        let render_camera = self.focus_manager.interpolated_camera(&self.camera, &self.scene);
        let (w, h) = self.window_size;
        let view = render_camera.view_matrix();
        let proj = if self.spatial_mode {
            cgmath::perspective(cgmath::Deg(45.0), w / h, 1.0, 10000.0)
        } else {
            cgmath::ortho(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        };
        let ws_visible = Some(self.workspace_manager.active().visual_ids.as_slice());
        if let Err(SwapBuffersError::ContextLost(e)) = renderer::render_scene(backend, &self.scene, &view, &proj, &mut self.perf, ws_visible) {
            error!(?e, "Context lost");
            self.backend = None;
        }

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
    /// Updates scene focus, Wayland keyboard focus, FocusManager, and SpatialChrome consistently.
    fn set_keyboard_focus(&mut self, vid: Option<VisualId>) {
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

        // Update SpatialChrome on visuals
        for visual in &mut self.scene.visuals {
            visual.chrome.focused = Some(visual.id) == vid;
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
                    let mot_ev = MotionEvent {
                        location: pos,
                        serial,
                        time,
                    };
                    match kind {
                        PointerEventKind::Motion => {
                            ph.motion(self, Some((wl_surface.clone(), pos)), &mot_ev);
                        }
                        PointerEventKind::Down | PointerEventKind::Up => {
                            ph.motion(self, Some((wl_surface.clone(), pos)), &mot_ev);
                            ph.button(self, &btn_ev);
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
            let _ = kh_handle.input::<(), _>(
                self,
                Keycode::new(key as u32),
                state,
                serial,
                time,
                |_, _, _| FilterResult::Forward,
            );
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

    /// Public entry point for a pointer button press.
    pub fn handle_pointer_down(&mut self, x: f64, y: f64, shift: bool, ctrl: bool, alt: bool) {
        self.press_pos = (x, y);
        self.event_serial = self.event_serial.wrapping_add(1);
        self.interaction.window_size = self.window_size;
        let mode = self.interaction.handle_pointer_down(
            x, y, &mut self.scene, &self.camera, self.spatial_mode, shift, ctrl, alt,
        );
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
                        self.interaction.handle_pointer_down(
                            x, y, &mut self.scene, &self.camera,
                            self.spatial_mode, false, false, false,
                        );
                        // Force translate even though no modifier
                        self.interaction.force_translate(x, y, &mut self.scene,
                            &self.camera, self.spatial_mode);
                    }
                    _ => {}
                }
            }
        }
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
    }

    /// Public entry point for pointer motion.
    pub fn handle_pointer_move(&mut self, x: f64, y: f64) {
        let dx = x - self.last_mouse.0;
        let dy = y - self.last_mouse.1;
        self.last_mouse = (x, y);
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
        let mot_ev = MotionEvent {
            location: pos,
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
        if self.focus_manager.focus_mode {
            // Exit focus mode — restore previous camera
            self.focus_manager.exit(&mut self.camera, &self.scene);
            info!("focus mode off");
        } else {
            // Enter focus mode — save camera, target focused visual
            let Some(vid) = self.scene.focused_id else {
                info!("no focused visual to focus on");
                return;
            };
            self.focus_manager.enter(&self.camera, vid);
            info!(?vid, "focus mode on");
        }
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
    /// Tab key (23) is always consumed by the compositor for spatial mode toggle.
    pub fn handle_key(&mut self, linux_key: u32, pressed: bool) {
        // Track modifier keys (Linux keycodes: 29=Left Ctrl, 97=Right Ctrl, 42=Left Shift, 54=Right Shift)
        match linux_key {
            29 | 97 => { self.ctrl_pressed = pressed; }
            42 | 54 => { self.shift_pressed = pressed; }
            _ => {}
        }

        if pressed {
            self.workspace_manager.active_mut().auto_orbit = false;
        }
        tracing::debug!(?linux_key, pressed, ctrl = self.ctrl_pressed, shift = self.shift_pressed, "KEY EVENT");

        if pressed {
            // Ctrl+Tab -> next workspace, Ctrl+Shift+Tab -> previous workspace
            if linux_key == 23 && self.ctrl_pressed {
                if self.shift_pressed {
                    self.previous_workspace();
                } else {
                    self.next_workspace();
                }
                return;
            }
            // F1/F2/F3 -> switch workspaces 0/1/2 (X11 keycodes 67=F1, 68=F2, 69=F3)
            match linux_key {
                67 => { self.activate_workspace(0); return; }
                68 => { self.activate_workspace(1); return; }
                69 => { self.activate_workspace(2); return; }
                _ => {}
            }
            // Tab (23) or F5 (71) — spatial mode toggle (but not with Ctrl)
            // F5 is used because TigerVNC intercepts Tab for internal focus switching
            if (linux_key == 23 || linux_key == 71) && !self.ctrl_pressed {
                self.spatial_mode = !self.spatial_mode;
                tracing::info!(spatial_mode = self.spatial_mode, "mode toggled by key {}", linux_key);
                return;
            }
            // F6 (72) — toggle focus mode
            if linux_key == 72 {
                self.toggle_focus_mode();
                return;
            }
            // F — frame selected visual
            if linux_key == 33 {
                self.frame_selected();
                return;
            }
            // Home — frame all visuals
            if linux_key == 102 {
                self.frame_all();
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
        _positioner: PositionerState,
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
        _positioner: PositionerState,
        _token: u32,
    ) {
        // Accept reposition requests by sending a configure
        let _ = surface.send_configure();
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
                visual.chrome.app_id = app_id;
            }
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
    fn new_selection(&mut self, _ty: SelectionTarget, _source: Option<smithay::wayland::selection::SelectionSource>, _seat: Seat<Self>) {}
}

impl ClientDndGrabHandler for LookingGlass {}
impl ServerDndGrabHandler for LookingGlass {}

impl DataDeviceHandler for LookingGlass {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl BufferHandler for LookingGlass {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

delegate_shm!(LookingGlass);
delegate_output!(LookingGlass);
delegate_data_device!(LookingGlass);
