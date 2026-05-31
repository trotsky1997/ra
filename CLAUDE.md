# CLAUDE.md

Project-level guidance for AI coding agents (Claude Code, OpenCode,
Cursor, Codex, …) working in this repository. Treat this as durable
authorization for the things listed here; everything outside still
needs explicit user approval.

## What Ra is, in one paragraph

Ra is a Rust-native coding agent. One binary speaks four protocols:
ACP (over stdio), A2A (HTTP+gRPC, both client and server), MCP
(client only), and a feature-gated interactive TUI. Sessions are
persisted in ATIF v1.7 trajectories, observability is ATOF v0.1 via
NeMo Relay, and configuration lives in an HCP-flavoured `ra.toml`.
The architecture and current scope are described in `README.md`;
authoritative protocol schemas live in `spec/`; what's done and
what's next lives in `ROADMAP.md`.

## Build, test, run

The only mandatory toolchain is **stable Rust**. The TUI feature
requires nightly because `opentui_rust` uses edition 2024.

```bash
# Default build (stable, no TUI)
cargo build
cargo test                              # 19 tests across 8 suites

# Single test suite
cargo test --test multi_turn            # streaming + parallel + multi-turn
cargo test --test resume                # disk round-trip
cargo test --test skills_discover       # skills.sh auto-discovery
cargo test --lib hooks::                # hook engine

# Build + test the TUI feature
rustup toolchain install nightly        # one-time
cargo +nightly build --features tui
cargo +nightly test --features tui

# Schema regeneration (run after touching src/config.rs)
cargo run --bin gen-schema > spec/ra-config.schema.json

# Run a single prompt with the mock model (no API key)
cargo run -- "bash:echo hi"

# Run as ACP / A2A / TUI
cargo run -- acp
cargo run -- serve --http-port 3000 --grpc-port 50051
cargo +nightly run --features tui --bin ra -- tui
```

Required for `cargo +nightly build --features tui` to succeed: this
machine has nightly installed. CI / fresh-clone agents must run
`rustup toolchain install nightly` first.

## Project conventions

- **No `rustfmt.toml` / `clippy.toml`.** Default rustfmt + clippy
  rules apply. If you reformat, do the whole file you touched, not
  the surrounding ones.
- **Every change comes with a test or an end-to-end smoke.** New
  features land with at least one integration test in `tests/`,
  preferably one that exercises the public surface (see
  `tests/multi_turn.rs` for the pattern: ScriptedModel + assertions
  on Event ordering). Bug fixes land with a regression test.
- **Don't introduce panics on the hot path.** `Session`,
  `SessionRunner`, the tool catalogue, ACP/A2A handlers, and the TUI
  loop all use `anyhow::Result` for fallible work and surface errors
  on the broadcast bus or as `Event::Error`. Adding `unwrap()` /
  `expect()` to those paths is regressive.
- **Match the existing module layout when growing the codebase.**
  Tools live under `src/tools/`; protocol surfaces are top-level
  modules (`acp_server.rs`, `a2a_server.rs`, `mcp.rs`); persistence
  is `store.rs` + `atif.rs` + `atif_codec.rs`. New modules go where
  their nearest neighbour lives.
- **Don't write into `target/`, `Cargo.lock` (unless the toolchain
  did), or anything in `~/.cargo` from build scripts.** Ra has no
  `build.rs` today; keep it that way.
- **Don't run `cargo install` / `cargo publish` / `cargo yank` /
  `cargo update` / `git push --force` / `git reset --hard` /
  package manager installs (apt, brew, pip, npm install -g) unless
  the user explicitly asks.** These are out-of-process side effects.
- **`cargo update`** is fine as part of solving a dependency
  conflict, but flag it: bumping every transitive dep without
  context is a known way to break the build silently.

## Hot zones — be careful here

These files have load-bearing invariants that aren't obvious from
reading the code in isolation. Read the doc comment at the top
before editing.

