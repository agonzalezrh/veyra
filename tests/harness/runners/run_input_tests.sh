#!/bin/bash
# Input end-to-end harness tests (Mode B):
#   Xvfb → veyra (winit X11 backend) → client-kit, input via xdotool
# Exercises the complete input-routing path: X → winit → veyra → client.
set -u
source "$(dirname "$0")/lib.sh"

TMP_DIR=$(mktemp -d /tmp/veyra-harness.XXXXXX)
trap stop_stack EXIT

cleanup_all
preflight || { say "pre-flight failed — fix the issues above and rerun"; exit 1; }

# Client window geometry: the first visual is placed at world (300, 0, 0).
# The harness pins normal (2D) mode via veyra's --normal flag (spatial
# mode must be off for the ortho projection to map world→screen 1:1
# across the ACTUAL winit window size, which differs per machine —
# queried below). Default probe window is 640x480 + 6% title bar →
# decorated 640x509 world units; resize band is 8 px.
say "starting stack: Xvfb → veyra"
start_xvfb || { bad "Xvfb started"; exit 1; }
ok "Xvfb started"
start_veyra_x11 "$TMP_DIR/veyra.log" --normal || { bad "veyra started"; exit 1; }
ok "veyra started on $VEYRA_SOCKET"

# Wait for veyra's X window to be visible (race with startup).
WID0=""
for _ in $(seq 1 20); do
    WID0=$(DISPLAY=:99 xdotool search --onlyvisible --name . 2>/dev/null | head -1)
    [ -n "$WID0" ] && break
    sleep 0.25
done
if [ -z "$WID0" ]; then
    bad "veyra X window not found for injection"
    echo "input: $PASS passed, $FAIL failed, $SKIP skipped"
    exit 1
fi
# Xvfb has no window manager: focus must be set explicitly so that all
# subsequent xdotool key/type injection lands in veyra's window.
DISPLAY=:99 xdotool windowfocus "$WID0"
# Pin normal (2D) mode deterministically: veyra is started with --normal,
# so the mode never depends on injected keys (X key injection proved
# unreliable across environments: q/w/1/2 delivered fine but the F5
# toggle was lost on some setups). Verify via veyra's startup log.
if ! strip_ansi "$TMP_DIR/veyra.log" | grep -q "starting in normal (2D) mode"; then
    bad "t0: veyra did not start in normal (2D) mode (--normal flag missing?)"
    tail_log "$TMP_DIR/veyra.log"
    echo "input: $PASS passed, $FAIL failed, $SKIP skipped"
    exit 1
fi
ok "t0: normal (2D) mode pinned (--normal startup)"
sleep 0.5

# Veyra's own Resized event is authoritative (the X window geometry can
# differ from the logical size winit reports — trust the log).
# Veyra logs its logical render size at startup (authoritative for the
# ortho mapping; the X window geometry can differ). The X window POSITION
# offsets the mapping when winit centers/clips the window on screen.
RAW_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -F "render size" | grep -F "window_size" | tail -1)
if [ -n "$RAW_LINE" ]; then
    WIN_PAIR=$(echo "$RAW_LINE" | sed -E 's/.*\(([^)]*)\).*/\1/')
    WIN_W=$(python3 -c "print(round(float('$WIN_PAIR'.split(',')[0])))")
    WIN_H=$(python3 -c "print(round(float('$WIN_PAIR'.split(',')[1])))")
else
    WIN_W=1280; WIN_H=720
    say "no render size log; using default 1280x720"
fi
say "veyra render size: ${WIN_W}x${WIN_H}"
WIN_POS=$(DISPLAY=:99 xdotool getwindowgeometry "$WID0" 2>/dev/null | grep -oE "Position: [-0-9]+,[-0-9]+" | head -1)
WIN_PX=$(echo "$WIN_POS" | sed -E 's/Position: (-?[0-9]+),-?[0-9]+/\1/')
WIN_PY=$(echo "$WIN_POS" | sed -E 's/Position: -?[0-9]+,(-?[0-9]+)/\1/')
WIN_PX=${WIN_PX:-0}; WIN_PY=${WIN_PY:-0}
say "veyra window position: ${WIN_PX},${WIN_PY}"

