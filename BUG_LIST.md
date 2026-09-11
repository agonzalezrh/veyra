# Veyra Bug List — Post G-A/G-B Re-audit

## P0: None

No crash-level issues found. Chromium 151 runtime-verified against nested Winit/llvmpipe backend.

## P1: None remaining

### ~~Keyboard Layout~~ — ✅ G2 (Fixed)
System XKB config loaded from `/etc/default/keyboard`. Fallback to env vars.

### ~~Pointer Constraints~~ — ✅ G6 (Fixed)
`zwp_pointer_constraints_v1` + `wp_relative_pointer_v1` implemented. Locked/confined pointer skips spatial InteractionController.

### ~~DnD event processing~~ — ✅ G-B2 (Fixed)
Full client-initiated drag-and-drop via Smithay's `DnDGrab`: implicit-grab
start_drag validation, spatially-picked enter/motion/leave/drop, v3 action
negotiation, data transfer, Escape/client-death cleanup. Target selection
reuses the same 3D `pick_wayland_target` path as normal pointer input.
Verified by t22i–t25i (happy path, cancel+reuse, target change + moved
window, source death) in `run_input_tests.sh`.

### ~~1. Clipboard MIME types~~ — ✅ G-B1 (Fixed, G-C1 verified)
Selection source MIME types are passed through to receiving clients.
Verified end-to-end by harness tc1–tc4 (`run_protocol_tests.sh`): multi-MIME
roundtrip with exact offer-set assertion, supported/unsupported request
handling, clipboard replacement (cancel semantics), and source-death
clear. Real-client verification with foot in both directions in
`run_input_tests.sh` (tcfoot).

### ~~3. DRM presentation~~ — ✅ G-B3 (Fixed, hardware-gated)

`begin_frame` now allocates from a GBM swapchain (`GbmBufferedSurface`),
imports the buffer as an EGLImage/RBO/FBO left bound for the raw-GL
pipeline, and `finish_frame` flushes + arms a KMS page flip. Flip
completion events are drained (poll + `frame_submitted`) so the
swapchain retires; pacing becomes vblank-driven. EGL lives on the GBM
platform (same device) because a surfaceless display cannot import
foreign device buffers. gbm's dma-buf export is READ-ONLY — the backend
re-exports through PRIME_HANDLE_TO_FD with DRM_RDWR before import.
**Remaining hardware gate (M079, verified on VKMS + modetest)**:
software rasterizers cannot render into imported dma-bufs (llvmpipe
crashes in Mesa) and the read-only export blocks CPU fills — the M079
capability gate refuses those drivers cleanly with diagnostics
(`VEYRA_DRM_FORCE=1` overrides). Full presentation verification requires
a real GPU; see `tests/harness/runners/run_drm_tests.sh`.

### 4. Frame scheduling (no vblank sync)

**Area**: Rendering
**What's done**: RenderScheduler with dirty/animating state replaces fixed 16ms timer. Idle compositor does not render.
**What's missing**: No vsync-based scheduling. Frame pacing may be incorrect for video/games.

## P3: Remaining

