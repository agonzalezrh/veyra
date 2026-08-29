#!/bin/bash
# Shared helpers for the Veyra headless harness.

HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$HARNESS_DIR/../../.." && pwd)"
BIN="$ROOT_DIR/target/debug"
RT="/run/user/$(id -u)"
export XDG_RUNTIME_DIR="$RT"

PASS=0
FAIL=0

say()  { echo "[harness] $*"; }
ok()   { PASS=$((PASS+1)); echo "  PASS: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }

# Assert a substring appears in a file (grep -F).
assert_log() { # file pattern message
    if grep -qF "$2" "$1" 2>/dev/null; then ok "$3"; else bad "$3 (pattern '$2' not in $1)"; fi
}

# Assert a bash condition on the JSON event list of a client log.
# Each event is exposed as $e.<field> via python; expression receives
# the list as `events`.
assert_json() { # file python-expr message
    if python3 -c "
import json, sys
events = [json.loads(l) for l in open('$1') if l.strip()]
import sys as s
ok = bool(eval(sys.argv[1], {'events': events}))
s.exit(0 if ok else 1)
" "$2" 2>/dev/null; then ok "$3"; else bad "$3 (json expr failed: $2)"; fi
}

wait_for_log() { # file pattern timeout_s
    local f="$1" pat="$2" t="${3:-10}" n=0
    while [ $n -lt $((t*2)) ]; do
        grep -qF "$pat" "$f" 2>/dev/null && return 0
        sleep 0.5; n=$((n+1))
    done
    return 1
}

wait_process_exit() { # pid timeout_s
    local pid="$1" t="${2:-10}" n=0
    while [ $n -lt $((t*2)) ]; do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.5; n=$((n+1))
    done
    return 1
}

start_weston_headless() { # socket_name
    weston --backend=headless --socket="$1" --width=1280 --height=720 \
        > "$TMP_DIR/weston.log" 2>&1 &
    WESTON_PID=$!
    for _ in $(seq 1 40); do
        [ -S "$RT/$1" ] && return 0
        sleep 0.25
    done
    return 1
}

strip_ansi() { sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null; }

start_veyra_nested() { # parent_socket log_file
    WAYLAND_DISPLAY="$1" "$BIN/veyra" > "$2" 2>&1 &
    VEYRA_PID=$!
    for _ in $(seq 1 40); do
        VEYRA_SOCKET=$(strip_ansi "$2" | grep -oE "Listening on wayland socket: wayland-[a-z0-9-]+" | tail -1 | awk '{print $NF}')
        [ -n "$VEYRA_SOCKET" ] && [ -S "$RT/$VEYRA_SOCKET" ] && return 0
        sleep 0.25
    done
    return 1
}

start_veyra_x11() { # log_file
    # X11 mode: winit must not see a stale WAYLAND_DISPLAY
    env -u WAYLAND_DISPLAY DISPLAY=:99 "$BIN/veyra" > "$1" 2>&1 &
    VEYRA_PID=$!
    for _ in $(seq 1 40); do
        VEYRA_SOCKET=$(strip_ansi "$1" | grep -oE "Listening on wayland socket: wayland-[a-z0-9-]+" | tail -1 | awk '{print $NF}')
        [ -n "$VEYRA_SOCKET" ] && [ -S "$RT/$VEYRA_SOCKET" ] && return 0
        sleep 0.25
    done
    return 1
}

start_xvfb() {
    Xvfb :99 -screen 0 1280x720x24 > "$TMP_DIR/xvfb.log" 2>&1 &
    XVFB_PID=$!
    for _ in $(seq 1 40); do
        [ -S /tmp/.X11-unix/X99 ] && return 0
        sleep 0.25
    done
    return 1
}

stop_stack() {
    [ -n "${CLIENT_PIDS:-}" ] && kill $CLIENT_PIDS 2>/dev/null
    [ -n "${VEYRA_PID:-}" ] && kill "$VEYRA_PID" 2>/dev/null
    [ -n "${WESTON_PID:-}" ] && kill "$WESTON_PID" 2>/dev/null
    [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null
    sleep 0.5
    true
}

cleanup_all() {
    # Kill stale harness processes from previous runs.
    pkill -f "client-kit" 2>/dev/null
    pkill -f "weston --backend=headless" 2>/dev/null
    pkill -x veyra 2>/dev/null
    pkill -x Xvfb 2>/dev/null
    sleep 0.5
    true
}

trap stop_stack EXIT
