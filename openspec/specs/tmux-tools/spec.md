# tmux-tools Specification

## Purpose
Built-in Ra tools that wrap tmux for persistent terminal sessions,
interactive input, pane capture, bounded listening, and Ra-owned target
cleanup.
## Requirements
### Requirement: Native Tmux Tool Catalog

Ra SHALL include `tmux_run`, `tmux_send`, `tmux_capture`, `tmux_kill`,
`tmux_listen`, and `tmux_wait` in the default built-in catalog when
`[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes tmux tools

- **GIVEN** `[tools].builtin` is empty
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog includes `tmux_run`, `tmux_send`, `tmux_capture`,
  `tmux_kill`, `tmux_listen`, and `tmux_wait`

#### Scenario: Non-empty allow-list remains exact

- **GIVEN** `[tools].builtin` contains only `tmux_capture`
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog contains `tmux_capture` and omits the other tmux tools

### Requirement: Namespaced Tmux Sessions

Ra SHALL map logical tmux session names to Ra-owned tmux sessions using the
`ra__{session}` namespace.

#### Scenario: User session name is namespaced

- **GIVEN** a caller uses session `dev`
- **WHEN** any tmux tool builds a tmux target
- **THEN** the target session name is `ra__dev`

### Requirement: Tmux Run Tool

Ra SHALL provide a `tmux_run` tool that creates or reuses a named tmux
session/window and runs a command.

#### Scenario: Non-blocking run returns target

- **GIVEN** the target session/window does not exist
- **WHEN** `tmux_run` is called with `wait:false`
- **THEN** Ra creates the detached target and returns JSON containing the pane
  target without waiting for the process to exit

#### Scenario: Blocking run returns captured output

- **GIVEN** `tmux_run` is called with `wait:true`
- **WHEN** the command exits before the timeout
- **THEN** Ra returns JSON containing `ok:true`, the exit/capture status, and
  the captured pane output

### Requirement: Tmux Send Tool

Ra SHALL provide a `tmux_send` tool that sends input or tmux key names to a
target pane.

#### Scenario: Send input with Enter

- **GIVEN** a running tmux pane
- **WHEN** `tmux_send` is called with `keys:"q"` and `enter:true`
- **THEN** Ra invokes `tmux send-keys` for the target and appends Enter

### Requirement: Tmux Capture Tool

Ra SHALL provide a `tmux_capture` tool that captures target pane content with
optional scrollback line bounds.

#### Scenario: Capture recent history

- **GIVEN** a target pane has output in scrollback
- **WHEN** `tmux_capture` is called with `start_line:-50`
- **THEN** Ra returns a bounded JSON envelope containing the captured text

### Requirement: Tmux Kill Tool

Ra SHALL provide a `tmux_kill` tool that terminates Ra-owned tmux targets.

#### Scenario: Kill a session

- **GIVEN** a namespaced tmux session exists
- **WHEN** `tmux_kill` is called for its logical session name
- **THEN** Ra kills the corresponding `ra__*` tmux session

### Requirement: Tmux Listen Tool

Ra SHALL provide a `tmux_listen` tool that polls a pane until a shared tmux
event expression is observed or the timeout expires.

#### Scenario: Listen sees new output

- **GIVEN** a pane later emits new output
- **WHEN** `tmux_listen` is called with event `output_update`
- **THEN** Ra returns after the capture changes and includes the latest content

#### Scenario: Listen pattern timeout

- **GIVEN** a pane does not emit the requested pattern
- **WHEN** `tmux_listen` reaches its timeout
- **THEN** Ra returns JSON with `timed_out:true` instead of blocking
  indefinitely

#### Scenario: Listen hook expression

- **GIVEN** a pane later emits text matching a hook expression
- **WHEN** `tmux_listen` is called with event `hook`, a `hook` label, and a
  substring or regex pattern
- **THEN** Ra returns JSON identifying the hook event and the matched capture

### Requirement: Missing Tmux Guidance

Ra SHALL return a structured, actionable response when `tmux` is not available
instead of surfacing an opaque spawn failure.

#### Scenario: tmux is missing

- **GIVEN** `tmux` is not found on `PATH`
- **WHEN** any tmux tool executes
- **THEN** the tool returns JSON with `ok:false`, `error.kind:"missing_tmux"`,
  and installation guidance

### Requirement: Tmux Wait Tool

Ra SHALL provide a `tmux_wait` tool that blocks until a selected tmux wait
event occurs or a required timeout expires.

#### Scenario: Wait for program exit

- **WHEN** `tmux_wait` is called with event `program_exit`, a command, and a
  timeout
- **THEN** Ra runs the command in the target pane and returns after the command
  exits or the timeout expires

#### Scenario: Wait for program output

- **WHEN** `tmux_wait` is called with event `program_output`, a command, a
  pattern, and a timeout
- **THEN** Ra returns after the command output matches the expression, the
  command exits, or the timeout expires

#### Scenario: Wait for output update

- **GIVEN** a tmux pane later changes output
- **WHEN** `tmux_wait` is called with event `output_update` and a timeout
- **THEN** Ra returns after the capture changes or the timeout expires

#### Scenario: Wait for hook trigger

- **GIVEN** a tmux pane later emits text matching a hook expression
- **WHEN** `tmux_wait` is called with event `hook`, a `hook` label, a pattern,
  and a timeout
- **THEN** Ra returns after the hook expression matches or the timeout expires

#### Scenario: Sleep with timeout

- **WHEN** `tmux_wait` is called with event `sleep`, `duration_ms`, and
  `timeout_ms`
- **THEN** Ra sleeps for the shorter bounded duration and returns structured
  timeout state

### Requirement: Shared Tmux Event Expressions

Ra SHALL use the same event names and substring/regex expression semantics for
`tmux_listen` and `tmux_wait`.

#### Scenario: Shared pattern matching

- **GIVEN** `pattern` and `regex` fields are provided to either `tmux_listen`
  or `tmux_wait`
- **WHEN** the selected event requires output matching
- **THEN** Ra evaluates the pattern with the same substring or regex semantics
  for both tools
