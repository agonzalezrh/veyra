# Veyra

A native 3D Wayland compositor and spatial desktop. Runs ordinary Wayland applications as persistent 3D objects in a spatial workspace.

## Status

Early development. Runs as a nested compositor via winit. External frame producers are supported as an optional input mechanism, but are not required for the native Wayland desktop.

## Building

```sh
cargo build --release
```

## Running

```sh
WAYLAND_DISPLAY=wayland-1 cargo run
```

Set `BENCHMARK_VISUALS=N` to spawn N benchmark windows for performance testing.

## Requirements

- Rust 1.85+
- OpenGL/GLES support
- Linux with DRM/KMS (native backend) or any system with winit (nested backend)

## License

MIT
