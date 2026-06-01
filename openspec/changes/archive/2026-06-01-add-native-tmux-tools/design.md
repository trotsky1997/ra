# Design

## Tool Model

The tmux tools live in `src/tools/tmux.rs` and implement the existing `Tool`
trait. Ra owns typed parameters, session/window target construction,
dependency detection, JSON envelopes, and output bounding. tmux remains the
terminal multiplexer and command runner.

All spawned `tmux` invocations use `tokio::process::Command` with explicit argv
arrays. The user-provided `tmux_run.command` is intentionally a shell command
executed inside tmux, because tmux panes model interactive shells rather than
direct process argv.

## Session Namespacing

Callers pass logical session names such as `dev`. Ra maps those to tmux session
names as `ra__dev`. To keep tmux target strings unambiguous, logical session,
window, and pane identifiers are non-empty and limited to ASCII
letters/digits/`_`/`-`/`.`. This avoids accidental targeting of operator-owned
sessions and keeps `session:window.pane` construction deterministic.

## Command Execution

`tmux_run` ensures the namespaced session/window exists before running a
command.

- `wait=false` creates a detached session/window with the command when the
  target does not exist. When the target already exists, it sends the command
  plus Enter to the pane, matching terminal interaction semantics.
- `wait=true` respawns the target pane with a temporary shell script that runs
  the caller command and signals completion via `tmux wait-for -S <token>`.
  Ra waits for the signal with the caller's timeout, captures the pane output,
  and returns a bounded JSON envelope. The pane remains available for later
  capture.

This design chooses deterministic blocking behavior for `wait=true` rather
than attempting to infer whether an existing prompt is idle.

## Capture, Listen, and Kill

`tmux_capture` maps directly to `tmux capture-pane -p -t <target>` with
optional `-S`/`-E` line bounds and a Ra-side `max_output_bytes` cap.

`tmux_listen` is a bounded polling helper. It takes an initial capture and then
polls until the pane output changes or an optional substring/regex pattern is
found. It returns the latest capture, a best-effort delta when available, and a
timeout flag instead of opening a daemonized stream.

`tmux_kill` only targets Ra-owned namespaced sessions. It can kill a named
session, window, pane, or all `ra__*` sessions.

## Error Handling

If `tmux` is missing, every tool returns JSON with `ok:false`,
`error.kind:"missing_tmux"`, and installation guidance. tmux command failures
also return structured JSON with exit status, stdout, and stderr so callers can
distinguish missing targets from process errors without parsing anyhow strings.
