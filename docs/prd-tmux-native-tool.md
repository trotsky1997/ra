# PRD: Native Tmux Built-In Tools

## Overview / Problem Statement

Ra agents need a persistent terminal surface for long-running dev servers,
watchers, REPLs, and interactive CLIs. The existing `bash` tool is a one-shot
execution path: it runs a command, waits for exit, and returns combined
stdout/stderr. That makes it hard to start a process, inspect output later,
send input across turns, or clean up persistent terminal state.

## Goals & Success Metrics

- Agents can create and reuse named tmux sessions without colliding with user
  tmux sessions.
- Agents can run a command in tmux in blocking or non-blocking mode.
- Agents can send input/key names to a running pane.
- Agents can capture visible pane content or scrollback ranges.
- Agents can terminate Ra-owned tmux sessions/windows/panes.
- Agents can wait for new pane output or a pattern without opening a daemonized
  listener.
- Agents can actively block on tmux wait events, including program exit,
  program output, pane output updates, hook expressions, and bounded sleep.
- Missing `tmux` returns structured install guidance instead of an opaque spawn
  error.
- Focused tests cover parameter handling, missing-binary behavior, argv shape,
  catalog registration, and a real tmux round trip when tmux is available.

## User Personas & Stories

- As an agent using a dev server, I want to start it once and inspect its output
  later so that I do not block a turn while the process stays alive.
- As an agent using a REPL or interactive CLI, I want to send input to an
  existing pane so that I can continue the same session across turns.
- As an operator, I want Ra-owned tmux sessions to be namespaced so that agent
  tools cannot accidentally target my personal tmux sessions.
- As an operator, I want a cleanup tool so that agent-created terminal state can
  be removed deliberately.
- As an agent coordinating a long-running terminal workflow, I want one bounded
  wait primitive so that I can wait for completion, output, hooks, or a sleep
  interval without guessing with unbounded polling.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Provide a `tmux_run` built-in tool that creates or reuses a named session/window and runs a command. |
| Must | Support `tmux_run.wait=false` for non-blocking persistent commands and return the target pane metadata. |
| Must | Support `tmux_run.wait=true` for blocking execution with captured pane output and command exit status. |
| Must | Provide a `tmux_send` built-in tool that sends literal input or tmux key names to a target pane. |
| Must | Provide a `tmux_capture` built-in tool that captures pane content with optional `start_line` / `end_line` bounds. |
| Must | Provide a `tmux_kill` built-in tool that terminates Ra-owned sessions, windows, panes, or all `ra__*` sessions. |
| Must | Provide a `tmux_listen` built-in tool that polls until a shared tmux event expression is observed. |
| Must | Provide a `tmux_wait` built-in tool that blocks until `output_update`, `output_match`, `program_exit`, `program_output`, `hook`, or `sleep` resolves or `timeout_ms` expires. |
| Must | Require `tmux_wait.timeout_ms` so active waits are always bounded. |
| Must | Keep `tmux_listen` and `tmux_wait` on the same event expression semantics for `event`, `pattern`, `regex`, and `hook`. |
| Must | Namespace logical session names as `ra__{session}`. |
| Must | Register all six tools in `default_builtins` and respect `[tools].builtin` allow-list filtering. |
| Must | Return structured JSON for tool output, tmux failures, truncation state, and missing-`tmux` guidance. |
| Must | Document JSON schemas and behavior in `spec/tools.md`, README, and sample config. |
| Should | Keep `tmux_listen` bounded by timeout and polling parameters rather than creating a lifecycle daemon. |
| Won't | Add a `[tmux]` config section in this change. |
| Won't | Replace `bash` for one-shot commands. |
| Won't | Inject tmux plugins or custom `tmux.conf` state. |

## Non-Functional Requirements

- Use `tokio::process::Command` and explicit argv arrays for tmux invocations.
- Avoid shell string composition for tmux argv; the user command is a shell
  command only inside the tmux pane.
- Keep outputs bounded through `max_output_bytes` where pane content is
  returned.
- Preserve existing built-in tool behavior and tool allow-list semantics.
- Keep tests deterministic by using fake tmux binaries where possible and
  skipping or isolating real tmux integration behavior when tmux is absent.

## Design Considerations

Ra should treat tmux session state as local host state. The tools therefore run
tmux locally instead of routing through ACP `terminal/*` reverse calls. Logical
session names are validated and mapped to `ra__{session}` targets so all cleanup
and capture operations remain scoped to Ra-owned sessions.

`tmux_listen` is a bounded polling primitive, not a background stream.
`tmux_wait` is the active blocking companion. Both tools use the same event
expression model:

- `output_update`: the pane capture changes after the initial snapshot.
- `output_match`: the pane capture matches `pattern`.
- `program_exit`: a supplied command exits.
- `program_output`: a supplied command produces matching output, or any output
  when no pattern is supplied.
- `hook`: the pane capture matches the same substring/regex expression with an
  optional hook label.
- `sleep`: wait for `duration_ms` bounded by `timeout_ms`.

## Technical Considerations

Implementation lives in `src/tools/tmux.rs` and follows the existing built-in
`Tool` trait pattern. Registration and exports are in `src/tools/mod.rs` and
`src/lib.rs`; UI title/kind hints are in `src/session_runner.rs`.

OpenSpec source of truth is archived under
`openspec/changes/archive/2026-06-01-add-native-tmux-tools/`, and the promoted
capability spec is `openspec/specs/tmux-tools/spec.md`.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| PRD and GitHub issue scope update | Agent | Before implementation handoff completion |
| OpenSpec propose/apply/archive | Agent | Same change |
| Implementation and tests | Agent | Same PR |
| PR and other top model review | Agent / reviewer | After validation |

## Open Questions & Risks

- `tmux_run.wait=true` needs deterministic completion. Ra uses a wrapper script
  and `tmux wait-for`; this intentionally respawns the target pane for blocking
  runs. `tmux_wait` reuses this behavior for program waits.
- `tmux_listen` uses polling rather than tmux control mode. This keeps the tool
  simple and bounded but is not a live event stream.
- Hook waits are expression-based labels over pane output, not tmux-native
  `set-hook` integration.
- Persistent tmux sessions remain after non-blocking runs until an operator or
  agent calls `tmux_kill`.

## Appendix

- GitHub issue: `https://github.com/trotsky1997/ra/issues/24`
- Pull request: `https://github.com/trotsky1997/ra/pull/26`
