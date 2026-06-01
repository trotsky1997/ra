# Add Native Task Workflow Tools

## Why

Ra agents often need to run project-defined tests and local CI workflow checks
as a first-class TDD loop: define or run the failing check, edit code, rerun the
same check, then validate workflow behavior before review. Today `mise`, `just`,
and `wrkflw` calls go through `bash`, which hides intent from the tool catalog
and relies on shell-string construction for common task-runner operations.

## What Changes

- Add native built-in tools named `mise`, `just`, and `wrkflw`.
- Execute each tool from structured `args` with argv-safe local process spawning.
- Support optional `cwd`, optional `timeout_ms`, and `max_output_bytes`.
- Return a stable bounded JSON envelope for success, non-zero exits, timeouts,
  invalid requests, and missing-binary guidance.
- Update docs and focused tests so task/workflow tools are presented as the
  preferred path for test-first project recipes and local workflow validation.

## Capabilities

### New Capabilities

### Modified Capabilities

- `tools`: add native task and workflow CLI wrappers to the built-in tool
  catalog requirements.

## Impact

- Affected code: `src/tools/`, tool registration in `src/tools/mod.rs`, and
  test coverage under `tests/`.
- Affected docs: `docs/`, `README.md`, and `spec/tools.md`.
- Runtime dependencies: `mise`, `just`, and `wrkflw` are optional host binaries;
  missing binaries return structured guidance instead of failing startup.
- No breaking changes. `bash` remains the fallback for arbitrary scripts and
  exact allow-list behavior is preserved.
