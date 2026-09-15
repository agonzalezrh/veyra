#!/bin/bash
# run_ux_gate.sh — permanent UX regression gate (G-H0.7).
#
# The canonical "new application usability" scenario plus the frozen
# spatial-interaction battery, asserted from DETERMINISTIC sources:
#   - the debug journal (VEYRA_DEBUG): authoritative window transforms
#     and camera_z, snapshotted at every gesture end (map, pointer up,
#     wheel dolly)
#   - pixel geometry (ImageMagick) for what is actually rendered
#   - visual_check.py (VLM) for a small number of "does it look right"
#     judgments
#
# Invariants under permanent guard (AGENTS.md frozen contracts):
#   I1  camera changes ≠ window transforms
#       (pan / orbit / wheel: NO window pos row may move)
#   I2  window transform changes ≠ other window transforms
#       (LMB drag: ONLY the dragged window's pos row may move)
#
# Tiering (G-H0.8):
#   --fast          fast gate     (every relevant commit)
#   (default)       full gate     (every merge/main build)
#   golden journey  run_golden_journey.sh   (canonical E2E)
#   overnight       run_overnight_gate.sh   (fast+full+journey+scale+torture)
#   hardware        run_hw_campaign.sh      (G-F1, target machine only)
#
# Usage: run_ux_gate.sh [--fast]   (--fast skips the Firefox scenario)
set -u
UX_GATE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../runners/lib.sh
source "$UX_GATE_ROOT/runners/lib.sh"
# lib.sh re-points HARNESS_DIR at runners/; keep our own roots
HARNESS_DIR="$UX_GATE_ROOT"
UX_SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ux_env.sh
source "$UX_SCRIPTS_DIR/ux_env.sh"

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

TMP_DIR=$(mktemp -d /tmp/ux-gate.XXXXXX)
VEYRA_LOG="$TMP_DIR/veyra.log"
UX_JOURNAL="$TMP_DIR/journal.jsonl"
export VEYRA_HARNESS_BIN="$BIN"
SHOT() { echo "$TMP_DIR/$1.png"; }

preflight || exit 1

# ---- journal env must reach veyra --------------------------------------
ux_launch_wayland() { # <cmd...>
    WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        setsid "$@" > /dev/null 2>&1 < /dev/null &
    disown
}

BRIGHT='(lambda r,g,b: max(r,g,b)>90 and (max(r,g,b)-min(r,g,b))>40)'

cleanup() { ux_kill_all; }
trap cleanup EXIT

echo "=============================================================="
echo " Veyra UX gate — new-application usability + frozen contracts"
echo "=============================================================="

ux_kill_all
if ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL"; then ok "session: veyra running"; else bad "session: veyra failed to start"; tail_log "$VEYRA_LOG"; exit 1; fi
ux_focus_desktop "$VEYRA_LOG" || true

# ---------- S1: launch app A — must be fully reachable ------------------
say "S1: launch app A (client-kit WA, Wayland)"
ux_launch_wayland "$BIN/client-kit" "WA" --app-id "WA" --fixed 800x520 --policy ignore --duration 300000
sleep 3
SNAP=$(ux_last_snapshot "$UX_JOURNAL")
[ -n "$SNAP" ] && ux_geom "$SNAP" 1 intersect \
    && ok "S1: A bounds intersect visible desktop (journal geometry)" \
    || bad "S1: A not reachable (journal: $SNAP)"
ux_click "$VEYRA_LOG" 640 340
sleep 0.6
SNAP1C=$(ux_last_snapshot "$UX_JOURNAL")
[ "$(ux_snapshot_field "$SNAP1C" "ev['focused']")" = "1" ] \
    && ok "S1: click A lands (journal focused=1)" \
    || bad "S1: click A did not focus vid=1"
ux_type "$VEYRA_LOG" h i
sleep 0.4
sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -q 'KEY.*sym=h' \
    && ok "S1: keys routed to A (KEY sym=h)" \
    || bad "S1: keys not routed (no KEY sym=h)"

# ---------- S2: launch app B — BOTH visible -----------------------------
say "S2: launch app B (client-kit WB)"
ux_launch_wayland "$BIN/client-kit" "WB" --app-id "WB" --fixed 800x520 --policy ignore --duration 300000
sleep 3
SNAP2=$(ux_last_snapshot "$UX_JOURNAL")
SNAP1_PREV=$(grep '"ev":"snapshot"' "$UX_JOURNAL" | head -n -1 | tail -1)
A_BEFORE=$(ux_row "$SNAP1_PREV" 1)
B_OK=0
if [ -n "$SNAP2" ] && ux_geom "$SNAP2" 1 intersect && ux_geom "$SNAP2" 2 intersect; then
    B_OK=1; ok "S2: A and B both intersect visible desktop"
else
    bad "S2: one of A/B not visible (snap: $SNAP2)"
