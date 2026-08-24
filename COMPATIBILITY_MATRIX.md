# Veyra Compatibility Matrix — Group F Audit

**Date**: 2026-08-24
**Scope**: Source-code audit against Wayland protocol specifications
**Method**: Static analysis of compositor source code (no runtime testing)

---

## 1. Per-Application Compatibility Estimates

### 1.1 Terminals (foot, Alacritty, Kitty, WezTerm)

| Feature         | Status | Notes |
|-----------------|--------|-------|
| Map (initial)   | ✅ OK  | XDG toplevel lifecycle works; `send_configure` sent on create |
| Input (keyboard)| ⚠️ P2  | Keyboard input uses winit keycodes → direct Smithay keyboard handler (compositor.rs:1047). No xkbcommon keymap state management — clients may get wrong keysym in non-US layouts |
| Input (pointer) | ✅ OK  | 3D picking → UV → Wayland pointer motion/button (compositor.rs:987-1021) |
| Popups          | ⚠️ P3  | Popup created but position is hardcoded offset (100, -50) rather than using PositionerState (compositor.rs:548-549) |
| Clipboard       | ⚠️ P2  | DataDeviceHandler and SelectionHandler exist but `new_selection` is a no-op (compositor.rs:2207). Copy/paste *may* work through Smithay defaults but no explicit offer transfer |
| DnD             | ⚠️ P3  | `ClientDndGrabHandler` and `ServerDndGrabHandler` are implemented as empty structs (compositor.rs:2210-2211). DnD events accepted but not processed |
| Workspace       | ✅ OK  | Full workspace lifecycle with per-workspace transforms and focus |
| Persistence     | ✅ OK  | Atomic save/load with v1→v2 migration |
| Fullscreen      | ❌ P3  | No fullscreen protocol handling; `set_fullscreen` requests ignored |

**Terminal estimate**: ~70% functional. Keyboard layout issues (P2) and lack of fullscreen (P3) are main gaps.

### 1.2 GTK Applications (GTK3/4: Nautilus, Gedit, Evince, etc.)

| Feature         | Status | Notes |
|-----------------|--------|-------|
| Map (initial)   | ✅ OK  | Standard XDG toplevel |
| CSD             | ✅ OK  | Client-side decorations are the client's responsibility; no interference from compositor |
| SSDO protocol   | ❌ P3  | `ext_session_lock_manager_v1`, `ext_idle_notify_v1` — not implemented. Not critical for basic use |
| Fractional scaling | ❌ P3 | `wp_fractional_scale_manager_v1` — not advertised (no fractional_scale in smithay features) |
| Popups (menus)  | ⚠️ P2  | Hardcoded popup positioning means GTK menus appear offset from parent window (compositor.rs:548-549) |
| Input           | ⚠️ P3  | Same keyboard layout issue as terminals |
| Clipboard       | ⚠️ P2  | `new_selection` is a no-op — GTK clipboard operations may not function |
| DnD             | ⚠️ P3  | DnD grab handlers are empty — file drag-and-drop in Nautilus will not work |
| Workspace       | ✅ OK  | |
| Persistence     | ✅ OK  | |

**GTK estimate**: ~60% functional. Popup positioning (P2) and clipboard (P2) are primary blockers.

### 1.3 Qt Applications (Qt5/Qt6: Dolphin, Kate, Konsole, etc.)

| Feature         | Status | Notes |
|-----------------|--------|-------|
| Map (initial)   | ✅ OK  | Standard XDG toplevel |
| CSD             | ✅ OK  | Qt uses CSD by default |
| Popups (menus)  | ⚠️ P2  | Same hardcoded offset as GTK. Qt popups more sensitive to positioning — may appear in wrong location |
| Input           | ⚠️ P3  | Keyboard layout issues |
| clipboard       | ⚠️ P2  | Qt clipboard offers multiple MIME types; no-op selection handler means copy/paste broken |
| DnD             | ❌ P2  | Empty DnD handlers break Qt drag-and-drop within and between applications |
| Fullscreen      | ❌ P3  | Qt fullscreen requests ignored |
| Wayland-specific protocols | ⚠️ P3 | Qt expects `wp_presentation`, `wp_relative_pointer`, `zwp_pointer_constraints_v1` — none implemented |