| # | Area | Description | Difficulty |
|---|------|-------------|------------|
| ~~6~~ | ~~IME/Text Input~~ | ✅ **G-E2 (Implemented)**: zwp_text_input_v3 + zwp_input_method_v2 advertised; text-input focus follows keyboard focus; IME keyboard grab fully routed (keymap, repeat, keys). End-to-end loop verified by input-suite t25: focused field enable → IME activate → grab receives injected keys → commit_string("あ"/"漢") → committed_string delivered to the focused surface. Candidate popups (input_popup_surface_v2) now RENDER as visuals anchored to the focused text field's visual (position from smithay's tracked PopupSurface.location, from the text-input cursor rectangle) — G-E4. | ~~High~~ |
| ~~9~~ | ~~Output Change Events~~ | ✅ **G-E2 (Fixed)**: the config file is watched with inotify (event-driven, zero idle wakeups; calloop's signals feature is not enabled). A write triggers Config::load and applies the output scale live — wl_output scale to bound clients + preferred fractional scale to mapped surfaces. Protocol t23 verifies an already-bound client observes scale 2 → 1 with no restart. | ~~Low~~ |
| ~~11~~ | ~~Subsurface support~~ | ✅ **G-E2 (Implemented)**: subsurface commits map as visuals PARENTED to the parent visual (J2 parent-local transforms) with SubsurfaceCachedState positioning and the G-C3 logical-geometry path (viewport dst/src > buffer_scale). Cleanup uses veyra's own sub→parent link (smithay's get_parent is unreliable inside destroy dispatch). Protocol t24 verifies mapping, geometry, and removal-with-parent. | ~~Low~~ |
| 12 | Serial Validation | Popup serial validation may not catch all edge cases | Medium |
| ~~18~~ | ~~Spatial-mode pointer delivery~~ | ✅ **G-E5 (Fixed)**: in spatial mode the pick path correctly unprojects the cursor onto the visual's plane (`screen_to_visual_uv`), but delivery handed smithay `MotionEvent.location = raw screen coords` with `surface_global_origin` — smithay computes surface-local as `location − origin`, which is only valid when screen↔surface is 1:1 (ortho). Under perspective the delivered coordinates drifted up to ~70px from the visual target: clicks landed ~27px above the bookmarks bar (bookmarks unclickable), URL bar clicks missed. Fixed by delivering `location = origin + pos` (the unprojected surface coordinate) in all five delivery sites: route_to_content, route_hover, handle_axis, DnD pointer down/up. Verified live in spatial mode: bookmark clicks navigate (kernel.org, Docs.rs), URL-bar typing navigates (veyra.dev). | Medium |
| ~~19~~ | ~~X11 keyboard focus~~ | ✅ **G-E5 (Fixed)** — two stacked defects: (a) EVERY first-mapped X11 surface — including Firefox's URL-dropdown, menus, tooltips and 1x1 helper windows — took keyboard focus on map ("focus-on-map policy"), stealing the keyboard from the parent app mid-typing. Fix: skip focus-on-map for override-redirect surfaces and EWMH non-focusable window types (DropdownMenu/Menu/PopupMenu/Tooltip/Notification/Utility/Toolbar/Splash). (b) X11 windows NEVER received key events: `set_focus` passed the X11 window's raw wl_surface, bypassing smithay's `KeyboardTarget for X11Surface` (which performs the ICCCM SetInputFocus / WM_TAKE_FOCUS dance) — X input focus stayed on None and XWayland dropped all keys. Fix: `SeatHandler::KeyboardFocus` is now `KeyboardFocusTarget` (WlSurface | X11Surface enum delegating KeyboardTarget), and X11 visuals are focused as X11Surface targets. Verified live: X input focus lands on the Firefox window (`xdotool getwindowfocus`), Ctrl+L focuses the URL bar, typing navigates (example.com). Input 110/0/0, protocol 111/0/0, unit 483/0/1. | Medium |
| ~~15~~ | ~~Duplicate Button Press~~ | ✅ **G-C2 (Verified clean)**: client-kit pointer events now log the wl serial; input-suite t26i asserts exactly one press/release per physical click with distinct serials over repeated runs. No duplication at HEAD — earlier observations are not reproducible; the t26i regression guard keeps watch. | ~~Medium~~ |
| ~~16~~ | ~~Stuck META Modifier~~ | ✅ **G-C2 (Fixed)**: root cause was NOT duplicated input — `route_keyboard` dropped modifier releases when no visual held keyboard focus (focus vanished mid-press: window close, workspace switch, drag start), latching smithay's XKB state so every later client enter reported `logo:true`. Modifiers are seat state: they now always reach the keyboard handle (`feed_keyboard_event`). The previously-SKIPped tcfoot foot-copy flow now runs and passes both directions. | ~~Medium~~ |

