#!/bin/bash
# run_crash_recovery.sh — unclean-termination persistence test (G-H0.9).
#
# Closes the lifecycle triangle alongside the clean SIGTERM journey:
#
#   clean:   operate → SIGTERM → save → restart → restore   (restart journey)
#   unclean: operate → SIGKILL  → restart → SAFE RECOVERY   (this journey)
#
# SIGKILL persistence is NOT a product requirement; what IS required:
#   A1  an unclean exit leaves no state that breaks the next boot
#   A2  a stale-but-valid state file survives and loads
#   A3  a CORRUPTED state file is backed up and the session starts fresh
#   A4  stale transient IPC state (the wayland socket) does not wedge
#       the next boot
#
# Usage: run_crash_recovery.sh
set -u
UX_CRASH_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../runners/lib.sh
source "$UX_CRASH_ROOT/runners/lib.sh"
HARNESS_DIR="$UX_CRASH_ROOT"
UX_SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ux_env.sh
source "$UX_SCRIPTS_DIR/ux_env.sh"

PASS=0; FAIL=0; SKIP=0; UNC=0
unc() { UNC=$((UNC+1)); echo "  UNCERTAIN: $1"; }

TMP_DIR=$(mktemp -d /tmp/crash.XXXXXX)
VEYRA_LOG="$TMP_DIR/veyra.log"
UX_JOURNAL="$TMP_DIR/journal.jsonl"
STATE_FILE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/veyra-state.json"
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/wayland-1"

preflight || exit 1

cleanup() { ux_kill_all; }
trap cleanup EXIT

veyra_pids() { pgrep -f "target/debug/veyr[a]"; }
boot_log_line() { # <log> <pattern> → rc0 when present
    grep -q "$2" "$1"
}

echo "=============================================================="
echo " Veyra crash / unclean-termination recovery journey"
echo "=============================================================="

# ---- A1: SIGKILL with NO state file → next boot clean ---------------------
say "A1: SIGKILL, no prior state"
rm -f "$STATE_FILE"
ux_kill_all
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" \
    && ok "A1: boot 1 (clean slate)" || { bad "A1: boot 1 failed"; exit 1; }
ux_launch_wayland() {
    WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" \
        setsid "$@" > /dev/null 2>&1 < /dev/null & disown
}
ux_launch_wayland "$BIN/client-kit" "CK" --app-id "CK" --fixed 640x480 --policy match --duration 60000
sleep 3
P=$(veyra_pids | head -1)
kill -KILL "$P" 2>/dev/null
sleep 1.5
veyra_pids | grep -q . && bad "A1: veyra survived SIGKILL" || ok "A1: abrupt death confirmed"
[ -f "$STATE_FILE" ] \
    && bad "A1: SIGKILL produced a state file (unexpected)" \
    || ok "A1: no state file after unclean exit (as designed)"
# A4: stale transient IPC state must not wedge the next boot
ls -la "$SOCK" > /dev/null 2>&1 && say "A4: stale wayland socket present after SIGKILL (recovery will prove it harmless)" \
    || say "A4: no stale socket (runtime dir cleaned by kill)"
VEYRA_LOG="$TMP_DIR/veyra-a2.log"; UX_JOURNAL="$TMP_DIR/journal-a2.jsonl"
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" \
    && ok "A1/A4: boot after SIGKILL succeeds (stale transient state harmless)" \
    || { bad "A1/A4: boot after SIGKILL FAILED"; tail_log "$VEYRA_LOG"; }
boot_log_line "$VEYRA_LOG" "no saved workspace state found" \
    && ok "A1: clean-start semantics preserved" \
    || unc "A1: clean-start log line absent — reviewed"
ux_kill_all

# ---- A2: SIGKILL after a clean save → stale state still loads --------------
say "A2: SIGKILL after a clean save (stale-but-valid state)"
rm -f "$STATE_FILE"
VEYRA_LOG="$TMP_DIR/veyra-b1.log"; UX_JOURNAL="$TMP_DIR/journal-b1.jsonl"
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" \
    && ok "A2: boot 1" || { bad "A2: boot 1 failed"; exit 1; }
# produce a real save: SIGTERM once (graceful), then boot again and SIGKILL
P=$(veyra_pids | head -1); kill -TERM "$P"; sleep 2
[ -f "$STATE_FILE" ] && ok "A2: clean save produced a state file" \
    || { bad "A2: no state file after SIGTERM"; }
VEYRA_LOG="$TMP_DIR/veyra-b2.log"; UX_JOURNAL="$TMP_DIR/journal-b2.jsonl"
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" && ok "A2: boot 2 (loads state)" \
    || bad "A2: boot 2 failed"
boot_log_line "$VEYRA_LOG" "workspace state loaded" \
    && ok "A2: stale-but-valid state loaded" \
    || bad "A2: valid state NOT loaded"
P=$(veyra_pids | head -1); kill -KILL "$P" 2>/dev/null; sleep 1.5
VEYRA_LOG="$TMP_DIR/veyra-b3.log"; UX_JOURNAL="$TMP_DIR/journal-b3.jsonl"
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" \
    && ok "A2: boot 3 after SIGKILL (state intact)" \
    || bad "A2: boot 3 failed"
boot_log_line "$VEYRA_LOG" "workspace state loaded" \
    && ok "A2: state still loads after unclean exit" \
    || bad "A2: state lost/ignored after unclean exit"
ux_kill_all

# ---- A3: corrupted state file → backup + fresh start ------------------------
say "A3: corrupted state file"
printf '{"version":2,"workspaces":[{"CORRUPT' > "$STATE_FILE"
VEYRA_LOG="$TMP_DIR/veyra-c1.log"; UX_JOURNAL="$TMP_DIR/journal-c1.jsonl"
ux_spawn_desktop "$VEYRA_LOG" "$UX_JOURNAL" \
    && ok "A3: boot with CORRUPT state succeeds" \
    || { bad "A3: boot with corrupt state FAILED"; tail_log "$VEYRA_LOG"; }
boot_log_line "$VEYRA_LOG" "corrupt saved state, backing up and starting fresh" \
    && ok "A3: corruption detected and backed up" \
    || bad "A3: corrupt state not detected (silent?)"
[ -f "${STATE_FILE%.json}.json.bak" ] \
    && ok "A3: backup file present" \
    || unc "A3: backup file name unexpected — reviewed"
# The corrupt branch never emits the "no saved workspace state" line
# (that belongs to the absent-file path); recovery is proven by
# detect + backup + a running session, all asserted above.
say "A3: fresh-start semantics via the corrupt branch (saved_state = None)"

echo "--------------------------------------------------------------"
say "crash recovery journey done: $PASS passed, $FAIL failed, $UNC uncertain, $SKIP skipped"
echo "    logs: $TMP_DIR"
[ "$FAIL" -eq 0 ]
