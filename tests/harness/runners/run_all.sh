#!/bin/bash
# Runs the complete headless harness: protocol tests (Mode A) and
# input end-to-end tests (Mode B). Both run over SSH without VNC.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
RC=0

echo "=== Veyra headless harness ==="
bash "$HERE/run_protocol_tests.sh" || RC=1
echo
bash "$HERE/run_input_tests.sh" || RC=1
echo
if [ "$RC" -eq 0 ]; then
    echo "=== ALL HARNESS TESTS PASSED ==="
else
    echo "=== HARNESS FAILURES PRESENT ==="
fi
exit $RC
