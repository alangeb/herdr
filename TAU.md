# TAU.md — Tau / TauRLM / Herdr pane-state incident, design, and implementation record

Date: 2026-10-22 local
Primary repo for changes: `/home/alangeb/herdr`
Branch: `tau-selfname-launcher`
Pushed HEAD before this dirty patch: `69b34bf3 fix(tau): sync published agent detection manifests`
Current dirty patch: server shutdown-source observability: signal/API/panic logging, not yet committed or deployed.

## Executive verdict

The Tau/TauRlm integration should treat Tau A2A as coordination/session identity only. Herdr must derive Tau/TauRlm pane lifecycle dots from visible terminal/console evidence, not from Tau A2A `turn_active` or Tau report-agent lifecycle state. Tau/TauRlm rich internal state can be exposed as metadata, but Herdr's dot vocabulary remains idle/working/blocked/unknown.

Target behavior:
- ERROR means visible foreground red text, not the literal word `ERROR` in normal prose.
- A clean prompt means idle/finished.
- A prompt with `[RUNNING]` or equivalent active status means working.
- Nonprompt visible content or ANSI/color activity means working.
- Blocked means no visible prompt plus no visible screen change for a configured duration.
- If Herdr has no fresh visible screen evidence, it should show unknown/dot rather than guess.
- After a live handoff, clients must reconnect/repaint; stale imported pane state can remain unknown until a fresh screen snapshot is available.

## Current live state

- Local Herdr server: pid 2712745, binary `~/.local/bin/herdr`, version 0.9.1, protocol 22, restart_needed: no.
- Local server currently reflects the pushed red-text/published-manifest binary through the earlier live handoff.
- The current dirty observability patch is NOT live until rebuilt and live-handoffed.
- Release, cuda, and spark were deployed with the pushed red-text/published-manifest binary earlier and live-handoffed without killing agents.
- The current TauRLM session is standalone under terminator/mosh (`taurlm-launcher` -> `taurlm`), not under the new local Herdr server.
- Restart/reconnect the Herdr client after every live server handoff. No config reload is required.

## Repo boundaries and prior mistake

The intended implementation repo is `/home/alangeb/herdr`. Earlier in the thread, implementation was mistakenly done in `/home/alangeb/tau` and `/home/alangeb/.herdr/tau` because `PLAN.md` described a Tau-side console-state implementation. Those Tau/installed-integration edits were reverted. No files were edited in `/home/alangeb/.herdr/worktrees/tau/wt-beta`.

Reverted Tau-side artifacts included:
- `/home/alangeb/tau/integrations/herdr/console_state.py`
- `/home/alangeb/tau/integrations/herdr/herdr_report.py`
- `/home/alangeb/tau/integrations/herdr/herdr_wrapper.py`
- `/home/alangeb/tau/integrations/herdr/manifest.sha256`
- `/home/alangeb/tau/integrations/herdr/tau-herdr`
- `/home/alangeb/tau/integrations/herdr/tau_entry.py`
- `/home/alangeb/tau/integrations/herdr/taurlm-herdr`
- `/home/alangeb/tau/integrations/herdr/taurlm_entry.py`
- `/home/alangeb/tau/specs/active/2026-10-20-herdr-console-state.md`
- `/home/alangeb/tau/specs/INDEX.md`
- `/home/alangeb/tau/src/sanity.sh`
- `/home/alangeb/tau/src/tests/test_herdr_drift_stage.py`
- `/home/alangeb/tau/src/tests/test_herdr_console_state.py`
- `/home/alangeb/.herdr/tau` reset to pre-my-change installed integration state.

## Pushed Herdr commits on `tau-selfname-launcher`

Chronological relevant commits:
- `4962bb42 fix(tau): use screen detection for Tau pane state`
- `388662d7 fix(tau): ignore session-only lifecycle hook state`
- `a74f490d fix(tau): tighten Tau screen detection`
- `7d4dbddb feat(tau): classify pane state from visible console ANSI`
- `ed34d877 fix(tau): classify pane errors from red screen text`
- `69b34bf3 fix(tau): sync published agent detection manifests`

