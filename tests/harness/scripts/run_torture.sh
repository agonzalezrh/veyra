#!/usr/bin/env bash
# G-I1/G-I2 (lite): lifecycle torture — churn real Wayland clients
# through veyra (open/close waves, size variance, workspace hammering)
# and assert the compositor survives with zero protocol errors and
# zero panics. Single-output scope; output-hotplug and GPU-reset
# variants are hardware-gated (see BUG_LIST #14, G-F1).
#
# Usage: tests/harness/scripts/run_torture.sh [waves] [clients_per_wave]
set -u
cd "$(dirname "$0")/../../.."
BIN=target/debug/veyra
KIT=target/debug/client-kit
WAVES=${1:-10}
PER_WAVE=${2:-10}
RTDIR=/tmp/opencode/veyra-runtime
LOG=/tmp/opencode/torture-veyra.log

[ -x "$BIN" ] || { echo "build first: cargo build && cargo build -p client-kit"; exit 1; }
[ -x "$KIT" ] || { echo "client-kit missing"; exit 1; }

if ! ls /tmp/.X11-unix/X99 >/dev/null 2>&1; then
    setsid Xvfb :99 -screen 0 1280x720x24 >/tmp/opencode/xvfb-torture.log 2>&1 </dev/null &
    disown
    sleep 2
fi
pkill -9 -f "target/debug/veyr[a]" 2>/dev/null
sleep 1

setsid env -u WAYLAND_DISPLAY RUST_LOG="veyra=info,veyra::compositor=debug" \
    XDG_RUNTIME_DIR="$RTDIR" DISPLAY=:99 "$BIN" --normal >"$LOG" 2>&1 </dev/null &
disown
sleep 4
grep -q "render size" "$LOG" || { echo "FAIL: veyra did not start"; exit 1; }

fail=0
total_clients=0
export DISPLAY=:99 WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR="$RTDIR"

echo "=== Phase A: open/close churn ($WAVES waves x $PER_WAVE clients) ==="
for w in $(seq 1 "$WAVES"); do
    pids=()
    for c in $(seq 1 "$PER_WAVE"); do
        total_clients=$((total_clients + 1))
        "$KIT" "churn-w${w}-c${c}" \
            --app-id "churn-w${w}-c${c}" \
            --fixed "$((100 + c * 17))x$((80 + c * 11))" \
            --exit-after-commits 12 \
            --duration 2500 \
            >/dev/null 2>&1 &
        pids+=($!)
    done
    # Workspace hammering while the wave runs.
    xdotool key F2 >/dev/null 2>&1
    sleep 0.15
    xdotool key F3 >/dev/null 2>&1
    sleep 0.15
    xdotool key F1 >/dev/null 2>&1
    for p in "${pids[@]}"; do
        wait "$p" || fail=$((fail + 1))
    done
    alive=$(pgrep -f "target/debug/veyr[a]" | wc -l)
    if [ "$alive" -eq 0 ]; then
        echo "FAIL: veyra died during wave $w"
        exit 1
    fi
    echo "wave $w: ok ($total_clients clients so far)"
done

echo "=== Phase B: rapid resize churn ==="
"$KIT" "resizer" --app-id "resizer" --min 100x80 --max 800x600 \
    --resize-to "640x480" --exit-after-commits 40 --duration 4000 \
    >/dev/null 2>&1 &
rpid=$!
for i in $(seq 1 12); do
    xdotool key F$(( (i % 3) + 1 )) >/dev/null 2>&1
    sleep 0.1
done
wait "$rpid" || fail=$((fail + 1))

echo "=== Assertions ==="
# 1. Compositor alive.
pgrep -f "target/debug/veyr[a]" >/dev/null || { echo "FAIL: veyra not running at end"; exit 1; }
# 2. No panics.
if grep -qE "panicked at" "$LOG"; then
    echo "FAIL: compositor panicked during torture"
    grep -A3 "panicked at" "$LOG" | head -12
    exit 1
fi
# 3. Protocol errors from clients: client-kit exits non-zero on
# protocol errors — `fail` counts them.
echo "clients with non-zero exit: $fail / $total_clients"
if [ "$fail" -gt 0 ]; then
    echo "FAIL: $fail clients exited non-zero (protocol errors?)"
    exit 1
fi

echo "TORTURE: PASS ($total_clients clients churned, compositor healthy)"
exit 0
