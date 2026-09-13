# Action vocabulary (normative)

Steps are a discriminated union: each step object has exactly one
top-level key. Semantics here are binding for the runner implementation.

## Expression context

Fields accepting `{{ ... }}` are evaluated with Python `eval` over the
context dict (builtins restricted to `int`, `float`, `min`, `max`,
`abs`, `round`). Context is pre-seeded per stack (`spec/bindings.md`
has the geometry):

| Name | Meaning (x11 stack, 1280x720) |
|------|-------------------------------|
| `WIN_W`, `WIN_H` | veyra's logged logical render size |
| `CX`, `CY` | framebuffer center (first visual center) |
| `XL`, `XR`, `YT`, `YB` | first-visual decorated rect edges (640x480 + 6% title bar) |
| `TB_TOP`, `TB_H` | taskbar top edge (684) and height (36) |
| `WID0` | X window id of veyra's own window (injection target) |
| `VEYRA_SOCKET` | veyra's Wayland socket name |
| `VEYRA_RUNTIME` | private XDG_RUNTIME_DIR of the stack |

Context functions:

- `win_cx(token)`, `win_cy(token)` — center of the **last**
  `surface mapped` compositor log line whose text contains `token`
  (matches `app_id=...` and X11 class names). Mapping is the ortho
  1:1: `sx = WIN_W/2 + wx`, `sy = WIN_H/2 - wy`.
- `mapped_count(token)` — number of `surface mapped` lines containing
  `token` so far.
- `xdisplay()` — XWayland display number parsed from the
  `XWayland ready` log line's `display=N` field (fallback `0`).

ANSI escape sequences are stripped from all log greps (suite lesson:
tracing color codes break `grep -n` anchors).

## Client/process steps

- **launch** — start a `client-kit` tester on veyra's Wayland socket
  (`probe`, `resizer`, `keyboard`, `pointer`, `maximizer`, `popups`,
  `dnd`, `ime`, `clip`) with the given args; `app_id` sets
  `--app-id`; `json` captures stdout (JSON event log) under the test
  dir. Non-zero exit of the client is NOT an assert by itself — assert
  via `client_json`.
- **exec** — run an arbitrary application. `display: xvfb` injects
  `DISPLAY=:99`; `display: xwayland` injects `DISPLAY=:<xdisplay()>`
  (the app then goes through veyra's XWM); `wayland: true` injects
  `WAYLAND_DISPLAY=$VEYRA_SOCKET` + `XDG_RUNTIME_DIR=$VEYRA_RUNTIME`.
  `wait_log` scopes a startup wait and may record an anchor.
- **kill** — `pkill -f` + signal; for destruction-torture scenarios.
- **quiesce** — kill leftover client-kit processes between phases
  (suite hygiene; a surviving maximized window steals the next test's
  focus click).

## Input steps (xdotool over Xvfb :99)

- **click** — `xdotool mousemove X Y click N`; `double: true` uses
  `--repeat 2 --delay 60`.
- **drag** — `mousedown`, interpolated `mousemove` steps (default 6,
  150 ms apart — compositor must see intermediate motion), `mouseup`.
- **key** — single `xdotool key`. Reliability note: F-key single-shots
  were lost on some setups; use **chord** for modifier combos and
  verify mode transitions via log/state, never assume.
- **chord** — `keydown` each mod, `key <k>`, `keyup` mods with
  `hold_ms` pauses (this is the reliable pattern for Meta+Q,
  Ctrl+Tab, Meta+Shift+T etc.).
- **type** — `xdotool type`; `clear_mods: true` (default) first does
  `xdotool keyup super alt ctrl shift` (stuck-META history).
- **scroll** — `xdotool click 4/5` at position.
- **focus_x11** — `xdotool windowfocus $WID0`; Xvfb has no WM, X
  keyboard focus never moves on its own and must be re-pinned before
  key sequences.
- **release_mods** — `xdotool keyup super alt ctrl shift`.

## Flow / observation steps

- **wait_log** — poll (500 ms interval) for pattern in veyra's log;
  `app_id` appends an `app_id=<x>` filter; `after` restricts to lines
  after the named anchor; `anchor` records the matched line number for
  later `after:`/`since:` scoping. Timeout ⇒ assertion FAIL.
- **wait_stable** — two captures N ms apart, changed-pixel fraction
  below threshold; wait+retry until timeout.
- **capture** — screenshot checkpoint (see README §4 and the scenario
  schema). Category, stability, Stage-A `image_checks`, optional VLM.
- **assert** — one deterministic assertion: `log_contains` (scoped,
  polled up to `within_s`), `log_not_contains` (evaluated at scenario
  end over the slice since `since:`), `client_json` (boolean expr over
  the event list), `process_alive`, `wayland_socket_alive`,
  `x11_window_visible`.
- **set_context** — add a variable for later expressions.
- **sleep_ms** — allowed only with a `reason`; discouraged (state
  waits should use `wait_log`/`wait_stable`).

## Stage-A deterministic image checks (reference implementation)

PyYAML is available; Pillow is **not** installed — image math uses
ImageMagick (`import`, `identify`, `compare`), which the harness
already depends on:

- `not_black` / `not_white` — `identify -format "%[mean]"` outside
  [0..4] / [251..255] mean range.
- `size_matches_display` — `identify` dimensions == Xvfb geometry.
- `region_not_uniform` — region stddev above threshold.
- `region_min_changed_pct` — `compare -metric AE` (fuzz ~2%) over the
  region vs the previous capture, ≥ pct% differing pixels.
- changed-pixel fraction for stability = `compare -metric AE` full
  frame / total pixels (fuzz ~2% to ignore dithering noise).

Every check's raw numbers go into the result record.
