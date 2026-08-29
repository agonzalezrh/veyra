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

say "protocol tests done"
echo "-------------------------------------"
echo "protocol: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
