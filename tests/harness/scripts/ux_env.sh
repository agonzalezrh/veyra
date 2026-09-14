#!/bin/bash
# ux_env.sh — codified environment for the Veyra UX gate (G-H0.7).
#
# The nested-session display rules that used to live in test authors'
# heads are encoded HERE so nobody has to remember them:
#
#   :99        → the test desktop (Xvfb): xdotool, import, screenshots
#   :<N>       → veyra's XWayland: ONLY for launching X11 clients
#   windowfocus → XTEST keyboard events are dropped by the X server
#                unless the veyra winit window holds X input focus
#                FIRST (zero KEY logs in the veyra log = focus missing)
#
# Source this file, then call the ux_* helpers. Requires lib.sh's
# say/ok/bad helpers when used inside a runner.

UX_DESKTOP_DISPLAY=":99"
UX_XVFB_GEOMETRY="${UX_XVFB_GEOMETRY:-1280x720x24}"

ux_log() { echo "[ux] $*"; }

# --- session lifecycle -------------------------------------------------

ux_kill_all() {
    pgrep -f "target/debug/veyr[a]" | xargs -r kill -9 2>/dev/null
    pkill -9 -f "Xvf[b] :99" 2>/dev/null
    pkill -9 xterm 2>/dev/null
    pkill -9 -f "client-ki[t]" 2>/dev/null
    pkill -9 -f "firefo[x]" 2>/dev/null
    sleep 1
}

ux_spawn_desktop() { # <veyra-log>
    local log="$1"
    setsid Xvfb :99 -screen 0 "$UX_XVFB_GEOMETRY" > /tmp/ux-xvfb.log 2>&1 < /dev/null &
    disown
    sleep 2
    setsid env RUST_LOG="veyra=info" XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        DISPLAY="$UX_DESKTOP_DISPLAY" "$BIN/veyra" > "$log" 2>&1 < /dev/null &
    disown
    for _ in $(seq 1 40); do
        grep -q "Veyra running" "$log" 2>/dev/null && return 0
        sleep 0.5
    done
    return 1
}

# --- the display split, codified ---------------------------------------

ux_desktop() { echo "$UX_DESKTOP_DISPLAY"; }

ux_x11_display() { # <veyra-log> — veyra's XWayland display (for CLIENTS)
    sed -E 's/\x1b\[[0-9;]*m//g' "$1" \
        | grep -oP "X11 window manager display=\K[0-9]+" | tail -1
}

ux_launch_x11() { # <veyra-log> <cmd...> — an X11 client INSIDE veyra
    local log="$1"; shift
    local d
    d="$(ux_x11_display "$log")"
    if [ -z "$d" ]; then
        ux_log "no XWayland display found in $log"
        return 1
    fi
    DISPLAY=":$d" setsid "$@" > /dev/null 2>&1 < /dev/null &
    disown
}

# --- input, always correct ---------------------------------------------

ux_focus_desktop() { # <veyra-log> — focus the winit window for XTEST keys
    local log="$1"
    local win
    win=$(DISPLAY="$UX_DESKTOP_DISPLAY" xdotool search --onlyvisible --name "." 2>/dev/null \
        | while read -r w; do
            local gw
            gw=$(DISPLAY="$UX_DESKTOP_DISPLAY" xdotool getwindowgeometry "$w" 2>/dev/null \
                | awk '/Geometry:/ {split($2,a,"x"); print a[1]; exit}')
            [ "${gw:-0}" -ge 1000 ] && { echo "$w"; break; }
          done | head -1)
    if [ -n "$win" ]; then
        DISPLAY="$UX_DESKTOP_DISPLAY" xdotool windowfocus "$win"
    else
        ux_log "WARNING: no >=1000px window found for focus"
        return 1
    fi
}

ux_click() { # <log> <x> <y> [button]
    local log="$1" x="$2" y="$3" btn="${4:-1}"
    ux_focus_desktop "$log" > /dev/null 2>&1
    DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$x" "$y" click "$btn"
}

ux_type() { # <log> <keys...>
    local log="$1"; shift
    ux_focus_desktop "$log" > /dev/null 2>&1
    DISPLAY="$UX_DESKTOP_DISPLAY" xdotool key "$@"
}

