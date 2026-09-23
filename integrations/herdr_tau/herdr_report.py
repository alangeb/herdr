#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=1
import json, os, re, shlex, shutil, socket, subprocess, sys, time
from pathlib import Path

VALID = {"working", "idle", "blocked", "unknown"}
SRC = "herdr:tau"


def source_for(agent_label=None):
    label = str(agent_label or os.environ.get("HERDR_AGENT") or "tau").strip().lower()
    return f"herdr:{label}"

def norm(state):
    s = str(state or "unknown").strip().lower().replace("_", "-").replace(" ", "-")
    a = {"active": "working", "busy": "working", "waiting": "blocked", "needs-input": "blocked",
         "done": "idle", "complete": "idle", "exited": "idle", "stopped": "idle"}
    s = a.get(s, s)
    return s if s in VALID else "unknown"

def pane():
    return os.environ.get("HERDR_PANE_ID") or os.environ.get("HERDR_SESSION_ID") or "unknown"

def in_herdr():
    v = (os.environ.get("HERDR_ENV") or "").lower()
    if v in {"0", "false", "no", "off"}:
        return False
    return bool(v or os.environ.get("HERDR_PANE_ID") or os.environ.get("HERDR_BIN_PATH")
                or os.environ.get("HERDR_SOCKET_PATH") or os.environ.get("HERDR_STATE_FILE"))

def seq(pid=None):
    pid = pid or pane()
    key = re.sub(r"[^A-Za-z0-9_.-]+", "_", pid)
    root = Path(os.environ.get("HERDR_STATE_DIR") or Path.home() / ".cache" / "herdr_tau")
    try:
        root.mkdir(parents=True, exist_ok=True)
        p = root / (key + ".json")
        n = 0
        if p.exists():
            n = int(json.loads(p.read_text()).get("seq", 0))
        n += 1
        tmp = Path(str(p) + ".tmp")
        tmp.write_text(json.dumps({"pane": pid, "seq": n, "updated": time.time()}) + "\n")
        tmp.replace(p)
        return n
    except Exception:
        return int(time.time() * 1000)

def call(args):
    try:
        return subprocess.run([str(a) for a in args], stdin=subprocess.DEVNULL,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=1).returncode == 0
    except Exception:
        return False

def herdr_bin():
    b = os.environ.get("HERDR_BIN_PATH") or "herdr"
    if not Path(b).exists():
        b = shutil.which(b)
    return b

def fmt(argv, vals):
    out = []
    for x in argv:
        s = str(x)
        for k, v in vals.items():
            s = s.replace("{" + k + "}", str(v))
        out.append(s)
    return out

def report(state, detail="", pane_id=None, agent=None, agent_session_id=None):
    if not in_herdr():
        return True
    pid = pane_id or pane()
    s = norm(state)
    n = seq(pid)
    agent_label = agent or os.environ.get("HERDR_AGENT", "tau")
    source = source_for(agent_label)
    session_id = str(agent_session_id or "").strip()
    vals = {"state": s, "pane": pid, "seq": str(n), "detail": str(detail or ""), "source": source, "agent": agent_label, "agent_session_id": session_id}
    if os.environ.get("HERDR_REPORT_CMD"):
        try:
            if call(fmt(shlex.split(os.environ["HERDR_REPORT_CMD"]), vals)):
                return True
        except Exception:
            pass
    raw = os.environ.get("HERDR_REPORT_ARGS")
    if raw:
        try:
            argv = json.loads(raw)
        except Exception:
            argv = shlex.split(raw)
        if call(fmt(argv, vals)):
            return True
    b = herdr_bin()
    if b:
        # Primary: herdr pane report-agent <pane_id> --source --agent --state --seq --message
        args = [b, "pane", "report-agent", pid,
                "--source", source, "--agent", agent_label,
                "--state", s, "--seq", str(n)]
        if vals["detail"]:
            args += ["--message", vals["detail"]]
        if session_id:
            args += ["--agent-session-id", session_id]
        if call(args):
            return True
        # Legacy fallbacks (older herdr versions)
        for tail in ([b, "state", "set"], [b, "report"]):
            legacy = tail + ["--pane", pid, "--state", s, "--seq", str(n), "--source", source, "--detail", vals["detail"]]
            if call(legacy):
                return True
    sock = os.environ.get("HERDR_SOCKET_PATH")
    if sock and Path(sock).exists():
        for req in (vals, {"action": "report", "pane": pid, "state": s, "seq": n, "source": source, "detail": vals["detail"], "agent-session-id": session_id}):
            try:
                c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                c.settimeout(0.5)
                c.connect(sock)
                c.sendall((json.dumps(req) + "\n").encode())
                try:
                    c.shutdown(socket.SHUT_WR)
                except Exception:
                    pass
                data = c.recv(4096)
                c.close()
                if data and b'"ok":false' not in data and b'"success":false' not in data:
                    return True
            except Exception:
                pass
    sf = os.environ.get("HERDR_STATE_FILE")
    if sf:
        try:
            p = Path(sf).expanduser()
            p.parent.mkdir(parents=True, exist_ok=True)
            p.open("a").write(json.dumps(vals) + "\n")
            return True
        except Exception:
            pass
    sys.stderr.write("herdr_report: failed to report %s\n" % s)
    return False

