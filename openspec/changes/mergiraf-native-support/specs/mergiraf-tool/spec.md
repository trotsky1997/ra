## ADDED Requirements

### Requirement: mergiraf Tool Catalog Registration

Ra SHALL include `mergiraf` in the default built-in catalog when `[tools].builtin` is empty.

#### Scenario: Empty allow-list exposes mergiraf tool

- **WHEN** Ra builds the default built-in tool catalog with an empty `[tools].builtin` allow-list
- **THEN** the catalog includes `mergiraf`

#### Scenario: Non-empty allow-list remains exact

- **WHEN** Ra builds the default built-in tool catalog with `[tools].builtin` containing only `mergiraf`
- **THEN** the catalog contains `mergiraf` and omits unspecified tools

### Requirement: mergiraf merge Action

Ra SHALL provide a `mergiraf` tool with a `merge` action that invokes `mergiraf merge <base> <ours> <theirs>` with argv-safe process spawning and no shell interpolation.

#### Scenario: merge preserves argv boundaries for required files

- **WHEN** the agent calls `mergiraf` with `action: "merge"`, `base`, `ours`, and `theirs` paths
- **THEN** Ra invokes the `mergiraf` binary with `merge`, `base`, `ours`, and `theirs` as separate argv entries

#### Scenario: merge accepts optional language override

- **WHEN** the agent calls `mergiraf` with `action: "merge"` and `language: "java"`
- **THEN** Ra appends `--language` and `java` as separate argv entries

#### Scenario: merge accepts compact flag

- **WHEN** the agent calls `mergiraf` with `action: "merge"` and `compact: true`
- **THEN** Ra appends `--compact` to the argv

#### Scenario: merge result envelope on clean merge

- **WHEN** mergiraf exits with code 0
- **THEN** Ra returns JSON with `ok: true`, `action: "merge"`, `exit_code: 0`, `stdout`, `stderr`, and `truncated`

#### Scenario: merge result envelope on unresolved conflicts

- **WHEN** mergiraf exits with a non-zero code indicating remaining conflicts
- **THEN** Ra returns JSON with `ok: false`, the non-zero `exit_code`, `stdout`, `stderr`, and `truncated`

### Requirement: mergiraf solve Action

Ra SHALL provide a `mergiraf` tool with a `solve` action that invokes `mergiraf solve <file>` with argv-safe process spawning.

#### Scenario: solve passes file path as single argv entry

- **WHEN** the agent calls `mergiraf` with `action: "solve"` and a `file` path
- **THEN** Ra invokes the `mergiraf` binary with `solve` and the file path as separate argv entries

#### Scenario: solve result envelope on success

- **WHEN** `mergiraf solve` exits with code 0
- **THEN** Ra returns JSON with `ok: true`, `action: "solve"`, `exit_code: 0`, `stdout`, `stderr`, and `truncated`

### Requirement: mergiraf languages Action

Ra SHALL provide a `mergiraf` tool with a `languages` action that invokes `mergiraf languages --gitattributes` and returns the supported extension list.

#### Scenario: languages returns gitattributes format

- **WHEN** the agent calls `mergiraf` with `action: "languages"`
- **THEN** Ra invokes the `mergiraf` binary with `languages` and `--gitattributes` as separate argv entries
- **AND** Ra returns the output in the standard JSON envelope

### Requirement: mergiraf Missing Binary Guidance

Ra SHALL return a structured error when the `mergiraf` binary is not found on PATH, without breaking agent startup.

#### Scenario: Missing binary returns structured guidance

- **WHEN** the `mergiraf` binary is not found on PATH
- **THEN** Ra returns JSON with `ok: false`, no `exit_code`, and `error.kind: "missing_mergiraf"` with an `install_hint`

#### Scenario: Missing binary does not block catalog startup

- **WHEN** `mergiraf` is absent from PATH and Ra starts
- **THEN** Ra still registers the tool in the catalog and the startup succeeds

### Requirement: mergiraf Output Bounding

Ra SHALL bound all process output returned from `mergiraf` invocations via `max_output_bytes`.

#### Scenario: Output within bounds is returned in full

- **WHEN** combined stdout and stderr are within `max_output_bytes`
- **THEN** Ra returns the full output with `truncated: false`

#### Scenario: Output exceeding bounds is truncated

- **WHEN** combined stdout and stderr exceed `max_output_bytes`
- **THEN** Ra clips the output and returns `truncated: true`

### Requirement: mergiraf ACP Host Isolation

Ra SHALL spawn the `mergiraf` binary locally even when an ACP host is attached, matching the behavior of other native CLI wrappers.

#### Scenario: ACP host does not wrap mergiraf spawns

- **WHEN** an ACP host is attached
- **THEN** `mergiraf` still spawns the local binary directly
- **AND** ACP terminal permission prompts do not wrap those local spawns
