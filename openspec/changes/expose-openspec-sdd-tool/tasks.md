# Tasks

## 1. Tool Contract

- [x] 1.1 Define the `openspec` tool input schema with an `action` enum and per-action fields (`change`, `item`, `artifact`, `specs`, `tools`, `path`, `description`, `confirm_archive`, `skip_specs`, `cwd`, `timeout_ms`, `max_output_bytes`).
- [x] 1.2 Define the stable result envelope for success, openspec errors, invalid requests, timeout, truncation, and missing-binary guidance.

## 2. Implementation

- [x] 2.1 Add `OpenSpecTool` under `src/tools/openspec.rs` using argv-safe process spawning with closed stdin and `--no-color`.
- [x] 2.2 Build invocations purely in `OpenSpecInvocation::from_params`, mapping each action to its `openspec` subcommand argv and requesting `--json` where supported.
- [x] 2.3 Enforce non-interactive safety: `init --tools` default `none`, `validate --strict` always, and `archive` only with explicit `confirm_archive` (then `-y`).
- [x] 2.4 Implement stdout/stderr budgeting that preserves valid JSON and sets `truncated` accurately.
- [x] 2.5 Add the derived `workflow_state` action that summarizes apply-readiness from `status --json` and degrades gracefully on schema mismatch.
- [x] 2.6 Register `openspec` in `tools::default_builtins` and preserve exact allow-list behavior.

## 3. Documentation

- [x] 3.1 Document `openspec` in `README.md` built-in tools.
- [x] 3.2 Document the `openspec` schema, actions, output envelope, and error cases in `spec/tools.md`.
- [x] 3.3 Update init/config examples (`src/init.rs`, `spec/ra.toml.example`) that enumerate built-in tools.

## 4. Tests

- [x] 4.1 Add catalog tests for default registration and exact allow-list selection.
- [x] 4.2 Add unit tests for per-action argv mapping, required-field validation, unsafe change-name rejection, and `workflow_state` derivation.
- [x] 4.3 Add integration tests with a fake `openspec` binary for argv, non-zero exit envelope, archive confirmation gating, and missing-binary guidance.
- [x] 4.4 Run focused tool tests and the full Rust test suite, plus `openspec validate --strict`.
