# Veyra — Prioritized Bug List

**Generated**: 2026-08-24
**Scope**: F001–F012 source-code protocol audit
**Classification**: P0 (crash) → P4 (optional protocol gap)

---

## P0 — Compositor crash, deadlock, corrupted global state

*None found.*

---

## P1 — Normal application fundamentally unusable

### B001 — Pointer constraints / relative pointer not implemented
- **Area**: Wayland protocol — `zwp_pointer_constraints_v1`, `zwp_relative_pointer_v1`
- **File**: compositor.rs — not present
- **Description**: Many games and browser FPS/WebGL applications require pointer confinement to function. Without this protocol, first-person camera control is impossible. Chromium-based browsers may refuse to enter fullscreen without pointer lock.
- **Affected**: SDL games, browser FPS games, Unity WebGL
- **Fix difficulty**: Medium (2-3 days, new protocol handler + Smithay integration)
- **Classification rationale**: This is P1 because it makes an entire class of applications (3D games) fundamentally unusable.

### B002 — Keyboard layout hardcoded to US
- **Area**: Input — xkbcommon keymap
- **File**: compositor.rs:249 — `add_keyboard(XkbConfig::default(), 0, 0)`
- **Description**: The keyboard is initialized with the default XkbConfig which uses the US keymap. Non-US keyboard layouts will produce incorrect keysyms. Users with AZERTY, QWERTZ, Dvorak, Colemak, or CJK IME layouts cannot type normally.
- **Affected**: All applications that accept text input (terminals, editors, browsers)
- **Fix difficulty**: Medium (1-2 days, load system xkb keymap from XDG_CONFIG_HOME or /etc/default/keyboard)

---

## P2 — Major feature broken

### B003 — Popup positioning ignores PositionerState
- **Area**: XDG shell — popup positioning
- **File**: compositor.rs:548-549
- **Description**: When `new_popup` is called, the `_positioner: PositionerState` parameter is completely ignored. The popup visual is positioned at a hardcoded offset of `(100, -50, 10)` from the parent visual. This means all context menus, dropdowns, tooltips, and select menus appear in wrong locations.
- **Affected**: All applications using XDG popups (GTK, Qt, Electron, browsers)
- **Fix difficulty**: Medium (parse PositionerState for anchor/gravity/offset, compute correct relative position)
- **Code details**:
  ```
  compositor.rs:1990 — fn new_popup(..., _positioner: PositionerState)
  compositor.rs:548 — visual.transform.position = parent.transform.position + Vector3::new(100.0, -50.0, 10.0)
  ```

### B004 — Selection/Clipboard is a no-op
- **Area**: Wayland protocol — data device selection
- **File**: compositor.rs:2207
- **Description**: `SelectionHandler::new_selection` is implemented as an empty function. When a client sets the selection (copy), no transfer occurs. When another client requests the selection (paste), no data is delivered. Copy/paste across all applications is broken.
- **Affected**: All applications
- **Fix difficulty**: Medium (implement offer lifecycle: accept MIME types, transfer data via fd, handle source destruction)
- **Code details**:
  ```rust
  fn new_selection(&mut self, _ty: SelectionTarget, _source: Option<SelectionSource>, _seat: Seat<Self>) {}
  ```

### B005 — Drag-and-drop handlers are empty
- **Area**: Wayland protocol — data device DnD
- **File**: compositor.rs:2210-2211
- **Description**: `ClientDndGrabHandler` and `ServerDndGrabHandler` are implemented as empty structs with no methods. Drag-and-drop operations are accepted by the protocol but no drag icons, enter/leave/motion/drop events are processed.
- **Affected**: All applications (file managers, browsers, editors, IDEs)
- **Fix difficulty**: High (full DnD lifecycle: drag icon rendering, motion events, drop surface detection, data transfer)
- **Code details**:
  ```rust
  impl ClientDndGrabHandler for LookingGlass {}
  impl ServerDndGrabHandler for LookingGlass {}
  ```