Main design:
- Tau and TauRlm are `session_identity_only_integration`.
- `herdr pane report-agent` for Tau/TauRlm updates session identity only.
- Tau/TauRlm dot state is derived from Herdr screen/console-state detection.
- Prompt detection supports Tau `>>>` and TauRlm `rlm[...]>>>`.
- Prompt with typed suffix is idle/finished.
- Prompt with active suffix such as `[RUNNING]` is working.
- Nonprompt visible content is working.
- Rich console state is published as metadata tokens such as `tau-console-state`, `tau-console-detail`, `tau-console-prompt`.

## Console-state implementation

Implemented in Herdr:
- `/home/alangeb/herdr/src/detect/console_state.rs`
- `/home/alangeb/herdr/src/detect/console_state_tests.rs`
- `/home/alangeb/herdr/src/detect/mod.rs`
- `/home/alangeb/herdr/src/pane.rs`
- `/home/alangeb/herdr/src/terminal/state.rs`
- `/home/alangeb/herdr/src/detect/manifests/tau.toml`
- `/home/alangeb/herdr/src/detect/manifests/taurlm.toml`

Behavior:
- The classifier scans visible terminal snapshots, not raw scrollback.
- ANSI SGR state is parsed to detect foreground red text.
- Red normal/bright/xterm-256/truecolor text maps to internal ERROR.
- Red-only spaces do not count.
- Red background does not count.
- Plain prose containing `error`, `ERROR`, `state: error`, or `confirmed` must not cause ERROR or BLOCKED.
- ERROR maps to Herdr dot unknown because Herdr's dot vocabulary does not have an error dot, but rich metadata can still say ERROR.
- After live handoff, pane state can remain unknown until client repaint supplies visible terminal revision/data.

Known limitation and next behavior change needed:
- Current deployed red-text patch is an improvement over prose ERROR regex, but `agent explain` can still explain bundled manifest rules rather than console-state detection.
- A follow-up should make `agent explain` report console-state detector output for Tau/TauRlm and/or emit richer explain metadata.

## Blocked design discussion

The word `BLOCKED` in output is not a lifecycle signal. It is usually prose. Examples of false signals:
- `[TOOL CALL] ... context_mode_blocked ... ]`
- `confirmed`
- `awaiting approval`
- normal answer text containing `blocked`.

Real blocked state should be derived by lack of evidence over time:
- no bottom prompt visible,
- agent process still alive,
- pane not artificially scrolled away from prompt,
- no visible content/color/cursor/scroll change for N seconds.

Recommended starting threshold: 30 seconds.
Suggested internal states:
- FINISHED -> idle
- WORKING_SINGLE/WORKING_DOUBLE -> working
- WAITING_OUTPUT -> working while quiet < threshold
- BLOCKED -> blocked when quiet >= threshold and no prompt
- ERROR -> unknown dot + ERROR metadata
- UNKNOWN -> unknown

Implementation caveat:
- After live handoff, quiet timers can be misleading if a pane has no fresh screen snapshot. Those panes should stay unknown until repaint.
- If a pane is scrolled away from the prompt, do not classify blocked unless detection forces bottom-of-buffer or uses live output deltas.

## Red-text ERROR rationale

Tau and TauRlm use Colors.RED specifically for errors/validation failures:
- `/home/alangeb/tau/src/agent_models.py` defines `ERROR = Colors.RED`.
- Tau `display_error` is red.
- Warnings are yellow, answers green, harness/reasoning separate.
- A red-based detector avoids false positives from normal prose.

Caveat: if red text remains visible, internal ERROR remains. It clears when the red text scrolls out or the pane clears. If red diff deletions are common, we may need exceptions later, but first rule remains simple: visible foreground red means Tau/TauRlm ERROR.

## wt-requirements and taul/wt-compress observed symptoms

Observed after live handoff:
- Some panes showed dot `.`/unknown despite agents alive.
- Cause: imported pane had terminal revision 0 or no fresh visible repaint. The new Tau console-state detector needs visible screen evidence.
- For the `/home/alangeb/tau` pane and for `wt-tester`, `wt-compress`, and `wt-planner`, process-info showed the agent wrapper and child process alive. The dot was not agent death; it was stale or missing state evidence.

