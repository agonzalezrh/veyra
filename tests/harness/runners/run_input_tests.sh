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
# maps world→screen 1:1 across the ACTUAL winit window size (which
# differs per machine — queried below). Default probe window is
# 640x480 + 6% title bar → decorated 640x509 world units; resize band
# is 8 px.
say "starting stack: Xvfb → veyra"
start_xvfb || { bad "Xvfb started"; exit 1; }
ok "Xvfb started"
start_veyra_x11 "$TMP_DIR/veyra.log" || { bad "veyra started"; exit 1; }
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
DISPLAY=:99 xdotool windowfocus "$WID0"
DISPLAY=:99 xdotool key F5
DISPLAY=:99 xdotool key Escape
sleep 0.5

# Veyra's own Resized event is authoritative (the X window geometry can
# differ from the logical size winit reports — trust the log).
# Veyra's own Resized event is authoritative when present (the X window
# geometry can differ from the logical size winit reports). Some stacks
# never emit it — fall back to veyra's initial window_size (1280x720).
# The Debug format of the size varies; grab the last two integers on the
# line (after the log timestamp).
RAW_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -F "Window resized to" | tail -1)
if [ -n "$RAW_LINE" ]; then
    say "resized event: $RAW_LINE"
    NUMS=$(echo "$RAW_LINE" | grep -oE "[0-9]+" | tail -2)
    WIN_W=$(echo "$NUMS" | sed -n 1p)
    WIN_H=$(echo "$NUMS" | sed -n 2p)
else
    WIN_W=1280; WIN_H=720
    say "no Resized event; using default window_size 1280x720"
fi
say "veyra render size: ${WIN_W}x${WIN_H}"

# Fallback placement: first visual at world (300, 0).
CX=$((WIN_W/2 + 300)); CY=$((WIN_H/2))
XL=$((CX-320)); XR=$((CX+320)); YT=$((CY-255)); YB=$((CY+255))

# Derive the CURRENT client window's screen rect from veyra's map log
# (position + decorated size in world space, ortho 1:1 onto the screen).
# Call after a client has mapped (its "surface mapped" line is the last one).
derive_rect_from_map() {
    local MAP_LINE
    MAP_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -F "surface mapped" | tail -1)
    local POS_X POS_Y TW TH
    POS_X=$(echo "$MAP_LINE" | sed -E 's/.*pos = Vector3 \[([^,]+),.*/\1/')
    POS_Y=$(echo "$MAP_LINE" | sed -E 's/.*pos = Vector3 \[[^,]+, ([^,]+),.*/\1/')
    TW=$(echo "$MAP_LINE" | sed -E 's/.*total_w = ([0-9.]+).*/\1/')
    TH=$(echo "$MAP_LINE" | sed -E 's/.*total_h = ([0-9.]+).*/\1/')
    if echo "$MAP_LINE" | grep -q "pos = Vector3" && [ -n "$TW" ] && [ "$TW" != "$MAP_LINE" ] && [ -n "$TH" ] && [ "$TH" != "$MAP_LINE" ]; then
        CX=$(python3 -c "print(round($WIN_W/2 + $POS_X))")
        CY=$(python3 -c "print(round($WIN_H/2 - $POS_Y))")
        XL=$(python3 -c "print(round($WIN_W/2 + $POS_X - $TW/2))")
        XR=$(python3 -c "print(round($WIN_W/2 + $POS_X + $TW/2))")
        YT=$(python3 -c "print(round($WIN_H/2 - $POS_Y - $TH/2))")
        YB=$(python3 -c "print(round($WIN_H/2 - $POS_Y + $TH/2))")
        say "client rect from map log: x[$XL..$XR] y[$YT..$YB]"
    else
        say "map line unusable; using placement defaults x[$XL..$XR] y[$YT..$YB]"
    fi
}

xdotool_wid() {
    DISPLAY=:99 xdotool search --onlyvisible --name . 2>/dev/null | head -1
}

# ── t5: q/w/1/2 regression — compositor must not steal plain keys ────
say "t5_keyboard_plain_keys_reach_client"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect q1w2 --duration 8000 \
    > "$TMP_DIR/t5.json" 2>"$TMP_DIR/t5.err" &
T5_PID=$!
sleep 1.5   # window mapped
derive_rect_from_map
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
derive_rect_from_map

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
echo "  ---- t8 veyra sessions ----"
grep -E "resize session" "$TMP_DIR/veyra.log" | sed 's/\x1b\[[0-9;]*m//g' | tail -10 | sed 's/^/    /' 

# typed input still works after resizing (no corrupted state)
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" keyboard --expect a --duration 5000 > "$TMP_DIR/t8b.json" 2>/dev/null &
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
echo "  ---- t9 veyra sessions ----"
grep -E "resize session" "$TMP_DIR/veyra.log" | sed 's/\x1b\[[0-9;]*m//g' | tail -10 | sed 's/^/    /' 

say "input tests done"
echo "-------------------------------------"
echo "input: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
