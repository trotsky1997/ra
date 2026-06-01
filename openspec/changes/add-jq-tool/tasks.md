# Tasks

## 1. Tool Contract

- [x] 1.1 Define the `jq` tool input schema with `filter`, one-of `input` or `path`, `cwd`, output flags, timeout, and `max_output_bytes`.
- [x] 1.2 Define the stable result envelope for success, jq errors, validation errors, truncation, and missing-binary guidance.

## 2. Implementation

- [x] 2.1 Add `JqTool` under `src/tools/` using argv-safe process spawning and stdin input.
- [x] 2.2 Validate exactly one input source before spawning jq and resolve relative paths against `cwd` or the session cwd.
- [x] 2.3 Implement stdout/stderr budgeting while preserving valid JSON and setting `truncated` accurately.
- [x] 2.4 Register `jq` in `tools::default_builtins` and preserve exact allow-list behavior.
- [x] 2.5 Add user-facing tool titles or UI hints if the existing surfaces enumerate built-in tool names.

## 3. Documentation

- [x] 3.1 Document `jq` in `README.md` built-in tools.
- [x] 3.2 Document the `jq` schema, examples, output envelope, and error cases in `spec/tools.md`.
- [x] 3.3 Update init/config examples if they enumerate built-in tools.

## 4. Tests

- [x] 4.1 Add catalog tests for default registration and exact allow-list selection.
- [x] 4.2 Add tests for inline input, file input, one-input-source validation, and jq flag-to-argv mapping.
- [x] 4.3 Add tests for non-zero jq exits, missing jq guidance, and output truncation.
- [x] 4.4 Run focused tool tests and the full Rust test suite.