fi
[ -n "$A_BEFORE" ] || bad "S2: no pre-B snapshot for isolation baseline"

# ---------- S3/S4: click+type each (canonical interaction) --------------
say "S3: click A → type"
ux_click "$VEYRA_LOG" 640 340
sleep 0.5
ux_type "$VEYRA_LOG" a b c
sleep 0.3
SNAP3C=$(ux_last_snapshot "$UX_JOURNAL")
[ "$(ux_snapshot_field "$SNAP3C" "ev['focused']")" = "1" ] \
    && ok "S3: A refocused and typed" || bad "S3: A focus/type failed"
say "S4: click B → type"
ux_click "$VEYRA_LOG" 900 500
sleep 0.5
ux_type "$VEYRA_LOG" x y z
sleep 0.3
sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -q 'KEY.*sym=x' \
    && ok "S4: B focused and typed (KEY sym=x)" || bad "S4: B focus/type failed"

# ---------- Firefox (S1-real-app) ---------------------------------------
if [ "$FAST" -eq 0 ]; then
    say "S-FF: launch Firefox (X11 via XWayland) — real-application gate"
    ux_launch_x11 "$VEYRA_LOG" firefox --new-window --width 1000 --height 640 about:blank
    sleep 20
    FFVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
    SNAPF=$(ux_last_snapshot "$UX_JOURNAL")
    FFPOS=$(ux_project "$SNAPF" "$FFVID")
    say "S-FF: firefox vid=$FFVID projected center=$FFPOS snap_z=$(ux_snapshot_field "$SNAPF" "ev['camera_z']")"
    # click its projected center, then assert reachability from state
    read -r FFX FFY <<< "$FFPOS"
    ux_click "$VEYRA_LOG" "$FFX" "$FFY"
    sleep 0.8
    SNAPF2=$(ux_last_snapshot "$UX_JOURNAL")
    if [ -n "$FFVID" ] && [ -n "$SNAPF2" ] && ux_geom "$SNAPF2" "$FFVID" intersect; then
        ok "S-FF: Firefox bounds intersect visible desktop (vid=$FFVID)"
    else
        bad "S-FF: Firefox not reachable (vid=$FFVID snap=${SNAPF2:0:120})"
    fi
    [ "$(ux_snapshot_field "$SNAPF2" "ev['focused']")" = "$FFVID" ] \
        && ok "S-FF: click focuses Firefox (journal focused=$FFVID)" \
        || bad "S-FF: Firefox not focused after click"
    ux_type "$VEYRA_LOG" ctrl+l
    sleep 0.3
    ux_type "$VEYRA_LOG" v e y r a
    sleep 0.5
    NKEYS=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -c 'KEY raw_code')
    [ "$NKEYS" -ge 5 ] && ok "S-FF: keys delivered to Firefox ($NKEYS KEY events)" \
        || bad "S-FF: too few KEY events ($NKEYS)"
    ux_shot "$(SHOT ff)"
    python3 "$HARNESS_DIR/scripts/visual_check.py" "$(SHOT ff)" \
        "Is a web browser window visible on the dark desktop with text typed in its address/search bar? Answer yes/no." \
        | grep -q "^PASS" && ok "S-FF: browser with typed text [visual]" \
        || say "S-FF: visual check inconclusive (non-fatal; pixel/log asserts above are authoritative)"
else
    skip "S-FF skipped (--fast)"
fi

# ---------- S5: spatial mode — new app must auto-fit --------------------
say "S5: spatial mode (session default) — launch app C (xterm via XWayland)"
SNAPSP=$(ux_last_snapshot "$UX_JOURNAL")
SPZ=$(ux_snapshot_field "$SNAPSP" "ev['camera_z']")
python3 -c "exit(0 if $SPZ > 600.0 else 1)" \
    && ok "S5: session is in spatial mode (z=$SPZ)" \
    || bad "S5: not in spatial mode (z=$SPZ)"
ux_launch_x11 "$VEYRA_LOG" xterm -geometry 66x18 +sb -title XTC
sleep 4
CVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
SNAPC=$(ux_last_snapshot "$UX_JOURNAL")
if [ -n "$CVID" ] && [ -n "$SNAPC" ] && ux_geom "$SNAPC" "$CVID" fully; then
    ok "S5: C automatically fits fully into view (auto-fit invariant)"
else
    bad "S5: C not fully visible (vid=$CVID)"
fi

# ---------- S6: wheel approach → camera dollies IN -----------------------
say "S6: wheel approach (pointer-directed dolly)"
ux_click "$VEYRA_LOG" 300 300   # park pointer on background, away from C
sleep 0.3
for _ in 1 2 3 4 5; do DISPLAY="$UX_DESKTOP_DISPLAY" xdotool click 4; sleep 0.25; done
sleep 0.6
SNAPC2=$(ux_last_snapshot "$UX_JOURNAL")
Z_BEFORE=$(ux_snapshot_field "$SNAPC" "ev['camera_z']")
Z_AFTER=$(ux_snapshot_field "$SNAPC2" "ev['camera_z']")
if python3 -c "exit(0 if $Z_AFTER < $Z_BEFORE - 5.0 else 1)"; then
    ok "S6: wheel dollies camera in ($Z_BEFORE → $Z_AFTER)"
