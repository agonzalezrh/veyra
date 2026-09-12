//! Wayland protocol integration and central compositor state.

use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportAll;
use smithay::backend::SwapBuffersError;

use crate::backend::PresentationBackend;
use smithay::backend::input::{KeyState, Keycode};
use smithay::delegate_compositor;
use smithay::delegate_data_device;
use smithay::delegate_dmabuf;
use smithay::delegate_fractional_scale;
use smithay::delegate_input_method_manager;
use smithay::delegate_output;
use smithay::delegate_pointer_constraints;
use smithay::delegate_primary_selection;
use smithay::delegate_relative_pointer;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_text_input_manager;
use smithay::delegate_viewporter;
use smithay::delegate_xdg_shell;
use smithay::input::keyboard::{
    FilterResult, KeyboardHandle, KeyboardTarget, KeysymHandle, LedState, ModifiersState,
};
use smithay::input::pointer::{ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle};
use smithay::input::Seat;
use smithay::input::SeatHandler;
use smithay::input::SeatState;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::IsAlive;
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::with_states;
use smithay::wayland::compositor::BufferAssignment;
use smithay::wayland::compositor::CompositorClientState;
use smithay::wayland::compositor::CompositorHandler;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::compositor::SurfaceAttributes;
use smithay::wayland::fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::ext_data_control::DataControlState as ExtDataControlState;
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::wlr_data_control::DataControlState as WlrDataControlState;
use smithay::wayland::selection::{SelectionHandler, SelectionTarget};
use smithay::wayland::shell::xdg::Configure;
use smithay::wayland::shell::xdg::PositionerState;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::XdgShellHandler;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use smithay::wayland::shm::ShmHandler;
use smithay::wayland::shm::ShmState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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
use crate::producer::{FrameProducer, FrameResult};
use crate::recovery::Recovery;
use crate::renderer;
use crate::scene::{DamageKind, Scene, Visual, VisualContent, VisualId};
use crate::scheduler::RenderScheduler;
use crate::session::Session;
use crate::shelf::SpatialShelf;
use crate::window::{PopupInfo, SurfaceLifecycle, ToplevelInfo};
use crate::workspace::WorkspaceManager;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
// P1 (audit): Session trait provides is_active() on the libseat handle
// used by backend recreation.
use smithay::backend::session::Session as _;

// G-B1 selection bookkeeping shared with `ClientData::disconnected`
// (which has no LookingGlass access): who owns each selection, the
// keyboard-focus client used to restore focus after the refresh, and
// a flag requesting the dead-selection cleanup broadcast. The toggle
// itself runs from the frame path — calling into smithay's seat state
// from inside the client-destruction dispatch context killed the
// compositor (re-entrant teardown).
static SELECTION_OWNER_CLIPBOARD: Mutex<Option<Client>> = Mutex::new(None);
static SELECTION_OWNER_PRIMARY: Mutex<Option<Client>> = Mutex::new(None);
static KBD_FOCUS_CLIENT: Mutex<Option<Client>> = Mutex::new(None);
static SELECTION_REFRESH_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// #12: input-serial ledger shared with `ClientData::disconnected`
// (which has no LookingGlass access) so a disconnecting client's
// entries are dropped with it. ClientIds are never reused — this is
// memory hygiene plus defense against any future id recycling.
static INPUT_SERIAL_LEDGER: std::sync::LazyLock<
    Mutex<HashMap<ClientId, std::collections::VecDeque<u32>>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// #12 ledger capacity: how many recent input serials per client are
/// kept for popup-grab validation.
pub const INPUT_SERIAL_LEDGER_CAP: usize = 16;

/// #12: push one input serial into a client's ledger (pure helper for
/// tests). Serial 0 is never a real input event — drop it.
fn ledger_push(ledger: &mut std::collections::VecDeque<u32>, serial: u32) {
    if serial == 0 {
        return;
    }
    ledger.push_back(serial);
    while ledger.len() > INPUT_SERIAL_LEDGER_CAP {
        ledger.pop_front();
    }
}

/// #12: validate against a client's ledger (pure helper for tests).
/// Serial 0 can never validate: it is never recorded and never a real
/// event serial.
fn ledger_contains(ledger: Option<&std::collections::VecDeque<u32>>, serial: u32) -> bool {
    serial != 0 && ledger.map(|q| q.contains(&serial)).unwrap_or(false)
}

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
        // G-B1: smithay does not clear a selection whose owning client
        // disconnected — paste-receiving clients would keep a dead
        // offer and hang waiting for data. If the owner just died,
        // toggle the data-device/primary focus (None → current) to
        // force smithay's dead-selection cleanup: the send_selection
        // broadcast detects the dead source, clears the selection and
        // sends Selection{null} to every device, then focus is
        // restored so the focused client is re-offered the (empty)
        // state.
        let was_clipboard = SELECTION_OWNER_CLIPBOARD
            .lock()
            .unwrap()
            .take_if(|c| c.id() == client_id)
            .is_some();
        let was_primary = SELECTION_OWNER_PRIMARY
            .lock()
            .unwrap()
            .take_if(|c| c.id() == client_id)
            .is_some();
        if was_clipboard || was_primary {
            SELECTION_REFRESH_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
            info!(
                ?client_id,
                "selection owner disconnected; refresh scheduled"
            );
        }
        // #12: drop the disconnected client's input-serial ledger.
        INPUT_SERIAL_LEDGER.lock().unwrap().remove(&client_id);
    }
}

#[allow(dead_code)] // reserved API surface; not yet wired
/// P1 (audit): which presentation backend family is in use. DRM can be
/// recreated in-session (try_new_with_session); winit cannot (the
/// calloop-registered event source owns the window and cannot be rebuilt
/// mid-run), so its context loss must fail loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendOrigin {
    Winit,
    Drm,
}

const BACKEND_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const BEGIN_FRAME_FAILURE_LIMIT: u32 = 3;
/// P2 (audit): a producer that fails this many CONSECUTIVE frames is
/// disconnected instead of erroring every frame forever. The threshold
/// tolerates periodic error emitters (SimulatedGlitch glitches every
/// 20th frame with successes in between — never consecutive).
const PRODUCER_ERROR_LIMIT: u32 = 60;

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
    /// P1 (audit): how the backend was created — decides whether a lost
    /// GL context can be recreated in-session. The DRM path can genuinely
    /// re-run `DrmGraphicsBackend::try_new_with_session`; the winit path
    /// cannot (the event loop owns the window), so failure must at least
    /// be LOUD instead of a silent no-op loop.
    pub backend_origin: Option<BackendOrigin>,
    /// Native mode only: a clone of the libseat session so backend
    /// recreation re-opens the session-owned DRM device after context
    /// loss (the old backend's DRM master died with it).
    pub drm_session: Option<smithay::backend::session::libseat::LibSeatSession>,

    /// P1 (audit): begin_frame failures are a state transition, not a
    /// warn loop — N consecutive failures drop the backend like
    /// ContextLost does.
    begin_frame_failures: u32,
    backend_lost_logged: bool,
    last_backend_attempt: Option<std::time::Instant>,
    pub toplevels: Vec<ToplevelInfo>,
    pub popups: Vec<PopupInfo>,
    pub scene: Scene,
    // G-E5.3: the live camera moved into OutputState (per-output
    // presentation view). Access with camera()/camera_mut().
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
    /// P2 (audit): consecutive-failure counters per producer — a
    /// persistently failing producer is disconnected instead of
    /// erroring every frame indefinitely.
    producer_error_counts: HashMap<VisualId, u32>,
    pub perf: PerfStats,
    pub output: Option<Output>,
    pub window_size: (f32, f32),
    /// #14 phase 1: the per-output state registry. The live single
    /// output registers here so the multi-monitor data layer is
    /// exercised; later phases migrate consumers off the scalar
    /// `window_size` onto per-output state.
    pub outputs: crate::outputs::OutputManager,
    pub last_mouse: (f64, f64),
    // Reserved API surface (relative-delta consumers); not read yet.
    #[allow(dead_code)]
    pub last_dx: f64,
    #[allow(dead_code)]
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
    /// P2 #9: per-GL-context render caches (DrawGl programs/VAOs, font
    /// atlas) — owned here so they reset exactly when the context does.
    pub render_caches: renderer::RenderCaches,
    /// P1 #2: true while the libseat session is paused (VT switch) —
    /// presentation is suspended until the seat reactivates.
    pub session_paused: bool,
    /// R6: wake handle pinging the event loop — dirty state renders
    /// immediately instead of waiting for the pacing timer.
    pub render_ping: Option<smithay::reexports::calloop::ping::Ping>,
    /// R6: pacing timer token (animations, pending frame callbacks).
    /// None while the compositor is idle — the timer source is dropped
    /// entirely, so an idle compositor wakes for nothing.
    pacing_timer: Option<smithay::reexports::calloop::RegistrationToken>,
    /// R6: true while the pacing timer will fire again (armed or
    /// self-rescheduling); false after it dropped on idle.
    pacing_active: bool,
    /// Modifier key state for keyboard shortcuts.
    ctrl_pressed: bool,
    shift_pressed: bool,
    alt_pressed: bool,
    meta_pressed: bool,
    /// G-C3: wp_viewporter global state.
    #[allow(dead_code)] // global registered on the display; the field is bookkeeping
    pub viewporter_state: ViewporterState,
    /// G-C3: wp_fractional_scale_manager_v1 global state.
    #[allow(dead_code)] // global registered on the display; the field is bookkeeping
    pub fractional_scale_state: FractionalScaleManagerState,
    /// G-C3: preferred scale advertised to clients (output scale).
    pub preferred_scale: f64,
    /// G-C4: xwayland-shell-v1 global state (X11 window association).
    pub xwayland_shell_state: XWaylandShellState,
    /// G-C4: the running X11 window manager, once XWayland is ready.
    pub x11_wm: Option<smithay::xwayland::xwm::X11Wm>,
    /// G-C4: X11 windows by their associated wl_surface (xwayland shell).
    pub x11_windows: HashMap<WlSurface, smithay::xwayland::xwm::X11Surface>,
    /// G-C4: X11 display number (for spawning X clients with DISPLAY).
    pub x11_display: Option<u32>,
    /// G-C4: the Wayland client identity of the XWayland server.
    pub xwayland_client: Option<Client>,
    /// G-C4: event-loop handle for selection transfers.
    pub loop_handle: Option<smithay::reexports::calloop::LoopHandle<'static, LookingGlass>>,
    /// G-C4: selections currently owned by an X client (clipboard, primary).
    /// SelectionTarget doesn't implement Hash, so ownership is tracked
    /// per target kind.
    pub x11_owns_clipboard: bool,
    pub x11_owns_primary: bool,
    /// G-D2: zwlr_data_control_manager_v1 (wlr clipboard managers).
    pub wlr_data_control_state: WlrDataControlState,
    /// G-D2: ext_data_control_manager_v1 (new ext clipboard managers).
    pub ext_data_control_state: ExtDataControlState,
    /// G-D3: ext_foreign_toplevel_list_v1 (docks/taskbars observing
    /// the compositor's toplevels).
    #[allow(dead_code)] // global registered on the display; the field is bookkeeping
    pub foreign_toplevel_state: smithay::wayland::foreign_toplevel_list::ForeignToplevelListState,
    /// #6: zwp_text_input_v3 manager state.
    #[allow(dead_code)] // global registered on the display; the field is bookkeeping
    pub text_input_state: smithay::wayland::text_input::TextInputManagerState,
    /// #6: zwp_input_method_v2 manager state.
    #[allow(dead_code)] // global registered on the display; the field is bookkeeping
    pub input_method_state: smithay::wayland::input_method::InputMethodManagerState,
    /// #6: IME popup surfaces (input_popup_surface_v2 role).
    pub ime_popups: Vec<smithay::wayland::input_method::PopupSurface>,
    /// #6: IME popup visuals by their wl_surface.
    pub ime_popup_visuals: HashMap<WlSurface, VisualId>,
    /// #6: the visual of the text field the IME popup anchors to
    /// (cached from parent_geometry during IME activation; Cell because
    /// the handler callback only receives &self).
    pub ime_parent_vid: std::cell::Cell<Option<VisualId>>,
    /// G-D3: foreign toplevel handles per visual.
    pub foreign_toplevels:
        HashMap<VisualId, smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle>,
    /// #11: subsurface visuals by their wl_surface.
    pub subsurface_visuals: HashMap<WlSurface, VisualId>,
    /// #11: subsurface → parent surface link, recorded at map time.
    /// smithay's get_parent cannot be trusted at destroy dispatch time
    /// (the client's objects are being torn down), so veyra keeps its
    /// own link for cleanup.
    pub subsurface_parents: HashMap<WlSurface, WlSurface>,
    /// #12: recent INPUT serials per client (pointer buttons + keyboard
    /// events). xdg_popup.grab must name the serial of the input event
    /// that triggered the popup — this ledger is the validation source.
    /// Capped per client; configures/frame callbacks never enter it.
    /// G-D4: wp_presentation global state.
    #[allow(dead_code)] // registered global / configuration snapshot
    pub presentation_state: smithay::wayland::presentation::PresentationState,
    /// G-D4: monotonic presentation sequence counter.
    presentation_seq: u64,
    /// Application switcher (Alt+Tab).
    pub app_switcher: ApplicationSwitcher,
    /// Application focus history: MRU ordering of focused toplevels
    /// (J1). Updated only on actual focus transitions; popups never
    /// enter. See crate::focus_history.
    pub focus_history: crate::focus_history::FocusHistory,
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
    #[allow(dead_code)] // registered global / configuration snapshot
    pub config: Config,
    /// Session lifecycle management.
    pub session: Session,
    /// Recovery operations for destroyed focus, corrupt state, etc.
    #[allow(dead_code)] // reserved API surface (Recovery module not yet wired)
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
    /// Outstanding fullscreen transitions (I7): intent + serial + the
    /// pre-fullscreen snapshot (see crate::fullscreen).
    pub fullscreen: crate::fullscreen::FullscreenCoordinator,
    /// In-progress pointer resize session (I3b), None when idle.
    pub resize_session: Option<crate::resize::ResizeSession>,
    /// Client-initiated wl_data_device drag in progress (G-B2). While
    /// set, compositor window manipulation is suppressed and pointer
    /// motion/release are always forwarded to the seat pointer so
    /// Smithay's DnDGrab can drive the protocol.
    pub dnd_active: bool,
    /// Relative pointer manager for sending relative motion deltas.
    #[allow(dead_code)] // global registered on the display; the field is bookkeeping
    pub relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState,
    /// DMA-BUF buffer import state.
    pub dmabuf_manager: crate::dmabuf::DmabufManager,
}

/// Result of routing a pointer event to the selected visual's content.
#[derive(Clone, Copy, PartialEq)]
enum ContentRouting {
    Routed,
    TitleBarHit,
    NoTarget,
}

/// Monotonic milliseconds timestamp for input events (R10).
///
/// Anchored to process start via `Instant` — wall-clock sources
/// (`SystemTime`) jump backwards on NTP syncs or manual clock changes,
/// which corrupts Wayland event and frame-callback timestamps. The u32
/// millisecond representation wraps every ~49.7 days by Wayland
/// definition; clients compare timestamps with wrapping arithmetic.
fn now_ms() -> u32 {
    static PROCESS_START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = PROCESS_START.get_or_init(std::time::Instant::now);
    std::time::Instant::now().duration_since(*start).as_millis() as u32
}

/// G-D4: CLOCK_MONOTONIC time since boot — the wp_presentation
/// protocol's timestamp domain (its global is created with
/// CLOCK_MONOTONIC; timestamps must come from the same clock).
fn monotonic_since_boot() -> (u32, u32) {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: plain clock read with a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u32, ts.tv_nsec as u32)
}

/// R6: cadence for continuous work — animation ticks and client
/// frame-callback completion. Matches the previous fixed timer.
const RENDER_PACING_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// R6: pacing timer callback — renders each tick and keeps the
/// cadence only while continuous work remains; otherwise disarms the
/// timer completely so an idle compositor neither renders nor wakes.
fn pacing_timer_callback(
    _deadline: std::time::Instant,
    _meta: &mut (),
    state: &mut LookingGlass,
) -> smithay::reexports::calloop::timer::TimeoutAction {
    use smithay::reexports::calloop::timer::TimeoutAction;
    state.render();
    if state.should_render() {
        TimeoutAction::ToDuration(RENDER_PACING_INTERVAL)
    } else {
        state.pacing_active = false;
        TimeoutAction::Drop
    }
}

/// G-C4: ownership marker for compositor-side (X11-bridged)
/// selections. Selections served by a Wayland client (wl_data_source)
/// never reach `send_selection` with this marker — smithay routes
/// those directly to the client source; only server-side selections
/// (X11-bridged) arrive here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionOwner;

/// G-D1: map a config scale to smithay's Scale — integral values keep
/// the wl_output.scale integer path, fractional ones use Fractional.
fn scale_from_f64(s: f64) -> Scale {
    if (s - s.round()).abs() < f64::EPSILON {
        Scale::Integer(s.round() as i32)
    } else {
        Scale::Fractional(s)
    }
}

