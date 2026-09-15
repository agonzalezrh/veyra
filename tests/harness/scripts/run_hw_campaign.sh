#!/bin/bash
# run_hw_campaign.sh — G-F1 real-hardware validation campaign (stages).
#
# The nested/Xvfb stack cannot exercise the real presentation path.
# This runner executes the F1.1–F1.7 campaign on the TARGET MACHINE
# (a real DRM device with a real GPU). Without qualifying hardware it
# refuses honestly — the VKMS probe validated topology/lifecycle only.
#
# Hardware qualification = the exact chain
#   libseat → DRM → GBM → EGL → GLES → DMA-BUF → KMS fb → page flip
# behaving identically to the nested stack, with the UX gate + golden
# journey as the behavioral oracle.
#
# Usage (on target hardware):
#   VEYRA_HW_DEVICE=/dev/dri/card0 run_hw_campaign.sh [stage...]
# Stages: f1.1 f1.2 f1.3 f1.4 f1.5 f1.6 f1.7  (default: all, in order)
set -u
GATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="/tmp/veyra-hw-campaign-$STAMP"
mkdir -p "$OUT"
DEVICE="${VEYRA_HW_DEVICE:-/dev/dri/card0}"

hw_available() {
    # Two gates: a real DRM driver AND an explicit human assertion that
    # this machine is the qualification target (a desktop llvmpipe card
    # must not silently "run" the campaign).
    [ "${VEYRA_HW:-0}" = "1" ] || return 1
    [ -c "$DEVICE" ] || return 1
    # A REAL accelerator has a driver other than vkms/vgem (VKMS has no
    # GPU: one connector, no hotplug, software clocks — topology only).
    local driver
    driver=$(basename "$(readlink -f "/sys/class/drm/$(basename "$DEVICE")/device/driver" 2>/dev/null)" 2>/dev/null)
    case "$driver" in
        vkms|vgem|"") return 1 ;;
        *) return 0 ;;
    esac
}

skip_stage() { echo "[hw] SKIP $1: $2"; }

stage_f1_1() { # baseline: 1 output, 1 app, normal mode — firefox + foot
    echo "[hw] F1.1 baseline: rendering/input/resize/fullscreen/minimize/restore"
    echo "[hw]   → native session: veyra --native (libseat), then:"
    echo "[hw]     UX gate fast tier + golden journey against the native display"
    echo "[hw]     (screenshots via the DRM fb or an external capture path)"
    return 1 # not executable until a qualified device is present
}

stage_f1_2() { # spatial battery on hardware
    echo "[hw] F1.2 spatial: wheel/pan/orbit/rotation — gate spatial tier"
    return 1
}

stage_f1_3() { # multi-output: different modes, straddling, pointer across
    echo "[hw] F1.3 multi-output: two connectors, different modes"
    return 1
}

stage_f1_4() { # stress: churn batteries
    echo "[hw] F1.4 stress: client/workspace/resize/camera churn"
    return 1
}

stage_f1_5() { # session lifecycle: VT switch, suspend/resume
    echo "[hw] F1.5 session lifecycle: VT switch/return, suspend/resume"
    return 1
}

stage_f1_6() { # GPU failure/recovery
    echo "[hw] F1.6 GPU failure/recovery: context loss, reset, output gone"
    return 1
}

stage_f1_7() { # soak: 1h → 4h → 24h with periodic capture
    echo "[hw] F1.7 soak: periodic screenshot/VLM/memory/frame-time/error count"
    return 1
}

if ! hw_available; then
    skip_stage "all" "no qualifying GPU on $DEVICE (vkms/vgem/absent — topology-only validation)"
    echo "[hw] The F1 campaign requires the target machine. Stages:"
    echo "[hw]   F1.1 baseline · F1.2 spatial · F1.3 multi-output ·"
    echo "[hw]   F1.4 stress · F1.5 session lifecycle · F1.6 GPU recovery · F1.7 soak"
    exit 0
fi

STAGES="${*:-f1.1 f1.2 f1.3 f1.4 f1.5 f1.6 f1.7}"
FAILED=0
for s in $STAGES; do
    "stage_${s//[.]/_}" || FAILED=1
done
echo "[hw] campaign done → $OUT (overall: $([ $FAILED -eq 0 ] && echo PASS || echo FAIL))"
exit $FAILED
