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
| 6 | IME/Text Input | `zwp_input_method_v1` / `zwp_text_input_v3` not implemented | High |
| 9 | Output Change Events | ~~Mode set once~~ R11 mode sync + G-D1 config scale verified; runtime scale *changes* (config reload) have no trigger yet | Low |
| 11 | Subsurface support | Not explicitly handled | Low |
| 12 | Serial Validation | Popup serial validation may not catch all edge cases | Medium |
| 17 | X11 selection data transfer | G-C4/G-D5 bridge: Wayland→X fully verified (xterm pastes compositor clipboard, t27i visual); X→Wayland publish verified (offers + mimes reach clients). The final X→Wayland data FETCH from the X selection owner stalls inside smithay's XWM transfer machinery (receive requested via zwp_primary_selection_offer.receive; no data arrives). Investigate smithay's XWM PendingTransfer flow for fetch-from-X-owner. | Medium |
| ~~15~~ | ~~Duplicate Button Press~~ | ✅ **G-C2 (Verified clean)**: client-kit pointer events now log the wl serial; input-suite t26i asserts exactly one press/release per physical click with distinct serials over repeated runs. No duplication at HEAD — earlier observations are not reproducible; the t26i regression guard keeps watch. | ~~Medium~~ |
| ~~16~~ | ~~Stuck META Modifier~~ | ✅ **G-C2 (Fixed)**: root cause was NOT duplicated input — `route_keyboard` dropped modifier releases when no visual held keyboard focus (focus vanished mid-press: window close, workspace switch, drag start), latching smithay's XKB state so every later client enter reported `logo:true`. Modifiers are seat state: they now always reach the keyboard handle (`feed_keyboard_event`). The previously-SKIPped tcfoot foot-copy flow now runs and passes both directions. | ~~Medium~~ |

## P4: Feature Gaps

| # | Area | Description | Difficulty |
|---|------|-------------|------------|
| ~~13~~ | ~~XWayland~~ | ✅ **G-C4 (Implemented)**: Xwayland spawned at startup (clean degradation when missing); X11Wm drives the X side; windows associate via xwayland-shell-v1 and commit through the same pipeline as native toplevels (chrome, placement, focus-on-map, taskbar). Selections bridge both directions. Known limitations: override-redirect windows unmanaged; X11 move/resize grabs not wired to spatial interaction. | ~~High~~ |
| 14 | Multi-monitor | Single output only | High |
| 15 | Data Control | ~~not implemented~~ ✅ G-D2 (zwlr + ext advertised and wired) | ~~Medium~~ |
| 16 | Foreign Toplevel | ~~not implemented~~ ✅ G-D3 (ext_foreign_toplevel_list_v1, publish/update/withdraw wired) | ~~Medium~~ |

## Recommended Fix Order for G-E

1. **X11 selection data transfer** (P3, #17) — complete the X→Wayland clipboard loop
2. **Output change events, runtime half** (P3, #9) — config-reload hook for scale changes
3. **Subsurface support** (P3, #11) — Chromium/Qt menus
4. **IME/Text Input** (P3, #6) — CJK input for real desktop use