**Qt estimate**: ~50% functional. DnD (P2) and clipboard (P2) significantly impact Qt usability.

### 1.4 Electron Applications (VS Code, Slack, Discord, etc.)

| Feature         | Status | Notes |
|-----------------|--------|-------|
| Map (initial)   | ✅ OK  | Works via XDG toplevel |
| CSD             | ⚠️ P3  | Electron uses a mix of CSD/SSD; may need `kde-decoration` protocol (not implemented) |
| Popups (context)| ⚠️ P3  | Hardcoded popup positioning may cause context menus to misalign |
| Input (IME)     | ❌ P3  | `zwp_input_method_v1`, `zwp_text_input_v3` — not implemented. IME input (common in VS Code + CJK) won't work |
| Clipboard       | ⚠️ P2  | Selection handler is no-op |
| DnD             | ❌ P2  | Empty DnD handlers |
| Fullscreen      | ❌ P3  | Not supported |
| Pointer lock    | ❌ P3  | No pointer constraints protocol |
| Chrome-sandbox  | ⚠️ P1  | Electron apps use namespaced sandboxing; may require user namespaces to be available |

**Electron estimate**: ~40% functional. DnD (P2) and clipboard (P2) are major; IME (P3) blocks CJK users.

### 1.5 Browsers (Firefox, Chromium, GNOME Web)

| Feature         | Status | Notes |
|-----------------|--------|-------|
| Map (initial)   | ✅ OK  | |
| Tabs (GTK menus) | ⚠️ P3 | Browser tab context menus use popups; positioning is wrong |
| Fullscreen video | ❌ P3 | No fullscreen protocol — browser fullscreen video won't work |
| Pointer lock    | ❌ P2  | No pointer constraints — FPS games in browser won't get pointer lock |
| Relative pointer| ❌ P3  | No relative pointer protocol — mouse look in browser won't work |
| DnD (tabs, URLs)| ❌ P2  | Drag tabs between windows, drag URLs to desktop — all broken |
| Clipboard       | ⚠️ P2  | Copy/paste in browser won't function reliably |
| Popups (select) | ❌ P3  | `<select>` dropdowns and autocomplete popups use XDG popup — hardcoded position |

**Browser estimate**: ~35% functional. Multiple P2 issues make browsers partially usable for browsing but not for web apps.

### 1.6 SDL/Games

| Feature         | Status | Notes |
|-----------------|--------|-------|
| Map (initial)   | ✅ OK  | XDG toplevel |
| Fullscreen      | ❌ P2  | No fullscreen — games requesting fullscreen mode will get a windowed view or fail |
| Pointer lock    | ❌ P1  | Many games require pointer confinement to function (first-person camera). Without `zwp_pointer_constraints_v1`, these games are unplayable |
| Relative pointer| ❌ P2  | Without `wp_relative_pointer`, mouse look in games has accumulated absolute-position issues |
| Keyboard        | ⚠️ P2  | No raw keyboard protocol — game hotkeys (especially non-US layouts) may not map correctly |
| Frame callbacks | ⚠️ P3  | Frame callbacks work through Smithay's compositor but the compositor's render loop uses a fixed 16ms timer rather than vblank sync (main.rs:180) |

**SDL/Games estimate**: ~20% functional. Pointer lock (P1) is a hard blocker for most games.

### 1.7 XWayland Applications

| Feature         | Status | Notes |
|-----------------|--------|-------|
| XWayland        | ❌ P4  | Not implemented. No xwayland imports anywhere in the codebase |

**XWayland estimate**: 0%. Not implemented. Not a bug — this is a known gap (see AGENTS.md §19).

---

## 2. Priority Bug List

### P0 — Compositor crash, deadlock, corrupted global state

| # | Area | File:Line | Description | Fix Difficulty |
|---|------|-----------|-------------|----------------|
| — | — | — | *No P0 issues found.* | — |

