# Add Native mergiraf Support

## Why

Ra agents frequently trigger git operations (merge, rebase, cherry-pick) that produce conflict markers in files. Today agents fall back to generic `bash` to invoke `mergiraf` — forfeiting structured error handling, missing-binary guidance, and argv safety. Adding a native `mergiraf` tool brings syntax-aware merge conflict resolution into Ra's built-in tool catalog with the same reliability guarantees as `jq`, `git`, and other native wrappers.

## What Changes

- Add a native built-in `mergiraf` tool that exposes the core mergiraf subcommands (`merge`, `solve`, `languages`) as structured operations.
- Execute mergiraf through argv-safe process spawning — no shell interpolation.
- Return a stable JSON envelope with exit status, stdout, stderr, truncation state, and structured missing-binary guidance.
- Bound all returned output through `max_output_bytes`.
- Register the tool in `default_builtins`, document it in `README.md` and `spec/tools.md`, add focused integration tests.

## Capabilities

### New Capabilities

- `mergiraf-tool`: Native Ra built-in that wraps the `mergiraf` CLI for syntax-aware merge conflict resolution. Covers `merge` (git merge-driver invocation), `solve` (resolve conflicts in a file with existing markers), and `languages` (list supported extensions in gitattributes format). Returns a bounded JSON envelope consistent with existing native CLI wrapper conventions.

### Modified Capabilities

- `tools`: Add the `mergiraf` built-in tool entry to the existing tool catalog requirements.

## Impact

- Affected code: `src/tools/` (new `mergiraf.rs`), tool registration in `src/tools/mod.rs`, `src/config.rs` if tool metadata is config-driven.
- Affected docs: `README.md` built-in tools table, `spec/tools.md`.
- Runtime dependency: `mergiraf` binary must be on `PATH`; missing binary returns structured guidance, not an opaque spawn error.
- No breaking changes. `bash` remains available as the general fallback; existing allow-list semantics are unchanged.
