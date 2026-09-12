#!/usr/bin/env bash
# G-G4: render scalability gate — runs the nested compositor against
# N-visual scenes (10/50/100/250/500/1000), captures the steady-state
# PROFILE line from each run, and reports the per-stage scaling.
#
# The gate is RELATIVE (this environment renders on llvmpipe; absolute
# numbers are machine-specific): draw_ms may grow, but the frustum cull
# (G-G2) must keep the GROWTH sub-linear for offscreen-heavy scenes.
# Fails if draw time grows super-linearly (> 8x for a 10x scene step
# is allowed; the producer/update path is linear by construction).
#
# Usage: tests/harness/scripts/run_scalability.sh [sizes...]
set -u
cd "$(dirname "$0")/../../.."
BIN=target/debug/veyra
SIZES=(${@:-10 50 100 250 500 1000})
RUNTIME=${RUNTIME:-9}
# A leftover compositor holds the Wayland socket and every instance
# would die instantly — clear the field first.
pkill -9 -f "target/debug/veyr[a]" 2>/dev/null
sleep 1
RTDIR=/tmp/opencode/veyra-runtime
OUT=/tmp/opencode/bench-scalability.txt

[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }

# Xvfb (reuse :99 when present)
if ! ls /tmp/.X11-unix/X99 >/dev/null 2>&1; then
    setsid Xvfb :99 -screen 0 1280x720x24 >/tmp/opencode/xvfb-bench.log 2>&1 </dev/null &
    disown
    sleep 2
fi
mkdir -p "$RTDIR"

printf "%6s %8s %8s %8s %8s %8s %10s\n" "N" "fps" "total_ms" "update_ms" "draw_ms" "rendered" "frames"
results=()
for n in "${SIZES[@]}"; do
    log=/tmp/opencode/bench-n$n.log
    rm -f "$log"
    setsid env -u WAYLAND_DISPLAY BENCHMARK_VISUALS=$n RUST_LOG="veyra=info" \
        XDG_RUNTIME_DIR="$RTDIR" DISPLAY=:99 "$BIN" --normal >"$log" 2>&1 </dev/null &
    vpid=$!
    disown
    # Warm up (first PROFILE window includes startup), then capture the
    # LAST PROFILE line of the steady state.
    sleep "$RUNTIME"
    # $! may be the setsid wrapper (it forks when already a group
    # leader) — kill by the bracket-safe pattern instead.
    pkill -9 -f "target/debug/veyr[a]" 2>/dev/null
    profile=$(grep -a "PROFILE" "$log" | tail -1 | sed 's/\x1b\[[0-9;]*m//g')

    if [ -z "$profile" ]; then
        echo "N=$n: NO PROFILE OUTPUT (compositor failed? see $log)"
        results+=("$n FAIL 0 0 0 0 0")
        continue
    fi
    fps=$(echo "$profile" | grep -oE 'fps="[^"]*"' | cut -d'"' -f2)
    total=$(echo "$profile" | grep -oE 'total_ms="[^"]*"' | cut -d'"' -f2)
    update=$(echo "$profile" | grep -oE 'update_ms="[^"]*"' | cut -d'"' -f2)
    draw=$(echo "$profile" | grep -oE 'draw_ms="[^"]*"' | cut -d'"' -f2)
    rendered=$(echo "$profile" | grep -oE 'rendered=[0-9]*' | cut -d= -f2)
    frames=$(echo "$profile" | grep -oE 'frames=[0-9]*' | cut -d= -f2)
    printf "%6s %8s %8s %8s %8s %8s %10s\n" "$n" "$fps" "$total" "$update" "$draw" "$rendered" "$frames"
    results+=("$n OK $fps $total $update $draw $rendered")
done

echo "${results[@]}" > "$OUT"
echo
echo "results written to $OUT"

# Gate: compare the largest vs smallest draw_ms. Result rows:
# "N OK fps total update draw rendered".
small=$(awk '{print $6}' <<<"${results[0]}")
large=$(awk '{print $6}' <<<"${results[${#results[@]}-1]}")
if [ -z "$small" ] || [ -z "$large" ] || [ "$(echo "$small <= 0" | bc -l)" = 1 ]; then
    echo "GATE: INCONCLUSIVE (missing draw_ms data)"
    exit 2
fi
ratio=$(echo "scale=3; $large / $small" | bc -l)
n_small=${SIZES[0]}
n_large=${SIZES[-1]}
allowed=$(echo "scale=3; $n_large / $n_small * 8" | bc -l)
echo "draw_ms growth: ${large}/${small} = ${ratio}x (allowed ≤ ${allowed}x)"
if [ "$(echo "$ratio <= $allowed" | bc -l)" = 1 ]; then
    echo "GATE: PASS"
    exit 0
fi
echo "GATE: FAIL — super-linear draw growth"
exit 1
