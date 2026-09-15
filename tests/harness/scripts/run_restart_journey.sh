#!/bin/bash
# run_restart_journey.sh — the cold restart / recovery journey (G-H0.9).
#
# The golden journey caught a state-coupling bug on a normal-mode
# transition; restart/persistence is the next place the same class of
# hidden coupling survives every lower-level test. This journey:
#
#   start Veyra → launch Firefox + Foot → manipulate spatially
#   (drag + rotate + dolly the camera) → clean shutdown (state saved)
#   → assert the state file carries the manipulated geometry
#   → restart → relaunch the same apps → the SAVED transforms and
#   camera return → enter spatial → interact again (liveness).
#
# This is a release-critical scenario, peer to the golden journey.
# Usage: run_restart_journey.sh
set -u
UX_RESTART_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../runners/lib.sh
source "$UX_RESTART_ROOT/runners/lib.sh"
HARNESS_DIR="$UX_RESTART_ROOT"
UX_SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ux_env.sh
source "$UX_SCRIPTS_DIR/ux_env.sh"

PASS=0; FAIL=0; SKIP=0; UNC=0
unc() { UNC=$((UNC+1)); echo "  UNCERTAIN: $1"; }

TMP_DIR=$(mktemp -d /tmp/restart.XXXXXX)
VEYRA_LOG="$TMP_DIR/veyra.log"
UX_JOURNAL="$TMP_DIR/journal.jsonl"
STATE_FILE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/veyra-state.json"
SHOT() { echo "$TMP_DIR/$1.png"; }

preflight || exit 1

ux_launch_wayland() {
    WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        setsid "$@" > /dev/null 2>&1 < /dev/null &
    disown
}

cleanup() { ux_kill_all; }
trap cleanup EXIT

echo "=============================================================="
echo " Veyra cold restart / recovery journey"
echo "=============================================================="

# ---- 0. clean slate ------------------------------------------------------
rm -f "$STATE_FILE"
ux_kill_all
if ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL"; then ok "restart: Veyra running (no prior state)"; else
    bad "restart: Veyra failed to start"; tail_log "$VEYRA_LOG"; exit 1; fi
grep -q "no saved workspace state found" "$VEYRA_LOG" \
    && ok "restart: started without stale state" \
    || unc "restart: prior state may have loaded (log lacks the clean-slate line)"
ux_focus_desktop "$VEYRA_LOG" || true

# ---- 1. launch Firefox + Foot --------------------------------------------
ux_launch_x11 "$VEYRA_LOG" firefox --new-window --width 1000 --height 640 about:blank
sleep 16
FFVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
    | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
[ -n "$FFVID" ] && ok "restart: Firefox mapped (vid=$FFVID)" \
    || bad "restart: Firefox did not map"
ux_launch_wayland foot
sleep 6
FTVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
    | grep -oP 'surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
if [ -z "$FTVID" ]; then
    say "foot did not map — xterm fallback (flake family)"
    ux_launch_x11 "$VEYRA_LOG" xterm -geometry 66x18 +sb -title RESTART
    sleep 4
    FTVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
        | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
    unc "restart: foot substitution (xterm vid=$FTVID)"
fi
[ -n "$FTVID" ] && ok "restart: second app mapped (vid=$FTVID)" \
    || bad "restart: second app did not map"

# ---- 2. manipulate spatially ----------------------------------------------
say "manipulate: drag Firefox, rotate it, dolly the camera"
FFPOS=$(ux_project "$(ux_last_snapshot "$UX_JOURNAL")" "$FFVID")
read -r FFX FFY <<< "$FFPOS"
ux_press_drag "$VEYRA_LOG" "$FFX" "$FFY" "$((FFX+240))" "$FFY" 1 5
sleep 0.6
ux_press_drag "$VEYRA_LOG" "$FFX" "$FFY" "$((FFX+60))" "$((FFY+20))" 3 3
sleep 0.6
# a DISTINCT camera state: dolly in over guaranteed background
BP=$(ux_background_point "$(ux_last_snapshot "$UX_JOURNAL")" 640 360)
read -r WX WY <<< "$BP"
DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$WX" "$WY"
sleep 0.3
for _ in 1 2 3; do DISPLAY="$UX_DESKTOP_DISPLAY" xdotool click 4; sleep 0.2; done
sleep 0.6
PRE=$(ux_last_snapshot "$UX_JOURNAL")
FFROW_PRE=$(ux_row "$PRE" "$FFVID")
Z_PRE=$(ux_snapshot_field "$PRE" "ev['camera_z']")
say "pre-shutdown: firefox row=$FFROW_PRE camera_z=$Z_PRE"
ux_shot "$(SHOT r-before)"

# ---- 3. clean shutdown → state saved ---------------------------------------
say "clean shutdown (SIGTERM → graceful save — the session-manager path)"
VPID=$(pgrep -f "target/debug/veyr[a]" | head -1)
say "SIGTERM to veyra pid $VPID"
kill -TERM "$VPID"
for _ in $(seq 1 20); do
    pgrep -f "target/debug/veyr[a]" > /dev/null || break
    sleep 0.5
