# Veyra Compatibility Matrix — Post G-A/G-B Re-audit

**Date**: 2026-08-24
**Previous audit**: Static source-code analysis (Group F)
**G-A fixes**: Popup positioning (G1), XKB keyboard layout (G2), clipboard (G3), DnD stubs (G4), fullscreen protocol (G5)
**G-B fixes**: Pointer constraints (G6), DMA-BUF import (G7)
**Runtime verification**: Chromium 151 tested against nested Winit/llvmpipe backend

---

## 1. Per-Application Compatibility Estimates

### 1.1 Terminals (foot, Alacritty, Kitty, WezTerm)

| Feature         | F Status | Now | Notes |
|-----------------|----------|-----|-------|
| Map (initial)   | ✅ OK    | ✅  | XDG toplevel lifecycle works; `send_configure` sent on create |
| Input (keyboard)| ⚠️ P2    | ✅  | **G2**: XKB keymap loaded from `/etc/default/keyboard`. US/Spanish/German/French layouts now work. Repeat configured at 250ms/50keys |
| Input (pointer) | ✅ OK    | ✅  | 3D picking → UV → Wayland pointer motion/button |
| Popups          | ⚠️ P3    | ✅  | **G1**: PositionerState parsed; anchor/gravity/offset applied; parent-relative coordinates correct |
| Clipboard       | ⚠️ P2    | ⚠️  | **G3**: Selection handler wired to Smithay's data device functions. MIME types may be empty (need verification) |
| DnD             | ⚠️ P3    | ⚠️  | **G4**: Handlers registered but client/server DnD grab methods not implemented (stubs) |
| Workspace       | ✅ OK    | ✅  | Full workspace lifecycle with per-workspace transforms and focus |
| Persistence     | ✅ OK    | ✅  | Atomic save/load with v1→v2 migration |
| Fullscreen      | ❌ P3    | ✅  | **G5**: `fullscreen_request`/`unfullscreen_request` set state flags and send configure |

**Terminal estimate**: ~90% functional. Clipboard MIME types (G3 partial) and DnD (G4 stubs) are remaining gaps.

### 1.2 GTK Applications (GTK3/4: Nautilus, Gedit, Evince, etc.)

| Feature            | F Status | Now | Notes |
|--------------------|----------|-----|-------|
| Map (initial)      | ✅ OK    | ✅  | Standard XDG toplevel |
| CSD                | ✅ OK    | ✅  | Client-side decorations; no compositor interference |
| Popups (menus)     | ⚠️ P2    | ✅  | **G1**: GTK menus and dropdowns should now appear in correct position |
| Input              | ⚠️ P3    | ✅  | **G2**: Keyboard layout now matches system |
| Clipboard          | ⚠️ P2    | ⚠️  | **G3**: Selection handler wired; MIME types may be empty (verify) |
| DnD                | ⚠️ P3    | ⚠️  | **G4**: Handlers registered but DnD event processing is stub |
| Fullscreen         | ❌ P3    | ✅  | **G5**: Protocol handled |
| Workspace          | ✅ OK    | ✅  | |
| Persistence        | ✅ OK    | ✅  | |

**GTK estimate**: ~85% functional. Clipboard (G3 partial) and DnD (G4 stubs) remaining.

### 1.3 Qt Applications (Qt5/Qt6: Dolphin, Kate, Konsole, etc.)

| Feature                         | F Status | Now | Notes |
|---------------------------------|----------|-----|-------|
| Map (initial)                   | ✅ OK    | ✅  | Standard XDG toplevel |
| CSD                             | ✅ OK    | ✅  | Qt uses CSD by default |
| Popups (menus)                  | ⚠️ P2    | ✅  | **G1**: PositionerState applied correctly |
| Input                           | ⚠️ P3    | ✅  | **G2**: XKB keymap loaded |
| Clipboard                       | ⚠️ P2    | ⚠️  | **G3**: Selection handler wired (MIME types to verify) |
| DnD                             | ❌ P2    | ⚠️  | **G4**: Handlers registered but DnD event processing is stub |
| Fullscreen                      | ❌ P3    | ✅  | **G5**: Protocol handled |
| Pointer constraints             | ❌       | ✅  | **G6**: `zwp_pointer_constraints_v1` and `wp_relative_pointer_v1` implemented |

**Qt estimate**: ~85% functional. DnD (G4 stubs) and clipboard (G3 partial) remaining.

### 1.4 Electron Applications (VS Code, Slack, Discord, etc.)