### P1 — Normal application fundamentally unusable

| # | Area | File:Line | Description | Fix Difficulty |
|---|------|-----------|-------------|----------------|
| 1 | Pointer Lock | compositor.rs:17 | `zwp_pointer_constraints_v1` and `zwp_relative_pointer_v1` not implemented. Games (SDL) and browser FPS games are unplayable | Medium (new protocol handler) |
| 2 | Keyboard Layout | compositor.rs:249 | `add_keyboard` uses default XkbConfig (US layout). Non-US keyboard layouts will produce wrong keysyms | Medium (xkbcommon keymap loading) |
| 3 | Wayland socket | — | Wayland socket created once; no reconnection handling for restarted clients | Low |

### P2 — Major feature broken

| # | Area | File:Line | Description | Fix Difficulty |
|---|------|-----------|-------------|----------------|
| 4 | Popup Positioning | compositor.rs:548-549 | Popup position is hardcoded to (100, -50) from parent. `_positioner: PositionerState` is ignored. All menus, dropdowns, and popups appear in wrong location | Medium (parse PositionerState and compute correct offset) |
| 5 | Clipboard | compositor.rs:2207 | `SelectionHandler::new_selection` is a no-op. Copy/paste data never transferred to/from clients | Medium (implement offer lifecycle and data transfer) |
| 6 | DnD | compositor.rs:2210-2211 | `ClientDndGrabHandler` and `ServerDndGrabHandler` are empty structs. Drag-and-drop accepted but events not processed | High (full DnD lifecycle needed) |
| 7 | Fullscreen | compositor.rs | No `set_fullscreen` handler in XdgShellHandler. Fullscreen requests silently ignored | Low (send configure with fullscreen state, adjust visual transform) |
| 8 | Frame Scheduling | main.rs:180 | Fixed 16ms timer instead of vblank-synchronized frame callbacks. May cause tearing or frame pacing issues | Medium (use PresentationFeedback protocol for timing) |
| 9 | GBM/EGL/DRM | drm_backend.rs:130-136 | `begin_frame` and `finish_frame` are no-ops. DRM backend cannot actually present frames | High (page flip and CRTC state management) |

### P3 — Minor compatibility issue

| # | Area | File:Line | Description | Fix Difficulty |
|---|------|-----------|-------------|----------------|
| 10 | Fractional Scaling | Cargo.toml:10-21 | `wp_fractional_scale_manager_v1` not in smithay features. HiDPI (125%/150%/175%) scaling not supported | Low (add feature, expose global) |
| 11 | IME/Text Input | compositor.rs | `zwp_input_method_v1`, `zwp_text_input_v3` not implemented. CJK input broken | High (complex protocol) |
| 12 | Relative Pointer | compositor.rs | `zwp_relative_pointer_v1` not implemented. Mouse-look in games uses absolute coordinates | Medium |
| 13 | Pointer Constraints | compositor.rs | `zwp_pointer_constraints_v1` not implemented. Confinement/lock for games and browser fullscreen | Medium |
| 14 | Output Change Events | compositor.rs:262-268 | Output mode set once, never updated. Clients don't receive output hotplug/resize events | Low |
| 15 | Viewporter | compositor.rs | `wp_viewporter` not implemented. Clients that use viewport scaling won't work | Low |
| 16 | Presentation Feedback | compositor.rs | `wp_presentation` not implemented. Frame timing feedback missing | Medium |
| 17 | Serial Validation | compositor.rs:2010-2019 | Popup grab accepts without serial validation | Medium |
| 18 | Session Recovery | recovery.rs:20-25 | `Recovery::recover()` is a stub that only logs but doesn't actually restore safe state | Low |
| 19 | Buffer Scale | compositor.rs:493-497 | Damage coordinates use buffer_scale but the output only advertises Scale::Integer(1). HiDPI outputs will have incorrect damage regions | Low |
| 20 | XWayland | — | Not implemented. All X11 applications (GIMP, older Qt apps, X11 games) won't run | High (large feature) |
| 21 | Keyboard Focus Reset | compositor.rs:1523-1524 | Workspace switch calls `set_keyboard_focus(ws.focused_id)`. If `focused_id` was saved as `None`, Wayland keyboard focus is correctly cleared. However, there's no mechanism to deliver `wl_keyboard.leave` to the previously-focused surface — Smithay handles this internally through `set_focus` | Low |

