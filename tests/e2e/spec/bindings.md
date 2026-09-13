# Canonical bindings, geometry, log patterns, client events (normative reference)

Extracted from the current source and suites. If the compositor changes
a default, update this registry in the same commit — scenarios assert
through these constants.

## 1. Default keybindings (src/navigation.rs)

| Binding | Keys | Notes |
|---|---|---|
| ToggleSpatial | F5, Meta+Tab | camera/mode only — never a scene mutation |
| ToggleFocus | F6 (config `input.focus_key`) | camera operation |
| WorkspaceNext / Prev | Ctrl+Tab / Ctrl+Shift+Tab | |
| AppNext / AppPrev | Alt+Tab / Alt+Shift+Tab; F7 / F8 | F7/F8 for deterministic nested testing |
| Escape | Escape | priority chain: pointer-constraint → DnD → drag → workspace-overview → overview → focus → ResetCamera |
| FrameAll | Home | |
| OpenContextMenu | Menu key | also right-click |
| ToggleOverview | Meta+O | camera mode, no duplicate windows |
| ToggleWorkspaceOverview | Meta+P | |
| DeEmphasize | Meta+M | |
| FrameSelected | Meta+F | |
| ToggleShelf | Meta+D | |
| SendToShelf | Meta+Down | |
| Launcher | Meta+Space | |
| CloseApp | Meta+Q, Meta+W | |
| CycleLayout | Meta+L | Arc/Circle cycling (G-H3) |
| ToggleMaximize | Meta+Up, F11 | |
| MinimizeSelected | Meta+N, F9 | |
| RestoreSelected | Meta+U, F10 | |
| ToggleFullscreen | Meta+G, F12 | maximized state preserved through the cycle |
| ReopenClosed | Meta+Shift+T | closed-window history |
| HelpOverlay | Meta+/ | |
| Camera bookmarks | Meta+1..Meta+0 | slots 0–9 (`bookmark_slot`, src/navigation.rs) |
| Camera movement | W/A/S/D (+ orientation keys) when **no visual has focus** | src/input.rs logs "W pressed, camera forward" etc. |
| Mouse camera | right-drag orbit, middle-drag pan, scroll-over-empty zoom | scroll over a client goes to the client |

Config-overridable keys (`config.rs`, `VEYRA_CONFIG_PATH`):
`input.focus_key` (F6), `input.overview_key` (F9 — legacy default),
`shortcuts.alt_tab/launcher/toggle_shelf/send_to_shelf/reset_camera`
(Escape). Scenarios use the fixed laptop-test F-keys unless testing the
override itself.

## 2. Stack geometry (x11 stack — Xvfb :99, 1280x720x24)

- veyra started via `--normal` (+ optional args), `WAYLAND_DISPLAY`
  unset (winit must not pick Wayland), `RUST_LOG=veyra=info,veyra::compositor=debug`.
- Ortho world↔screen mapping is 1:1 after camera pinning:
  `sx = WIN_W/2 + wx`, `sy = WIN_H/2 - wy`.
- `WIN_W/WIN_H` from the startup log line
  `render size ... window_size (W, H)` (veyra's own Resized event is
  authoritative; the X window geometry can differ).
- First visual: world center (300, 0) → screen (940, 360); default
  640x480 + 6% title bar → decorated rect x[620..1260], y[106..614];
  resize band 8 px. Harness convention clicks the window at (CX, CY)
  = (640, 360) — the center of the first window.
- Taskbar: top y=684, height 36. Workspace buttons x=6..96
  (button 1 center ≈ (21, 702)). Window buttons start x=104, 170 px
  wide, 4 px gap (item0 center ≈ (189, 702), item1 ≈ (363, 702)).
  Click ALWAYS activates (never minimizes); minimize is F9/Meta+N.
- Title bar strip ≈ 29 px; right-aligned buttons (minimize | maximize |
  close) ≈ 21 px side, 8 px right margin, 6 px gap → close ≈ (XR-19),
  maximize ≈ (XR-68), minimize ≈ (XR-95), vertical center ≈ YT+14.
- veyra's XWayland: spawned by veyra; X11 clients use
  `DISPLAY=:<xdisplay()>`; readiness line logs `display=N`.

## 3. Compositor log-pattern registry (machine-evidence hooks)

ANSI stripped before matching. Patterns marked **(TBD)** must be
discovered in src during implementation and recorded here before the
corresponding scenario asserts them.