### B006 — Fullscreen protocol not handled
- **Area**: XDG shell — toplevel fullscreen
- **File**: compositor.rs — not present (no `set_fullscreen` override)
- **Description**: Clients can send `set_fullscreen` but there is no handler in `XdgShellHandler`. The compositor ignores the request. Applications that expect fullscreen mode (video players, games, presentation apps) will remain windowed.
- **Affected**: Games, video players, presentation tools
- **Fix difficulty**: Low (detect fullscreen request, send configure with fullscreen state, adjust visual to fill output)

### B007 — DRM backend cannot present frames
- **Area**: Native backend — DRM/KMS presentation
- **File**: drm_backend.rs:130-136
- **Description**: `DrmGraphicsBackend::begin_frame` returns `Ok(())` without binding the EGL context to a DRM framebuffer. `finish_frame` returns `Ok(())` without a page flip. When run with `--native`, the compositor starts but no frames are displayed on the physical monitor.
- **Affected**: DRM/KMS native mode (all applications when running on hardware)
- **Fix difficulty**: High (implement EGL-to-DRM framebuffer binding, page flip handling via DRM event queue, double-buffering)
- **Code details**:
  ```rust
  fn begin_frame(&mut self) -> Result<(), SwapBuffersError> { Ok(()) }
  fn finish_frame(&mut self) -> Result<(), SwapBuffersError> { Ok(()) }
  ```

### B008 — DMA-BUF buffer import not supported
- **Area**: Compositor — buffer handling
- **File**: compositor.rs:484-503
- **Description**: `handle_commit` only handles `BufferAssignment::NewBuffer` and passes it to `import_shm_buffer`. Clients that use `zwp_linux_dmabuf_v1` (hardware video decoding, GPU rendering, browser WebGL) will fail to commit their buffers. This affects browser video playback, Vulkan/GL-rendered content, and hardware-accelerated applications.
- **Affected**: Browsers (video playback), games, GPU-accelerated applications
- **Fix difficulty**: Medium (implement DMA-BUF import path in GlesRenderer)

---

## P3 — Minor compatibility issue

### B009 — No fractional scaling protocol
- **Area**: Wayland protocol — `wp_fractional_scale_manager_v1`
- **File**: Cargo.toml:10-21 — not in smithay features
- **Description**: `wp_fractional_scale_manager_v1` is not enabled in smithay's feature flags. Only integer scale factors (1x, 2x) are supported. HiDPI displays with fractional scaling (125%, 150%, 175%) won't be properly reported to clients.
- **Fix difficulty**: Low (add feature flag, expose global)

### B010 — IME/Text Input protocol not implemented
- **Area**: Wayland protocol — `zwp_input_method_v1`, `zwp_text_input_v3`
- **File**: compositor.rs — not present
- **Description**: Applications that use input method editors (CJK text input, emoji pickers, on-screen keyboards) cannot function. This affects users of East Asian languages, emoji input in any application, and virtual keyboards on tablets.
- **Fix difficulty**: High (complex multi-process protocol)

### B011 — No output hotplug or mode change events
- **Area**: Output — mode/resolution changes
- **File**: compositor.rs:262-268
- **Description**: Output mode is set once at startup (`1280x720@60Hz`) and never updated. Clients receive no output change events if the display resolution changes (e.g., monitor hotplug, display settings change).
- **Fix difficulty**: Low (add output mode change handling)

### B012 — Popup grab accepts without serial validation
- **Area**: XDG shell — popup grab
- **File**: compositor.rs:2010-2019
- **Description**: The `grab` method for popup surfaces accepts the grab without validating the serial against the seat's serial. A malicious or buggy client could grab the pointer indefinitely.
- **Fix difficulty**: Medium (validate serial against seat state)