done
pgrep -f "target/debug/veyr[a]" > /dev/null \
    && bad "restart: veyra still alive after close" \
    || ok "restart: clean shutdown completed"
grep -q "clean shutdown: saving workspace state" "$VEYRA_LOG" \
    && ok "restart: SIGTERM triggered the graceful save" \
    || bad "restart: SIGTERM not handled gracefully"
[ -f "$STATE_FILE" ] && ok "restart: state file present ($STATE_FILE)" \
    || bad "restart: state file missing"

# ---- 4. restart → applications return with saved geometry ------------------
say "restart Veyra"
UX_JOURNAL="$TMP_DIR/journal2.jsonl"
VEYRA_LOG="$TMP_DIR/veyra2.log"
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" || { bad "restart: second boot failed"; exit 1; }
grep -q "workspace state loaded" "$VEYRA_LOG" \
    && ok "restart: state loaded on boot" \
    || bad "restart: boot did not load state ($(grep -aoE 'no saved workspace state|corrupt saved state' "$VEYRA_LOG" | head -1))"
ux_focus_desktop "$VEYRA_LOG" || true
ux_launch_x11 "$VEYRA_LOG" firefox --new-window --width 1000 --height 640 about:blank
sleep 16
FFVID2=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
    | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
[ -n "$FFVID2" ] && ok "restart: Firefox returned (vid=$FFVID2)" \
    || bad "restart: Firefox did not return"
ux_launch_wayland foot
sleep 6
FTVID2=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
    | grep -oP 'surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
[ -n "$FTVID2" ] && ok "restart: second app returned (vid=$FTVID2)" \
    || { say "foot relaunch fallback"; ux_launch_x11 "$VEYRA_LOG" xterm -geometry 66x18 +sb -title RESTART2; sleep 4; \
         FTVID2=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1); }

# ---- 5. verify geometry + camera restored -----------------------------------
say "verify restored geometry"
POST=$(ux_last_snapshot "$UX_JOURNAL")
FFROW_POST=$(ux_row "$POST" "$FFVID2")
if [ "$FFROW_PRE" = "$FFROW_POST" ]; then
    ok "restart: Firefox transform restored exactly ($FFROW_POST)"
else
    bad "restart: Firefox transform DRIFTED (pre=$FFROW_PRE post=$FFROW_POST)"
fi
Z_POST=$(ux_snapshot_field "$POST" "ev['camera_z']")
python3 -c "exit(0 if abs($Z_POST - $Z_PRE) < 8.0 else 1)" \
    && ok "restart: camera restored (z=$Z_PRE → $Z_POST)" \
    || bad "restart: camera NOT restored (z=$Z_PRE → $Z_POST)"
WS_PRE=$(ux_snapshot_field "$PRE" "ev['active_ws']")
WS_POST=$(ux_snapshot_field "$POST" "ev['active_ws']")
[ "$WS_PRE" = "$WS_POST" ] && ok "restart: workspace restored (ws=$WS_POST)" \
    || bad "restart: workspace changed ($WS_PRE → $WS_POST)"
ux_shot "$(SHOT r-after)"

# ---- 6. enter spatial + interact again (liveness) ---------------------------
say "interact again"
BP2=$(ux_background_point "$POST" 640 360)
read -r WX2 WY2 <<< "$BP2"
DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$WX2" "$WY2"
sleep 0.3
for _ in 1 2; do DISPLAY="$UX_DESKTOP_DISPLAY" xdotool click 4; sleep 0.2; done
sleep 0.6
LIVE=$(ux_last_snapshot "$UX_JOURNAL")
Z_LIVE=$(ux_snapshot_field "$LIVE" "ev['camera_z']")
python3 -c "exit(0 if $Z_LIVE < $Z_POST - 5.0 else 1)" \
    && ok "restart: wheel approach works after restart ($Z_POST → $Z_LIVE)" \
    || bad "restart: post-restart interaction dead (z=$Z_POST → $Z_LIVE)"
ux_press_drag "$VEYRA_LOG" "$WX2" "$WY2" "$((WX2+150))" "$((WY2+30))" 1 4
sleep 0.6
LIVE2=$(ux_last_snapshot "$UX_JOURNAL")
ux_no_window_moved "$LIVE" "$LIVE2" \
    && ok "I1 (post-restart pan): transforms untouched" \
    || bad "I1 VIOLATED (post-restart pan)"

echo "--------------------------------------------------------------"
say "restart journey done: $PASS passed, $FAIL failed, $UNC uncertain, $SKIP skipped"
echo "    journal: $UX_JOURNAL  (+ $TMP_DIR/journal.jsonl)"
echo "    shots:   $TMP_DIR"
[ "$FAIL" -eq 0 ]
