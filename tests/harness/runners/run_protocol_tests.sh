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

# Requirements 11+12 — maximize presentation semantics: the maximized
# window is CENTERED on the view (pos origin, identity rotation) and
# unmaximize restores the exact pre-maximize pose (map == unmaximize).
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
MAX_CENTERED="pos=Vector3 [0.0, 0.0, 0.0]"
ID_ROT="rot=Quaternion { v: Vector3 [0.0, 0.0, 0.0], s: 1.0 }"
if [ -n "$MAX_POS" ] && [ "$MAX_POS" = "$MAX_CENTERED" ] \
   && [ -n "$MAX_ROT" ] && [ "$MAX_ROT" = "$ID_ROT" ] \
   && [ -n "$MAP_SCALE" ] && [ "$MAX_SCALE" = "$MAP_SCALE" ]; then
    ok "t10: maximized window centered on view (pos origin, identity rot, scale kept)"
else
    bad "t10: maximized not centered (max: $MAX_POS $MAX_ROT $MAX_SCALE)"
fi
if [ -n "$MAP_POS" ] && [ "$MAP_POS" = "$UNM_POS" ] \
   && [ -n "$MAP_ROT" ] && [ "$MAP_ROT" = "$UNM_ROT" ] \
   && [ -n "$MAP_SCALE" ] && [ "$MAP_SCALE" = "$UNM_SCALE" ]; then
    ok "t10: unmaximize restores exact pre-maximize transform (map == unmaximize)"
else
    bad "t10: transform not restored (map: $MAP_POS $MAP_ROT $MAP_SCALE / unmax: $UNM_POS $UNM_ROT $UNM_SCALE)"
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

# ── t12: client-requested minimize (I5) ──────────────────────────────
# The client sends xdg_toplevel.set_minimized and keeps committing.
# Requirements:
#   - veyra applies the minimize (log) and never destroys the surface
#   - commits CONTINUE while minimized (liveness + latest content)
#   - the focused window's keyboard focus is re-routed
# (Compositor-binding restore coverage lives in the X11 input suite —
# this stack is nested Wayland with no injection device.)
say "t12_minimize_client_request"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" maximizer \
    --minimize-after 3 --duration 9000 \
    > "$TMP_DIR/t12.json" 2>"$TMP_DIR/t12.err" &
T12_PID=$!
wait_process_exit $T12_PID 14

assert_json "$TMP_DIR/t12.json" \
    "any(e['ev']=='request_minimize' for e in events)" \
    "t12: client sent set_minimized"
assert_log "$TMP_DIR/veyra.log" "minimize applied" "t12: veyra applied the minimize (I5)"
assert_log "$TMP_DIR/veyra.log" "refocusing after minimize" "t12: focus replacement on minimize"
# Client kept committing after the minimize request (alive, not frozen):
# at least 2 commits follow the request in the event stream.
assert_json "$TMP_DIR/t12.json" \
    "(lambda rm: len(rm) == 1 and sum(1 for e in events[rm[0]:] if e['ev']=='commit') >= 2)([i for i,e in enumerate(events) if e['ev']=='request_minimize'])" \
    "t12: client commits continue while minimized"
# The minimized surface survives the minimize: the only destroy after the
# last 'minimize applied' is the natural exit at --duration expiry.
LAST_MIN=$(strip_ansi "$TMP_DIR/veyra.log" | grep -an "minimize applied" | tail -1 | cut -d: -f1)
if [ -n "$LAST_MIN" ]; then
    DEST_AFTER=$(strip_ansi "$TMP_DIR/veyra.log" | tail -n +"$((LAST_MIN + 1))" | grep -ac "surface destroyed")
    if [ "$DEST_AFTER" -eq 1 ]; then
        ok "t12: minimized surface survived minimize (destroyed only at exit)"
    else
        bad "t12: minimized surface survived minimize (destroys after minimize: $DEST_AFTER, want 1)"
    fi
else
    bad "t12: no minimize applied line found"
fi
assert_log "$TMP_DIR/veyra.log" "surface destroyed" "t12: exit-after-minimize cleanup works"

