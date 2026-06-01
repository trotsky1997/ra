# Tools Delta

## ADDED Requirements

### Requirement: Native jq Tool Catalog

Ra SHALL include `jq` in the default built-in catalog when `[tools].builtin` is
empty.

#### Scenario: Empty allow-list exposes jq

- **GIVEN** `[tools].builtin` is empty
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog includes `jq`

#### Scenario: Non-empty allow-list remains exact

- **GIVEN** `[tools].builtin` contains only `jq`
- **WHEN** Ra builds the default built-in tool catalog
- **THEN** the catalog contains `jq` and omits unspecified tools

### Requirement: jq Filter Execution

Ra SHALL provide a `jq` tool that runs a jq filter against exactly one JSON input
source using argv-safe process spawning and stdin.

#### Scenario: Inline JSON input is filtered

- **GIVEN** a caller provides `filter: ".name"` and inline JSON input
- **WHEN** `jq` executes successfully
- **THEN** Ra returns JSON with `ok: true`, `exit_code: 0`, and stdout from jq

#### Scenario: File JSON input is filtered

- **GIVEN** a caller provides `filter` and a JSON `path`
- **WHEN** `jq` executes successfully
- **THEN** Ra reads the file and passes its contents to jq on stdin

#### Scenario: Exactly one input source is required

- **GIVEN** a caller provides both `input` and `path`, or neither input source
- **WHEN** `jq` validates the request
- **THEN** the call fails before invoking the jq binary

#### Scenario: Output flags map to jq argv

- **GIVEN** a caller enables `raw_output`, `compact_output`, and `sort_keys`
- **WHEN** `jq` builds its invocation
- **THEN** it invokes jq with `-r`, `-c`, and `-S` as separate argv entries

### Requirement: jq Output Envelope

Ra SHALL return a bounded, valid JSON envelope for jq execution results,
including failures.

#### Scenario: jq failure is structured

- **GIVEN** jq exits with a non-zero status
- **WHEN** the tool returns
- **THEN** Ra returns JSON with `ok: false`, the jq exit code, stderr, and no
  shell-expanded command string

#### Scenario: jq output is bounded

- **GIVEN** jq output exceeds `max_output_bytes`
- **WHEN** Ra builds the result
- **THEN** Ra returns valid JSON with `truncated: true`

#### Scenario: jq binary is missing

- **GIVEN** `jq` is not found on `PATH`
- **WHEN** the tool executes
- **THEN** Ra returns JSON with `ok: false`, `error.kind: "missing_jq"`, and
  installation guidance
