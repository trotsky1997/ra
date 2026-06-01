## Why

Ra agents need a persistent terminal surface for long-running dev servers,
watchers, REPLs, and interactive CLIs. The existing `bash` tool is a
one-shot shell execution path, so agents cannot reliably start a process,
inspect later output, or send input across turns.

## What Changes

- Add native tmux-backed tools:
  - `tmux_run` starts or reuses a namespaced tmux session/window and runs a
    command in blocking or non-blocking mode.
  - `tmux_send` sends input or tmux key names to a pane.
  - `tmux_capture` captures visible pane content or scrollback ranges.
  - `tmux_kill` terminates a Ra-owned session/window/pane.
  - `tmux_listen` polls a pane until output changes or an optional pattern is
    observed.
- Namespace user session names as `ra__{session}` to avoid collisions with
  operator-owned tmux sessions.
- Return structured JSON envelopes for tmux command results and structured
  install guidance when `tmux` is not on `PATH`.
- Register the tools in the default built-in catalog while respecting the
  `[tools].builtin` allow-list.
- Document the tool schemas in `spec/tools.md`, README, and the sample config.

## Capabilities

### New Capabilities
- `tmux-tools`: Native tmux tool support for persistent terminal sessions.

### Modified Capabilities

None.

## Impact

- Adds `src/tools/tmux.rs` and new exports/registrations in `src/tools/mod.rs`
  and `src/lib.rs`.
- Updates tool UI hints in `src/session_runner.rs`.
- Updates public tool documentation in `spec/tools.md`, README, and
  `spec/ra.toml.example`.
- Adds unit and integration tests for parameter handling, missing binary
  guidance, default catalog registration, argv-safe tmux invocation, and a
  tmux round-trip when `tmux` is installed.
