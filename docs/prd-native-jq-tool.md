# PRD: Native jq Tool

## Overview / Problem Statement

Ra users frequently need to inspect, reshape, and extract fields from JSON
produced by agent tools, project files, and command output. Today they must
route these operations through `bash` pipelines, which asks the model to compose
shell strings, loses structured parameters, and makes output budgeting and
permission semantics less predictable than Ra's native tool catalog.

## Goals & Success Metrics

- Ra exposes a built-in `jq` tool in the default catalog when
  `[tools].builtin` is empty.
- A non-empty `[tools].builtin` allow-list can select `jq` exactly without
  pulling in unrelated tools.
- The tool accepts a jq filter plus either inline JSON input or a JSON file path.
- The tool runs jq without shell interpolation, returns a stable JSON envelope,
  and bounds stdout/stderr by a caller-controlled output budget.
- Focused Rust tests cover default catalog registration, allow-list behavior,
  argv construction, file and inline input handling, truncation, invalid input,
  and missing-binary guidance.

## User Personas & Stories

- As an agent using Ra, I want to query JSON with a structured `jq` tool so that
  I can avoid brittle shell pipelines for common data-shaping work.
- As an operator, I want jq execution to preserve argv boundaries and return
  bounded output so that tool calls are easier to audit and recover from.
- As a project maintainer, I want allow-list semantics to remain exact so that
  enabling `jq` does not accidentally broaden the available tool surface.

## Functional Requirements

| Priority | Requirement |
| --- | --- |
| Must | Add a native built-in tool named `jq` to the default tool catalog. |
| Must | Preserve exact `[tools].builtin` allow-list behavior for `jq`. |
| Must | Accept a required jq `filter` string. |
| Must | Accept exactly one input source: inline JSON text via `input`, or a filesystem path via `path`. |
| Must | Invoke the `jq` binary with argv-safe process spawning, passing JSON data on stdin. |
| Must | Support common jq flags for JSON output control without exposing shell execution. |
| Must | Return structured JSON containing `ok`, `tool`, `filter`, `exit_code`, `stdout`, `stderr`, and `truncated`. |
| Must | Bound returned stdout/stderr with `max_output_bytes` and preserve valid JSON in the tool result. |
| Must | Return structured missing-binary guidance when `jq` is not found on `PATH`. |
| Should | Support `cwd` for resolving relative input paths. |
| Should | Support `raw_output`, `compact_output`, and `sort_keys` booleans. |
| Could | Add future support for jq slurp/raw-input modes in a separate change. |
| Won't | Vendor or reimplement jq's filter language. |
| Won't | Replace `bash` for arbitrary command pipelines. |
| Won't | Add mutation behavior; the tool only reads input and returns transformed output. |

## Non-Functional Requirements

- Keep the tool implementation localized to the existing `src/tools/` pattern.
- Avoid network calls and package downloads at runtime.
- Use deterministic tests that can simulate jq execution and missing jq without
  requiring external services.
- Keep output safe for model consumption by bounding all process output.
- Do not weaken existing permission, hook, or built-in allow-list behavior.

## Design Considerations

The tool should feel like the existing native `git`, `gh`, and webfetch
wrappers: a narrow, structured wrapper around a proven CLI, not a broad shell
escape hatch. Inputs should make the safe path obvious: provide a filter, choose
one JSON source, and let Ra handle stdin and output budgeting.

## Technical Considerations

Implementation is expected to add a `JqTool` under `src/tools/`, register it in
`tools::default_builtins`, document it in `README.md` and `spec/tools.md`, and
add focused integration/unit tests. The local process path should use
`tokio::process::Command::args` and stdin, while ACP-hosted execution should
follow the existing native CLI wrapper conventions if reusable.

## Timeline & Milestones

| Milestone | Owner | Target |
| --- | --- | --- |
| PRD and OpenSpec proposal | Agent | Before implementation |
| Native tool implementation and docs | Agent | Implementation phase |
| Focused tests and full validation | Agent | Before PR handoff |

## Open Questions & Risks

- Should v1 require a real `jq` binary on `PATH`, or should Ra optionally support
  a Rust jq-compatible library later? Current proposal chooses the real binary
  for correctness and scope control.
- How much of jq's flag surface should v1 expose? Current proposal limits this
  to output-shaping booleans and leaves advanced modes for follow-up work.
- If ACP terminal reverse-calls are used for hosted execution, stdin handling may
  need careful parity with local execution.

## Appendix

Relevant existing patterns: `src/tools/cli.rs`, `src/tools/webfetch.rs`,
`src/tools/mod.rs`, `tests/webfetch_tools.rs`, `tests/extended_tools.rs`, and
`spec/tools.md`.
