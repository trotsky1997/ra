# OpenSpec SDD Tool Delta

## ADDED Requirements

### Requirement: Native OpenSpec Tool Catalog

Ra SHALL include `openspec` in the default built-in catalog when
`[tools].builtin` is empty, and SHALL include or exclude it exactly when the
allow-list is non-empty.

#### Scenario: Empty allow-list exposes openspec

- **GIVEN** `[tools].builtin` is empty
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog includes `openspec`

#### Scenario: Non-empty allow-list remains exact

- **GIVEN** `[tools].builtin` contains only `openspec`
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog contains `openspec` and omits unspecified tools

### Requirement: OpenSpec Lifecycle Actions

Ra SHALL provide an `openspec` tool that maps structured actions to upstream
`openspec` CLI subcommands using argv-safe process spawning, and SHALL request
`--json` output where the subcommand supports it.

#### Scenario: Status action targets a change as JSON

- **GIVEN** a caller provides `action: "status"` and `change: "add-thing"`
- **WHEN** the tool builds its invocation
- **THEN** it invokes `openspec status --change add-thing --json`

#### Scenario: Instructions action defaults to the apply artifact

- **GIVEN** a caller provides `action: "instructions"` and a `change` without an `artifact`
- **WHEN** the tool builds its invocation
- **THEN** it requests instructions for the `apply` artifact

#### Scenario: Read-only actions do not mutate the project

- **GIVEN** a caller invokes `status`, `list`, `show`, `instructions`, or `workflow_state`
- **WHEN** the tool runs
- **THEN** no change directory or spec file is created, modified, or removed

### Requirement: Non-Interactive Safety

Ra SHALL run every `openspec` invocation non-interactively so that an
unattended agent cannot hang on a prompt, and SHALL require explicit
confirmation before destructive operations.

#### Scenario: Init is never interactive

- **GIVEN** a caller provides `action: "init"` without `tools`
- **WHEN** the tool builds its invocation
- **THEN** it passes `--tools none` so the CLI does not prompt for tool selection

#### Scenario: Validation is always strict

- **GIVEN** a caller provides `action: "validate"`
- **WHEN** the tool builds its invocation
- **THEN** it passes `--strict`

#### Scenario: Archive requires explicit confirmation

- **GIVEN** a caller provides `action: "archive"` without `confirm_archive: true`
- **WHEN** the tool validates the request
- **THEN** the call fails before invoking the openspec binary

#### Scenario: Confirmed archive skips the interactive prompt

- **GIVEN** a caller provides `action: "archive"`, a `change`, and `confirm_archive: true`
- **WHEN** the tool builds its invocation
- **THEN** it invokes `openspec archive <change> -y`

#### Scenario: Positional values cannot be parsed as flags

- **GIVEN** a caller provides a leading-dash value for a field that becomes a CLI positional (for example `change: "--skip-specs"` on `archive`, `item` on `show`/`validate`, `artifact` on `instructions`, or `path` on `init`/`update`)
- **WHEN** the tool validates the request
- **THEN** the call fails before invoking the openspec binary instead of letting the value be parsed as an option

### Requirement: OpenSpec Output Envelope

Ra SHALL return a bounded, valid JSON envelope for openspec execution results,
including failures, surfacing exit status and stderr.

#### Scenario: Successful run reports command and exit status

- **GIVEN** openspec exits with status zero
- **WHEN** the tool returns
- **THEN** Ra returns JSON with `ok: true`, the action, the command metadata, and `exit_code: 0`

#### Scenario: Non-zero exit is structured

- **GIVEN** openspec exits with a non-zero status
- **WHEN** the tool returns
- **THEN** Ra returns JSON with `ok: false`, the exit code, stderr, and `error.kind: "openspec_error"`

#### Scenario: Output is bounded

- **GIVEN** openspec output exceeds `max_output_bytes`
- **WHEN** Ra builds the result
- **THEN** Ra returns valid JSON with `truncated: true`

#### Scenario: Missing binary is actionable

- **GIVEN** `openspec` is not found on `PATH`
- **WHEN** the tool executes
- **THEN** Ra returns JSON with `ok: false`, `error.kind: "missing_openspec"`, and installation guidance

### Requirement: Derived Workflow State

Ra SHALL provide a `workflow_state` action that runs `status --json` and folds
an apply-readiness summary into the envelope, degrading gracefully when the
status payload does not match the expected shape.

#### Scenario: Workflow state classifies artifacts

- **GIVEN** a `status --json` payload with artifacts in `ready`, `blocked`, and `done` states
- **WHEN** the `workflow_state` action returns
- **THEN** the envelope includes a `workflow_state` object listing each artifact under its state and an `applyReady` flag

#### Scenario: Workflow state tolerates schema drift

- **GIVEN** a `status` payload that does not contain the expected fields
- **WHEN** the `workflow_state` action returns
- **THEN** Ra omits the derived summary instead of failing the call