def _extract_state(o):
    if isinstance(o, str):
        s = o.strip().lower()
        n = norm(s)
        return n if n != "unknown" or s == "unknown" else None
    if isinstance(o, dict):
        for k in ("state", "status", "agent_state", "herdr_state"):
            if k in o:
                v = str(o[k]).strip().lower()
                n = norm(v)
                if n != "unknown" or v == "unknown":
                    return n
        if o.get("waiting") or o.get("waiting_input") or o.get("needs_input") or o.get("blocked"):
            return "blocked"
        for k in ("turn_active", "active", "busy", "working", "running"):
            if k in o:
                return "working" if o[k] else "idle"
        for v in o.values():
            s = _extract_state(v)
            if s:
                return s
    elif isinstance(o, list):
        for v in o:
            s = _extract_state(v)
            if s:
                return s
    return None

def _status_text(t):
    try:
        o = json.loads(t)
        return _extract_state(o)
    except Exception:
        pass
    low = t.lower()
    m = re.search(r"turn_active\s*[:=]\s*(true|false)", low)
    if m:
        return "working" if m.group(1) == "true" else "idle"
    for s in ("working", "idle", "blocked", "unknown"):
        if re.search(r"\b" + s + r"\b", low):
            return s
    return None

def tau_status(root=None):
    cmd = os.environ.get("TAU_A2A_STATUS_CMD") or os.environ.get("TAU_HERDR_STATUS_CMD")
    if cmd:
        try:
            r = subprocess.run(shlex.split(cmd), cwd=str(Path(root).expanduser()) if root else None,
                               stdin=subprocess.DEVNULL, capture_output=True, timeout=0.75)
            out = (r.stdout + r.stderr).decode("utf-8", "replace")
            s = _status_text(out)
            if s:
                return s
        except Exception:
            pass
    pane_id = pane()
    for env in ("HERDR_TAU_STATUS_SOCKET", "TAU_A2A_SOCKET", "TAU_SOCKET", "TAURLM_A2A_SOCKET"):
        sock = os.environ.get(env)
        if not sock or not Path(sock).expanduser().exists():
            continue
        for req in [{"type": "status"}, {"type": "agent_card"}, {"method": "status", "pane": pane_id}]:
            try:
                c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                c.settimeout(0.4)
                c.connect(str(Path(sock).expanduser()))
                c.sendall((json.dumps(req) + "\n").encode())
                data = c.recv(8192).decode("utf-8", "replace")
                c.close()
                s = _status_text(data)
                if s:
                    return s
            except Exception:
                pass
    sentinel = (os.environ.get("TAU_HERDR_SENTINEL") or "0").lower()
    if sentinel not in {"1", "true", "yes", "on"}:
        return None
    path = os.environ.get("TAU_HERDR_SENTINEL_FILE")
    if not path and root:
        path = str(Path(root).expanduser() / ".tau-turn-active")
    if not path or not Path(path).exists():
        return "idle"
    try:
        age = time.time() - Path(path).stat().st_mtime
        txt = Path(path).read_text(errors="ignore").strip().lower()
        s = _status_text(txt)
        if s:
            return s
        return "blocked" if age > float(os.environ.get("TAU_HERDR_SENTINEL_BLOCK_AFTER", "60")) else "working"
    except Exception:
        return "unknown"



