# Veyra

A spatial Wayland compositor — ordinary applications in 3D space.

## Status

Early development. Runs as a nested compositor via winit. Supports Wayland clients and external frame producers.

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
