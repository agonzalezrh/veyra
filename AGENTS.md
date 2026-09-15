# Veyra — AI Development Instructions

## 1. Project identity

**Veyra is a spatial Wayland compositor/desktop.**

The primary goal is to create a **spatial desktop for normal Wayland applications**.

Normal Wayland application surfaces (XDG toplevels) are first-class 3D visual objects in the scene.

The user should be able to run normal Linux applications — terminals, browsers, editors, GTK/Qt applications — and arrange their windows in genuine 3D space.

Applications remain unaware that the desktop is spatial.

The core product is:

```text
Wayland clients
       ↓
XDG toplevel surfaces
       ↓
Visual objects
       ↓
3D Scene
       ↓
Camera / Layout / Interaction
       ↓
GLES Renderer
```

The MVP must be demonstrable using ordinary Wayland applications alone.

Provider-specific code belongs ONLY behind the `FrameProducer`, `InputSink`, and `ProviderCapabilities` interfaces. The following must NEVER contain provider-specific logic:

- `Scene`
- `Visual`
- `Renderer`
- `Camera`
- `InteractionController`
- `InputRouter`
- `Layout`
- `Workspace`

The compositor owns:

- window placement
- 3D transforms
- camera
- workspaces
- animation
- picking
- spatial navigation
- rendering
- decorations
- spatial interaction

The application owns:

- application rendering
- Wayland surface contents
- application semantics
- normal Wayland lifecycle

---

# 2. Technology

Primary language:

- Rust

Primary compositor framework:

- Smithay

Primary initial renderer:

- OpenGL / GLES

Initial backend:

- nested Wayland/winit backend where practical

Later backend:

- DRM/GBM/libseat

Required eventual compatibility:

- native Wayland
- XWayland

Do not introduce Vulkan during the initial implementation.

Vulkan may be added later behind the Renderer abstraction.

---

# 3. Fundamental architectural principles

## 3.1 Separate concerns

Never mix these concepts unnecessarily:

1. Wayland protocol state
2. Window-management state
3. Scene state
4. Rendering state
5. Animation state
6. Input state

A Wayland surface is NOT a scene node.

A window is NOT a renderer texture.

A renderer must NOT become the owner of window-management state.

## 3.2 Four-layer architecture

Wayland protocol state, application content state, spatial presentation state, and rendering state are four separate layers. No provider-specific implementation may leak upward across these boundaries.

```text
┌──────────────────────┐
│  Wayland Protocol    │  clients, surfaces, protocol objects
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│  Application Content │  buffer contents, window metadata
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│  Spatial Workspace   │  VisualState, Camera, Focus, Layout, Snap
└──────────┬───────────┘
           │
┌──────────▼───────────┐
│  Scene + Renderer    │  scene graph, GLES/DRM presentation
└──────────────────────┘
```

Each layer owns its own state. Lower layers never reach upward.

In particular:

- A Wayland surface has compositor-global lifetime.
- A Visual has workspace-local lifetime.
- A Renderer never owns window-management state.

## 3.3 Spatial desktop invariants

These rules apply to all spatial operations (Groups C+):

1. **Camera state never modifies Visual transforms.** Camera movement (orbit/pan/zoom/focus/overview) changes the view, not the scene.

2. **Arrangement produces transforms; it does not own transforms.** The arrangement engine computes desired positions; WorkspaceState owns them.

3. **Spatial groups contain presentation relationships, never Wayland protocol relationships.** A Group is a `Vec<VisualId>` with a transform, not a Wayland object.

4. **Overview is a camera mode, not a scene mutation.** The overview "zoom-out" changes `CameraMode`; Visual transforms remain untouched.

5. **Focus is a camera/presentation operation, never a Wayland surface lifecycle operation.** Focusing changes the camera trajectory and visual emphasis, not Wayland keyboard focus or surface state.

6. **No spatial feature may require a Wayland client to know that Veyra is 3D.** Applications must believe they are talking to a normal 2D Wayland compositor.

---

# 4. Source of truth

The compositor global state is the authoritative owner of:

- clients
- windows
- outputs
- workspaces
- scenes
- cameras
- input state
- configuration

Avoid global mutable state.

Prefer explicit ownership through the central compositor state.

Smithay's architecture is designed around a central compositor state and handler/delegate model. Follow that model rather than fighting it.

---

# 5. Do not over-engineer early

The implementation must proceed incrementally.

Never implement:

- Vulkan
- advanced lighting
- multi-monitor spatial layouts
- advanced reflections
- plugin APIs
- persistence
- scripting
- advanced gestures

until the earlier milestone explicitly requires them.

---

# 6. Never implement future milestones early

If a task says:

"Implement 3D transforms"

do NOT additionally implement:

- ray picking
- animations
- spatial workspaces
- Vulkan
- multi-monitor
- window backs

unless explicitly requested.

This is extremely important.

---

# 7. Minimal coherent changes

For each task:

1. Inspect the current repository.
2. Identify the smallest set of files that should change.
3. Explain the intended implementation.
4. Implement it.
5. Compile.
6. Run relevant tests.
7. Run clippy when appropriate.
8. Report failures honestly.
9. Do not silently disable functionality.
10. Do not rewrite unrelated code.

---

# 8. Never fake functionality

Do not create fake implementations merely to satisfy tests.

Examples of forbidden shortcuts:

- hardcoded window positions pretending to be a layout engine
- fake Wayland events
- sleeping instead of synchronization
- CPU rendering when GPU rendering is required
- fake application surfaces
- hardcoded protocol responses
- ignoring configure/ack_configure semantics
- silently dropping unsupported protocol behavior

If something cannot yet be implemented correctly, state that clearly.

---

# 9. Wayland correctness

Wayland protocol semantics take priority over convenience.

In particular:

- respect object lifetimes
- respect surface roles
- respect configure/ack_configure
- respect double-buffered state
- respect popup/transient relationships
- respect subsurfaces
- correctly handle surface destruction
- correctly handle client destruction

Do not invent compositor-specific semantics where Wayland already defines behavior.

---

# 10. Window model

The logical window model must remain independent from the rendering implementation.

Conceptually:

