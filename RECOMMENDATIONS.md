# Veyra Code Review Recommendations

Review date: 2026-09-08

## Addendum — G-C → G-E campaign status (2026-09-12)

The September 8–10 review's residuals were largely consumed by the G-C/G-D/G-E
compatibility campaigns (see COMPATIBILITY_MATRIX.md §3 for the mapping and
AGENTS.md §24 for the milestone log). State as of G-E5:

- Clipboard/DnD/IME/XWayland/fractional-scale/presentation-feedback/subsurface
  gaps from the original review: all implemented and suite-verified.
- P2 #9 remainder (raw-pointer EGL surface-rebinding + `expect()` panic):
  **still open** — scheduled as G-F3 (safe presentation API); not forgotten.
- Pointer-constraints harness client: still absent (structural + protocol
  coverage exists; no dedicated harness client).
- Real-GPU native validation: still outstanding (G-F1) — VKMS/llvmpipe
  cannot rasterize imported dma-bufs.
- New platform focus adopted post-audit: multi-output architecture
  (G-E5 phases 1–3; phase 1 `outputs.rs` registry landed), event-driven
  DRM flip dispatch (#4 step 1), GL context-loss recovery, projection
  NaN guard, producer failure thresholds, popup serial hardening (#12),
  window-model extraction into window.rs, taskbar shell polish.

Next recommended sequence: G-E5.2–G-E5.7 (multi-output), G-F1–G-F5
(native hardening), then G-G/G-H/G-I per AGENTS.md §24.

## Remediation status (2026-09-08, post-review campaign)

All findings addressed in commits R1-R13 (79a65c4..c546355):

| Finding | Status | Commit |
|---------|--------|--------|
| P1 #1 frame lifecycle | FIXED — render_scene draw-only; finish errors reach the frame owner | 79a65c4 |
| P1 #2 --native | FIXED — native-first startup (winit never initialized in native mode); libseat session owns the DRM device; libinput-over-udev feeds the same input methods as winit; session (de)activation gates presentation; clean refusal + winit fallback. Verified on VKMS (session, session-owned device open, KMS mode adoption, libinput wiring, loop runs); rendering on real GPUs requires hardware (M079 gate refuses software stacks cleanly) | G-B3 + this commit |
| P1 #3 persistence identity | FIXED — chrome.app_id identity, consuming take for duplicate app_ids, saved-workspace restore, camera sync | 59a4a14 |
| P1 #4 dmabuf cache | FIXED — cache removed; smithay's weak-keyed per-frame cleanup owns lifetime | 53e4171 |
| P1 #5 group transforms | FIXED — one world-transform API (group * chain * local) used by render/pick/bounds/input; arrangement moves the GROUP transform; membership validation | f92bc6d |
| P2 #6 fixed 16ms poll | FIXED — ping-driven immediate renders + self-disarming 16ms pacer; idle = no renders, no timer wakeups | 14d1211 |
| P2 #7 pointer constraints | FIXED — focus-gated activation, real protocol deactivation, confinement enforced (region clamp) | faf347c |
| P2 #8 workspace destruction | FIXED — visuals rehomed (membership, transforms, detached, focus) into a surviving workspace | b45e60e |
| P2 #9 global GL caches + stderr | MOSTLY FIXED — production eprintln removed (R9); DrawGl/font-atlas caches now context-owned (RenderCaches in LookingGlass, reset on all three context-loss paths, statics deleted). Remaining: the raw-pointer surface-rebinding workaround and its expect() panic still need the safe presentation-API restructure | 1c93a10, 0e284af |
| P2 #10 SystemTime | FIXED — Instant-anchored now_ms | 1c93a10 |
| P2 #11 output mode sync | FIXED — output handle stored; mode synced at startup, on resize, and on --native | 0324dcf |
| P3 #12 decoration conversion | FIXED — Visual::title_bar_fraction single API; all call sites converted | 6b03546 |
| P3 #13 quality gates | FIXED — clippy --workspace --all-targets -D warnings green; cargo fmt --check clean; tautologies removed; shallow capability tests annotated where they document API surface | ffb9c30, c546355 |

Known residual gaps (honest): no mock PresentationBackend unit test of
the frame contract (smithay's DummyRenderer cannot satisfy the
GlesRenderer-typed backend; a structural guard test covers the
invariant instead); no harness client exercises zwp_pointer_constraints;
no automated advertised-mode-vs-projection resize comparison;
--native rendering on REAL hardware is validated only up to the M079
gate in this environment (VKMS + llvmpipe cannot rasterize imported
dma-bufs); the raw-pointer EGL surface-rebinding workaround in
render_scene still needs the safe presentation-API restructure
(P2 #9 remainder).

## Scope

This review covered the compositor state and Wayland handlers, rendering and
presentation backends, spatial scene/workspace state, persistence, input, and
the integration-test harness. Findings below are concrete implementation
issues, rather than planned protocol support such as XWayland, IME, or
fractional scaling.

## P1 - Fix Before Claiming Desktop Reliability

### 1. Give one layer sole ownership of the frame lifecycle

**Files:** `src/compositor.rs:1279-1312`, `src/renderer.rs:891-892`,
`src/renderer.rs:1204-1211`

`LookingGlass::render` calls `begin_frame` and `finish_frame`, while
`renderer::render_scene` calls both again. A Winit frame is therefore made
current and submitted twice. `render_scene` also converts non-context-loss
submission failures into `Ok(())`, hiding presentation failures from its
caller.

Make `render_scene` draw-only and leave begin/finish plus error propagation in
`LookingGlass::render` (or move all three operations into `render_scene`, but
not both). Add a mock `PresentationBackend` test that asserts exactly one begin
and one finish per requested frame and verifies that every finish error reaches
the caller.

### 2. Do not expose `--native` until it can present and accept native input

**Files:** `src/main.rs:96-140`, `src/drm_backend.rs:63-79`,
`src/drm_backend.rs:147-163`, `src/native_backend.rs:19-49`

The native path always initializes Winit first, then replaces its backend when
`--native` succeeds. The DRM backend creates a surfaceless GLES renderer;
`begin_frame` has no render target, `finish_frame` never queues a page flip,
and `egl_surface` is `None`. `native_backend::run_native` is an unused stub,
so there is no libinput/session event loop either. A successful `--native`
startup currently cannot display or interact with a desktop.

Either fail `--native` clearly until DRM/KMS is complete, or implement a
separate native startup path with session-managed DRM FD acquisition, GBM
swapchain allocation, EGL binding to each acquired buffer, page-flip handling,
libinput dispatch, and output state updates. Gate it on a real capability probe
and validate it on hardware with a smoke test that observes a page flip.

### 3. Repair persistence identity and restore the recorded workspace state

**Files:** `src/persist.rs:86-111`, `src/persist.rs:150-176`,
`src/persist.rs:133-141`, `src/compositor.rs:549-574`,
`src/compositor.rs:853-893`

Capture uses `v.decoration.title` as the application identity, but toplevel
mapping populates `visual.chrome.title` and `visual.chrome.app_id`, not
`decoration.title`. Normal Wayland visuals are therefore filtered out of every
saved workspace. Even if a saved entry exists, mapping discards the workspace
index returned by `find_visual` and always adds the visual to the active
workspace. Finally, `capture_multi` accepts the current camera but never uses
it, so the active workspace camera can be stale at shutdown.

Persist a stable launch/match key separately from display title, record an
instance discriminator for duplicate app IDs, and restore membership using the
returned workspace index. Snapshot the active camera into its workspace before
capture. Add end-to-end tests for a title-changing client, two instances of one
app, transforms in multiple workspaces, and a camera move immediately before
shutdown.

### 4. Bound DMA-BUF texture ownership to buffer lifetime

**Files:** `src/dmabuf.rs:12-16`, `src/dmabuf.rs:46-72`,
`src/compositor.rs:4608-4614`

Every imported DMA-BUF is appended to `DmabufManager::textures`, but the cache
is never read or evicted and `buffer_destroyed` does nothing. A client that
allocates replacement DMA-BUFs can make the compositor retain both file
descriptors and GPU textures indefinitely. The commit path also imports the
buffer again through `ImportAll`.

Use one texture ownership path. Associate imported resources with the
`WlBuffer` and release them in `buffer_destroyed`, or rely on Smithay's
per-surface import cache and remove this vector. Add a lifecycle test that
creates, commits, destroys, and repeats DMA-BUF buffers while asserting that
the tracked resource count returns to zero.

### 5. Integrate group transforms into rendering, picking, and arrangement

**Files:** `src/group.rs:174-185`, `src/renderer.rs:963-975`,
`src/scene.rs:795-828`, `src/arrange.rs:178-203`

`world_matrix_with_groups` is the only method that applies a group transform,
but the renderer uses `world_matrix`, picking uses `world_transform`, and input
uses the same non-group path. Arrangement represents a group by its first
member and `apply_arrangement` moves only that member. Changing a group
transform therefore has no visible or interactive effect on the group.

Establish one scene-world-transform API that composes both parent and group
relationships, then use it at every renderer, picker, input, and bounds call
site. Arrange groups by updating their group transform rather than a member
transform. Reject nonexistent/duplicate/multi-group membership unless those
semantics are explicitly supported. Add a test with real `Visual` entries that
checks group translation, rotation, picking, and arrangement move all members
together.

## P2 - Address Next For Correctness and Long-Running Stability

### 6. Make scheduling genuinely demand-driven and presentation-timed

**Files:** `src/main.rs:220-231`, `src/compositor.rs:1118-1137`,
`src/scheduler.rs:37-50`

The permanent 16 ms timer calls `render` regardless of
`RenderScheduler::needs_render`; `render` clears the scheduler but renders
unconditionally. This is fixed-rate polling, not the documented dirty/animation
scheduler, and it prevents an idle compositor from reaching the stated
near-zero CPU goal. It also has no presentation/vblank feedback.

Arm a timer only while an animation needs its next tick. For regular surface
updates, request a frame from the active presentation backend and render in its
frame-ready callback. Guard `render` with scheduler state while still tracking
pending Wayland frame callbacks explicitly. Add an integration test that proves
idle time produces neither render calls nor timer wakeups, while a frame
callback and an animation each make bounded progress.

### 7. Implement pointer constraints according to focus and region semantics

**Files:** `src/pointer_constraints.rs:28-64`,
`src/compositor.rs:1362-1372`, `src/compositor.rs:3070-3095`

`new_constraint` activates locked and confined constraints immediately,
regardless of pointer focus. `unlock` only changes local booleans and does not
deactivate the Smithay constraint. Confinement is never tracked or enforced;
only locked pointers alter routing. This lets an unfocused client affect global
pointer behavior and leaves stale protocol state after focus changes.

Activate/deactivate constraints as pointer focus enters/leaves the owning
surface, honor the client-supplied region for confinement, and ensure focus
changes, surface destruction, and Escape all deactivate the protocol object.
Test locked and confined constraints before focus, during focus transitions,
and after client destruction.

### 8. Preserve or deliberately rehome visuals when a workspace is destroyed

**Files:** `src/compositor.rs:3507-3529`, `src/workspace.rs:176-194`

Destroying a workspace removes only its `Workspace` record. Its visuals remain
alive in the global scene but no remaining workspace owns them, so they are not
rendered, pickable, taskbar-visible, or reachable through workspace navigation.
The comment claiming visual state is cleaned up is misleading.

Before removing a workspace, move its visuals, detached state, and a valid
focus target to a chosen destination workspace (normally the active fallback),
or close them through a documented lifecycle policy. Add tests that destroy
both active and inactive populated workspaces and verify every live visual has
exactly one valid membership afterwards.

### 9. Keep renderer caches scoped to a GL context and remove production stderr

**Files:** `src/renderer.rs:40-45`, `src/renderer.rs:817-823`,
`src/renderer.rs:894-915`, `src/renderer.rs:1060`, `src/renderer.rs:1098`

`DRAW_GL` and `FONT_ATLAS` are process-global raw GL handles. They are not
invalidated on context loss, backend replacement, or multiple compositor
instances, so a later context can use stale object IDs. The raw pointer rebinding
workaround and `expect` make context failure a process panic. Two unconditional
`eprintln!` calls also emit per-frame diagnostics in production.

Make cached GPU objects owned by a renderer-context struct and drop/reset them
with the context. Replace the raw-pointer rebinding protocol with a safe
presentation API that provides a current drawable context. Return rendering
errors instead of panicking and convert diagnostics to opt-in tracing. Test
cache reinitialization with a renderer test double or a headless EGL context.

### 10. Use a monotonic Wayland timestamp source

**File:** `src/compositor.rs:349-355`

`now_ms` is documented as monotonic but uses `SystemTime`. NTP or manual clock
changes can move event and frame-callback times backwards; truncation to `u32`
also wraps about every 49.7 days.

Base timestamps on a process-start `Instant`, converting elapsed time to the
Wayland-required `u32` representation. Unit-test monotonicity using an injected
clock or a timestamp helper that handles wrap semantics.

### 11. Synchronize the advertised output with the actual backend size

**Files:** `src/compositor.rs:384-404`, `src/compositor.rs:426`,
`src/main.rs:239-245`, `src/compositor.rs:4375`

The output is created with a fixed 1280x720 mode, is not stored in
`LookingGlass.output`, and is never updated after Winit resize or native mode
selection. The empty output handler cannot propagate output lifecycle changes.
Clients can make geometry decisions from stale output metadata while rendering
and input use a different framebuffer size.

Keep the output handle as compositor state, initialize it from the selected
backend, and update mode/scale plus notify clients on every backend resize or
DRM hotplug. Add a nested-backend resize integration test that compares
advertised output mode, projection dimensions, and pointer mapping.

## P3 - Quality and Coverage Improvements

### 12. Centralize decoration coordinate conversion

**Files:** `src/scene.rs:234-263`, `src/compositor.rs:1667-1729`,
`src/compositor.rs:3181-3196`, `src/compositor.rs:3225-3235`

The title-bar fraction is derived correctly in some call sites as
`height / (1 + height)`, but `Visual::hit_title_bar` and `content_uv` use the
content-relative height directly. Other paths hardcode `0.06 / 1.06` instead of
using the visual decoration. Custom chrome height therefore makes draw, hit
testing, surface origin, and input UV conversion disagree.

Give `Visual` a single full-quad-to-content conversion API and use it in
renderer-independent input, hit testing, and pointer focus origin calculation.
Test default and non-default title heights at the boundaries.

### 13. Turn warnings and shallow tests into enforced quality gates

**Files:** `src/persist.rs:133-141`, `src/capabilities.rs:174-180`,
`src/anchor.rs:113-133`, `src/group.rs:203-211`, `tests/harness/`

`cargo test --workspace` passes 453 tests but emits 124 compiler warnings. The
strict Clippy command currently fails first in the harness with 17 errors. Some
tests are tautological or exercise no real state: the capability probe always
returns an error when asked to construct a renderer, the capability test only
asserts a value equals itself, and several anchor/group tests do not add a
`Visual` to the scene.

Fix the existing warnings, add CI for `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`, then
replace placeholder tests with behavioral tests using real scene entries and a
mock presentation backend. Keep hardware-dependent tests explicitly separated
from unit tests rather than treating structural coverage as runtime validation.

## Verification Performed

- `cargo test --workspace`: passed, 453 passed and 1 ignored; emitted 124
  warnings.
- `cargo clippy --workspace --all-targets -- -D warnings`: failed in the
  `client-kit` harness with 17 diagnostics, including unused imports/fields,
  an unreachable pattern, and Clippy findings.
