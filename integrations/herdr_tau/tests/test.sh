#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
for f in herdr_report.py herdr_wrapper.py tau_entry.py taurlm_entry.py tests/stub-herdr; do
  python3 -m py_compile "$root/$f"
done
for f in tau-herdr taurlm-herdr; do
  sh -n "$root/$f"
done
log="$(mktemp)"
cache="$(mktemp -d)"
export HERDR_ENV=1 HERDR_PANE_ID=syntax-test HERDR_STATE_DIR="$cache" HERDR_STUB_LOG="$log"
export HERDR_BIN_PATH="$root/tests/stub-herdr" HERDR_TAU_TEST_CHILD=1 HERDR_TAU_POLL_SEC=0.2

"$root/tau-herdr" >/dev/null
grep -q '^working$' "$log" || { echo "FAIL: tau did not report working"; exit 1; }
grep -q '^idle$' "$log" || { echo "FAIL: tau did not report idle"; exit 1; }

# Reset log for taurlm test
: > "$log"
"$root/taurlm-herdr" >/dev/null
grep -q '^working$' "$log" || { echo "FAIL: taurlm did not report working"; exit 1; }
grep -q '^idle$' "$log" || { echo "FAIL: taurlm did not report idle"; exit 1; }

# Test outside herdr (no env vars) - should be safe no-op
unset HERDR_ENV HERDR_PANE_ID HERDR_BIN_PATH HERDR_STUB_LOG HERDR_STATE_DIR HERDR_TAU_TEST_CHILD
python3 "$root/herdr_report.py" report working "test" && echo "safe-outside OK"

rm -f "$log"; rm -rf "$cache"
echo OK