Window
├── identity
├── Wayland surface relationship
├── application metadata
├── lifecycle state
├── workspace membership
├── logical geometry
└── visual state

Visual state contains:

- position
- rotation
- scale
- opacity
- animation state

Use quaternions for internal 3D rotation.

Do not use Euler angles as the authoritative rotation representation.

---

# 11. Scene graph

Windows must eventually be represented as scene nodes.

The scene graph must be independent from Wayland protocol handlers.

Conceptually:

Scene
├── background
├── workspace nodes
│   ├── window node
│   ├── window node
│   └── window node
└── compositor UI

Do not render windows directly from Wayland callbacks.

Wayland callbacks update state.

The frame/rendering pipeline consumes state.

---

# 12. Renderer abstraction

Create a renderer abstraction early enough that rendering implementation is replaceable.

Initial implementation:

OpenGL/GLES

Possible future implementation:

Vulkan

The compositor must not contain OpenGL calls throughout unrelated code.

OpenGL-specific code belongs in the renderer subsystem.

---

# 13. Frame loop

Never use:

- busy loops
- arbitrary sleeps
- fixed-delay frame loops

Use compositor/backend/display timing.

Eventually target:

- 60 Hz
- 120 Hz
- 144 Hz
- 165 Hz
- 240 Hz

without changing application semantics.

---

# 14. Input

Input must be understood in terms of the 3D scene.

Pointer interaction eventually becomes:

screen coordinate
→ camera unprojection
→ world ray
→ scene intersection
→ selected window
→ Wayland surface coordinates

Do not permanently assume that screen X/Y directly correspond to window X/Y.

---

# 15. Coordinate systems

Explicitly distinguish:

- surface coordinates
- window-local coordinates
- workspace coordinates
- world coordinates
- camera coordinates
- output coordinates
- framebuffer coordinates

Never silently mix them.

Name conversions explicitly.

---

# 16. Performance

Avoid unnecessary CPU/GPU copies.

Eventually prefer:

Wayland buffer
→ DMA-BUF/EGL image
→ GPU texture
→ 3D composition

However, simpler SHM paths are acceptable during early development.

Correctness comes before optimization.

---

# 17. Damage

Initially full-frame rendering is acceptable.

Later implement proper:

- surface damage
- window damage
- scene damage
- output damage

Do not prematurely build a complicated damage system before the renderer is functional.

---

# 18. Multi-monitor

Do not implement until explicitly scheduled.

The first compositor target is:

- one GPU
- one output
- one workspace
- one camera

The architecture should not prevent later expansion.

---

# 19. XWayland

XWayland is required for a useful desktop but should not block the earliest compositor milestones.

Implement it after native Wayland applications are functioning.

---

# 20. User experience

The central UX principle is:

"2D when working, 3D when navigating."

Normal mode should feel familiar.

Spatial mode should expose the 3D desktop experience.

Do not force users to manipulate windows in 3D continuously.

---

# 21. 3D design philosophy

Prefer subtle and useful 3D.

Good:

- perspective
- spatial overview
- spatial Alt-Tab
- camera movement
- window backs
- depth
- spatial workspaces
- smooth transitions

Avoid excessive:

- bouncing
- spinning
- permanent transparency
- distracting particles
- gratuitous animation

The project should feel like a desktop, not a demo scene.

---

# 22. Testing

Every subsystem must have tests where practical.

Important test categories:

- geometry
- transforms
- camera mathematics
- ray intersection
- layout algorithms
- animation
- workspace management
- window lifecycle
- protocol state
- configuration parsing

Rendering tests may initially use mathematical/unit testing rather than screenshots.

---

# 23. Task discipline

Every implementation task must have:

## Before implementation

- objective
- relevant architecture
- affected files
- dependencies

## After implementation

- compilation result
- tests run
- clippy result
- known limitations
- files changed

---

# 24. Development roadmap

The project is organized into milestone groups. Each group is implemented
autonomously as a batch, with the AI running all milestones, running
tests, and committing before moving to the next group.

## Group A — Workspace Foundation (M056.1–M060)

Per-workspace transforms, picking, snapping, focus, layout, and persistence.
State model formalized with WorkspaceManager. Multi-workspace lifecycle,
navigation, and persistence v2 with schema versioning.

Exit criteria: 155–170 tests, 3+ workspaces with independent state,
no cross-workspace interaction.

**Status: ✅ Complete (149 tests)**

## Group B — Native Wayland Desktop (M061–M066)

Toplevel lifecycle hardening, keyboard focus model, pointer grabs,
XDG popups/transients, decorations/chrome, native input integration.
Veyra runs a normal Wayland session with foot, menus, dialogs.

**Status: ✅ Complete (177 tests)**

## Group C — Spatial Desktop (M067–M073)

Spatial anchoring, groups, intelligent arrangement, focus mode v2,
spatial de-emphasis, spatial overview, workspace overview.
Applications inhabit a navigable spatial desktop.

Exit criteria: 210–240 tests, camera/overview/focus are camera-only
operations (never scene mutations), arrangement produces transforms
(never owns them), all Wayland clients remain unaware of the 3D desktop.

**Status: ✅ Complete (228 tests)**

## Group D — Production Architecture (M074–M080)

**Keep `GlesRenderer` concrete. Abstract the presentation backend.**

The renderer and presentation backend are separate concerns:
- GLES is the rendering technology (concrete, stays concrete)
- Winit vs DRM/KMS is how rendered frames are presented (abstract)

`FrameProducer` may continue to operate on `GlesRenderer`. Producers
must NOT know about presentation backends. Remove direct raw GL
operations from producers behind narrow `GlesRenderer`-owned texture
operations.

Do not introduce a general `Renderer` trait. Do not implement DRM/KMS
until presentation-backend boundary and render scheduling are
established.

### M074 — Presentation backend abstraction
### M075 — Texture upload boundary (remove raw GL from producers)
### M076 — Render scheduling (dirty state, no fixed 16ms timer)
### M077 — Damage tracking
### M078 — Native DRM/KMS backend (`DrmGraphicsBackend<GlesRenderer>`)
### M079 — EGL/GLES capability detection (G200eW scenario)
### M080 — Benchmark suite + long-running soak test

