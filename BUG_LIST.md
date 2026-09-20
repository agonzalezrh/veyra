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

### 4. Frame scheduling (no vblank sync) — partially addressed (G-E5 step 1)

**Area**: Rendering
**What's done**: RenderScheduler with dirty/animating state replaces fixed 16ms timer. Idle compositor does not render.
**What's changed (step 1)**: page-flip completions are now EVENT-DRIVEN — a calloop source on the DRM event fd dispatches flip completions and wakes the render loop (PresentationBackend::as_any downcast); begin_frame's poll(0) remains only as a safety-net drain. The flip completion is the vblank tick that future pacing builds on.
**What's missing**: full vsync-based frame PACING (queueing the next flip on flip completion rather than on dirty state); frame pacing may still be incorrect for video/games.

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
| 14 | Multi-monitor | Single output only. **G-E5 audit complete** — single-output assumptions inventoried (see below): `LookingGlass.output` (one wl_output global; sync_output_mode/scale mutate it), `window_size (f32,f32)` consumed ~35× (pointer unproject, overview, layout bounds, taskbar, fullscreen PresentationArea, xwm placement, IME parent fallback), winit backend = one window, DRM backend = one connector/one mode, projection aspect hardcoded 1280/720 in two picking paths (scene.rs:1688, group.rs:380). **Phase 1 DONE (outputs.rs)**: `OutputState`/`OutputManager` registry — mode/scale/global-position state, horizontal row tiling, `output_at_global`/`to_local` hit testing, extents; the live output registers/mirrors via `sync_output_mode`/`sync_output_scale`. **Phase 2**: migrate the ~35 `window_size` consumers onto per-output state; per-output winit window / DRM connector. **Phase 3**: outputs tiling the global desktop plane with per-output camera + unproject hit testing; kill the two hardcoded 1280/720 aspects. Milestone-sized. | High |
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

## #20 (FIXED, regression guard) — winit wheel values were always 0.0; smithay sign convention

Discovered by the live wheel-dolly session (G-H0.2–H0.4, 2026-09-14).

Two independent bugs in the axis path, both invisible to unit tests
because they live at the event-conversion boundary:

1. **LineDelta wheels carry their value in `amount_v120`, not
   `amount`.** smithay's winit backend converts XTEST wheel buttons
   4/5 and physical wheel notches into
   `MouseScrollDelta::LineDelta`, for which
   `PointerAxisEvent::amount()` returns `None` — every dispatch site
   reading `amount(...).unwrap_or(0.0)` saw ZERO for every wheel
   event. Compositor camera zoom AND client scroll were both dead.
   Fix: `amount(..).or_else(|| amount_v120(..))` at BOTH dispatch
   sites (main.rs winit path, native_backend.rs libinput path).

2. **smithay negates LineDelta.** Wheel-up arrives as `-120`
   (`amount_v120 = -y * 120`). The dolly path must normalize the sign
   exactly once — wheel-up approaches the cursor target.

PERMANENT RULE for any future input-path work:
- winit/LineDelta wheel values REQUIRE the amount_v120 fallback;
- the compositor normalizes wheel direction EXACTLY ONCE (at the
  camera path; client forwarding passes the raw v120 value through);
- regression coverage lives in `wheel_zoom_tests` (compositor.rs) via
  the pure `dolly_step` semantics plus the live wheel demos.

## #21 (PARTIALLY FIXED) — chrome: "can't click Restore", dead toolbar clicks, floating menus

User report (2026-09-19): chrome's "Restore pages?" bubble's Restore
button is not clickable; toolbar interactions die. A second report
(add) described a BLACK window appearing when opening the address bar.

### Root cause 1 — FIXED: the multi-client X stacking wedge

Every X11 window mapped by smithay's XWM sits at the SAME root origin
(xdotool: all frames at +2+2 / +20+20 — the XWM never spreads them).
XWayland reconstructs the X cursor as `window_root + surface_local`
and the X core hit-test resolves the button target to the TOPMOST
window at that point. Consequence: **the newest-mapped X11 client ate
every older client's pointer buttons** — with two xev windows, clicks
aimed at client #1 were received by client #2 (live-reproduced); with
chrome + any other X11 client, chrome's own clicks died. Keyboard was
unaffected (protocol-routed, no X hit-testing).

FIX: raise the PICKED window (`X11Wm::raise_window`) before delivering
the button, so the hit-test resolves to the window veyra picked.
Verified: two xevs both receive their own clicks; a click aimed at a
window partially behind another lands on the picked one.

### Root cause 2 — FIXED: X11 transients were mirror-placed

chrome's menus/popups/tooltips map as separate X11 windows positioned
RELATIVE to chrome's own X geometry. veyra mirror-placed every new
visual (H2), so the Alt+F menu floated mid-air at (963,0) (live
capture) instead of hanging under the kebab.

FIX: `x11_transient_anchor` — transient-for owner (or same-client
popup-type fallback) + X-geometry delta (scaled to the owner visual),
owner yaw inherited, +2 z. `configure_notify` re-anchors (chrome moves
menu windows AFTER mapping — the map-time geometry is stale).
Verified: the Alt+F menu renders with its right edge just below the
three-dot button.

### Investigated, NOT fixed: chrome's Views layer ignores compositor-mediated presses

Even with delivery proven pixel-exact (xev receives the exact coords,
1:1, with full [Enter, Motion, Press, Release] streams), chrome's
Views widgets (toolbar buttons, the bubble's Restore) do not react,
while PAGE clicks (WebUI/renderer) do. Discriminating experiments:

- Same coords: a direct XTEST click on chrome's X window (bypassing
  the compositor) opens the kebab menu; the compositor-routed press at
  the identical surface coords does not.
- Wire capture (strace): the working sequence is CORE WarpPointer +
  XTEST press/release with 100ms/50ms spacing.
- A helper process running the identical byte sequence with identical
  spacing WORKS when driven from a shell, and FAILS when the same
  bytes come from veyra's pipe (unresolved; ptrace of the compositor
  is sandbox-blocked).
- Leading theory: chromium's Views layer routes presses by the X
  server's CURSOR POSITION (XQueryPointer), not the event's
  coordinates; Xwayland's wl_pointer delivery never moves the server
  cursor, so every compositor-mediated press resolves to the stale
  cursor position (the window center = the page area) and the toolbar
  never sees it. A hover-time cursor-sync (XTEST warp on pointer move
  over X11 surfaces) was built and moves the cursor correctly, but
  chrome still ignored presses from that context — the residual
  difference (fresh short-lived connection vs compositor-owned
  long-lived connection) is unexplained.

The experimental pipeline (hover cursor-sync via an `xinput-helper`
child process + XTEST press/release, env-gated) was built and exercised
this session but REVERTED from the tree pending a reliable design: it
fixed nothing end-to-end in its final form. The DEFAULT path remains
the proven wl_pointer delivery (page clicks + keyboard verified
end-to-end). The helper's proven-working core (connect + translate +
WarpPointer + 100ms/50ms-paced XTEST press/release) is small and fully
described above for a future attempt.

### The "black window" symptom: not reproduced

On the current build chrome's omnibox dropdown renders correctly in
BOTH modes (in-window Views on X11; a 1086x89 SUBSURFACE on Wayland —
the #11 path). Menus render (the anchoring fix). The reported black
window is consistent with the OLD behavior class (an OR popup mapped
without a rendered buffer, mirror-placed) which the anchoring + this
build's render path no longer produces. Re-test requested on the
user's build (the startup stamp line identifies builds).
