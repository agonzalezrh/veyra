# Veyra — Hands-On Test Script

Work through these tasks in order. There are no expected outcomes to
match — just use the desktop and note what feels wrong, slow, or
confusing. Roughly 30-60 minutes.

## Setup

```sh
cargo build --release
./target/release/veyra        # nested window on your desktop
```

Launch apps: firefox is on the taskbar's right side (or run
`firefox` / `xterm` / `google-chrome` — they connect to veyra
automatically).

## Tasks

1. Launch veyra. Read what's on screen. Launch a browser.
2. Launch a terminal. Notice where each one appeared.
3. Move the browser to one area and the terminal to another
   (Alt+drag on a window moves it; drag empty space to move the view).
4. Get close to the browser and do some real work in it
   (scroll a page, type in a field). Wheel over the window scrolls it.
5. Back away (wheel over empty space) and travel to the terminal.
6. Type a few commands in the terminal.
7. Close your eyes for five seconds, then find each window again.
8. Switch between the two using only the taskbar.
9. Switch to workspace 2 (the taskbar's `2`), open another app there,
   then come back to workspace 1.
10. Intentionally lose the camera: several big pans until nothing is
    recognizable. Then press `Home`.
11. Right-click each window and skim the actions menu.
12. Minimize a window from the taskbar and restore it.
13. Work normally for 15+ minutes. Use it like your actual desktop.

## While working, notice

- Can you find things by memory?
- Does the camera help or get in the way?
- Does anything feel surprising or un-rememberable?
- Is typing/scrolling reliable everywhere?
- Can you always get back to a good view?

## If something breaks

- `Home` recovers the view from any camera state.
- The taskbar reaches every window on every workspace.
- Run veyra with `RUST_LOG=info` and capture the log; screenshots
  help. Note what you did just before it broke.
