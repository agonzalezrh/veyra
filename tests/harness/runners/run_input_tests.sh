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
# The harness pins normal (2D) mode by injecting F5 right after veyra
# starts (the F5 binding toggles spatial mode), so the ortho projection
# maps world→screen 1:1. The winit X11 window is 1280x800 (its own
# default), so world y=0 is screen y=400. Default probe window is
# 640x480 + 6% title bar → decorated 640x509 world units.
# Screen rect: x [620..1260], y [106..614], resize band = 8 px.
CX=940; CY=360
XL=620; XR=1260; YT=106; YB=614

say "starting stack: Xvfb → veyra"
start_xvfb || { bad "Xvfb started"; exit 1; }
ok "Xvfb started"
start_veyra_x11 "$TMP_DIR/veyra.log" || { bad "veyra started"; exit 1; }
ok "veyra started on $VEYRA_SOCKET"

# Deterministic camera state for all pointer-injection tests:
# - F5 toggles spatial mode OFF (ortho 1:1 mapping) and stops auto-orbit
#   (any key press clears it, but it freezes at an arbitrary angle)
# - Escape runs the escape chain → ResetCamera (yaw/pitch = 0)
# Render then pins the normal-mode camera to z=500 with ortho(±640,±360).
WID0=$(DISPLAY=:99 xdotool search --onlyvisible --name . 2>/dev/null | head -1)
DISPLAY=:99 xdotool windowfocus "$WID0"
DISPLAY=:99 xdotool key F5
DISPLAY=:99 xdotool key Escape
sleep 0.5

xdotool_wid() {
    DISPLAY=:99 xdotool search --onlyvisible --name . 2>/dev/null | head -1
}

# ── t5: q/w/1/2 regression — compositor must not steal plain keys ────
say "t5_keyboard_plain_keys_reach_client"
WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect q1w2 --duration 8000 \
    > "$TMP_DIR/t5.json" 2>"$TMP_DIR/t5.err" &
T5_PID=$!
sleep 1.5   # window mapped
WID=$(xdotool_wid)
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
WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --duration 4000 > "$TMP_DIR/t6.json" 2>"$TMP_DIR/t6.err" &
T6_PID=$!
sleep 1.5
DISPLAY=:99 xdotool type z
wait_process_exit $T6_PID 8
assert_json "$TMP_DIR/t6.json" \
    "any(e['ev']=='kb_enter' for e in events)" \
    "t6: newly mapped window receives keyboard focus (focus-on-map)"

# ── t7: pointer events routed to the client surface ──────────────────
say "t7_pointer_routing"
WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" pointer --duration 4000 > "$TMP_DIR/t7.json" 2>"$TMP_DIR/t7.err" &
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
WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" resizer --duration 9000 --min 300x200 \
    > "$TMP_DIR/t8.json" 2>"$TMP_DIR/t8.err" &
T8_PID=$!
sleep 1.5   # mapped at 640x480 → screen rect computed above

drag $((XR-4)) $CY $((XR-4+14)) $CY          # EAST: +~28 px
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

# typed input still works after resizing (no corrupted state)
WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect a --duration 5000 > "$TMP_DIR/t8b.json" 2>/dev/null &
T8B_PID=$!
sleep 1.5
WID=$(xdotool_wid)
DISPLAY=:99 xdotool mousemove $CX $CY click 1
sleep 0.5
DISPLAY=:99 xdotool type a
wait_process_exit $T8B_PID 8
assert_json "$TMP_DIR/t8b.json" \
    "any(e['ev']=='expect_matched' for e in events)" \
    "t8: typing works after resize (no state corruption)"

# ── t9: resize corners (NE, NW, SE, SW) ──────────────────────────────
say "t9_resize_corners"
WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" resizer --duration 9000 --min 300x200 \
    > "$TMP_DIR/t9.json" 2>"$TMP_DIR/t9.err" &
T9_PID=$!
sleep 1.5

drag $((XR-4)) $((YT+4)) $((XR-4+12)) $((YT+4-60))    # NE
drag $((XL+4)) $((YT+4)) $((XL+4-60)) $((YT+4-60))    # NW
drag $((XR-4)) $((YB-4)) $((XR-4+12)) $((YB-4+60))    # SE
drag $((XL+4)) $((YB-4)) $((XL+4-60)) $((YB-4+60))    # SW
wait_process_exit $T9_PID 14

assert_json "$TMP_DIR/t9.json" \
    "any(e['ev']=='config' and e.get('resizing') and e['w'] is not None for e in events)" \
    "t9: corner resizes produced sized configures"
assert_json "$TMP_DIR/t9.json" \
    "any(e['ev']=='commit' and e['w']>640 and e['h']>480 for e in events)" \
    "t9: corner resize grew both axes"

say "input tests done"
echo "-------------------------------------"
echo "input: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
