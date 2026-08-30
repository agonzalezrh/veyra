#!/bin/bash
# Protocol-level harness tests (Mode A):
#   weston-headless → veyra (nested winit) → client-kit / real clients
# Deterministic assertions on client JSON logs and veyra's log — no
# screenshots, no sleeps beyond readiness polling.
set -u
source "$(dirname "$0")/lib.sh"

TMP_DIR=$(mktemp -d /tmp/veyra-harness.XXXXXX)
trap stop_stack EXIT

cleanup_all
preflight || { say "pre-flight failed — fix the issues above and rerun"; exit 1; }

say "starting stack: weston-headless → veyra"
start_weston_headless wayland-harness || { bad "weston-headless started"; exit 1; }
ok "weston-headless started"
start_veyra_nested wayland-harness "$TMP_DIR/veyra.log" || { bad "veyra started"; exit 1; }
ok "veyra started on $VEYRA_SOCKET"

# ── t1: toplevel lifecycle ───────────────────────────────────────────
say "t1_lifecycle"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" probe --duration 2500 > "$TMP_DIR/t1.json" 2>"$TMP_DIR/t1.err" &
T1_PID=$!
wait_process_exit $T1_PID 10
assert_log "$TMP_DIR/veyra.log" "surface mapped" "t1: veyra mapped the toplevel"
assert_json "$TMP_DIR/t1.json" \
    "any(e['ev']=='config' and e.get('first') and e.get('serial',0)>0 and e['w'] is None for e in events)" \
    "t1: initial configure received (empty size, serial>0)"
assert_json "$TMP_DIR/t1.json" \
    "sum(1 for e in events if e['ev']=='commit') >= 2" \
    "t1: client committed buffers"
assert_json "$TMP_DIR/t1.json" \
    "any(e['ev']=='frame' for e in events)" \
    "t1: frame callbacks delivered (render loop alive)"

# ── t2: client owns geometry ─────────────────────────────────────────
say "t2_client_geometry_authority"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" probe --resize-to 800x600 --after-commits 3 --duration 2500 \
    > "$TMP_DIR/t2.json" 2>"$TMP_DIR/t2.err" &
T2_PID=$!
wait_process_exit $T2_PID 10
assert_log "$TMP_DIR/veyra.log" "geometry adopted from client buffer" \
    "t2: veyra adopted the client's committed size"
assert_json "$TMP_DIR/t2.json" \
    "not any(e['ev']=='config' and not e.get('first') and e['w'] is not None for e in events)" \
    "t2: veyra never pushed a sized configure back (no fighting)"
assert_json "$TMP_DIR/t2.json" \
    "any(e['ev']=='commit' and e['w']==800 and e['h']==600 for e in events)" \
    "t2: client committed its own 800x600"

# ── t3: client exit → cleanup + focus replacement ────────────────────
say "t3_client_exit_cleanup"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" probe --duration 6000 \
    > "$TMP_DIR/t3a.json" 2>"$TMP_DIR/t3a.err" &
T3A_PID=$!
sleep 1.5
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" probe --exit-after-commits 1 --duration 3000 \
    > "$TMP_DIR/t3.json" 2>"$TMP_DIR/t3.err" &
T3_PID=$!
wait_process_exit $T3_PID 10
wait_for_log "$TMP_DIR/veyra.log" "surface destroyed" 5
assert_log "$TMP_DIR/veyra.log" "surface destroyed" "t3: veyra cleaned up after client exit"
assert_log "$TMP_DIR/veyra.log" "refocusing after close" "t3: focus replacement selected (I1/H6)"
if kill -0 $T3A_PID 2>/dev/null; then ok "t3: survivor window unaffected"; else bad "t3: survivor window died"; fi
kill $T3A_PID 2>/dev/null
wait $T3A_PID 2>/dev/null