impl LookingGlass {
    pub fn new(
        display_handle: &DisplayHandle,
        backend: Box<dyn PresentationBackend>,
        config: Config,
    ) -> Self {
        // G-E5.3: the camera is per-output — seed the registry with the
        // default primary output up front; the real mode/name arrive via
        // sync_output_mode at startup.
        let mut outputs = crate::outputs::OutputManager::new();
        outputs.add(crate::outputs::OutputState {
            name: "default".into(),
            mode: (1280, 720),
            refresh_mhz: 60000,
            scale: 1.0,
            global_pos: (0, 0),
            camera: Camera::new(),
        });
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(display_handle);
        let shm_state = ShmState::new::<Self>(display_handle, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(display_handle);
        let mut seat_state = SeatState::new();
        let primary_selection_state = PrimarySelectionState::new::<Self>(display_handle);
        // G-C3: fractional scaling + viewporter protocol support.
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(display_handle);
        let viewporter_state = ViewporterState::new::<Self>(display_handle);
        // G-C4: xwayland-shell-v1 (X11 window ↔ wl_surface association).
        let xwayland_shell_state = XWaylandShellState::new::<Self>(display_handle);
        // G-D2: data-control protocols for clipboard managers. Both
        // broadcast the same seat selection state as wl_data_device.
        let wlr_data_control_state =
            smithay::wayland::selection::wlr_data_control::DataControlState::new::<Self, _>(
                display_handle,
                Some(&primary_selection_state),
                |_| true,
            );
        let ext_data_control_state =
            smithay::wayland::selection::ext_data_control::DataControlState::new::<Self, _>(
                display_handle,
                Some(&primary_selection_state),
                |_| true,
            );
        // G-D3: ext_foreign_toplevel_list_v1 for docks/taskbars.
        let foreign_toplevel_state =
            smithay::wayland::foreign_toplevel_list::ForeignToplevelListState::new::<Self>(
                display_handle,
            );
        // #6: IME/text-input protocols. zwp_text_input_v3 lets clients
        // declare text fields; zwp_input_method_v2 lets an IME (fcitx5)
        // connect, grab the keyboard, and commit text into the focused
        // field. The text-input focus follows the keyboard focus.
        let text_input_state =
            smithay::wayland::text_input::TextInputManagerState::new::<Self>(display_handle);
        let input_method_state = smithay::wayland::input_method::InputMethodManagerState::new::<
            Self,
            _,
        >(display_handle, |_| true);
        // G-D4: wp_presentation feedback (CLOCK_MONOTONIC domain, the
        // same clock Wayland timestamps use).
        let presentation_state = smithay::wayland::presentation::PresentationState::new::<Self>(
            display_handle,
            libc::CLOCK_MONOTONIC as u32,
        );

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
        // R11: the handle is compositor state — without it the output's
        // advertised mode could never follow the actual backend size.
        // G-D1: the advertised SCALE comes from the config (wl_output
        // scale + wp_fractional_scale preferred scale for clients).
        let out_scale = config.appearance.output_scale;
        output.change_current_state(
            Some(Mode {
                size: (1280, 720).into(),
                refresh: 60000,
            }),
            None,
            Some(scale_from_f64(out_scale)),
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
            spatial_mode: true,
            spatial_cam_pose: None,
            spatial_cam_adapted: false,
            workspace_manager: WorkspaceManager::new(config.workspace.count),
            producers: Vec::new(),
            producer_error_counts: HashMap::new(),
            perf: PerfStats::new(),
            output: Some(output),
            window_size: (1280.0, 720.0),
            outputs,
            last_mouse: (0.0, 0.0),
            last_dx: 0.0,
            last_dy: 0.0,
            press_pos: (0.0, 0.0),
            nav_button: 0,
            event_serial: 0,
            last_down_vid: None,
            saved_state: None,
            render_ping: None,
            pacing_timer: None,
            pacing_active: false,
            render_caches: Default::default(),
            session_paused: false,
            backend_origin: None,
            drm_session: None,
            begin_frame_failures: 0,
            backend_lost_logged: false,
            last_backend_attempt: None,
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
            viewporter_state,
            fractional_scale_state,
            preferred_scale: out_scale,
            xwayland_shell_state,
            x11_wm: None,
            x11_windows: HashMap::new(),
            x11_display: None,
            xwayland_client: None,
            loop_handle: None,
            x11_owns_clipboard: false,
            x11_owns_primary: false,
            wlr_data_control_state,
            ext_data_control_state,
            foreign_toplevel_state,
            text_input_state,
            input_method_state,
            ime_popups: Vec::new(),
            ime_popup_visuals: HashMap::new(),
            ime_parent_vid: std::cell::Cell::new(None),
            foreign_toplevels: HashMap::new(),
            subsurface_visuals: HashMap::new(),
            subsurface_parents: HashMap::new(),
            presentation_state,
            presentation_seq: 0,
            app_switcher: ApplicationSwitcher::new(),
            focus_history: crate::focus_history::FocusHistory::new(),
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
            pointer_constraints: crate::pointer_constraints::PointerConstraints::new(
                display_handle,
            ),
            closed_windows: crate::closed::ClosedWindowHistory::new(10),
            pending_reopen: None,
            client_resizes: crate::client_resize::ClientResizeCoordinator::default(),
            maximize: crate::maximize::MaximizeCoordinator::default(),
            fullscreen: crate::fullscreen::FullscreenCoordinator::default(),
            resize_session: None,
            dnd_active: false,
            relative_pointer_state:
                smithay::wayland::relative_pointer::RelativePointerManagerState::new::<LookingGlass>(
                    display_handle,
                ),
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
                    self.camera_mut().position.x = first.camera.x;
                    self.camera_mut().position.y = first.camera.y;
                    self.camera_mut().position.z = first.camera.z;
                    self.camera_mut().yaw = first.camera.yaw;
                    self.camera_mut().pitch = first.camera.pitch;
                }

                // Store saved visual state for surface remapping
                self.saved_state = Some(state);

                // Validate version mismatch
                if self
                    .saved_state
                    .as_ref()
                    .is_some_and(|s| s.version > crate::persist::CURRENT_VERSION)
                {
                    warn!(
                        "saved state version {} > current version {}, attempt load",
                        self.saved_state.as_ref().unwrap().version,
                        crate::persist::CURRENT_VERSION
                    );
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
    pub fn save_state(&mut self) {
        // R3: the live camera belongs to the ACTIVE workspace — without
        // this sync the workspace's recorded camera is whatever was
        // last captured at workspace SWITCH, so a camera move followed
        // by shutdown restores a stale view.
        {
            let cam = self.camera().clone();
            let ws = self.workspace_manager.active_mut();
            ws.camera = cam;
        }
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
    /// #11: subsurface commit — the subsurface renders as a visual
    /// PARENTED to its parent surface's visual (J2 parent-local
    /// transforms: it follows the parent's move/rotate/scale with zero
    /// extra bookkeeping). Position comes from the double-buffered
    /// SubsurfaceCachedState.location; the top-left corner is
    /// converted to the visual's center convention. Synchronized
    /// (sync) subsurfaces apply with the parent commit per protocol —
    /// smithay's state machine handles the double-buffered merge.
    /// #6: IME popup commit — the candidate window renders as a visual
    /// PARENTED to the focused text field's visual (cached during IME
    /// activation via parent_geometry). Position comes from smithay's
    /// tracked PopupSurface::location (parent-relative, derived from
    /// the text-input cursor rectangle).
    fn handle_ime_popup_commit(&mut self, surface: &WlSurface) {
        let Some(popup) = self
            .ime_popups
            .iter()
            .find(|p| p.wl_surface() == surface)
            .cloned()
        else {
            return;
        };
        let Some(parent_vid) = self.ime_parent_vid.get() else {
            return;
        };

        let (wl_buffer, _damage): (
            Option<_>,
            Vec<smithay::utils::Rectangle<i32, smithay::utils::Buffer>>,
        ) = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let attrs = cached.current();
            let buf = match &attrs.buffer {
                Some(BufferAssignment::NewBuffer(b)) => Some(b.clone()),
                _ => None,
            };
            (buf, Vec::new())
        });
        let Some(wl_buffer) = wl_buffer else { return };

        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let renderer = backend.renderer();
        let import = with_states(surface, |states| {
            renderer.import_buffer(&wl_buffer, Some(states), &[])
        });
        if let Some(Ok(texture)) = import {
            use smithay::backend::renderer::Texture;
            let tex_size = texture.size();
            let logical_size = smithay::utils::Size::new(tex_size.w, tex_size.h);
            let location = popup.location();
            // J2: parent-local center offset (see
            // scene::surface_child_local_offset) — same top-left↔center
            // convention as subsurfaces.
            let (pw, ph, ptf, psx, psy) = self
                .scene
                .get(parent_vid)
                .map(|p| {
                    (
                        p.geometry.size.w,
                        p.geometry.size.h,
                        p.title_bar_fraction(),
                        p.transform.scale.x,
                        p.transform.scale.y,
                    )
                })
                .unwrap_or((0, 0, 0.0, 1.0, 1.0));
            let inv = |s: f32| if s.abs() > 1e-6 { 1.0 / s } else { 1.0 };
            let (dx, dy) = crate::scene::surface_child_local_offset(
                (location.x, location.y),
                (logical_size.w, logical_size.h),
                (pw, ph),
                ptf,
            );

            let existing_vid = self.ime_popup_visuals.get(surface).copied();
            if let Some(vid) = existing_vid {
                if let Some(visual) = self.scene.get_mut(vid) {
                    if let Some(dst) = visual.texture_mut() {
                        *dst = texture;
                    }
                    visual.geometry = smithay::utils::Rectangle::new(
                        smithay::utils::Point::new(0, 0),
                        logical_size,
                    );
                    visual.transform.position =
                        cgmath::Vector3::new(dx * inv(psx), dy * inv(psy), 15.0);
                }
            } else {
                let mut visual = Visual::new(
                    VisualContent::WaylandSurface(texture),
                    smithay::utils::Rectangle::new(smithay::utils::Point::new(0, 0), logical_size),
                );
                visual.decoration.title_bar_height = 0.0;
                visual.parent = Some(parent_vid);
                visual.transform.position =
                    cgmath::Vector3::new(dx * inv(psx), dy * inv(psy), 15.0);
                let vid = visual.id;
                info!(
                    ?vid,
                    ?parent_vid,
                    ?location,
                    w = logical_size.w,
                    h = logical_size.h,
                    "ime popup mapped"
                );
                self.ime_popup_visuals.insert(surface.clone(), vid);
                self.wayland_surfaces.insert(vid, surface.clone());
                self.scene.add(visual);
                self.workspace_manager.active_mut().add(vid);
            }
            self.schedule_render();
        }
    }

    /// #6: remove an IME popup visual (dismiss/destroy).
    pub fn remove_ime_popup_visual(&mut self, surface: &WlSurface) {
        if let Some(vid) = self.ime_popup_visuals.remove(surface) {
            info!(?vid, "ime popup removed");
            self.scene.remove(vid);
            self.wayland_surfaces.remove(&vid);
            for i in 0..self.workspace_manager.len() {
                if let Some(ws) = self.workspace_manager.get_mut(i) {
                    ws.remove(vid);
                }
            }
            self.schedule_render();
        }
    }

    fn handle_subsurface_commit(&mut self, surface: &WlSurface) {
        let parent_surface = smithay::wayland::compositor::get_parent(surface);
        let parent_vid = parent_surface
            .as_ref()
            .and_then(|p| self.find_vid_for_surface(p));
        let Some(parent_vid) = parent_vid else {
            // No parent visual (parent not mapped): subsurfaces wait
            // for a commit after the parent exists.
            return;
        };

        let (wl_buffer, _damage): (
            Option<_>,
            Vec<smithay::utils::Rectangle<i32, smithay::utils::Buffer>>,
        ) = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let attrs = cached.current();
            let buf = match &attrs.buffer {
                Some(BufferAssignment::NewBuffer(b)) => Some(b.clone()),
                _ => None,
            };
            (buf, Vec::new())
        });
        let Some(wl_buffer) = wl_buffer else { return };

        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let renderer = backend.renderer();
        let import = with_states(surface, |states| {
            renderer.import_buffer(&wl_buffer, Some(states), &[])
        });
        if let Some(Ok(texture)) = import {
            use smithay::backend::renderer::Texture;
            let tex_size = texture.size();
            // G-C3 logical geometry for the subsurface buffer.
            let (vp_dst, vp_src, buf_scale) = with_states(surface, |states| {
                let scale = {
                    let mut c = states.cached_state.get::<SurfaceAttributes>();
                    c.current().buffer_scale
                };
                let mut vp = states
                    .cached_state
                    .get::<smithay::wayland::viewporter::ViewportCachedState>();
                let vp = vp.current();
                (
                    vp.dst.map(|s| (s.w, s.h)),
                    vp.src.map(|r| ((r.loc.x, r.loc.y), (r.size.w, r.size.h))),
                    scale,
                )
            });
            let (logical_wh, src_uv) =
                logical_geometry_from_buffer((tex_size.w, tex_size.h), buf_scale, vp_dst, vp_src);
            let logical_size = smithay::utils::Size::new(logical_wh.0, logical_wh.1);
            let src_uv = src_uv.unwrap_or([0.0, 0.0, 1.0, 1.0]);

            let location = with_states(surface, |states| {
                let mut sub = states
                    .cached_state
                    .get::<smithay::wayland::compositor::SubsurfaceCachedState>();
                sub.current().location
            });

            let existing_vid = self.subsurface_visuals.get(surface).copied();
            // J2: parent-local center offset (see
            // scene::surface_child_local_offset). Computed from the
            // parent's CURRENT geometry so a resized parent repositions
            // its children on their next commit.
            let (pw, ph, ptf, psx, psy) = self
                .scene
                .get(parent_vid)
                .map(|p| {
                    (
                        p.geometry.size.w,
                        p.geometry.size.h,
                        p.title_bar_fraction(),
                        p.transform.scale.x,
                        p.transform.scale.y,
                    )
                })
                .unwrap_or((0, 0, 0.0, 1.0, 1.0));
            let inv = |s: f32| if s.abs() > 1e-6 { 1.0 / s } else { 1.0 };
            let (dx, dy) = crate::scene::surface_child_local_offset(
                (location.x, location.y),
                (logical_size.w, logical_size.h),
                (pw, ph),
                ptf,
            );
            if let Some(vid) = existing_vid {
                if let Some(visual) = self.scene.get_mut(vid) {
                    if let Some(dst) = visual.texture_mut() {
                        *dst = texture;
                    }
                    visual.geometry = smithay::utils::Rectangle::new(
                        smithay::utils::Point::new(0, 0),
                        logical_size,
                    );
                    visual.src_uv = src_uv;
                    visual.transform.position =
                        cgmath::Vector3::new(dx * inv(psx), dy * inv(psy), 10.0);
                }
            } else {
                let mut visual = Visual::new(
                    VisualContent::WaylandSurface(texture),
                    smithay::utils::Rectangle::new(smithay::utils::Point::new(0, 0), logical_size),
                );
                visual.src_uv = src_uv;
                // Presentation children are raw client content: no veyra
                // chrome strip, no ring (the renderer also gates chrome
                // on parented visuals).
                visual.decoration.title_bar_height = 0.0;
                visual.parent = Some(parent_vid);
                visual.transform.position =
                    cgmath::Vector3::new(dx * inv(psx), dy * inv(psy), 10.0);
                let vid = visual.id;
                info!(
                    ?vid,
                    ?parent_vid,
                    ?location,
                    w = logical_size.w,
                    h = logical_size.h,
                    "subsurface mapped"
                );
                self.subsurface_visuals.insert(surface.clone(), vid);
                if let Some(parent) = &parent_surface {
                    self.subsurface_parents
                        .insert(surface.clone(), parent.clone());
                }
                self.wayland_surfaces.insert(vid, surface.clone());
                self.scene.add(visual);
                // Subsurfaces are presentation children: same workspace
                // membership as the parent, never taskbar/focus items.
                self.workspace_manager.active_mut().add(vid);
            }
            self.schedule_render();
        }
    }

    fn sanitize_damage(
        damage: Vec<smithay::utils::Rectangle<i32, smithay::utils::Buffer>>,
        last_size: Option<(i32, i32)>,
    ) -> Vec<smithay::utils::Rectangle<i32, smithay::utils::Buffer>> {
        use smithay::utils::{Point, Rectangle, Size};
        let Some((w, h)) = last_size else {
            return Vec::new();
        };
        let buf = Rectangle::new(Point::new(0, 0), Size::new(w, h));
        damage
            .into_iter()
            .filter_map(|r| r.intersection(buf))
            .filter(|r| r.size.w > 0 && r.size.h > 0)
            .collect()
    }

