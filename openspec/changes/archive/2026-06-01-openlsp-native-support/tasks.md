## 1. Config

- [x] 1.1 Add `OpenlspSection` struct to `src/config.rs` with fields: `enabled` (bool, default true), `binary` (Option<String>), `workspace_root` (Option<String>), `timeout` (f64, default 30.0)
- [x] 1.2 Add `openlsp: OpenlspSection` field to `RaConfig` struct
- [x] 1.3 Add `[openlsp]` example block to `spec/ra.toml.example`

## 2. Binary Resolution

- [x] 2.1 Implement `resolve_openlsp_binary(cfg: &OpenlspSection) -> Option<(String, Vec<String>)>` in `src/tools/lsp.rs` — tries config override, then `openlsp` on PATH via `which`, then `bunx openlsp` if `bun` is on PATH

## 3. LspTool Implementation

- [x] 3.1 Create `src/tools/lsp.rs` with `LspTool` struct holding resolved binary path and config
- [x] 3.2 Implement `Tool` trait for `LspTool`: `name() = "lsp"`, description, JSON schema with `operation` (string) and optional `params` (object)
- [x] 3.3 Implement `execute`: serialize input to openlsp JSON envelope, spawn child process, pass envelope on stdin, capture stdout/stderr, return output string
- [x] 3.4 Apply per-call timeout from config using `tokio::time::timeout`

## 4. Registration

- [x] 4.1 Add `mod lsp` and `pub use lsp::LspTool` to `src/tools/mod.rs`
- [x] 4.2 Add `lsp` entry to `default_builtins` in `src/tools/mod.rs` — call `resolve_openlsp_binary` at registration time; skip if None

## 5. Tests

- [x] 5.1 Create `tests/openlsp.rs` with a test that verifies `lsp` is absent from `default_builtins` when no openlsp binary exists (mock PATH)
- [x] 5.2 Add unit test in `src/tools/lsp.rs` for `resolve_openlsp_binary` with a config override pointing to a known binary
- [x] 5.3 Add config parse test in `src/config.rs` for the `[openlsp]` section defaults and overrides
- [x] 5.4 Run `cargo test` and fix any failures