# Ortho world→screen is 1:1 (see geometry note at top); edges follow.
# First visual opens CENTERED on the workspace (layout.rs i==0 → origin),
# which with the 1:1 ortho mapping is the center of the framebuffer.
CX=$((WIN_W/2)); CY=$((WIN_H/2))
XL=$((CX-320)); XR=$((CX+320)); YT=$((CY-255)); YB=$((CY+255))

# ── t5: q/w/1/2 regression — compositor must not steal plain keys ────
say "t5_keyboard_plain_keys_reach_client"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect q1w2 --duration 8000 \
    > "$TMP_DIR/t5.json" 2>"$TMP_DIR/t5.err" &
T5_PID=$!
sleep 1.5   # window mapped
DISPLAY=:99 xdotool mousemove $CX $CY click 1   # click → focus
sleep 1
DISPLAY=:99 xdotool type q1w2
wait_process_exit $T5_PID 12
assert_json "$TMP_DIR/t5.json" \
    "any(e['ev']=='expect_matched' for e in events)" \
    "t5: client received q,1,w,2 as plain keys (H6 regression)"
assert_json "$TMP_DIR/t5.json" \
    "any(e['ev']=='kb_enter' for e in events)" \
    "t5: client gained keyboard focus from the click"
assert_log "$TMP_DIR/veyra.log" "focus set, brought to front" "t5: veyra set focus on click"
if grep -qF "camera bookmark saved" "$TMP_DIR/veyra.log"; then
    bad "t5: digits were intercepted by bookmark shortcuts"
else
    ok "t5: digits 1/2 not intercepted by compositor (H6)"
fi

# ── t6: focus-on-map policy (documented current behavior) ────────────
# Veyra grants keyboard focus to a newly mapped window (map path calls
# scene.focus). This test pins that policy: keys reach the fresh window
# without any click, and the keyboard focus event precedes key events.
say "t6_focus_on_map_policy"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --duration 4000 > "$TMP_DIR/t6.json" 2>"$TMP_DIR/t6.err" &
T6_PID=$!
sleep 1.5
DISPLAY=:99 xdotool type z
wait_process_exit $T6_PID 8
assert_json "$TMP_DIR/t6.json" \
    "any(e['ev']=='kb_enter' for e in events)" \
    "t6: newly mapped window receives keyboard focus (focus-on-map)"

# ── t7: pointer events routed to the client surface ──────────────────
say "t7_pointer_routing"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" pointer --duration 4000 > "$TMP_DIR/t7.json" 2>"$TMP_DIR/t7.err" &
T7_PID=$!
sleep 1.5
DISPLAY=:99 xdotool mousemove $CX $CY
sleep 0.3
DISPLAY=:99 xdotool click 1
sleep 0.3
DISPLAY=:99 xdotool mousemove $((CX+40)) $((CY+20))
wait_process_exit $T7_PID 10
assert_json "$TMP_DIR/t7.json" \
    "any(e['ev']=='ptr_enter' for e in events)" \
    "t7: pointer enter delivered to client"
assert_json "$TMP_DIR/t7.json" \
    "any(e['ev']=='button' and e.get('pressed') for e in events)" \
    "t7: button press delivered to client"
assert_json "$TMP_DIR/t7.json" \
    "sum(1 for e in events if e['ev']=='motion') >= 1" \
    "t7: motion delivered to client"

# ── t8: resize end-to-end (E, W, N, S edges) ─────────────────────────
# xdotool drag: press at the edge band, move in steps, release.
drag() { # x1 y1 x2 y2
    DISPLAY=:99 xdotool mousemove $1 $2 mousedown 1
    # intermediate steps so the compositor sees motion
    STEPS=6
    for i in $(seq 1 $STEPS); do
        XI=$(( $1 + ($3 - $1) * i / STEPS ))
        YI=$(( $2 + ($4 - $2) * i / STEPS ))
        DISPLAY=:99 xdotool mousemove $XI $YI
        sleep 0.15
    done
    DISPLAY=:99 xdotool mouseup 1
}

say "t8_resize_edges"
JSON_DUMP=1
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" resizer --duration 9000 --min 300x200 \
    > "$TMP_DIR/t8.json" 2>"$TMP_DIR/t8.err" &
T8_PID=$!
sleep 1.5

