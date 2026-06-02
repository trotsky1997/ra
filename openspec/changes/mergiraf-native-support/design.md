# Design

## Context

Ra has an established pattern for native CLI wrapper tools: `git`, `gh`, `jq`, `mise`, `just`, and `wrkflw` all share the same shape — argv-safe process spawning, a bounded JSON result envelope, and structured missing-binary guidance. The `bash` tool handles arbitrary shell work but bypasses the catalog's structured schema and safety guarantees.

Mergiraf is a syntax-aware git merge driver. Agents working on git-heavy tasks (merge, rebase, cherry-pick) need to invoke `mergiraf merge`, `mergiraf solve`, and `mergiraf languages`. Today that works only through `bash`, which provides no structure and gives no actionable message when `mergiraf` is absent from PATH.

The `mergiraf` tool should follow exactly the same conventions as `jq` and `wrkflw`, staying narrow and making no attempt to replicate mergiraf's internal logic in Rust.

## Goals / Non-Goals

**Goals:**

- Wrap the `mergiraf` binary with argv-safe process spawning and no shell interpolation.
- Expose `merge` (git merge-driver invocation), `solve` (resolve conflicts in a file), and `languages` (list supported extensions) as structured sub-actions.
- Return a consistent JSON envelope: `ok`, `tool`, `action`, `exit_code`, `stdout`, `stderr`, `truncated`, and optional `error`.
- Return `error.kind: "missing_mergiraf"` with install guidance when the binary is absent.
- Bound all process output via `max_output_bytes`.
- Register via `default_builtins` so the tool obeys `[tools] builtin` allow-list semantics.
- Document in `README.md` and `spec/tools.md`; add focused integration tests.

**Non-Goals:**

- Do not implement mergiraf's tree-sitter parsing or conflict resolution logic in Rust.
- Do not expose every mergiraf flag in v1 — keep the schema narrow (the key flags for the merge-driver and solve paths).
- Do not remove `bash` as the fallback for advanced or experimental mergiraf invocations.
- Do not configure `.gitconfig` or `.gitattributes` on the user's behalf.

## Decisions

### Use the system `mergiraf` binary

Invoke `mergiraf` from PATH rather than vendoring or reimplementing it. This matches every other Ra native CLI wrapper and keeps the implementation small and correct. If the binary is missing, return `ok: false` with `error.kind: "missing_mergiraf"` and installation guidance (`cargo install mergiraf` or distro package).

Alternative considered: a Rust crate embedding mergiraf logic. Ruled out — broadens scope enormously and risks drift from upstream behavior.

### Three actions: `merge`, `solve`, `languages`

- `merge`: maps to `mergiraf merge <base> <ours> <theirs> [flags]` — the git merge-driver path. Accepts the three required file paths plus optional `--language`, `--compact`, and `--allow-parse-errors`.
- `solve`: maps to `mergiraf solve <file>` — resolves conflict markers in a file that already has them. Simpler invocation, single path argument.
- `languages`: maps to `mergiraf languages --gitattributes` — read-only, no file args.

Alternative considered: exposing arbitrary `args` to cover all mergiraf subcommands. Ruled out — reintroduces the shell-escape surface this tool is meant to avoid; advanced use stays in `bash`.

### JSON envelope consistent with existing wrappers

Fields: `ok: bool`, `tool: "mergiraf"`, `action: string`, `exit_code: int | null`, `stdout: string`, `stderr: string`, `truncated: bool`, `error?: {kind, message, install_hint}`. The `truncated` flag clips combined output before returning, following the `jq` and `webfetch` patterns.

### ACP hosts: spawn locally like `mise`/`just`/`wrkflw`

The tool spawns the local `mergiraf` binary directly even when an ACP host is attached, matching the pattern established by the other native CLIs. ACP terminal permission prompts do not wrap the spawn; `[tools].builtin` and PreToolUse/PostToolUse hooks remain the governance mechanism.

## Risks / Trade-offs

- Missing `mergiraf` on PATH → return structured install guidance; this is expected behavior for optional native tools.
- `merge` action writes to the `ours` file in-place (mergiraf's standard behavior) → callers must be aware the file is mutated; tests should use temp dirs.
- Narrow v1 flag surface may not cover `--compact` or `--allow-parse-errors` edge cases → these can be added in a follow-up change without breaking the envelope.
- mergiraf exits non-zero when conflicts remain unresolved → `ok: false` is correct here; callers should inspect `exit_code` to distinguish "conflicts remain" from "binary missing".

## Open Questions

None blocking implementation. The install guidance string (cargo vs. binary release) can be finalized during implementation by checking the upstream release method.
