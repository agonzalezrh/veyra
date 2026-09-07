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

### 1. Clipboard MIME types (G3 partial)
**Area**: DataDevice / Selection handler
**File**: `src/compositor.rs`
**What's done**: Selection handler wired to Smithay's `set_data_device_selection` and `set_primary_selection`. Data device focus updated on keyboard focus changes.
**What's missing**: `new_selection` passes an empty `vec![]` for MIME types. Paste-receiving clients see the offer but find no compatible MIME types, causing silent paste failure.
**Fix**: Collect MIME types from the `source` parameter and pass them instead of empty vec.

### 2. DnD event processing (G4 stub)

**Area**: DataDevice / DnD
**File**: `src/compositor.rs`
**What's done**: `ClientDndGrabHandler` and `ServerDndGrabHandler` registered (necessary for protocol acceptance).
**What's missing**: Neither trait has method implementations. DnD enters/leaves/motions/drops are not processed.
**Fix**: Implement the DnD grab handler methods (drag enter/motion/leave/drop) + data transfer.

### 3. DRM presentation

**Area**: Native backend
**File**: `src/drm_backend.rs`
**What's done**: `DrmGraphicsBackend<GlesRenderer>` implements `PresentationBackend` trait. Device, connector, mode, EGL context, and GBM surface initialized.
**What's missing**: `begin_frame` returns `Ok(())` without binding EGL surface to DRM framebuffer. `finish_frame` returns `Ok(())` without page flip. No visible output on native DRM.
**Fix**: GBM framebuffer allocation + EGL surface bind + page flip + flip handler.

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

## Recommended Fix Order for G-B Remaining

1. **Clipboard MIME types** (P2, #1) — pass actual MIME types instead of empty vec
2. **DRM presentation** (P2, #3) — page flip implementation for native backend
3. **Fractional scaling** (P3) — for HiDPI
4. **Output change events** (P3) — for hotplug support
5. **Duplicate Button Press** (P3, #15) — verify/fix double delivery of wl_pointer presses
