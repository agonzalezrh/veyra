# Veyra

A native 3D Wayland compositor and spatial desktop. Runs ordinary Wayland applications as persistent 3D objects in a spatial workspace.

## Status

Advanced development. Runs nested (winit) and natively (DRM/KMS via libseat).
Ordinary Wayland applications — terminals, GTK/Qt, Electron, Chromium,
Firefox (XWayland) — run with keyboard, pointer, clipboard, drag-and-drop,
IME, popups, subsurfaces, and fractional scaling. X11 applications run
through the built-in XWayland manager. The spatial desktop (workspaces,
overview, focus, arrangement) is presentation-only: applications never
see the 3D layer. Multi-output support is in progress (outputs.rs registry
landed; consumer migration ongoing). See COMPATIBILITY_MATRIX.md for the
current compatibility assessment and AGENTS.md for the roadmap.

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
