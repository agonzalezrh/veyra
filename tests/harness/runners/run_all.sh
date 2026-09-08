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

# G-B3: native DRM/KMS presentation (verifies what the available
# device allows; software renderers are gated by the M079 check).
bash "$HERE/run_drm_tests.sh" || RC=1
echo
if [ "$RC" -eq 0 ]; then
    echo "=== ALL HARNESS TESTS PASSED ==="
else
    echo "=== HARNESS FAILURES PRESENT ==="
fi
exit $RC
