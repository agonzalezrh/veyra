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
- **G-E5.4 step 1** explicit pointer→output resolution at all four
  input entry points (identity single-output; output-local coords feed
  picking)

Remaining: G-E5.3 per-output cameras (workspace/world shared, view
per-output), G-E5.5 one-winit-window-per-output, G-E5.6 DRM
multi-connector, G-E5.7 multi-monitor integration tests. Exit criteria:
two outputs w/ different resolutions+scales, cross-output picking,
straddling windows, per-output fullscreen/shell/IME, hotplug without
restart — and single-output tests pass unchanged.

**Status: 🟡 G-E5 multi-output in progress (500 unit tests; protocol 111/0/0; input 110/0/0)**

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