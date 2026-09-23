#!/bin/bash
# End-to-end: fake REPL agent publishing an a2a socket AFTER the wrapper's
# startup window; wrapper must adopt + register the session and keep
# reporting state transitions.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

taufake="$tmp/taufake"; mkdir -p "$taufake/src"
cat >"$taufake/src/tau.py" <<'PYEOF'
import json, os, signal, socket, sys, time
sock_path = f"/tmp/taua2a-{os.getpid()}.sock"
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path); srv.listen(8); srv.settimeout(0.5)
start = time.time()
while time.time() - start < 9:
    time.sleep(0.2)  # simulate slow resume: a2a socket appears AFTER the wrapper startup wait
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
try:
    while time.time() - start < 30:
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            continue
        try:
            req = json.loads(conn.recv(4096).decode().splitlines()[0] or "{}")
            turn = (int(time.time() - start) % 6) < 3
            if req.get("type") == "agent_card":
                out = {"type": "agent_card", "mode": "rlm",
                       "session_id": "fake-session-1"}
            else:
                out = {"type": "status_response", "pid": os.getpid(),
                       "turn_active": turn}
            conn.sendall((json.dumps(out) + "\n").encode())
        except Exception:
            pass
        finally:
            conn.close()
finally:
    try: os.unlink(sock_path)
    except OSError: pass
PYEOF

stub="$tmp/stub-herdr"
cat >"$stub" <<'EOF'
#!/bin/sh
log="$HERDR_STUB_LOG"
args="$*"
case " $args " in
  *" report-agent-session "*)
    case " $args " in *"--agent-session-id "*)
      id=$(echo "$args" | sed -n 's/.*--agent-session-id \([^ ]*\).*/\1/p')
      echo "session $id" >> "$log" ;;
    esac
    ;;
  *" report-agent "*)
    state=$(echo "$args" | sed -n 's/.*--state \([^ ]*\).*/\1/p')
    [ -n "$state" ] && echo "$state" >> "$log"
    ;;
  *" release-agent "*)
    echo "release" >> "$log"
    ;;
esac
exit 0
EOF
chmod +x "$stub"

export HERDR_ENV=1 HERDR_PANE_ID=a2atest HERDR_STATE_DIR="$tmp/cache"
export HERDR_STUB_LOG="$tmp/log" HERDR_BIN_PATH="$stub"
export TAU_ROOT="$taufake" TAURLM_ROOT="$taufake" HERDR_TAU_POLL_SEC=0.25
mkdir -p "$tmp/work"; cd "$tmp/work"
timeout 15 "$root/taurlm-herdr" || true

grep -q "^session fake-session-1$" "$tmp/log" || { echo "FAIL: session never registered"; cat "$tmp/log"; exit 1; }
grep -q "^working$" "$tmp/log" || { echo "FAIL: no working report"; cat "$tmp/log"; exit 1; }
grep -q "^idle$" "$tmp/log" || { echo "FAIL: no idle transition reported while agent live"; cat "$tmp/log"; exit 1; }
grep -q "^release$" "$tmp/log" || { echo "FAIL: no release on exit"; cat "$tmp/log"; exit 1; }
# session must be registered even though the a2a socket appeared late:
test "$(grep -c '^session ' "$tmp/log")" -ge 1
echo "a2a late-session + state-transition OK"
