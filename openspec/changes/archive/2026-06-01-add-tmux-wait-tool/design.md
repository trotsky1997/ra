## Context

The current tmux tools include `tmux_run`, `tmux_send`, `tmux_capture`,
`tmux_kill`, and `tmux_listen`. `tmux_listen` polls pane output until it
changes or a substring/regex appears. `tmux_run.wait=true` already has an
internal deterministic completion path using a temporary script and
`tmux wait-for`, but that completion behavior is only reachable as part of
`tmux_run`.

The new requirement asks for `tmux_wait` as the active blocking version of
`tmux_listen`, with a timeout and support for program completion, program
output/result, tmux output updates, hook triggers, and sleep.

## Goals / Non-Goals

**Goals:**

- Add `tmux_wait` as a first-class built-in tool.
- Keep `tmux_wait` bounded by a caller-supplied `timeout_ms`.
- Share event names and expression matching between `tmux_listen` and
  `tmux_wait`.
- Preserve existing `tmux_listen` calls that omit an explicit event.

**Non-Goals:**

- Do not add a daemonized listener or tmux control-mode stream.
- Do not add a `[tmux]` config section.
- Do not allow tmux tools to target non-`ra__*` sessions.

## Decisions

### Shared Event Expression

Use a single event enum for both listen and wait:

- `output_update`: the pane capture changed after the initial snapshot.
- `output_match`: the pane capture matches `pattern`.
- `program_exit`: a supplied `command` finishes before timeout.
- `program_output`: a supplied `command` produces output matching `pattern`,
  or any new output when no pattern is supplied.
- `hook`: the pane capture matches the same substring/regex expression, with
  an optional `hook` name echoed in the response for caller correlation.
- `sleep`: wait for `duration_ms` without polling tmux.

For compatibility, `tmux_listen` keeps its existing default: without an
explicit event, it behaves as `output_match` when `pattern` is present and
`output_update` otherwise.

### Program Waits Reuse the Blocking Run Path

`tmux_wait` handles `program_exit` and `program_output` by ensuring the target
session/window exists, respawning the pane with the same wrapper-script pattern
used by `tmux_run.wait=true`, and then polling capture output until the wait
event completes or times out. This keeps process completion deterministic and
keeps captured output in the pane for later inspection.

### Hook Semantics

This change treats hooks as named wait/listen expressions over tmux pane
output. The same `pattern` and `regex` fields determine when a hook fires, and
the optional `hook` string labels the returned event. This is intentionally a
tool-level event abstraction, not tmux `set-hook` integration.

## Risks / Trade-offs

- Polling can miss very transient output if the pane scrollback is too small.
  Mitigation: keep `start_line`, `end_line`, and `max_output_bytes` available.
- `program_exit` respawns the target pane for deterministic completion, which
  replaces whatever process was in that pane. Mitigation: document this as the
  same behavior class as `tmux_run.wait=true`.
- Hook events are expression-based rather than tmux-native hooks. Mitigation:
  return the event kind and optional hook label so callers can standardize on
  one expression model now and evolve later if tmux-native hooks are needed.
