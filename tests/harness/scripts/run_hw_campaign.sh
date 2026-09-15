#!/bin/bash
# run_hw_campaign.sh — G-F1 real-hardware validation campaign.
#
# PRINCIPLE (user directive): the hardware campaign reuses the EXACT
# same gates as the nested stack — no separate hardware test suite.
#
#            NESTED                     HARDWARE
#   UX gate ────────────┐
#   golden journey ─────┼──→ same scenarios ──→ real DRM backend
#   restart journey ────┤        (libseat → DRM → GBM → EGL →
#   torture ────────────┘        GLES → DMA-BUF → KMS flip)
#
# The only difference is the ENVIRONMENT. An E2E-NESTED PASS with an
# E2E-HW FAIL localizes the defect to the abstraction boundary.
#
# On hardware the two environment shims are required (G-F1 work):
#   input injection:  VEYRA_INPUT_BACKEND=ydotool (uinput) replaces
#                     the XTEST path (there is no Xvfb on a native
#                     session); ux_env.sh routes through it.
#   frame capture:    VEYRA_SHOT_DIR=<dir> — the compositor dumps its
#                     presented frame to PNG (debug path), replacing
#                     the Xvfb `import` screenshot.
#
# Small-first ordering (user directive):
#   f1.1 boot · f1.2 apps · f1.3 golden journey · f1.4 spatial ·
#   f1.5 fullscreen/max/min · f1.6 multi-output · f1.7 restart
# then (clean only): VT switch, hotplug, GPU reset, long soak.
#
# Usage (on the target machine):
#   VEYRA_HW=1 VEYRA_INPUT_BACKEND=ydotool run_hw_campaign.sh [stage...]
set -u
GATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UX_SCRIPTS_DIR="$GATE_DIR"
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="/tmp/veyra-hw-campaign-$STAMP"
mkdir -p "$OUT"
DEVICE="${VEYRA_HW_DEVICE:-/dev/dri/card0}"

hw_available() {
    # Two gates: a real DRM driver AND an explicit human assertion that
    # this machine is the qualification target (a desktop llvmpipe /
    # simple-framebuffer card must not silently "run" the campaign).
    [ "${VEYRA_HW:-0}" = "1" ] || return 1
    [ -c "$DEVICE" ] || return 1
    local driver
    driver=$(basename "$(readlink -f "/sys/class/drm/$(basename "$DEVICE")/device/driver" 2>/dev/null)" 2>/dev/null)
    case "$driver" in
        vkms|vgem|simple-framebuffer|"") return 1 ;;
        *) return 0 ;;
    esac
}

skip_all() {
    echo "[hw] SKIP: no qualifying GPU on $DEVICE (or VEYRA_HW!=1)."
    echo "[hw] Qualification stages (each reuses the NESTED runners):"
    echo "[hw]   f1.1 boot + baseline   → run_ux_gate.sh --fast (native)"
    echo "[hw]   f1.2 apps              → firefox + foot on the native seat"
    echo "[hw]   f1.3 golden journey    → run_golden_journey.sh (native)"
    echo "[hw]   f1.4 spatial battery   → run_ux_gate.sh (native)"
    echo "[hw]   f1.5 fs/max/min        → maximize/fullscreen/minimize scenario"
    echo "[hw]   f1.6 multi-output      → two connectors, straddling window"
    echo "[hw]   f1.7 restart           → run_restart_journey.sh (native)"
    echo "[hw] then: VT switch · hotplug · GPU reset · soak (1h→4h→24h)"
    echo "[hw] Shims needed on hardware: ydotool input + VEYRA_SHOT_DIR capture."
    exit 0
}

# Native-session launcher: no Xvfb — veyra takes the DRM seat directly
# (libseat). The ux_env.sh display split collapses to "no desktop
# display": xdotool/import calls route through the ydotool/VEYRA_SHOT
# shims when VEYRA_INPUT_BACKEND is set.
hw_run() { # <name> <cmd...>
    local name="$1"; shift
    echo "[hw] stage $name → $OUT/$name.log"
    VEYRA_HW_SESSION=1 "$@" > "$OUT/$name.log" 2>&1
    local rc=$?
    [ $rc -eq 0 ] && echo "[hw]   PASS: $name" || echo "[hw]   FAIL: $name (rc=$rc)"
    return $rc
}

FAILED=0
if ! hw_available; then skip_all; fi

for stage in "${@:-f1.1 f1.2 f1.3 f1.4 f1.5 f1.6 f1.7}"; do
    case "$stage" in
        f1.1) hw_run "f1.1-boot-baseline" "$UX_SCRIPTS_DIR/run_ux_gate.sh" --fast || FAILED=1 ;;
        f1.2) echo "[hw] f1.2: launch firefox + foot on the native seat (manual or scripted)"
              echo "[hw]       covered end-to-end by f1.3's golden journey" ;;
        f1.3) hw_run "f1.3-golden" "$UX_SCRIPTS_DIR/run_golden_journey.sh" || FAILED=1 ;;
        f1.4) hw_run "f1.4-spatial" "$UX_SCRIPTS_DIR/run_ux_gate.sh" || FAILED=1 ;;
        f1.5) echo "[hw] f1.5: maximize/fullscreen/minimize scenario (gate extension — G-F1)" ;;
        f1.6) echo "[hw] f1.6: multi-output (two connectors, straddling, cross-output pointer)" ;;
        f1.7) hw_run "f1.7-restart" "$UX_SCRIPTS_DIR/run_restart_journey.sh" || FAILED=1 ;;
        *) echo "[hw] unknown stage $stage" ;;
    esac
done
echo "[hw] campaign done → $OUT (overall: $([ $FAILED -eq 0 ] && echo PASS || echo FAIL))"
exit $FAILED