### P4 — Optional protocol/feature gap

| # | Area | Description | Fix Difficulty |
|---|------|-------------|----------------|
| 22 | XWayland | Full XWayland integration for X11 app compatibility | High |
| 23 | Multi-monitor | Only single output supported | High |
| 24 | Screen Locking | `ext_session_lock_v1` not implemented | Medium |
| 25 | Idle/Inhibit | `ext_idle_notify_v1`, `zwp_idle_inhibit_manager_v1` not implemented | Medium |
| 26 | Data Control | `zwlr_data_control_manager_v1` not implemented | Medium |
| 27 | Virtual Keyboard | `zwp_virtual_keyboard_v1` not implemented | Medium |
| 28 | Input Method | `zwp_input_method_v1` not implemented | High |
| 29 | Foreign Toplevel | `ext_foreign_toplevel_list_v1` not implemented | Medium |
| 30 | Screenshot | `zwlr_screenshot_manager_v1` not implemented | Low |
| 31 | Gamma Control | `zwlr_gamma_control_v1` not implemented | Low |

---

## 3. Protocol Gaps (Not Implemented)

| Protocol | Importance | Applications Affected |
|----------|-----------|----------------------|
| `zwp_pointer_constraints_v1` | High | Games, browsers, Chromium |
| `zwp_relative_pointer_v1` | High | Games, browser FPS |
| `wp_fractional_scale_manager_v1` | Medium | HiDPI on all apps |
| `wp_viewporter` | Medium | Video players, some toolkits |
| `wp_presentation` | Medium | Frame pacing, video sync |
| `zwp_input_method_v1` / `zwp_text_input_v3` | Medium | CJK/IME input |
| `zwlr_data_control_manager_v1` | Medium | Clipboard managers |
| `kde_server_side_decoration` | Low | Qt SSD mode |
| `ext_session_lock_v1` | Low | Screen lockers |
| `ext_idle_notify_v1` | Low | Idle/inhibit |
| `zwlr_foreign_toplevel_list_v1` | Low | Taskbars, window managers |
| xdg-decoration (server-side) | Low | SSD for apps that request it |

---

## 4. Risks

### Architectural Risks

1. **Winit keycode → keyboard path**: The compositor receives winit `KeyEvent` with platform-dependent keycodes and emits them directly into Smithay's keyboard handler. If winit's keycode mapping differs from what Smithay's `XkbConfig` expects, keyboard input breaks. The current code at `compositor.rs:1047` passes `Keycode::new(key as u32)` without any translation.

2. **SHM-only texture upload**: Buffer import uses `import_shm_buffer` (compositor.rs:508). DMA-BUF textures are not handled — any client using `zwp_linux_dmabuf_v1` will fail to commit. This breaks hardware-accelerated rendering pipelines in browsers, games, and video players.

3. **Fixed 16ms timer vs vblank**: The render loop at `main.rs:180` uses a `Timer` with 16ms duration (~60Hz). This is not synchronized to the display's vblank. The `DrmGraphicsBackend::finish_frame` is a no-op so DRM has no page flip. Frame pacing will be incorrect.

4. **No subsurface support**: The compositor handles `wl_surface.commit` but doesn't check for subsurface role. Subsurfaces from clients (e.g., video overlays in browsers, GTK tooltips) will be treated as independent surfaces.

5. **Output scale is hardcoded to 1**: `Scale::Integer(1)` at `compositor.rs:266`. No dynamic output mode changes, no fractional scaling. All client coordinate mapping assumes 1:1 pixel-to-logical ratio.

### Implementation Risks

