# Add Native jq Tool

## Why

Ra agents frequently need to inspect and reshape JSON returned by tools,
project files, and command output. Today that work goes through `bash` and shell
pipelines, which hides the command shape from the tool catalog, risks quoting
mistakes, and leaves output budgeting to ad hoc prompting.

## What Changes

- Add a native built-in `jq` tool for running jq filters against inline JSON or a
  JSON file.
- Execute jq through argv-safe process spawning and stdin rather than shell
  interpolation.
- Return a stable JSON envelope with command metadata, exit status, stdout,
  stderr, truncation state, and structured missing-binary guidance.
- Bound all returned output through `max_output_bytes`.
- Update tool registration, README, `spec/tools.md`, and focused tests.

## Capabilities

### New Capabilities

### Modified Capabilities

- `tools`: add the native `jq` built-in tool contract to the existing tool
  catalog requirements.

## Impact

- Affected code: `src/tools/`, tool registration in `src/tools/mod.rs`, and
  test coverage under `tests/`.
- Affected docs: `README.md`, `spec/tools.md`, and config/init references if
  they enumerate built-in tools.
- Runtime dependency: the `jq` binary must be discoverable on `PATH`; missing
  binary behavior is structured and actionable.
- No breaking changes. `bash` remains available as the general fallback and
  existing allow-list semantics stay exact.