def _json_obj(t):
    try:
        return json.loads(t)
    except Exception:
        return None


def _session_from_obj(o):
    if not isinstance(o, dict):
        return ""
    for key in ("agent_session_id", "session_id", "session", "agent-session-id"):
        v = o.get(key)
        if v is not None and str(v).strip():
            return str(v).strip()
    return ""


def _state_session_from_text(t):
    obj = _json_obj(t)
    session_id = _session_from_obj(obj)
    if obj is not None:
        state = _extract_state(obj)
    else:
        state = _status_text(t)
    if not session_id:
        m = re.search(r'(?:agent[-_]session[-_]id|session[-_]id)\s*[:=]\s*([^,\n"}]+)', t, re.I)
        if m:
            session_id = m.group(1).strip().strip('\"')
    return state, session_id


def _a2a_status_with_session(sock_path, pane_id, root=None):
    sock = os.environ.get(sock_path) if not sock_path else sock_path
    sock = os.environ.get(sock_path) if sock.startswith("/") else os.environ.get(sock_path) or sock
    return None


def tau_status_with_session(root=None):
    cmd = os.environ.get("TAU_A2A_STATUS_CMD") or os.environ.get("TAU_HERDR_STATUS_CMD")
    if cmd:
        try:
            r = subprocess.run(shlex.split(cmd), cwd=str(Path(root).expanduser()) if root else None,
                               stdin=subprocess.DEVNULL, capture_output=True, timeout=0.75)
            out = (r.stdout + r.stderr).decode("utf-8", "replace")
            state, session_id = _state_session_from_text(out)
            if state or session_id:
                return state, session_id
        except Exception:
            pass
    pane_id = pane()
    sock_path = None
    for env in ("HERDR_TAU_STATUS_SOCKET", "TAU_A2A_SOCKET", "TAU_SOCKET", "TAURLM_A2A_SOCKET"):
        candidate = os.environ.get(env)
        if candidate and Path(candidate).expanduser().exists():
            sock_path = candidate
            break
    if not sock_path and root:
        # Best effort: A2A sockets are commonly /tmp/taua2a-<pid>.sock, but root alone
        # does not identify the pid; do not scan all tmp sockets.
        pass
    if not sock_path:
        return None, ""
    state = None
    session_id = ""
    for req in [{"type": "status"}, {"type": "agent_card"}]:
        try:
            c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            c.settimeout(0.4)
            c.connect(str(Path(sock_path).expanduser()))
            c.sendall((json.dumps(req) + "\n").encode())
            data = c.recv(16384).decode("utf-8", "replace")
            c.close()
            s, sid = _state_session_from_text(data)
            state = state or s
            session_id = session_id or sid
            if state and session_id:
                break
        except Exception:
            pass
    if state or session_id:
        return state, session_id
    # Fall back to legacy state detection without session identity.
    return tau_status(root), ""


def wait_for_agent_session(root=None, timeout_ms=5000, interval_ms=250):
    end = time.time() + max(0, float(timeout_ms)) / 1000.0
    while time.time() < end:
        state, session_id = tau_status_with_session(root)
        if session_id:
            return session_id
        time.sleep(max(0.05, interval_ms / 1000.0))
    return ""

if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "report":
        sys.exit(0 if report(sys.argv[2], " ".join(sys.argv[3:])) else 1)
    if len(sys.argv) >= 2 and sys.argv[1] == "status":
        print(tau_status(sys.argv[2] if len(sys.argv) > 2 else None) or "unknown")
        sys.exit(0)
    sys.stderr.write("usage: herdr_report.py report STATE [DETAIL] | status [ROOT]\n")
    sys.exit(2)