Old prose-based failures:
- `wt-compress`: normal prose contained `error`; old ERROR regex falsely mapped ERROR -> unknown.
- `wt-tester`: prose contained `confirmed`/blocked-like terms; old blocked regex falsely mapped blocked.
- `taul`/`/home/alangeb/tau`: old ERROR regex matched `state: error` in prose; red-text patch fixes the rule.
- `wt-planner`: stale imported blocked state plus broad blocked regex; repaint corrected to working.

Current expectation after client reconnect/repaint:
- Idle panes with clean `>>>` should become idle/finished.
- Mid-turn panes with content/[RUNNING] should become working.
- Panes with red error chrome should be internal ERROR and dot unknown.
- Panes with no visible prompt and no updates for threshold should become blocked.
- Panes with no fresh screen evidence should remain unknown.

## Server crash incident: local agents lost

Time of loss:
- Old server died around 2026-10-22 01:34 local / 2026-10-21 23:34 UTC.
- New server started around 2026-10-22 02:53 local when the client was restarted.
- Current old server process pid 1132898 is gone.

Observed evidence:
- Server log shows pane sessions terminated with Hangup/Terminate/Kill beginning at 2026-10-21T23:34:15.864Z.
- Tau agent audit logs show SESSION_END `reason=signal:SIGHUP` shortly after.
- Other agents report I/O errors as their PTYs are torn down.
- Old client stayed alive until later and logged `endpoint transport failed endpoint=local error=connection was lost`.
- No `app.shutdown` final log line appeared for the old server.
- No explicit API `server.stop` completion appeared in the log at that time.
- Host-shutdown D-Bus path did not log.
- Mosh/client restart alone was not the cause: old client stayed alive and server was detached.
- A pure OOM/SIGKILL is unlikely because orderly PaneRuntime kill logs appear. SIGKILL cannot emit those logs.

Most likely explanations:
1. External SIGTERM/SIGINT to the old server pid, causing normal quit/drop path.
2. Explicit `herdr server stop`, though no API stop log was observed.
3. Server panic/abort while already exiting or after cleanup started; stderr was /dev/null and no server panic hook existed.
4. External kill after the server had already started shutdown; final shutdown/snapshot logs were missing.

Crash analysis conclusion:
- Agents were collateral damage of server shutdown/teardown.
- The exact external trigger is not recoverable from current logs.
- Current logs are insufficient because server stderr is /dev/null, signal-source logging was absent, server panic hook was absent, coredumps disabled, and audit/kernel logs need root.

## Daemonization vs crash survivability

Herdr server is already daemonized:
- `setsid()` and detached server process are used.
- Current server has pid=pgid=sid, no controlling tty.
- Client/mosh disconnect alone should not kill it.

Daemonization does not protect agents during a server crash or normal server quit:
- Normal server drop intentionally kills pane sessions.
- Only live handoff sets preserve_processes_on_drop.
- Crash-safe agent survival would need either:
  - a crash-safe preserve/orphan protocol,
  - a persistent agent supervisor,
  - an attached/recoverable PTY design,
  - or a journal + restart/reimport mechanism.
- Therefore: daemonization survives terminal/mosh disconnect, but not Herdr server termination by default.

## Current augmentations just implemented in dirty patch

Files dirty now:
- `/home/alangeb/herdr/src/api/server.rs`
- `/home/alangeb/herdr/src/server/headless.rs`
- `/home/alangeb/herdr/src/server/headless/tests/mod.rs`
- `/home/alangeb/herdr/src/detect/console_state.rs` (clippy cleanup)
- `/home/alangeb/herdr/src/detect/mod.rs` (clippy cleanup)

Implemented observability:
1. `server.stop` API now records a flag and logs:
   - `event="api.server.stop"`
   - `subsystem="api"`
   - `outcome="requested"`

2. Server shutdown path logs:
   - `event="server.quit"`
   - `source=signal|api_stop|app`
   - `signal_name=SIGHUP|SIGINT|SIGQUIT|SIGTERM|none`