# ── t4: real clients smoke ───────────────────────────────────────────
say "t4_real_clients"
for c in foot weston-terminal weston-simple-shm weston-simple-egl; do
    if ! command -v "$c" >/dev/null 2>&1; then
        skip "t4: $c not installed (optional real-client smoke)"
        continue
    fi
    LINES_BEFORE=$(wc -l < "$TMP_DIR/veyra.log")
    XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$c" > "$TMP_DIR/$c.log" 2>&1 &
    CPID=$!
    sleep 3
    if kill -0 $CPID 2>/dev/null; then ok "t4: $c stayed alive"; else bad "t4: $c died"; fi
    if tail -n +$LINES_BEFORE "$TMP_DIR/veyra.log" | grep -qF "surface mapped"; then
        ok "t4: $c mapped in veyra"
    else
        bad "t4: $c did not map in veyra"
    fi
    kill $CPID 2>/dev/null
    wait $CPID 2>/dev/null
done

# ── t10: client-requested maximize cycle (I4) ─────────────────────────
# Client maps 640x480, requests set_maximized, then unset_maximized.
# Veyra answers with configure(view size, Maximized) and a restore
# configure back to the pre-maximize committed size. The client's spatial
# transform (position/rotation/scale) must be identical before, during
# and after — maximize changes allocated geometry, never the transform.
say "t10_maximize_client_cycle"
# The maximize target is veyra's render size — parse it from the log.
RAW=$(strip_ansi "$TMP_DIR/veyra.log" | grep -F "render size" | grep -F "window_size" | tail -1)
WIN_PAIR=$(echo "$RAW" | sed -E 's/.*\(([^)]*)\).*/\1/')
MW=$(python3 -c "print(round(float('$WIN_PAIR'.split(',')[0])))")
MH=$(python3 -c "print(round(float('$WIN_PAIR'.split(',')[1])))")
JSON_DUMP=1
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" maximizer \
    --maximize-after 2 --unmaximize-after 8 --duration 5000 \
    > "$TMP_DIR/t10.json" 2>"$TMP_DIR/t10.err" &
T10_PID=$!
wait_process_exit $T10_PID 12

assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='request_maximize' for e in events)" \
    "t10: client sent set_maximized"
assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='config' and e['maximized'] and e['w']==$MW and e['h']==$MH for e in events)" \
    "t10: normal→maximize: sized configure with Maximized state"
assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='commit' and e['w']==$MW and e['h']==$MH for e in events)" \
    "t10: client committed the maximized size"
assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='config' and not e['maximized'] and e['w']==640 and e['h']==480 for e in events)" \
    "t10: maximize→normal: restore configure to pre-maximize size"
assert_json "$TMP_DIR/t10.json" \
    "any(e['ev']=='commit' and e['w']==640 and e['h']==480 for e in events)" \
    "t10: client committed the restored size"
assert_json "$TMP_DIR/t10.json" \
    "[i for i,e in enumerate(events) if e['ev']=='request_maximize'][0] < [i for i,e in enumerate(events) if e['ev']=='config' and e['maximized']][0]" \
    "t10: request precedes the maximized configure"
assert_log "$TMP_DIR/veyra.log" "maximize requested" "t10: veyra recorded maximize intent"
assert_log "$TMP_DIR/veyra.log" "maximize fulfilled" "t10: veyra completed the maximize transaction"
assert_log "$TMP_DIR/veyra.log" "unmaximize fulfilled" "t10: veyra completed the unmaximize transaction"

# Requirement 9 — configure → ACK → commit ordering (veyra's view).
REQ=$(strip_ansi "$TMP_DIR/veyra.log" | grep -an "maximize requested" | head -1 | cut -d: -f1)
ACK=$(strip_ansi "$TMP_DIR/veyra.log" | grep -an "client resize acknowledged" | head -1 | cut -d: -f1)
FUL=$(strip_ansi "$TMP_DIR/veyra.log" | grep -an "maximize fulfilled" | head -1 | cut -d: -f1)
if [ -n "$REQ" ] && [ -n "$ACK" ] && [ -n "$FUL" ] && [ "$REQ" -lt "$ACK" ] && [ "$ACK" -lt "$FUL" ]; then
    ok "t10: configure → ACK → commit ordering"
else
    bad "t10: configure → ACK → commit ordering (requested=$REQ ack=$ACK fulfilled=$FUL)"
fi