**Note**: `/dev/dri/cardN` existence does not imply usable 3D rendering.
EGL/GLES capability detection is required before committing to native
rendering.

**Status: ✅ Complete (247 tests)**

Exit criteria: Wayland/spatial/renderer layers cleanly separated,
presentation backend abstracted, damage tracking + frame scheduling,
benchmark suite, DRM native session works reliably with clean failure
diagnostics, long-running soak test, 240+ tests.

## Group E — Desktop Shell & Human Interaction

Application switching (Alt+Tab), launcher abstraction, spatial task shelf,
coherent camera/input navigation, context menus, configuration system,
session lifecycle, accessibility and recovery.
Desktop shell increasingly implemented as ordinary Wayland applications
rather than compositor-specific UI.

### E-A — Interaction (E1–E4)
**Status: ✅ Complete (284 tests)**
### E-B — Desktop UX (E5–E6)
**Status: ✅ Complete (307 tests)**
### E-C — Session (E7–E8)
**Status: ✅ Complete (344 tests)**

## Group F — Real-Application Compatibility Audit

Diagnostic phase. Real GTK, Qt, Electron, Firefox, Chromium, SDL/XWayland
applications tested against Veyra. No compositor code modified during audit.
Results in `COMPATIBILITY_MATRIX.md` and `BUG_LIST.md`.

## Group G — Compatibility Fix Campaign

### G-A — First Fix Batch (G1–G5)

Priority order from audit:
- G1: XDG popup positioning (affects all toolkits)
- G2: Proper XKB keyboard layout handling
- G3: Clipboard / data-device
- G4: Drag & drop
- G5: Fullscreen protocol

### G-B — Remaining Issues (G6–G10)
**Status: ✅ Complete (344 tests)**

### G-C — Protocol Completeness & Input Integrity (G-C1–G-C4)
- G-C1: clipboard MIME verified end-to-end (tc1–tc4 + real foot both directions)
- G-C2: stuck-META root cause fixed (modifier releases are seat state);
  duplicate-press verified clean with t26i regression guard
- G-C3: wp_fractional_scale_manager_v1 + wp_viewporter advertised; commit
  path adopts LOGICAL geometry (viewport dst/src > buffer_scale); viewport
  src crop via renderer u_src; t20 scale-2 end-to-end
- G-C4: XWayland — XWM + xwayland-shell-v1; X11 windows are first-class
  visuals through the native commit pipeline; selections bridge both
  directions; t21 lifecycle test

**Status: ✅ Complete (480 unit tests; protocol 97/0/0; input 93/0/0)**

### G-D — Desktop Integration Protocols (G-D1–G-D5)
- G-D1: appearance.output_scale drives wl_output scale + preferred
  fractional scale; client-kit logs outmode events; t22 config-driven
  scale end-to-end on its own veyra instance
- G-D2: zwlr_data_control_manager_v1 + ext_data_control_manager_v1
- G-D3: ext_foreign_toplevel_list_v1 (publish at map, live
  title/app_id updates, withdraw on destroy; X11 windows included)
- G-D4: wp_presentation feedback (CLOCK_MONOTONIC, output refresh)
- G-D5: clip_tester --primary (raw protocol); t27i X11 selection bridge
  with real xterm — Wayland→X fully verified, X→Wayland publish
  verified, data fetch from X owner tracked as BUG_LIST #17
- CRITICAL: frame callbacks now complete for ALL mapped surfaces —
  X11 windows previously froze after one frame (blank xterm/xclock)

**Status: ✅ Complete (483 unit tests; protocol 104/0/0; input 95/0/1)**

### G-E — Bridge Completion & Remaining Gaps (G-E1–)
- G-E1: BUG_LIST #17 RESOLVED — the X→Wayland selection "stall" was a
  test-harness chain (unpinned Xvfb focus → empty selections; click one
  row low; focus-gated device broadcasts). With the focus pinned and
  the click on the typed word, the FULL X11 selection bridge verifies:
  xterm → XWM transfer → Wayland client payload delivery (t27i green,
  no skips). xterm→xterm roundtrip also verified.

**Status: ✅ G-E1 Complete (483 unit tests; protocol 104/0/0; input 96/0/0)**

### G-E2 — Remaining P3 Gaps (#9, #11, #6)
- G-E2: ALL remaining P3 items shipped:
  - **#9 runtime scale change**: config file watched with inotify
    (event-driven; calloop's signals feature is not enabled — a write
    triggers Config::load and the output scale applies live: wl_output
    scale to bound clients + preferred fractional scale to mapped
    surfaces). CString pitfall fixed (inotify_add_watch requires a
    NUL-terminated path). Protocol t23: bound client observes scale
    2 → 1 with no restart.
  - **#11 subsurfaces**: subsurface commits map as visuals parented to
    the parent visual (J2 parent-local transforms), SubsurfaceCachedState
    positioning, G-C3 logical geometry. Cleanup keeps veyra's own
    sub→parent link (smithay get_parent unreliable in destroy dispatch).
    client-kit --subsurface + protocol t24 (mapping/geometry/removal).
    LESSON: t22 replaces the veyra instance — later tests must assert
    against CURRENT_VEYRA_LOG, not veyra.log (cost a long debug chase).
  - **#6 IME/text input**: zwp_text_input_v3 + zwp_input_method_v2
    advertised; grab fully routed by smithay. Input-suite t25 verifies
    the FULL loop: field enable → IME activate → grab receives injected
    keys → commit_string("あ"/"漢") → committed_string delivered to the
    focused field. Known limitation: IME candidate popups tracked but
    not rendered yet.

**Status: ✅ G-E2 Complete (483 unit tests; protocol 111/0/0; input 103/0/0)**

### G-E3 — Popup Serial Validation (#12)
- G-E3: xdg_popup.grab validates the serial against a per-client
  ledger of recent INPUT serials (pointer buttons + keyboard events,
  capped at 16 per client). Bogus/future/foreign serials are rejected:
  "popup grab rejected" + popup_done dismissal (client survives).
  Valid grabs log "popup grab accepted (serial validated)". Input-suite
  t26 (`popups --grab`): real-press-serial grabs accepted, bogus-serial
  grabs rejected with popup_done. Harness note: a manual empty
  Dispatch<wl_pointer> impl silently blocks sctk pointer events — use
  delegate_pointer!.

