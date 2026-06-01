# Ra roadmap

A list of what's done and what's next. Things land here once they're
on disk and tested; speculative ideas live as GitHub issues.

## Done

### Core runtime
- Streaming text + parallel tool calls + multi-turn tool loop, with
  the broadcast bus emitting `AgentStart` / `TurnStart` / `TextDelta`
  / `ThinkingDelta` / `ToolCallStart` / `ToolCallUpdate` / `ToolCallEnd`
  / `TurnEnd` / `AgentEnd` / `Error` (`src/events.rs`).
- `Session::cancel` based on `tokio_util::CancellationToken`,
  re-armed on every prompt so a cancelled session is reusable
  (`src/session.rs`).
- Multi-backend LLM layer through [graniet/llm](https://github.com/graniet/llm):
  Anthropic, OpenAI, Google, DeepSeek, Ollama, Groq, xAI, plus any
  OpenAI-compatible Responses API endpoint.

### Protocols
- **ACP v1 end-to-end** as both client and agent over stdio JSON-RPC,
  including reverse calls for `fs/read_text_file`,
  `fs/write_text_file`, `terminal/*`, and `session/request_permission`
  (`src/acp_server.rs`).
- **A2A bidirectional** — serve as a remote agent over HTTP (JSON-RPC
  + REST + agent card) and gRPC, with optional Bearer-token auth,
  *and* consume remote A2A agents as local tools so the LLM can
  delegate (`src/a2a_server.rs`, `src/a2a_tool.rs`). A2A clients
  reconnecting with a known `task_id` are auto-resumed from the
  on-disk trajectory.
- **MCP client** via [rmcp](https://github.com/modelcontextprotocol/rust-sdk)
  1.7 (stdio + Streamable HTTP).
- **ATIF v1.7** trajectory persistence (`src/store.rs`,
  `src/atif_codec.rs`) with `ra resume <id>` / `ra sessions` CLI.
- **ATOF v0.1** observability via [NeMo Relay](https://github.com/NVIDIA/NeMo-Relay):
  every prompt / tool / hook / TUI event produces scope and mark
  events; configurable backends (stderr / file / OTLP).

### Configuration & customisation
- HCP-flavored TOML config (`src/config.rs`) with auto-resolution
  `--config` → `$RA_CONFIG` → `./ra.toml` → `~/.ra.toml`.
- JSON Schema generation (`cargo run --bin gen-schema`).
- Native [skills.sh](https://github.com/vercel-labs/skills) discovery
  from `.ra/skills/` and `.agents/skills/` (project + global) with
  no config needed (`src/skills.rs`).
- [agentskills.io v1](https://agentskills.io/) `SKILL.md` parser with
  YAML frontmatter and progressive disclosure.
- [agents.md](https://agents.md/) auto-walk from cwd to git root.
- Native [OpenSpec](https://github.com/Fission-AI/OpenSpec) discovery
  (`src/openspec.rs`): finds the nearest `openspec/` directory (cwd →
  git root) and folds a catalog of capability specs (requirement /
  scenario counts) and active changes (task progress, touched
  capabilities) into the system prompt with progressive disclosure.
  Archived changes are skipped; `[openspec]` toggles it and `path`
  overrides the location. Ra consumes the convention, it doesn't
  reimplement the `openspec` CLI.
- User-defined slash-command prompt templates.
- Lifecycle hooks wire-compatible with the
  [Claude Code hooks spec](https://code.claude.com/docs/en/hooks.md):
  `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `Stop` with
  the official decision schema (`decision: "block"` /
  `permissionDecision` / `additionalContext` / `continue: false`)
  and exit-code-2 handling (`src/hooks.rs`).

### Built-in tools
- `read`, `write`, `edit` — Claude-Code-shaped, ACP fs reverse-call
  when an editor host is connected.
- `bash` — ACP `terminal/*` reverse-call with permission gating, else
  local `/bin/sh -c`.
- Native [RTK](https://github.com/rtk-ai/rtk) integration: every
  `bash` command consults `rtk rewrite` first, swapping verbose
  `git status` / `cargo test` / `kubectl` output for RTK's
  token-compressed equivalents (60–90% savings).

### TUI
- `ra tui` interactive terminal chat backed by
  [opentui_rust](https://github.com/Dicklesworthstone/opentui_rust),
  feature-gated as `--features tui` (requires nightly because
  opentui_rust uses edition 2024). Streaming text, live tool-call
  collation, scrollback, Ctrl-C cancel, Ctrl-D quit, trajectory
  saved on exit.
- `tokio::sync::broadcast` UI event bus with a default ATOF bridge so
  TUI sessions appear in observability traces.

## Next

Roughly ordered by user value, not certainty.

### High-impact
- **Inline diff viewer in TUI for `edit` and `write`.** Right now the
  TUI prints the tool's text output. With opentui's alpha blending
  we can render a side-by-side or unified diff with colour for
  added / removed lines, and ask the user to accept / reject before
  the edit lands.
- **Session browser inside TUI.** A modal that calls `SessionStore::list`
  and lets the user select a saved session to resume without
  exiting and re-running `ra resume <id>`.
- **Slash commands inside TUI.** SessionRunner already expands user
  prompt templates when invoked over ACP/A2A; TUI bypasses
  SessionRunner today and so can't run them. Wiring SessionRunner
  through the TUI submit path enables `/skill-name args…` syntax
  in the chat.

### Quality of life
- **`ra init`** to scaffold an `ra.toml` and a `.ra/skills/`
  directory in the cwd, with the most common config knobs commented
  in.
- **Upstream PR to vercel-labs/skills** adding a `ra` agent entry
  pointing at `./.ra/skills/` + `~/.ra/skills/`. Until merged,
  `npx skills add ... -a universal` already lands files where Ra
  reads them.
- **`docs/architecture.md`** walking through the request path
  (ACP `session/prompt` → SessionRunner → Session::run_one_turn →
  Model::stream → Tool::execute → ACP reverse-call). The
  `spec/` collection covers the wire format; this would cover the
  internal flow.

### Speculative
- **TUI mouse support.** `RendererOptions::enable_mouse = true`
  unlocks click-to-select on the scrollback. Cheap to add but
  changes the feel of the app.
- **Threaded renderer** (`opentui_rust::ThreadedRenderer`) if
  profiling shows the 30 fps redraw blocks the broadcast consumer.
  Today it doesn't.
- **A2A push notifications.** The agent-card already advertises
  `streaming: true`; adding `push_notifications` lets remote
  clients subscribe to long-running tasks instead of polling.

## Anti-roadmap

What we deliberately won't do:

- **A2A serve over WebSockets.** The spec's three transports
  (HTTP-JSON-RPC + REST + gRPC) cover every client we know about;
  WebSockets would be a fourth surface to maintain with no demand.
- **Reinvent skills.sh.** It's the package manager. `ra` is the
  consumer. We don't ship `ra skills add` / `ra skills update` —
  use `npx skills` and Ra reads the directory it lands in.
- **A custom widget framework on top of opentui_rust.** The TUI
  hand-rolls layout because the chat surface is small. If the TUI
  grows panels and modals to the point where ratatui's widget set
  would help, we'll reconsider — but we won't write a half-baked
  widget abstraction.