## P4: Feature Gaps

| # | Area | Description | Difficulty |
|---|------|-------------|------------|
| ~~13~~ | ~~XWayland~~ | ✅ **G-C4 (Implemented)**: Xwayland spawned at startup (clean degradation when missing); X11Wm drives the X side; windows associate via xwayland-shell-v1 and commit through the same pipeline as native toplevels (chrome, placement, focus-on-map, taskbar). Selections bridge both directions. Known limitations: override-redirect windows unmanaged; X11 move/resize grabs not wired to spatial interaction. | ~~High~~ |
| 14 | Multi-monitor | Single output only. **G-E5 audit complete** — single-output assumptions inventoried (see below): `LookingGlass.output` (one wl_output global; sync_output_mode/scale mutate it), `window_size (f32,f32)` consumed ~35× (pointer unproject, overview, layout bounds, taskbar, fullscreen PresentationArea, xwm placement, IME parent fallback), winit backend = one window, DRM backend = one connector/one mode, projection aspect hardcoded 1280/720 in two picking paths (scene.rs:1688, group.rs:380). Required shape: per-output state map (mode/scale/global position), per-output window_size with one output per winit window / per DRM connector, outputs tiling the global desktop plane with camera + unproject per-output hit testing. Milestone-sized batch. | High |
| 15 | Data Control | ~~not implemented~~ ✅ G-D2 (zwlr + ext advertised and wired) | ~~Medium~~ |
| 16 | Foreign Toplevel | ~~not implemented~~ ✅ G-D3 (ext_foreign_toplevel_list_v1, publish/update/withdraw wired) | ~~Medium~~ |

## Recommended Fix Order for G-E — COMPLETE

All three remaining P3 items shipped in G-E2: #9 (inotify config reload),
#11 (subsurfaces), #6 (IME text-input loop). #12 (popup serial
validation) shipped in G-E3. Remaining known gaps are P4-only:
#14 multi-monitor (High), plus the IME candidate-popup rendering noted
under #6.

### ~~12. Popup serial validation~~ — ✅ G-E3 (Implemented)
xdg_popup.grab now validates the serial against a per-client ledger of
recent INPUT serials (pointer button presses/releases + keyboard events
delivered to that client, capped at 16 entries). Serials from the
future, from other clients, or guessed (configure/frame serials never
enter the ledger) are rejected: veyra logs "popup grab rejected" and
dismisses the popup with popup_done — the toolkit-visible consequence
of a failed grab, without killing the client. Valid grabs are accepted
with "popup grab accepted (serial validated)".
Input-suite t26 (`popups --grab`): odd cycles grab with the serial of
the last REAL button press (accepted), even cycles with a bogus
never-issued serial (rejected -> popup_done delivered to the client).

## Resolution notes: #17 (X11 selection bridge) — RESOLVED in G-E

The "stall" was a chain of test-harness artifacts, not a compositor or
smithay defect:
1. Diagnostic scripts never pinned the Xvfb keyboard focus (no WM!),
   so injected keystrokes went to root — xterm never received the
   typed text, and the double-click selected nothing → xterm took
   ownership of an EMPTY selection → transfers completed with 0 bytes
   (misread as a stall).
2. The harness double-click was one terminal row too low (row 2 is
   blank; row 1 holds the prompt + typed word) and one row hit veyra's
   title strip before that.
3. Data-device selection broadcasts are gated by the primary-selection
   FOCUS client — a paster that bound BEFORE the selection existed
   misses it; the reliable desktop flow (select first, then open/focus
   the paster) delivers the X-owned selection at device bind.

With the focus pinned and the click on the typed word, the FULL bridge
verifies: xterm selection → XWM incoming transfer → Wayland client
receives the payload (t27i part B green). xterm→xterm roundtrip also
verified ("hello-worldhello-world").