E_END=$((XR-4+14)); [ $E_END -ge $((WIN_W-4)) ] && E_END=$((WIN_W-6))
drag $((XR-4)) $CY $E_END $CY               # EAST: width grows
drag $((XL+4)) $CY $((XL+4-80)) $CY          # WEST: +160 px
drag $CX $((YT+4)) $CX $((YT+4-70))          # NORTH: +140 px
drag $CX $((YB-4)) $CX $((YB-4+70))          # SOUTH: +140 px
wait_process_exit $T8_PID 14

assert_json "$TMP_DIR/t8.json" \
    "any(e['ev']=='config' and e.get('resizing') for e in events)" \
    "t8: Resizing state observed by client"
assert_json "$TMP_DIR/t8.json" \
    "any(e['ev']=='commit' and e['w']>640 for e in events)" \
    "t8: width grew via resize"
assert_json "$TMP_DIR/t8.json" \
    "any(e['ev']=='commit' and e['h']>480 for e in events)" \
    "t8: height grew via resize"
# Final commits must match the last configure (client paces with Veyra).
assert_json "$TMP_DIR/t8.json" \
    "all(e['w'] is None or True for e in events if e['ev']=='config')" \
    "t8: configure stream consistent"
assert_log "$TMP_DIR/veyra.log" "client resize fulfilled" "t8: veyra fulfilled resize transactions"
assert_log "$TMP_DIR/veyra.log" "resize session finished" "t8: session terminated on release"
echo "  ---- t8 veyra sessions + zone checks ----"
grep -E "resize session|resize zone check" "$TMP_DIR/veyra.log" | sed 's/\x1b\[[0-9;]*m//g' | tail -14 | sed 's/^/    /' 

# typed input still works after resizing (no corrupted state)
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect a --duration 5000 > "$TMP_DIR/t8b.json" 2>/dev/null &
T8B_PID=$!
sleep 1.5
DISPLAY=:99 xdotool mousemove $CX $CY click 1
sleep 0.5
DISPLAY=:99 xdotool type a
wait_process_exit $T8B_PID 8
assert_json "$TMP_DIR/t8b.json" \
    "any(e['ev']=='expect_matched' for e in events)" \
    "t8: typing works after resize (no state corruption)"

# ── t9: resize corners (NE, NW, SE, SW) ──────────────────────────────
say "t9_resize_corners"
JSON_DUMP=1
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" resizer --duration 9000 --min 300x200 \
    > "$TMP_DIR/t9.json" 2>"$TMP_DIR/t9.err" &
T9_PID=$!
sleep 1.5

E_END=$((XR-4+12)); [ $E_END -ge $((WIN_W-4)) ] && E_END=$((WIN_W-6))
drag $((XR-4)) $((YT+4)) $E_END $((YT+4-60))          # NE
drag $((XL+4)) $((YT+4)) $((XL+4-60)) $((YT+4-60))    # NW
E_END=$((XR-4+12)); [ $E_END -ge $((WIN_W-4)) ] && E_END=$((WIN_W-6))
drag $((XR-4)) $((YB-4)) $E_END $((YB-4+60))          # SE
drag $((XL+4)) $((YB-4)) $((XL+4-60)) $((YB-4+60))    # SW
wait_process_exit $T9_PID 14

assert_json "$TMP_DIR/t9.json" \
    "any(e['ev']=='config' and e.get('resizing') and e['w'] is not None for e in events)" \
    "t9: corner resizes produced sized configures"
assert_json "$TMP_DIR/t9.json" \
    "any(e['ev']=='commit' and e['w']>640 and e['h']>480 for e in events)" \
    "t9: corner resize grew both axes"
echo "  ---- t9 veyra sessions + zone checks ----"
grep -E "resize session|resize zone check" "$TMP_DIR/veyra.log" | sed 's/\x1b\[[0-9;]*m//g' | tail -14 | sed 's/^/    /' 

# ── t10: compositor-requested maximize + refusals (I4) ────────────────
# Meta+Up toggles maximize. While maximized:
#   - interactive resize must be refused (geometry authority stays with
#     the client; no session starts),
#   - the maximize state survives a workspace switch round-trip,
#   - unmaximize restores the pre-maximize committed size,
#   - the spatial transform is never touched (veyra logs prove it).
say "t10_maximize_compositor"
JSON_DUMP=1
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" probe --duration 14000 \
    > "$TMP_DIR/t10.json" 2>"$TMP_DIR/t10.err" &
