# Add Native mise, just, and wrkflw Tools

## Problem

Ra currently relies on `bash` for project task runners and local workflow
validation. That makes test-driven loops such as "run failing tests, edit, rerun
tests" depend on shell-string construction even when the operation is a common
developer tool invocation.

## Proposed Scope

- Add native built-in tools:
  - `mise`
  - `just`
  - `wrkflw`
- Use argv-safe local process spawning with an `args` array rather than shell
  interpolation.
- Support optional `cwd`, optional `timeout_ms`, and `max_output_bytes`.
- Return a stable JSON envelope with command metadata, exit code, stdout,
  stderr, truncation state, and structured errors.
- Return structured missing-binary guidance when the requested CLI is not on
  `PATH`.
- Register the tools in the default catalog and preserve exact
  `[tools].builtin` allow-list behavior.
- Document usage in `README.md` and `spec/tools.md`.
- Add focused tests with fake binaries to verify TDD-oriented execution without
  requiring host installations.

## Out Of Scope

- Reimplementing task discovery or workflow parsing inside Ra.
- Auto-installing `mise`, `just`, or `wrkflw`.
- Replacing `bash` for arbitrary project scripts or pipelines.
- Streaming rich workflow logs beyond the existing tool update events.

## Acceptance Criteria

- `default_builtins(&[])` includes `mise`, `just`, and `wrkflw`.
- `default_builtins(&["mise"])`, `default_builtins(&["just"])`, and
  `default_builtins(&["wrkflw"])` each return only the requested tool.
- Calling `mise` with `args: ["run", "test"]` spawns `mise run test` with
  argv boundaries preserved.
- Calling `just` with `args: ["test"]` spawns `just test` with argv boundaries
  preserved.
- Calling `wrkflw` with workflow args spawns `wrkflw` with argv boundaries
  preserved.
- Each tool honors `cwd`, reports non-zero exits as `ok:false`, and preserves
  stdout/stderr in the bounded JSON envelope.
- Missing binaries return `error.kind` values of `missing_mise`,
  `missing_just`, or `missing_wrkflw` with installation guidance.
- `cargo fmt --check`, focused tests, full `cargo test`, and
  `openspec validate add-native-task-workflow-tools --strict` pass.
