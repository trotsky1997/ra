# Tasks

## 1. Tool Contract

- [ ] 1.1 Define the `mergiraf` tool input schema with an `action` discriminant (`merge` | `solve` | `languages`), action-specific required fields (`base`/`ours`/`theirs` for merge, `file` for solve), optional flags (`language`, `compact`, `allow_parse_errors`), and `max_output_bytes`.
- [ ] 1.2 Define the stable result envelope: `ok`, `tool`, `action`, `exit_code`, `stdout`, `stderr`, `truncated`, and optional `error` (`kind`, `message`, `install_hint`).

## 2. Implementation

- [ ] 2.1 Add `MergirafTool` under `src/tools/` using argv-safe process spawning (no shell interpolation) and action-specific argument construction.
- [ ] 2.2 Implement the `merge` action: spawn `mergiraf merge <base> <ours> <theirs>` with optional `--language`, `--compact`, and `--allow-parse-errors` argv entries.
- [ ] 2.3 Implement the `solve` action: spawn `mergiraf solve <file>`.
- [ ] 2.4 Implement the `languages` action: spawn `mergiraf languages --gitattributes`.
- [ ] 2.5 Implement stdout/stderr budgeting using `max_output_bytes`, setting `truncated: true` when output is clipped.
- [ ] 2.6 Return `error.kind: "missing_mergiraf"` with an `install_hint` when the binary is absent; do not panic or break catalog startup.
- [ ] 2.7 Register `mergiraf` in `tools::default_builtins` and verify it obeys `[tools].builtin` allow-list semantics.

## 3. Documentation

- [ ] 3.1 Add `mergiraf` row to the built-in tools table in `README.md`.
- [ ] 3.2 Document the `mergiraf` schema, all three actions, result envelope, and error cases in `spec/tools.md`.
- [ ] 3.3 Update `spec/ra.toml.example` and any init templates that enumerate built-in tools.

## 4. Tests

- [ ] 4.1 Add catalog registration tests: default (empty allow-list includes `mergiraf`) and exact allow-list behavior.
- [ ] 4.2 Add `merge` action tests: required argv construction, `--language` and `--compact` flag mapping, and argv boundary preservation.
- [ ] 4.3 Add `solve` action tests: file path as single argv entry, success and non-zero exit envelopes.
- [ ] 4.4 Add `languages` action tests: `--gitattributes` flag appended, output in envelope.
- [ ] 4.5 Add tests for missing-binary guidance and output truncation.
- [ ] 4.6 Run focused tool tests and the full Rust test suite (`cargo test`).
