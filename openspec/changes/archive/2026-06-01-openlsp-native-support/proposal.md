## Why

Ra's built-in tool set covers file I/O and shell execution but has no language-aware code intelligence. Integrating openlsp gives the agent native access to LSP diagnostics, go-to-definition, hover, and references — enabling self-correction after edits without requiring the user to configure an MCP server.

## What Changes

- New built-in tool `lsp` that spawns openlsp as a child process and dispatches JSON command envelopes.
- New `[openlsp]` config section in `ra.toml` to control the openlsp binary path, workspace root, and timeout.
- `lsp` is registered in `default_builtins` and filtered by the existing `[tools] builtin` allow-list.
- PostToolUse hook example in `spec/ra.toml.example` showing openlsp wired for auto-diagnostics after `write`/`edit`.

## Capabilities

### New Capabilities

- `openlsp-tool`: Built-in Ra tool that wraps the openlsp CLI, accepting LSP operation names and file paths and returning structured JSON results.

### Modified Capabilities

- (none — no existing spec-level requirements change)

## Impact

- `src/tools/lsp.rs` — new file implementing `LspTool`.
- `src/tools/mod.rs` — register `LspTool` in `default_builtins`.
- `src/config.rs` — add `OpenlspSection` to `RaConfig`.
- `Cargo.toml` — no new Rust dependencies (openlsp is a Bun/Node CLI, invoked via `tokio::process::Command`).
- `tests/openlsp.rs` — integration tests (mock binary path, schema validation, error propagation).
- `spec/ra.toml.example` — document the new `[openlsp]` section.
- Requires `bun` or `npx` on PATH at runtime; gracefully skipped if absent (same pattern as `rtk`).