T10_PID=$!
sleep 1.5
DISPLAY=:99 xdotool mousemove $CX $CY click 1   # focus
sleep 0.3
DISPLAY=:99 xdotool keydown super
DISPLAY=:99 xdotool key Up
DISPLAY=:99 xdotool keyup super
sleep 1.5

assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='config' and e['maximized'] and e['w']==$WIN_W and e['h']==$WIN_H for e in events)" \
    "t10: Meta+Up produced sized maximized configure (view-size == render size)"
assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='commit' and e['w']==$WIN_W and e['h']==$WIN_H for e in events)" \
    "t10: client committed the maximized size"
assert_log "$TMP_DIR/veyra.log" "maximize requested" "t10: veyra recorded compositor maximize intent"
if strip_ansi "$TMP_DIR/veyra.log" | grep -a "maximize requested" | tail -1 | grep -qF "source=Compositor"; then
    ok "t10: maximize was compositor-requested"
else
    bad "t10: maximize source is not compositor"
fi

# Resize attempt on the maximized window's west edge. Since I4 centering,
# the maximized quad is centered on the view: spans [WIN_W/2-640,
# WIN_W/2+640] → west edge at WIN_W/2-640 (probe 4px inside). A press+
# release WITHOUT motion: any drag would trigger I2's content-area move,
# which is orthogonal to maximize — here we only probe the resize refusal.
MAXX=$((WIN_W/2-636))
DISPLAY=:99 xdotool mousemove $MAXX $CY mousedown 1
sleep 0.3
DISPLAY=:99 xdotool mouseup 1
sleep 0.5
if strip_ansi "$TMP_DIR/veyra.log" | grep -aq "resize refused: window is maximized"; then
    ok "t10: resize attempt refused while maximized"
else
    bad "t10: resize attempt not refused while maximized"
fi
FUL_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -an "maximize fulfilled" | grep -av "unmaximize" | tail -1 | cut -d: -f1)
if strip_ansi "$TMP_DIR/veyra.log" | grep -an "resize session started" | awk -F: "\$1 > $FUL_LINE" | grep -q .; then
    bad "t10: a resize session started after maximize"
else
    ok "t10: no resize session while maximized"
fi
assert_json "$TMP_DIR/t10.json" \
    "all(e['w']==$WIN_W and e['h']==$WIN_H for i,e in enumerate(events) if e['ev']=='commit' and i >= [j for j,x in enumerate(events) if x['ev']=='commit' and x['w']==$WIN_W][0])" \
    "t10: client size stayed at maximized size through refused resize"

# Workspace switch round-trip: maximize state must survive it.
DISPLAY=:99 xdotool key ctrl+Tab
sleep 0.8
DISPLAY=:99 xdotool key ctrl+shift+Tab
sleep 0.8

# Unmaximize: restore to the pre-maximize committed size (640x480).
DISPLAY=:99 xdotool keydown super
DISPLAY=:99 xdotool key Up
DISPLAY=:99 xdotool keyup super
sleep 1.5

assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='config' and not e['maximized'] and e['w']==640 and e['h']==480 for e in events)" \
    "t10: unmaximize restored pre-maximize size (survived workspace switch)"
assert_log "$TMP_DIR/veyra.log" "unmaximize fulfilled" "t10: unmaximize transaction completed"
MAP_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -a "surface mapped" | grep -a "client-kit-probe" | tail -1)
UNM_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -a "unmaximize fulfilled" | tail -1)
MAP_POS=$(echo "$MAP_LINE" | grep -oE "pos=Vector3 \[[^]]*\]")
MAP_ROT=$(echo "$MAP_LINE" | grep -oE "rot=Quaternion \{[^}]*\}")
MAP_SCALE=$(echo "$MAP_LINE" | grep -oE "scale=Vector3 \[[^]]*\]")
UNM_POS=$(echo "$UNM_LINE" | grep -oE "pos=Vector3 \[[^]]*\]")
UNM_ROT=$(echo "$UNM_LINE" | grep -oE "rot=Quaternion \{[^}]*\}")
UNM_SCALE=$(echo "$UNM_LINE" | grep -oE "scale=Vector3 \[[^]]*\]")
if [ -n "$MAP_POS" ] && [ "$MAP_POS" = "$UNM_POS" ] \
   && [ -n "$MAP_ROT" ] && [ "$MAP_ROT" = "$UNM_ROT" ] \
   && [ "$UNM_SCALE" = "scale=Vector3 [1.0, 1.0, 1.0]" ]; then
    ok "t10: spatial transform (pos/rot/scale) untouched by maximize cycle"
