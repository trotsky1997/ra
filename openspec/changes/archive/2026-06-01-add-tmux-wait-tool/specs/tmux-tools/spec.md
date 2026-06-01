## MODIFIED Requirements

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

## ADDED Requirements

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