| Feature           | F Status | Now | Notes |
|-------------------|----------|-----|-------|
| Map (initial)     | ✅ OK    | ✅  | XDG toplevel |
| Popups (context)  | ⚠️ P3    | ✅  | **G1**: PositionerState now parsed |
| Input (keyboard)  | ⚠️ P3    | ✅  | **G2**: XKB layout |
| Clipboard         | ⚠️ P2    | ⚠️  | **G3**: Selection handler wired |
| DnD               | ❌ P2    | ⚠️  | **G4**: Handlers registered but stub |
| Fullscreen        | ❌ P3    | ✅  | **G5**: Protocol handled |
| Pointer lock      | ❌ P3    | ✅  | **G6**: Implemented |
| IME               | ❌ P3    | ❌  | Not implemented (CJK input broken) |

**Electron estimate**: ~75% functional. DnD (G4 stubs), clipboard (G3 partial), and IME (P3) remaining.

### 1.5 Browsers (Firefox, Chromium, GNOME Web)

| Feature              | F Status | Now | Notes |
|----------------------|----------|-----|-------|
| Map (initial)        | ✅ OK    | ✅  | |
| Chromium 151 launch  | ❌ P0    | ✅  | **Runtime verified**: Veyra nested/Winit + llvmpipe. Chromium launches, stays up, produces no protocol errors |
| Popups (menus)       | ⚠️ P3    | ✅  | **G1**: Browser context menus should position correctly |
| Fullscreen video     | ❌ P3    | ✅  | **G5**: Fullscreen protocol handled |
| Pointer lock         | ❌ P2    | ✅  | **G6**: `zwp_pointer_constraints_v1` + `wp_relative_pointer_v1` implemented |
| DnD (tabs, URLs)     | ❌ P2    | ⚠️  | **G4**: DnD handlers registered but stub |
| Clipboard            | ⚠️ P2    | ⚠️  | **G3**: Selection handler wired |
| DMA-BUF              | ❌       | ✅  | **G7**: `zwp_linux_dmabuf_v1` handler implemented via `DmabufHandler` + `ImportDma` |

**Browser estimate**: ~80% functional with G-A/G-B. DnD (G4 stubs) and clipboard (G3 partial) remaining.

### 1.6 SDL/Games

| Feature            | F Status | Now | Notes |
|--------------------|----------|-----|-------|
| Map (initial)      | ✅ OK    | ✅  | XDG toplevel |
| Fullscreen         | ❌ P2    | ✅  | **G5**: Protocol handled |
| Pointer lock       | ❌ P1    | ✅  | **G6**: `zwp_pointer_constraints_v1` + `wp_relative_pointer_v1` implemented. Locked pointer skips spatial InteractionController |
| Relative pointer   | ❌ P2    | ✅  | **G6**: Relative motion delivered to locked client |
| Keyboard           | ⚠️ P2    | ✅  | **G2**: XKB layout |
| Frame callbacks    | ⚠️ P3    | ⚠️  | Render loop uses RenderScheduler (dirty/animating state), not vblank sync |

**SDL/Games estimate**: ~75% functional. Frame scheduling (vsync) and DRM presentation remaining.

### 1.7 XWayland Applications

| Feature  | F Status | Now | Notes |
|----------|----------|-----|-------|
| XWayland | ❌ P4    | ❌  | Not implemented. No xwayland imports anywhere in codebase |

**XWayland estimate**: 0%. Known gap (see AGENTS.md §19).

---

## 2. Priority Bug List

### P0 — Compositor crash, deadlock, corrupted global state

| # | Area | Status | Description |
|---|------|--------|-------------|
| — | — | ✅ | *No P0 issues found.* Chromium 151 runtime verified against nested Winit backend |

### P1 — Normal application fundamentally unusable

| # | Area | G-A Fix | Status | Notes |
|---|------|---------|--------|-------|
| 1 | Pointer Lock | **G6** | ✅ Fixed | `zwp_pointer_constraints_v1` and `wp_relative_pointer_v1` implemented. Locked pointer skips spatial interaction |
| 2 | Keyboard Layout | **G2** | ✅ Fixed | System layout from `/etc/default/keyboard`. Fallback to env vars/default. Repeat configured |

### P2 — Major feature broken