# ── t12b: close-while-minimized with a maximized window (I4/I5) ──────
# client: maximize (client-requested) → minimize (client-requested) → exit
say "t12b_maximize_then_minimize_close"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" maximizer \
    --maximize-after 2 --minimize-after 6 --duration 8000 \
    > "$TMP_DIR/t12b.json" 2>"$TMP_DIR/t12b.err" &
T12B_PID=$!
wait_process_exit $T12B_PID 12
assert_json "$TMP_DIR/t12b.json" \
    "any(e['ev']=='request_maximize' for e in events) and any(e['ev']=='request_minimize' for e in events)" \
    "t12b: client requested maximize then minimize"
assert_log "$TMP_DIR/veyra.log" "maximize fulfilled" "t12b: maximize completed before minimize"
LAST_MAX=$(strip_ansi "$TMP_DIR/veyra.log" | grep -an "unmaximize fulfilled\|maximize fulfilled" | grep -av "unmaximize" | tail -1 | cut -d: -f1)
MIN_AFTER_MAX=$(strip_ansi "$TMP_DIR/veyra.log" | tail -n +"$LAST_MAX" | grep -ac "minimize applied")
if [ "$MIN_AFTER_MAX" -ge 1 ]; then
    ok "t12b: minimized while maximized (state layered, transform untouched)"
else
    bad "t12b: minimized while maximized"
fi
wait_for_log "$TMP_DIR/veyra.log" "surface destroyed" 5
assert_log "$TMP_DIR/veyra.log" "surface destroyed" "t12b: close-while-minimized cleaned up"

# ── t15: compositor shutdown with live clients (I6) ──────────────────
# veyra is terminated while clients are connected; the clients must
# notice the socket EOF and exit on their own within the timeout.
say "t15_shutdown_with_clients"
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" probe \
    --duration 20000 > "$TMP_DIR/t15.json" 2>"$TMP_DIR/t15.err" &
T15_PID=$!
sleep 2
# Deregister the EXIT trap's stop_stack so the kill below is the teardown
# (the harness normally stops clients first; here we do it explicitly).
kill "$VEYRA_PID" 2>/dev/null
T15_DEAD=0
for _ in $(seq 1 20); do
    if ! kill -0 "$T15_PID" 2>/dev/null; then T15_DEAD=1; break; fi
    sleep 0.25
done
if [ "$T15_DEAD" -eq 1 ]; then
    ok "t15: client exited on compositor shutdown (socket EOF)"
else
    bad "t15: client did not exit after compositor shutdown"
    kill "$T15_PID" 2>/dev/null
fi

# ── t16: popup creation → dismissal → recreation cycle (I7a) ─────────
# t15 killed veyra; restart a fresh instance so the log is scoped.
say "t16_popup_lifecycle"
start_veyra_nested wayland-harness "$TMP_DIR/veyra.log" || { bad "t16: veyra restarted"; exit 1; }
XDG_RUNTIME_DIR="$VEYRA_RUNTIME" WAYLAND_DISPLAY="$VEYRA_SOCKET" "$BIN/client-kit" popups \
    --cycles 3 --duration 15000 > "$TMP_DIR/t16.json" 2>"$TMP_DIR/t16.err"
POPU_EXIT=$?
CREATED=$(grep -c '"ev":"popup_created"' "$TMP_DIR/t16.json" || true)
MAPPED=$(strip_ansi "$TMP_DIR/veyra.log" | grep -c "popup mapped" || true)
DROPPED=$(strip_ansi "$TMP_DIR/veyra.log" | grep -c "popup destroyed, visual dropped" || true)
if [ "$POPU_EXIT" -eq 0 ] && [ "$CREATED" -eq 3 ]; then
    ok "t16: 3 popup cycles completed (client exits cleanly)"
else
    bad "t16: popup cycles did not complete cleanly (exit=$POPU_EXIT created=$CREATED)"
fi
if [ "$MAPPED" -eq 3 ]; then
    ok "t16: compositor mapped all 3 popups"
else
    bad "t16: compositor popup mappings wrong ($MAPPED != 3)"
fi
if [ "$DROPPED" -eq 3 ]; then
    ok "t16: every client-destroyed popup had its visual dropped (no zombies)"
else
    bad "t16: zombie popups remain ($DROPPED visual drops for 3 destroys)"
fi

say "protocol tests done"
echo "-------------------------------------"
echo "protocol: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
