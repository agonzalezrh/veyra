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

# ---- shared gate/journey assertion helpers (G-H0.8) ----

# ux_row <snapshot-line> <vid> → "x y z w h" or ""
ux_row() {
    ux_snapshot_field "$1" "next((','.join(map(str,r['pos']+r['size'])) for r in ev['windows'] if r['vid']==$2), '')"
}
# ux_pos_moved <snapA> <snapB> <vid> → rc0 if pos changed
ux_pos_moved() {
    local a b
    a=$(ux_row "$1" "$2"); b=$(ux_row "$1" "$3" 2>/dev/null)
    a=$(ux_row "$1" "$2")
    b=$(ux_row "$3" "$2")
    [ -n "$a" ] && [ -n "$b" ] && [ "${a%% *}" != "${b%% *}" ] \
        && [ "${a#* }" != "${b#* }" ]
}
# I1: no window moved between two snapshots
ux_no_window_moved() {
    python3 - "$1" "$2" <<'PYEOF'
import json, sys
a = json.loads(sys.argv[1]); b = json.loads(sys.argv[2])
pa = {r['vid']: r['pos'] for r in a['windows']}
pb = {r['vid']: r['pos'] for r in b['windows']}
common = set(pa) & set(pb)
sys.exit(0 if all(pa[v] == pb[v] for v in common) else 1)
PYEOF
}
# I2: exactly one vid moved between two snapshots; prints the vid
ux_single_mover() {
    python3 - "$1" "$2" <<'PYEOF'
import json, sys
a = json.loads(sys.argv[1]); b = json.loads(sys.argv[2])
pa = {r['vid']: r['pos'] for r in a['windows']}
pb = {r['vid']: r['pos'] for r in b['windows']}
moved = [v for v in pb if pa.get(v) != pb[v]]
print(moved[0] if len(moved) == 1 else "MANY" if moved else "NONE")
PYEOF
}
# geometry from authoritative state: projected rect vs desktop
# ux_geom <snapshot-line> <vid> intersect|fully
ux_geom() {
    python3 - "$1" "$2" "$3" <<'PYEOF'
import json, math, sys
line, vid, mode = sys.argv[1], int(sys.argv[2]), sys.argv[3]
ev = json.loads(line)
row = next((r for r in ev['windows'] if r['vid'] == vid), None)
if row is None: sys.exit(2)
x, y, z = row['pos']; w, h = row['size']
cx, cy, cz = ev.get('camera', (0.0, 0.0, ev['camera_z']))
tz = math.tan(math.radians(22.5))
aspect = 1280/720
sx = 640.0/(cz*tz*aspect); sy = 360.0/(cz*tz)
x0, x1 = 640+(x-cx-w/2)*sx, 640+(x-cx+w/2)*sx
y0, y1 = 360-(y-cy+h/2)*sy, 360-(y-cy-h/2)*sy
desk = (0, 0, 1280, 690)
inter = not (x1 < desk[0] or x0 > desk[2] or y1 < desk[1] or y0 > desk[3])
inside = x0 >= desk[0] and y0 >= desk[1] and x1 <= desk[2] and y1 <= desk[3]
sys.exit(0 if (inside if mode == 'fully' else inter) else 1)
PYEOF
}

# ux_project <snapshot-line> <vid> → "cx cy" (screen px, yaw≈0) or ""
ux_project() {
    python3 - "$1" "$2" <<'PYEOF'
import json, math, sys
line, vid = sys.argv[1], int(sys.argv[2])
ev = json.loads(line)
row = next((r for r in ev['windows'] if r['vid'] == vid), None)
if row is None: print(""); sys.exit(0)
x, y, z = row['pos']; w, h = row['size']
cx, cy, cz = ev['camera']
tz = math.tan(math.radians(22.5)); aspect = 1280/720
sx = 640.0/(cz*tz*aspect); sy = 360.0/(cz*tz)
print(round(640+(x-cx)*sx), round(360-(y-cy)*sy))
PYEOF
}


# ux_background_point <snapshot-line> [toward_x toward_y] → "x y"
# The first candidate point whose projected position is NOT covered by
# any window rect; prefers points on the center→(toward) line so wheel
# gestures can aim at a specific window over empty space.
ux_background_point() {
    python3 - "$1" "${2:-640}" "${3:-360}" <<'PYEOF'
import json, math, sys
line, tx, ty = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
ev = json.loads(line)
cx, cy, cz = ev.get('camera', (0.0, 0.0, ev['camera_z']))
tz = math.tan(math.radians(22.5)); aspect = 1280/720
sx = 640.0/(cz*tz*aspect); sy = 360.0/(cz*tz)
rects = []
for r in ev['windows']:
    x, y, z = r['pos']; w, h = r['size']
    x0 = 640+(x-cx-w/2)*sx; x1 = 640+(x-cx+w/2)*sx
    y0 = 360-(y-cy+h/2)*sy; y1 = 360-(y-cy-h/2)*sy
    rects.append((x0, y0, x1, y1))
def covered(px, py):
    return any(x0-8 <= px <= x1+8 and y0-8 <= py <= y1+8 for x0,y0,x1,y1 in rects)
dx, dy = tx-640, ty-360
n = math.hypot(dx, dy) or 1.0
dx, dy = dx/n, dy/n
candidates = [(640+dx*t, 360+dy*t) for t in range(620, -621, -60)]
candidates += [(60, 60), (1220, 60), (60, 660), (1220, 660), (640, 60), (640, 660)]
for px, py in candidates:
    if 0 <= px <= 1280 and 0 <= py <= 685 and not covered(px, py):
        print(round(px), round(py)); break
else:
    print("NONE")
PYEOF
}

# ux_click2 <log> <x> <y> — a click that does NOT refocus the desktop
# window first (used inside gestures where the X focus is already set).
ux_click2() {
    DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$2" "$3" click 1
}

# ux_desktop_window <log> → the veyra winit window id (geometry-derived)
ux_desktop_window() {
    DISPLAY="$UX_DESKTOP_DISPLAY" xdotool search --onlyvisible --name "." 2>/dev/null \
        | while read -r w; do
            local gw
            gw=$(DISPLAY="$UX_DESKTOP_DISPLAY" xdotool getwindowgeometry "$w" 2>/dev/null \
                | awk '/Geometry:/{split($2,a,"x");print a[1];exit}')
            [ "${gw:-0}" -ge 1000 ] && { echo "$w"; break; }
          done | head -1
}
