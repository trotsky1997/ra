# Tasks

## 1. Tool Contracts

- [x] 1.1 Define the `sd` input schema, CLI argv mapping, validation rules, defaults, and result envelope.
- [x] 1.2 Define the `comby` input schema, enum actions, CLI argv mapping, validation rules, defaults, and result envelope.

## 2. Implementation

- [x] 2.1 Add `SdTool` under `src/tools/` using argv-safe process spawning, explicit paths, cwd resolution, timeout handling, output budgeting, and missing-binary guidance.
- [x] 2.2 Add `CombyTool` under `src/tools/` using argv-safe process spawning, action-dependent validation, cwd resolution, timeout handling, output budgeting, and missing-binary guidance.
- [x] 2.3 Register `sd` and `comby` in `tools::default_builtins` and preserve exact allow-list behavior by tool name.

## 3. Documentation

- [x] 3.1 Document `sd` in `spec/tools.md` with schema, examples, output envelope, and error cases.
- [x] 3.2 Document `comby` in `spec/tools.md` with schema, action mappings, examples, output envelope, and error cases.

## 4. Tests

- [x] 4.1 Add fake-binary integration tests for `sd` covering argv mapping, string mode, path validation, missing binary, non-zero exit, truncation, and catalog registration.
- [x] 4.2 Add fake-binary integration tests for `comby` covering rewrite/check/diff argv mapping, validation, missing binary, non-zero exit, truncation, and catalog registration.
- [x] 4.3 Run formatting, focused tool tests, OpenSpec validation, and the full Rust test suite when feasible.
