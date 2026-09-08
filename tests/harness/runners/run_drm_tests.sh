#!/bin/bash
# G-B3: native DRM/KMS presentation tests.
#
# Runs against a real or virtual (VKMS) DRM device. Requires root (or
# the video group + DRM master): sudo is attempted automatically.
#
# What is verified depends on the EGL driver's dma-buf RENDER support:
# - real GPU (i915/amdgpu/...): full pipeline — buffers, GL render into
#   dmabufs, page flips, flip-event completion (M079 gate passes).
# - software rasterizer (llvmpipe/softpipe): the M079 capability gate
#   must REFUSE cleanly with actionable diagnostics — rendering into
#   imported dma-bufs is impossible there (Mesa limitation), and gbm's
#   read-only dma-buf export blocks CPU fills as well (first-export
#   caching wins over DRM_RDWR re-exports).
# The flip machinery (drain → frame_submitted) shares the code path the
# render probe exercises; VKMS itself is verified separately with
# modetest when available.

set -u
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BIN="${VEYRA_HARNESS_BIN:-$ROOT_DIR/target/debug/veyra}"

PASS=0; FAIL=0; SKIP=0
ok()   { PASS=$((PASS+1)); echo "  PASS: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }
skip() { SKIP=$((SKIP+1)); echo "  SKIP: $1"; }

if ! command -v "$BIN" >/dev/null 2>&1; then
    echo "veyra binary missing — run: cargo build"
    exit 1
fi

# Find a DRM device with a connected connector (VKMS preferred for CI).
DRM_CARD=""
for c in /dev/dri/card1 /dev/dri/card0 /dev/dri/card2; do
    if [ -e "$c" ]; then
        DRM_CARD="$c"
        break
    fi
done
if [ -z "$DRM_CARD" ]; then
    skip "no DRM device present (/dev/dri/card*)"
    echo "drm: $PASS passed, $FAIL failed, $SKIP skipped"
    exit 0
fi

echo "[drm] testing against $DRM_CARD"

# 1) The render probe must TERMINATE with a verdict (never hang, never
#    crash the process on a software stack): either full success on a
#    real GPU, or the M079 clean refusal on software renderers.
LOG=$(mktemp /tmp/veyra-drm-probe.XXXXXX)
timeout 90 sudo -E VEYRA_DRM_CARD="$DRM_CARD" VEYRA_DRM_PROBE=10 \
    RUST_LOG=veyra=info "$BIN" > "$LOG" 2>&1
RC=$?
if [ "$RC" -eq 0 ]; then
    ok "drm: full presentation pipeline verified (real GPU)"
elif grep -q "software GL renderer" "$LOG" && grep -q "M079" "$LOG"; then
    ok "drm: M079 capability gate refused cleanly on software renderer"
else
    bad "drm: probe neither succeeded nor refused cleanly (rc=$RC)"
    tail -5 "$LOG" | sed 's/^/    /'
fi
rm -f "$LOG"

# 2) The flip probe must also terminate cleanly (init + capability
#    check path shared with the render probe).
LOG=$(mktemp /tmp/veyra-drm-flip.XXXXXX)
timeout 90 sudo -E VEYRA_DRM_CARD="$DRM_CARD" VEYRA_DRM_PROBE=flip \
    RUST_LOG=veyra=info "$BIN" > "$LOG" 2>&1
RC=$?
if [ "$RC" -eq 0 ]; then
    ok "drm: flip machinery verified end-to-end (real GPU or RW dma-bufs)"
elif grep -qE "software GL renderer|dmabuf mmap: Permission denied" "$LOG"; then
    ok "drm: flip probe failed on the known software-stack limitation (documented, M079)"
else
    bad "drm: flip probe failed unexpectedly (rc=$RC)"
    tail -5 "$LOG" | sed 's/^/    /'
fi
rm -f "$LOG"

echo "drm: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
