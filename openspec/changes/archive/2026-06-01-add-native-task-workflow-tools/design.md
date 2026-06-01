# Design: Native Task Workflow Tools

## Context

Ra already has native built-in tools for common developer CLIs such as `git`,
`gh`, `jq`, and webfetch. Those tools keep frequently repeated operations out of
free-form shell strings and preserve exact built-in allow-list behavior. Project
test loops still commonly go through `bash` even when they are expressed as
`mise run test`, `just test`, or local GitHub Actions validation through
`wrkflw`.

The requested feature is additive: expose task/workflow CLIs as first-class
tools so agents can run tests before and after edits and validate workflow files
with structured, bounded results.

## Goals / Non-Goals

**Goals:**

- Add `mise`, `just`, and `wrkflw` as built-in tools with exact allow-list
  behavior.
- Preserve argv boundaries and avoid local shell interpolation.
- Return consistent JSON envelopes for success, validation errors, non-zero
  exits, timeouts, missing binaries, and truncation.
- Keep implementation and tests localized to the existing `src/tools/` pattern.

**Non-Goals:**

- Do not reimplement task discovery, mise configuration parsing, justfile
  parsing, or GitHub Actions execution semantics.
- Do not auto-install missing binaries.
- Do not replace `bash` for arbitrary pipelines.
- Do not add a new project configuration surface in this change.

## Decisions

### Shared wrapper for task/workflow CLIs

Implement a shared native runner used by `MiseTool`, `JustTool`, and
`WrkflwTool`. The three tools have the same wire shape: `args`, `cwd`,
`timeout_ms`, and `max_output_bytes`. A shared implementation avoids duplicating
process spawning, cwd validation, timeout handling, output bounding, and error
envelope code.

Alternative considered: use the existing `NativeCliParams` from `cli.rs`.
Rejected because that wrapper returns plain combined stdout/stderr and does not
support cwd, timeout, missing-binary guidance, or bounded JSON envelopes.

### Local process spawning in v1

The local path uses `tokio::process::Command::args` and sets `current_dir` after
validating `cwd`. This is the most direct way to preserve argv boundaries and
return structured stdout/stderr. ACP-host terminal parity can be added later if
it can preserve the same envelope.

Alternative considered: route through `bash` or host terminal for every call.
Rejected because it would reintroduce shell-string construction and weaken the
core reason for native tool support.

### Structured missing-binary behavior

Each tool checks `PATH` through `which` before spawning. Missing binaries return
`ok:false` with tool-specific error kinds (`missing_mise`, `missing_just`,
`missing_wrkflw`) and installation guidance. Startup does not fail if these
optional CLIs are absent.

Alternative considered: omit tools from the catalog when binaries are missing.
Rejected because users can still intentionally expose the tool and receive clear
install guidance when a run is attempted.

### TDD-oriented descriptions, not higher-level orchestration

Descriptions should steer the model toward test-first usage (`mise run test`,
`just test`, local workflow validation) but the tools should stay generic argv
wrappers. Higher-level recipe discovery and orchestration would be larger and
less predictable than this feature needs.

## Risks / Trade-offs

- `wrkflw` has a smaller installed base than `mise` and `just` -> Mitigation:
  return structured missing-binary guidance and do not fail startup.
- Workflow runs can be verbose or long-lived -> Mitigation: expose
  `timeout_ms`, bound output with `max_output_bytes`, and mark truncation.
- Different projects use different task names -> Mitigation: keep the tools as
  generic argv wrappers while descriptions give common TDD examples.
- ACP hosted execution parity is not covered in v1 -> Mitigation: retain `bash`
  as fallback for host-specific terminal workflows and keep the native local
  path structurally safe.

## Migration Plan

No migration is required. The tools are additive. Existing `bash` workflows and
existing `[tools].builtin` allow-lists keep their behavior. Users who want only
one task runner can include that tool name in `[tools].builtin`.

## Open Questions

- Should a future change add recipe-discovery helpers such as `mise tasks` or
  `just --summary` as structured subcommands?
- Should ACP-hosted execution render the same envelope by running a quoted
  terminal command and post-processing output, or should it remain local-only?