    #[allow(dead_code)] // reserved API surface; not yet wired
    pub(crate) fn handle_commit(&mut self, surface: &WlSurface) {
        // #11: subsurfaces (DnD icons, client-side decorations, Qt/Chromium
        // menus) — render as visuals parented to their parent surface.
        let is_subsurface = smithay::wayland::compositor::get_role(surface)
            == Some(smithay::wayland::compositor::SUBSURFACE_ROLE);
        if is_subsurface {
            self.handle_subsurface_commit(surface);
            return;
        }

        // #6: IME candidate popups render as visuals anchored to the
        // focused text field's visual.
        if smithay::wayland::compositor::get_role(surface)
            == Some(smithay::wayland::input_method::INPUT_POPUP_SURFACE_ROLE)
        {
            self.handle_ime_popup_commit(surface);
            return;
        }

        // Determine if this is a toplevel or popup commit
        let is_popup = self.popups.iter().any(|p| p.wl_surface == *surface);

        // G-C4: X11 windows commit through the same pipeline (xwayland
        // shell association). They are not xdg toplevels; the X11
        // surface supplies chrome and the X side drives the lifecycle.
        let x11_window = self.x11_windows.get(surface).cloned();
        let x11_vid = x11_window.as_ref().and_then(|x11| {
            let wid = x11.window_id();
            self.x11_windows
                .iter()
                .find(|(_, w)| w.window_id() == wid)
                .and_then(|(s, _)| {
                    self.wayland_surfaces
                        .iter()
                        .find(|(_, vs)| *vs == s)
                        .map(|(vid, _)| *vid)
                })
        });

        let existing_vid = if x11_window.is_some() {
            x11_vid
        } else {
            self.find_surface_visual_id(surface)
        };

        // Find the lifecycle state for this surface
        let lifecycle = if x11_window.is_some() {
            // Commit-driven lifecycle for X11: mapping state is owned
            // by the X window (map_window_request/unmapped_window).
            SurfaceLifecycle::Mapped
        } else if is_popup {
            self.popups
                .iter()
                .find(|p| p.wl_surface == *surface)
                .map(|p| p.lifecycle)
                .unwrap_or(SurfaceLifecycle::Destroyed)
        } else {
            self.toplevels
                .iter()
                .find(|t| t.toplevel.wl_surface() == surface)
                .map(|t| t.lifecycle)
                .unwrap_or(SurfaceLifecycle::Destroyed)
        };

        if lifecycle == SurfaceLifecycle::Destroyed {
            return;
        }

        let is_first_map = if x11_window.is_some() {
            x11_vid.is_none()
        } else {
            lifecycle != SurfaceLifecycle::Mapped
        };
        let is_remap = is_first_map && existing_vid.is_some();

        // Extract buffer + damage (shared path for toplevels and popups)
        let (wl_buffer, damage): (Option<_>, Vec<_>) = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let attrs = cached.current();
            let buf = match &attrs.buffer {
                Some(BufferAssignment::NewBuffer(b)) => Some(b.clone()),
                _ => None,
            };
            let dmg = attrs
                .damage
                .iter()
                .map(|d| match d {
                    smithay::wayland::compositor::Damage::Buffer(r) => *r,
                    smithay::wayland::compositor::Damage::Surface(r) => {
                        let bs = attrs.buffer_scale.max(1);
                        smithay::utils::Rectangle::new(
                            smithay::utils::Point::new(r.loc.x * bs, r.loc.y * bs),
                            smithay::utils::Size::new(r.size.w * bs, r.size.h * bs),
                        )
                    }
                })
                .collect();
            (buf, dmg)
        });
        let Some(wl_buffer) = wl_buffer else { return };
        // Damage must fit the buffer being imported: race-resized clients
        // report damage in previous-buffer coordinates. Clamping against
        // the CURRENT buffer's dimensions (not the last committed size —
        // on a shrink 464→432 the stale bound let a 464-tall damage
        // upload into the 432-tall texture: GL_INVALID_VALUE spam).
        let buf_size =
            smithay::backend::renderer::buffer_dimensions(&wl_buffer).map(|s| (s.w, s.h));
        let damage = Self::sanitize_damage(damage, buf_size);

        if let Some(backend) = self.backend.as_mut() {
            let renderer = backend.renderer();
            // Use ImportAll::import_buffer to handle SHM, EGL, and DMA-BUF buffers
            let result = with_states(surface, |states| {
                renderer.import_buffer(&wl_buffer, Some(states), &damage)
            });
            match result {
                Some(Ok(texture)) => {
                    use smithay::backend::renderer::Texture;
                    // G-C3: logical geometry — wp_viewporter dst/src take
                    // precedence, else buffer dimensions divided by the
                    // buffer scale. The texture stays in buffer pixels;
                    // the renderer stretches it over the (logical) quad.
                    let buf_wh = buf_size.unwrap_or_else(|| {
                        let s = texture.size();
                        (s.w, s.h)
                    });
                    let (viewport_dst, viewport_src, buffer_scale) =
                        with_states(surface, |states| {
                            let scale = {
                                let mut c = states.cached_state.get::<SurfaceAttributes>();
                                c.current().buffer_scale
                            };
                            let buf_logical = smithay::utils::Size::new(
                                buf_wh.0 / scale.max(1),
                                buf_wh.1 / scale.max(1),
                            );
                            // Validates the viewport against the committed
                            // buffer; raises OutOfBuffer/BadSize itself.
                            let _valid = smithay::wayland::viewporter::ensure_viewport_valid(
                                states,
                                buf_logical,
                            );
                            let mut vp = states
                                .cached_state
                                .get::<smithay::wayland::viewporter::ViewportCachedState>(
                            );
                            let vp = vp.current();
                            (
                                vp.dst.map(|s| (s.w, s.h)),
                                vp.src.map(|r| ((r.loc.x, r.loc.y), (r.size.w, r.size.h))),
                                scale,
                            )
                        });
                    let (logical_wh, src_uv) = logical_geometry_from_buffer(
                        buf_wh,
                        buffer_scale,
                        viewport_dst,
                        viewport_src,
                    );
                    let logical_size = smithay::utils::Size::new(logical_wh.0, logical_wh.1);
                    let src_uv = src_uv.unwrap_or([0.0, 0.0, 1.0, 1.0]);
                    if is_first_map {
                        let tex_size = texture.size();

                        if let Some(x11) = x11_window.clone() {
                            // ── X11 window commit (G-C4) ──
                            let ws_eligible = self.workspace_manager.active().visual_ids.clone();
                            let pos = layout::place_new_visual(
                                logical_size.w as f32,
                                logical_size.h as f32,
                                &self.scene,
                                self.visible_bounds(),
                                &ws_eligible,
                            );
                            let mut visual = Visual::new(
                                VisualContent::WaylandSurface(texture),
                                smithay::utils::Rectangle::new(
                                    smithay::utils::Point::new(0, 0),
                                    logical_size,
                                ),
                            );
                            visual.src_uv = src_uv;
                            visual.chrome.title = x11.title();
                            visual.chrome.app_id = x11.class();
                            let x11_title = visual.chrome.title.clone();
                            let x11_app_id = visual.chrome.app_id.clone();
                            let x11_vid = visual.id;
                            self.register_foreign_toplevel(x11_vid, &x11_title, &x11_app_id);
                            visual.transform.position = pos;
                            let visual_id = visual.id;
                            let x11_map_pos = visual.transform.position;
                            info!(
                                ?visual_id,
                                app_id = %visual.chrome.app_id,
                                title = %visual.chrome.title,
                                pos = ?x11_map_pos,
                                total_w = visual.total_width(),
                                total_h = visual.total_height(),
                                "x11 surface mapped"
                            );
                            self.wayland_surfaces.insert(visual_id, surface.clone());
                            self.scene.add(visual);
                            self.workspace_manager.active_mut().add(visual_id);
                            // focus-on-map policy (same as native toplevels)
                            // — EXCEPT X11 surfaces that per EWMH never take
                            // input focus: override-redirect windows
                            // (dropdowns, menus, tooltips, helper windows)
                            // and the corresponding _NET_WM_WINDOW_TYPEs.
                            // Granting them focus steals the keyboard from
                            // the parent app mid-typing (BUG_LIST #19).
                            use smithay::xwayland::xwm::WmWindowType;
                            let x11_focusable = !x11.is_override_redirect()
                                && !matches!(
                                    x11.window_type(),
                                    Some(
                                        WmWindowType::DropdownMenu
                                            | WmWindowType::Menu
                                            | WmWindowType::PopupMenu
                                            | WmWindowType::Tooltip
                                            | WmWindowType::Notification
                                            | WmWindowType::Utility
                                            | WmWindowType::Toolbar
                                            | WmWindowType::Splash
                                    )
                                );
                            if x11_focusable {
                                self.set_keyboard_focus(Some(visual_id));
                            }
                            if !self.spatial_mode {
                                self.auto_fit_camera();
                            }
                        } else if is_popup {
                            // ── Popup commit ──
                            let popup_idx = self
                                .popups
                                .iter()
                                .position(|p| p.wl_surface == *surface)
                                .unwrap();
                            self.popups[popup_idx].lifecycle = SurfaceLifecycle::Mapped;
                            self.popups[popup_idx].size = Some((tex_size.w, tex_size.h));

                            if is_remap {
                                if let Some(vid) = existing_vid {
                                    // Compute position before mutable borrow of scene
                                    let new_pos = self.popups[popup_idx].positioner.get_geometry();
                                    let parent_pos = self.popups[popup_idx]
                                        .parent_toplevel_vid
                                        .and_then(|pvid| {
                                            self.scene.visuals.iter().find(|v| v.id == pvid).map(
                                                |parent| {
                                                    (
                                                        parent.transform.position,
                                                        parent.total_width(),
                                                        parent.total_height(),
                                                    )
                                                },
                                            )
                                        });
                                    if let Some(visual) = self.scene.get_mut(vid) {
                                        if let Some(dst) = visual.texture_mut() {
                                            *dst = texture;
                                        }
                                        visual.geometry = smithay::utils::Rectangle::new(
                                            smithay::utils::Point::new(0, 0),
                                            logical_size,
                                        );
                                        visual.src_uv = src_uv;
                                        // Recompute position from updated positioner.
                                        // J2: parent-local, same as the map path
                                        // (parent linkage was set at first map).
                                        if let Some((_p_pos, p_total_w, p_total_h)) = parent_pos {
                                            let popup_w = new_pos.size.w as f32;
                                            let popup_h = new_pos.size.h as f32;
                                            let local_x = new_pos.loc.x as f32 + popup_w * 0.5
                                                - p_total_w * 0.5;
                                            let local_y = -(new_pos.loc.y as f32 + popup_h * 0.5
                                                - p_total_h * 0.5);
                                            visual.transform.position =
                                                cgmath::Vector3::new(local_x, local_y, 10.0);
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
                                let parent_info =
                                    parent_vid.and_then(|pvid| {
                                        self.scene.visuals.iter().find(|v| v.id == pvid).map(
                                            |parent| {
                                                (
                                                    parent.transform.position,
                                                    parent.total_width(),
                                                    parent.total_height(),
                                                    pvid,
                                                )
                                            },
                                        )
                                    });
                                let mut visual = Visual::new(
                                    VisualContent::WaylandSurface(texture),
                                    smithay::utils::Rectangle::new(
                                        smithay::utils::Point::new(0, 0),
                                        logical_size,
                                    ),
                                );
                                visual.src_uv = src_uv;
                                if let Some((_p_pos, p_total_w, p_total_h, pvid)) = parent_info {
                                    let popup_w = popup_geometry.size.w as f32;
                                    let popup_h = popup_geometry.size.h as f32;
                                    // J2: the popup transform is PARENT-LOCAL —
                                    // an offset from the parent's center in the
                                    // parent's own coordinate frame. The renderer
                                    // composes world = parent_world * local, so
                                    // the popup follows parent move/rotate/scale
                                    // with zero extra bookkeeping. (The old code
                                    // baked the parent's world position into the
                                    // local transform, double-applying it for any
                                    // parent not at the origin.)
                                    let local_x = popup_geometry.loc.x as f32 + popup_w * 0.5
                                        - p_total_w * 0.5;
                                    let local_y = -(popup_geometry.loc.y as f32 + popup_h * 0.5
                                        - p_total_h * 0.5);
                                    visual.transform.position =
                                        cgmath::Vector3::new(local_x, local_y, 10.0);
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
                                // J2 observability: log the parent-local offset
                                // and the derived world position (parent
                                // transform applied) for harness assertions.
                                let (local_dbg, world_dbg) = self
                                    .scene
                                    .visuals
                                    .iter()
                                    .find(|v| v.id == visual_id)
                                    .map(|v| {
                                        let w = self.scene.world_matrix(visual_id);
                                        (
                                            (v.transform.position.x, v.transform.position.y),
                                            (w[3][0], w[3][1], w[3][2]),
                                        )
                                    })
                                    .unwrap_or(((0.0, 0.0), (0.0, 0.0, 0.0)));
                                info!(?visual_id, ?parent_vid, ?popup_geometry,
                                      local = ?local_dbg, world = ?world_dbg, "popup mapped");
                            }
                        } else {
                            // ── Toplevel commit ──
                            let idx = self
                                .toplevels
                                .iter()
                                .position(|t| t.toplevel.wl_surface() == surface)
                                .unwrap();
                            self.toplevels[idx].lifecycle = SurfaceLifecycle::Mapped;
                            self.toplevels[idx].size = Some((logical_size.w, logical_size.h));

                            if is_remap {
                                if let Some(vid) = existing_vid {
                                    if let Some(visual) = self.scene.get_mut(vid) {
                                        if let Some(dst) = visual.texture_mut() {
                                            *dst = texture;
                                        }
                                        visual.geometry = smithay::utils::Rectangle::new(
                                            smithay::utils::Point::new(0, 0),
                                            logical_size,
                                        );
                                        visual.src_uv = src_uv;
                                        self.workspace_manager.active_mut().add(vid);
                                        info!(?vid, app_id = %self.toplevels[idx].app_id, "surface remapped");
                                    }
                                }
                            } else {
                                let _z_off = [-200.0, 0.0, 200.0];
                                let y_ang = [5.0, 0.0, -5.0];
                                let _n = self.toplevels.len();
                                let angle_y = if idx < y_ang.len() { y_ang[idx] } else { 0.0 };
                                let mut visual = Visual::new(
                                    VisualContent::WaylandSurface(texture),
                                    smithay::utils::Rectangle::new(
                                        smithay::utils::Point::new(0, 0),
                                        logical_size,
                                    ),
                                );
                                visual.src_uv = src_uv;
                                use cgmath::Deg;
                                use cgmath::Rotation3;
                                visual.chrome.title = self.toplevels[idx].title.clone();
                                visual.chrome.app_id = self.toplevels[idx].app_id.clone();
                                let app_id = self.toplevels[idx].app_id.clone();
                                // G-D3: publish to foreign-toplevel clients at map.
                                let ftl_title = visual.chrome.title.clone();
                                let ftl_app_id = visual.chrome.app_id.clone();
                                let ftl_vid = visual.id;
                                self.register_foreign_toplevel(ftl_vid, &ftl_title, &ftl_app_id);
                                // Pending reopen (I1): reattach saved transform
                                // when the relaunched app's toplevel maps.
                                let mut reopened: Option<crate::closed::PendingReopen> = None;
                                if self
                                    .pending_reopen
                                    .as_ref()
                                    .is_some_and(|pr| pr.app_id == app_id)
                                {
                                    reopened = self.pending_reopen.take();
                                    if let Some(pr) = &reopened {
                                        visual.transform = pr.transform.clone();
                                        info!(app_id = %app_id, workspace = pr.workspace, "pending reopen applied");
                                    }
                                }
                                // R3: consuming match (duplicate app_ids
                                // restore in capture order); returns the
                                // saved workspace index for membership.
                                let restored = self.saved_state.as_mut().and_then(|s| {
                                    s.take_visual(&app_id).map(|(ws_idx, vs)| {
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
                                        ws_idx
                                    })
                                });
                                if restored.is_none() && reopened.is_none() {
                                    let ws_eligible =
                                        self.workspace_manager.active().visual_ids.clone();
                                    let pos = layout::place_new_visual(
                                        tex_size.w as f32 * visual.transform.scale.x,
                                        tex_size.h as f32 * visual.transform.scale.y,
                                        &self.scene,
                                        self.visible_bounds(),
                                        &ws_eligible,
                                    );
                                    visual.transform.position = pos;
                                    visual.transform.rotation =
                                        cgmath::Quaternion::from_angle_y(Deg(angle_y));
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
                                // J1-fix: frame whatever is now on the
                                // active workspace — including restored
                                // windows, which keep their persisted
                                // transforms and may sit outside the
                                // current view (the "sometimes cut"
                                // report). Camera-only, zoom-out-only.
                                if !self.spatial_mode {
                                    self.auto_fit_camera();
                                }
                                // R3: a restored window returns to its SAVED
                                // workspace, not the active one.
                                if let Some(ws_idx) = restored {
                                    if ws_idx < self.workspace_manager.len()
                                        && ws_idx != self.workspace_manager.active_id()
                                    {
                                        if let Some(ws) = self.workspace_manager.get_mut(ws_idx) {
                                            ws.add(visual_id);
                                        }
                                        self.workspace_manager.active_mut().remove(visual_id);
                                        info!(
                                            ?visual_id,
                                            workspace = ws_idx,
                                            "restored window placed in saved workspace"
                                        );
                                    }
                                }
                                // Reopen targets a specific workspace: move the
                                // visual there if it differs from the active one.
                                if let Some(ws_idx) = reopen_workspace {
                                    if ws_idx < self.workspace_manager.len()
                                        && ws_idx != self.workspace_manager.active_id()
                                    {
                                        if let Some(ws) = self.workspace_manager.get_mut(ws_idx) {
                                            ws.add(visual_id);
                                        }
                                        self.workspace_manager.active_mut().remove(visual_id);
                                        info!(
                                            ?visual_id,
                                            workspace = ws_idx,
                                            "reopened window restored to workspace"
                                        );
                                    }
                                }
                                self.scene.focus(Some(visual_id));
                                self.app_switcher
                                    .register_visual(&self.toplevels[idx].app_id, visual_id);
                                // J1: a new window takes focus (GNOME
                                // policy) — this also seeds the MRU.
                                self.set_keyboard_focus(Some(visual_id));
                                info!(?visual_id, app_id = %self.toplevels[idx].app_id,
                                       pos = ?map_pos,
                                       rot = ?map_rot,
                                       total_w = map_total_w,
                                       total_h = map_total_h,
                                       scale = ?map_scale,
                                       "surface mapped");
                                crate::debug_journal::event(
                                    "map",
                                    &[
                                        ("vid", crate::debug_journal::d(visual_id)),
                                        (
                                            "app",
                                            crate::debug_journal::s(&self.toplevels[idx].app_id),
                                        ),
                                        ("pos", format!("{:?}", map_pos)),
                                        ("size", format!("[{},{}]", map_total_w, map_total_h)),
                                    ],
                                );
                                self.debug_snapshot();
                                // G-B1: a freshly mapped client may have
                                // bound its data device just now — smithay
                                // never sends the CURRENT selection to
                                // newly registered devices. Schedule the
                                // selection refresh so the new client
                                // learns the active clipboard (the
                                // broadcast must not run inside this
                                // commit dispatch; see
                                // ClientData::disconnected).
                                SELECTION_REFRESH_PENDING
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                self.schedule_render();
                            }
                        }
                    } else if let Some(vid) = existing_vid {
                        // G-C4 diagnostics: X11 repaint damage must
                        // produce commits — silent blank windows are
                        // traceable from the harness.
                        if x11_window.is_some() {
                            tracing::debug!(?vid, "x11 surface repaint commit");
                        }
                        // Resolve any outstanding geometry request (I3a).
                        // A mismatched buffer means the client overrode us —
                        // committed geometry always wins.
                        let outcome = self
                            .client_resizes
                            .note_commit(vid, (logical_size.w, logical_size.h));
                        match outcome {
                            crate::client_resize::CommitOutcome::Fulfilled => {
                                info!(
                                    ?vid,
                                    w = logical_size.w,
                                    h = logical_size.h,
                                    "client resize fulfilled"
                                );
                                // Client pacing (I3b): continue with the
                                // latest desired size, if the session moved on.
                                self.flush_resize_desired(vid);
                            }
                            crate::client_resize::CommitOutcome::ClientOverride => {
                                info!(
                                    ?vid,
                                    w = logical_size.w,
                                    h = logical_size.h,
                                    "client overrode requested size; adopting committed geometry"
                                );
                            }
                            crate::client_resize::CommitOutcome::NotResizing => {}
                        }
                        let committed = (logical_size.w, logical_size.h);
                        // I4: a committed buffer completes any outstanding
                        // maximize/unmaximize transition for this surface.
                        self.complete_maximize_intent(vid, committed);
                        self.flush_deferred_maximize();
                        // I7: same completion discipline for fullscreen.
                        self.complete_fullscreen_intent(vid, committed);
                        self.flush_fullscreen_deferred();
                        if let Some(visual) = self.scene.get_mut(vid) {
                            if let Some(dst) = visual.texture_mut() {
                                *dst = texture;
                            }
                            // Adopt committed LOGICAL dimensions: the client
                            // decides geometry. Transform (position/rotation/
                            // scale) is spatial state and is never touched.
                            if visual.geometry.size != logical_size {
                                visual.geometry = smithay::utils::Rectangle::new(
                                    smithay::utils::Point::new(0, 0),
                                    logical_size,
                                );
                                info!(
                                    ?vid,
                                    w = logical_size.w,
                                    h = logical_size.h,
                                    "visual geometry adopted from client buffer"
                                );
                            }
                            visual.src_uv = src_uv;
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
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn add_external_visual(&mut self, pixels: Vec<u8>, width: u32, height: u32) {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::renderer::ImportMem;

        let Some(backend) = self.backend.as_mut() else {
            return;
        };
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
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn register_input_sink(&mut self, vid: VisualId, sink: Box<dyn InputSink>) {
        self.input_sinks.insert(vid, sink);
        info!(?vid, "input sink registered");
    }

    pub fn add_benchmark_visual(
        &mut self,
        mut producer: Box<dyn FrameProducer>,
        index: usize,
        total: usize,
    ) {
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let renderer = backend.renderer();
        if !matches!(producer.update(renderer), FrameResult::Unchanged) {
            return;
        }
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
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn add_producer(&mut self, mut producer: Box<dyn FrameProducer>) -> Option<VisualId> {
        let renderer = self.backend.as_mut()?.renderer();
        let result = producer.update(renderer);
        let is_ok = matches!(
            result,
            FrameResult::Updated | FrameResult::Unchanged | FrameResult::Resized(_, _)
        );
        if !is_ok {
            match result {
                FrameResult::Error(msg) => {
                    warn!(?msg, "frame producer not added: initial update failed")
                }
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
        let ws_eligible = self.workspace_manager.active().visual_ids.clone();
        visual.transform.position = layout::place_new_visual(
            w as f32,
            h as f32,
            &self.scene,
            self.visible_bounds(),
            &ws_eligible,
        );
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

    /// R6: whether a frame must be produced now — dirty state, an
    /// active animation, or a mapped client still waiting for a frame
    /// callback (a client that requested wl_surface.frame() stalls
    /// forever unless the compositor presents).
    pub fn should_render(&self) -> bool {
        self.scheduler.needs_render() || self.has_pending_frame_callbacks()
    }

    /// R6: any mapped toplevel surface with an unanswered frame
    /// callback request. The scan mirrors the completion loop in
    /// render() (toplevels only).
    pub fn has_pending_frame_callbacks(&self) -> bool {
        self.toplevels.iter().any(|t| {
            if t.lifecycle != SurfaceLifecycle::Mapped
                && t.lifecycle != SurfaceLifecycle::Configured
            {
                return false;
            }
            let surface = t.toplevel.wl_surface();
            with_states(surface, |states| {
                !states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .frame_callbacks
                    .is_empty()
            })
        })
    }

    /// R6: the render-loop pump, driven by the ping source. Renders
    /// dirty state immediately and keeps the pacing timer armed only
    /// while continuous work exists (animations, pending client frame
    /// callbacks). When idle, the timer source is dropped — the
    /// compositor neither renders nor wakes.
    pub fn pump_render_loop(&mut self, handle: &smithay::reexports::calloop::LoopHandle<'_, Self>) {
        if self.scheduler.is_dirty() {
            self.render();
        }
        if self.should_render() && !self.pacing_active {
            // Drop the inert source from the previous idle period (its
            // timer fired and returned TimeoutAction::Drop).
            if let Some(tok) = self.pacing_timer.take() {
                handle.remove(tok);
            }
            if let Ok(tok) = handle.insert_source(
                smithay::reexports::calloop::timer::Timer::from_duration(RENDER_PACING_INTERVAL),
                pacing_timer_callback,
            ) {
                self.pacing_timer = Some(tok);
                self.pacing_active = true;
            }
        }
    }

    /// R11: keep the advertised wl_output mode in step with the actual
    /// backend framebuffer size. Called at startup (from the selected
    /// backend) and on every resize; smithay propagates mode events to
    /// connected clients automatically.
    pub fn sync_output_mode(&mut self, w: i32, h: i32, refresh: i32) {
        let Some(output) = self.output.clone() else {
            return;
        };
        let mode = Mode {
            size: (w, h).into(),
            refresh,
        };
        let current = output.current_mode();
        // P3 (audit): a refresh-only change must still apply — the old
        // early-return on size match silently ignored it (masked so far
        // because every caller passed a hardcoded 60000).
        if current.as_ref().map(|m| (m.size, m.refresh)) == Some(((w, h).into(), refresh)) {
            return;
        }
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);
        // #14/G-E5.3: the registry is pre-seeded with the default
        // primary output; the real sync updates mode and name. The
        // camera is per-output state and is NOT touched here.
        self.outputs
            .update_primary_mode(w as u32, h as u32, refresh);
        self.outputs.rename_primary(output.name());
        info!(w, h, refresh, "output mode synced with backend size");
    }

    /// G-D1/#9: update the advertised output scale at runtime — changes
    /// the wl_output scale event for bound clients and re-broadcasts
    /// the preferred fractional scale to every mapped surface. Called
    /// from the config-reload path (inotify watch in main.rs).
    pub fn sync_output_scale(&mut self, scale: f64) {
        let Some(output) = self.output.clone() else {
            return;
        };
        if self.preferred_scale == scale {
            return;
        }
        output.change_current_state(None, None, Some(scale_from_f64(scale)), None);
        self.preferred_scale = scale;
        // #14 phase 1: keep the registry's scale in step.
        self.outputs.update_primary_scale(scale);
        // wp_fractional_scale clients learn the new preferred scale on
        // their next bind; already-bound surfaces get the update here.
        let surfaces: Vec<WlSurface> = self.wayland_surfaces.values().cloned().collect();
        for surface in surfaces {
            with_states(&surface, |states| {
                smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                    fs.set_preferred_scale(scale);
                });
            });
        }
        info!(scale, "output scale updated");
    }

    /// #9: apply runtime config changes (SIGHUP-equivalent: the config
    /// file is watched with inotify in main.rs). Scope: the output
    /// scale — workspace/layout/input changes need dedicated migration
    /// logic per field and are intentionally not picked up live.
    pub fn apply_config_changes(&mut self, config: Config) {
        let scale = config.appearance.output_scale;
        if (scale - self.preferred_scale).abs() > f64::EPSILON {
            info!(scale, "config reload: output scale changed");
            self.sync_output_scale(scale);
        }
    }

    /// Schedule a render and record the request in perf stats.
    pub fn schedule_render(&mut self) {
        self.perf.record_requested();
        self.scheduler.schedule_render();
        // R6: ping the event loop so dirty frames render immediately
        // rather than waiting for the next pacing tick.
        if let Some(ping) = &self.render_ping {
            ping.ping();
        }
    }

    /// P1 (audit): bring the presentation backend back after a lost GL
    /// context. DRM retries are throttled (the device may be revoked
    /// while the VT is paused); winit cannot be recreated mid-session,
    /// so it fails LOUDLY exactly once instead of silently no-oping
    /// forever. Wayland client state is untouched either way — clients
    /// stay connected and their surface state survives the outage.
    fn try_recreate_backend(&mut self) {
        if self.backend.is_some() {
            return;
        }
        let Some(origin) = self.backend_origin else {
            return;
        };
        match origin {
            BackendOrigin::Winit => {
                if !self.backend_lost_logged {
                    self.backend_lost_logged = true;
                    error!(
                        "GL context lost on the winit (nested) backend; the winit \
                         window/event-loop cannot be rebuilt mid-session. The \
                         compositor is now idle — Wayland clients remain \
                         connected but nothing renders. Restart veyra to recover."
                    );
                }
            }
            BackendOrigin::Drm => {
                let now = std::time::Instant::now();
                if let Some(last) = self.last_backend_attempt {
                    if now.duration_since(last) < BACKEND_RETRY_INTERVAL {
                        return;
                    }
                }
                self.last_backend_attempt = Some(now);
                let Some(session) = self.drm_session.as_ref() else {
                    if !self.backend_lost_logged {
                        self.backend_lost_logged = true;
                        error!(
                            "DRM context lost and no libseat session held; cannot recreate backend"
                        );
                    }
                    return;
                };
                if !session.is_active() {
                    // VT backgrounded: device access is revoked. Retry
                    // when the session notifier flips back to active
                    // (that path calls schedule_render()).
                    debug!("backend recreate deferred: seat session inactive");
                    return;
                }
                match crate::drm_backend::DrmGraphicsBackend::try_new_with_session(session) {
                    Ok(drm) => {
                        let (w, h) = drm.size();
                        self.backend = Some(Box::new(drm));
                        // The GPU caches died with the old context.
                        self.render_caches = Default::default();
                        self.window_size = (w, h);
                        self.sync_output_mode(w as i32, h as i32, 60000);
                        self.backend_lost_logged = false;
                        self.last_backend_attempt = None;
                        info!(w, h, "presentation backend recreated after context loss");
                    }
                    Err(e) => {
                        warn!(?e, "backend recreation attempt failed (will retry)");
                    }
                }
            }
        }
    }

    pub fn render(&mut self) {
        use crate::perf::PipelineStage;

        // P1 #2: the session owns the display — no presentation while
        // the VT is backgrounded.
        if self.session_paused {
            self.scheduler.clear();
            return;
        }

        // G-B1: flush a pending selection refresh (owner disconnect)
        // before drawing — the toggle must NOT run inside the client
        // destruction dispatch (see ClientData::disconnected).
        if SELECTION_REFRESH_PENDING.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.refresh_selection_state();
        }

        // Always render to complete pending wl_surface.frame callbacks.
        // If nothing changed, begin_frame/render_scene/finish_frame are
        // still required to send callback.done() to waiting clients.
        // Without this, foot and other clients stall waiting for callbacks.

        // Clear stale focus: if the focused visual has been destroyed, clean up
        self.clear_stale_focus();
        self.perf.record_rendered();
        self.scheduler.clear();

        // P1 (audit): a lost context must not turn the compositor into a
        // silent no-op loop — attempt recovery before giving up on the frame.
        self.try_recreate_backend();
        if self.backend.is_none() {
            return;
        }

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
                let vid = *vid;
                let t0 = std::time::Instant::now();
                let result = producer.update(renderer);
                let dt = t0.elapsed().as_nanos() as u64;
                match result {
                    FrameResult::Updated => {
                        self.producer_error_counts.remove(&vid);
                        self.perf.record_stage(PipelineStage::ProducerUpdate, dt);
                        updates.push((vid, producer.texture().clone()));
                        i += 1;
                    }
                    FrameResult::Unchanged => {
                        self.producer_error_counts.remove(&vid);
                        self.perf.record_stage(PipelineStage::ProducerUpdate, dt);
                        self.perf.record_dropped();
                        i += 1;
                    }
                    FrameResult::Resized(w, h) => {
                        // Update visual geometry to match new framebuffer size.
                        // The transform.scale is NOT modified — it's the user's spatial scale.
                        if let Some(visual) = self.scene.get_mut(vid) {
                            visual.geometry = smithay::utils::Rectangle::new(
                                smithay::utils::Point::new(0, 0),
                                smithay::utils::Size::new(w as i32, h as i32),
                            );
                            info!(?vid, new_w = w, new_h = h, "visual resized");
                        }
                        self.perf.record_stage(PipelineStage::ProducerUpdate, dt);
                        updates.push((vid, producer.texture().clone()));
                        i += 1;
                    }
                    FrameResult::Error(msg) => {
                        let count = self
                            .producer_error_counts
                            .entry(vid)
                            .and_modify(|c| *c = c.saturating_add(1))
                            .or_insert(1);
                        if *count >= PRODUCER_ERROR_LIMIT {
                            error!(
                                ?vid,
                                failures = *count,
                                last_error = ?msg,
                                "producer failing persistently — disconnecting"
                            );
                            self.scene.disconnect(vid);
                            self.producers.swap_remove(i);
                            self.producer_error_counts.remove(&vid);
                            continue; // swap_remove moved a new element into i
                        }
                        if *count == 1 || *count % 20 == 0 {
                            warn!(?vid, failures = *count, ?msg, "producer error");
                        }
                        i += 1;
                    }
                    FrameResult::Finished => {
                        info!(?vid, "producer finished, disconnecting visual");
                        self.producer_error_counts.remove(&vid);
                        self.scene.disconnect(vid);
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
        self.perf.record_stage(
            PipelineStage::TexCopy,
            t_tex_start.elapsed().as_nanos() as u64,
        );

        // Step 3: Apply layout
        let (world_w, world_h) = self.fb_size();
        let detached = self.layout_detached();
        // Layout only speaks for the active workspace (audit: foreign
        // workspace transforms must not be rearranged every frame).
        // Disjoint field borrows — no per-frame Vec clone.
        layout::apply_layout(
            &mut self.scene,
            self.workspace_manager.active().layout_mode,
            &layout::LayoutConfig::default(),
            &detached,
            world_w,
            world_h,
            &self.workspace_manager.active().visual_ids,
        );

        // Apply shelf transforms to shelved visuals (overrides layout)
        if self.shelf.visible {
            self.shelf.apply_shelf_transforms(&mut self.scene);
        }

        // J4 shell plane: rebuild the taskbar model from live state
        // each frame (before the backend mutable borrow).
        let taskbar = self.build_taskbar();
        // G-E5.2: framebuffer size read before the backend borrow.
        let (w, h) = self.fb_size();
        let fb_h = h;
        if !self.spatial_mode {
            self.camera_mut().position = cgmath::Point3::new(0.0, 0.0, 500.0);
            self.camera_mut().yaw = 0.0;
            self.camera_mut().pitch = 0.0;
        } else if self.workspace_manager.active().auto_orbit {
            let t = (self.perf.frame_count as f32) * 0.003;
            self.camera_mut().yaw = t.cos() * 0.8;
            self.camera_mut().pitch = (t * 0.5).sin() * 0.3 + 0.2;
        } else if !self.spatial_cam_adapted
            && self.spatial_cam_pose.is_none()
            && self.focus_manager.transition.is_none()
        {
            // One-shot frustum fit: with fov_y=45° and aspect w/h, a
            // camera at distance 1.2071*h sees exactly the ortho view
            // rectangle (±w/2, ±h/2) on the z=0 plane. Camera::new's
            // fixed z=800 leaves windows along the placement spiral
            // outside the frustum (invisible in spatial mode).
            self.spatial_cam_adapted = true;
            let d = (fb_h * 1.2071f32).max(600.0);
            self.camera_mut().position = cgmath::Point3::new(0.0, 0.0, d);
            self.camera_mut().yaw = 0.0;
            self.camera_mut().pitch = 0.0;
            info!(distance = d, "spatial camera fitted to view");
        }
        // Focus/overview mode interpolates the camera toward the target
        let render_camera = self
            .focus_manager
            .interpolated_camera(self.camera(), &self.scene);
        let view = render_camera.view_matrix();
        let proj = Self::projection_for(self.spatial_mode, w, h);
        // Step 5: present
        // Step 4: Camera + render
        let back: &mut dyn PresentationBackend = match self.backend.as_mut() {
            Some(b) => b.as_mut(),
            None => return,
        };
        // In workspace overview mode, show all workspaces' visuals
        let ws_visible = match self.focus_manager.camera_mode {
            CameraMode::WorkspaceOverview => None, // show all
            _ => Some(self.workspace_manager.active().visual_ids.as_slice()),
        };
        // Keep animating if focus/overview transition is active
        if self.focus_manager.transition.is_some() || self.workspace_manager.active().auto_orbit {
            self.scheduler.set_animating(true);
        } else {
            self.scheduler.set_animating(false);
        }
        let context_menu = if self.context_menu.visible {
            Some(&self.context_menu)
        } else {
            None
        };
        // Bind the EGL surface before rendering (makes rendering context current)
        if let Err(e) = back.begin_frame() {
            // P2 #9: the GPU caches die with the context — but a begin
            // failure may also be transient (surface-less window during a
            // resize). Only a PERSISTENT failure is a state transition:
            // after BEGIN_FRAME_FAILURE_LIMIT consecutive failures the
            // backend is dropped (recreated via try_recreate_backend)
            // instead of warn-per-dirty-frame forever.
            self.begin_frame_failures = self.begin_frame_failures.saturating_add(1);
            if self.begin_frame_failures >= BEGIN_FRAME_FAILURE_LIMIT {
                error!(
                    ?e,
                    failures = self.begin_frame_failures,
                    "begin_frame persistently failing — dropping backend for recreation"
                );
                self.backend = None;
                self.render_caches = Default::default();
                self.last_backend_attempt = None;
            } else {
                warn!(
                    ?e,
                    failures = self.begin_frame_failures,
                    "begin_frame failed"
                );
            }
            self.scheduler.clear();
            self.perf
                .record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
            self.perf.record_frame();
            return;
        }
        self.begin_frame_failures = 0;
        let overlays = renderer::Overlays {
            context_menu,
            taskbar: Some(&taskbar),
        };
        let context_lost = match renderer::render_scene(
            back,
            &self.scene,
            &view,
            &proj,
            &mut self.perf,
            ws_visible,
            &overlays,
            &mut self.render_caches,
        ) {
            Err(SwapBuffersError::ContextLost(e)) => {
                error!(?e, "Context lost");
                true
            }
            _ => false,
        };
        if context_lost {
            self.backend = None;
            // P2 #9: the GPU caches die with the context.
            self.render_caches = Default::default();
            self.last_backend_attempt = None;
            self.scheduler.clear();
            self.perf
                .record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
            self.perf.record_frame();
            return;
        }
        // R1: presentation errors must reach the frame owner. A lost
        // context drops the backend; render() re-enters through
        // try_recreate_backend (DRM recreates for real, winit fails
        // loudly) exactly like the begin path above; temporary failures
        // are logged and the frame is not counted as presented.
        match back.finish_frame() {
            Ok(()) => {
                self.perf.record_presented();
                if !updates.is_empty() {
                    self.perf.record_damage();
                }
            }
            Err(SwapBuffersError::ContextLost(e)) => {
                error!(?e, "Context lost on finish_frame");
                self.backend = None;
                // P2 #9: the GPU caches die with the context.
                self.render_caches = Default::default();
                self.last_backend_attempt = None;
                self.scheduler.clear();
                self.perf
                    .record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
                self.perf.record_frame();
                return;
            }
            Err(e) => {
                error!(?e, "finish_frame failed");
            }
        }

        // Complete pending frame callbacks for ALL mapped Wayland surfaces
        // (toplevels, popups, AND X11 windows — Xwayland paces its window
        // repaints on frame callbacks, so an X11 window whose callbacks
        // were never answered renders exactly one frame and freezes).
        let time = now_ms();
        for surface in self.wayland_surfaces.values() {
            with_states(surface, |states| {
                let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                let current = attrs.current();
                for cb in &current.frame_callbacks {
                    cb.done(time);
                }
                current.frame_callbacks.clear();
            });
        }

        // G-D4: wp_presentation feedback — the full-frame pipeline just
        // presented every mapped surface's committed content. Feedback
        // callbacks are taken from the surface's presentation state and
        // answered with CLOCK_MONOTONIC presentation time.
        self.presentation_seq = self.presentation_seq.wrapping_add(1);
        let seq = self.presentation_seq;
        let refresh_mhz = self
            .output
            .as_ref()
            .and_then(|o| o.current_mode())
            .map(|m| m.refresh)
            .unwrap_or(60000);
        let refresh = if refresh_mhz > 0 {
            smithay::wayland::presentation::Refresh::fixed(Duration::from_nanos(
                1_000_000_000u64 / refresh_mhz as u64,
            ))
        } else {
            smithay::wayland::presentation::Refresh::fixed(Duration::from_millis(16))
        };
        let (psec, pnsec) = monotonic_since_boot();
        let ptime = Duration::new(psec as u64, pnsec);
        let output = self.output.clone();
        for surface in self.wayland_surfaces.values() {
            let feedbacks = with_states(surface, |states| {
                std::mem::take(
                    &mut states
                        .cached_state
                        .get::<smithay::wayland::presentation::PresentationFeedbackCachedState>()
                        .current()
                        .callbacks,
                )
            });
            for feedback in feedbacks {
                if let Some(output) = &output {
                    feedback.presented(
                        output,
                        ptime,
                        refresh,
                        seq,
                        smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync,
                    );
                } else {
                    feedback.discarded();
                }
            }
        }

        // Flush protocol events generated during this frame (frame callbacks,
        // input forwarding, configure events). libwayland buffers them; without
        // an explicit flush they are only delivered when the client itself
        // sends traffic, so clients pacing rendering with wl_surface.frame()
        // (e.g. foot) stall until the next input event.
        let _ = self.display_handle.flush_clients();

        self.scene.clear_damage();

        self.perf
            .record_stage(PipelineStage::Total, t_frame.elapsed().as_nanos() as u64);
        self.perf.record_frame();
    }

    /// P1 (audit): the single projection constructor for rendering AND
    /// picking. winit reports 0×N sizes on some minimize/resize transitions;
    /// an unclamped `w / h` yields ∞/NaN aspect ratios that poison matrices,
    /// GL uniforms, and every ray cast derived from them. Sizes are clamped
    /// to ≥1 so the degenerate case degrades to a 1px view instead of NaN.
    pub fn projection_for(spatial_mode: bool, w: f32, h: f32) -> Matrix4<f32> {
        let w = w.max(1.0);
        let h = h.max(1.0);
        if spatial_mode {
            cgmath::perspective(cgmath::Deg(45.0), w / h, 1.0, 10000.0)
        } else {
            cgmath::ortho(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, -1000.0, 1000.0)
        }
    }

    /// G-E5.3: the live presentation camera of the PRIMARY output.
    /// Workspace/world state is shared; the view is output-local.
    /// With N outputs each OutputState carries its own Camera —
    /// camera()/camera_mut() resolve the primary; per-query resolution
    /// (the output under the pointer) arrives with G-E5.5/5.6.
    pub fn camera(&self) -> &Camera {
        &self
            .outputs
            .primary()
            .expect("primary output registered at construction")
            .camera
    }

    pub fn camera_mut(&mut self) -> &mut Camera {
        &mut self
            .outputs
            .primary_mut()
            .expect("primary output registered at construction")
            .camera
    }

    /// Split borrow: focus manager + primary camera (the focus
    /// transitions mutate the camera through the manager API).
    fn focus_parts(&mut self) -> (&mut FocusManager, &mut Camera) {
        let Self {
            focus_manager,
            outputs,
            ..
        } = self;
        (
            focus_manager,
            &mut outputs
                .primary_mut()
                .expect("primary output registered at construction")
                .camera,
        )
    }

    /// Split borrow: primary camera + shared scene (Camera::frame_*
    /// reads the scene while mutating the view).
    fn camera_and_scene(&mut self) -> (&mut Camera, &Scene) {
        let Self { scene, outputs, .. } = self;
        (
            &mut outputs
                .primary_mut()
                .expect("primary output registered at construction")
                .camera,
            scene,
        )
    }

    /// Split borrow: focus manager + camera + scene — the focus exit
    /// path mutates BOTH the manager and the camera while reading the
    /// scene for the restore target.
    fn focus_exit_parts(&mut self) -> (&mut FocusManager, &mut Camera, &Scene) {
        let Self {
            focus_manager,
            scene,
            outputs,
            ..
        } = self;
        (
            focus_manager,
            &mut outputs
                .primary_mut()
                .expect("primary output registered at construction")
                .camera,
            scene,
        )
    }

    /// G-E5.2: the framebuffer size of the output a frame presents to.
    /// Registry-backed: the primary output's mode, falling back to the
    /// legacy scalar while consumers migrate. The projection and picking
    /// paths consume this so the framebuffer source is EXPLICIT —
    /// G-E5.4 replaces "primary" with the output under the pointer per
    /// query. Coordinate chain (ARCHITECTURE §15): surface → window-local
    /// → workspace → world → camera → output-local → framebuffer.
    pub fn fb_size(&self) -> (f32, f32) {
        self.outputs.primary_size().unwrap_or(self.window_size)
    }

    /// Compute proj × view matrix for the current camera.
    fn proj_view(&self) -> Matrix4<f32> {
        let (w, h) = self.fb_size();
        Self::projection_for(self.spatial_mode, w, h) * self.camera().view_matrix()
    }

    /// Route a pointer event to the selected visual's InputSink.
    /// Focus follows click: sets focused visual to the selected one.
    /// Title bar hits are NOT routed to content — caller should start a drag.
    /// R7: release any active pointer constraint AND deactivate its
    /// protocol object. veyra's spatial routing can move pointer focus
    /// without a protocol-level wl_pointer leave, so smithay's
    /// automatic deactivate-on-leave does not always fire — without
    /// this, clients would keep believing they are locked/confined.
    pub fn unlock_pointer(&mut self) {
        let surfaces: Vec<WlSurface> = self
            .pointer_constraints
            .locked_surface
            .iter()
            .chain(self.pointer_constraints.confined_surface.iter())
            .cloned()
            .collect();
        if !surfaces.is_empty() {
            if let Some(ph) = self.pointer_handle.clone() {
                for surface in &surfaces {
                    smithay::wayland::pointer_constraints::with_pointer_constraint(
                        surface,
                        &ph,
                        |c| {
                            if let Some(c) = c {
                                c.deactivate();
                            }
                        },
                    );
                }
            }
            self.pointer_constraints.clear_locked();
            self.pointer_constraints.clear_confined();
            info!("pointer constraint released (deactivated)");
        }
    }

    /// R7: activate pending constraints once pointer focus ENTERS the
    /// owning surface. Constraints requested without pointer focus stay
    /// inactive — an unfocused client must not affect global pointer
    /// behavior (new_constraint only activates when already focused).
    pub fn activate_constraints_for_focus(&mut self, surface: &WlSurface) {
        let Some(ph) = self.pointer_handle.clone() else {
            return;
        };
        smithay::wayland::pointer_constraints::with_pointer_constraint(
            surface,
            &ph,
            |constraint| {
                let Some(c) = constraint else { return };
                if c.is_active() {
                    return;
                }
                match &*c {
                    smithay::wayland::pointer_constraints::PointerConstraint::Locked(_) => {
                        c.activate();
                        self.pointer_constraints.pointer_locked = true;
                        self.pointer_constraints.locked_surface = Some(surface.clone());
                        info!("pointer locked (focus entered surface)");
                    }
                    smithay::wayland::pointer_constraints::PointerConstraint::Confined(_) => {
                        c.activate();
                        self.pointer_constraints.confined_surface = Some(surface.clone());
                        info!("pointer confined (focus entered surface)");
                    }
                }
            },
        );
    }

    /// Authoritative keyboard focus setter.
    /// Updates scene focus, Wayland keyboard focus, data device focus, FocusManager, and SpatialChrome consistently.
    /// Unlocks pointer if focus changes to a different surface than the locked one.
    fn set_keyboard_focus(&mut self, vid: Option<VisualId>) {
        // Unlock pointer on focus change (unless the same visual)
        if self.pointer_constraints.pointer_locked {
            let locked_surface = self.pointer_constraints.locked_surface.clone();
            let is_same_surface = vid
                .and_then(|v| self.wayland_surfaces.get(&v))
                .is_some_and(|s| locked_surface.as_ref() == Some(s));
            if !is_same_surface {
                self.unlock_pointer();
            }
        }

        // Update scene focus
        self.scene.focus(vid);

        // J1: an actual focus transition of a real toplevel updates the
        // MRU. Only toplevel-row owners qualify — popups and external
        // visuals never pollute the application history.
        if let Some(vid) = vid {
            if self.toplevels.iter().any(|t| t.visual_id == Some(vid)) {
                self.focus_history.touch(vid);
                info!(?vid, order = ?self.focus_history.order(), "focus history updated");
                crate::debug_journal::event(
                    "focus",
                    &[
                        ("vid", crate::debug_journal::d(vid)),
                        ("mru", crate::debug_journal::d(self.focus_history.order())),
                    ],
                );
            }
        }

        // Update Wayland keyboard focus
        if let (Some(vid), Some(kh)) = (vid, self.keyboard_handle.clone()) {
            if let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() {
                let serial = self.next_serial();
                // X11 windows must be focused as X11Surface targets so
                // smithay moves the X-side input focus (BUG_LIST #19).
                let target = match self.x11_windows.get(&wl_surface) {
                    Some(x11) => KeyboardFocusTarget::X11(x11.clone()),
                    None => KeyboardFocusTarget::Wl(wl_surface),
                };
                kh.set_focus(self, Some(target), serial);
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

        // G-C4: reflect keyboard focus in the X11 window state.
        let x11_pairs: Vec<(VisualId, smithay::xwayland::xwm::X11Surface)> = self
            .x11_windows
            .iter()
            .filter_map(|(surface, x11)| {
                self.wayland_surfaces
                    .iter()
                    .find(|(_, s)| *s == surface)
                    .map(|(v, _)| (*v, x11.clone()))
            })
            .collect();
        for (xvid, x11) in x11_pairs {
            let _ = x11.set_activated(vid == Some(xvid));
        }
    }

    /// Update the data device focus to match the keyboard focus.
    /// This ensures clipboard/primary selection is offered to the correct client.
    /// G-B1: force smithay to re-evaluate the clipboard/primary
    /// selections. Smithay clears a selection whose source client died
    /// during any selection broadcast (send_selection's alive check) —
    /// toggling the data-device focus None → current triggers exactly
    /// that broadcast: dead selections are cleared and every device
    /// receives Selection{null}, then the focused client is re-offered
    /// the current state.
    fn refresh_selection_state(&mut self) {
        let kbd = KBD_FOCUS_CLIENT.lock().unwrap().clone();
        if let Some(ref seat) = self.seat {
            let dh = &self.display_handle;
            smithay::wayland::selection::data_device::set_data_device_focus::<Self>(dh, seat, None);
            smithay::wayland::selection::primary_selection::set_primary_focus::<Self>(
                dh, seat, None,
            );
            smithay::wayland::selection::data_device::set_data_device_focus::<Self>(
                dh,
                seat,
                kbd.clone(),
            );
            smithay::wayland::selection::primary_selection::set_primary_focus::<Self>(
                dh, seat, kbd,
            );
            info!("selection state refreshed after owner disconnect");
        }
    }

    fn update_data_device_focus(&mut self, vid: Option<VisualId>) {
        let client = vid.and_then(|vid| self.wayland_surfaces.get(&vid).and_then(Resource::client));
        // G-B1: publish the keyboard-focus client for the selection
        // disconnect cleanup (it restores focus after the toggle).
        *KBD_FOCUS_CLIENT.lock().unwrap() = client.clone();
        if let Some(ref seat) = self.seat {
            let dh = &self.display_handle;
            smithay::wayland::selection::data_device::set_data_device_focus::<Self>(
                dh,
                seat,
                client.clone(),
            );
            smithay::wayland::selection::primary_selection::set_primary_focus::<Self>(
                dh, seat, client,
            );
        }
    }

    /// The camera-visible area of the z=0 workspace plane, for placing
    /// new windows where the user can actually see them (J1 follow-up:
    /// placement used to ignore the frustum and clip windows at the
    /// screen edge). Uses the camera's distance to the workspace origin
    /// and the standard 45° vertical FOV.
    fn visible_bounds(&self) -> layout::VisibleBounds {
        let (w, h) = self.fb_size();
        let aspect = if h > 0.0 { w / h } else { 1.0 };
        let dist = {
            let p = self.camera().position;
            (p.x * p.x + p.y * p.y + p.z * p.z).sqrt()
        };
        layout::VisibleBounds::for_camera(dist, 45.0, aspect)
    }

    /// Emit a full window-table snapshot to the debug journal (if
    /// enabled). Cheap enough to call after every state transition.
    fn debug_snapshot(&self) {
        if !crate::debug_journal::enabled() {
            return;
        }
        let rows: Vec<crate::debug_journal::WindowRow> = self
            .scene
            .visuals
            .iter()
            .map(|v| {
                let info = self.toplevels.iter().find(|t| t.visual_id == Some(v.id));
                crate::debug_journal::WindowRow {
                    vid: v.id,
                    app_id: info.map(|t| t.app_id.clone()).unwrap_or_default(),
                    title: v.chrome.title.clone(),
                    workspace: self.workspace_for_visual(v.id),
                    pos: (
                        v.transform.position.x,
                        v.transform.position.y,
                        v.transform.position.z,
                    ),
                    size: (v.total_width(), v.total_height()),
                    focused: self.scene.focused_id == Some(v.id),
                    minimized: v.window_state == crate::scene::WindowState::Minimized,
                    maximized: info.map(|t| t.maximized).unwrap_or(false),
                    fullscreen: info.map(|t| t.fullscreened).unwrap_or(false),
                }
            })
            .collect();
        crate::debug_journal::snapshot(
            &rows,
            self.scene.focused_id,
            self.workspace_manager.active_id(),
            self.camera().position.z,
        );
    }

    /// J4: assemble the taskbar model for this frame from live state —
    /// window buttons in STABLE map order (not MRU: the order must not
    /// jump when a window is selected — physical feedback), workspace
    /// buttons, launcher pins. Pure projection of existing state; the
    /// shell owns nothing.
    fn build_taskbar(&self) -> crate::shell::TaskbarLayout {
        let (w, h) = self.fb_size();
        // Window buttons: STABLE map order (toplevel registration
        // order), active workspace only. Selection is communicated by
        // highlighting, never by reordering.
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        let label_of = |vid: VisualId| -> String {
            self.toplevels
                .iter()
                .find(|t| t.visual_id == Some(vid))
                .map(|t| {
                    let title = t.title.trim();
                    if title.is_empty() {
                        t.app_id.clone()
                    } else {
                        title.to_string()
                    }
                })
                .unwrap_or_else(|| "window".to_string())
        };
        let order: Vec<VisualId> = ws_ids
            .iter()
            .filter(|vid| self.toplevels.iter().any(|t| t.visual_id == Some(**vid)))
            .copied()
            .collect();
        let windows: Vec<(VisualId, String, bool, bool)> = order
            .iter()
            .map(|vid| {
                (
                    *vid,
                    label_of(*vid),
                    self.scene.focused_id == Some(*vid),
                    self.is_minimized(*vid),
                )
            })
            .collect();
        // Launcher pins: first few filtered entries.
        let launches: Vec<(usize, String)> = self
            .launcher
            .filtered()
            .iter()
            .take(4)
            .enumerate()
            .map(|(i, e)| (i, e.name.clone()))
            .collect();
        // Hover state for the renderer: only when the pointer is inside
        // the bar strip (the layout's hover test is bar-scoped).
        let (mx, my) = self.last_mouse;
        let hover = if my >= (h - crate::shell::TaskbarLayout::bar_height(h)) as f64 {
            Some((mx, my))
        } else {
            None
        };
        crate::shell::TaskbarLayout::build(
            w,
            h,
            &windows,
            self.workspace_manager.len(),
            self.workspace_manager.active_id(),
            &launches,
            hover,
        )
    }

    /// Handle a click inside the taskbar strip. Returns true when the
    /// click was consumed (caller must skip scene picking).
    fn handle_taskbar_click(&mut self, x: f64, y: f64) -> bool {
        let (_, h) = self.fb_size();
        let layout = self.build_taskbar();
        let Some(item) = layout.hit(h, x, y) else {
            return false;
        };
        match item.hit.clone() {
            crate::shell::TaskbarHit::Window(vid) => {
                // GNOME semantics: a taskbar click ALWAYS activates (or
                // restores) — it never minimizes. The earlier minimize-
                // on-focused-click toggle read as erratic ("sometimes
                // changes window, sometimes minimizes"); minimize stays
                // on the title-bar button and the keyboard.
                if self.is_minimized(vid) {
                    info!(?vid, "taskbar: restore window");
                    crate::debug_journal::event(
                        "taskbar_restore",
                        &[("vid", crate::debug_journal::d(vid))],
                    );
                    self.restore_minimized(vid, crate::maximize::MinimizeSource::Compositor);
                } else if self.scene.focused_id == Some(vid) {
                    info!(?vid, "taskbar: already focused");
                } else {
                    info!(?vid, "taskbar: activate window");
                    crate::debug_journal::event(
                        "taskbar_activate",
                        &[("vid", crate::debug_journal::d(vid))],
                    );
                    self.scene.select(Some(vid));
                    self.scene.bring_to_front(vid);
                    self.set_keyboard_focus(Some(vid));
                }
                true
            }
            crate::shell::TaskbarHit::Workspace(idx) => {
                info!(workspace = idx, "taskbar: switch workspace");
                crate::debug_journal::event("taskbar_workspace", &[("to", idx.to_string())]);
                if idx != self.workspace_manager.active_id() {
                    self.switch_workspace(idx);
                }
                true
            }
            crate::shell::TaskbarHit::Launch(idx) => {
                info!(index = idx, name = %item.label, "taskbar: launch");
                if self.launcher.launch(idx).is_none() {
                    info!(index = idx, "taskbar: launch failed");
                }
                true
            }
        }
    }

    /// Zoom the camera out (never in) so every non-detached visual of
    /// the active workspace fits inside the view frustum. Pure camera
    /// operation — visual transforms are never modified. Called after a
    /// new window lands in a position the current view cannot show
    /// (J1-fix follow-up: two default-size windows no longer overlap or
    /// clip; the view widens instead).
    fn auto_fit_camera(&mut self) {
        let (w, h) = self.fb_size();
        let aspect = if h > 0.0 { w / h } else { 1.0 };
        let tan_half = (45.0f32.to_radians() / 2.0).tan();
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        let mut req_w = 0.0f32;
        let mut req_h = 0.0f32;
        for v in &self.scene.visuals {
            if self.scene.detached_set.contains(&v.id) || !ws_ids.contains(&v.id) {
                continue;
            }
            let vw = v.total_width();
            let vh = v.total_height();
            let ext_x = v.transform.position.x.abs() + vw * 0.5;
            let ext_y = v.transform.position.y.abs() + vh * 0.5;
            req_w = req_w.max(ext_x);
            req_h = req_h.max(ext_y);
        }
        // Edge breathing room in world units.
        let margin = 48.0;
        let need_h = (req_h + margin) / tan_half;
        let need_w = (req_w + margin) / (tan_half * aspect);
        let need = need_h.max(need_w);
        let dist = {
            let p = self.camera().position;
            (p.x * p.x + p.y * p.y + p.z * p.z).sqrt()
        };
        if need > dist + 1.0 && need < 6000.0 {
            // Normal mode camera looks along -z from (x, y, z): push z.
            self.camera_mut().position.z = need;
            info!(dist = need, prev = dist, req_w, req_h, "camera auto-fit");
            self.schedule_render();
        }
    }

    fn route_to_content(&mut self, kind: PointerEventKind, x: f64, y: f64) -> ContentRouting {
        let Some(vid) = self.scene.selected_id else {
            return ContentRouting::NoTarget;
        };
        if !self.scene.is_active(vid) {
            return ContentRouting::NoTarget;
        }

        let (w, h) = self.fb_size();
        // P1 (audit): same degenerate-size guard as projection_for — a
        // 0-height framebuffer must not produce inf NDC coordinates.
        let (w, h) = (w.max(1.0), h.max(1.0));
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();

        // R12: the title-bar fraction of the full quad comes from the
        // visual's single conversion API (was recomputed inline here).
        let data = self.scene.visuals.iter().find(|v| v.id == vid).map(|v| {
            (
                v.id,
                v.total_width(),
                v.total_height(),
                v.title_bar_fraction(),
                v.geometry.size,
            )
        });
        let Some((vid, gw, gh, title_frac_f, geom_size)) = data else {
            return ContentRouting::NoTarget;
        };
        let title_frac = title_frac_f as f64;
        // J2: UV mapping against the WORLD transform so parented
        // visuals (popups) route clicks where they are drawn.
        let transform = self.scene.world_transform(vid);

        if let Some((u, v)) =
            input_router::screen_to_visual_uv(&pv, ndc_x, ndc_y, &transform, gw, gh)
        {
            // J3: title-bar BUTTONS win over focus/drag/resize. Clicking
            // a button dispatches through the same handlers the context
            // menu uses — no second semantics. Buttons deliberately do
            // NOT take keyboard focus (GNOME convention).
            if kind == PointerEventKind::Down {
                if let Some(button) = crate::chrome::hit_button(gw, gh, title_frac_f, u, v) {
                    info!(?vid, button = %button.name(), u, v, "title bar button pressed");
                    match button {
                        crate::chrome::TitleButton::Minimize => {
                            self.begin_minimize(vid, crate::maximize::MinimizeSource::Compositor);
                        }
                        crate::chrome::TitleButton::Maximize => {
                            self.toggle_maximize_for(
                                vid,
                                crate::maximize::MaximizeSource::Compositor,
                            );
                        }
                        crate::chrome::TitleButton::Close => {
                            if let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() {
                                for t in &self.toplevels {
                                    if t.toplevel.wl_surface() == &wl_surface {
                                        t.toplevel.send_close();
                                        info!(?vid, "title bar: close sent");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    return ContentRouting::Routed;
                }
            }

            if kind == PointerEventKind::Down {
                self.set_keyboard_focus(Some(vid));
                self.scene.bring_to_front(vid);
                info!(?vid, "focus set, brought to front");
            }

            // Resize zones win over title-bar/content routing on pointer
            // down (I3b). An 8 logical-px band along the decorated border.
            if kind == PointerEventKind::Down && self.resize_session.is_none() {
                let band_u = 8.0 / geom_size.w.max(1) as f64;
                // Full-quad height IS total_height (content + bar).
                let band_v = 8.0 / gh as f64;
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
            let geom_w = self
                .scene
                .visuals
                .iter()
                .find(|v| v.id == vid)
                .map(|v| v.geometry.size.w as f64);

            if let (Some(wl_surface), Some(ph)) = (wl_surface, pointer_handle) {
                if let Some(gw) = geom_w {
                    let geom_h = self
                        .scene
                        .visuals
                        .iter()
                        .find(|v| v.id == vid)
                        .map(|v| v.geometry.size.h as f64)
                        .unwrap_or(1.0);
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
                    // BUG_LIST #18: Smithay derives surface-local coords as
                    // `MotionEvent.location - focus origin`. In spatial mode
                    // the window is perspective-transformed on screen, so a
                    // raw screen position minus the orthographic origin is
                    // NOT the surface coordinate the user is pointing at.
                    // The unprojected `pos` IS that coordinate — deliver it
                    // as `location = origin + pos` so the subtraction yields
                    // exactly `pos` while `location` keeps global semantics
                    // (grabs, constraints, relative motion).
                    let origin = self.surface_global_origin(vid);
                    let location: smithay::utils::Point<f64, smithay::utils::Logical> = match origin
                    {
                        Some(o) => smithay::utils::Point::new(o.x + pos.x, o.y + pos.y),
                        None => pos,
                    };
                    let mot_ev = MotionEvent {
                        location,
                        serial,
                        time,
                    };
                    match kind {
                        PointerEventKind::Motion => {
                            self.last_wayland_focus = Some(wl_surface.clone());
                            ph.motion(self, origin.map(|o| (wl_surface.clone(), o)), &mot_ev);
                            ph.frame(self);
                        }
                        PointerEventKind::Down | PointerEventKind::Up => {
                            self.last_wayland_focus = Some(wl_surface.clone());
                            ph.motion(self, origin.map(|o| (wl_surface.clone(), o)), &mot_ev);
                            ph.button(self, &btn_ev);
                            ph.frame(self);
                            // #12: button serials are the popup-grab source.
                            self.record_input_serial(&wl_surface, serial);
                            info!(?vid, ?pos, ?kind, "wl_pointer.enter + button + frame");
                        }
                        PointerEventKind::Scroll(_, _) => {}
                    }
                    return ContentRouting::Routed;
                }
            }

            let Some(sink) = self.input_sinks.get_mut(&vid) else {
                return ContentRouting::NoTarget;
            };
            sink.handle_pointer(kind, content_u, content_v);
            return ContentRouting::Routed;
        }
        ContentRouting::NoTarget
    }

    /// G-D3: publish a toplevel to ext_foreign_toplevel_list clients.
    pub fn register_foreign_toplevel(&mut self, vid: VisualId, title: &str, app_id: &str) {
        let handle = self
            .foreign_toplevel_state
            .new_toplevel::<Self>(title.to_string(), app_id.to_string());
        self.foreign_toplevels.insert(vid, handle);
    }

    /// G-D3: push title/app_id updates to foreign-toplevel clients.
    pub fn update_foreign_toplevel(&mut self, vid: VisualId, title: &str, app_id: &str) {
        if let Some(handle) = self.foreign_toplevels.get(&vid) {
            handle.send_title(title);
            handle.send_app_id(app_id);
            handle.send_done();
        }
    }

    /// G-D3: withdraw a toplevel from foreign-toplevel clients.
    pub fn unregister_foreign_toplevel(&mut self, vid: VisualId) {
        if let Some(handle) = self.foreign_toplevels.remove(&vid) {
            self.foreign_toplevel_state.remove_toplevel(&handle);
        }
    }

    /// #11: remove a subsurface's visual (parent destroy / subsurface
    /// death). The wl_surface keeps its compositor-global lifetime;
    /// only the workspace-local presentation state goes away.
    pub fn remove_subsurface_visual(&mut self, surface: &WlSurface) {
        self.subsurface_parents.remove(surface);
        if let Some(vid) = self.subsurface_visuals.remove(surface) {
            info!(?vid, "subsurface removed");
            self.scene.remove(vid);
            self.wayland_surfaces.remove(&vid);
            for i in 0..self.workspace_manager.len() {
                if let Some(ws) = self.workspace_manager.get_mut(i) {
                    ws.remove(vid);
                }
            }
            self.schedule_render();
        }
    }

    /// #11: remove all subsurface visuals parented to `parent_surface`
    /// (called when the parent's visual is destroyed).
    pub fn remove_subsurfaces_of(&mut self, parent_surface: &WlSurface) {
        let children: Vec<WlSurface> = self
            .subsurface_parents
            .iter()
            .filter(|(_, p)| *p == parent_surface)
            .map(|(s, _)| s.clone())
            .collect();
        for s in children {
            self.remove_subsurface_visual(&s);
        }
    }

    /// G-C4: find the visual id backing an X11 window, if mapped.
    pub fn x11_visual_for(&self, window: &smithay::xwayland::xwm::X11Surface) -> Option<VisualId> {
        let wid = window.window_id();
        self.x11_windows
            .iter()
            .find(|(_, w)| w.window_id() == wid)
            .and_then(|(surface, _)| self.find_vid_for_surface(surface))
    }

    /// G-C4: find the visual id for any tracked wl_surface (toplevel,
    /// popup, or X11-associated).
    pub fn find_vid_for_surface(&self, surface: &WlSurface) -> Option<VisualId> {
        if let Some(vid) = self
            .toplevels
            .iter()
            .find(|t| t.toplevel.wl_surface() == surface)
            .and_then(|t| t.visual_id)
        {
            return Some(vid);
        }
        if let Some(vid) = self
            .popups
            .iter()
            .find(|p| &p.wl_surface == surface)
            .and_then(|p| p.visual_id)
        {
            return Some(vid);
        }
        self.wayland_surfaces
            .iter()
            .find(|(_, s)| *s == surface)
            .map(|(vid, _)| *vid)
    }

    /// G-C4: remove an X11 window's visual + bookkeeping (map loss,
    /// destroy, minimize). The X window itself stays alive where the
    /// protocol allows remapping.
    pub fn destroy_x11_visual(&mut self, vid: VisualId) {
        // #11: child subsurfaces die with the parent
        if let Some(surface) = self.wayland_surfaces.get(&vid).cloned() {
            self.remove_subsurfaces_of(&surface);
        }
        self.unregister_foreign_toplevel(vid);
        self.scene.remove(vid);
        self.wayland_surfaces.remove(&vid);
        self.input_sinks.remove(&vid);
        for i in 0..self.workspace_manager.len() {
            if let Some(ws) = self.workspace_manager.get_mut(i) {
                ws.remove(vid);
            }
        }
        self.focus_history.remove(vid);
        if self.scene.focused_id == Some(vid) {
            self.refocus_after_close(Some(vid));
        }
        if self.scene.selected_id == Some(vid) {
            self.scene.selected_id = None;
            self.scene.focus(self.scene.focused_id);
        }
        if self.interaction.is_dragging_visual(vid) {
            self.interaction.handle_pointer_up();
        }
        self.schedule_render();
    }

    /// Feed a key event to smithay's keyboard handle WITHOUT requiring
    /// a focused visual (BUG_LIST #16). smithay updates its XKB
    /// modifier state and broadcasts to whatever client currently holds
    /// keyboard focus; with no focus, only the state is updated.
    fn feed_keyboard_event(&mut self, key: u32, pressed: bool) {
        let Some(kh) = self.keyboard_handle.clone() else {
            return;
        };
        let serial = self.next_serial();
        let time = now_ms();
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let _ = kh.input::<(), _>(self, Keycode::new(key), state, serial, time, |_, _, _| {
            FilterResult::Forward
        });
        let _ = self.display_handle.flush_clients();
    }

    /// Route a keyboard event to the focused visual's InputSink.
    /// key: winit platform key code (X11 keycodes when under X11, offset +8 from evdev).
    /// The offset is subtracted to get raw evdev codes for HID mapping.
    fn route_keyboard(&mut self, key: u32, pressed: bool) {
        let Some(vid) = self.scene.focused_id else {
            tracing::debug!(key, pressed, "keyboard event dropped: no focused visual");
            return;
        };
        if !self.scene.is_active(vid) {
            tracing::debug!(
                key,
                pressed,
                ?vid,
                "keyboard event dropped: visual not active"
            );
            return;
        }

        // For Wayland surfaces, set keyboard focus and deliver key event
        let wl_focus = self.wayland_surfaces.get(&vid).cloned();
        let kh = self.keyboard_handle.clone();
        if let (Some(wl_surface), Some(ref kh_handle)) = (wl_focus, kh) {
            let serial = self.next_serial();
            let time = now_ms();
            let state = if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            };
            // Ensure keyboard focus is on the right surface — as an
            // X11Surface target for X11 windows (BUG_LIST #19).
            let focus_target = match self.x11_windows.get(&wl_surface) {
                Some(x11) => KeyboardFocusTarget::X11(x11.clone()),
                None => KeyboardFocusTarget::Wl(wl_surface.clone()),
            };
            kh_handle.set_focus(self, Some(focus_target), serial);
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
            // #12 edge case: keyboard key serials are INPUT serials —
            // popups opened from the keyboard (context-menu key, app
            // menus) validate against the key-press serial. Without this
            // recording, every keyboard-opened popup grab was rejected.
            self.record_input_serial(&wl_surface, serial);
            let _ = self.display_handle.flush_clients();
            return;
        }

        // For external producers (non-Wayland), use InputSink path
        let Some(sink) = self.input_sinks.get_mut(&vid) else {
            return;
        };
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
    /// `closed` is the window that just died — the replacement is the
    /// most recent MRU entry OTHER than it (J1). When some other window
    /// was closed instead, the current focus stays a valid candidate.
    fn refocus_after_close(&mut self, closed: Option<VisualId>) {
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        // J1: focus after close comes from the MRU history — the most
        // recently focused OTHER window on this workspace, not a
        // stacking-order guess. `this` reborrows self immutably for the
        // focusability predicate while the history query runs.
        let this: &LookingGlass = self;
        let replacement = this.focus_history.focus_replacement(closed, &move |vid| {
            Some(vid) != closed
                && ws_ids.contains(&vid)
                && this.scene.is_visible(vid)
                && !this.is_minimized(vid)
        });
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
    pub fn begin_client_resize(
        &mut self,
        vid: VisualId,
        w: i32,
        h: i32,
    ) -> Option<smithay::utils::Serial> {
        if w <= 0 || h <= 0 {
            return None;
        }
        if self.client_resizes.awaiting_ack(vid) {
            return None;
        }
        let wl_surface = self.wayland_surfaces.get(&vid).cloned()?;
        let toplevel = self
            .toplevels
            .iter()
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
            self.toplevels
                .iter()
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
        // I7: neither do fullscreen windows.
        if self.is_fullscreened(vid) {
            info!(?vid, "resize session refused: window is fullscreen");
            return false;
        }
        let Some(wl_surface) = self.wayland_surfaces.get(&vid).cloned() else {
            return false;
        };
        if !self
            .toplevels
            .iter()
            .any(|t| t.toplevel.wl_surface() == &wl_surface)
        {
            return false; // popups are transient and not resizable
        }
        let Some(visual) = self.scene.get(vid) else {
            return false;
        };
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
            let attrs = *states.cached_state.get::<SurfaceCachedState>().current();
            (
                attrs.min_size.w.max(1),
                attrs.min_size.h.max(1),
                if attrs.max_size.w > 0 {
                    attrs.max_size.w
                } else {
                    i32::MAX
                },
                if attrs.max_size.h > 0 {
                    attrs.max_size.h
                } else {
                    i32::MAX
                },
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
        let Some(session) = self.resize_session.clone() else {
            return;
        };
        let (w, h) = self.fb_size();
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();
        // Unproject against the FROZEN start transform: the session frame
        // does not follow the visual as its geometry evolves.
        let Some(local) = input_router::screen_to_visual_local_point(
            &pv,
            ndc_x,
            ndc_y,
            &session.start_transform,
            session.start_total.0,
            session.start_total.1,
        ) else {
            return;
        };
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
        let Some(session) = self.resize_session.as_ref() else {
            return;
        };
        if session.vid != vid {
            return;
        }
        if self.client_resizes.awaiting_ack(vid) {
            return;
        }
        let desired = session.desired;
        let outstanding = self.client_resizes.entry(vid).map(|e| e.requested);
        if outstanding != Some(desired) {
            self.begin_client_resize(vid, desired.0, desired.1);
        }
    }

    /// Terminate the resize session on pointer release.
    /// Returns true when a session was active.
    pub fn finish_resize_session(&mut self) -> bool {
        let Some(session) = self.resize_session.take() else {
            return false;
        };
        self.abort_client_resize(session.vid);
        info!(vid = ?session.vid, "resize session finished");
        true
    }

    // ── Maximize/unmaximize (I4) ─────────────────────────────────────────

    /// The configured size for a maximized surface: the current view size.
    /// The window quad grows around its spatial position when the client
    /// commits bigger buffers — the transform itself is never touched.
    fn maximize_target(&self) -> (i32, i32) {
        let (w, h) = self.fb_size();
        ((w.round() as i32).max(1), (h.round() as i32).max(1))
    }

    /// Whether the toplevel for `vid` is currently maximized (I4).
    pub fn is_maximized(&self, vid: VisualId) -> bool {
        self.toplevels
            .iter()
            .any(|t| t.visual_id == Some(vid) && t.maximized)
    }

    /// Whether the toplevel for `vid` is currently fullscreen (I7).
    pub fn is_fullscreened(&self, vid: VisualId) -> bool {
        self.toplevels
            .iter()
            .any(|t| t.visual_id == Some(vid) && t.fullscreened)
    }

    // ── I7: fullscreen / unfullscreen ─────────────────────────────────

    /// Begin a fullscreen transition.
    ///
    /// Snapshot discipline: the pre-fullscreen presentation + state is
    /// captured EXACTLY ONCE here (entering PENDING_FULLSCREEN), never
    /// while fullscreen. The client is configured with the Fullscreen
    /// state bit and the presentation area — never a hardcoded size.
    ///
    /// Maximize interactions: a maximized window snapshots
    /// `was_maximized=true` and keeps the Maximized state bit parked on
    /// the toplevel; unfullscreen returns it to maximized (not NORMAL).
    pub fn begin_fullscreen(&mut self, vid: VisualId, source: crate::fullscreen::FullscreenSource) {
        use crate::fullscreen::{FullscreenIntent, FullscreenKind};
        let Some(toplevel) = self.toplevel_for_vid(vid) else {
            return;
        };
        if self.is_fullscreened(vid) {
            info!(?vid, ?source, "fullscreen ignored: already fullscreen");
            return;
        }
        if self.resize_session.as_ref().is_some_and(|s| s.vid == vid) {
            info!(
                ?vid,
                ?source,
                "fullscreen refused: resize session in progress"
            );
            return;
        }
        let Some(visual) = self.scene.get(vid) else {
            return;
        };
        let restore = (visual.geometry.size.w, visual.geometry.size.h);
        if restore.0 <= 0 || restore.1 <= 0 {
            info!(
                ?vid,
                ?source,
                "fullscreen refused: no committed geometry yet"
            );
            return;
        }
        let was_maximized = self.is_maximized(vid);
        let snapshot = crate::fullscreen::FullscreenSnapshot::capture(
            restore,
            visual.transform.position,
            visual.transform.rotation,
            was_maximized,
        );
        // Size the client to the presentation area — nothing hardcoded.
        let target = crate::fullscreen::PresentationArea::for_window_size(self.fb_size()).size();

        // Defer while the surface still owes an ACK for a prior configure.
        let wl_surface = self.wayland_surfaces.get(&vid).cloned();
        let unacked = wl_surface.is_some_and(|wl_surface| {
            with_states(&wl_surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|attrs| !attrs.lock().unwrap().pending_configures().is_empty())
                    .unwrap_or(false)
            })
        });
        if unacked || self.client_resizes.awaiting_ack(vid) {
            self.fullscreen
                .defer(vid, FullscreenKind::Fullscreen, source);
            info!(?vid, ?source, "fullscreen deferred: configure outstanding");
            return;
        }

        // Park the maximize intent if one races with fullscreen: the
        // fullscreen transition owns the window from here on.
        self.maximize.abort(vid);
        toplevel.with_pending_state(|state| {
            state.size = Some(smithay::utils::Size::new(target.0, target.1));
            state.states.set(xdg_toplevel::State::Fullscreen);
            // was_maximized keeps the Maximized bit until unfullscreen;
            // the client still reports fullscreen+maximized correctly.
            if !was_maximized {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
        });
        let serial = toplevel.send_configure();
        self.client_resizes.mark_sent(vid, serial, target);
        self.fullscreen.begin(FullscreenIntent {
            vid,
            kind: FullscreenKind::Fullscreen,
            source,
            serial,
            target,
            previous: restore,
            snapshot: None,
        });
        // The snapshot lives on the toplevel row so it survives intent
        // replacement and is consumed by unfullscreen.
        if let Some(info) = self.toplevels.iter_mut().find(|t| t.visual_id == Some(vid)) {
            info.fullscreen_snapshot = Some(snapshot);
        }
        info!(
            ?vid,
            ?source,
            ?serial,
            target_w = target.0,
            target_h = target.1,
            was_maximized,
            "fullscreen requested"
        );
    }

    /// Begin an unfullscreen transition: restores the state that existed
    /// immediately before fullscreen (MAXIMIZED → FULLSCREEN → MAXIMIZED,
    /// NORMAL → FULLSCREEN → NORMAL) and hands the pre-fullscreen size
    /// back to the client.
    pub fn begin_unfullscreen(
        &mut self,
        vid: VisualId,
        source: crate::fullscreen::FullscreenSource,
    ) {
        use crate::fullscreen::{FullscreenIntent, FullscreenKind};
        let Some(toplevel) = self.toplevel_for_vid(vid) else {
            return;
        };
        if !self.is_fullscreened(vid) && self.fullscreen.intent(vid).is_none() {
            info!(?vid, ?source, "unfullscreen ignored: not fullscreen");
            return;
        }
        let snapshot = self
            .toplevels
            .iter()
            .find(|t| t.visual_id == Some(vid))
            .and_then(|t| t.fullscreen_snapshot.clone())
            .unwrap_or_else(|| {
                // Defensive fallback: no snapshot recorded (e.g. a client
                // that set fullscreen before veyra saw geometry). Restore
                // to the window size, un-maximized.
                crate::fullscreen::FullscreenSnapshot::capture(
                    (800, 600),
                    cgmath::Vector3::new(0.0, 0.0, 0.0),
                    cgmath::Quaternion::new(1.0, 0.0, 0.0, 0.0),
                    false,
                )
            });
        let target = snapshot.restore_size;
        let was_maximized = snapshot.was_maximized;

        let wl_surface = self.wayland_surfaces.get(&vid).cloned();
        let unacked = wl_surface.is_some_and(|wl_surface| {
            with_states(&wl_surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|attrs| !attrs.lock().unwrap().pending_configures().is_empty())
                    .unwrap_or(false)
            })
        });
        if unacked || self.client_resizes.awaiting_ack(vid) {
            self.fullscreen
                .defer(vid, FullscreenKind::Unfullscreen, source);
            info!(
                ?vid,
                ?source,
                "unfullscreen deferred: configure outstanding"
            );
            return;
        }

        toplevel.with_pending_state(|state| {
            state.size = Some(smithay::utils::Size::new(target.0, target.1));
            state.states.unset(xdg_toplevel::State::Fullscreen);
            // MAXIMIZED → FULLSCREEN → MAXIMIZED: the bit stays parked
            // while fullscreen, so unfullscreen leaves it set.
        });
        let previous = self
            .scene
            .get(vid)
            .map(|v| (v.geometry.size.w, v.geometry.size.h))
            .unwrap_or(target);
        let serial = toplevel.send_configure();
        self.client_resizes.mark_sent(vid, serial, target);
        self.fullscreen.begin(FullscreenIntent {
            vid,
            kind: FullscreenKind::Unfullscreen,
            source,
            serial,
            target,
            previous,
            snapshot: Some(snapshot),
        });
        info!(
            ?vid,
            ?source,
            ?serial,
            restore_w = target.0,
            restore_h = target.1,
            was_maximized,
            "unfullscreen requested"
        );
    }

    /// Toggle fullscreen on the focused (or selected) visual.
    pub fn toggle_fullscreen_selected(&mut self, source: crate::fullscreen::FullscreenSource) {
        let Some(vid) = self.scene.focused_id.or(self.scene.selected_id) else {
            info!(?source, "fullscreen toggle ignored: no focused visual");
            return;
        };
        self.toggle_fullscreen_for(vid, source);
    }

    /// Toggle fullscreen on a specific visual (also the context-menu path).
    pub fn toggle_fullscreen_for(
        &mut self,
        vid: VisualId,
        source: crate::fullscreen::FullscreenSource,
    ) {
        if self.is_fullscreened(vid) {
            self.begin_unfullscreen(vid, source);
        } else {
            self.begin_fullscreen(vid, source);
        }
    }

    /// Complete a fullscreen/unfullscreen transaction after a client
    /// commit. Same rules as the maximize coordinator: draining commits
    /// (acked configure, old size still committed) keep the intent armed;
    /// `committed == target` completes with client_matched=true; a third
    /// size completes with the acknowledged STATE applying anyway.
    fn complete_fullscreen_intent(&mut self, vid: VisualId, committed: (i32, i32)) {
        use crate::fullscreen::FullscreenKind;
        let Some(intent) = self.fullscreen.intent(vid) else {
            return;
        };
        if committed == intent.previous && committed != intent.target {
            return; // draining commit — keep the intent armed
        }
        let intent = self
            .fullscreen
            .take_intent(vid)
            .expect("intent peeked above");
        let client_matched = committed == intent.target;
        if let Some(info) = self.toplevels.iter_mut().find(|t| t.visual_id == Some(vid)) {
            match intent.kind {
                FullscreenKind::Fullscreen => {
                    info.fullscreened = true;
                    // Presentation: fullscreen is temporary presentation
                    // policy, NOT a reset transform. The parked pose is
                    // the fullscreen snapshot on the toplevel row; the
                    // visual centers on the workspace view so the quad
                    // fills the viewport edge to edge.
                    if let Some(v) = self.scene.get_mut(vid) {
                        v.transform.position =
                            cgmath::Vector3::new(0.0, 0.0, v.transform.position.z);
                        v.transform.rotation = cgmath::Quaternion::new(1.0, 0.0, 0.0, 0.0);
                    }
                }
                FullscreenKind::Unfullscreen => {
                    info.fullscreened = false;
                    // Restore from the snapshot captured at ENTRY — the
                    // snapshot rides on the intent (unfullscreen took a
                    // copy); consumed exactly once here.
                    if let Some(snapshot) = info.fullscreen_snapshot.take() {
                        info.maximized = snapshot.was_maximized;
                        if !snapshot.was_maximized {
                            info.restore_size = None;
                            info.restore_pose = None;
                        }
                        if let Some(v) = self.scene.get_mut(vid) {
                            let (px, py, pz) = snapshot.restore_pos;
                            let [ri, rj, rk, rw] = snapshot.restore_rot;
                            v.transform.position = cgmath::Vector3::new(px, py, pz);
                            // cgmath Quaternion::new is scalar-first (w, x, y, z)
                            v.transform.rotation = cgmath::Quaternion::new(rw, ri, rj, rk);
                        }
                        if snapshot.was_maximized {
                            // Back in maximized presentation: centered on
                            // the view, bit still parked from fullscreen.
                            if let Some(v) = self.scene.get_mut(vid) {
                                v.transform.position =
                                    cgmath::Vector3::new(0.0, 0.0, v.transform.position.z);
                                v.transform.rotation = cgmath::Quaternion::new(1.0, 0.0, 0.0, 0.0);
                            }
                        }
                    }
                }
            }
        }
        let state_msg = match intent.kind {
            FullscreenKind::Fullscreen => "fullscreen fulfilled",
            FullscreenKind::Unfullscreen => "unfullscreen fulfilled",
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
                  w = intent.target.0, h = intent.target.1, "{}", state_msg);
        }
        self.schedule_render();
    }

    /// Flush a deferred fullscreen/unfullscreen request (ack path).
    fn flush_fullscreen_deferred(&mut self) {
        if let Some((vid, kind, source)) = self.fullscreen.take_deferred() {
            match kind {
                crate::fullscreen::FullscreenKind::Fullscreen => self.begin_fullscreen(vid, source),
                crate::fullscreen::FullscreenKind::Unfullscreen => {
                    self.begin_unfullscreen(vid, source)
                }
            }
        }
    }

    fn toplevel_for_vid(&self, vid: VisualId) -> Option<ToplevelSurface> {
        self.toplevels
            .iter()
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
        let Some(toplevel) = self.toplevel_for_vid(vid) else {
            return;
        };
        if self.is_maximized(vid) {
            info!(?vid, ?source, "maximize ignored: already maximized");
            return;
        }
        // Explicit maximize↔fullscreen semantics (I7): a fullscreen
        // window stays fullscreen; maximize requests on it are refused
        // (GNOME-style). Exit fullscreen first.
        if self.is_fullscreened(vid) {
            info!(?vid, ?source, "maximize refused: window is fullscreen");
            return;
        }
        if self.resize_session.as_ref().is_some_and(|s| s.vid == vid) {
            info!(
                ?vid,
                ?source,
                "maximize refused: resize session in progress"
            );
            return;
        }
        let Some(visual) = self.scene.get(vid) else {
            return;
        };
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
        let unacked = wl_surface.is_some_and(|wl_surface| {
            with_states(&wl_surface, |states| {
                states
                    .data_map
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
            vid,
            kind: MaximizeKind::Maximize,
            source,
            serial,
            target,
            restore,
            previous: restore,
            restore_pos,
            restore_rot,
        });
        info!(
            ?vid,
            ?source,
            ?serial,
            target_w = target.0,
            target_h = target.1,
            restore_w = restore.0,
            restore_h = restore.1,
            "maximize requested"
        );
    }

    /// Begin an unmaximize transition (I4): configure the client back to
    /// its pre-maximize committed size and clear the Maximized state bit.
    pub fn begin_unmaximize(&mut self, vid: VisualId, source: crate::maximize::MaximizeSource) {
        use crate::maximize::{MaximizeIntent, MaximizeKind};
        let Some(toplevel) = self.toplevel_for_vid(vid) else {
            return;
        };
        if !self.is_maximized(vid) {
            info!(?vid, ?source, "unmaximize ignored: not maximized");
            return;
        }
        let restore = self
            .toplevels
            .iter()
            .find(|t| t.visual_id == Some(vid))
            .and_then(|t| t.restore_size)
            .unwrap_or_else(|| self.maximize_target());
        let wl_surface = self.wayland_surfaces.get(&vid).cloned();
        let unacked = wl_surface.is_some_and(|wl_surface| {
            with_states(&wl_surface, |states| {
                states
                    .data_map
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
        let previous = self
            .scene
            .get(vid)
            .map(|v| (v.geometry.size.w, v.geometry.size.h))
            .unwrap_or(restore);
        // Unmaximize restores the captured pre-maximize presentation pose
        // (position + rotation); the size restore goes to the client.
        let (restore_pos, restore_rot) = self
            .toplevels
            .iter()
            .find(|t| t.visual_id == Some(vid))
            .and_then(|t| t.restore_pose)
            .unwrap_or_else(|| match self.scene.get(vid) {
                Some(v) => {
                    let p = v.transform.position;
                    let r = v.transform.rotation;
                    ((p.x, p.y, p.z), [r.v.x, r.v.y, r.v.z, r.s])
                }
                None => ((0.0, 0.0, 0.0), [0.0, 0.0, 0.0, 1.0]),
            });
        let serial = toplevel.send_configure();
        self.client_resizes.mark_sent(vid, serial, restore);
        self.maximize.begin(MaximizeIntent {
            vid,
            kind: MaximizeKind::Unmaximize,
            source,
            serial,
            target: restore,
            restore,
            previous,
            restore_pos,
            restore_rot,
        });
        info!(
            ?vid,
            ?source,
            ?serial,
            restore_w = restore.0,
            restore_h = restore.1,
            "unmaximize requested"
        );
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
        self.toplevels
            .iter()
            .any(|t| t.visual_id == Some(vid) && t.minimized)
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
            info!(
                ?vid,
                ?source,
                "minimize refused: resize session in progress"
            );
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
            info!(
                ?vid,
                ?source,
                "minimize refused: not a toplevel (popups/transients stay visible)"
            );
            return;
        }
        // An in-flight maximize/unmaximize transition would fight the
        // hidden state (center-on-view at commit). Drop it: the surface
        // just stops being visible.
        self.maximize.abort(vid);
        // An in-flight fullscreen transition would also apply a
        // presentation pose on commit while hidden; drop it. The parked
        // fullscreen snapshot stays so a later unfullscreen still
        // restores the original state.
        self.fullscreen.abort(vid);

        let was_focused = self.scene.focused_id == Some(vid);
        if let Some(info) = self.toplevels.iter_mut().find(|t| t.visual_id == Some(vid)) {
            info.minimized = true;
        }
        self.scene.set_minimized(vid, true);
        info!(?vid, ?source, "minimize applied");
        crate::debug_journal::event("minimize", &[("vid", crate::debug_journal::d(vid))]);
        self.debug_snapshot();

        // J1: the minimized window leaves the application MRU immediately
        // (acceptance table: minimize A → MRU drops A). Entries are
        // removed one by one so the predicate can borrow `self` while
        // the history mutates.
        let stale: Vec<VisualId> = self
            .focus_history
            .order()
            .iter()
            .copied()
            .filter(|v| self.scene.is_minimized(*v))
            .collect();
        for v in &stale {
            self.focus_history.remove(*v);
        }
        if !stale.is_empty() {
            info!(affected = ?stale, order = ?self.focus_history.order(),
                  "focus history pruned after minimize");
        }

        if was_focused {
            // Focus the best remaining window on this workspace: J1 MRU
            // order (most recently focused other window), skipping the
            // just-minimized one.
            let ws_ids = self.workspace_manager.active().visual_ids.clone();
            let this: &LookingGlass = self;
            let replacement = this.focus_history.focus_replacement(Some(vid), &move |r| {
                r != vid && ws_ids.contains(&r) && this.scene.is_visible(r) && !this.is_minimized(r)
            });
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
                info!(
                    ?vid,
                    workspace = ws_idx,
                    "restoring across workspace switch"
                );
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
        crate::debug_journal::event("restore", &[("vid", crate::debug_journal::d(vid))]);
        self.debug_snapshot();
        self.schedule_render();
    }

    /// Restore the most recently minimized window (F10 / shell path).
    /// The scan runs in reverse toplevel order, so the latest-created
    /// minimized window wins.
    pub fn restore_last_minimized(&mut self, source: crate::maximize::MinimizeSource) {
        let target = self
            .toplevels
            .iter()
            .rev()
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
    /// windows (kept centered on the view), minimized windows (hidden;
    /// their transform must be preserved for restore) and fullscreen
    /// windows (presented centered; snapshot holds the restore pose).
    fn layout_detached(&self) -> Vec<VisualId> {
        let mut d = self.scene.detached_set.clone();
        d.extend(
            self.toplevels
                .iter()
                .filter(|t| t.maximized || t.minimized || t.fullscreened)
                .filter_map(|t| t.visual_id),
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
        let Some(intent) = self.maximize.intent(vid) else {
            return;
        };
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
                        info.restore_pose = Some((
                            (p.x, p.y, p.z),
                            [
                                intent.restore_rot[0],
                                intent.restore_rot[1],
                                intent.restore_rot[2],
                                intent.restore_rot[3],
                            ],
                        ));
                        v.transform.position = cgmath::Vector3::new(0.0, 0.0, p.z);
                        v.transform.rotation = cgmath::Quaternion::new(1.0, 0.0, 0.0, 0.0);
                    }
                }
                MaximizeKind::Unmaximize => {
                    info.maximized = false;
                    if let Some(v) = self.scene.get_mut(vid) {
                        // Restore the exact pre-maximize pose captured at
                        // maximize time (fall back to the transition pose).
                        let pose = info
                            .restore_pose
                            .take()
                            .unwrap_or((intent.restore_pos, intent.restore_rot));
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
        let Some((vid, kind, source)) = self.maximize.take_deferred() else {
            return;
        };
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
        let (w, h) = self.fb_size();
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();
        let ws_ids: Vec<VisualId> = self
            .workspace_manager
            .active()
            .visual_ids
            .iter()
            .copied()
            .filter(|id| self.scene.is_visible(*id))
            .collect();
        let picked = self.scene.pick_visible(&pv, ndc_x, ndc_y, &ws_ids);

        if let Some((vid, _)) = picked {
            let ws_count = self.workspace_manager.len();
            self.context_menu.show(x, y, vid, ws_count);
            self.context_menu.set_maximize_label(self.is_maximized(vid));
            let m = crate::context_menu::MenuMetrics::for_framebuffer(
                self.fb_size().0,
                self.fb_size().1,
            );
            info!(
                menu_width = m.menu_width,
                item_height = m.item_height,
                glyph_scale = m.glyph_scale,
                fb_w = self.fb_size().0,
                fb_h = self.fb_size().1,
                "context menu metrics"
            );
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
                let cam = self.camera().clone();
                self.focus_manager.enter(&cam, target, &self.scene);
                info!(?target, "context menu: focus");
            }
            MenuAction::Arrange => {
                let ws = self.workspace_manager.active_mut();
                let mode = ws.layout_mode;
                let detached = self.layout_detached();
                let eligible = self.workspace_manager.active().visual_ids.clone();
                let (ww, wh) = self.fb_size();
                layout::apply_layout(
                    &mut self.scene,
                    mode,
                    &layout::LayoutConfig::default(),
                    &detached,
                    ww,
                    wh,
                    &eligible,
                );
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
                    // The window keeps its world position: if it now sits
                    // outside the target workspace's view, the auto-fit on
                    // the next switch will frame it. Moving to the ACTIVE
                    // workspace (no-op switch) fits immediately.
                    info!(
                        ?target,
                        workspace = ws_idx,
                        "context menu: move to workspace"
                    );
                    if ws_idx == current_ws {
                        self.auto_fit_camera();
                    }
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
            MenuAction::Fullscreen => {
                // Same coordinator as Meta+G / F12 / client requests (I7).
                self.toggle_fullscreen_for(target, crate::fullscreen::FullscreenSource::Compositor);
                info!(?target, "context menu: fullscreen toggle");
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
        let m =
            crate::context_menu::MenuMetrics::for_framebuffer(self.fb_size().0, self.fb_size().1);
        if let Some(idx) =
            self.context_menu
                .item_at(x, y, m.menu_width as f64, m.item_height as f64)
        {
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
        // G-E5.4: convert global-plane pointer px to output-local px.
        let (_out, x, y) = self.resolve_pointer_output(x, y);
        self.press_pos = (x, y);
        self.event_serial = self.event_serial.wrapping_add(1);
        self.interaction.window_size = self.fb_size();
        // J4: the shell plane owns the bottom strip — clicks there route
        // to the taskbar (workspace switch, window activate/minimize,
        // launcher) and never reach the 3D scene.
        {
            let (_, h) = self.fb_size();
            let bar_top = h - crate::shell::TaskbarLayout::bar_height(h);
            if y >= bar_top as f64 && self.handle_taskbar_click(x, y) {
                self.schedule_render();
                return;
            }
        }
        // G-B2: during a client DnD grab no new compositor manipulation
        // may start, but the press still reaches the seat pointer so
        // Smithay's button bookkeeping stays consistent. The focus is
        // the SPATIAL pick at the pointer position — route_to_content
        // routes to the selected visual, which is the drag origin, not
        // the surface under the pointer.
        if self.dnd_active {
            if let Some(ph) = self.pointer_handle.clone() {
                let serial = self.next_serial();
                let time = now_ms();
                let picked = self.pick_wayland_target(x, y);
                // BUG_LIST #18: `location = origin + pos` delivery (see
                // route_to_content) so the drop target sees the true
                // surface coordinate in spatial mode.
                let focus = picked.as_ref().and_then(|(vid, s, _)| {
                    self.surface_global_origin(*vid).map(|o| (s.clone(), o))
                });
                let location: smithay::utils::Point<f64, smithay::utils::Logical> =
                    match (&picked, &focus) {
                        (Some((_, _, pos)), Some((_, o))) => {
                            smithay::utils::Point::new(o.x + pos.x, o.y + pos.y)
                        }
                        _ => (x, y).into(),
                    };
                let focus_for_ledger = focus.clone();
                ph.motion(
                    self,
                    focus,
                    &MotionEvent {
                        location,
                        serial,
                        time,
                    },
                );
                ph.button(
                    self,
                    &ButtonEvent {
                        serial,
                        time,
                        button: 0x110,
                        state: smithay::backend::input::ButtonState::Pressed,
                    },
                );
                ph.frame(self);
                // #12: button serials are the popup-grab source.
                if let Some((fs, _)) = focus_for_ledger.as_ref() {
                    self.record_input_serial(fs, serial);
                }
            }
            let _ = self.display_handle.flush_clients();
            return;
        }
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        let cam = self.camera().clone();
        let mode = self.interaction.handle_pointer_down(
            x,
            y,
            &mut self.scene,
            &cam,
            self.spatial_mode,
            shift,
            ctrl,
            alt,
            Some(ws_ids),
        );
        // In overview mode, clicking a visual should focus it
        if matches!(self.focus_manager.camera_mode, CameraMode::Overview) {
            if let Some(vid) = self.scene.selected_id {
                let cam = self.camera().clone();
                self.focus_manager.enter(&cam, vid, &self.scene);
                info!(?vid, "overview click -> focus");
            }
            return;
        }
        // In workspace overview, clicking a visual switches to its workspace
        if matches!(
            self.focus_manager.camera_mode,
            CameraMode::WorkspaceOverview
        ) {
            if let Some(vid) = self.scene.selected_id {
                if let Some(ws_id) = self.workspace_for_visual(vid) {
                    let _ = self.activate_workspace(ws_id);
                    let (fm, cam) = self.focus_parts();
                    fm.exit_overview(cam);
                    self.set_keyboard_focus(Some(vid));
                    info!(
                        ?vid,
                        workspace = ws_id,
                        "workspace overview click -> switch"
                    );
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
                if self.route_to_content(PointerEventKind::Down, x, y)
                    == ContentRouting::TitleBarHit
                {
                    // Start a translate drag from the title bar
                    let ws_ids = self.workspace_manager.active().visual_ids.clone();
                    let cam = self.camera().clone();
                    self.interaction.handle_pointer_down(
                        x,
                        y,
                        &mut self.scene,
                        &cam,
                        self.spatial_mode,
                        false,
                        false,
                        false,
                        Some(ws_ids),
                    );
                    // Force translate even though no modifier
                    let cam = self.camera().clone();
                    self.interaction.force_translate(
                        x,
                        y,
                        &mut self.scene,
                        &cam,
                        self.spatial_mode,
                    );
                }
            }
        }
        let _ = self.display_handle.flush_clients();
    }

    /// Public entry point for pointer button release.
    pub fn handle_pointer_up(&mut self, x: f64, y: f64) {
        // G-E5.4: output-local conversion (identity single-output).
        let (_out, x, y) = self.resolve_pointer_output(x, y);
        self.event_serial = self.event_serial.wrapping_add(1);
        self.last_down_vid = None;
        // Finish a pointer resize before any content routing (I3b).
        if self.finish_resize_session() {
            self.schedule_render();
            return;
        }
        let has_active = self.interaction.is_dragging();
        let dragged_vid = if has_active {
            self.scene.selected_id
        } else {
            None
        };
        // G-B2: while a client DnD grab is active the release MUST
        // always reach the seat pointer — Smithay's DnDGrab ends the
        // drag there and negotiates the drop/cancel. The focus is the
        // SPATIAL pick at the release point (same path as motion):
        // route_to_content routes to the SELECTED visual — the drag
        // origin — which would drop onto the wrong surface. Focus None
        // over empty space is correct: DnDGrab sends
        // wl_data_device.leave and finishes with a cancelled
        // (unvalidated) drop on the source.
        if self.dnd_active {
            if let Some(ph) = self.pointer_handle.clone() {
                let serial = self.next_serial();
                let time = now_ms();
                let picked = self.pick_wayland_target(x, y);
                // BUG_LIST #18: `location = origin + pos` delivery (see
                // route_to_content) so the drop target sees the true
                // surface coordinate in spatial mode.
                let focus = picked.as_ref().and_then(|(vid, s, _)| {
                    self.surface_global_origin(*vid).map(|o| (s.clone(), o))
                });
                let location: smithay::utils::Point<f64, smithay::utils::Logical> =
                    match (&picked, &focus) {
                        (Some((_, _, pos)), Some((_, o))) => {
                            smithay::utils::Point::new(o.x + pos.x, o.y + pos.y)
                        }
                        _ => (x, y).into(),
                    };
                ph.motion(
                    self,
                    focus.clone(),
                    &MotionEvent {
                        location,
                        serial,
                        time,
                    },
                );
                ph.button(
                    self,
                    &ButtonEvent {
                        serial,
                        time,
                        button: 0x110,
                        state: smithay::backend::input::ButtonState::Released,
                    },
                );
                ph.frame(self);
                // #12: release serials also count as triggering input.
                if let Some((fs, _)) = focus.as_ref() {
                    self.record_input_serial(fs, serial);
                }
                if focus.is_none() {
                    self.last_wayland_focus = None;
                }
            }
            self.schedule_render();
            let _ = self.display_handle.flush_clients();
            return;
        }
        self.interaction.handle_pointer_up();
        if has_active {
            // J2 observability: after a drag ends, log where the dragged
            // window ended up and where its popup children now are in
            // world space (they follow via the scene graph parent chain).
            if let Some(vid) = dragged_vid {
                let dragging = self
                    .scene
                    .visuals
                    .iter()
                    .find(|v| v.id == vid)
                    .map(|v| (v.transform.position, v.transform.rotation));
                let popup_worlds: Vec<(VisualId, (f32, f32, f32))> = self
                    .scene
                    .visuals
                    .iter()
                    .filter(|v| v.parent == Some(vid))
                    .map(|v| {
                        let w = self.scene.world_matrix(v.id);
                        (v.id, (w[3][0], w[3][1], w[3][2]))
                    })
                    .collect();
                match dragging {
                    Some((pos, rot)) if !popup_worlds.is_empty() => {
                        info!(?vid, pos = ?pos, rot = ?rot,
                              popups = ?popup_worlds, "drag finished with popups attached");
                    }
                    Some((pos, _)) => info!(?vid, pos = ?pos, "drag finished"),
                    None => {}
                }
            }
        }
        if !has_active {
            self.route_to_content(PointerEventKind::Up, x, y);
        }
        let _ = self.display_handle.flush_clients();
    }

    /// Public entry point for pointer motion.
    /// G-E5.4: resolve a pointer position to its output. Input arrives
    /// in the PRESENTED framebuffer's pixel space; with N outputs that
    /// space is the global desktop plane tiled by `outputs`. Returns
    /// the output id and output-LOCAL coordinates (the coordinates all
    /// downstream picking/unprojection consume). With the single nested
    /// window the resolution is the identity (primary at (0,0)); the
    /// conversion point is explicit so multi-output input never silently
    /// assumes global == framebuffer (ARCHITECTURE §15).
    fn resolve_pointer_output(
        &self,
        x: f64,
        y: f64,
    ) -> (Option<crate::outputs::OutputId>, f64, f64) {
        match self.outputs.to_local(x as i32, y as i32) {
            Some((id, lx, ly)) => (Some(id), lx as f64, ly as f64),
            // Outside every output (possible mid-transition): clamp to
            // the primary and let existing edge handling apply.
            None => (
                self.outputs.primary_id(),
                x.clamp(0.0, (self.fb_size().0 - 1.0).max(0.0) as f64),
                y.clamp(0.0, (self.fb_size().1 - 1.0).max(0.0) as f64),
            ),
        }
    }

    pub fn handle_pointer_move(&mut self, x: f64, y: f64) {
        // G-E5.4: output-local conversion; deltas stay RAW (global) —
        // camera orbit/pan velocity must not change with output tiling.
        let (_out, lx, ly) = self.resolve_pointer_output(x, y);
        let dx = x - self.last_mouse.0;
        let dy = y - self.last_mouse.1;
        self.last_mouse = (x, y);

        // When pointer is locked, route relative motion to the locked client
        // and skip all spatial interaction.
        if self.pointer_constraints.pointer_locked {
            if let (Some(ph), Some(surface)) = (
                self.pointer_handle.clone(),
                self.pointer_constraints.locked_surface.clone(),
            ) {
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
                // Same focus-location convention as everywhere else:
                // the locked surface's global origin (below title bar).
                let origin = self
                    .wayland_surfaces
                    .iter()
                    .find(|(_, s)| **s == surface)
                    .and_then(|(vid, _)| self.surface_global_origin(*vid));
                ph.motion(self, origin.map(|o| (surface.clone(), o)), &mot_ev);
                ph.relative_motion(self, origin.map(|o| (surface, o)), &rel_ev);
            }
            return;
        }
        // R7: when confined, motion is routed to the confined surface
        // with the position CLAMPED into the confinement area (client
        // region ∩ surface content). Without enforcement the protocol
        // was accepted but had no effect.
        if let Some(surface) = self.pointer_constraints.confined_surface.clone() {
            if let Some(ph) = self.pointer_handle.clone() {
                let origin = self
                    .wayland_surfaces
                    .iter()
                    .find(|(_, s)| **s == surface)
                    .and_then(|(vid, _)| self.surface_global_origin(*vid));
                let vid = self
                    .wayland_surfaces
                    .iter()
                    .find(|(_, s)| **s == surface)
                    .map(|(vid, _)| *vid);
                let size = vid.and_then(|vid| {
                    self.scene
                        .visuals
                        .iter()
                        .find(|v| v.id == vid)
                        .map(|v| (v.total_width() as f64, v.total_height() as f64))
                });
                if let (Some(o), Some((cw, ch))) = (origin, size) {
                    let region_rects = self
                        .pointer_handle
                        .as_ref()
                        .and_then(|ph| {
                            smithay::wayland::pointer_constraints::with_pointer_constraint(
                                &surface,
                                ph,
                                |c| {
                                    c.and_then(|c| match &*c {
                                        smithay::wayland::pointer_constraints::PointerConstraint::Confined(confined) => {
                                            confined.region().map(|attrs| attrs.rects.clone())
                                        }
                                        _ => None,
                                    })
                                },
                            )
                        })
                        .unwrap_or_default();
                    let (cx, cy) = crate::pointer_constraints::constrain_to_region(
                        (x, y),
                        (o.x, o.y),
                        (cw, ch),
                        &region_rects,
                    );
                    let serial = self.next_serial();
                    let time = now_ms();
                    let mot_ev = MotionEvent {
                        location: smithay::utils::Point::new(cx, cy),
                        serial,
                        time,
                    };
                    let rel_ev = smithay::input::pointer::RelativeMotionEvent {
                        delta: (dx, dy).into(),
                        delta_unaccel: (dx, dy).into(),
                        utime: time as u64 * 1000,
                    };
                    ph.motion(self, Some((surface.clone(), o)), &mot_ev);
                    ph.relative_motion(self, Some((surface, o)), &rel_ev);
                }
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
        // Client DnD grab (G-B2): ALL motion feeds Smithay's DnDGrab
        // through the seat pointer. route_hover picks the target with
        // the SAME 3D spatial path as normal input (pick_wayland_target
        // → Scene.pick_visible), so a moved/rotated window's drag
        // target follows its world transform; camera state is not
        // involved. Compositor window manipulation stays suppressed.
        if self.dnd_active {
            self.route_hover(lx, ly);
            self.schedule_render();
            return;
        }
        // Pointer resize session (I3b): suppress drag/hover routing while
        // resizing so pointer focus stays on the resized surface.
        if self.resize_session.is_some() {
            self.update_resize_session(x, y);
            self.schedule_render();
            return;
        }
        self.interaction.window_size = self.fb_size();
        let was_dragging = self.interaction.is_dragging();
        let cam = self.camera().clone();
        self.interaction.handle_pointer_move(
            x,
            y,
            &mut self.scene,
            &cam,
            self.spatial_mode,
        );

        // Snap correction: if currently dragging, snap dragged visual to nearby edges
        if self.interaction.is_dragging() {
            if let Some(vid) = self.scene.selected_id {
                if let Some(visual) = self.scene.visuals.iter().find(|v| v.id == vid) {
                    let mpos = visual.transform.position;
                    let mw = visual.total_width();
                    let mh = visual.total_height();
                    // Build anchor list from non-selected, non-detached, same-workspace visuals only
                    let ws_ids = self.workspace_manager.active().visual_ids.as_slice();
                    let anchors: Vec<_> = self
                        .scene
                        .visuals
                        .iter()
                        .filter(|v| {
                            v.id != vid
                                && !self.scene.detached_set.contains(&v.id)
                                && ws_ids.contains(&v.id)
                        })
                        .map(|v| (v.transform.position, v.total_width(), v.total_height()))
                        .collect();
                    if let Some(snap) =
                        crate::snap::snap_position(mpos, mw, mh, &anchors, &Default::default())
                    {
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
                    if (x - self.press_pos.0).abs() > threshold
                        || (y - self.press_pos.1).abs() > threshold
                    {
                        let cam = self.camera().clone();
                        self.interaction.force_translate(
                            x,
                            y,
                            &mut self.scene,
                            &cam,
                            self.spatial_mode,
                        );
                    }
                }
            }
        }
        // If still not dragging after all checks, route hover events
        // (output-local coordinates — the picking space).
        if !was_dragging && !self.interaction.is_dragging() {
            self.route_hover(lx, ly);
        }
    }

    /// Global (screen-space) top-left of a visual's CLIENT surface —
    /// the location smithay 0.7 expects in the pointer-focus tuple:
    /// both wl_pointer and wl_data_device derive surface-local
    /// coordinates as `global_pointer - focus_location`. The origin is
    /// the content quad's top-left corner (below the title bar) under
    /// the full world transform, so moved/rotated/parented visuals
    /// report coordinates that match where they are RENDERED.
    fn surface_global_origin(
        &self,
        vid: VisualId,
    ) -> Option<smithay::utils::Point<f64, smithay::utils::Logical>> {
        let v = self.scene.visuals.iter().find(|v| v.id == vid)?;
        let t = self.scene.world_transform(vid);
        let gw = v.total_width();
        let gh = v.total_height();
        // R12: fraction from the visual's single conversion API.
        let title_frac = v.title_bar_fraction();
        let corner_local = cgmath::Vector3::new(
            v.transform.scale.x * (-gw / 2.0),
            v.transform.scale.y * (gh / 2.0 - gh * title_frac),
            0.0,
        );
        let corner_world = t.rotation * corner_local + t.position;
        let (w, h) = self.fb_size();
        Some(
            (
                (w / 2.0 + corner_world.x) as f64,
                (h / 2.0 - corner_world.y) as f64,
            )
                .into(),
        )
    }

    /// Pick the Wayland surface under the given screen position via 3D ray cast.
    /// Returns (visual id, surface, surface-local content position).
    /// Returns None when the cursor is over empty space, a title bar, or a
    /// non-Wayland visual.
    fn pick_wayland_target(
        &self,
        x: f64,
        y: f64,
    ) -> Option<(
        VisualId,
        WlSurface,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )> {
        let (w, h) = self.fb_size();
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let ndc_x = (x as f32 / w) * 2.0 - 1.0;
        let ndc_y = -((y as f32 / h) * 2.0 - 1.0);
        let pv = self.proj_view();

        let ws_visible: Vec<VisualId> = self
            .workspace_manager
            .active()
            .visual_ids
            .iter()
            .copied()
            .filter(|id| self.scene.is_visible(*id))
            .collect();
        let (vid, _) = self.scene.pick_visible(&pv, ndc_x, ndc_y, &ws_visible)?;
        if !self.scene.is_active(vid) {
            return None;
        }
        let wl_surface = self.wayland_surfaces.get(&vid).cloned()?;
        let v = self.scene.visuals.iter().find(|v| v.id == vid)?;
        let transform = v.transform.clone();
        let total_w = v.total_width();
        let total_h = v.total_height();
        let (u, uv) =
            input_router::screen_to_visual_uv(&pv, ndc_x, ndc_y, &transform, total_w, total_h)?;
        // R12: use the visual's own chrome height (was hardcoded
        // 0.06/1.06, wrong for custom title heights).
        let (_cu, content_v) = v.content_uv(u, uv);
        let title_frac = v.title_bar_fraction() as f64;
        if uv < title_frac {
            return None; // title bar — not content
        }
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
        // BUG_LIST #18: same delivery convention as route_to_content —
        // `location = origin + pos` so Smithay's subtraction yields the
        // unprojected surface coordinate (spatial-mode safe).
        let origin = self.surface_global_origin(vid);
        let location: smithay::utils::Point<f64, smithay::utils::Logical> = match origin {
            Some(o) => smithay::utils::Point::new(o.x + pos.x, o.y + pos.y),
            None => pos,
        };
        let mot_ev = MotionEvent {
            location,
            serial: self.next_serial(),
            time: now_ms(),
        };
        ph.motion(self, origin.map(|o| (wl_surface.clone(), o)), &mot_ev);
        // R7: constraints requested while unfocused activate now that
        // pointer focus has entered this surface.
        self.activate_constraints_for_focus(&wl_surface);
    }

    /// Center the camera on the currently selected visual.
    pub fn frame_selected(&mut self) -> bool {
        let Some(vid) = self.scene.selected_id else {
            return false;
        };
        let (cam, scene) = self.camera_and_scene();
        let result = cam.frame_visual(vid, scene);
        if result {
            info!(?vid, "camera framed on selected");
        }
        result
    }

    /// Frame all visuals in view.
    pub fn frame_all(&mut self) -> bool {
        let (cam, scene) = self.camera_and_scene();
        let result = cam.frame_all(scene);
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
                    CameraMode::Focus(_) => {
                        let (fm, cam, scene) = self.focus_exit_parts();
                        fm.exit(cam, scene)
                    }
                    _ => {
                        let (fm, cam) = self.focus_parts();
                        fm.exit_overview(cam)
                    }
                }
                info!("focus mode off");
            }
            CameraMode::Normal => {
                // Enter focus mode — save camera, target focused visual
                let Some(vid) = self.scene.focused_id else {
                    info!("no focused visual to focus on");
                    return;
                };
                let cam = self.camera().clone();
                self.focus_manager.enter(&cam, vid, &self.scene);
                info!(?vid, "focus mode on");
            }
        }
    }

    /// Enter overview mode: show all visuals in the active workspace.
    pub fn enter_overview(&mut self) {
        let ws = self.workspace_manager.active();
        if let Some(overview_cam) = crate::focus::overview_camera(&self.scene, &ws.visual_ids) {
            self.focus_manager
                .enter_overview(&self.camera().clone(), overview_cam);
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
        self.focus_manager
            .enter_workspace_overview(&self.camera().clone(), overview_cam);
        info!("workspace overview mode on");
    }

    /// Get the next serial number for input events.
    fn next_serial(&mut self) -> smithay::utils::Serial {
        self.event_serial = self.event_serial.wrapping_add(1);
        smithay::utils::Serial::from(self.event_serial)
    }

    /// #12: record an input serial against the owning client (pointer
    /// button or keyboard event delivery). Popup grab requests validate
    /// against this ledger.
    fn record_input_serial(&mut self, surface: &WlSurface, serial: smithay::utils::Serial) {
        if let Some(client) = surface.client() {
            let mut ledger = INPUT_SERIAL_LEDGER.lock().unwrap();
            ledger_push(ledger.entry(client.id()).or_default(), u32::from(serial));
        }
    }

    /// #12: validate an xdg_popup grab serial — it must name a recent
    /// input event delivered to the grabbing client. Serials from the
    /// future or from other clients (garbage, replayed, guessed) are
    /// rejected: the popup is dismissed with popup_done per the
    /// toolkit-visible consequence of a failed grab.
    fn validate_popup_grab(
        &mut self,
        surface: &smithay::wayland::shell::xdg::PopupSurface,
        serial: Serial,
    ) -> bool {
        let wl = surface.wl_surface().clone();
        let client_id = wl.client().map(|c| c.id());
        let valid = client_id
            .map(|c| {
                ledger_contains(
                    INPUT_SERIAL_LEDGER.lock().unwrap().get(&c),
                    u32::from(serial),
                )
            })
            .unwrap_or(false);
        if !valid {
            warn!(
                ?serial,
                "popup grab rejected: serial not a recent input event of this client"
            );
            surface.send_popup_done();
            self.popups
                .retain(|p| p.popup.wl_surface() != surface.wl_surface());
        }
        valid
    }

    /// Orbit camera (right-drag).
    pub fn handle_orbit(&mut self, dx: f64, dy: f64) {
        self.camera_mut().handle_orbit(dx, dy);
    }

    /// Pan camera (middle-drag).
    pub fn handle_pan(&mut self, dx: f64, dy: f64) {
        self.camera_mut().handle_pan(dx, dy, 0.05);
    }

    /// Zoom camera (scroll).
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn handle_zoom(&mut self, delta: f64) {
        self.camera_mut().handle_zoom(delta);
    }

    /// Handle a pointer axis (scroll) event at the given screen position.
    ///
    /// Scroll is routed to the Wayland surface under the cursor (terminals,
    /// browsers scroll their content). Only when no client surface is under
    /// the cursor does the camera zoom (the pre-existing global behavior).
    pub fn handle_axis(&mut self, x: f64, y: f64, dx: f64, dy: f64) {
        // G-E5.4: output-local conversion for the pick target.
        let (_out, x, y) = self.resolve_pointer_output(x, y);
        let Some(ph) = self.pointer_handle.clone() else {
            self.camera_mut().handle_zoom(dy);
            return;
        };

        if let Some((vid, wl_surface, pos)) = self.pick_wayland_target(x, y) {
            // Ensure pointer focus is on the target surface before axis events.
            self.last_wayland_focus = Some(wl_surface.clone());
            // BUG_LIST #18: deliver the unprojected surface coordinate
            // (see route_to_content for the convention).
            let origin = self.surface_global_origin(vid);
            let location: smithay::utils::Point<f64, smithay::utils::Logical> = match origin {
                Some(o) => smithay::utils::Point::new(o.x + pos.x, o.y + pos.y),
                None => pos,
            };
            let mot_ev = MotionEvent {
                location,
                serial: self.next_serial(),
                time: now_ms(),
            };
            ph.motion(self, origin.map(|o| (wl_surface, o)), &mot_ev);

            let time = now_ms();
            let frame = smithay::input::pointer::AxisFrame::new(time)
                .source(smithay::backend::input::AxisSource::Wheel)
                .value(smithay::backend::input::Axis::Horizontal, dx)
                .value(smithay::backend::input::Axis::Vertical, dy);
            ph.axis(self, frame);
            ph.frame(self);
        } else {
            if dx.abs() > dy.abs() {
                self.camera_mut().handle_zoom(dx);
            } else {
                self.camera_mut().handle_zoom(dy);
            }
        }
    }

    /// Save camera bookmark.
    pub fn save_bookmark(&mut self, slot: usize) {
        self.camera_mut().save_bookmark(slot);
        info!(slot, "camera bookmark saved");
    }

    /// Restore camera bookmark.
    pub fn restore_bookmark(&mut self, slot: usize) -> bool {
        let result = self.camera_mut().restore_bookmark(slot);
        if result {
            info!(slot, "camera bookmark restored");
        }
        result
    }

    /// Convenience: get the layout mode from the active workspace.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn layout_mode(&self) -> layout::LayoutMode {
        self.workspace_manager.active().layout_mode
    }

    /// Switch to a workspace by ID.
    /// Saves the current workspace state and restores the target workspace state.
    /// Uses set_keyboard_focus() to ensure Wayland keyboard focus stays in sync.
    /// Returns true if the switch occurred.
    pub fn switch_workspace(&mut self, idx: usize) -> bool {
        // Unlock pointer on workspace switch (R7: deactivates the
        // protocol object too, and also releases confinement).
        self.unlock_pointer();
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
            let cam = self.camera().clone();
            let ws = self.workspace_manager.active_mut();
            ws.camera = cam;
            ws.focused_id = self.scene.focused_id;
            ws.detached_set = self.scene.detached_set.clone();
            ws.focus_manager_state = self.focus_manager.clone();
        }
        if !self.workspace_manager.switch(idx, &mut self.scene) {
            return false;
        }
        // Sync camera, layout, focus from saved workspace state.
        // G-E5.3: capture everything the mutable camera write would
        // overlap with BEFORE taking the mutable borrow (the camera
        // lives in the output registry now).
        let ws_snap = {
            let ws = self.workspace_manager.active();
            (
                ws.camera.clone(),
                ws.detached_set.clone(),
                ws.focus_manager_state.clone(),
                ws.focused_id,
                ws.visual_ids.clone(),
            )
        };
        let (ws_camera, ws_detached, ws_fm, saved, ws_visual_ids) = ws_snap;
        *self.camera_mut() = ws_camera;
        self.scene.detached_set = ws_detached;
        // Sync focus manager state
        self.focus_manager = ws_fm;
        // Reset camera mode on workspace switch (each workspace has its own view)
        self.focus_manager.camera_mode = CameraMode::Normal;
        self.focus_manager.transition = None;
        self.focus_manager.saved_camera = None;
        // Use authoritative focus setter for Wayland keyboard focus sync.
        // J1: a saved focus that is no longer focusable (minimized while
        // the workspace was inactive, destroyed, etc.) is replaced by
        // the most recent MRU entry that belongs to the target
        // workspace — the focused visual must always be live and visible.
        // `saved` comes from the workspace snapshot captured above.
        let saved_ok = saved
            .map(|vid| self.workspace_manager.active().contains(vid) && self.scene.is_visible(vid))
            .unwrap_or(false)
            && saved.map(|vid| !self.is_minimized(vid)).unwrap_or(false);
        let focus_target = if saved_ok {
            saved
        } else {
            let this: &LookingGlass = self;
            let fallback_ws = ws_visual_ids.clone();
            this.focus_history
                .next_after(None, &move |v| {
                    fallback_ws.contains(&v) && this.scene.is_visible(v)
                })
                .or_else(|| ws_visual_ids.first().copied())
        };
        self.set_keyboard_focus(focus_target);
        info!(workspace = idx, old = old_id, restored = ?focus_target, "switched workspace");
        crate::debug_journal::event(
            "workspace",
            &[("to", idx.to_string()), ("from", old_id.to_string())],
        );
        self.debug_snapshot();
        // J4 follow-up: a workspace whose saved camera cannot show its
        // row (e.g. a window moved here from another workspace) must
        // still be readable after the switch.
        self.auto_fit_camera();
        true
    }

    #[allow(dead_code)] // reserved API surface; not yet wired
    /// Create a new workspace and return its ID.
    pub fn create_workspace(&mut self) -> usize {
        let id = self.workspace_manager.add();
        info!(workspace = id, "workspace created");
        id
    }

    /// Destroy a workspace by ID.
    /// Fails if it's the last workspace.
    /// Wayland surfaces survive — only their workspace membership is cleaned up.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn destroy_workspace(&mut self, id: usize) -> Result<(), String> {
        if self.workspace_manager.len() <= 1 {
            return Err("cannot destroy the last workspace".into());
        }
        // If destroying the active workspace, save current state and switch to 0 first
        if id == self.workspace_manager.active_id() {
            {
                let cam = self.camera().clone();
                let focused = self.scene.focused_id;
                let detached = self.scene.detached_set.clone();
                let fm_state = self.focus_manager.clone();
                let ws = self.workspace_manager.active_mut();
                ws.camera = cam;
                ws.focused_id = focused;
                ws.detached_set = detached;
                ws.focus_manager_state = fm_state;
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
    #[allow(dead_code)] // reserved API surface; not yet wired
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
        // BUG_LIST #16: duplicated XTEST input has been observed to
        // deliver the SAME modifier press twice (e.g. the maximize
        // sequences: keydown super → key Up → keyup super leaves one
        // unmatched down). Modifiers never auto-repeat, so a press
        // while the modifier is already held is a duplicate — ignore
        // it, and ignore a release of a modifier that is not held.
        // Non-modifier keys are untouched (their releases MUST pass).
        {
            let dup = match linux_key {
                keys::CTRL_L | keys::CTRL_R => self.ctrl_pressed == pressed,
                keys::SHIFT_L | keys::SHIFT_R => self.shift_pressed == pressed,
                keys::ALT_L | keys::ALT_R => self.alt_pressed == pressed,
                keys::META_L | keys::META_R => self.meta_pressed == pressed,
                _ => false,
            };
            if dup {
                tracing::debug!(?linux_key, pressed, "duplicate modifier event ignored");
                return;
            }
        }
        match linux_key {
            keys::CTRL_L | keys::CTRL_R => {
                self.ctrl_pressed = pressed;
            }
            keys::SHIFT_L | keys::SHIFT_R => {
                self.shift_pressed = pressed;
            }
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
            self.alt_tab_active = true;
            if self.shift_pressed {
                self.switch_app_focus(false);
            } else {
                self.switch_app_focus(true);
            }
            return;
        }

        tracing::debug!(
            ?linux_key,
            pressed,
            ctrl = self.ctrl_pressed,
            shift = self.shift_pressed,
            alt = self.alt_pressed,
            meta = self.meta_pressed,
            "KEY EVENT"
        );

        // BUG_LIST #16: modifier state is SEAT state, not surface state.
        // A modifier press fed to smithay while a surface was focused
        // must have its release reach smithay even when focus vanished
        // in between (window closed, workspace switch, drag start) —
        // otherwise smithay's XKB state latches the modifier and every
        // subsequent client enter reports it stuck (observed:
        // logo:true for the rest of the session). Modifiers therefore
        // always go through the keyboard handle; only non-modifier
        // keys require a focused visual (route_keyboard below).
        if matches!(
            linux_key,
            keys::CTRL_L
                | keys::CTRL_R
                | keys::SHIFT_L
                | keys::SHIFT_R
                | keys::ALT_L
                | keys::ALT_R
                | keys::META_L
                | keys::META_R
        ) {
            self.feed_keyboard_event(linux_key, pressed);
            return;
        }

        // If context menu is visible, route keyboard navigation to it
        if self.context_menu.visible && pressed {
            use crate::keys;
            match linux_key {
                keys::UP => {
                    self.context_menu.select_prev();
                    self.swallow_release = Some(linux_key);
                    return;
                }
                keys::DOWN => {
                    self.context_menu.select_next();
                    self.swallow_release = Some(linux_key);
                    return;
                }
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
                keys::F1 => {
                    self.activate_workspace(0);
                    return;
                }
                keys::F2 => {
                    self.activate_workspace(1);
                    return;
                }
                keys::F3 => {
                    self.activate_workspace(2);
                    return;
                }
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
            self.camera_mut().handle_key(linux_key, pressed, 1.0);
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
                    self.spatial_cam_pose =
                        Some((self.camera().position, self.camera().yaw, self.camera().pitch));
                    self.spatial_mode = false;
                } else {
                    // Re-entering spatial: restore the saved pose so the
                    // desktop looks exactly like before the toggle.
                    self.spatial_mode = true;
                    if let Some((pos, yaw, pitch)) = self.spatial_cam_pose.take() {
                        self.camera_mut().position = pos;
                        self.camera_mut().yaw = yaw;
                        self.camera_mut().pitch = pitch;
                    } else if !self.spatial_cam_adapted {
                        // First spatial entry through the toggle: fit the
                        // frustum to the workspace view.
                        let d = (self.fb_size().1 * 1.2071f32).max(600.0);
                        self.camera_mut().position = cgmath::Point3::new(0.0, 0.0, d);
                        self.camera_mut().yaw = 0.0;
                        self.camera_mut().pitch = 0.0;
                    }
                    self.spatial_cam_adapted = true;
                }
                tracing::info!(spatial_mode = self.spatial_mode, "spatial mode toggled");
            }
            ToggleFocus => {
                self.toggle_focus_mode();
            }
            ToggleOverview => match self.focus_manager.camera_mode {
                CameraMode::Overview | CameraMode::WorkspaceOverview => {
                    let (fm, cam) = self.focus_parts();
                    fm.exit_overview(cam);
                    info!("overview mode off");
                }
                _ => {
                    self.enter_overview();
                }
            },
            ToggleWorkspaceOverview => match self.focus_manager.camera_mode {
                CameraMode::WorkspaceOverview => {
                    let (fm, cam) = self.focus_parts();
                    fm.exit_overview(cam);
                    info!("workspace overview off");
                }
                _ => {
                    self.enter_workspace_overview();
                }
            },
            WorkspaceNext => {
                self.next_workspace();
            }
            WorkspacePrev => {
                self.previous_workspace();
            }
            AppNext => {
                // J1: shell app switch over the MRU (Alt+Tab keyboard
                // path is handled inline above with held-modifier state).
                self.switch_app_focus(true);
            }
            AppPrev => {
                self.switch_app_focus(false);
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
            ToggleFullscreen => {
                self.toggle_fullscreen_selected(crate::fullscreen::FullscreenSource::Compositor);
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
        // If a pointer constraint is active, release it first (R7:
        // Escape must deactivate the protocol object — locked OR
        // confined).
        if self.pointer_constraints.pointer_locked
            || self.pointer_constraints.confined_surface.is_some()
        {
            self.unlock_pointer();
            return;
        }
        // An active client DnD grab is cancelled first: the spatial
        // escape chain below knows nothing about protocol drags.
        if self.dnd_active {
            self.cancel_dnd_grab();
            return;
        }

        use crate::focus::CameraMode;
        let in_workspace_overview = matches!(
            self.focus_manager.camera_mode,
            CameraMode::WorkspaceOverview
        );
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
                let (fm, cam) = self.focus_parts();
                    fm.exit_overview(cam);
            }
            EscapeAction::ExitOverview => {
                let (fm, cam) = self.focus_parts();
                    fm.exit_overview(cam);
            }
            EscapeAction::ExitFocus => {
                let (fm, cam, scene) = self.focus_exit_parts();
                fm.exit(cam, scene);
            }
            EscapeAction::ResetCamera => {
                self.reset_camera();
            }
        }
    }

    /// Open context menu on the focused visual (triggered by Menu key).
    pub fn open_context_menu_on_focused(&mut self) {
        if let Some(vid) = self.scene.focused_id {
            let (fw, fh) = self.fb_size();
            let (x, y) = (fw as f64 * 0.5, fh as f64 * 0.5);
            let ws_count = self.workspace_manager.len();
            self.context_menu.show(x, y, vid, ws_count);
            self.context_menu.set_maximize_label(self.is_maximized(vid));
            let m = crate::context_menu::MenuMetrics::for_framebuffer(
                self.fb_size().0,
                self.fb_size().1,
            );
            info!(
                menu_width = m.menu_width,
                item_height = m.item_height,
                glyph_scale = m.glyph_scale,
                fb_w = self.fb_size().0,
                fb_h = self.fb_size().1,
                "context menu metrics"
            );
            info!(?vid, "context menu opened via keyboard");
        }
    }

    /// Close the focused application.
    pub fn close_focused_app(&mut self) {
        let Some(vid) = self.scene.focused_id else {
            return;
        };
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
        let match_idx =
            self.launcher.applications.iter().position(|e| {
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
    /// J1: switch application focus along the MRU history. Forward
    /// = Alt+Tab (next most recent), back = Alt+Shift+Tab. Only this
    /// workspace's live, non-minimized windows participate; popups are
    /// never in the history.
    pub fn switch_app_focus(&mut self, forward: bool) -> bool {
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        let this: &LookingGlass = self;
        let current = this.scene.focused_id;
        let candidate = if forward {
            this.focus_history
                .next_after(current, &move |v| ws_ids.contains(&v))
        } else {
            this.focus_history
                .previous_before(current, &move |v| ws_ids.contains(&v))
        };
        match candidate {
            Some(next) => {
                info!(?next, forward, "app switch (MRU)");
                self.set_keyboard_focus(Some(next));
                self.scene.select(Some(next));
                true
            }
            None => {
                info!(forward, "app switch: no other focusable window");
                false
            }
        }
    }

    pub fn cycle_visuals(&mut self) -> bool {
        // J1: window cycling now follows the MRU history (not stacking
        // order): the same coherence rule Alt+Tab uses.
        if self.switch_app_focus(true) {
            return true;
        }
        // Empty history (e.g. fresh session with windows never focused):
        // fall back to the first window of the workspace.
        let ws_ids = self.workspace_manager.active().visual_ids.clone();
        if ws_ids.is_empty() {
            return false;
        }
        let next = ws_ids[0];
        self.set_keyboard_focus(Some(next));
        self.scene.select(Some(next));
        info!(?next, "cycled to visual");
        true
    }

    /// Reset the camera to its default position.
    pub fn reset_camera(&mut self) {
        self.camera_mut().position = cgmath::Point3::new(0.0, 0.0, 800.0);
        self.camera_mut().yaw = 0.0;
        self.camera_mut().pitch = 0.0;
        info!("camera reset");
    }

    /// Recover from destroyed focus — if the focused visual no longer exists,
    /// clear focus state cleanly.
    #[allow(dead_code)] // reserved API surface; not yet wired
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
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn cancel_interaction(&mut self) {
        if self.interaction.is_dragging() {
            self.interaction.handle_pointer_up();
            info!("interaction cancelled");
        }
        self.cancel_dnd_grab();
    }

    /// Abort an in-progress client DnD grab (G-B2): unset the seat
    /// pointer grab so Smithay's DnDGrab runs its unset path (cancel
    /// on the source, leave on the target, offer cleanup). No stale
    /// drag state may survive a cancel, client death, or recovery.
    pub fn cancel_dnd_grab(&mut self) {
        if !self.dnd_active {
            return;
        }
        if let Some(ph) = self.pointer_handle.clone() {
            if let Some(serial) = ph.with_grab(|s, _| s) {
                ph.unset_grab(self, serial, now_ms());
            }
        }
        self.dnd_active = false;
        info!("dnd: grab cancelled");
        self.schedule_render();
    }

    /// Run full recovery: cancel interaction → exit focus → exit overview → reset camera.
    #[allow(dead_code)] // reserved API surface; not yet wired
    pub fn recover(&mut self) {
        use crate::focus::CameraMode;
        info!("full recovery sequence");

        self.cancel_interaction();

        if matches!(self.focus_manager.camera_mode, CameraMode::Focus(_)) {
            let (fm, cam, scene) = self.focus_exit_parts();
                fm.exit(cam, scene);
            info!("recovery: exited focus mode");
        }

        if matches!(self.focus_manager.camera_mode, CameraMode::Overview)
            || matches!(
                self.focus_manager.camera_mode,
                CameraMode::WorkspaceOverview
            )
        {
            let (fm, cam) = self.focus_parts();
                    fm.exit_overview(cam);
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

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // G-C4: the XWayland server client carries smithay's own
        // XWaylandClientData instead of our ClientState — both provide
        // a CompositorClientState.
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        let data: &'a smithay::xwayland::XWaylandClientData = client
            .get_data::<smithay::xwayland::XWaylandClientData>()
            .expect("client data is neither ClientState nor XWaylandClientData");
        &data.compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.handle_commit(surface);
    }
}

delegate_compositor!(LookingGlass);

impl FractionalScaleHandler for LookingGlass {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // The preferred scale is what the output advertises; surfaces
        // created before the scale changed already got it via
        // new_fractional_scale on bind. Single output: constant 1.0.
        with_states(&surface, |states| {
            smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(self.preferred_scale);
            });
        });
    }
}

delegate_fractional_scale!(LookingGlass);
delegate_viewporter!(LookingGlass);

// #6: IME/text-input protocol delegation. The grab machinery (keymap,
// repeat info, key forwarding) lives entirely in smithay's
// InputMethodManagerState; veyra only supplies popup lifecycle +
// parent geometry.
delegate_text_input_manager!(LookingGlass);
delegate_input_method_manager!(LookingGlass);

impl smithay::wayland::input_method::InputMethodHandler for LookingGlass {
    fn new_popup(&mut self, surface: smithay::wayland::input_method::PopupSurface) {
        info!("ime popup surface created");
        let wl = surface.wl_surface().clone();
        self.ime_popups.push(surface);
        // The IME may have committed its popup surface before this
        // point (commit -> role detected -> no PopupSurface yet).
        self.handle_ime_popup_commit(&wl);
    }

    fn dismiss_popup(&mut self, surface: smithay::wayland::input_method::PopupSurface) {
        // smithay dismisses + re-adds the popup on parent changes; the
        // old visual must go with the old surface instance.
        self.remove_ime_popup_visual(surface.wl_surface());
        self.ime_popups
            .retain(|p| p.wl_surface() != surface.wl_surface());
    }

    fn popup_repositioned(&mut self, _surface: smithay::wayland::input_method::PopupSurface) {}

    fn parent_geometry(
        &self,
        parent: &WlSurface,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        // The IME popup anchors to the focused text field's window.
        // Cache the anchor visual for the render path (interior
        // mutability: this callback receives &self only).
        if let Some(vid) = self.find_vid_for_surface(parent) {
            self.ime_parent_vid.set(Some(vid));
        }
        self.find_vid_for_surface(parent)
            .and_then(|vid| self.scene.get(vid))
            .map(|v| {
                let geo = &v.geometry;
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::new(geo.loc.x, geo.loc.y),
                    geo.size,
                )
            })
            .unwrap_or_else(|| {
                smithay::utils::Rectangle::new(
                    smithay::utils::Point::new(0, 0),
                    smithay::utils::Size::new(1280, 800),
                )
            })
    }
}

/// G-C3: pure math behind the commit-path geometry — logical surface
/// size and the normalized viewport src rect for a committed buffer.
///
/// Precedence: wp_viewporter destination size, then wp_viewporter
/// source-rect size (rounded per protocol), then the buffer dimensions
/// divided by the integer wl_surface buffer scale.
fn logical_geometry_from_buffer(
    buf: (i32, i32),
    buffer_scale: i32,
    viewport_dst: Option<(i32, i32)>,
    viewport_src: Option<((f64, f64), (f64, f64))>,
) -> ((i32, i32), Option<[f32; 4]>) {
    let scale = buffer_scale.max(1);
    let logical = viewport_dst.unwrap_or_else(|| match viewport_src {
        Some((_, size)) => (size.0.round() as i32, size.1.round() as i32),
        None => (buf.0 / scale, buf.1 / scale),
    });
    let src_uv = viewport_src.map(|(loc, size)| {
        [
            (loc.0 / buf.0.max(1) as f64) as f32,
            (loc.1 / buf.1.max(1) as f64) as f32,
            (size.0 / buf.0.max(1) as f64) as f32,
            (size.1 / buf.1.max(1) as f64) as f32,
        ]
    });
    (logical, src_uv)
}

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

        let info = PopupInfo {
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
        surface: smithay::wayland::shell::xdg::PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        serial: Serial,
    ) {
        // #12: the spec requires the serial of the input event that
        // triggered the popup. Validated against the client's recent
        // input-serial ledger; a rejected grab dismisses the popup.
        if self.validate_popup_grab(&surface, serial) {
            info!(?serial, "popup grab accepted (serial validated)");
        }
    }

    fn reposition_request(
        &mut self,
        surface: smithay::wayland::shell::xdg::PopupSurface,
        positioner: PositionerState,
        _token: u32,
    ) {
        // Update stored positioner state for this popup
        if let Some(info) = self
            .popups
            .iter_mut()
            .find(|p| p.popup.wl_surface() == surface.wl_surface())
        {
            info.positioner = positioner;
        }
        // Accept reposition requests by sending a configure
        let _ = surface.send_configure();
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        // Client-initiated fullscreen (I7): same coordinator as the
        // compositor key/menu path. The Fullscreen state bit + size are
        // sent by begin_fullscreen, not here.
        let vid = self
            .find_toplevel(surface.wl_surface())
            .and_then(|t| t.visual_id);
        match vid {
            Some(vid) => self.begin_fullscreen(vid, crate::fullscreen::FullscreenSource::Client),
            None => {
                info!("fullscreen request before the surface mapped; ignored");
            }
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let vid = self
            .find_toplevel(surface.wl_surface())
            .and_then(|t| t.visual_id);
        match vid {
            Some(vid) => self.begin_unfullscreen(vid, crate::fullscreen::FullscreenSource::Client),
            None => {
                info!("unfullscreen request for unknown toplevel ignored");
            }
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        // Client-initiated minimize (xdg_toplevel.set_minimized): same
        // compositor-side flow as Meta+Down — hide the visual, keep the
        // surface mapped, focus the next window. No state bit exists for
        // minimized (xdg-shell), so no configure is sent.
        let vid = self
            .find_toplevel(surface.wl_surface())
            .and_then(|t| t.visual_id);
        if let Some(vid) = vid {
            self.begin_minimize(vid, crate::maximize::MinimizeSource::Client);
        } else {
            info!("minimize request for unknown toplevel ignored");
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let vid = self
            .find_toplevel(surface.wl_surface())
            .and_then(|t| t.visual_id);
        match vid {
            Some(vid) => self.begin_maximize(vid, crate::maximize::MaximizeSource::Client),
            None => {
                info!("maximize request before the surface mapped; ignored");
            }
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let vid = self
            .find_toplevel(surface.wl_surface())
            .and_then(|t| t.visual_id);
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
            let vid = self
                .toplevels
                .iter()
                .find(|t| t.toplevel.wl_surface() == &surface)
                .and_then(|t| t.visual_id);
            if let Some(vid) = vid {
                self.flush_resize_desired(vid);
                // I4: the freed surface may now take a deferred maximize.
                self.flush_deferred_maximize();
                // I7: ...or a deferred fullscreen request.
                self.flush_fullscreen_deferred();
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
                visual.chrome.title = title.clone();
            }
            // G-D3: keep foreign-toplevel clients in step.
            let app_id = self
                .scene
                .get(vid)
                .map(|v| v.chrome.app_id.clone())
                .unwrap_or_default();
            self.update_foreign_toplevel(vid, &title, &app_id);
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
            // G-D3: keep foreign-toplevel clients in step.
            let title = self
                .scene
                .get(vid)
                .map(|v| v.chrome.title.clone())
                .unwrap_or_default();
            self.update_foreign_toplevel(vid, &title, &app_id);
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
            // R7: a destroyed surface must not leave stale constraint
            // state — release and deactivate anything it owned.
            if self.pointer_constraints.locked_surface.as_ref() == Some(wl_surface)
                || self.pointer_constraints.confined_surface.as_ref() == Some(wl_surface)
            {
                self.unlock_pointer();
            }
            // G-B2: if the dying surface participates in the active DnD
            // (drag origin or current target), abort the grab so no
            // stale drag state outlives the client.
            if self.dnd_active {
                let dying = info.wl_surface.clone();
                let mut participates = false;
                if let Some(ph) = self.pointer_handle.as_ref() {
                    if let Some(sd) = ph.grab_start_data() {
                        if let Some((f, _)) = sd.focus {
                            participates |= f == dying;
                        }
                    }
                    if let Some(f) = ph.current_focus() {
                        participates |= f == dying;
                    }
                }
                if participates {
                    self.cancel_dnd_grab();
                }
            }
            if let Some(vid) = info.visual_id {
                let was_focused =
                    self.scene.focused_id == Some(vid) || self.scene.selected_id == Some(vid);
                // Drop any outstanding geometry request for the dead surface (I3a).
                self.client_resizes.abort(vid);
                // Drop any outstanding maximize intent for the dead surface (I4).
                self.maximize.abort(vid);
                // Record a tombstone before cleanup so the window can be
                // reopened with its transform and workspace (I1).
                let transform = self.scene.get_mut(vid).map(|v| v.transform.clone());
                if let Some(transform) = transform {
                    let ws_idx = self
                        .workspace_for_visual(vid)
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
                    self.refocus_after_close(Some(vid));
                }
            }
            info!(
                app_id = %info.app_id,
                title = %info.title,
                "surface destroyed"
            );
        }
    }

    fn popup_destroyed(&mut self, surface: smithay::wayland::shell::xdg::PopupSurface) {
        let wl_surface = surface.wl_surface();
        let Some(idx) = self.popups.iter().position(|p| p.wl_surface == *wl_surface) else {
            return;
        };
        let info = self.popups.remove(idx);
        if let Some(vid) = info.visual_id {
            remove_popup_visual(self, vid);
            info!(?vid, "popup destroyed, visual dropped");
        } else {
            info!("popup destroyed before mapping");
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
    let popup_ids: Vec<VisualId> = state
        .popups
        .iter()
        .filter(|p| p.visual_id == Some(vid) || p.parent_toplevel_vid == Some(vid))
        .filter_map(|p| p.visual_id)
        .collect();
    // Remove their visuals from the scene / workspace bookkeeping
    for pvid in popup_ids {
        remove_popup_visual(state, pvid);
    }
    // Remove from tracking list
    state
        .popups
        .retain(|p| p.visual_id != Some(vid) && p.parent_toplevel_vid != Some(vid));
}

/// Drop a popup's presentation state (visual, surface map, workspace
/// membership, selection). The popup's scene visual has workspace-local
/// lifetime while the Wayland surface has compositor-global lifetime —
/// both must be released together. Used by both destroy paths: parent
/// toplevel destruction and the client destroying the popup itself
/// (audit fix: zombie popups kept visuals alive forever).
fn remove_popup_visual(state: &mut LookingGlass, pvid: VisualId) {
    state.scene.remove(pvid);
    state.wayland_surfaces.remove(&pvid);
    state.input_sinks.remove(&pvid);
    for i in 0..state.workspace_manager.len() {
        if let Some(ws) = state.workspace_manager.get_mut(i) {
            ws.remove(pvid);
        }
    }
    if state.interaction.is_dragging_visual(pvid) {
        state.interaction.handle_pointer_up();
    }
}

/// Clean up a visual from ALL workspaces, focus, interaction, snap, and scene state.
/// This is used by the XdgShellHandler when a toplevel is destroyed.
fn cleanup_visual_permanently(state: &mut LookingGlass, vid: VisualId) {
    // Clean up child popups first
    cleanup_popups_by_vid(state, vid);
    // #11: child subsurfaces die with the parent
    if let Some(surface) = state.wayland_surfaces.get(&vid).cloned() {
        state.remove_subsurfaces_of(&surface);
    }
    // G-D3: withdraw from foreign-toplevel clients
    state.unregister_foreign_toplevel(vid);
    // Drop any outstanding client geometry request (I3a)
    state.client_resizes.abort(vid);
    // Drop any outstanding maximize intent (I4)
    state.maximize.abort(vid);
    // Drop any outstanding fullscreen intent (I7)
    state.fullscreen.abort(vid);
    // J1: dead windows leave the application focus history.
    state.focus_history.remove(vid);
    // Drop a resize session targeting the removed visual (I3b)
    if state.resize_session.as_ref().is_some_and(|s| s.vid == vid) {
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
        let cam = state.camera_mut();
        std::mem::swap(&mut saved, cam);
        state.focus_manager.exit(&mut saved, &state.scene);
        let cam = state.camera_mut();
        std::mem::swap(&mut saved, cam);
    }
    // Clean up overview if focused on that visual
    if matches!(state.focus_manager.camera_mode, CameraMode::Focus(t) if t == vid) {
        state.focus_manager.camera_mode = CameraMode::Normal;
        state.focus_manager.transition = None;
    }
    // Remove from all groups
    state.scene.remove_from_all_groups(vid);
}

/// BUG_LIST #19: keyboard focus target that can be either a native
/// Wayland surface or an X11 surface. Focusing an `X11Surface` goes
/// through smithay's `KeyboardTarget<..> for X11Surface` impl, which
/// performs the ICCCM input-focus dance (SetInputFocus / WM_TAKE_FOCUS
/// per the window's input mode) — focusing its raw wl_surface instead
/// delivers wl_keyboard events that XWayland drops because the X-side
/// input focus is never moved (X focus stays on None; X11 apps receive
/// no key events at all).
#[derive(Clone, PartialEq, Debug)]
pub enum KeyboardFocusTarget {
    Wl(WlSurface),
    X11(smithay::xwayland::xwm::X11Surface),
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            KeyboardFocusTarget::Wl(s) => s.alive(),
            KeyboardFocusTarget::X11(x) => x.alive(),
        }
    }
}

impl smithay::wayland::seat::WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<std::borrow::Cow<'_, WlSurface>> {
        match self {
            KeyboardFocusTarget::Wl(s) => s.wl_surface(),
            KeyboardFocusTarget::X11(x) => smithay::wayland::seat::WaylandFocus::wl_surface(x),
        }
    }
}

impl KeyboardTarget<LookingGlass> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<LookingGlass>,
        data: &mut LookingGlass,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            KeyboardFocusTarget::Wl(s) => {
                KeyboardTarget::<LookingGlass>::enter(s, seat, data, keys, serial)
            }
            KeyboardFocusTarget::X11(x) => {
                KeyboardTarget::<LookingGlass>::enter(x, seat, data, keys, serial)
            }
        }
    }

    fn leave(&self, seat: &Seat<LookingGlass>, data: &mut LookingGlass, serial: Serial) {
        match self {
            KeyboardFocusTarget::Wl(s) => {
                KeyboardTarget::<LookingGlass>::leave(s, seat, data, serial)
            }
            KeyboardFocusTarget::X11(x) => {
                KeyboardTarget::<LookingGlass>::leave(x, seat, data, serial)
            }
        }
    }

    fn key(
        &self,
        seat: &Seat<LookingGlass>,
        data: &mut LookingGlass,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            KeyboardFocusTarget::Wl(s) => {
                KeyboardTarget::<LookingGlass>::key(s, seat, data, key, state, serial, time)
            }
            KeyboardFocusTarget::X11(x) => {
                KeyboardTarget::<LookingGlass>::key(x, seat, data, key, state, serial, time)
            }
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<LookingGlass>,
        data: &mut LookingGlass,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            KeyboardFocusTarget::Wl(s) => {
                KeyboardTarget::<LookingGlass>::modifiers(s, seat, data, modifiers, serial)
            }
            KeyboardFocusTarget::X11(x) => {
                KeyboardTarget::<LookingGlass>::modifiers(x, seat, data, modifiers, serial)
            }
        }
    }
}

impl SeatHandler for LookingGlass {
    type KeyboardFocus = KeyboardFocusTarget;
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
    type SelectionUserData = SelectionOwner;
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<smithay::wayland::selection::SelectionSource>,
        _seat: Seat<Self>,
    ) {
        // G-B1: smithay's wl_data_device.set_selection arm already
        // stores the CLIENT source in the seat data (device.rs:144) —
        // wrapping the mime types into a compositor-side selection
        // here would be immediately superseded and would break
        // cancel-on-replace / clear-on-death semantics (those need
        // the client source identity). This hook therefore only
        // records WHO owns the selection (for disconnect cleanup)
        // and logs the negotiated mime set. The owner is the
        // keyboard-focused client: smithay DENIES set_selection from
        // any other client (device.rs SetSelection guard).
        let owner = source
            .as_ref()
            .and_then(|_| KBD_FOCUS_CLIENT.lock().unwrap().clone());
        let mime_types: Vec<String> = source.as_ref().map(|s| s.mime_types()).unwrap_or_default();
        info!(?ty, ?owner, mimes = ?mime_types, "selection source changed");
        match ty {
            SelectionTarget::Clipboard => {
                *SELECTION_OWNER_CLIPBOARD.lock().unwrap() = owner;
                self.x11_owns_clipboard = false;
            }
            SelectionTarget::Primary => {
                *SELECTION_OWNER_PRIMARY.lock().unwrap() = owner;
                self.x11_owns_primary = false;
            }
        }
        // G-C4: a Wayland client now owns the selection — tell the X11
        // side so X clients see it (and clear the X ownership marker).
        if let Some(wm) = self.x11_wm.as_mut() {
            let mimes = if source.is_some() {
                Some(mime_types)
            } else {
                None
            };
            match wm.new_selection(ty, mimes) {
                Ok(()) => info!(?ty, "selection propagated to x11"),
                Err(e) => warn!(?e, "failed to propagate selection to X11"),
            }
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: std::os::unix::io::OwnedFd,
        _seat: Seat<Self>,
        user_data: &Self::SelectionUserData,
    ) {
        // G-C4: an X11-owned selection is served by round-tripping
        // through the X selection owner.
        if *user_data == SelectionOwner {
            if let (Some(wm), Some(lh)) = (self.x11_wm.as_mut(), self.loop_handle.clone()) {
                if let Err(e) = wm.send_selection(ty, mime_type, fd, lh) {
                    warn!(?e, "x11 selection transfer to wayland client failed");
                }
            }
            return;
        }
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
    fn started(
        &mut self,
        source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        // A client began a wl_data_device drag (G-B2): the compositor
        // must stop manipulating windows for the remainder of the
        // gesture and feed pointer motion/release to the seat pointer
        // so Smithay's DnDGrab can drive enter/motion/leave/drop.
        // Race guard: a spatial drag started from the same press (5px
        // threshold) must not keep running alongside the protocol drag.
        if self.interaction.is_dragging() {
            self.interaction.handle_pointer_up();
        }
        self.dnd_active = true;
        info!(
            has_source = source.is_some(),
            has_icon = icon.is_some(),
            "dnd: client drag started"
        );
        self.schedule_render();
    }

    fn dropped(&mut self, target: Option<WlSurface>, validated: bool, _seat: Seat<Self>) {
        self.dnd_active = false;
        info!(?target, validated, "dnd: drop finished");
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
                    options: if options.is_empty() {
                        None
                    } else {
                        Some(options)
                    },
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

// G-D2: data-control protocols — clipboard managers observe and set
// selections through the same seat state as wl_data_device.
impl smithay::wayland::selection::wlr_data_control::DataControlHandler for LookingGlass {
    fn data_control_state(
        &self,
    ) -> &smithay::wayland::selection::wlr_data_control::DataControlState {
        &self.wlr_data_control_state
    }
}

impl smithay::wayland::selection::ext_data_control::DataControlHandler for LookingGlass {
    fn data_control_state(
        &self,
    ) -> &smithay::wayland::selection::ext_data_control::DataControlState {
        &self.ext_data_control_state
    }
}

smithay::delegate_data_control!(LookingGlass);
smithay::delegate_ext_data_control!(LookingGlass);

// G-D3: ext_foreign_toplevel_list_v1 — docks/taskbars observe toplevels.
impl smithay::wayland::foreign_toplevel_list::ForeignToplevelListHandler for LookingGlass {
    fn foreign_toplevel_list_state(
        &mut self,
    ) -> &mut smithay::wayland::foreign_toplevel_list::ForeignToplevelListState {
        &mut self.foreign_toplevel_state
    }
}

smithay::delegate_foreign_toplevel_list!(LookingGlass);

// G-D4: wp_presentation — feedback answers are fired from the render
// loop after each presented frame.
smithay::delegate_presentation!(LookingGlass);
delegate_pointer_constraints!(LookingGlass);
delegate_relative_pointer!(LookingGlass);
delegate_dmabuf!(LookingGlass);

#[cfg(test)]
mod serial_ledger_tests {
    use super::*;
    use std::collections::VecDeque;

    fn q(items: &[u32]) -> VecDeque<u32> {
        items.iter().copied().collect()
    }

    #[test]
    fn ledger_skips_serial_zero() {
        // P1: serial 0 is never a real input event (smithay's initial
        // Serial) — recording it would let a zero-serial grab validate.
        let mut l = VecDeque::new();
        ledger_push(&mut l, 0);
        assert!(l.is_empty());
        assert!(!ledger_contains(Some(&l), 0));
    }

    #[test]
    fn ledger_caps_at_limit_dropping_oldest() {
        let mut l = VecDeque::new();
        for s in 1..=(INPUT_SERIAL_LEDGER_CAP as u32 + 5) {
            ledger_push(&mut l, s);
        }
        assert_eq!(l.len(), INPUT_SERIAL_LEDGER_CAP);
        // Newest kept…
        assert!(ledger_contains(
            Some(&l),
            INPUT_SERIAL_LEDGER_CAP as u32 + 5
        ));
        // …oldest evicted — a stale serial no longer validates.
        assert!(!ledger_contains(Some(&l), 1));
    }

    #[test]
    fn ledger_rejects_unknown_and_empty() {
        assert!(!ledger_contains(None, 7));
        assert!(!ledger_contains(Some(&q(&[1, 2, 3])), 9));
        assert!(ledger_contains(Some(&q(&[1, 2, 3])), 2));
    }

    #[test]
    fn ledger_survives_window_wraparound() {
        // u32 wraparound: a huge serial followed by small ones (both
        // legitimate after wrapping) are recorded as-is — matching
        // contains() semantics, no ordering assumptions.
        let mut l = VecDeque::new();
        ledger_push(&mut l, u32::MAX - 2);
        ledger_push(&mut l, 4);
        assert!(ledger_contains(Some(&l), u32::MAX - 2));
        assert!(ledger_contains(Some(&l), 4));
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    fn all_finite(m: &Matrix4<f32>) -> bool {
        let cells: &[f32; 16] = m.as_ref();
        cells.iter().all(|v| v.is_finite())
    }

    #[test]
    fn zero_height_is_clamped_not_nan() {
        // P1 (audit): winit reports 0×N on some minimize transitions.
        assert!(all_finite(&LookingGlass::projection_for(
            false, 1280.0, 0.0
        )));
        assert!(all_finite(&LookingGlass::projection_for(true, 1280.0, 0.0)));
    }

    #[test]
    fn zero_width_is_clamped_not_nan() {
        assert!(all_finite(&LookingGlass::projection_for(false, 0.0, 720.0)));
        assert!(all_finite(&LookingGlass::projection_for(true, 0.0, 720.0)));
    }

    #[test]
    fn normal_size_projection_unchanged() {
        let p = LookingGlass::projection_for(false, 1280.0, 720.0);
        let expected = cgmath::ortho(-640.0, 640.0, -360.0, 360.0, -1000.0, 1000.0);
        assert_eq!(p, expected);
        let sp = LookingGlass::projection_for(true, 1280.0, 720.0);
        let expected_sp = cgmath::perspective(cgmath::Deg(45.0), 1280.0 / 720.0, 1.0, 10000.0);
        assert_eq!(sp, expected_sp);
    }
}

#[cfg(test)]
mod damage_tests {
    use super::*;

    fn rect(
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Buffer> {
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

#[cfg(test)]
mod r10_tests {
    /// R10: timestamps come from a monotonic source — repeated calls
    /// never move backwards, and the anchor is process start (not the
    /// wall clock, which NTP can move).
    #[test]
    fn now_ms_is_monotonic() {
        let a = super::now_ms();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = super::now_ms();
        assert!(b >= a, "timestamp moved backwards: {a} -> {b}");
        // u32 wrapping representation is the Wayland contract; the
        // value must fit its type by construction.
        let _c: u32 = super::now_ms();
    }
}

#[cfg(test)]
mod gc3_logical_geometry_tests {
    use super::logical_geometry_from_buffer as g;

    #[test]
    fn scale1_buffer_is_identity() {
        let (size, src) = g((800, 600), 1, None, None);
        assert_eq!(size, (800, 600));
        assert_eq!(src, None);
    }

    #[test]
    fn buffer_scale_2_halves_logical_size() {
        let (size, src) = g((1600, 1200), 2, None, None);
        assert_eq!(size, (800, 600));
        assert_eq!(src, None);
    }

    #[test]
    fn zero_scale_treated_as_one() {
        let (size, _) = g((800, 600), 0, None, None);
        assert_eq!(size, (800, 600));
    }

    #[test]
    fn viewport_dst_wins_over_buffer_scale() {
        let (size, _) = g((1600, 1200), 2, Some((640, 480)), None);
        assert_eq!(size, (640, 480));
    }

    #[test]
    fn viewport_src_size_used_when_no_dst() {
        let src = Some(((10.0, 20.0), (320.0, 240.0)));
        let (size, uv) = g((640, 480), 1, None, src);
        assert_eq!(size, (320, 240));
        let uv = uv.expect("src rect produces normalized uv");
        assert_eq!(uv, [10.0 / 640.0, 20.0 / 480.0, 0.5, 0.5]);
    }

    #[test]
    fn viewport_dst_and_src_both_set_uses_dst_with_src_uv() {
        let src = Some(((0.0, 0.0), (100.0, 50.0)));
        let (size, uv) = g((1000, 500), 1, Some((200, 100)), src);
        assert_eq!(size, (200, 100));
        let uv = uv.expect("src rect still crops the texture");
        assert_eq!(uv, [0.0, 0.0, 0.1, 0.1]);
    }

    #[test]
    fn no_viewport_has_identity_uv() {
        let (_, src) = g((800, 600), 1, None, None);
        assert!(src.is_none(), "no viewport means identity (None) uv rect");
    }
}
