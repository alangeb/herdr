#!/bin/bash
# Working-tree precedence: src/tau.py found from cwd (or ancestors) must be
# launched instead of the global fallback root; explicit *_ROOT still wins.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

mk_tree() { # $1=dir $2=marker
  mkdir -p "$1/src" "$1/deep/inside"
  cat >"$1/src/tau.py" <<PYEOF
import os, sys
with open("$2", "a") as f: f.write(os.path.abspath(sys.argv[0]) + "\n")
PYEOF
}
mk_tree "$tmp/treeA" "$tmp/mark"
mk_tree "$tmp/treeB" "$tmp/mark"

stub="$tmp/stub-herdr"; printf '#!/bin/sh\nexit 0\n' >"$stub"; chmod +x "$stub"
export HERDR_ENV=1 HERDR_PANE_ID=treeroot HERDR_STATE_DIR="$tmp/cache" HERDR_BIN_PATH="$stub"
unset TAU_ROOT TAURLM_ROOT || true

cd "$tmp/treeA/deep/inside"
timeout 15 "$root/taurlm-herdr" || true
grep -q "^$(readlink -f "$tmp/treeA")/src/tau.py$" "$tmp/mark" || { echo "FAIL: cwd tree not launched"; cat "$tmp/mark" 2>/dev/null; exit 1; }

: >"$tmp/mark"
cd "$tmp/treeA/deep/inside"
TAURLM_ROOT="$tmp/treeB" timeout 15 "$root/taurlm-herdr" || true
grep -q "^$(readlink -f "$tmp/treeB")/src/tau.py$" "$tmp/mark" || { echo "FAIL: env root must win over cwd tree"; cat "$tmp/mark"; exit 1; }

echo "working-tree-precedence OK"
