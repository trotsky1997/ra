## Context

Ra's built-in tools (`read`, `write`, `edit`, `bash`, `git`, `gh`, `ast_grep`) cover file I/O and shell execution but have no language-aware code intelligence. openlsp is a Bun-native CLI that wraps LSP operations behind a stable JSON envelope interface, designed specifically for coding agents. It exposes diagnostics, go-to-definition, hover, references, formatting, and static analysis via `POST /command` (HTTP server mode) or stdin/stdout (one-shot CLI mode).

The existing MCP client already lets users wire openlsp as an MCP server via `[[mcp.servers]]`, but that requires manual config and a running MCP adapter. Native support means zero-config: if `openlsp` (or `bunx openlsp`) is on PATH, the `lsp` tool is available automatically.

## Goals / Non-Goals

**Goals:**
- Add a built-in `lsp` tool that invokes openlsp as a child process (one-shot CLI mode).
- Support the full openlsp command envelope: `lsp`, `format`, `analyze`, `capabilities`, `session-close`.
- Respect the existing `[tools] builtin` allow-list (opt-out via `builtin = [...]` without `"lsp"`).
- Add `[openlsp]` config section for binary path, workspace root, and per-call timeout.
- Graceful degradation: if openlsp is not on PATH, the tool is silently omitted (same pattern as `rtk`).

**Non-Goals:**
- Implementing an LSP client in Rust (openlsp already handles that).
- HTTP server mode (one-shot CLI is sufficient for agent use; HTTP adds lifecycle complexity).
- Bundling openlsp as a Rust dependency (it's a Bun/Node tool; agents install it separately).
- Replacing the MCP path (users who prefer MCP can keep using `[[mcp.servers]]`).

## Decisions

### D1: One-shot CLI mode over HTTP server mode

openlsp supports both `bun run src/cli.ts <command>` (one-shot) and `bun run src/cli.ts serve` (HTTP). One-shot is simpler: no lifecycle management, no port conflicts, no cleanup. The agent calls it per-operation, which matches how `git` and `gh` tools work. Latency is acceptable for agent use (LSP startup is ~200ms; agents don't need sub-100ms response).

**Alternative considered:** HTTP server mode with a persistent child process. Rejected because it requires process lifecycle management (start/stop/health-check) and complicates the tool's `execute` path significantly.

### D2: Binary resolution — `openlsp` → `bunx openlsp` fallback

Resolution order: (1) `[openlsp] binary` config override, (2) `openlsp` on PATH, (3) `bunx openlsp` if `bun` is on PATH. This mirrors how many Bun tools are distributed and avoids requiring a global install.

### D3: JSON envelope passthrough

The `lsp` tool accepts `operation` (string) and `params` (object) and constructs the openlsp JSON envelope `{"command": operation, ...params}`. This keeps the tool schema minimal and forwards the full openlsp API surface without Ra needing to know every operation.

**Alternative considered:** Separate tools per operation (`lsp_diagnostics`, `lsp_hover`, etc.). Rejected as premature — one tool with an `operation` discriminator is simpler and matches how `git` accepts arbitrary `args`.

### D4: Config section `[openlsp]` added to `RaConfig`

Follows the existing pattern (`[rtk]`, `[openspec]`, `[mcp]`). Fields: `enabled` (bool, default true), `binary` (optional string override), `workspace_root` (optional string), `timeout` (f64 seconds, default 30.0).

## Risks / Trade-offs

- **Bun/Node runtime dependency** → Mitigation: graceful skip if binary not found; tool simply absent from catalog.
- **openlsp startup latency per call** → Mitigation: acceptable for agent use; document in tool description. Future: HTTP mode can be added later.
- **openlsp API changes** → Mitigation: JSON passthrough means Ra doesn't encode openlsp's schema; breakage surfaces as tool errors, not compile errors.
- **Large LSP output** → Mitigation: openlsp's `--json` output is already structured; agent can filter by operation.

## Migration Plan

No migration needed. The `lsp` tool is additive. Existing configs are unaffected. Users who want to disable it can add `"lsp"` exclusion via the `builtin` allow-list or set `[openlsp] enabled = false`.

## Open Questions

- (none — design is self-contained)