**Status: ✅ G-E3 Complete (483 unit tests; protocol 111/0/0; input 108/0/0)**

### G-E4 — IME Candidate Popup Rendering
- G-E4: input_popup_surface_v2 commits map as visuals PARENTED to the
  focused text field's visual (anchor cached from parent_geometry
  during IME activation; position from smithay's tracked
  PopupSurface.location, derived from the text-input cursor rectangle).
  Dismissal (parent change) removes the old visual. t25 extended:
  ime_tester creates a 220x80 candidate popup after the grab; veyra
  logs "ime popup mapped".

**Status: ✅ G-E4 Complete (483 unit tests; protocol 111/0/0; input 110/0/0)**

### G-E5 — Real-App Input Integrity + Audit Remediation
- G-E5 input fixes (BUG_LIST #18/#19): spatial-mode pointer delivery
  now delivers the unprojected surface coordinate at all five sites
  (clicks no longer land tens of px off under perspective); X11
  override-redirect/EWMH non-focusable windows no longer steal keyboard
  focus on map; SeatHandler::KeyboardFocus is KeyboardFocusTarget
  (WlSurface | X11Surface) so X11 windows receive ICCCM input focus and
  key events (Firefox Ctrl+L/typing verified end-to-end; Chrome
  bookmarks/URL typing verified in spatial mode).
- G-E5 audit remediation: GL context-loss recovery (DRM recreates via
  stashed libseat session; winit fails loudly), begin_frame failure
  state transition, producer consecutive-error disconnect, projection
  NaN guard (degenerate framebuffer sizes), refresh-only mode sync,
  per-frame clone elimination, event-driven DRM page-flip dispatch
  (BUG_LIST #4 step 1), popup-grab keyboard serial recording + ledger
  hardening (#12), window-model type extraction into window.rs,
  outputs.rs per-output state registry (#14 phase 1), taskbar shell
  polish (SDF rounded-rect renderer, hover, separators, accent
  underline) and PUA glyph sentinel.

**Status: ✅ G-E5 Complete (499 unit tests; protocol 111/0/0; input 110/0/0)**

### G-E5 (continued) — Multi-Output Architecture

Post-audit direction: remove the single-output architectural constraint
before any new visual features. Five-phase plan adopted:

```text
G-E5  Multi-output architecture   ← in progress
G-F   Native hardware / presentation hardening
G-G   Rendering + performance scalability
G-H   Desktop UX / spatial interaction
G-I   Production hardening / release
```

Milestones landed:
- **G-E5.0** documentation reconciliation (COMPATIBILITY_MATRIX rewritten
  to post-G-E5 reality; RECOMMENDATIONS addendum; README refresh)
- **G-E5.1** `OutputId` + `HashMap<OutputId, OutputState>` registry
  (outputs.rs; stable ids, row tiling, hit testing, primary semantics)
- **G-E5.2** registry-backed `fb_size()`; ALL single-output consumers
  migrated (projection, picking, layout, taskbar, menus, fullscreen,
  camera fits) — the scalar survives only as the write path + fallback
- **G-E5.3** per-output cameras: the live presentation camera moved into
  `OutputState` (workspace/world shared, view per-output); camera()/
  camera_mut() resolve the primary; split-borrow helpers for focus
  transitions; registry pre-seeded at construction; ~64 sites migrated
- **G-E5.4 step 1** explicit pointer→output resolution at all four
  input entry points (identity single-output; output-local coords feed
  picking)
- **G-F3 step 1** (pulled forward): EGL surface rebinding contained in
  one SurfaceBinding type; the make_current expect() panic removed —
  rebind failures surface as ContextLost into the G-E5 recovery path
- **G-G1** (pulled forward): HashSet visible set in the draw loop
  (was O(N·V) slice contains)
- **G-G2** conservative frustum culling (all-corners-outside-one-plane,
  5% slack, selected/hovered exempt — regressions degrade to over-draw,
  never to a hidden focused window)
- **G-G3** hot-path allocation cleanup (per-visual chrome clone,
  per-button glyph String, per-surface output clone)
- **G-G4** render scalability gate (tests/harness/scripts/
  run_scalability.sh): draw_ms 0.064 -> 0.123 for 10 -> 1000 visuals
  (1.92x per 100x scene growth, llvmpipe/debug — culling effective)
- **G-G5 step 1** output-damage accumulation (Scene::output_damage →
  clipped framebuffer AABBs reported per frame; scissored present =
  G-G6 waits on DRM buffer preservation)
- **G-G6** partial-present machinery shipped dormant-and-correct:
  query-only EGL preservation probe, gated on
  PresentationBackend::preservation_probe_allowed() (default false —
  llvmpipe quirk: extra unconditional GL calls between make_current
  cycles intermittently BadAlloc the next swap; 8/8 clean after
  gating); renderer-side prev-frame diff as damage authority
  (matrix/size/focus/visibility/content, old+new footprints);
  scissored clear/draw ≤70% coverage with identical-view guard;
  PROFILE partial= counter. Activates on drivers that natively
  preserve buffers; winit stays full-frame (correct fallback)
- **G-H3** Arc + Circle arrangements with Meta+L cycling
  (deterministic, content-aware, persistent)
- **G-I1/I2 lite** lifecycle torture runner
  (tests/harness/scripts/run_torture.sh): 200-client churn +
  workspace hammering + resize churn — 0 panics, 0 protocol failures

- **G-E5.5** simulated multi-output SHIPPED: one winit surface, N
  logical outputs (VEYRA_SIM_OUTPUTS=N), each with its own viewport,
  camera, and viewport-size projection over the SHARED scene
  (OutputFramePlan; build_frame_plans pure). Input: pointer→output via
  desktop_origin fb↔global conversion; pointer_view() picks through
  the POINTER'S output's own camera/projection (all four conversion
  sites migrated). OutputFrameReport = per-output presentation
  bookkeeping. Live-verified: window confined to its output's slice,
  independent per-viewport clears, taskbar across the simulation fb.
  Coordinate-contract bugs found and fixed by the live session: EGL
  fresh-bind viewport reset (rebind re-asserts), GL bottom-left
  viewport origin (top-left→GL conversion), sync_output_mode clobbering
  seeded modes with simulation extents.

- **G-E5.6.1** DRM topology model (pure): ConnectorState/TopologyMode/
  TopologyConnector/TopologyCrtc; deterministic assignment (ascending
  connector ids → lowest compatible free CRTC via encoder-candidate
  overlap); unassigned reasons (Disconnected / NoCompatibleEncoder /
  NoFreeCrtc — the hotplug hook); preferred-mode selection; 11 tests
- **G-E5.6.2** per-output presentation state + topology-driven init:
  OutputPresentation { crtc, gbm_surface, fb_cache, flip_pending,
  current_buffer, size } extracted from the backend (renderer stays
  per device); discover_topology() maps the REAL device onto the
  model (encoder→CRTC via filter_crtcs); init replaces the
  first-connected-display scan with topology.assign(). VKMS-validated:
  connector 38 → crtc 37 @1024x768; the probe's dmabuf-mmap
  permission failure is PRE-EXISTING (identical on pre-change HEAD).

- **G-E5.6.3/6.4** per-output frame lifecycle + buffer ownership:
  OutputFrameState (Idle/Rendering/Submitted/FlipPending) with PURE
  transitions (begin rejects double-begin/begin-during-flip;
  submit/arm_flip chain armed only on queue_buffer success; spurious
  flips are no-ops; force_idle = the 6.6 disconnect hook);
  begin_output/finish_output(idx) seam (begin_frame/finish_frame
  delegate to 0 until 6.5 wires OutputId→index); flip events
  ATTRIBUTED BY CRTC — flip(A) retires only A, unknown-CRTC events
  dropped; 11 lifecycle tests. VKMS re-validated through the new path.

- **G-E5.6.5** OutputId→backend-index binding + native mode adoption:
  OutputBindings (the explicit map — survives reordering/non-
  contiguous ids, backend_removed detaches+compacts); backend exposes
  its TopologyAssignment; adopt_native_outputs registers REAL KMS
  modes/positions via global_positions (the E5.5 oracle). Pure test:
  2-connector assignment → OutputStates → plans tile exactly as the
  nested simulation.
- **G-E5.6.6** hotplug lifecycle: OutputLifecycle
  (Disconnected/Discovered/Active/Draining/Removed, ordered
  transitions; Draining still presented until quiesced);
  OutputManager::unplug_output → HotplugEffect { was_primary,
  promoted } with scene/workspaces structurally unreachable (THE
  invariant: a physical output disappearing never destroys windows);
  replug = NEW identity; DrmGraphicsBackend::unplug_output =
  force_idle THEN remove (presentation-only, wired for runtime
  events). 9 hotplug tests.
- **G-E5.6.7** consolidated regression suite (drm_regression.rs):
  15 tests ordered BY ARCHITECTURE (L1 topology → L2 bindings → L3
  frame state → L4 CRTC attribution → L5 hotplug → full chain with
  scrambled ids). 582 unit tests.

Remaining: G-F3 (safe EGL/presentation API), then the hardware matrix
(G-F1/F4/F5) — E5.6 is COMPLETE at every layer that can be validated
without multi-connector hardware (VKMS: one connector, no hotplug). Exit criteria:
two outputs w/ different resolutions+scales, cross-output picking,
straddling windows, per-output fullscreen/shell/IME, hotplug without
restart — and single-output tests pass unchanged.

**Status: 🟡 G-E5 multi-output COMPLETE (E5.1–E5.7: registry, cameras, input, simulated presentation, DRM topology/lifecycle/ownership/regression-suite); integration beyond VKMS hardware-gated (582 unit tests; protocol 111/0/0; input 108/2)**

### G-H0 — Spatial interaction ergonomics (in progress)
- **G-H0.1** decorationless default: DecorationConfig::default()
  .title_bar_height = 0.0 (UX-P1: application content owns the window
  area; operations live in taskbar/context menu/keyboard — parity
  unchanged, verified). SSD chrome draws only when explicitly
  requested; edge ring (spatial focus indication) stays. Harness
  updated: t20i re-purposed into the decoration-policy regression test
  (the OLD title-strip position must deliver content clicks to the
  client); t27i parses veyra's XWayland display from the log (stale
  :0 servers made smithay pick :1). Known harness flake: foot's PTY
  slave exits with SIGHUP under the harness (environmental — foot runs
  clean manually; clipboard covered by protocol tests).
- **G-H0.2/H0.3/H0.4** pointer-directed wheel dolly: background wheel
  = camera dolly along the pointer's ray (zoom-to-cursor via the
  desktop-plane intersection); client wheel = client scroll; Meta+wheel
  = camera everywhere (spatial); normal mode = no-op (ortho pin).
  Fixed winit LineDelta wheels reading 0.0 (amount_v120 fallback —
  clients were getting zero wheel values too) and the smithay
  LineDelta negation (wheel-up approached, not retreated).

- **G-H0.5** left-drag on empty spatial background = grab-the-world
  pan: the gesture is decided by the PRESS location (background arms a
  grab; window/shell presses keep window interaction); pointer-relative
  — the grabbed world point (ray ∩ z=0 plane) stays exactly under the
  cursor while dragging; 5 px click/pan threshold; camera-only state.
  Mouse vocabulary now complete: LEFT=move, RIGHT=look, WHEEL=approach.
  Live-verified (grab resolution through the spatial camera; invariant
  unit-tested end to end).
- **G-H0.5.3 (post-review fix)** ONE authoritative pointer pick: the
  dual pick path (InteractionController's full-fb NDC picking fed
  output-local coords — correct only for the identity output, broken
  with N outputs; route_to_content double-converted) is closed.
  pick_visual_at() (pointer_view → scene.pick_visible) is the single
  authority for selection/manipulation/gestures/menus; the
  InteractionController receives the RESOLVED target
  (handle_pointer_down_picked/begin_manipulation). New right-button
  grammar: right-drag on a WINDOW rotates THAT window (threshold-
  gated; release below threshold = context menu); on background =
  camera orbit. Live-verified: two windows tilt OPPOSITE ways,
  desktop/taskbar unrotated, no menu.

**FROZEN INTERACTION CONTRACTS (G-H0.5, do not regress):**

```text
Empty spatial scene:              Window:
  LMB drag  -> camera/world pan     LMB click -> select/focus
  RMB drag  -> camera orbit         LMB drag  -> APPLICATION CONTENT
  Wheel     -> pointer-directed        (text selection, sliders)
               dolly                Meta+drag -> move THAT window
                                    RMB drag  -> rotate THAT window
                                    Wheel     -> application scroll
Meta + wheel -> camera dolly regardless of target
Taskbar click on an off-view window -> camera frames it (H1)
(Amended H1 from manual-test evidence: a plain LMB drag on a window
belongs to the application — text selection was impossible under the
old "LMB drag = manipulate" rule; window movement moved to the
desktop-standard Meta+drag. Camera navigation on background and the
5px thresholds are unchanged.)
Right release < 5px on window -> context menu (click semantics)
5 px threshold separates clicks from drags everywhere.
The gesture is decided by the PRESS location.
```

INVARIANTS (both directions, structurally enforced):
- camera manipulation NEVER changes window transforms, workspace
  membership, or application coordinates;
- window manipulation NEVER changes camera state.
Picking authority: pick_visual_at() (pointer_view ->
scene.pick_visible) is the ONLY definition of "what is under the
pointer"; nothing else may pick. XWayland pointer verified: X11
windows receive click, right-click menu, and wheel through the same
path.

- **G-F3** opaque FrameTarget — safe presentation API: the
  renderer/presentation boundary is a COMPILE-TIME property. FrameTarget
  (EglWindow { ctx, surface } | BoundFbo) owns ALL binding detail;
  make_current(gl, viewport) enforces the rebind invariant (fresh
  binds reset viewport/scissor — re-assert the ACTIVE viewport);
  natively_preserves_buffers(probe_gate) carries the G-G6 query;
  PresentationBackend::frame_target() replaces egl_surface().
  renderer.rs can no longer name a presentation type — enforced by
  renderer_never_names_presentation_types (structural test scanning
  for egl_surface/EGLSurface/egl_context/EGLContext/gbm/crtc tokens).
  Verified through both paths (simulated multi-output + VKMS probe).
  E5 multi-output architecture COMPLETE; remaining: G-F1/F4/F5
  (hardware-gated).

- **G-H0.6** user-report batch (live E2E + VLM verification):
  1. **Window rotation 0.3→1.2 °/px** (interaction.rs RotateX/Y/Z) —
     "rotate a window is almost impossible, is too slow".
  2. **Camera orbit factor 5.0→0.9** (0.26 °/px; was 1.43 °/px — a
     ~100 px right-drag swung the scene ~150°). Root of the
     "moving one window moves all windows" perception: a near-miss
     right/left drag on the background spun or panned the world.
  3. **Camera auto-fit after placement** (fit_camera_to_placed, called
     from all three map sites): a placed window that does not fit the
     z=0 frustum dollies the camera out (camera-only op; transforms
     untouched — arrangement still owns placement). Root of "second
     window positioned where the first is": a second 900×600 window
     appended at world x=948 against a ±360 visible half-width
     (88 % off-screen, no camera response).
  4. Verified live (Xvfb + xdotool + VLM image checks): two 900×600
     windows fully visible side-by-side after auto-fit (camera z
     869→1963); client-kit drag isolation (+250 px, WB static);
     Firefox X11 drag isolation (−184 px); xterm click-focus + typing
     end-to-end ("echo works" executed); Firefox URL bar typing with
     live autocomplete dropdown; rotation visibly responsive.
  - HARNESS LESSON: for the nested session, xdotool/import target
    :99 (the desktop); DISPLAY=:0 is ONLY for launching X11 clients
    into veyra's XWayland; XTEST keys additionally require
    `xdotool windowfocus <veyra-winit-win>` or they are dropped by
    the X server (zero KEY logs = focus was missing).
  - Snap firefox cannot connect to XWayland :0 from its sandbox
    (environmental; unsets WAYLAND_DISPLAY and it maps via X11).
  - Suites: input 108/2 (tcfoot foot-PTY SIGHUP flake family), protocol
    111/0/0, 586 unit tests, clippy 0 (drm_regression/outputs unused
    imports + dead stores cleaned).

- **G-H0.7** UX regression gate + two REAL interaction bugs it caught
  (the gate immediately paid for itself):
  1. **Pan armed on WINDOW presses** (compositor.rs down flow):
     background_press was derived from route_to_content's result —
     every successful content delivery (Routed ≠ TitleBarHit) armed
     the grab-the-world pan, so in spatial mode LMB-dragging a window
     PANNED THE SCENE (the literal "moving one window moves all the
     windows"). The earlier "drag isolation verified" checks had been
     pan artifacts (the second window was clipped off-screen).
     Background is now exactly pick=None (or an off-workspace hit).
  2. **Drag-plane degeneracy** (interaction.rs drag_plane_normal): the
     wall-plane candidate is rejected when the ray never crosses it
     ahead of the camera (t = num/denom ≤ 0 — camera in the plane or
     plane behind the view). Symptom: rotate a window (right-drag),
     then LMB-drag did NOTHING (grab point collapsed to the camera;
     delta ≡ 0). New regression test
     drag_plane_normal_camera_in_wall_plane_falls_back.
  3. Right-button release now ENDS the rotation drag (a stale RotateY
     consumed subsequent left-drags as more rotation).
  - **tests/harness/scripts/ux_env.sh** — the nested-session
    environment codified (Xvfb :99 = desktop/xdotool; veyra's XWayland
    display parsed from the log for CLIENT launches only; automatic
    windowfocus before XTEST keys; journal/pixel helpers). Test
    authors no longer need to remember the display rules.
  - **tests/harness/scripts/run_ux_gate.sh** — the permanent UX gate
    (20 checks): new-application usability (A/B launch → journal
    geometry asserts → click-focus → typed keys), Firefox real-app
    scenario, spatial auto-fit, wheel dolly-in, LMB pan, RMB orbit,
    window rotation, window drag isolation, normal-mode return — with
    the FROZEN INVARIANTS asserted from the debug journal:
    I1 camera changes ≠ window transforms (pan/orbit/wheel),
    I2 window transform changes ≠ other windows (single-mover drag).
    Journal snapshots now fire at every gesture end (pointer up, wheel
    dolly, spatial toggle, frame_all, X11 map) and carry the full
    camera position.
  - Gate debug cycle caught harness bugs too (Geometry grep
    case-sensitivity — the very lesson ux_env codifies; stale-snapshot
    reads before state-transition journaling existed).
  - Suites after the fixes: 587 unit tests, protocol 111/0/0, input
    108/2 (tcfoot flake family only), clippy 0, UX gate 20/20.
  - **Wheel retreat study (user-requested, empirical)**: 10× wheel-in +
    10× wheel-out at the same background point returns the view to
    within ONE pixel-sample of the start (w2-0 vs w2-out bbox equal,
    51929 vs 51930 samples); VLM confirms both windows stay visible
    and intelligible mid-zoom and the end state matches a fresh view.
    VERDICT: keep the zoom-to-cursor model — no geometry change; the
    lateral travel is the pointer-ray geometry working as designed.

- **G-H0.8** golden user journey + gate tiering + the normal-mode
  centering fix the journey exposed:
  1. **tests/harness/scripts/run_golden_journey.sh** — the canonical
     "does the desktop actually work?" gate: launch Veyra → Firefox →
     Foot (xterm fallback if the foot PTY flake hits, reported as
     UNCERTAIN) → both automatically visible (journal geometry) →
     normal↔spatial round trip → wheel toward Firefox over a
     JOURNAL-DERIVED background point (ux_background_point) → click
     focus → typed URL → scroll over the app (asserts the camera is
     UNTOUCHED — the wheel→application contract) → wheel away →
     LMB pan → RMB orbit → rotate Firefox → rotate the second app
     DIFFERENTLY (disjoint pixel-diff regions, selection-ring isolated
     by re-baselining after selection) → return normal → both apps
     still correct. 20 pass / 0 fail / 1 UNCERTAIN (rotation-region
     overlap — reported for review, not a silent failure).
  2. **Normal-mode content centering** (the journey caught a REAL gap):
     returning to normal mode pinned the camera at (0,0,500), so a
     row that grew in spatial mode was UNREACHABLE in the 2D view
     ("return normal → everything still correct" failed). Entering
     normal mode now centers the camera on the active workspace's
     content bbox; the per-frame pin keeps x/y (only z=500 + yaw/pitch
     are pinned), which also makes middle-drag pan persist in normal
     mode. UX gate re-verified 20/20 after the change.
  3. **VLM taxonomy** (visual_check.py): PASS / FAIL / UNCERTAIN —
     deterministic failures and clear anomalies are FAIL; weak
     suspicion (blurry/ambiguous/small) is UNCERTAIN and is counted
     separately (unc()) and reported — an uncertain finding never
     poisons the suite silently and never fails it either.
  4. **Gate tiering**: run_ux_gate.sh --fast = fast gate (per
     commit); default = full gate (per merge); run_golden_journey.sh =
     canonical E2E; run_overnight_gate.sh = nightly (fast + full +
     journey + scalability + torture); run_hw_campaign.sh = G-F1
     F1.1–F1.7 stage skeleton — honest SKIP without VEYRA_HW=1 AND a
     real driver (this box's card0 is simple-framebuffer; VKMS is
     topology-only).
  - Shared gate/journey helpers (ux_geom/ux_project/ux_row/
    ux_no_window_moved/ux_single_mover/ux_background_point/ux_click2)
    factored into ux_env.sh.
  - Suites: 587 unit, protocol 111/0/0, input 108/2 (tcfoot flake
    family), clippy 0, UX gate 20/20, golden journey 20/0/1.

- **G-H0.9** cold-restart/recovery journey + THREE defects it caught
  on its first two runs (the E2E layer's third consecutive catch):
  1. **SIGTERM killed veyra WITHOUT saving** — logout/systemd send
     SIGTERM; veyra only saved on the winit CloseRequested path.
     Now: signals are blocked process-wide at startup, a sigwait
     thread sets a flag and pings the render loop, and the pump
     (main-thread state access) runs save_state → shutdown_sequence
     → exit(0). "shutdown signal received — saving state" in the log.
  2. **X11 windows never restored persisted transforms** — the X11
     map path had no saved_state/pending_reopen consultation (Firefox
     always came back at the origin). The Wayland restore pattern
     (reopen + take_visual + saved-workspace membership, R3) is now
     mirrored in the X11 commit path; restored X11 windows skip the
     placement AND the placement auto-fit.
  3. **The one-shot spatial frustum fit stomped the RESTORED camera**
     on the first frame (load_saved_state set the camera, the fit
     reset it to the default distance). load now marks
     spatial_cam_adapted=true; restored X11 maps do the same.
  - tests/harness/scripts/run_restart_journey.sh — start → launch
    Firefox + Foot → manipulate spatially (drag + rotate + dolly) →
    SIGTERM → assert the state file carries the MANIPULATED transform
    and camera → restart → relaunch → transform restored EXACTLY
    (298.8), camera restored EXACTLY (1064.7), workspace restored →
    interact again (wheel approach + I1 pan). Result: 15/0/0.
    (xdotool windowclose is NOT a clean shutdown — it destroys the X
    window and winit panics on the dead drawable; SIGTERM is the
    production path and the journey uses it.)
  - run_hw_campaign.sh rewired per directive: the hardware campaign
    REUSES the nested runners (gate fast/full + golden journey +
    restart journey — no separate hardware suite). Environment shims
    documented (ydotool/uinput input; VEYRA_SHOT_DIR frame capture);
    small-first stage order f1.1 boot → f1.7 restart, then VT/hotplug/
    reset/soak. Honest SKIP without VEYRA_HW=1 + a real driver (this
    box's card0 is simple-framebuffer).
  - run_overnight_gate.sh now includes the restart journey.
  - Suites: 587 unit, clippy 0, restart journey 15/0/0; the UX gate +
    golden journey unchanged (20/20, 20/0/1).

- **G-H0.9b** crash/unclean-termination journey — the lifecycle
  triangle closed alongside the clean SIGTERM path:
  clean: operate → SIGTERM → save → restart → restore (restart journey);
  unclean: operate → SIGKILL → SAFE RECOVERY (this journey).
  - tests/harness/scripts/run_crash_recovery.sh (14/0/0):
    A1 SIGKILL with no state file → next boot clean-starts (stale
    transient IPC state — the leftover wayland socket — proven
    harmless); A2 SIGKILL after a clean save → the stale-but-valid
    state still loads on the next TWO boots (SIGKILL never damages a
    written file); A3 CORRUPTED state file → "corrupt saved state,
    backing up and starting fresh" + veyra-state.json.bak + a running
    session (SIGKILL persistence is NOT a product requirement — safe
    recovery IS, per directive).
  - Added to run_overnight_gate.sh.
  - Directive recorded: further nested journeys are FAILURE-DRIVEN
    only; the interaction grammar is frozen; the next meaningful
    unknown is the real libseat→DRM→GBM→EGL→GLES→KMS path (G-F1).
  - Suites: 587 unit, clippy 0; gate 20/20, golden 20/0/1, restart
    15/0/0, crash 14/0/0.

- **G-H0.10** manual-test fix batch (four user reports) + the gate
  fixes their gate-run exposed:
  1. **Grammar amendment (user report #1: "can't select")**: plain LMB
     drag on a window now delivers to the application (text selection
     works); window movement moved to Meta+drag (mutter/kwin
     convention). S10 of the gate updated to Meta+drag; contracts
     table amended (above).
  2. **Taskbar activation frames off-view windows (#4)**: activating a
     window whose projected center is outside the framebuffer now
     dollies the camera to frame it (camera-only; visual_center_onscreen
     + Camera::frame_visual).
  3. **Gate non-vacuousness**: S7 pan / S8 orbit now assert the CAMERA
     MOVED (the I1-only asserts passed vacuously when a gesture died);
     S7/S8 press points are journal-derived background points (the
     fixed (150,100)/(200,200) points could land on a window after a
     pan — S8 was silently ROTATING a window instead of orbiting).
  4. **Gate bugs found by the user's run**: unc() was not defined in
     the gate (their S-FF VLM finding crashed the assert); S11's F5
     retry now uses --clearmodifiers (their machine had a LATCHED ALT:
     "KEY raw_code=71 ... alt: true" — the F5 no-modifier binding
     never matched); rot0 baseline-shot restored (dropped by the
     TARGET-loop edit → SIZE-MISMATCH).
  5. Harness hygiene: ux_kill_all TERMs firefox before kill -9 (the
     SIGKILL sets firefox's crash flag → a crash dialog pollutes the
     next S-FF); snap firefox rejects --profile outside its sandbox
     (environmental on this box, noted).
  - Live: gate 23/0/1 (the one UNCERTAIN = this box's dirty snap
    profile dialog — logged for review); firefox select now WORKS
    (content drags deliver); typing unchanged (gate-verified: ctrl+l
    + URL text + autocomplete).
  - Suites: 587 unit, clippy 0.

- **G-H0.10b** gate hardening from the user's hardware-independence
  runs (three more catches):
  1. **"Spatial Mode" added to the context menu** (MenuAction::
     ToggleSpatial) — mouse-only mode toggle: accessibility for a
     user with a stuck/latched modifier, and the gate's S11 no
     longer depends on fragile keyboard delivery. The gate
     right-clicks the FOCUSED window at its CURRENT projected
     position (stale post-drag coordinates hit the background — the
     menu never opened) and clicks item 11 (a one-off miscount
     clicked Close and KILLED the target window — "close sent
     target=VisualId(2)").
  2. **Stuck-modifier diagnosis**: the user's F5 arrived with
     "alt: true" even after --clearmodifiers + blind keyups — the
     minimal Xvfb keymap can bind Super_L onto Mod1, so the "alt"
     was the gate's own Meta+drag leftover, machine-dependently
     mapped. The menu path removes the keyboard from the equation
     entirely; F5 remains as the fallback (with blind releases).
  3. **visual_check.py auto-prefixes http://** for VEYRA_VLM_URL
     (the user's host: "unknown url type: 192.168.68.62").
  - context_menu tests updated (geometry-relative item_at
    expectations; Spatial Mode at index 11, Close at 12).
  - Local: gate 23/0/1 (S11 via the menu ✓), clippy 0, 587 unit.

Real-app bug (foot): its 5 CSD subsurfaces (title bar + 4 borders)
rendered as chrome-only ghosts scattered around the desktop. Two root
causes, both in the #11 subsurface path:
1. **top-left↔center mismatch**: child locals were computed as
   `loc + size/2` (parent top-left frame) but the parent chain composes
   `world = parent_matrix * child_local` with the parent anchored at its
   CENTER — every subsurface displaced by exactly (+pw/2, −ph/2). Fixed
   by `scene::surface_child_local_offset` (unit-tested: foot's exact CSD
   layout), applied to subsurfaces AND IME popups, parent-scale
   compensated.
2. **chrome on presentation children**: the renderer drew ring + title
   strip + buttons on EVERY visual — min-clamped 21px buttons overflowed
   the 5px border subsurfaces (the "ghost buttons"). Chrome (and
   u_edge ring/strip) is now gated on `visual.parent.is_none()`; child
   visuals get `title_bar_height = 0`.
Verified: Xvfb+foot repro clean in normal AND spatial modes (CSD frame
hugs the window and follows rotation); t24 green; fmt/clippy clean.

---

# 25. Commit discipline

Prefer small commits.

Example:

M001 repository skeleton

M002 Smithay initialization

M003 nested backend

M004 first Wayland surface

etc.

Do not combine unrelated milestones.

---

# 26. When uncertain

Do not guess about Smithay APIs.

Check the installed/current Smithay documentation and examples.

Smithay is actively evolving. Do not blindly copy code written for an older major/minor API.

---

# 27. Definition of success

The project is successful when a user can:

1. Start Veyra.
2. Launch normal Linux applications.
3. Use them normally.
4. Enter spatial mode.
5. See application windows in 3D.
6. Navigate the scene.
7. Select a window using spatial picking.
8. Move/rotate windows.
9. Switch workspaces spatially.
10. Return to normal work without losing application state.

Everything else is secondary.