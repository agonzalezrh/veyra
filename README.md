# Veyra — One Monitor, A Larger Workspace

Veyra is a spatial Wayland compositor: it turns **one physical monitor
into a much larger desktop** by letting your applications live in a
navigable 3D workspace. Arrange windows side by side, near and far —
then move between them the way you move around a real desk.

```
        ONE PHYSICAL MONITOR
                 |
      [ Browser+Docs ]  [ IDE+Term ]  [ Chat+Mail ]
           spatial regions you travel between
```

## The idea

Normal desktops cram everything onto one flat screen or hide it behind
alt-tab. Veyra keeps your applications **in places**: the browser on the
left, the terminal on the right, the IDE further back. Your memory of
*where things are* becomes the primary navigation — the camera just
takes you there.

Applications are ordinary Wayland (and X11) programs. They never know
the desktop is 3D.

## Getting started

Build and run (a nested window on your existing desktop):

```sh
cargo build --release
./target/release/veyra
```

Requirements: Rust 1.98+, a Wayland session, OpenGL/GLES (software
rendering works). Wayland client apps need `WAYLAND_DISPLAY=wayland-1`;
X11 apps connect to veyra's XWayland display automatically.

## Controls

**Camera (empty space):**

| Input | Effect |
|---|---|
| Wheel | Approach / back away (toward the pointer) |
| Left-drag | Move the world (pan) |
| Right-drag | Look around (orbit) |
| `Home` | Show the whole desktop (always works, even when lost) |

**Windows:**

| Input | Effect |
|---|---|
| Click | Focus |
| Left-drag | The application (text selection, sliders) |
| Alt+drag (or Meta+drag) | Move the window |
| Right-drag | Rotate the window (release without moving = menu) |
| Wheel | Scroll the application |

**Shell:**

| Input | Effect |
|---|---|
| Taskbar | Every window on every workspace (`·N` = its workspace); click to travel there |
| Right-click a window | Actions menu (focus, move, minimize, close, arrange…) |
| `F5` or the menu | Toggle spatial mode |

## Workspaces

Three rooms ship by default. Each remembers its own camera and windows.
Switch with the taskbar (the numbered buttons) or keyboard bindings; the
camera glides between rooms. Home frames the current room.

## Persistence

Window positions, rotations, workspace membership and cameras are saved
on exit and restored on the next launch. Corrupt state backs up and
starts fresh; a lost camera is always one `Home` away.

## Known limitations

- Aiming the wheel *at* a window scrolls that window (approach via
  nearby empty space). Intentional; the hint line reminds you.
- Identical same-app windows are distinguished by title only (hover a
  taskbar button to light that window in the scene).
- The bitmap font is an intentional aesthetic for now.
- Very large single rows (20+ windows) pull the camera far back.

## For developers

Architecture, milestones and the full testing program live in
`AGENTS.md`. The E2E harness is under `tests/harness/scripts/`:
`run_ux_gate.sh` (fast gate), `run_golden_journey.sh`,
`run_restart_journey.sh`, `run_overnight_gate.sh` (full suite).