# Requirements 11+12 — spatial transform untouched by maximize: position,
# rotation and scale identical across map → maximize → unmaximize.
MAP_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -a "surface mapped" | grep -a "client-kit-maximizer" | tail -1)
MAX_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -a "maximize fulfilled" | grep -av "unmaximize" | tail -1)
UNM_LINE=$(strip_ansi "$TMP_DIR/veyra.log" | grep -a "unmaximize fulfilled" | tail -1)
MAP_POS=$(echo "$MAP_LINE" | grep -oE "pos=Vector3 \[[^]]*\]")
MAP_ROT=$(echo "$MAP_LINE" | grep -oE "rot=Quaternion \{[^}]*\}")
MAP_SCALE=$(echo "$MAP_LINE" | grep -oE "scale=Vector3 \[[^]]*\]")
MAX_POS=$(echo "$MAX_LINE" | grep -oE "pos=Vector3 \[[^]]*\]")
MAX_ROT=$(echo "$MAX_LINE" | grep -oE "rot=Quaternion \{[^}]*\}")
MAX_SCALE=$(echo "$MAX_LINE" | grep -oE "scale=Vector3 \[[^]]*\]")
UNM_POS=$(echo "$UNM_LINE" | grep -oE "pos=Vector3 \[[^]]*\]")
UNM_ROT=$(echo "$UNM_LINE" | grep -oE "rot=Quaternion \{[^}]*\}")
UNM_SCALE=$(echo "$UNM_LINE" | grep -oE "scale=Vector3 \[[^]]*\]")
if [ -n "$MAP_POS" ] && [ "$MAP_POS" = "$MAX_POS" ] && [ "$MAP_POS" = "$UNM_POS" ] \
   && [ -n "$MAP_ROT" ] && [ "$MAP_ROT" = "$MAX_ROT" ] && [ "$MAP_ROT" = "$UNM_ROT" ] \
   && [ -n "$MAP_SCALE" ] && [ "$MAP_SCALE" = "$MAX_SCALE" ] && [ "$MAP_SCALE" = "$UNM_SCALE" ]; then
    ok "t10: 3D transform (position/rotation/scale) preserved across maximize/unmaximize"
else
    bad "t10: 3D transform preserved (map: $MAP_POS $MAP_ROT $MAP_SCALE / max: $MAX_POS $MAX_ROT $MAX_SCALE / unmax: $UNM_POS $UNM_ROT $UNM_SCALE)"
fi
if echo "$MAX_LINE" | grep -qF "client_matched=true"; then
    ok "t10: fulfilled by matching client commit (geometry authority kept)"
else
    bad "t10: maximize fulfilled without matching client commit"
fi

# ── t11: maximize → close → cleanup + fresh relaunch (I4) ─────────────
# A window that exits while maximized must clean up like any other and
# leave no maximize state behind for a relaunched instance.
say "t11_maximize_close_relaunch"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" maximizer \
    --maximize-after 2 --exit-after-commits 8 --duration 6000 \
    > "$TMP_DIR/t11a.json" 2>"$TMP_DIR/t11a.err" &
T11_PID=$!
wait_process_exit $T11_PID 12
wait_for_log "$TMP_DIR/veyra.log" "surface destroyed" 5
assert_log "$TMP_DIR/veyra.log" "surface destroyed" "t11: maximized window cleaned up on exit"
assert_log "$TMP_DIR/veyra.log" "refocusing after close" "t11: focus replacement after maximized close (I1/H6)"
DESTROY_LINES=$(grep -ac "surface destroyed" "$TMP_DIR/veyra.log")
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" maximizer \
    --maximize-after 3 --unmaximize-after 7 --duration 5000 \
    > "$TMP_DIR/t11.json" 2>"$TMP_DIR/t11.err" &
T11B_PID=$!
wait_process_exit $T11B_PID 12
if [ "$DESTROY_LINES" -ge 1 ]; then ok "t11: destroy recorded before relaunch"; else bad "t11: no destroy recorded"; fi
assert_json "$TMP_DIR/t11.json" \
    "any(e['ev']=='config' and e.get('first') and not e['maximized'] for e in events)" \
    "t11: relaunched instance starts unmaximized (no state carry-over)"
assert_json "$TMP_DIR/t11.json" \
    "any(e['ev']=='config' and e['maximized'] and e['w']==$MW and e['h']==$MH for e in events) and any(e['ev']=='config' and not e['maximized'] and e['w']==640 and e['h']==480 for e in events)" \
    "t11: fresh maximize cycle works after maximized close"

say "protocol tests done"
echo "-------------------------------------"
echo "protocol: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
