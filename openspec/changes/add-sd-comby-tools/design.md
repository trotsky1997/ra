# Design

## Context

Ra already has native tool wrappers for common CLIs. `git` and `gh` preserve
argv boundaries, while `jq`, task workflow tools, webfetch, tmux, and openspec
return bounded JSON envelopes with structured missing-binary and timeout
handling. The `sd` and `comby` tools should follow the JSON-envelope pattern
because they can fail independently, emit large diffs, and need deterministic
fake-binary integration tests.

The user-facing need is split across two rewriting classes:

- `sd`: fast regex or literal replacement across explicit file path arguments.
- `comby`: structural template check, diff, and rewrite actions across a
  directory using Comby's `:[hole]` syntax.

## Goals / Non-Goals

**Goals:**

- Provide default built-in tools named `sd` and `comby`.
- Preserve exact allow-list semantics with `[tools].builtin = ["sd"]` and
  `[tools].builtin = ["comby"]`.
- Spawn both host binaries through `Command::arg` without shell interpolation.
- Validate unsafe or unsupported request shapes before spawning a binary.
- Return bounded JSON envelopes for success, non-zero exits, invalid requests,
  timeouts, and missing binaries.
- Cover behavior with fake-binary integration tests that do not require real
  `sd` or `comby` installs.

**Non-Goals:**

- Do not vendor, install, or version-probe `sd` or `comby`.
- Do not parse or reinterpret `sd`/`comby` stdout and stderr beyond bounding
  the returned envelope.
- Do not support stdin mode for either tool.
- Do not change `edit`, `apply_patch`, `ast_grep`, or `bash` semantics.

## Decisions

### Use dedicated tool modules

Add `src/tools/sd.rs` and `src/tools/comby.rs` rather than folding the tools
into the generic task workflow wrapper. The schemas and validation rules are
tool-specific: `sd` requires `find`, `replace`, and non-empty `paths`; `comby`
has action-dependent requirements and CLI mappings.

Alternative considered: expose both as generic `args` wrappers. That would
preserve argv boundaries but would not give the model discoverable fields or
pre-spawn validation.

### Keep execution local and transparent

Both tools resolve `cwd` against the session cwd, validate that it is a
directory, find the binary with `which`, then spawn the host command locally
with stdin set to null. The returned command metadata records `program`,
`args`, and `cwd`. Ra does not use ACP terminal wrapping for these tools so
argv boundaries and bounded JSON envelopes remain consistent.

Alternative considered: route through the host terminal like `git`/`gh`. That
would weaken deterministic envelope handling and expose these rewrite tools to
shell rendering.

### Map `sd` directly to its CLI

`sd` maps to:

```text
sd [--fixed-strings] [extra_args...] -- <find> <replace> <paths...>
```

`paths` must be non-empty because the agent tool call has no interactive stdin
stream to rewrite. `extra_args` is appended before the find/replace positionals
so callers can pass flags such as `--flags i` without shell strings. Ra inserts
`--` before `find`/`replace` so leading-dash patterns and replacements are not
interpreted as sd flags.

Alternative considered: support empty `paths` as stdin mode. That would make
tool calls hang or do nothing in unattended agent contexts, so validation
rejects it before spawning `sd`.

### Map `comby` to explicit actions

`comby` uses `CombyAction`:

- `rewrite`: `comby <match> <rewrite> [extensions...] [-d directory]
  [-matcher matcher] [-include-files regex] [-exclude-files regex] -in-place
  [extra_args...]`
- `check`: `comby <match> "" [extensions...] [-d directory] [-matcher matcher]
  [-include-files regex] [-exclude-files regex] -match-only [extra_args...]`
- `diff`: `comby <match> <rewrite> [extensions...] [-d directory]
  [-matcher matcher] [-include-files regex] [-exclude-files regex] -diff
  [extra_args...]`

`rewrite` and `diff` require `rewrite_template`; `check` does not. `rewrite`
defaults to in-place mutation as requested. `check` and `diff` do not pass
`-in-place`, preserving dry-run behavior.

Alternative considered: a `dry_run` boolean on a single rewrite action. The
enum keeps the wire shape clearer for the model and matches existing
enum-action tool patterns.

### Bound output with PRD defaults

`sd` defaults to a 30,000 ms timeout and 32,768 byte result envelope.
`comby` defaults to a 60,000 ms timeout and 65,536 byte result envelope.
Both trim stderr first to a fixed safety budget, then clip stdout as needed to
preserve valid JSON and set `truncated: true`.

Alternative considered: no default timeout. These commands can traverse many
files, so bounded defaults are safer for unattended agent runs.

## Risks / Trade-offs

- Missing host binaries -> Return structured install guidance and avoid
  opaque spawn errors.
- `extra_args` can still request surprising CLI behavior -> Keep argv-safe
  execution, document that it is an advanced escape hatch, and rely on tool
  allow-lists/hooks for policy.
- `comby` CLI flags may vary across versions -> Do not version-probe in this
  change; surface non-zero exits transparently in the envelope.
- Very small `max_output_bytes` values may still exceed the requested budget
  because valid JSON must be preserved -> Match existing envelope behavior and
  document the budget as best-effort.
