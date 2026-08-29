# Veyra Headless Test Harness

Deterministic integration tests that run over SSH without VNC or a
graphical session. All assertions are on **Wayland protocol behavior**
(client JSON event logs, compositor logs) — never screenshots or
timing sleeps.

## Architecture

Two stacks, both fully headless:

```text
Mode A (protocol):  weston --backend=headless → veyra (nested winit)
                    → client-kit / real clients → JSON protocol assertions

Mode B (input E2E): Xvfb → veyra (winit X11 backend) → client-kit
                    → xdotool injection → protocol assertions
```

## Running

```bash
cargo build -p client-kit          # test clients
cargo build                        # compositor
tests/harness/runners/run_all.sh   # everything (≈2 min)
```

Suites:

- `runners/run_protocol_tests.sh` — Mode A: toplevel lifecycle
  (configure/ACK/commit/frame callbacks), client geometry ownership
  (client-driven resize adopted, no compositor fighting), client exit
  cleanup + focus replacement, real-client smoke (foot, weston-terminal,
  weston-simple-shm, weston-simple-egl).
- `runners/run_input_tests.sh` — Mode B: full input path X → winit →
  veyra → client. Plain-key regression (q/w/1/2 must reach clients),
  focus-on-map policy, pointer enter/button/motion routing, resize
  end-to-end for 4 edges + 4 corners (Resizing state, configure→ACK→
  commit transactions, geometry growth, typing-after-resize integrity).

## client-kit subcommands

All clients log JSON lines on stdout: `config` (serial, size, states),
`commit`, `frame`, `key` (code/keysym/char), `mods`, pointer events,
`close`, `exit`.

```
client-kit probe    [--duration MS] [--app-id ID] [--exit-after-commits N]
                    [--resize-to WxH --after-commits N]
client-kit resizer  [--duration MS] [--min WxH] [--max WxH]
client-kit keyboard --expect STR [--duration MS]   # exit 0 on match, 3 on timeout
client-kit pointer  [--duration MS]
```

`probe --resize-to` commits a different size mid-flight to test the
"client decides geometry" ownership invariant from the client side.

## Determinism notes

- Mode B pins the camera before any pointer test: **F5** (normal mode,
  stops auto-orbit) then **Escape** (ResetCamera). The render then uses
  ortho(±640,±360) so world↔screen mapping is 1:1.
- The first visual maps at world (300, 0) → screen (940, 360); default
  client window 640x480 + 6% title bar → screen rect x[620..1260],
  y[106..614]; resize band 8 px.
- veyra must start with `WAYLAND_DISPLAY` unset in X11 mode (winit
  prefers Wayland otherwise) — `start_veyra_x11` handles this.
- Client processes flush before exiting so their final commit is
  observed by the compositor.

## Known limitations

- Pixel output on Xvfb/llvmpipe renders only the clear color (input and
  protocol paths are unaffected). Visual/screenshot testing belongs on
  real hardware (laptop) per the test matrix.
- Chromium smoke test not yet included (not installed on this server).
- VKMS/DRM-native path deferred (needs DRM device enumeration, seatd,
  uinput injection) — see the capability report.