| Pattern | Meaning | Used by |
|---|---|---|
| `Listening on wayland socket: wayland-<id>` | readiness | all |
| `starting in normal (2D) mode` | `--normal` honored | E-A01 |
| `render size` + `window_size (W, H)` | logical render size | geometry |
| `surface mapped` (+ `pos=Vector3 [x,y,z] rot=Quaternion {…} scale=Vector3 [x,y,z]`, `app_id=`) | visual mapped | win_cx/win_cy |
| `surface destroyed` | visual destroyed | lifecycle |
| `focus set, brought to front` | click focus | E-F01 |
| `focus history updated` (vid=VisualId(N) order=[…]) | MRU | focus tests |
| `focus history pruned after minimize` / `refocusing after minimize … replacement=` | minimize focus flow | E-Kxx |
| `minimize applied` / `minimize restored` | F9/F10 | E-Kxx |
| `maximize requested` (`source=Compositor`) / `maximize fulfilled` / `unmaximize fulfilled` | maximize | E-Ixx |
| `fullscreen requested` / `fullscreen fulfilled` / `unfullscreen fulfilled` | fullscreen | E-Jxx |
| `close sent to focused app` | Meta+Q close | E-Lxx |
| `client resize fulfilled` / `resize session started` / `finished` / `resize refused: window is maximized` | resize engine | E-Hxx |
| `taskbar: activate window` / `restore window` / `switch workspace` | taskbar routing | E-Uxx |
| `switched workspace` | workspace change | E-Mxx |
| `camera bookmark saved` | Meta+N bookmarks | E-Rxx |
| `W pressed, camera forward` (S/A/D analog) | keyboard camera | E-Dxx |
| `dnd: client drag started` / `drop finished` / `grab cancelled` | DnD | E-AExx |
| `popup mapped` / `popup grab accepted (serial validated)` / `popup grab rejected` | popups | E-ABxx |
| `ime popup mapped` | IME popup | E-AFxx |
| `drag finished with popups attached` (pos= rot= popups=[…]) | popup follow math | E-ABxx |
| `app switch` / `no other focusable window` | MRU switcher | E-Pxx |
| `XWayland ready; starting X11 window manager` (`display=N`) | XWM up | E-AIxx |
| `spatial mode` transition lines | **(TBD — discover in src/input.rs / renderer)** | E-C01/E-C02 |
| overview / workspace-overview enter/exit lines | **(TBD)** | E-Oxx/E-Nxx |
| context-menu open/close lines | **(TBD)** | E-V01 |
| `panicked` | any panic ⇒ FAIL CRASH | all |

## 4. client-kit event catalog (tests/harness/src)

JSON lines on stdout; assertions are boolean exprs over the `events`
list:

- `config` — serial, `w`/`h` (None when unset), state bits:
  `maximized`, `fullscreen`, `resizing`, `activated`
- `commit` — `w`, `h`
- `frame` — frame callback
- `key` — `code`, `sym` (e.g. `XK_Return`), `char`, `pressed`
- `mods` — modifier state
- `kb_enter` / `kb_leave` — keyboard focus
- `ptr_enter` / `motion` / `button` (`pressed`) — pointer
- `expect_matched` — `keyboard --expect` success (exit 3 on timeout)
- `exit` — `code`
- `dnd_*` — `dnd_drag_started`, `dnd_enter`, `dnd_mime`, `dnd_motion`,
  `dnd_drop`, `dnd_data {payload}`, `dnd_leave`, `dnd_cancelled`,
  `dnd_send`, `dnd_drop_performed`, `dnd_finished`
- `clip_*` — `clip_data {payload, mime}`, `clip_mime`
- IME events (ime tester): text-input enable/commit stream

Subcommand flags (exact): see `tests/harness/README.md` —
`probe [--duration MS] [--app-id ID] [--exit-after-commits N]
[--resize-to WxH --after-commits N] [--maximize-after N]
[--text-input] [--subsurface]`,
`resizer [--duration MS] [--min WxH] [--max WxH]`,
`keyboard [--expect STR] [--duration MS]`,
`pointer [--duration MS]`,
`maximizer [--duration MS]`,
`popups [--cycles N] [--duration MS] [--hold] [--grab]`,
`dnd --role source|dest [--mime M] [--payload S] [--duration MS]`,
`ime [--duration MS]`,
`clip --mode set|paste [--mimes LIST] [--payload S] [--duration MS] [--primary]`.

## 5. Known stack hazards (encode as invariants, not surprises)

- XTEST duplicate-press history ⇒ stuck modifiers; `release_mods`
  before typing into real clients (tcfoot lesson).
- A leftover maximized client steals the next test's focus click ⇒
  `quiesce` between phases.
- `wait_for_log` on a shared log false-passes on earlier tests' lines ⇒
  per-scenario stack + `after:` anchors.
- `xDotool` F-key single-shots can vanish between XTEST and winit ⇒
  re-pin X focus (`focus_x11`) before key sequences; verify mode
  changes via log.
- t22-style config replacement replaces the veyra instance ⇒ later
  assertions read the CURRENT instance's log (per-scenario stacks in
  the e2e runner make this structurally safe).
