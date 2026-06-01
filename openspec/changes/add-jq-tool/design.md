# Design

## Context

Ra already has two relevant native-tool patterns:

- `git` and `gh` wrap common CLIs with argv-safe arguments.
- `webfetch_fetch` and `webfetch_crawl` wrap an external CLI, return bounded
  JSON envelopes, and provide structured missing-dependency guidance.

`jq` should follow those patterns while remaining read-only. It transforms JSON
input and returns output; it does not write files or replace arbitrary shell
pipelines.

## Goals / Non-Goals

**Goals:**

- Provide a built-in `jq` tool that is available by default and allow-listable by
  name.
- Keep execution argv-safe and avoid shell interpolation.
- Accept one explicit JSON input source and pass it to jq on stdin.
- Return bounded, valid JSON results with actionable error details.
- Keep implementation and tests localized to the existing tool architecture.

**Non-Goals:**

- Do not implement jq's filter language in Rust.
- Do not expose the full jq option surface in v1.
- Do not make the tool mutate input files.
- Do not remove `bash` as the fallback for complex pipelines.

## Decisions

### Use the system `jq` binary

Ra will invoke `jq` from `PATH` rather than vendoring or reimplementing jq.
This preserves jq compatibility, keeps implementation small, and matches
existing native CLI wrapper behavior. If `jq` is missing, the tool returns
`ok: false` with `error.kind: "missing_jq"` and installation guidance.

Alternative considered: a Rust jq-compatible library. That would reduce the
external binary dependency but risks semantic drift from jq and broadens the
change beyond native tool support.

### Pass all JSON input through stdin

The tool accepts either inline `input` or a `path`, but execution always feeds
bytes to jq over stdin. This gives consistent local behavior and avoids mixing
path arguments with filter arguments. Relative paths resolve against `cwd` when
provided, otherwise the session cwd.

Alternative considered: pass file paths directly to jq. That matches jq CLI
usage but creates two process shapes and makes ACP-host parity harder.

### Keep v1 flags narrow

The schema exposes `raw_output`, `compact_output`, and `sort_keys`, mapped to
`-r`, `-c`, and `-S`. Advanced modes such as slurp, raw-input, null-input,
argument binding, and multiple input files are left for later changes.

Alternative considered: expose arbitrary `args`. That would recreate the
shell-like surface this tool is meant to avoid.

### Bound stdout and stderr in the result envelope

The tool returns JSON containing `ok`, `tool`, `filter`, `exit_code`, `stdout`,
`stderr`, `truncated`, and optional `error`. `max_output_bytes` bounds returned
process output while preserving valid JSON. Truncation clips stdout before
stderr and sets `truncated: true`, following the webfetch tool pattern.

## Risks / Trade-offs

- Missing `jq` on PATH -> return structured install guidance instead of an
  opaque spawn error.
- jq filters can be CPU-expensive on large input -> keep a timeout field or
  reuse existing process timeout conventions if present during implementation.
- ACP-hosted stdin parity may differ from local process execution -> validate
  hosted execution behavior against existing native CLI wrapper capabilities
  before implementation.
- Narrow v1 flags may not cover every jq workflow -> leave advanced modes as
  explicit follow-up scope rather than accepting arbitrary args.
