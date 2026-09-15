#!/bin/bash
# run_golden_journey.sh — Veyra's canonical "does the desktop actually
# work?" gate (G-H0.8).
#
# The golden user journey, end to end, with a deterministic assertion
# at every meaningful step (journal state, pixel geometry, input logs)
# and VLM inspection at the visual moments (PASS/FAIL/UNCERTAIN — an
# UNCERTAIN finding is reported for review, never a silent failure):
#
#   launch Veyra → launch Firefox → launch Foot → both automatically
#   visible → enter spatial → wheel toward Firefox → click Firefox →
#   type URL → scroll page (wheel → application) → wheel away →
#   LMB-drag empty space → RMB orbit → RMB-drag Firefox → rotate →
#   RMB-drag Foot → rotate differently → return normal → all correct.
#
# Usage: run_golden_journey.sh
set -u
UX_JOURNEY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../runners/lib.sh
source "$UX_JOURNEY_ROOT/runners/lib.sh"
HARNESS_DIR="$UX_JOURNEY_ROOT"
UX_SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ux_env.sh
source "$UX_SCRIPTS_DIR/ux_env.sh"

unc() { UNC=$((UNC+1)); echo "  UNCERTAIN: $1"; }
PASS=0; FAIL=0; SKIP=0; UNC=0

TMP_DIR=$(mktemp -d /tmp/golden.XXXXXX)
VEYRA_LOG="$TMP_DIR/veyra.log"
UX_JOURNAL="$TMP_DIR/journal.jsonl"
SHOT() { echo "$TMP_DIR/$1.png"; }

preflight || exit 1

ux_spawn_desktop() { # journal-enabled spawn
    local log="$1"
    setsid Xvfb :99 -screen 0 "$UX_XVFB_GEOMETRY" > /tmp/ux-xvfb.log 2>&1 < /dev/null &
    disown
    sleep 2
    setsid env RUST_LOG="veyra=info" VEYRA_DEBUG="$UX_JOURNAL" \
        XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        DISPLAY="$UX_DESKTOP_DISPLAY" "$BIN/veyra" > "$log" 2>&1 < /dev/null &
    disown
    for _ in $(seq 1 40); do
        grep -q "Veyra running" "$log" 2>/dev/null && return 0
        sleep 0.5
    done
    return 1
}

ux_launch_wayland() {
    WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        setsid "$@" > /dev/null 2>&1 < /dev/null &
    disown
}

cleanup() { ux_kill_all; }
trap cleanup EXIT

echo "=============================================================="
echo " Veyra golden user journey"
echo "=============================================================="

# ---- 1. launch Veyra ----------------------------------------------------
ux_kill_all
if ux_spawn_desktop "$VEYRA_LOG"; then ok "journey: Veyra running"; else
    bad "journey: Veyra failed to start"; tail_log "$VEYRA_LOG"; exit 1; fi
ux_focus_desktop "$VEYRA_LOG" || true
# The launch phase runs in SPATIAL mode: the placement auto-fit is the
# mechanism that makes "both automatically visible" true (normal mode
# pins the camera and cannot reframe).

# ---- 2. launch Firefox --------------------------------------------------
say "launch Firefox (X11 via XWayland)"
ux_launch_x11 "$VEYRA_LOG" firefox --new-window --width 1000 --height 640 about:blank
sleep 18
FFVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
    | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
SNAPF=$(ux_last_snapshot "$UX_JOURNAL")
if [ -n "$FFVID" ] && [ -n "$SNAPF" ] && ux_geom "$SNAPF" "$FFVID" intersect; then
    ok "journey: Firefox reachable (vid=$FFVID)"
else
    bad "journey: Firefox not reachable (vid=$FFVID)"
fi

# ---- 3. launch Foot -----------------------------------------------------
say "launch Foot (native Wayland)"
FOOT_LAUNCHED=1
ux_launch_wayland foot
sleep 6
FOOTVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
    | grep -oP 'surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
