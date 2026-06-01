# Expose Agent-Own OpenSpec SDD as a Native Tool

## Why

Ra already folds an agent-own spec-driven-development playbook into the system
prompt, but the loop itself is still free-form `bash`: the agent must remember
exact `openspec` command syntax, hand-build `--json` invocations, dodge
interactive `init`/`archive` prompts, and recover from validation errors out of
opaque shell output. That makes a core Ra workflow less reliable than other
built-in tool workflows that expose narrow, structured operations.

## What Changes

- Add a native built-in `openspec` tool that exposes the agent-own OpenSpec
  lifecycle as structured actions over the upstream CLI.
- Read-only actions: `status`, `list`, `show`, `instructions`, plus a derived
  `workflow_state` summary of apply-readiness from `status --json`.
- Mutation actions: `init` (`--tools none` by default), `update`, `new_change`,
  strict `validate`, and `archive` with explicit `confirm_archive` confirmation.
- Run every `openspec` call non-interactively: closed stdin, `--json` where
  supported, `--no-color`, never an interactive prompt.
- Return a bounded JSON envelope with command metadata, exit status, stdout,
  stderr, truncation state, and structured missing-binary guidance.
- Preserve `[tools] builtin` allow-list semantics so the tool can be included
  or excluded explicitly.
- Update tool registration, README, `spec/tools.md`, init/config examples, and
  focused tests.

## Capabilities

### New Capabilities

- `openspec-sdd-tool`: a native lifecycle-control tool that wraps the upstream
  `openspec` CLI as structured, non-interactive actions and returns a bounded
  JSON envelope.

### Modified Capabilities

## Impact

- Affected code: `src/tools/openspec.rs` (new), tool registration in
  `src/tools/mod.rs`, and test coverage under `tests/`.
- Affected docs: `README.md`, `spec/tools.md`, `spec/ra.toml.example`, and
  `src/init.rs` (which enumerate built-in tools).
- Runtime dependency: the `openspec` binary must be discoverable on `PATH`;
  missing-binary behavior is structured and actionable, not a hard failure.
- No breaking changes. `bash` remains the general fallback, the existing
  `src/openspec.rs` discovery/playbook layer is unchanged, and allow-list
  semantics stay exact.