3. Unix signal handler now installs raw `sigaction` handlers for SIGHUP/SIGINT/SIGQUIT/SIGTERM.
   - Signal handler writes the signal number to a self-pipe (async-signal-safe enough for this purpose).
   - A watcher thread reads the pipe, stores `LAST_SERVER_SIGNAL`, and wakes the server event loop with `QuitSignal`.
   - `QuitSignal` handling now also sets `should_quit`.

4. Server panic hook added:
   - Logs `event="server.panic"` with location and message before delegating to previous panic hook.
   - This captures panics that previously disappeared because server stderr was `/dev/null`.

5. Live test of built binary on isolated XDG config verified:
   - SIGTERM produced `source=signal signal_name="SIGTERM"`.
   - `herdr server stop` produced API log and `source=api_stop`.

Remaining limitations:
- This captures shutdown source, but cannot recover old incident.
- Does not capture full backtrace unless panic message includes enough.
- Does not install a core dump or journal wrapper. Server still uses file logging; stderr is still detached.
- Does not yet add auditd/eBPF signal tracing.
- Does not yet change server to run under systemd user service.
- Does not yet solve crash-safe agent survival; live handoff remains the supported controlled restart mechanism.

## Commands used for tests

Herdr targeted tests:
- `RUSTUP_TOOLCHAIN=1.96.1 PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR=/tmp/herdr-target cargo test --bin herdr -- server::headless::tests::server_quit_signal api::server::tests::server_stop_control_bypasses_app_channel detect:: terminal::state::`
  Result: 269 passed, 0 failed, 0 ignored, 1? (latest run 269 passed/0 failed/0 ignored; broader run 476 passed, 1 ignored)
- `RUSTUP_TOOLCHAIN=1.96.1 PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR=/tmp/herdr-target cargo build --release`
  Result: success, pre-existing client warnings only.

Isolated live binary tests:
- Start isolated server:
  - `XDG_CONFIG_HOME=/tmp/herdr-augment-test2 /tmp/herdr-target/release/herdr server`
- Send SIGTERM, log contained:
  - `server.shutdown ... source=signal signal_name="SIGTERM"`
- API stop isolated test:
  - Start isolated server.
  - Run `XDG_CONFIG_HOME=/tmp/herdr-augment-test3 /tmp/herdr-target/release/herdr server stop`.
  - Log contained API stop requested and `server.quit ... source=api_stop`.

## Deployment workflow for Herdr changes

For source changes in `/home/alangeb/herdr`:
1. Commit branch.
2. Build locally:
   - `RUSTUP_TOOLCHAIN=1.96.1 PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR=/tmp/herdr-target cargo build --release`
3. Deploy binary to host `~/.local/bin/herdr`.
4. On target host:
   - For x86_64 cuda/release/local, copy the verified local x86_64 release binary.
   - For spark/aarch64, build natively or deploy an aarch64 binary. Do not copy x86_64 binary to spark.
5. Live handoff:
   - `herdr server live-handoff --import-exe ~/.local/bin/herdr --expected-protocol 22 --expected-version 0.9.1`
6. Verify:
   - `herdr status`
   - agent list/status and process survival.
7. Restart/reconnect Herdr client.
8. Restart/reconnect client again after every subsequent live handoff.

Important:
- A live handoff intentionally disconnects clients.
- Pane state may show unknown until client repaint supplies fresh visible terminal snapshots.
- Do not restart Tau agents just to refresh dots; that can lose agent work.
- Do not restart Herdr server without live handoff if agent survival matters.

## Current TODO

1. Review dirty patch and commit it.
2. Rebuild and deploy if you want observability live.
3. Add `agent explain` integration for console-state detector so explain shows actual Tau dot reason.
4. Replace remaining text-based blocked rules for Tau/TauRlm with quiet-duration state.
5. Publish ERROR metadata even when dot remains unknown.
6. Add stronger tests or an integration test that can inspect logger output.
7. Consider journal/systemd capture and coredump/backtrace policy for the server.
8. Consider crash-safe preserve/orphan protocol if agents must survive server crashes without live handoff.

see: herdr-overview, herdr-deploy-process, herdr-integration-build, herdr-wrapper-design, herdr-integration-decision