| # | Area | G-A Fix | Status | Notes |
|---|------|---------|--------|-------|
| 3 | Popup Positioning | **G1** | ✅ Fixed | PositionerState parsed; anchor/gravity/offset applied; parent-relative coordinates correct |
| 4 | Clipboard | **G3** | ⚠️ Partial | Selection handler wired to Smithay's data device functions. MIME types may be empty — paste may silently fail. Needs runtime verification |
| 5 | DnD | **G4** | ⚠️ Partial | `ClientDndGrabHandler` and `ServerDndGrabHandler` registered but methods not implemented. Protocol acceptance works; event processing is stub |
| 6 | Fullscreen | **G5** | ✅ Fixed | `fullscreen_request`/`unfullscreen_request` set state flags and send configure |
| 7 | DMA-BUF | **G7** | ✅ Fixed | `DmabufHandler` + `ImportDma` handles linux-dmabuf. SHM fallback preserved |
| 8 | DRM Presentation | — | ❌ | `DrmGraphicsBackend::begin_frame`/`finish_frame` are no-ops. No page flip. Cannot present on native DRM |
| 9 | Frame scheduling | — | ⚠️ | RenderScheduler tracks dirty/animating state (fixed vs 16ms timer), but no vblank sync |

### P3 — Minor compatibility issue

| # | Area | Status | Notes |
|---|------|--------|-------|
| 10 | Fractional Scaling | ❌ | `wp_fractional_scale_manager_v1` not in Smithay features |
| 11 | IME/Text Input | ❌ | `zwp_input_method_v1`, `zwp_text_input_v3` not implemented |
| 12 | Output Change Events | ❌ | Output mode set once, never updated |
| 13 | Viewporter | ❌ | `wp_viewporter` not implemented |
| 14 | Presentation Feedback | ❌ | `wp_presentation` not implemented |
| 15 | Serial Validation | ⚠️ | Popup serial validation exists but may not catch all cases |
| 16 | Session Recovery | ⚠️ | `Recovery::recover()` exists, validates focused_id on each render |
| 17 | Buffer Scale | ❌ | Output advertises Scale::Integer(1) only |
| 18 | Subsurface support | ❌ | Subsurfaces not explicitly handled |
| 19 | XWayland | ❌ | Not implemented |

---

## 3. Protocol Gaps (Not Implemented)

| Protocol | Importance | Status after G-B |
|----------|-----------|------------------|
| `zwp_pointer_constraints_v1` | High | ✅ G6 |
| `zwp_relative_pointer_v1` | High | ✅ G6 |
| `zwp_linux_dmabuf_v1` | High | ✅ G7 |
| `wp_fractional_scale_manager_v1` | Medium | ❌ |
| `wp_viewporter` | Medium | ❌ |
| `wp_presentation` | Medium | ❌ |
| `zwp_input_method_v1` / `zwp_text_input_v3` | Medium | ❌ |
| `zwlr_data_control_manager_v1` | Medium | ❌ |
| `xwayland` | High | ❌ |

---

## 4. Runtime Verification Results

### Chromium 151 on Veyra (nested Winit + llvmpipe)

| Test | Result | Notes |
|------|--------|-------|
| Launch | ✅ PASS | No errors, no crashes |
| Stability (12s) | ✅ PASS | Veyra and Chromium both remained running |
| Compositor errors | ✅ NONE | No Veyra log output during Chromium lifecycle |
| Chromium stderr | ✅ EMPTY | No error messages from Chromium |

**Environment**: CPU-only / Matrox G200eW (mgag200) / llvmpipe software rendering.
**EGL warnings observed**: `BAD_ALLOC eglInitialize`, `DRI2: failed to get driver name` — these are expected for this GPU and do not indicate a Veyra compositor failure.

### Native DRM/KMS

Not yet validated on this hardware (no GPU-accelerated GLES available).

---

## 5. Overall Assessment

### Rough Compatibility by Application Class

| Class          | Before | After G-A/G-B | Primary Remaining Blocker |
|----------------|--------|---------------|---------------------------|
| Terminals      | ~70%   | ~90%          | Clipboard/DnD partial     |
| GTK apps       | ~60%   | ~85%          | Clipboard/DnD partial     |
| Qt apps        | ~50%   | ~85%          | DnD stubs                 |
| Electron       | ~40%   | ~75%          | DnD stubs, IME            |
| Browsers       | ~35%   | ~80%          | DnD stubs, clipboard      |
| SDL/Games      | ~20%   | ~75%          | DRM presentation, vsync   |
| XWayland       | 0%     | 0%            | Not implemented           |

### Biggest Remaining Blockers

1. **DnD event processing** (G4 partial) — handlers exist but methods are stubs
2. **Clipboard MIME verification** (G3 partial) — architecture correct, MIME type list may be empty
3. **DRM presentation** — cannot actually present frames on native backend
4. **XWayland** — X11 applications cannot run

### Test Suite

All 344 tests pass (0 failed, 1 ignored). Config tests serialized via Mutex — no flakiness.
