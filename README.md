# Veyra

A native 3D Wayland compositor and spatial desktop. Runs ordinary Wayland applications as persistent 3D objects in a spatial workspace.

## Status

Advanced development. Runs nested (winit) and natively (DRM/KMS via libseat).
Ordinary Wayland applications — terminals, GTK/Qt, Electron, Chromium,
Firefox (XWayland) — run with keyboard, pointer, clipboard, drag-and-drop,
IME, popups, subsurfaces, and fractional scaling. X11 applications run
through the built-in XWayland manager. The spatial desktop (workspaces,
overview, focus, arrangement) is presentation-only: applications never
see the 3D layer. Nested multi-output simulation is implemented
(VEYRA_SIM_OUTPUTS=N: N logical outputs with independent cameras,
viewports, and modes inside one window, over a shared scene). Native
DRM multi-connector output support is in progress (G-E5.6). See
COMPATIBILITY_MATRIX.md for the current compatibility assessment and
AGENTS.md for the roadmap.

## Building

```sh
cargo build --release
```

## Running

```sh
WAYLAND_DISPLAY=wayland-1 cargo run          # nested (winit) session
cargo run -- --native                        # native DRM/KMS session (libseat)
```

Set `BENCHMARK_VISUALS=N` to spawn N benchmark windows for performance testing.

## Requirements

- Rust 1.85+
- OpenGL/GLES support
- Linux with DRM/KMS (native backend) or any system with winit (nested backend)

## License

MIT