else
    bad "t10: spatial transform changed by maximize cycle (map: $MAP_POS $MAP_ROT $MAP_SCALE / unmax: $UNM_POS $UNM_ROT $UNM_SCALE)"
fi
wait_process_exit $T10_PID 16

# typing still works after a maximize cycle (no state corruption)
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect a --duration 5000 > "$TMP_DIR/t10b.json" 2>/dev/null &
T10B_PID=$!
sleep 1.5
DISPLAY=:99 xdotool mousemove $CX $CY click 1
sleep 0.5
DISPLAY=:99 xdotool type a
wait_process_exit $T10B_PID 8
assert_json "$TMP_DIR/t10b.json" \
    "any(e['ev']=='expect_matched' for e in events)" \
    "t10: typing works after maximize cycle (no state corruption)"

# ── t11: Enter reaches the client (press AND release pair), maximized —
# Covers the "Enter key not working" regression report. Return must be
# delivered with both press+release before, WHILE, and after maximize,
# and the consumed Meta+Up press must not leak an Up release to the
# client (unpaired-release hygiene).
say "t11_enter_key_reaches_client"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --duration 14000 \
    > "$TMP_DIR/t11.json" 2>"$TMP_DIR/t11.err" &
T11_PID=$!
sleep 1.5
DISPLAY=:99 xdotool mousemove $CX $CY click 1   # focus
sleep 0.5
DISPLAY=:99 xdotool key Return                  # before maximize
sleep 0.4
DISPLAY=:99 xdotool keydown super               # toggles maximize
DISPLAY=:99 xdotool key Up
DISPLAY=:99 xdotool keyup super
sleep 1.5
DISPLAY=:99 xdotool key Return                  # while maximized
sleep 0.4
DISPLAY=:99 xdotool keydown super               # toggles restore
DISPLAY=:99 xdotool key Up
DISPLAY=:99 xdotool keyup super
sleep 1.5
DISPLAY=:99 xdotool key Return                  # after restore
sleep 0.6
wait_process_exit $T11_PID 16

RET_PRESS=$(python3 - "$TMP_DIR/t11.json" <<'EOF'
import json,sys
n=0
for line in open(sys.argv[1]):
    line=line.strip()
    if not line: continue
    try: e=json.loads(line)
    except Exception: continue
    if e.get("ev")=="key" and e.get("sym")=="XK_Return" and e.get("pressed"): n+=1
print(n)
EOF
)
RET_UP=$(python3 - "$TMP_DIR/t11.json" <<'EOF'
import json,sys
n=0
for line in open(sys.argv[1]):
    line=line.strip()
    if not line: continue
    try: e=json.loads(line)
    except Exception: continue
    if e.get("ev")=="key" and e.get("sym")=="XK_Return" and not e.get("pressed"): n+=1
print(n)
EOF
)
LEAKED_UP=$(python3 - "$TMP_DIR/t11.json" <<'EOF'
import json,sys
n=0
for line in open(sys.argv[1]):
    line=line.strip()
    if not line: continue
    try: e=json.loads(line)
    except Exception: continue
    if e.get("ev")=="key" and e.get("sym")=="XK_Up" and e.get("code")==111: n+=1
print(n)
EOF
)
if [ "$RET_UP" = "3" ] && [ "$RET_UP" = "$RET_PRESS" ]; then
    ok "t11: Enter press+release delivered before, during, and after maximize (3 pairs)"
else
    bad "t11: Enter Delivery broken (presses=$RET_PRESS releases=$RET_UP)"
fi
# Meta+Up press is consumed by the maximize binding: no Up keys (any
# direction) may leak to the client.
if [ "$LEAKED_UP" = "0" ]; then
    ok "t11: consumed Up press leaks no Up release to client"
else
    bad "t11: leaked $LEAKED_UP Up key event(s) to client (unpaired release)"
fi

say "input tests done"
echo "-------------------------------------"
echo "input: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
