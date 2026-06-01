## Why

Agents need a blocking tmux primitive that can wait for concrete terminal
events without starting a separate listener or relying on ad hoc sleeps. The
existing `tmux_listen` covers bounded polling, but the current tmux tool set
does not expose a single active wait tool for program completion, program
output, pane updates, hooks, and deliberate sleep.

## What Changes

- Add a `tmux_wait` built-in tool with a required `timeout_ms`.
- Extend the shared tmux event expression model so `tmux_listen` and
  `tmux_wait` use the same event names and substring/regex matching semantics.
- Support wait events for `output_update`, `output_match`, `program_exit`,
  `program_output`, `hook`, and `sleep`.
- Update PRD, tool docs, config examples, schemas, implementation, and tests.

## Capabilities

### New Capabilities

### Modified Capabilities

- `tmux-tools`: add the active `tmux_wait` tool and shared event expression
  semantics for `tmux_wait` and `tmux_listen`.

## Impact

- Affected code: `src/tools/tmux.rs`, built-in registration/export paths,
  session runner tool hints, and tmux integration tests.
- Affected docs: tmux PRD, README, tool specification, config examples, and the
  GitHub issue / PR scope text.
- No new external dependencies are required.
