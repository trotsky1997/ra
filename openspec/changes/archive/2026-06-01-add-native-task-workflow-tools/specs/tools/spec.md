# Tools Delta

## ADDED Requirements

### Requirement: Native Task Workflow Tool Catalog

Ra SHALL include `mise`, `just`, and `wrkflw` in the default built-in catalog
when `[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes task workflow tools

- **WHEN** Ra builds the default built-in tool catalog with an empty
  `[tools].builtin` allow-list
- **THEN** the catalog includes `mise`, `just`, and `wrkflw`

#### Scenario: Non-empty allow-list remains exact

- **WHEN** Ra builds the default built-in tool catalog with `[tools].builtin`
  containing only `mise`
- **THEN** the catalog contains `mise` and omits unspecified tools

### Requirement: Native Task Workflow Tool Execution

Ra SHALL provide `mise`, `just`, and `wrkflw` tools that execute the matching
host binary with structured argv parameters and no local shell interpolation.

#### Scenario: mise task run preserves argv boundaries

- **WHEN** the agent calls `mise` with `args: ["run", "test"]`
- **THEN** Ra invokes the `mise` binary with `run` and `test` as separate argv
  entries

#### Scenario: just recipe run preserves argv boundaries

- **WHEN** the agent calls `just` with `args: ["test"]`
- **THEN** Ra invokes the `just` binary with `test` as a separate argv entry

#### Scenario: wrkflw workflow validation preserves argv boundaries

- **WHEN** the agent calls `wrkflw` with workflow validation arguments
- **THEN** Ra invokes the `wrkflw` binary with each argument as a separate argv
  entry

#### Scenario: cwd selects project working directory

- **WHEN** a caller provides `cwd`
- **THEN** Ra runs the requested task workflow binary from that directory

### Requirement: Native Task Workflow Result Envelope

Ra SHALL return a bounded, valid JSON envelope for `mise`, `just`, and `wrkflw`
execution results, including failures.

#### Scenario: Successful task returns structured output

- **WHEN** a task workflow tool exits with status 0
- **THEN** Ra returns JSON with `ok: true`, the tool name, command metadata,
  `exit_code: 0`, stdout, stderr, and `truncated`

#### Scenario: Non-zero task is structured

- **WHEN** a task workflow tool exits with a non-zero status
- **THEN** Ra returns JSON with `ok: false`, the exit code, stdout, stderr, and
  `error.kind: "command_failed"`

#### Scenario: Missing binary is structured

- **WHEN** the requested `mise`, `just`, or `wrkflw` binary is not found on
  `PATH`
- **THEN** Ra returns JSON with `ok: false`, no exit code, and a tool-specific
  missing-binary error kind

#### Scenario: Timeout is structured

- **WHEN** a task workflow tool exceeds `timeout_ms`
- **THEN** Ra returns JSON with `ok: false`, no exit code, and
  `error.kind: "timeout"`

#### Scenario: Output is bounded

- **WHEN** stdout or stderr exceeds `max_output_bytes`
- **THEN** Ra returns valid JSON with `truncated: true`
