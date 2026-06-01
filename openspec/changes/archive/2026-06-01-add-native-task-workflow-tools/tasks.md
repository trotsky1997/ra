# Tasks

## 1. Product And Spec Artifacts

- [x] 1.1 Add PRD and GitHub issue draft for native task workflow tools.
- [x] 1.2 Add OpenSpec proposal, design, and tools delta spec.

## 2. Test-First Coverage

- [x] 2.1 Add focused tests proving `mise`, `just`, and `wrkflw` register in the default catalog and obey exact allow-list selection.
- [x] 2.2 Add fake-binary tests proving each tool preserves argv boundaries and honors `cwd`.
- [x] 2.3 Add fake-binary tests for non-zero exit envelopes, missing-binary guidance, timeout behavior, and truncation.

## 3. Implementation

- [x] 3.1 Add a shared task/workflow CLI wrapper with `args`, `cwd`, `timeout_ms`, and `max_output_bytes`.
- [x] 3.2 Implement `MiseTool`, `JustTool`, and `WrkflwTool` using argv-safe local process spawning.
- [x] 3.3 Register the tools in `tools::default_builtins` and re-export them from the library surface.

## 4. Documentation

- [x] 4.1 Document `mise`, `just`, and `wrkflw` in the README built-in tool table and native-tool notes.
- [x] 4.2 Document schemas, examples, result envelopes, and error cases in `spec/tools.md`.

## 5. Validation And Handoff

- [x] 5.1 Run formatting, focused tests, full `cargo test`, and strict OpenSpec validation.
- [x] 5.2 Archive the completed OpenSpec change.
- [x] 5.3 Create the GitHub issue and open a PR for review.
- [x] 5.4 Assign Opus/Claude-side review or document the review request path.