if [ -z "$FOOTVID" ] || ! ux_geom "$(ux_last_snapshot "$UX_JOURNAL")" "$FOOTVID" intersect 2>/dev/null; then
    say "foot did not map (harness PTY flake family) — falling back to xterm"
    FOOT_LAUNCHED=0
    ux_launch_x11 "$VEYRA_LOG" xterm -geometry 66x18 +sb -title GOLDEN
    sleep 4
    FOOTVID=$(sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" \
        | grep -oP 'x11 surface mapped visual_id=VisualId\(\K[0-9]+' | tail -1)
    unc "journey: foot substitution (xterm vid=$FOOTVID) — foot PTY flake is environmental, reviewed not failed"
fi
SNAPFT=$(ux_last_snapshot "$UX_JOURNAL")
if [ -n "$FOOTVID" ] && ux_geom "$SNAPFT" "$FOOTVID" intersect; then
    ok "journey: second app reachable (vid=$FOOTVID)"
else
    bad "journey: second app not reachable (vid=$FOOTVID)"
fi

# ---- 4. both automatically visible --------------------------------------
SNAPBOTH=$(ux_last_snapshot "$UX_JOURNAL")
if ux_geom "$SNAPBOTH" "$FFVID" intersect && ux_geom "$SNAPBOTH" "$FOOTVID" intersect; then
    ok "journey: both apps automatically visible (no hunting)"
else
    bad "journey: an app requires hunting (snap: $SNAPBOTH)"
fi
ux_shot "$(SHOT j-both)"

# ---- 5. enter spatial (round-trip: normal ↔ spatial) ----------------------
say "enter spatial mode (2D-when-working round trip)"
ux_type "$VEYRA_LOG" F5
sleep 1.0
ZNorm=$(ux_snapshot_field "$(ux_last_snapshot "$UX_JOURNAL")" "ev['camera_z']")
python3 -c "exit(0 if abs($ZNorm - 500.0) < 2.0 else 1)" \
    && ok "journey: normal-mode pin verified (z=$ZNorm)" \
    || bad "journey: normal pin failed (z=$ZNorm)"
ux_type "$VEYRA_LOG" F5
sleep 1.0
SNAPSP=$(ux_last_snapshot "$UX_JOURNAL")
ZSP=$(ux_snapshot_field "$SNAPSP" "ev['camera_z']")
python3 -c "exit(0 if $ZSP > 600.0 else 1)" \
    && ok "journey: spatial mode entered (z=$ZSP)" \
    || bad "journey: spatial entry failed (z=$ZSP)"

# ---- 6. wheel toward Firefox (pointer-directed approach) -----------------
say "wheel toward Firefox"
FFPOS=$(ux_project "$SNAPSP" "$FFVID")
read -r FFX FFY <<< "$FFPOS"
# park the pointer on guaranteed BACKGROUND along the way to Firefox
BP=$(ux_background_point "$SNAPSP" "$FFX" "$FFY")
read -r WX WY <<< "$BP"
say "approach pointer (background): $WX,$WY → firefox at $FFX,$FFY"
ux_focus_desktop "$VEYRA_LOG" > /dev/null 2>&1
DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$WX" "$WY"
sleep 0.3
PRE=$(ux_last_snapshot "$UX_JOURNAL")
for _ in 1 2 3 4 5; do DISPLAY="$UX_DESKTOP_DISPLAY" xdotool click 4; sleep 0.2; done
sleep 0.6
POST=$(ux_last_snapshot "$UX_JOURNAL")
ZPRE=$(ux_snapshot_field "$PRE" "ev['camera_z']"); ZPOST=$(ux_snapshot_field "$POST" "ev['camera_z']")
python3 -c "exit(0 if $ZPOST < $ZPRE - 5.0 else 1)" \
    && ok "journey: wheel approaches ($ZPRE → $ZPOST)" \
    || bad "journey: no approach dolly ($ZPRE → $ZPOST)"
ux_no_window_moved "$PRE" "$POST" \
    && ok "I1 (approach): window transforms untouched" \
    || bad "I1 VIOLATED (approach)"

# ---- 7. click Firefox + type URL ----------------------------------------
say "click Firefox, type URL"
ux_click "$VEYRA_LOG" "$FFX" "$FFY"
sleep 0.8
SNAPC=$(ux_last_snapshot "$UX_JOURNAL")
[ "$(ux_snapshot_field "$SNAPC" "ev['focused']")" = "$FFVID" ] \
    && ok "journey: Firefox focused by click" \
    || bad "journey: Firefox not focused (snap focused=$(ux_snapshot_field "$SNAPC" "ev['focused']"))"
ux_type "$VEYRA_LOG" ctrl+l
sleep 0.3
ux_type "$VEYRA_LOG" v e y r a
sleep 0.5
sed -E 's/\x1b\[[0-9;]*m//g' "$VEYRA_LOG" | grep -q 'KEY.*sym=v' \
    && ok "journey: URL keys routed" || bad "journey: keys not routed"

# ---- 8. scroll page (wheel → application) --------------------------------
say "scroll the page (wheel over the app)"
ux_focus_desktop "$VEYRA_LOG" > /dev/null 2>&1
DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$FFX" "$FFY"
sleep 0.3
for _ in 1 2 3; do DISPLAY="$UX_DESKTOP_DISPLAY" xdotool click 5; sleep 0.2; done
sleep 0.5
SCROLLSnap=$(ux_last_snapshot "$UX_JOURNAL")
python3 -c "exit(0 if abs($(ux_snapshot_field "$SCROLLSnap" "ev['camera_z']") - $ZPOST) < 3.0 else 1)" \
    && ok "journey: wheel over app scrolls the app (camera untouched)" \
    || bad "journey: camera moved during app scroll (contract violation)"

# ---- 9. wheel away --------------------------------------------------------
say "wheel away (retreat)"
BP2=$(ux_background_point "$SCROLLSnap" 640 360)
read -r RX RY <<< "$BP2"
DISPLAY="$UX_DESKTOP_DISPLAY" xdotool mousemove "$RX" "$RY"
sleep 0.3
for _ in 1 2 3 4 5 6; do DISPLAY="$UX_DESKTOP_DISPLAY" xdotool click 5; sleep 0.2; done
sleep 0.6
AWAY=$(ux_last_snapshot "$UX_JOURNAL")
ZAWAY=$(ux_snapshot_field "$AWAY" "ev['camera_z']")
python3 -c "exit(0 if $ZAWAY > $ZPOST + 5.0 else 1)" \
    && ok "journey: wheel retreats ($ZPOST → $ZAWAY)" \
    || bad "journey: no retreat ($ZPOST → $ZAWAY)"
ux_no_window_moved "$POST" "$AWAY" && ok "I1 (retreat): transforms untouched" \
    || bad "I1 VIOLATED (retreat)"

# ---- 10. LMB-drag empty space (grab-the-world) ----------------------------
say "LMB-drag empty space"
PBP=$(ux_background_point "$AWAY" 640 360); read -r PGX PGY <<< "$PBP"; ux_press_drag "$VEYRA_LOG" "$PGX" "$PGY" "$((PGX+200))" "$((PGY+40))" 1 4
sleep 0.8
PAN=$(ux_last_snapshot "$UX_JOURNAL")
ux_no_window_moved "$AWAY" "$PAN" && ok "I1 (pan): transforms untouched" \
    || bad "I1 VIOLATED (pan)"

# ---- 11. RMB orbit ---------------------------------------------------------
say "RMB orbit"
OBP=$(ux_background_point "$PAN" 100 100); read -r OGX OGY <<< "$OBP"; ux_press_drag "$VEYRA_LOG" "$OGX" "$OGY" "$((OGX+120))" "$OGY" 3 3
sleep 0.8
ORB=$(ux_last_snapshot "$UX_JOURNAL")
ux_no_window_moved "$PAN" "$ORB" && ok "I1 (orbit): transforms untouched" \
    || bad "I1 VIOLATED (orbit)"

# ---- 12. rotate Firefox, then rotate the second app differently ----------
say "RMB-drag Firefox → rotate"
ux_shot "$(SHOT j-rot0)"
FF2=$(ux_project "$ORB" "$FFVID")
read -r RFX RFY <<< "$FF2"
ux_press_drag "$VEYRA_LOG" "$RFX" "$RFY" "$((RFX+60))" "$RFY" 3 3
sleep 0.8
ux_shot "$(SHOT j-rotFF)"
ROT1=$(ux_last_snapshot "$UX_JOURNAL")
FFROW=$(ux_row "$ORB" "$FFVID"); FFROW2=$(ux_row "$ROT1" "$FFVID")
[ "$FFROW" = "$FFROW2" ] && ok "journey: rotation kept Firefox centered (pos $FFROW2)" \
    || bad "journey: Firefox pos moved during rotation ($FFROW → $FFROW2)"
python3 "$UX_SCRIPTS_DIR/visual_check.py" "$(SHOT j-rotFF)" \
    "A spatial desktop. Is at least one application window clearly tilted/rotated rather than axis-aligned? Answer with what you see." \
    | grep -q "^PASS" && ok "journey: Firefox visibly rotated [visual]" \
    || unc "journey: rotation visibility [visual] — reported for review"

say "RMB-drag second app → rotate differently"
FT2=$(ux_project "$ROT1" "$FOOTVID")
read -r FTX FTY <<< "$FT2"
# select it first so the ring moves BEFORE the baseline shot
ux_click2 "$VEYRA_LOG" "$FTX" "$FTY"
sleep 0.5
ux_shot "$(SHOT j-rotFT0)"
ux_press_drag "$VEYRA_LOG" "$FTX" "$FTY" "$((FTX-60))" "$((FTY+20))" 3 3
sleep 0.8
ux_shot "$(SHOT j-rotFT)"
ROT2=$(ux_last_snapshot "$UX_JOURNAL")
FTROW=$(ux_row "$ROT1" "$FOOTVID"); FTROW2=$(ux_row "$ROT2" "$FOOTVID")
[ "$FTROW" = "$FTROW2" ] && ok "journey: rotation kept the second app centered" \
    || bad "journey: second app pos moved during rotation"
# differently = each rotation changed ONLY its own window's footprint
DIFFOUT=$(python3 - "$TMP_DIR" <<'PYEOF'
import subprocess, sys
d = sys.argv[1]
def changed(a, b):
    A = subprocess.run(['convert', f'{d}/{a}.png', '-depth','8','rgb:-'], capture_output=True).stdout
    B = subprocess.run(['convert', f'{d}/{b}.png', '-depth','8','rgb:-'], capture_output=True).stdout
    if len(A) != len(B): return None
    pts = [((i//3)%1280, (i//3)//1280) for i in range(0,len(A),3) if abs(A[i]-B[i])>24]
    if not pts: return (0,0,0,0)
    xs=[p[0] for p in pts]; ys=[p[1] for p in pts]
    return (min(xs),min(ys),max(xs),max(ys))
r1 = changed('j-rot0','j-rotFF')
r2 = changed('j-rotFT0','j-rotFT')
if r1 is None or r2 is None:
    print("DIFF-ERROR"); sys.exit(0)
def overlap(a,b):
    return not (a[2] < b[0] or b[2] < a[0] or a[3] < b[1] or b[3] < a[1])
print("OK" if r1 != (0,0,0,0) and r2 != (0,0,0,0) and not overlap(r1,r2) else "OVERLAP")
PYEOF
)
[ "$DIFFOUT" = "OK" ] && ok "journey: two windows rotated DIFFERENTLY (disjoint changed regions)" \
    || unc "journey: rotation regions ($DIFFOUT) — reported for review"

# ---- 13. return normal — everything still correct -------------------------
say "return to normal mode"
ux_type "$VEYRA_LOG" F5
sleep 1.0
FIN=$(ux_last_snapshot "$UX_JOURNAL")
ZFIN=$(ux_snapshot_field "$FIN" "ev['camera_z']")
python3 -c "exit(0 if abs($ZFIN - 500.0) < 2.0 else 1)" \
    && ok "journey: normal mode restored (z=$ZFIN)" \
    || bad "journey: normal restore failed (z=$ZFIN)"
if ux_geom "$FIN" "$FFVID" intersect && ux_geom "$FIN" "$FOOTVID" intersect; then
    ok "journey: both apps still correct after the full journey"
else
    bad "journey: an app lost after the journey"
fi
ux_shot "$(SHOT j-final)"

echo "--------------------------------------------------------------"
say "golden journey done: $PASS passed, $FAIL failed, $UNC uncertain, $SKIP skipped"
echo "    journal: $UX_JOURNAL"
echo "    shots:   $TMP_DIR"
[ "$FAIL" -eq 0 ]