ux_press_drag() { # <log> <x0> <y0> <x1> <y1> <button> [steps]
    local log="$1" x0="$2" y0="$3" x1="$4" y1="$5" btn="$6" steps="${7:-5}"
    ux_focus_desktop "$log" > /dev/null 2>&1
    local d="$UX_DESKTOP_DISPLAY"
    xdotool="DISPLAY=$d xdotool"
    DISPLAY="$d" xdotool mousemove "$x0" "$y0" mousedown "$btn"
    local i
    for i in $(seq 1 "$steps"); do
        local x y
        x=$(python3 -c "print(round($x0 + ($x1-$x0)*$i/$steps))")
        y=$(python3 -c "print(round($y0 + ($y1-$y0)*$i/$steps))")
        DISPLAY="$d" xdotool mousemove "$x" "$y"
        sleep 0.08
    done
    DISPLAY="$d" xdotool mouseup "$btn"
}

ux_shot() { # <name>
    DISPLAY="$UX_DESKTOP_DISPLAY" import -window root "$1"
}

# --- geometry (deterministic pixels, no VLM needed) --------------------
# ux_bbox <png> <python-lambda(r,g,b)->bool>  → "minx miny maxx maxy"
ux_bbox() {
    python3 - "$1" "$2" <<'PYEOF'
import subprocess, sys
img, pred_src = sys.argv[1], sys.argv[2]
pred = eval(pred_src)
out = subprocess.run(['convert', img, '-depth', '8', 'rgb:-'], capture_output=True).stdout
W = 100000
# width unknown here; use ImageMagick identify
try:
    W = int(subprocess.run(['identify', '-format', '%w', img], capture_output=True, text=True).stdout)
except Exception:
    pass
pts = []
for i in range(0, len(out), 3*5):
    r, g, b = out[i], out[i+1], out[i+2]
    if pred(r, g, b):
        px = (i//3) % W; py = (i//3) // W
        pts.append((px, py))
if not pts:
    print("NONE")
else:
    xs = [p[0] for p in pts]; ys = [p[1] for p in pts]
    print(min(xs), min(ys), max(xs), max(ys))
PYEOF
}

# ux_fully_visible <png> <pred> — window bbox fully inside the desktop
ux_fully_visible() { # <png> <pred> → rc 0 when fully visible
    local bb
    bb=$(ux_bbox "$1" "$2")
    [ "$bb" = "NONE" ] && return 1
    read -r x0 y0 x1 y1 <<< "$bb"
    [ "$x0" -ge 0 ] && [ "$y0" -ge 0 ] && [ "$x1" -le 1279 ] && [ "$y1" -le 689 ]
}

# ux_intersects <png> <pred> — window bounds intersect the visible desktop
ux_intersects() {
    local bb
    bb=$(ux_bbox "$1" "$2")
    [ "$bb" != "NONE" ]
}

# --- journal (authoritative state) --------------------------------------

# ux_last_snapshot <journal> → prints the LAST snapshot event line
ux_last_snapshot() {
    grep '"ev":"snapshot"' "$1" 2>/dev/null | tail -1
}

# ux_snapshot_field <snapshot-line> <python-expr over ev-dict `ev`>
ux_snapshot_field() {
    python3 - "$1" "$2" <<'PYEOF'
import json, sys
line, expr = sys.argv[1], sys.argv[2]
ev = json.loads(line)
print(eval(expr, {}, {"ev": ev}))
PYEOF
}

# ux_window_center <png> → "x y" — center of the LEFTMOST bright window
# region (client content is saturated on a dark desktop; the shell is
# dark). Deterministic press-target for window gestures.
ux_window_center() {
    python3 - "$1" <<'PYEOF'
import subprocess, sys
img = sys.argv[1]
out = subprocess.run(['convert', img, '-depth', '8', 'rgb:-'], capture_output=True).stdout
W = int(subprocess.run(['identify', '-format', '%w', img], capture_output=True, text=True).stdout)
H = int(subprocess.run(['identify', '-format', '%h', img], capture_output=True, text=True).stdout)
cols = []
for x in range(0, W, 4):
    n = 0
    for y in range(0, H-30, 6):   # skip taskbar strip
        i = (y*W + x)*3
        r, g, b = out[i], out[i+1], out[i+2]
        if max(r,g,b) > 90 and (max(r,g,b)-min(r,g,b)) > 40:
            n += 1
    cols.append((x, n))
# leftmost column-run with a substantial bright count
run = None
for x, n in cols:
    if n >= 8:
        run = x if run is None else run
    elif run is not None:
        break
if run is None:
    print("NONE"); sys.exit(0)
xs = run + 10
ys = []
for y in range(0, H-30, 2):
    i = (y*W + xs)*3
    r, g, b = out[i], out[i+1], out[i+2]
    if max(r,g,b) > 90 and (max(r,g,b)-min(r,g,b)) > 40:
        ys.append(y)
print(xs, (min(ys)+max(ys))//2 if ys else H//2)
PYEOF
}