- `src/session.rs` — turn loop, broadcast bus ordering, `cancel()`
  semantics, `restore_messages()` for resume. Changing the event
  emission order will silently break ACP, A2A, TUI, and the
  multi-turn integration test simultaneously.
- `src/atif_codec.rs` — encode/decode must round-trip;
  `tests/resume.rs` is the canary.
- `src/hooks.rs` — wire format follows the upstream Claude Code
  hooks spec verbatim (PascalCase event names, exit-code-2 = block,
  `hookSpecificOutput.permissionDecision` for PreToolUse).
  Renaming or reshaping fields is a public-API break for hook
  scripts.
- `src/tools/rtk.rs` — RTK exits non-zero (3, not 0) when it has
  a recipe; the trustworthy signal is "stdout non-empty", *not*
  `status.success()`. Don't "fix" that.
- `src/tools/search.rs` — `canonical_probe()` strips paths and
  aliases `fdfind` → `fd` so RTK matches; reverting that breaks
  RTK on Debian.
- `src/a2a_server.rs` — A2A reqwest version is **0.13** (aliased as
  `reqwest13` in Cargo.toml) while the rest of the codebase pins
  **0.12** for llm/nemo-relay. Don't try to unify.
- `src/tui.rs` — `Renderer` is `!Send`. The whole render + input
  + broadcast loop must stay on the main thread. Spawning a future
  that touches `self.renderer` will not compile, but spawning one
  that mutates anything in `TuiApp` will compile and silently
  race.

## Doing common things

- **Adding a new built-in tool**: implement `Tool` in
  `src/tools/`, register it in `tools::default_builtins`,
  document the JSON schema in `spec/tools.md`. See `WriteTool`
  (`src/tools/fs.rs`) as the simplest reference.
- **Adding a new config section**: extend `RaConfig` in
  `src/config.rs` with `#[derive(Debug, Default, Clone,
  Deserialize, JsonSchema)]` and `#[serde(deny_unknown_fields)]`,
  then regenerate the schema with `cargo run --bin gen-schema >
  spec/ra-config.schema.json` and update `spec/ra.toml.example`.
- **Adding an Event variant**: must be added to ATOF mapping in
  `src/tui.rs::spawn_atof_bridge`, the print-mode dispatcher in
  `src/main.rs::spawn_event_printer`, and the multi-turn
  integration test (`tests/multi_turn.rs`) — those three are the
  consumers that exhaustively `match` on `Event`.
- **Touching the ACP wire**: the schemas in `spec/acp-v1.json` /
  `spec/acp-meta.json` are pulled from upstream and are
  authoritative. Ra uses a fork of the rust-sdk under
  `agent-client-protocol = { git = "...", branch =
  "feat/unstable-feature-passthrough" }`; PR upstream first, then
  point Cargo.toml back at the official crate.

## Pre-commit checklist

Before committing user-facing changes, run:

```bash
cargo build                              # stable
cargo test                               # 19 tests
cargo +nightly build --features tui      # if you touched src/tui.rs or Cargo.toml
cargo +nightly test --features tui       # if you touched src/tui.rs
cargo run --bin gen-schema > spec/ra-config.schema.json  # if you touched src/config.rs
```

If any of those fail, don't commit — fix the underlying issue. Don't
disable a test, don't `--no-verify` past a hook.

## Out of scope for AI agents (require user approval)

- Pushing branches, opening / merging PRs, force-pushing.
- `cargo publish`, GitHub release creation, tagging.
- Editing `Cargo.lock` by hand. (Letting cargo regenerate it is fine.)
- Any change to the `LICENSE` file.
- Adding a new top-level external dependency (something not already
  in `Cargo.toml`). Discuss the choice first; this project values
  small dep tree.
- Anything that touches the user's `~/.config`, `~/.cargo`,
  `~/.ra/skills/`, or other dotfile directories outside the repo.

## When in doubt

Read the README, then `ROADMAP.md`, then `spec/README.md`. If still
unclear, ask the user — concise question, one ask at a time. Don't
guess at protocol semantics; the schemas in `spec/` are normative.
