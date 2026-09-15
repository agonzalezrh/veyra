#!/bin/bash
# run_overnight_gate.sh — the nightly tier (G-H0.8).
#
#   fast gate      run_ux_gate.sh --fast      (every relevant commit)
#   full gate      run_ux_gate.sh            (every merge/main build)
#   golden journey run_golden_journey.sh     (canonical E2E)
#   scalability    scripts/run_scalability.sh
#   torture        scripts/run_torture.sh
#
# Usage: run_overnight_gate.sh [--skip-torture]
set -u
GATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$GATE_DIR/../../.." && pwd)"
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="/tmp/veyra-overnight-$STAMP"
mkdir -p "$OUT"
FAILED=0

run_stage() { # <name> <cmd...>
    local name="$1"; shift
    echo "[overnight] stage: $name → $OUT/$name.log"
    if "$@" > "$OUT/$name.log" 2>&1; then
        echo "[overnight]   PASS: $name"
    else
        echo "[overnight]   FAIL: $name (see $OUT/$name.log)"
        FAILED=1
    fi
}

run_stage "fast-gate" "$GATE_DIR/run_ux_gate.sh" --fast
run_stage "full-gate" "$GATE_DIR/run_ux_gate.sh"
run_stage "golden-journey" "$GATE_DIR/run_golden_journey.sh"
run_stage "restart-journey" "$GATE_DIR/run_restart_journey.sh"
run_stage "scalability" "$ROOT/tests/harness/scripts/run_scalability.sh"
if [ "${1:-}" != "--skip-torture" ]; then
    run_stage "torture" "$ROOT/tests/harness/scripts/run_torture.sh"
fi

echo "[overnight] done — artifacts in $OUT (overall: $([ $FAILED -eq 0 ] && echo PASS || echo FAIL))"
exit $FAILED
