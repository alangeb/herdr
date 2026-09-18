#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=1
import os, signal, subprocess, sys, threading, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))
import herdr_report as hr

def find_root(env_root, fallbacks):
    roots = [os.environ.get(env_root)] + fallbacks
    for r in roots:
        if not r:
            continue
        p = Path(r).expanduser()
        if (p / "src" / "tau.py").exists():
            return p
    return None

def run_cmd(source, cmd, cwd, agent_label="tau", launch_cwd=None):
    pane = hr.pane()
    try:
        proc = subprocess.Popen(
            [agent_label] + list(cmd)[1:],
            cwd=str(launch_cwd if launch_cwd is not None else cwd),
            executable=sys.executable,
        )
    except Exception as exc:
        hr.report("unknown", f"launch failed: {exc}", pane, agent=agent_label)
        return 127
    # Register agent session with herdr
    b = hr.herdr_bin()
    try:
        os.environ["TAU_A2A_SOCKET"] = f"/tmp/taua2a-{proc.pid}.sock"
    except Exception:
        pass
    agent_session_id = hr.wait_for_agent_session(cwd, timeout_ms=8000, interval_ms=250)
    if b:
        session_args = [b, "pane", "report-agent-session", pane,
                        "--source", hr.source_for(agent_label), "--agent", agent_label]
        if agent_session_id:
            session_args += [
                "--agent-session-id", agent_session_id,
                "--session-start-source", "startup",
                "--seq", "0",
            ]
        hr.call(session_args)
    hr.report("working", "child started", pane, agent=agent_label, agent_session_id=agent_session_id)
    last = ["working"]
    stop = threading.Event()
    def monitor():
        nonlocal agent_session_id
        interval = max(0.25, float(os.environ.get("HERDR_TAU_POLL_SEC", "1.0")))
        while not stop.is_set():
            time.sleep(interval)
            if proc.poll() is not None:
                break
            state, session_id = hr.tau_status_with_session(cwd)
            if session_id:
                agent_session_id = session_id
            if state and state != last[0]:
                if hr.report(state, "status monitor", pane, agent=agent_label, agent_session_id=agent_session_id):
                    last[0] = state
    threading.Thread(target=monitor, daemon=True).start()
    seen = [False]
    signaled = [None]
    def handler(sig, frame):
        signaled[0] = sig
        try:
            proc.terminate()
        except Exception:
            pass
    for sig in (signal.SIGINT, signal.SIGTERM, getattr(signal, "SIGHUP", signal.SIGTERM)):
        try:
            signal.signal(sig, handler)
        except Exception:
            pass
    while proc.poll() is None:
        if signaled[0] is not None and not seen[0]:
            hr.report("blocked", f"wrapper signal {signaled[0]}", pane, agent=agent_label)
            seen[0] = True
        time.sleep(0.1)
    stop.set()
    rc = proc.returncode
    hr.report("idle", f"child exited ({rc})", pane, agent=agent_label,
              agent_session_id=agent_session_id)
    try:
        b = hr.herdr_bin()
        if b:
            hr.call([b, "pane", "release-agent", pane, "--source", hr.source_for(agent_label), "--agent", agent_label])
    except Exception:
        pass
    return rc if isinstance(rc, int) else 1

def main(source, env_root, fallbacks, agent_label=None):
    agent_label = agent_label or source
    os.environ.setdefault("HERDR_AGENT", agent_label)
    if (os.environ.get("HERDR_TAU_TEST_CHILD") or "").lower() in {"1", "true", "yes", "on"}:
        return run_cmd(source, [agent_label, "-c", "print('herdr-wrapper-test-child')"], Path.cwd(), agent_label=agent_label)
    root = find_root(env_root, fallbacks)
    if not root or not (root / "src" / "tau.py").exists():
        hr.report("unknown", f"{env_root}/src/tau.py not found", agent=agent_label)
        print(f"{source}: cannot find src/tau.py; set {env_root}", file=sys.stderr)
        return 2
    tau_script = str(root / "src" / "tau.py")
    return run_cmd(source, [agent_label, tau_script] + sys.argv[1:], root,
                   agent_label=agent_label, launch_cwd=os.getcwd())