6. **Focus vs Selection confusion**: The compositor sometimes uses `selected_id` for keyboard focus routing (compositor.rs:955) and sometimes `focused_id` (compositor.rs:1035). In `route_to_content`, clicking sets focus on the selected visual. In `route_keyboard`, it uses `focused_id`. This dual-track could lead to keyboard events going to a different visual than expected.

7. **Popup lifecycle desync**: When a toplevel is destroyed (`cleanup_visual_permanently`, compositor.rs:2148), popups are cleaned up by matching `parent_toplevel_vid`. However, there's no handling for popup parent (non-toplevel) destruction — a popup that has a parent popup that gets dismissed won't cascade the dismissal.

8. **Wayland surface → VisualId lookup is O(n)**: `find_surface_visual_id` linear-searches through all toplevels and popups. With many windows, this becomes slow on every commit.

9. **No Wayland keyboard focus on hover**: `route_hover` calls `ph.motion()` which implicitly sets pointer focus, but keyboard focus is only updated on click. This is correct Wayland behavior, but the lack of `wl_keyboard.enter` on hover means `wl_data_device` selection offers won't be updated until click.

10. **DRM backend can't actually render**: `DrmGraphicsBackend::begin_frame` returns `Ok(())` without binding the EGL surface to the DRM framebuffer. `finish_frame` returns `Ok(())` without a page flip. The DRM backend is structurally complete but functionally broken for actual display.

---

## 5. Overall Assessment

### Rough Compatibility by Application Class

| Class | Estimate | Primary Blocker |
|-------|----------|-----------------|
| Terminals | ~70% | Keyboard layout (P1), popup positioning (P2) |
| GTK apps | ~60% | Popup positioning (P2), clipboard (P2) |
| Qt apps | ~50% | DnD (P2), clipboard (P2), popup (P2) |
| Electron | ~40% | DnD (P2), clipboard (P2), IME (P3) |
| Browsers | ~35% | Fullscreen (P3), pointer lock (P2), DnD (P2) |
| SDL/Games | ~20% | Pointer lock (P1), fullscreen (P2), relative pointer (P2) |
| XWayland | 0% | Not implemented (P4) |

### Biggest Blocker

**Popup positioning (P2, #4)** is the single most impactful compatibility issue because:
- Every toolkit uses XDG popups for context menus, dropdowns, tooltips, and dialogs
- Hardcoded offset means every menu is misplaced
- Fixing this would immediately improve GTK, Qt, Electron, and browser estimates by 10-15 points

**Selection/Clipboard (P2, #5)** is the second biggest blocker affecting all applications.

### Recommended Fix Order

1. **Popup positioning** (P2) — parse PositionerState, compute correct offset relative to parent
2. **Keyboard layout** (P1) — load system keymap or expose xkb common configuration
3. **Selection/Clipboard** (P2) — implement offer lifecycle and data transfer in SelectionHandler
4. **DnD handlers** (P2) — implement client-side DnD grab processing
5. **Fullscreen** (P3) — handle `set_fullscreen` in XdgShellHandler
6. **Pointer constraints + relative pointer** (P1/P2) — for game support
7. **Fractional scaling** (P3) — for HiDPI support
8. **DMA-BUF import** (P3) — for browser/video/game rendering performance
9. **DRM page flip** (P2) — for native backend to actually work
10. **XWayland** (P4) — for full desktop compatibility

### Test Suite Health

All 344 Group A–E tests pass. The test suite covers:
- Scene (picking, transforms, parenting, stacking)
- Input routing (UV mapping, HID, resize)
- Interaction (ray-plane, NDC)
- Focus (enter/exit, transitions, overview)
- Workspace (switch, save/restore, isolation)
- Session (startup, shutdown)
- Persistence (v2 round-trip, v1 migration, atomic save)
- Scheduler (dirty/animating states)
- Capabilities (report structure)
- Recovery (lifecycle)

The test suite does NOT cover:
- Wayland protocol handler behavior (all handlers are smithay delegates without direct tests)
- Coordinate system conversion between Wayland surface pixels and spatial world units
- Multi-client interaction
- Frame timing/pacing
- Damage region accumulation