### B013 — Recovery::recover() is a stub
- **Area**: Session — recovery
- **File**: recovery.rs:20-25
- **Description**: `Recovery::recover()` only logs that recovery is available but does not actually restore any saved state. If the compositor encounters corrupted state, the user's window layout is lost.
- **Fix difficulty**: Low (serialize safe state on each workspace switch, restore on panic/error)

### B014 — Frame callbacks not vblank-synchronized
- **Area**: Rendering — frame timing
- **File**: main.rs:180
- **Description**: The render loop uses a fixed 16ms timer rather than synchronizing to the display's vblank. This can cause frame pacing issues (jitter, uneven frame times). Smithay's `PresentationFeedback` protocol is not implemented.
- **Fix difficulty**: Medium (use Smithay's frame clock or DRM vblank events for timing)

### B015 — No subsurface handling
- **Area**: Compositor — surface role
- **File**: compositor.rs:459-663
- **Description**: `handle_commit` doesn't check whether a surface has a subsurface role. Subsurface content may be double-committed or incorrectly handled as independent surfaces.
- **Fix difficulty**: Low (role check in commit handler)

### B016 — Buffer scale handling is incomplete
- **Area**: Compositor — damage coordinates
- **File**: compositor.rs:493-497
- **Description**: Damage coordinates use `buffer_scale` for conversion but the output only advertises `Scale::Integer(1)`. When running on a HiDPI output (scale 2), damage regions will be incorrectly transformed.
- **Fix difficulty**: Low (propagate output scale through commit pipeline)

---

## P4 — Optional protocol/feature gap (Won't fix now)

| # | Area | Description | When |
|---|------|-------------|------|
| B017 | XWayland | X11 application compatibility via XWayland | Group I |
| B018 | Multi-monitor | Support for multiple independent outputs | Group H |
| B019 | Screen locking | `ext_session_lock_v1` | Future |
| B020 | Idle/Inhibit | `ext_idle_notify_v1`, `zwp_idle_inhibit_v1` | Future |
| B021 | Data control | `zwlr_data_control_manager_v1` (clipboard managers) | Future |
| B022 | Virtual keyboard | `zwp_virtual_keyboard_v1` | Future |
| B023 | Foreign toplevel | `ext_foreign_toplevel_list_v1` | Future |
| B024 | Screenshot | `zwlr_screenshot_v1` | Future |
| B025 | Composition | `zwlr_layer_shell_v1` (panels, notifications) | Future |
| B026 | GTK shell protocol | GTK primary selection protocol | Future |

---

## Summary by Priority

| Priority | Count | Key Issues |
|----------|-------|------------|
| P0 | 0 | — |
| P1 | 2 | Pointer lock (games), keyboard layout (all) |
| P2 | 6 | Popup positioning, clipboard, DnD, fullscreen, DRM presentation, DMA-BUF |
| P3 | 7 | Fractional scaling, IME, output events, serial validation, recovery, frame timing, subsurface, buffer scale |
| P4 | 10 | XWayland, multi-monitor, etc. |
| **Total** | **25** | |

---

## Recommended Execution Order

| Phase | Bugs | Description | Effect |
|-------|------|-------------|--------|
| 1 | B003 | Fix popup positioning | +10-15% compatibility across all toolkits |
| 2 | B002 | Fix keyboard layout | +5% compatibility (non-US users) |
| 3 | B004 | Fix clipboard/selection | +5% compatibility across all apps |
| 4 | B005 | Implement DnD | +5% compatibility (file managers, browsers) |
| 5 | B006 | Implement fullscreen | +3% compatibility (games, video) |
| 6 | B001 | Implement pointer constraints + relative pointer | +5% (games, browser lock) |
| 7 | B008 | Implement DMA-BUF import | +3% (video playback, GPU apps) |
| 8 | B014 | Vblank-synchronized frame callbacks | +2% (smooth video, games) |

Each phase is testably incremental. After Phase 4, terminal + GTK + Qt usability should reach ~85%.
