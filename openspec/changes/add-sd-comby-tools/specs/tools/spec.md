# Tools Delta

## ADDED Requirements

### Requirement: Native sd and comby Tool Catalog

Ra SHALL include `sd` and `comby` in the default built-in catalog when
`[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes sd and comby

- **WHEN** Ra builds the default built-in tool catalog with an empty
  `[tools].builtin` allow-list
- **THEN** the catalog includes `sd` and `comby`

#### Scenario: Non-empty allow-list can select sd

- **WHEN** Ra builds the default built-in tool catalog with `[tools].builtin`
  containing only `sd`
- **THEN** the catalog contains `sd` and omits unspecified tools

#### Scenario: Non-empty allow-list can select comby

- **WHEN** Ra builds the default built-in tool catalog with `[tools].builtin`
  containing only `comby`
- **THEN** the catalog contains `comby` and omits unspecified tools

### Requirement: sd Rewrite Execution

Ra SHALL provide an `sd` tool that runs the host `sd` binary for regex or
literal find/replace using argv-safe process spawning and explicit path
arguments.

#### Scenario: sd regex replacement preserves argv boundaries

- **WHEN** the caller provides `find`, `replace`, and one or more `paths`
- **THEN** Ra invokes `sd` with `find`, `replace`, and each path as separate
  argv entries

#### Scenario: sd string mode is passed as a flag

- **WHEN** the caller sets `string_mode: true`
- **THEN** Ra invokes `sd` with `--fixed-strings` before the find and replace
  positionals

#### Scenario: sd leading-dash positionals are protected

- **WHEN** the caller provides `find` or `replace` values beginning with `-`
- **THEN** Ra inserts `--` before the find and replace positionals

#### Scenario: sd extra args are argv-safe

- **WHEN** the caller provides `extra_args`
- **THEN** Ra passes each extra argument as a separate argv entry without shell
  interpolation

#### Scenario: sd requires explicit paths

- **WHEN** the caller provides an empty `paths` array
- **THEN** Ra returns `error.kind: "invalid_request"` before invoking `sd`

### Requirement: comby Structural Rewrite Execution

Ra SHALL provide a `comby` tool that runs the host `comby` binary for
structural check, diff, and rewrite actions using argv-safe process spawning.

#### Scenario: comby rewrite mutates in place

- **WHEN** the caller sets `action: "rewrite"` with a rewrite template
- **THEN** Ra invokes `comby` with `-in-place`

#### Scenario: comby check does not require rewrite template

- **WHEN** the caller sets `action: "check"` without `rewrite_template`
- **THEN** Ra invokes `comby` with an empty rewrite positional and
  `-match-only`

#### Scenario: comby diff requires rewrite template

- **WHEN** the caller sets `action: "diff"` without `rewrite_template`
- **THEN** Ra returns `error.kind: "invalid_request"` before invoking `comby`

#### Scenario: comby rewrite requires rewrite template

- **WHEN** the caller sets `action: "rewrite"` without `rewrite_template`
- **THEN** Ra returns `error.kind: "invalid_request"` before invoking `comby`

#### Scenario: comby filters map to argv

- **WHEN** the caller provides extensions, directory, matcher, include_files,
  exclude_files, or extra_args
- **THEN** Ra passes each requested value as separate argv entries without
  shell interpolation

### Requirement: sd and comby Result Envelopes

Ra SHALL return bounded, valid JSON envelopes for `sd` and `comby` execution
results, including failures.

#### Scenario: successful rewrite returns structured output

- **WHEN** `sd` or `comby` exits with status 0
- **THEN** Ra returns JSON with `ok: true`, the tool name, command metadata,
  `exit_code: 0`, stdout, stderr, and `truncated`

#### Scenario: non-zero exit is structured

- **WHEN** `sd` or `comby` exits with a non-zero status
- **THEN** Ra returns JSON with `ok: false`, the exit code, stdout, stderr, and
  `error.kind: "command_failed"`

#### Scenario: missing sd binary is structured

- **WHEN** `sd` is not found on `PATH`
- **THEN** Ra returns JSON with `ok: false`, no exit code,
  `error.kind: "missing_sd"`, and installation guidance

#### Scenario: missing comby binary is structured

- **WHEN** `comby` is not found on `PATH`
- **THEN** Ra returns JSON with `ok: false`, no exit code,
  `error.kind: "missing_comby"`, and installation guidance

#### Scenario: timeout is structured

- **WHEN** `sd` or `comby` exceeds `timeout_ms`
- **THEN** Ra returns JSON with `ok: false`, no exit code, and
  `error.kind: "timeout"`

#### Scenario: output is bounded

- **WHEN** stdout or stderr exceeds `max_output_bytes`
- **THEN** Ra returns valid JSON with `truncated: true`
