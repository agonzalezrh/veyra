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
(never owns them), all Wayland clients remain unaware of the 3D desktop."

## Group D — Production Architecture (M074–M080)

Renderer abstraction audit, DRM/KMS production path, GPU capability
detection, multi-display architecture, rendering performance, frame
scheduling and damage tracking, long-running stability soak tests.
No new spatial UX features in Group D — stabilize, measure, harden.

**Note**: `/dev/dri/cardN` existence does not imply usable 3D rendering.
EGL/GLES capability detection is required before committing to native
rendering.

**Status: ⬜ Not started**

Exit criteria: Wayland/spatial/renderer layers cleanly separated,
multi-display architecture defined, damage tracking + frame scheduling,
benchmark suite, DRM native session works reliably with clean failure
diagnostics, long-running soak test, 240+ tests."

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