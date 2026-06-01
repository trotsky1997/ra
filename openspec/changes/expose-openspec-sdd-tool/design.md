# Design

## Context

Ra's `src/openspec.rs` discovers a project's `openspec/` directory and folds a
catalog plus an agent-own playbook into the system prompt. That layer is the
*prompt* surface; it does not execute anything. The execution still happens
through `bash`, which the PRD identifies as the reliability gap.

This change adds an *execution* surface — a native tool — alongside the
existing prompt surface. The two are complementary: the playbook explains the
loop, the tool runs it with structured parameters and bounded output.

## Goals / Non-Goals

Goals:
- One narrow tool that maps OpenSpec concepts to typed actions.
- Non-interactive by construction; safe for unattended agents.
- Argv-safe spawning, bounded JSON output, structured missing-binary guidance.

Non-Goals:
- Reimplementing OpenSpec schema validation, artifact generation, or archiving
  logic in Rust. Ra delegates all semantics to the upstream CLI.
- Replacing `bash` or the prompt-side discovery/playbook layer.

## Decisions

### A single tool with an action enum, not one tool per subcommand

Following the existing native-CLI pattern (`git`/`gh` share `execute_native_cli`;
`webfetch_fetch`/`webfetch_crawl` share `execute_webfetch`), the tool exposes a
single `openspec` name with an `action` enum. This keeps the catalog small and
mirrors how the upstream CLI is itself a set of subcommands. The alternative —
ten separate tools — would bloat the tool catalog the model sees.

### Invocation building is pure and testable

`OpenSpecInvocation::from_params` turns typed params into an argv `Vec<String>`
without spawning, exactly like `JqInvocation`/`WebfetchInvocation`. This lets
unit tests assert the exact argv for every action (the bulk of the risk is
argv construction) with no process or filesystem dependency.

### Non-interactive safety is enforced, not advised

- `stdin` is closed (`Stdio::null()`) on every spawn so no subcommand can block
  on input.
- `init` always passes `--tools` (default `none`); bare `openspec init` prompts.
- `archive` is destructive to the change directory, so the tool refuses to run
  it unless `confirm_archive: true` is passed, and only then supplies `-y`.
- `validate` always passes `--strict` so the agent gets machine-actionable
  errors rather than lenient passes.
- `--no-color` is appended so ANSI escapes never corrupt the captured output.

### Errors are data, not exceptions

The upstream CLI frequently prints errors to stdout/stderr while still exiting
0, and is missing on hosts that never adopted OpenSpec. So the tool returns a
stable envelope (`ok`, `exit_code`, `stdout`, `stderr`, `error.kind`) instead of
propagating spawn errors: `invalid_request` (bad params, caught before spawn),
`missing_openspec` (binary not on PATH, with install guidance), `timeout`, and
`openspec_error` (non-zero exit). Output is bounded by `max_output_bytes` using
the same binary-search truncation as the jq/webfetch tools.

### `workflow_state` is a thin derivation, not a new command

`workflow_state` runs `status --json` and folds a summary
(`ready`/`blocked`/`done`, `applyReady`, `nextActions`) into the envelope. If
the status JSON does not match the expected shape (schema drift), the summary
is omitted rather than erroring — Ra keeps parsing shallow and delegates
authority to the CLI.

## Risks / Trade-offs

- Upstream JSON schema may evolve. Mitigation: the tool passes JSON through
  verbatim in `stdout`; only `workflow_state` parses it, and it degrades
  gracefully on mismatch.
- `openspec` is an optional dependency. Mitigation: the tool ships in the
  catalog regardless (like `jq`) and returns structured guidance when the
  binary is absent, so startup never breaks.