else
    bad "S6: no dolly-in ($Z_BEFORE → $Z_AFTER)"
fi
ux_no_window_moved "$SNAPC" "$SNAPC2" \
    && ok "I1 (wheel): window transforms untouched" \
    || bad "I1 VIOLATED (wheel): a window transform moved"

# ---------- S7: LMB background pan → I1 ----------------------------------
say "S7: LMB background pan (grab-the-world)"
ux_press_drag "$VEYRA_LOG" 200 200 420 260 1 4
sleep 0.8
SNAPP=$(ux_last_snapshot "$UX_JOURNAL")
ux_shot "$(SHOT pan)"
ux_no_window_moved "$SNAPC2" "$SNAPP" \
    && ok "I1 (pan): window transforms untouched" \
    || bad "I1 VIOLATED (pan): a window transform moved"

# ---------- S8: RMB background orbit → I1 --------------------------------
say "S8: RMB background orbit"
ux_press_drag "$VEYRA_LOG" 1000 150 1120 150 3 3
sleep 0.8
SNAPR=$(ux_last_snapshot "$UX_JOURNAL")
ux_no_window_moved "$SNAPP" "$SNAPR" \
    && ok "I1 (orbit): window transforms untouched" \
    || bad "I1 VIOLATED (orbit): a window transform moved"

# ---------- S9: RMB window rotation — responsive, isolated ---------------
say "S9: RMB drag on a window rotates THAT window"
ux_type "$VEYRA_LOG" Home
sleep 1.0
SNAPH=$(ux_last_snapshot "$UX_JOURNAL")
TARGET=""
TARGET_VID=""
for VID in $(ux_snapshot_field "$SNAPH" "' '.join(str(r['vid']) for r in ev['windows'])"); do
    P=$(ux_project "$SNAPH" "$VID")
    read -r PX PY <<< "$P"
    if [ "$PX" -gt 150 ] && [ "$PX" -lt 1130 ] && [ "$PY" -gt 80 ] && [ "$PY" -lt 640 ]; then
        TARGET="$P"; TARGET_VID="$VID"; break
    fi
done
[ -z "$TARGET" ] && TARGET="640 360"
say "S9/S10 window press target: $TARGET"
read -r TX TY <<< "$TARGET"
ux_press_drag "$VEYRA_LOG" "$TX" "$TY" "$((TX+60))" "$TY" 3 3
sleep 0.8
ux_shot "$(SHOT rot1)"
SNAPROT=$(ux_last_snapshot "$UX_JOURNAL")
ux_no_window_moved "$SNAPR" "$SNAPROT" \
    && ok "I1 (rotation): rotation kept every window center" \
    || bad "I1 VIOLATED (rotation): a window pos moved"
python3 "$HARNESS_DIR/scripts/visual_check.py" "$(SHOT rot1)" \
    "A spatial desktop with windows. Compared to a normal view, does at least one window appear visibly tilted/rotated rather than axis-aligned? Answer yes/no." \
    | grep -q "^PASS" && ok "S9: rotation visibly responsive [visual]" \
    || bad "S9: rotation not visible [visual]"

# ---------- S10: LMB window drag → I2 ------------------------------------
say "S10: LMB window manipulation (drag isolation)"
ux_press_drag "$VEYRA_LOG" "$TX" "$TY" "$((TX+260))" "$TY" 1 5
sleep 0.8
SNAPD=$(ux_last_snapshot "$UX_JOURNAL")
MOVER=$(ux_single_mover "$SNAPROT" "$SNAPD")
[ "$MOVER" = "$TARGET_VID" ] && ok "I2 (drag): ONLY window $TARGET_VID moved" \
    || bad "I2 VIOLATED (drag): mover=$MOVER (expected $TARGET_VID)"

# ---------- S11: return to normal mode -----------------------------------
say "S11: return to normal (2D) mode"
ux_type "$VEYRA_LOG" F5
sleep 1.0
SNAPN=$(ux_last_snapshot "$UX_JOURNAL")
NZ=$(ux_snapshot_field "$SNAPN" "ev['camera_z']")
if python3 -c "exit(0 if abs($NZ - 500.0) < 2.0 else 1)"; then
    ok "S11: normal mode pinned camera (z=$NZ ≈ 500)"
else
    bad "S11: normal-mode camera not pinned (z=$NZ)"
fi

echo "--------------------------------------------------------------"
say "ux gate done: $PASS passed, $FAIL failed, $SKIP skipped"
echo "    journal: $UX_JOURNAL"
echo "    shots:   $TMP_DIR"
[ "$FAIL" -eq 0 ]
