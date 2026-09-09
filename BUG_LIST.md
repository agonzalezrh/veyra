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
| 5 | Fractional Scaling | `wp_fractional_scale_manager_v1` not advertised | Low |
| 6 | IME/Text Input | `zwp_input_method_v1` / `zwp_text_input_v3` not implemented | High |
| 7 | Viewporter | `wp_viewporter` not implemented | Low |
| 8 | Presentation Feedback | `wp_presentation` not implemented | Medium |
| 9 | Output Change Events | Output mode set once, never updated | Low |
| 10 | Buffer Scale | Only Scale::Integer(1) advertised | Low |
| 11 | Subsurface support | Not explicitly handled | Low |
| 12 | Serial Validation | Popup serial validation may not catch all edge cases | Medium |
| 15 | Duplicate Button Press | wl_data_device test client received the same wl_pointer.button(press) twice with one serial (observed under Xvfb/winit; masked by latches so far). Verify whether the compositor double-sends button events to clients — real apps would double-handle clicks. Track the originating layer (winit event loop vs ph.button call sites vs client queue dispatch). | Medium |
| 16 | Stuck META Modifier | After the maximize test sequences (`xdotool keydown super; xdotool key Up; keyup super`), clients observe `logo:true` for the REST of the session — app keybindings like ctrl+shift+v never match. Almost certainly the same duplicated-XTEST-input anomaly as #15 (one unmatched super keydown). Verify duplication at the winit/X layer; consider debouncing modifier presses by serial. | Medium |

## P4: Feature Gaps

| # | Area | Description | Difficulty |
|---|------|-------------|------------|
| 13 | XWayland | Not implemented | High |
| 14 | Multi-monitor | Single output only | High |
| 15 | Data Control | `zwlr_data_control_manager_v1` not implemented | Medium |
| 16 | Foreign Toplevel | `ext_foreign_toplevel_list_v1` not implemented | Medium |

## Recommended Fix Order for G-C

1. **Duplicate Button Press** (P3, #15) — verify/fix double delivery of wl_pointer presses (shared root cause with #16)
2. **Fractional scaling** (P3, #5) — for HiDPI
3. **Output change events** (P3, #9) — for hotplug support
4. **XWayland** (P4, #13) — required for a useful desktop
