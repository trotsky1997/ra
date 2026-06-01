# PRD: Native Task Workflow Tools

## Overview / Problem Statement

Ra agents frequently need to drive local test-first workflows through project task
runners and CI workflow validators. Today `mise`, `just`, and `wrkflw` must be
called through `bash`, which asks the model to compose shell strings, hides the
tool intent from the catalog, and makes TDD loops harder to audit and repeat.

## Goals & Success Metrics

- Ra exposes built-in `mise`, `just`, and `wrkflw` tools in the default catalog
  when `[tools].builtin` is empty.
- A non-empty `[tools].builtin` allow-list can select each tool exactly without
  pulling in unrelated tools.
- Each tool accepts an argv array, optional `cwd`, optional timeout, and an
  output budget.
- Each tool preserves argv boundaries, returns a stable JSON envelope, and
  surfaces missing-binary guidance instead of opaque spawn failures.
- Focused Rust tests cover catalog registration, allow-list behavior, argv/cwd
  execution, missing-binary guidance, non-zero exit reporting, and output
  truncation.

## User Personas & Stories

- As an agent using Ra, I want native `mise` and `just` tools so that I can run
  project-defined checks before and after implementation without brittle shell
  command construction.
- As an agent using Ra, I want a native `wrkflw` tool so that I can validate
  GitHub Actions workflows locally before opening a pull request.
- As an operator, I want task and workflow executions to return bounded,
  structured output so that test-driven loops remain inspectable.
- As a project maintainer, I want exact allow-list behavior so enabling one
  workflow tool does not broaden the available tool surface.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Add native built-in tools named `mise`, `just`, and `wrkflw` to the default catalog. |
| Must | Preserve exact `[tools].builtin` allow-list behavior for all three tools. |
| Must | Accept `args` as an array of strings passed after the binary name. |
| Must | Support `cwd` for running a project task from a specific working directory. |
| Must | Invoke each binary with argv-safe process spawning and no shell interpolation in the local path. |
| Must | Return structured JSON containing `ok`, `tool`, `command`, `exit_code`, `stdout`, `stderr`, and `truncated`. |
| Must | Bound returned stdout/stderr with `max_output_bytes` and preserve valid JSON in the tool result. |
| Must | Return structured missing-binary guidance when the requested tool is not found on `PATH`. |
| Must | Support optional `timeout_ms` so long-running workflows can be stopped deterministically. |
| Should | Include descriptions that encourage test-first usage such as `mise run test`, `just test`, and `wrkflw` workflow validation. |
| Could | Add higher-level task discovery helpers in a later change. |
| Won't | Reimplement `mise`, `just`, or `wrkflw` semantics inside Ra. |
| Won't | Replace `bash` for arbitrary project scripts or command pipelines. |
| Won't | Download or install missing CLIs automatically. |

## Non-Functional Requirements

- Keep implementation localized to the existing `src/tools/` pattern.
- Avoid network calls and package downloads at runtime.
- Keep test fixtures deterministic by using fake binaries on `PATH`.
- Keep process output safe for model consumption by bounding all stdout and
  stderr in the returned envelope.
- Do not weaken existing permission, hook, or built-in allow-list behavior.

## Design Considerations

The tools should feel like the existing native CLI wrappers: narrow structured
adapters around established command-line tools. They should make the TDD loop
obvious by letting agents call project recipes directly, observe structured
failures, edit code, and rerun the same task without reconstructing a shell
command.

## Technical Considerations

Implementation is expected to add a shared task/workflow CLI wrapper under
`src/tools/`, register `MiseTool`, `JustTool`, and `WrkflwTool` in
`tools::default_builtins`, document the tools in `README.md` and
`spec/tools.md`, and add focused integration/unit tests. Local execution should
use `tokio::process::Command::args`, while ACP-hosted execution can remain a
future extension unless parity can be kept without losing output envelopes.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| PRD, GitHub issue draft, and OpenSpec proposal | Agent | Before implementation |
| Native tool implementation and docs | Agent | Implementation phase |
| Focused tests, full validation, archive, and PR | Agent | Before review handoff |

## Open Questions & Risks

- `wrkflw` is less ubiquitous than `mise` and `just`; v1 assumes the binary name
  is `wrkflw` from `bahdotsh/wrkflw` and handles absence through structured
  guidance.
- Some hosted ACP environments may prefer permission-gated terminal execution;
  the v1 local path prioritizes structured envelopes and argv safety.
- Long-running workflow jobs can produce large output; v1 bounds output and
  exposes `timeout_ms`, leaving richer streaming for a later change.

## Appendix

Relevant existing patterns: `src/tools/cli.rs`, `src/tools/jq.rs`,
`src/tools/webfetch.rs`, `src/tools/mod.rs`, `tests/jq_tool.rs`,
`tests/webfetch_tools.rs`, and `spec/tools.md`.
