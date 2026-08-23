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

The MVP must be demonstrable using ordinary Wayland applications alone. No VMs required.

### KVMFR/Looking Glass KVM

The project also supports loading external frame buffers through the `FrameProducer` abstraction. KVMFR framebuffers from Looking Glass KVM are one such optional provider.

**KVMFR is an optional `FrameProducer`, not the purpose of the compositor.**

The architecture must remain provider-agnostic:

```text
Veyra
   │
   ├── Wayland
   │       │
   │       ▼
   │     Visual
   │
   └── External (KVMFR, etc.)
           │
           ▼
         Visual
           │
           ▼
         Scene
           │
           ▼
        Renderer
```



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

Spatial mode should expose the Looking Glass experience.

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

# 24. Commit discipline

Prefer small commits.

Example:

M001 repository skeleton

M002 Smithay initialization

M003 nested backend

M004 first Wayland surface

etc.

Do not combine unrelated milestones.

---

# 25. When uncertain

Do not guess about Smithay APIs.

Check the installed/current Smithay documentation and examples.

Smithay is actively evolving. Do not blindly copy code written for an older major/minor API.

---

# 26. Definition of success

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